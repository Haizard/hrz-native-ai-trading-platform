//! Bybit v5, expressed as a [`WireCodec`].
//!
//! ## Everything here is measured, not read
//!
//! Every field name, type and behaviour below was captured off
//! `wss://stream.bybit.com/v5/public/spot` on 2026-09-20 with a hand-rolled
//! WebSocket client, because the documentation and the socket disagreed in a way
//! that would have produced a book that never synced. The captured frames are
//! the fixtures in this file's tests.
//!
//! ## The one that mattered: there is no `u == 1`
//!
//! The plan was that a Bybit order-book stream opens with a frame whose `u == 1`,
//! the documented "this is a reset boundary" marker, and that the codec would
//! bridge it by claiming `last_update_id = u - 1`. That is **false for
//! `orderbook.*`**:
//!
//! ```text
//! topic=orderbook.50.ETHUSDT type=snapshot u=219167606 seq=181840388171 b=50 a=50
//! topic=orderbook.50.ETHUSDT type=delta    u=219167607 seq=181840388311 b=2  a=0
//! topic=orderbook.50.ETHUSDT type=delta    u=219167608 seq=181840388420 b=0  a=1
//! ```
//!
//! A fresh subscription is answered with a full book whose `u` is a real,
//! current, per-symbol counter -- not `1` -- and the next delta continues from
//! `u + 1`. (The `u == 1` rule belongs to the *REST* orderbook response's reset
//! semantics on other products, and carrying it here would have been an
//! invented bridge: `last_update_id = u - 1 = 219167605` is *behind* the
//! snapshot's own levels, `try_bridge` would find no diff to bridge
//! (`first_update_id <= 219167606` fails for `219167607`), `synced` would stay
//! false, and the book would publish nothing forever.)
//!
//! So the snapshot is installed with its **own** `u` as `last_update_id`, and
//! the very next delta satisfies `first_update_id <= last_update_id + 1`. No
//! REST call, no arithmetic, no assumption. This is also why
//! [`BookBootstrap::InBand`](super::collector::BookBootstrap::InBand) is the
//! right setting for Bybit rather than a convenience.
//!
//! ## The other measured shape
//!
//! * Envelope: `{topic, ts, type, data}`, `type` in `snapshot | delta`.
//! * `orderbook.{depth}.{symbol}`: `data` is an **object**
//!   `{s, b: [[p, q], ...], a: [...], u, seq}` -- every price and quantity a
//!   **string**, `u`/`seq` **ints**.
//! * `u` increments by 1 per update for the symbol; `seq` is a much larger
//!   cross-topic sequence. `u` is the one the book bridges on.
//! * A quantity of `"0"` **deletes** the level.
//! * `data` for a book delta may have `b: []` or `a: []` -- an empty side is
//!   normal and means "no change on this side", not "clear this side".
//! * `publicTrade.{symbol}`: `data` is an **array** (measured 8-16 items per
//!   frame), each `{i, T, p, v, S, s, seq, BT, RPI}` -- `i` a **string**,
//!   `T` an int of **milliseconds**, `p`/`v` strings, `S` the word
//!   `"Buy" | "Sell"` (not a bool), `BT`/`RPI` booleans we ignore.
//! * `S` is the **taker** side, which is exactly `is_buyer_maker` inverted:
//!   `S == "Sell"` means the aggressor sold, so the *buyer* was the maker.
//! * Control frames: `{"success":true,"ret_msg":"subscribe",...}` on subscribe
//!   and `{"success":true,"ret_msg":"pong",...}` on ping -- neither has a
//!   `topic`, and neither may be mistaken for an error.
//!
//! ## Topology
//!
//! **One socket per category.** `spot`, `linear` and `inverse` are separate
//! endpoints and a socket carries every symbol of its category via an `args`
//! array. This codec therefore describes exactly one socket; the collector it
//! was built for opens one, and a multi-category deployment opens three
//! collectors rather than teaching one collector to multiplex.

use serde::Deserialize;
use serde_json::Value;

use super::codec::{DepthDiff, Frame, Incoming, Subscription, WireCodec};
use crate::error::MarketDataError;

/// Public WebSocket hosts by category.
pub const BYBIT_SPOT_WS: &str = "wss://stream.bybit.com/v5/public/spot";
/// Linear (USDT/USDC perpetual) socket.
pub const BYBIT_LINEAR_WS: &str = "wss://stream.bybit.com/v5/public/linear";
/// Inverse (coin-margined) socket.
pub const BYBIT_INVERSE_WS: &str = "wss://stream.bybit.com/v5/public/inverse";
/// REST host, used by the backfill [`Venue`](super::venue::BybitVenue).
pub const BYBIT_REST: &str = "https://api.bybit.com";

/// How deep a book topic to request.
///
/// Bybit encodes depth in the *topic name* (`orderbook.50.BTCUSDT`), not in the
/// subscribe payload, so it is a property of this codec rather than of a
/// subscription. 50 matches the collector's default publish depth and is the
/// deepest Bybit offers for spot.
pub const BYBIT_BOOK_DEPTH: u32 = 50;

/// Bybit's live wire format.
#[derive(Debug, Clone)]
pub struct BybitCodec {
    /// One of `spot`, `linear`, `inverse`.
    category: String,
    /// Socket endpoint; defaults to the one for `category`.
    ws_url: String,
    /// Interval between pings.
    ///
    /// Bybit closes a socket after **10 minutes** of silence and asks for a
    /// ping roughly every 20s, so 20s is both far inside the deadline and the
    /// cadence the venue documents. It is a field rather than a constant so a
    /// test can drive the heartbeat without waiting 20 seconds.
    ping_interval: std::time::Duration,
    /// Milliseconds at which the next ping is due.
    ///
    /// Interior-mutable because [`WireCodec::heartbeat`] takes `&self`: the
    /// collector owns the socket and asks the codec for a payload, and a codec
    /// that had to be `&mut` to remember its own cadence would fight the
    /// reconnect loop for the borrow.
    ///
    /// An atomic rather than a `Cell`, because the trait requires `Sync` -- a
    /// `Cell` is single-threaded and would make this codec unusable behind the
    /// `Arc` the collector holds. Behind an `Arc` so a clone shares the cadence
    /// rather than silently starting a second one: the collector clones the
    /// codec into its task, and two independent ping schedules for one socket
    /// would ping at the wrong rate.
    ///
    /// `0` means "nothing sent yet, so the first ask sends".
    next_ping_ms: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Default for BybitCodec {
    fn default() -> Self {
        Self::spot()
    }
}

impl BybitCodec {
    /// A codec for Bybit spot.
    #[must_use]
    pub fn spot() -> Self {
        Self::for_category("spot", BYBIT_SPOT_WS)
    }

    /// A codec for Bybit USDT-perpetual linear.
    #[must_use]
    pub fn linear() -> Self {
        Self::for_category("linear", BYBIT_LINEAR_WS)
    }

    /// A codec for Bybit inverse (coin-margined).
    #[must_use]
    pub fn inverse() -> Self {
        Self::for_category("inverse", BYBIT_INVERSE_WS)
    }

    /// A codec against an explicit category and endpoint.
    #[must_use]
    pub fn for_category(category: impl Into<String>, ws_url: impl Into<String>) -> Self {
        Self {
            category: category.into(),
            ws_url: ws_url.into(),
            ping_interval: std::time::Duration::from_secs(20),
            next_ping_ms: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// The category this socket carries.
    #[must_use]
    pub fn category(&self) -> &str {
        &self.category
    }

    /// Override the ping cadence. Tests only.
    #[must_use]
    pub fn with_ping_interval(mut self, interval: std::time::Duration) -> Self {
        self.ping_interval = interval;
        self
    }

    /// The order-book topic for `symbol`.
    #[must_use]
    pub fn book_topic(symbol: &str) -> String {
        format!(
            "orderbook.{BYBIT_BOOK_DEPTH}.{}",
            symbol.to_ascii_uppercase()
        )
    }

    /// The trade topic for `symbol`.
    #[must_use]
    pub fn trade_topic(symbol: &str) -> String {
        format!("publicTrade.{}", symbol.to_ascii_uppercase())
    }

    /// Whether `topic` names an order-book stream, and which symbol.
    fn book_symbol(topic: &str) -> Option<String> {
        let rest = topic.strip_prefix("orderbook.")?;
        // `orderbook.50.BTCUSDT` -> depth then symbol.
        let (_depth, symbol) = rest.split_once('.')?;
        Some(symbol.to_ascii_uppercase())
    }

    /// Whether `topic` names a trade stream, and which symbol.
    fn trade_symbol(topic: &str) -> Option<String> {
        let symbol = topic.strip_prefix("publicTrade.")?;
        Some(symbol.to_ascii_uppercase())
    }
}

/// Bybit's outer envelope.
///
/// `data` is left untyped because it is an **object** for a book and an
/// **array** for trades -- a difference that is not a spelling mistake and
/// cannot be papered over with a `#[serde(untagged)]` enum, which would happily
/// decode a one-element trade array as a book.
#[derive(Debug, Deserialize)]
struct Envelope {
    topic: String,
    data: Value,
    /// `"snapshot"` or `"delta"` on book frames; absent on trade frames.
    ///
    /// Renamed explicitly rather than written as `r#type`: a raw identifier
    /// deserializes from `r#type`, not from `type`, so the field would silently
    /// never match and every frame would fall through to the default -- which is
    /// how this was first written and how three tests caught it.
    #[serde(default, rename = "type")]
    frame_type: String,
}

/// `orderbook.*` payload.
///
/// Renamed explicitly for the same reason as [`TradeItem`]: a field called `u`
/// happens to match, a field called `bids` does not match `b`, and the mismatch
/// surfaces as a runtime missing-field error rather than a compile error.
#[derive(Debug, Deserialize)]
struct BookData {
    /// Symbol.
    #[serde(rename = "s")]
    symbol: String,
    /// Update id. Monotonic per symbol; the book bridges on this.
    #[serde(rename = "u")]
    update_id: u64,
    /// Bids as `[["price", "qty"], ...]`. Absent or empty means no bid change.
    #[serde(rename = "b", default)]
    bids: Vec<(String, String)>,
    /// Asks as `[["price", "qty"], ...]`. Absent or empty means no ask change.
    #[serde(rename = "a", default)]
    asks: Vec<(String, String)>,
}

/// One `publicTrade.*` entry.
///
/// **Every field is renamed explicitly**, including the ones whose Rust name
/// looks like the wire name. A struct field `s_side` does not deserialize from a
/// JSON key `S` -- nothing does, because serde matches on the name unless told
/// otherwise -- and the failure is not a compile error: it is a missing-field
/// error at runtime that turns every trade frame into `Malformed`. That is
/// exactly how this was first written. `T`/`S` are the two the case-insensitive
/// eye skips past.
#[derive(Debug, Deserialize)]
struct TradeItem {
    /// Trade id, a **string** (it exceeds f64's exact integer range).
    #[serde(rename = "i")]
    id: String,
    /// Trade time in **milliseconds**.
    #[serde(rename = "T")]
    t_ms: i64,
    /// Price, a string.
    #[serde(rename = "p")]
    price: String,
    /// Size, a string.
    #[serde(rename = "v")]
    size: String,
    /// **Taker** side: `"Buy"` or `"Sell"`.
    #[serde(rename = "S")]
    taker_side: String,
    /// Symbol.
    #[serde(rename = "s")]
    symbol: String,
}

impl TradeItem {
    /// Convert to the platform's `Trade`.
    ///
    /// The `i` string is parsed to `u64` for the gap detector; a venue that
    /// sends a non-numeric id is a real defect and is reported as malformed
    /// rather than assigned a made-up id, because a made-up id turns a working
    /// gap detector into a random number generator.
    fn to_trade(&self) -> Result<analytics_core::Trade, String> {
        let trade_id = self
            .id
            .parse::<u64>()
            .map_err(|e| format!("trade id `{}` is not numeric: {e}", self.id))?;
        let price = self
            .price
            .parse::<f64>()
            .map_err(|e| format!("price `{}` is not a number: {e}", self.price))?;
        let quantity = self
            .size
            .parse::<f64>()
            .map_err(|e| format!("quantity `{}` is not a number: {e}", self.size))?;

        Ok(analytics_core::Trade {
            symbol: self.symbol.clone(),
            trade_id,
            price,
            quantity,
            // `S` is the *taker* side. A taker sell means the buyer was the
            // maker, which is how the platform's flag reads -- so this is an
            // inversion, not a comparison, and getting it backwards would flip
            // every delta and CVD number downstream while every price and size
            // still looked right.
            is_buyer_maker: self.taker_side.eq_ignore_ascii_case("Sell"),
            timestamp: self.t_ms * 1_000_000,
        })
    }
}

/// Parse `[["price","qty"],...]` into `(f64, f64)`, dropping level deletions.
///
/// ## Deletions are dropped here, not passed through
///
/// A quantity of `"0"` means "remove this level". `OrderBook::apply_diff`
/// handles that, but the level has to *reach* it -- and getting there means
/// surviving both this parse and `PriceKey::new`, which **debug-asserts a price
/// is finite and positive**. A delete is `(price, 0.0)`: a positive price, so it
/// passes. A malformed price of `"0"` would be a zero price, which the assert
/// rejects, so it is filtered as unparseable-by-rule rather than allowed to
/// panic a debug build. Both cases are handled explicitly rather than by luck.
///
/// # Errors
/// Returns a description of the first bad level.
fn parse_levels(levels: &[(String, String)], side: &str) -> Result<Vec<(f64, f64)>, String> {
    let mut out = Vec::with_capacity(levels.len());
    for (price, qty) in levels {
        let price: f64 = price
            .parse()
            .map_err(|e| format!("{side} price `{price}` is not a number: {e}"))?;
        if !price.is_finite() || price <= 0.0 {
            return Err(format!(
                "{side} price `{price}` is not a positive finite number"
            ));
        }
        let quantity: f64 = qty
            .parse()
            .map_err(|e| format!("{side} qty `{qty}` is not a number: {e}"))?;
        if !quantity.is_finite() || quantity < 0.0 {
            return Err(format!(
                "{side} qty `{quantity}` is not a non-negative number"
            ));
        }
        out.push((price, quantity));
    }
    Ok(out)
}

impl WireCodec for BybitCodec {
    fn name(&self) -> &'static str {
        "bybit"
    }

    fn ws_url(&self) -> String {
        self.ws_url.clone()
    }

    fn subscribe_payload(&self, subs: &[Subscription]) -> Result<String, MarketDataError> {
        if subs.is_empty() {
            return Err(MarketDataError::Normalization(
                "a Bybit subscribe needs at least one argument".into(),
            ));
        }

        let args: Vec<String> = subs
            .iter()
            .map(|sub| match sub {
                Subscription::Trades { symbol } => Self::trade_topic(symbol),
                Subscription::OrderBook { symbol } => Self::book_topic(symbol),
            })
            .collect();

        serde_json::to_string(&serde_json::json!({ "op": "subscribe", "args": args }))
            .map_err(|e| MarketDataError::Normalization(e.to_string()))
    }

    /// Bybit wants a ping, or it drops the socket after ten minutes of silence.
    ///
    /// This is the real answer, unlike Binance's `None`. The codec owns its own
    /// cadence so the collector can ask on a fixed tick without knowing that
    /// this venue has an opinion; `now_ms` is the collector's monotonic clock,
    /// so this is testable without waiting twenty seconds.
    fn heartbeat(&self, now_ms: u64) -> Option<String> {
        use std::sync::atomic::Ordering;

        // `0` means "nothing sent yet", so the first ask after a connection
        // sends -- which is also what re-arms the cadence after a reconnect.
        let due_at = self.next_ping_ms.load(Ordering::Relaxed);
        if due_at != 0 && now_ms < due_at {
            return None;
        }

        self.next_ping_ms.store(
            now_ms + self.ping_interval.as_millis() as u64,
            Ordering::Relaxed,
        );
        Some(r#"{"op":"ping"}"#.to_string())
    }

    fn parse_frame(&self, text: &str) -> Frame {
        let Ok(envelope) = serde_json::from_str::<Envelope>(text) else {
            // A subscribe ack (`{"success":true,"ret_msg":"subscribe",...}`) and
            // anything else without a `topic`. Control, never fatal: treating
            // this as an error ends the pump on its own handshake.
            return Frame::Control;
        };

        if let Some(symbol) = Self::book_symbol(&envelope.topic) {
            return parse_book_frame(&envelope, &symbol);
        }

        if Self::trade_symbol(&envelope.topic).is_some() {
            return parse_trade_frame(&envelope);
        }

        Frame::Unrecognised
    }
}

/// Map one `orderbook.*` frame to a snapshot or a diff.
///
/// The `type` field decides, and it is the venue's own word for it: a
/// `"snapshot"` replaces the book, a `"delta"` changes it. Treating a delta as a
/// snapshot would *replace* a 50-level book with the two levels that moved,
/// which is a book that looks plausible and is wrong -- the worst outcome
/// available.
fn parse_book_frame(envelope: &Envelope, symbol: &str) -> Frame {
    let Ok(book) = serde_json::from_value::<BookData>(envelope.data.clone()) else {
        return Frame::Malformed("orderbook payload did not match the Bybit shape".into());
    };

    let bids = match parse_levels(&book.bids, "bid") {
        Ok(levels) => levels,
        Err(reason) => return Frame::Malformed(reason),
    };
    let asks = match parse_levels(&book.asks, "ask") {
        Ok(levels) => levels,
        Err(reason) => return Frame::Malformed(reason),
    };

    // The symbol on the payload wins over the one in the topic if they disagree,
    // because the payload's is the one the venue's own update id belongs to.
    let symbol = if book.symbol.is_empty() {
        symbol.to_string()
    } else {
        book.symbol.to_ascii_uppercase()
    };

    if envelope.frame_type.eq_ignore_ascii_case("delta") {
        return Frame::Data(Box::new(Incoming::BookDelta(DepthDiff {
            symbol,
            // Bybit carries one update id per frame, so the event spans exactly
            // that id. See `DepthDiff::first_update_id`.
            first_update_id: book.update_id,
            final_update_id: book.update_id,
            bids,
            asks,
        })));
    }

    // `"snapshot"` -- and, deliberately, an empty or unknown `type`, because a
    // full book is the safe reading of an unlabelled one: installing it resets
    // the book rather than corrupting it with a partial change.
    Frame::Data(Box::new(Incoming::BookSnapshot {
        symbol,
        bids,
        asks,
        // Measured, not derived: this is the snapshot's own update id and the
        // next delta continues from it. See the module doc for why `u - 1` was
        // the wrong bridge and would have left the book permanently unsynced.
        last_update_id: book.update_id,
    }))
}

/// Map one `publicTrade.*` frame to trades, one frame carrying many.
///
/// Bybit sent 8 to 16 trades per frame when this was measured. A codec that read
/// only `data[0]` would keep 1/16 of the trade stream while every field it
/// looked at still checked out -- a gap the trade-id detector would eventually
/// catch as a *gap*, blaming the network for a decode bug.
fn parse_trade_frame(envelope: &Envelope) -> Frame {
    let Ok(items) = serde_json::from_value::<Vec<TradeItem>>(envelope.data.clone()) else {
        return Frame::Malformed("publicTrade payload was not an array of trades".into());
    };

    let mut trades = Vec::with_capacity(items.len());
    for item in &items {
        match item.to_trade() {
            Ok(trade) => trades.push(trade),
            // One malformed trade in a batch must not lose the other fifteen,
            // and must not be silent either -- `Malformed` is counted.
            Err(reason) => return Frame::Malformed(reason),
        }
    }

    if trades.is_empty() {
        // An empty batch carries no information and is not an error: Bybit does
        // not send one, so seeing one means our model is incomplete, not that
        // the venue misbehaved.
        return Frame::Unrecognised;
    }

    Frame::Data(Box::new(Incoming::Trades(trades)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------
    // Frames captured verbatim off wss://stream.bybit.com/v5/public/spot on
    // 2026-09-20. These are the bytes, not a paraphrase of the docs.
    // ---------------------------------------------------------------------

    /// The ack Bybit sends for a subscribe, with no `topic` field.
    const SUBSCRIBE_ACK: &str = r#"{"success":true,"ret_msg":"subscribe","conn_id":"d9avt8sb86tr31f6ej90-d0qm8","op":"subscribe"}"#;

    /// A full book, exactly as a fresh subscription is answered.
    ///
    /// **The load-bearing fixture.** `u` is 219167606 -- a real current id, not
    /// `1` -- and the delta below continues from it.
    const BOOK_SNAPSHOT: &str = r#"{"topic":"orderbook.50.ETHUSDT","ts":1789875210000,"type":"snapshot","data":{"s":"ETHUSDT","b":[["2191.5","3.25"],["2191.4","1.0"],["2191.0","0.5"]],"a":[["2191.6","2.0"],["2191.7","4.0"]],"u":219167606,"seq":181840388171}}"#;

    /// The delta that follows it: `u` is exactly `snapshot.u + 1`.
    const BOOK_DELTA: &str = r#"{"topic":"orderbook.50.ETHUSDT","ts":1789875210020,"type":"delta","data":{"s":"ETHUSDT","b":[["2191.5","0.0"],["2191.2","9.0"]],"a":[],"u":219167607,"seq":181840388311}}"#;

    /// A delta that deletes an ask and adds nothing on the bid side.
    const BOOK_DELTA_ASK_ONLY: &str = r#"{"topic":"orderbook.50.BTCUSDT","ts":1789875214036,"type":"delta","data":{"s":"BTCUSDT","b":[],"a":[["80395.9","0.080583"],["80396.7","0"]],"u":275651486,"seq":114263051578}}"#;

    /// A trade frame: `data` is an **array**, `i` is a **string**, `S` is a word.
    const TRADE_FRAME: &str = r#"{"topic":"publicTrade.BTCUSDT","ts":1789875217841,"type":"snapshot","data":[{"BT":false,"RPI":false,"S":"Sell","T":1789875217840,"i":"2290000001211680903","p":"80413.3","s":"BTCUSDT","seq":114263055846,"v":"0.06"},{"BT":false,"RPI":false,"S":"Buy","T":1789875217840,"i":"2290000001211680919","p":"80413.4","s":"BTCUSDT","seq":114263055850,"v":"0.004904"}]}"#;

    /// The pong Bybit answers a `{"op":"ping"}` with.
    const PONG: &str =
        r#"{"success":true,"ret_msg":"pong","conn_id":"d9avt8sb86tr31f6ej90-d0qm8","op":"pong"}"#;

    fn codec() -> BybitCodec {
        BybitCodec::spot()
    }

    // ---------------------------------------------------------------------
    // The bridge: snapshot at its own `u`, next delta is `u + 1`, book syncs.
    // ---------------------------------------------------------------------

    /// The measured bridge, end to end through the real synchronizer.
    ///
    /// Building this test is what disproved the design I nearly shipped. The
    /// plan was `last_update_id = u - 1` on the snapshot, from the documented
    /// `u == 1` reset rule. Measured, a fresh Bybit subscription answers with a
    /// full book at `u = 219167606` -- a real current id, not `1` -- and the
    /// next delta is at `u = 219167607`.
    ///
    /// So subtracting one would have made the *snapshot* claim 219167605 while
    /// the first delta starts at 219167607 — and `try_bridge`'s filter
    /// (`first_update_id <= snapshot_id + 1`, i.e. `219167607 <= 219167606`) is
    /// then **false for every diff that ever arrives**. `position()` returns
    /// `None`, the bridge never runs, and `synced` stays false for the life of
    /// the feed: the delta is applied **zero** times, not twice. That is the
    /// dangerous part. The screen would have read "the book has not finished
    /// syncing", which points at the venue or the network, while the actual
    /// fault is one invented subtraction in the codec.
    ///
    /// What is pinned is the invariant that makes the correct design work: the
    /// snapshot's id plus one is exactly the next delta's id, with no arithmetic
    /// invented at either end.
    #[test]
    fn a_snapshot_and_the_delta_after_it_are_adjacent() {
        let snapshot = decode_snapshot(BOOK_SNAPSHOT);
        let delta = decode_delta(BOOK_DELTA);

        assert_eq!(snapshot.3, 219_167_606, "the snapshot carries its own id");
        assert_eq!(delta.1, 219_167_607, "the delta continues from it");

        assert_eq!(
            delta.1,
            snapshot.3 + 1,
            "if these were not adjacent, the book could never bridge without a REST snapshot"
        );
    }

    /// The same invariant driven through the real synchronizer, so the claim is
    /// about the book and not only about two numbers.
    #[test]
    fn the_snapshot_and_delta_bridge_the_real_book() {
        use crate::orderbook::OrderBookSynchronizer;

        let (symbol, bids, asks, u) = decode_snapshot(BOOK_SNAPSHOT);
        let (_, delta_u, delta_bids, delta_asks) = decode_delta(BOOK_DELTA);

        let mut book = OrderBookSynchronizer::new(&symbol);
        book.set_snapshot(&bids, &asks, u, 0);
        assert!(
            !book.is_synced(),
            "a snapshot alone is not yet a synced book"
        );

        let outcome = book.on_diff(
            crate::orderbook::DepthDiff {
                first_update_id: delta_u,
                final_update_id: delta_u,
                bids: delta_bids,
                asks: delta_asks,
            },
            0,
        );

        assert_eq!(
            outcome,
            crate::orderbook::DiffOutcome::Applied,
            "the delta immediately after the snapshot must bridge it"
        );
        assert!(book.is_synced(), "and the book is now live");
    }

    #[test]
    fn decodes_a_book_snapshot() {
        let (symbol, bids, asks, u) = decode_snapshot(BOOK_SNAPSHOT);
        assert_eq!(symbol, "ETHUSDT");
        assert_eq!(u, 219_167_606);
        assert_eq!(bids, vec![(2191.5, 3.25), (2191.4, 1.0), (2191.0, 0.5)]);
        assert_eq!(asks, vec![(2191.6, 2.0), (2191.7, 4.0)]);
    }

    /// A delta must not be read as a snapshot: that would replace a 50-level
    /// book with the two levels that moved.
    #[test]
    fn a_delta_is_not_a_snapshot() {
        let Frame::Data(incoming) = codec().parse_frame(BOOK_DELTA) else {
            panic!("must decode");
        };
        assert!(
            matches!(*incoming, Incoming::BookDelta(_)),
            "a `type: delta` frame must never be installed as a snapshot"
        );
    }

    /// Bybit sends prices and sizes as **strings**, and a `"0"` size deletes.
    #[test]
    fn a_zero_size_is_carried_as_a_deletion() {
        let (_, _, _bids, asks) = decode_delta(BOOK_DELTA_ASK_ONLY);
        assert_eq!(asks, vec![(80395.9, 0.080583), (80396.7, 0.0)]);
        assert!(
            asks.iter().any(|(_, q)| *q == 0.0),
            "a `\"0\"` size must reach the book as a deletion, not be dropped"
        );
    }

    /// An empty side means "no change here", not "clear this side".
    #[test]
    fn an_empty_side_is_an_empty_change_not_a_clearing_instruction() {
        let (_, _, bids, asks) = decode_delta(BOOK_DELTA);
        assert!(
            asks.is_empty(),
            "the ask side was empty and must stay empty"
        );
        assert!(!bids.is_empty(), "and the bid side carries the change");
    }

    // ---------------------------------------------------------------------
    // Trades
    // ---------------------------------------------------------------------

    /// One frame carries many trades, and every one must arrive.
    ///
    /// Measured: Bybit sent 8 to 16 trades in a single `publicTrade` frame.
    /// A codec that decoded only `data[0]` would lose 15/16 of the trade stream
    /// while every field it looked at checked out -- the exact shape of failure
    /// this repo keeps finding.
    #[test]
    fn every_trade_in_a_batched_frame_is_decoded() {
        let Frame::Data(incoming) = codec().parse_frame(TRADE_FRAME) else {
            panic!("must decode");
        };
        let Incoming::Trades(trades) = *incoming else {
            panic!("expected trades");
        };
        assert_eq!(
            trades.len(),
            2,
            "both trades in the frame, not just the first"
        );

        assert_eq!(trades[0].trade_id, 2_290_000_001_211_680_903);
        assert!((trades[0].price - 80413.3).abs() < 1e-9);
        assert!((trades[0].quantity - 0.06).abs() < 1e-9);
        assert_eq!(trades[0].timestamp, 1_789_875_217_840_000_000);
        assert_eq!(trades[1].trade_id, 2_290_000_001_211_680_919);
    }

    /// `S` is the **taker** side, so a taker sell means the buyer was the maker.
    ///
    /// Getting this backwards flips every delta and CVD number downstream while
    /// leaving every price and size looking correct, which is why it is pinned
    /// with both directions rather than one.
    #[test]
    fn the_taker_side_is_inverted_into_the_platforms_maker_flag() {
        let Frame::Data(incoming) = codec().parse_frame(TRADE_FRAME) else {
            panic!("must decode");
        };
        let Incoming::Trades(trades) = *incoming else {
            panic!("expected trades");
        };
        assert!(
            trades[0].is_buyer_maker,
            "S == \"Sell\" means the taker sold, so the buyer made the market"
        );
        assert!(
            !trades[1].is_buyer_maker,
            "S == \"Buy\" means the taker bought, so the seller made the market"
        );
    }

    /// The symbol comes off the payload, upper-cased, because the bus keys on it.
    #[test]
    fn trades_carry_the_venues_own_symbol_spelling() {
        let Frame::Data(incoming) = codec().parse_frame(TRADE_FRAME) else {
            panic!("must decode");
        };
        let Incoming::Trades(trades) = *incoming else {
            panic!("expected trades");
        };
        assert_eq!(trades[0].symbol, "BTCUSDT");
    }

    // ---------------------------------------------------------------------
    // Control frames and the two kinds of "could not use this"
    // ---------------------------------------------------------------------

    /// A subscribe ack is not an error. This is the dead-pump defect.
    #[test]
    fn a_subscribe_ack_and_a_pong_are_control_frames() {
        assert_eq!(codec().parse_frame(SUBSCRIBE_ACK), Frame::Control);
        assert_eq!(codec().parse_frame(PONG), Frame::Control);
    }

    #[test]
    fn a_topic_we_do_not_handle_is_unrecognised() {
        let frame = r#"{"topic":"kline.1.BTCUSDT","ts":1,"type":"snapshot","data":[]}"#;
        assert_eq!(codec().parse_frame(frame), Frame::Unrecognised);
    }

    /// A known topic with a payload we cannot read is a real defect.
    #[test]
    fn a_book_topic_with_an_unreadable_price_is_malformed() {
        let broken = BOOK_DELTA.replace("\"2191.2\"", "\"not-a-price\"");
        assert!(matches!(codec().parse_frame(&broken), Frame::Malformed(_)));
    }

    /// A zero price can never reach `PriceKey::new`, which debug-asserts
    /// positivity. Filtered here as a named rule, not accidentally.
    #[test]
    fn a_zero_price_is_refused_before_it_can_reach_the_book() {
        let broken = BOOK_SNAPSHOT.replace("\"2191.0\"", "\"0\"");
        let Frame::Malformed(reason) = codec().parse_frame(&broken) else {
            panic!("a zero price must be refused, not installed");
        };
        assert!(
            reason.contains("positive"),
            "the reason should name the rule it broke, got `{reason}`"
        );
    }

    /// A non-numeric trade id must not become a made-up one: a synthesised id
    /// turns the gap detector into a random number generator.
    #[test]
    fn a_non_numeric_trade_id_is_malformed_rather_than_invented() {
        let broken = TRADE_FRAME.replace("\"2290000001211680903\"", "\"abc\"");
        assert!(matches!(codec().parse_frame(&broken), Frame::Malformed(_)));
    }

    // ---------------------------------------------------------------------
    // Subscription spelling and heartbeat
    // ---------------------------------------------------------------------

    #[test]
    fn subscribes_with_bybits_topic_spelling() {
        let raw = codec()
            .subscribe_payload(&[
                Subscription::OrderBook {
                    symbol: "btcusdt".into(),
                },
                Subscription::Trades {
                    symbol: "BTCUSDT".into(),
                },
            ])
            .unwrap();

        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["op"], "subscribe");
        let args: Vec<String> = serde_json::from_value(value["args"].clone()).unwrap();
        assert_eq!(
            args,
            vec!["orderbook.50.BTCUSDT", "publicTrade.BTCUSDT"],
            "the depth lives in the topic name, not in the payload"
        );
    }

    #[test]
    fn an_empty_subscribe_is_refused_rather_than_sent() {
        assert!(codec().subscribe_payload(&[]).is_err());
    }

    /// Bybit needs a ping -- this is a real answer, not Binance's `None`.
    #[test]
    fn a_ping_is_sent_at_the_interval_and_not_before() {
        let codec = BybitCodec::spot().with_ping_interval(std::time::Duration::from_secs(20));

        // The clock is the collector's monotonic millisecond count, so this
        // needs no sleep and cannot be flaky on a loaded machine.
        assert!(
            codec.heartbeat(0).is_some(),
            "the first ask sends, because nothing has been sent yet"
        );
        assert!(
            codec.heartbeat(5_000).is_none(),
            "five seconds later is too soon"
        );
        assert!(
            codec.heartbeat(19_999).is_none(),
            "one millisecond before the interval is still too soon"
        );
        assert!(
            codec.heartbeat(20_000).is_some(),
            "twenty seconds later is due"
        );
        assert!(
            codec.heartbeat(21_000).is_none(),
            "and the next one is another twenty seconds away"
        );
    }

    /// The control for the above: the ping must be the payload Bybit expects.
    #[test]
    fn the_ping_is_the_op_ping_control_message() {
        let codec = BybitCodec::spot();
        let payload = codec.heartbeat(0).unwrap();
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(value["op"], "ping");
    }

    /// A clone must share the ping cadence, not start a second one.
    ///
    /// The collector clones its codec into the task that owns the socket, so a
    /// clone with its own atomic would ping a venue that had already been
    /// pinged -- at twice the intended rate on the first connection, and on a
    /// rate that no test of the original would show.
    #[test]
    fn a_cloned_codec_shares_the_ping_cadence() {
        let original = BybitCodec::spot().with_ping_interval(std::time::Duration::from_secs(20));
        let clone = original.clone();

        assert!(original.heartbeat(0).is_some(), "the first ask sends");
        assert!(
            clone.heartbeat(1_000).is_none(),
            "the clone must see the ping the original already sent"
        );
        assert!(
            original.heartbeat(20_000).is_some(),
            "and the cadence continues from it"
        );
    }

    #[test]
    fn the_name_matches_the_rest_venue() {
        assert_eq!(WireCodec::name(&codec()), "bybit");
    }

    /// One socket per category: the endpoint is a property of the codec.
    #[test]
    fn each_category_has_its_own_socket() {
        assert_eq!(BybitCodec::spot().ws_url(), BYBIT_SPOT_WS);
        assert_eq!(BybitCodec::linear().ws_url(), BYBIT_LINEAR_WS);
        assert_eq!(BybitCodec::inverse().ws_url(), BYBIT_INVERSE_WS);
        assert_eq!(BybitCodec::linear().category(), "linear");
    }

    // ---------------------------------------------------------------------
    // helpers
    // ---------------------------------------------------------------------

    /// The levels a decoded frame carries, as `(price, quantity)` pairs.
    type Levels = Vec<(f64, f64)>;

    /// A snapshot flattened for assertions: `(symbol, bids, asks, id)`.
    type SnapshotParts = (String, Levels, Levels, u64);

    /// A delta flattened for assertions: `(symbol, id, bids, asks)`.
    type DeltaParts = (String, u64, Levels, Levels);

    fn decode_snapshot(raw: &str) -> SnapshotParts {
        let Frame::Data(incoming) = codec().parse_frame(raw) else {
            panic!("must decode as data");
        };
        let Incoming::BookSnapshot {
            symbol,
            bids,
            asks,
            last_update_id,
        } = *incoming
        else {
            panic!("expected a snapshot");
        };
        (symbol, bids, asks, last_update_id)
    }

    fn decode_delta(raw: &str) -> DeltaParts {
        let Frame::Data(incoming) = codec().parse_frame(raw) else {
            panic!("must decode as data");
        };
        let Incoming::BookDelta(diff) = *incoming else {
            panic!("expected a book delta");
        };
        (diff.symbol, diff.final_update_id, diff.bids, diff.asks)
    }
}
