//! The wire seam: what a venue says, separated from how we drive the socket.
//!
//! ## Why this exists beside [`Venue`]
//!
//! [`Venue`](super::venue::Venue) is the **REST** seam: it answers "what does a
//! kline page look like from you" and it is what let a second market be read
//! without a second pager. The live half had no equivalent. `exchanges/mod.rs`
//! claimed a second venue meant "implementing this trait, not touching the
//! analytics, persistence or agent layers" -- true for REST, and false for live:
//! the live path named Binance in five places, and two venues differ in ways a
//! collector cannot paper over.
//!
//! The decisive one is **socket topology**. Binance carries many symbols on one
//! combined socket, so a subscription is a stream name and the symbol can be
//! read back out of the frame's `stream` field. Bybit opens **one socket per
//! category** and multiplexes symbols onto it with an `args` array, and its
//! frames carry no stream name at all -- only a `topic` and the symbol inside
//! `data`. A collector written around "one socket, subscribe by name" cannot be
//! pointed at that by changing a URL.
//!
//! So the seam is drawn at the *frame*, not at the socket: this trait knows how
//! to describe a connection and how to decode what comes back, and knows nothing
//! about reconnection, buffering, gap detection or publishing. One collector
//! holds one codec and does all of that once.
//!
//! ## An unknown frame is not an error
//!
//! [`Frame::Unrecognised`] and [`Frame::Malformed`] are deliberately *opposite*
//! answers, and collapsing them is the defect this repo keeps re-finding wearing
//! a fix as a disguise. A venue sends frames we have no rule for -- subscribe
//! acks, pongs, a new topic we did not ask for. Treating those as errors kills
//! the pump, and a dead pump is a dead order book that no assertion notices. A
//! frame on a topic we *do* recognise but cannot decode is a real defect and is
//! worth counting. The collector errors on the second and ignores the first.
//!
//! [`Venue`]: super::venue::Venue

use analytics_core::Trade;

use crate::error::MarketDataError;

/// A named data feed we want from a venue.
///
/// Kept abstract rather than a stream string because the two venues spell the
/// same request differently -- `btcusdt@depth@100ms` against
/// `orderbook.50.BTCUSDT` -- and because a codec must be able to *find* the
/// symbol in a frame that does not repeat it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subscription {
    /// Trades for `symbol`.
    Trades {
        /// Symbol, in whatever case the caller typed.
        symbol: String,
    },
    /// The order-book diff stream for `symbol`.
    ///
    /// Carries no depth: how many levels a venue sends is a property of the
    /// venue's stream name, not of what we asked for, and a codec that cannot
    /// choose is more honest than one given a number it silently ignores.
    OrderBook {
        /// Symbol, in whatever case the caller typed.
        symbol: String,
    },
}

impl Subscription {
    /// The symbol this subscription is for.
    #[must_use]
    pub fn symbol(&self) -> &str {
        match self {
            Self::Trades { symbol } | Self::OrderBook { symbol } => symbol,
        }
    }
}

/// A decoded frame the collector understands.
///
/// Timestamps are **nanoseconds**, the platform's unit, converted by the codec
/// from whatever the venue sends (both send milliseconds).
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// A single trade.
    Trade(Trade),
    /// Several trades from one frame.
    ///
    /// Bybit packs 8 to 16 trades into a single `publicTrade` frame. A codec that
    /// could only report one trade per frame would silently keep 1/16 of the
    /// stream -- every field it checked would look right, which is exactly the
    /// class of failure this repo keeps finding. One extra variant is cheaper
    /// than either dropping trades or teaching the collector about arrays.
    ///
    /// A codec that never batches simply never emits this.
    Trades(Vec<Trade>),
    /// A full order book, to be installed as a snapshot.
    ///
    /// The `last_update_id` is what the book bridges onto, and where it comes
    /// from is venue-specific: Binance's REST `lastUpdateId`, or the update id
    /// the venue puts on the frame itself. Bybit's own `u` is that id, and it is
    /// the value to use -- see `bybit_codec`'s module doc for why subtracting 1
    /// from it would leave the book permanently unsynced.
    BookSnapshot {
        /// Symbol, canonical case for the bus.
        symbol: String,
        /// Bid levels as `(price, quantity)`.
        bids: Vec<(f64, f64)>,
        /// Ask levels as `(price, quantity)`.
        asks: Vec<(f64, f64)>,
        /// The update id the snapshot is current as of.
        last_update_id: u64,
    },
    /// An incremental order-book change.
    BookDelta(DepthDiff),
}

/// One incremental order-book change, normalised across venues.
#[derive(Debug, Clone, PartialEq)]
pub struct DepthDiff {
    /// Symbol, canonical case for the bus.
    pub symbol: String,
    /// First update id in this event.
    ///
    /// Bybit sends only one id per update (`u`), so its codec sets this equal to
    /// `final_update_id` -- an event that spans exactly one id. That is the
    /// honest answer rather than an invented range, and it satisfies the
    /// synchronizer's bridge predicate
    /// (`first <= snapshot + 1 && final > snapshot`) for a contiguous stream,
    /// which is what a per-update id means.
    pub first_update_id: u64,
    /// Final update id in this event.
    pub final_update_id: u64,
    /// Bid level changes. Quantity `0.0` removes the level.
    pub bids: Vec<(f64, f64)>,
    /// Ask level changes.
    pub asks: Vec<(f64, f64)>,
}

/// What a codec made of one incoming frame.
///
/// `Unrecognised` is a normal, expected, non-fatal outcome -- see the module
/// doc. `Malformed` is not.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// A frame we understand.
    Data(Box<Incoming>),
    /// A connection-level frame: a subscribe ack, a pong, an error notice.
    ///
    /// Never fatal. A venue that answers an error to one subscription is still
    /// serving the others.
    Control,
    /// A frame this codec has no rule for.
    ///
    /// Distinct from [`Frame::Control`] only in intent, and distinct from
    /// [`Frame::Malformed`] in *consequence*: the collector ignores both this
    /// and `Control`, but logs this one at debug so a venue that starts sending
    /// a shape we have not seen is findable rather than invisible.
    Unrecognised,
    /// A frame on a topic we recognise that we could not decode.
    ///
    /// Worth an error and a counter: it means our model of the venue has drifted.
    Malformed(String),
}

/// Everything venue-specific about a live market-data connection.
///
/// Implementations are `Send + Sync` because a collector is moved into the task
/// that owns the socket, and `'static` because the reconnect loop outlives every
/// borrow in the caller.
pub trait WireCodec: Send + Sync + std::fmt::Debug {
    /// Venue name, e.g. `"binance"`. Must match the REST [`Venue`](super::venue::Venue)
    /// name for the same exchange, so a log line and a metric label agree.
    fn name(&self) -> &'static str;

    /// The socket to open.
    ///
    /// A venue with several sockets (Bybit: one per category) returns the one
    /// this transport carries. A codec that cannot express its topology in one
    /// URL must not be used with a collector that opens one socket; that is a
    /// type-level constraint a single method cannot enforce, so it is stated
    /// here and checked by each collector's own constructor.
    fn ws_url(&self) -> String;

    /// The text frame that subscribes `subs`.
    ///
    /// # Errors
    /// Returns [`MarketDataError::Normalization`] if the codec cannot express a
    /// subscription (a venue that lacks a topic, for instance).
    fn subscribe_payload(&self, subs: &[Subscription]) -> Result<String, MarketDataError>;

    /// A heartbeat to send now, or `None` if this venue needs none.
    ///
    /// `None` is a real answer, not a gap: Binance keeps the connection alive
    /// from its side and the collector must not invent a ping the venue does not
    /// expect. A codec that returns `Some` is asked on the collector's heartbeat
    /// interval and decides for itself whether this ask is the one that sends.
    ///
    /// `now_ms` is a **monotonic millisecond count** the collector produces from
    /// a single `Instant` it created when the connection opened. It is passed in
    /// rather than read from a clock inside the codec for two reasons: a codec
    /// that consulted `SystemTime` would be untestable without waiting real
    /// seconds, and a codec that stored an `Instant` could not be `Sync` without
    /// a lock. The collector owns the clock; the codec owns the cadence.
    fn heartbeat(&self, now_ms: u64) -> Option<String>;

    /// Decode one text frame.
    ///
    /// Must not fail: every outcome is expressible as a [`Frame`], because a
    /// codec that returns `Err` for a frame it merely does not recognise is how
    /// a pump dies on a subscribe ack.
    fn parse_frame(&self, text: &str) -> Frame;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subscription_knows_its_symbol() {
        assert_eq!(
            Subscription::Trades {
                symbol: "BTCUSDT".into()
            }
            .symbol(),
            "BTCUSDT"
        );
        assert_eq!(
            Subscription::OrderBook {
                symbol: "btcusdt".into()
            }
            .symbol(),
            "btcusdt"
        );
    }

    /// The two "we could not use this" answers must stay distinguishable.
    ///
    /// This is the dead-order-book defect pinned in the type system: if
    /// `Unrecognised` were folded into `Malformed`, every subscribe ack would be
    /// an error, and the collector's response to an error is to end the pump.
    #[test]
    fn unrecognised_and_malformed_are_different_answers() {
        assert_ne!(Frame::Unrecognised, Frame::Malformed("x".into()));
        assert_ne!(
            Frame::Control,
            Frame::Malformed("x".into()),
            "a control frame must never be the malformed path"
        );
    }
}
