//! The sandbox itself: the wasmtime engine, one instance's lifetime, and the
//! accounting of what an evaluation cost.
//!
//! ## Three limits, and why all three
//!
//! * **Fuel** bounds instructions. It is the only one that fails *identically*
//!   on every machine, which is what a trading system needs: "this document is
//!   too expensive" has to be a property of the document, not of the CPU.
//! * **Epoch** bounds wall clock. It catches the case fuel cannot -- a host
//!   function that blocks, or a module that does something expensive per
//!   instruction.
//! * **Memory** bounds linear memory, enforced by wasmtime's own limiter rather
//!   than by the guest, because the guest is the thing being limited.
//!
//! ## One instance, many candles
//!
//! [`Session`] holds a live instance across a whole replay. That is not an
//! optimization for its own sake: the interpreter accumulates `candles_seen` and
//! the record of skipped conditions, and re-instantiating per candle would throw
//! that away and make a sandboxed run disagree with a native one for reasons
//! that have nothing to do with isolation.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use strategy_dsl::ValidatedStrategy;
use strategy_runtime::context::MarketContext;
use strategy_runtime::signal::Signal;
use wasmtime::{Config, Engine, Instance, Linker, Memory, Module, Store};

use crate::allowlist;
use crate::error::SandboxError;
use crate::host::{self, HostState};
use crate::limits::SandboxLimits;

/// ABI version this host speaks. Must match the guest's.
pub const ABI_VERSION: u32 = 1;

/// How often the epoch ticker advances, in milliseconds.
const TICK_MS: u64 = 1;

/// The guest module, compiled by `build.rs` and embedded in this binary.
const GUEST_WASM: &[u8] = include_bytes!(env!("SANDBOX_GUEST_WASM"));

/// The embedded interpreter module.
#[must_use]
pub fn guest_wasm() -> &'static [u8] {
    GUEST_WASM
}

/// How every sandbox engine is configured.
///
/// Deliberately short. `consume_fuel` and `epoch_interruption` are the two that
/// matter; everything else is left at wasmtime's defaults on purpose.
/// Restricting the *instruction set* -- turning off SIMD or reference types --
/// would be the kind of hardening that looks good and breaks the compiler's
/// output in ways that surface months later, and the isolation here does not
/// depend on it: a module that cannot import anything cannot reach anything,
/// whatever instructions it runs.
#[must_use]
pub fn engine_config() -> Config {
    let mut config = Config::new();
    config.consume_fuel(true);
    config.epoch_interruption(true);
    config
}

/// What one evaluation cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResourceUsage {
    /// Instructions the guest consumed.
    pub fuel_consumed: u64,
    /// Instructions the guest had.
    pub fuel_budget: u64,
    /// Wall-clock time the evaluation took.
    pub elapsed: Duration,
    /// Linear memory the instance is holding, in bytes.
    pub memory_bytes: usize,
}

/// What a sandboxed execution produced.
///
/// `errors` being non-empty means the run is **not** trustworthy: a sandboxed
/// evaluation that failed part-way produced fewer signals than a native one
/// would have, and nothing downstream can tell the difference from a strategy
/// that simply chose not to trade. Callers that compare the two must check it.
#[derive(Debug, Clone)]
pub struct SandboxExecutionResult {
    /// Signals the guest emitted, in order.
    pub signals: Vec<Signal>,
    /// Everything that went wrong, in order.
    pub errors: Vec<SandboxError>,
    /// What the evaluation cost.
    pub resource_usage: ResourceUsage,
    /// Requests the host refused, in the guest's own words.
    pub denials: Vec<String>,
}

impl SandboxExecutionResult {
    /// Whether the evaluation ran to completion with nothing refused.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.errors.is_empty() && self.denials.is_empty()
    }
}

/// A compiled sandbox: the engine, the checked module and the limits.
///
/// Build once, then [`Sandbox::start`] a [`Session`] per run. The engine is what
/// holds the compiled code, and it is shared by every session.
pub struct Sandbox {
    engine: Engine,
    module: Module,
    limits: SandboxLimits,
    /// Advances the engine's epoch on a timer when this sandbox needs an
    /// in-process wall-clock limit. The supervised worker relies on its parent
    /// process deadline instead and therefore has no ticker thread.
    _ticker: Option<EpochTicker>,
}

impl Sandbox {
    /// The shipped interpreter, with default limits.
    ///
    /// # Errors
    ///
    /// Only if the embedded module fails to compile, which would be a build
    /// problem rather than a runtime one.
    pub fn new() -> Result<Self, SandboxError> {
        Self::with_limits(SandboxLimits::default())
    }

    /// Build a sandbox for a supervised worker process without an epoch
    /// ticker thread.
    ///
    /// The worker's parent must enforce a hard process deadline. Fuel and
    /// memory limits remain active; the process deadline replaces the ticker's
    /// wall-clock interruption for this single-threaded worker configuration.
    ///
    /// # Errors
    ///
    /// As [`Sandbox::new`].
    pub fn new_for_supervised_worker() -> Result<Self, SandboxError> {
        Self::from_wasm_with_epoch_ticker(GUEST_WASM, SandboxLimits::default(), false)
    }

    /// The shipped interpreter, with explicit limits.
    ///
    /// # Errors
    ///
    /// As [`Sandbox::new`].
    pub fn with_limits(limits: SandboxLimits) -> Result<Self, SandboxError> {
        Self::from_wasm(GUEST_WASM, limits)
    }

    /// A sandbox around an arbitrary module.
    ///
    /// Used by the adversarial tests to hand the host modules a Rust compiler
    /// would never produce. The allowlist check runs here, so a module that
    /// imports anything else is refused before it is ever instantiated.
    ///
    /// # Errors
    ///
    /// [`SandboxError::Instantiation`] if the bytes are not a valid module, or
    /// [`SandboxError::CapabilityDenied`] if it imports something it may not.
    pub fn from_wasm(wasm: &[u8], limits: SandboxLimits) -> Result<Self, SandboxError> {
        Self::from_wasm_with_epoch_ticker(wasm, limits, true)
    }

    fn from_wasm_with_epoch_ticker(
        wasm: &[u8],
        limits: SandboxLimits,
        use_epoch_ticker: bool,
    ) -> Result<Self, SandboxError> {
        let engine = Engine::new(&engine_config())
            .map_err(|error| SandboxError::Instantiation(error.to_string()))?;
        let module = Module::new(&engine, wasm)
            .map_err(|error| SandboxError::Instantiation(error.to_string()))?;

        allowlist::check_module(&module)?;

        let ticker = if use_epoch_ticker {
            Some(EpochTicker::start(engine.clone())?)
        } else {
            None
        };
        Ok(Self {
            engine,
            module,
            limits,
            _ticker: ticker,
        })
    }

    /// The ceilings this sandbox enforces.
    #[must_use]
    pub const fn limits(&self) -> SandboxLimits {
        self.limits
    }

    /// The compiled module.
    #[must_use]
    pub const fn module(&self) -> &Module {
        &self.module
    }

    /// Instantiate the interpreter and load `document` into it.
    ///
    /// The returned [`Session`] borrows nothing: it is `'static`, so a caller
    /// may own it for as long as it likes -- which is what lets a long-running
    /// bot hold one. `&self` is still borrowed for the duration of the call,
    /// because starting a session reads the compiled module.
    ///
    /// # Errors
    ///
    /// [`SandboxError::AbiMismatch`] if the module speaks a different protocol,
    /// or [`SandboxError::Guest`] if the document is refused.
    pub fn start(&self, document: &ValidatedStrategy) -> Result<Session, SandboxError> {
        Session::start(self, document)
    }

    /// Evaluate one context in a fresh instance.
    ///
    /// This is the spec's `execute_in_sandbox` (`docs/08-SANDBOX-WASM.md`),
    /// with one deliberate difference: it takes `&self` and a
    /// [`ValidatedStrategy`] rather than building an engine per call from a raw
    /// [`StrategyDocument`](strategy_dsl::StrategyDocument). Compiling a module
    /// and instantiating an interpreter per candle would be slower by orders of
    /// magnitude, and the repo's rule is that nothing executes without passing
    /// validation -- expressing that as a type is how the rule is enforced
    /// rather than remembered.
    ///
    /// For a run over many candles, prefer [`Sandbox::start`]: a [`Session`]
    /// keeps the interpreter's own counters, which a per-call instance resets.
    #[must_use]
    pub fn execute(
        &self,
        document: &ValidatedStrategy,
        context: &MarketContext,
    ) -> SandboxExecutionResult {
        let mut errors: Vec<SandboxError> = Vec::new();
        let mut session = match self.start(document) {
            Ok(session) => session,
            Err(error) => {
                return SandboxExecutionResult {
                    signals: Vec::new(),
                    errors: vec![error],
                    resource_usage: ResourceUsage::default(),
                    denials: Vec::new(),
                };
            }
        };

        let signals = match session.evaluate(context) {
            Ok(signals) => signals,
            Err(error) => {
                errors.push(error);
                Vec::new()
            }
        };

        SandboxExecutionResult {
            signals,
            errors,
            resource_usage: session.usage(),
            denials: session.denials().to_vec(),
        }
    }

    /// Execute a Pine-Lite preview inside the bounded Wasmtime guest.
    ///
    /// The request contains only source plus host-fetched market data; the
    /// guest has no market, network, filesystem, or trading host capability.
    pub fn execute_pine_preview(
        &self,
        request: sandbox_guest::pine::Request,
    ) -> Result<(sandbox_guest::pine::Preview, ResourceUsage), SandboxError> {
        const MAX_CANDLES: usize = 50_000;
        const MAX_TOTAL_SERIES_CANDLES: usize = 100_000;
        const MAX_SERIES_POOLS: usize = 8;
        const MAX_DATA_SERIES: usize = 64;
        const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
        const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;

        if request.candles.is_empty() || request.candles.len() > MAX_CANDLES {
            return Err(SandboxError::Guest(format!(
                "Pine-Lite preview requires 1..={MAX_CANDLES} chart candles"
            )));
        }
        if request.inputs.series_pool.len() > MAX_SERIES_POOLS
            || request.inputs.data_series.len() > MAX_DATA_SERIES
        {
            return Err(SandboxError::Guest(format!(
                "Pine-Lite preview permits at most {MAX_SERIES_POOLS} market pools and {MAX_DATA_SERIES} provider series"
            )));
        }
        let mut total_series_candles = request.candles.len();
        for series in request
            .inputs
            .series_pool
            .values()
            .chain(std::iter::once(&request.inputs.security))
        {
            if series.len() > MAX_CANDLES {
                return Err(SandboxError::Guest(format!(
                    "a Pine-Lite market series exceeds {MAX_CANDLES} candles"
                )));
            }
            total_series_candles = total_series_candles.saturating_add(series.len());
        }
        for series in request.inputs.data_series.values() {
            if series.len() > MAX_CANDLES {
                return Err(SandboxError::Guest(format!(
                    "a Pine-Lite provider series exceeds {MAX_CANDLES} values"
                )));
            }
            total_series_candles = total_series_candles.saturating_add(series.len());
        }
        if request.inputs.numbers.len() > 256
            || request.inputs.bools.len() > 256
            || request.inputs.strings.len() > 256
        {
            return Err(SandboxError::Guest(
                "Pine-Lite preview input settings exceed 256 values per type".into(),
            ));
        }
        if total_series_candles > MAX_TOTAL_SERIES_CANDLES {
            return Err(SandboxError::Guest(format!(
                "Pine-Lite preview market data exceeds {MAX_TOTAL_SERIES_CANDLES} candles"
            )));
        }

        let payload = serde_json::to_vec(&request)
            .map_err(|error| SandboxError::Instantiation(error.to_string()))?;
        if payload.len() > MAX_REQUEST_BYTES {
            return Err(SandboxError::Guest(format!(
                "Pine-Lite preview input exceeds {MAX_REQUEST_BYTES} bytes"
            )));
        }

        let state = HostState::new(self.limits);
        let mut store = Store::new(&self.engine, state);
        store.limiter(|state| state.limiter());
        let mut linker: Linker<HostState> = Linker::new(&self.engine);
        host::link(&mut linker).map_err(|error| SandboxError::Instantiation(error.to_string()))?;
        let instance = linker
            .instantiate(&mut store, &self.module)
            .map_err(|error| SandboxError::Instantiation(error.to_string()))?;
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| SandboxError::MissingExport("memory".into()))?;

        store
            .set_fuel(self.limits.fuel)
            .map_err(|error| SandboxError::Instantiation(error.to_string()))?;
        let deadline_ticks = u64::try_from(self.limits.timeout.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        store.set_epoch_deadline(deadline_ticks);
        let started = Instant::now();

        let result = (|| {
            let alloc = instance
                .get_typed_func::<i32, i32>(&mut store, "sbx_alloc")
                .map_err(|_| SandboxError::MissingExport("sbx_alloc".into()))?;
            let free = instance
                .get_typed_func::<(i32, i32), ()>(&mut store, "sbx_free")
                .map_err(|_| SandboxError::MissingExport("sbx_free".into()))?;
            let eval = instance
                .get_typed_func::<(i32, i32), i32>(&mut store, "pl_eval")
                .map_err(|_| SandboxError::MissingExport("pl_eval".into()))?;
            let result_ptr = instance
                .get_typed_func::<(), i32>(&mut store, "pl_result_ptr")
                .map_err(|_| SandboxError::MissingExport("pl_result_ptr".into()))?;
            let result_len = instance
                .get_typed_func::<(), i32>(&mut store, "pl_result_len")
                .map_err(|_| SandboxError::MissingExport("pl_result_len".into()))?;

            let payload_len = i32::try_from(payload.len())
                .map_err(|_| SandboxError::Guest("Pine-Lite preview input is too large".into()))?;
            let ptr = alloc.call(&mut store, payload_len).map_err(|error| {
                classify_pine_error(&store, self.limits, &error, started.elapsed())
            })?;
            if ptr <= 0 {
                return Err(SandboxError::MemoryLimitExceeded {
                    limit: self.limits.max_memory_bytes,
                });
            }
            memory
                .write(&mut store, ptr as usize, &payload)
                .map_err(|error| SandboxError::Trap(error.to_string()))?;

            let code = eval.call(&mut store, (ptr, payload_len)).map_err(|error| {
                classify_pine_error(&store, self.limits, &error, started.elapsed())
            });
            let code = match code {
                Ok(code) => code,
                Err(error) => {
                    let _ = free.call(&mut store, (ptr, payload_len));
                    return Err(error);
                }
            };
            let output_ptr = result_ptr.call(&mut store, ()).map_err(|error| {
                classify_pine_error(&store, self.limits, &error, started.elapsed())
            })?;
            let output_len = result_len.call(&mut store, ()).map_err(|error| {
                classify_pine_error(&store, self.limits, &error, started.elapsed())
            })?;
            if output_ptr <= 0 || output_len < 0 || output_len as usize > MAX_RESULT_BYTES {
                let _ = free.call(&mut store, (ptr, payload_len));
                return Err(SandboxError::Guest(
                    "the Pine-Lite guest returned an invalid or oversized result".into(),
                ));
            }
            let start = output_ptr as usize;
            let end = start.checked_add(output_len as usize).ok_or_else(|| {
                SandboxError::Guest("the Pine-Lite guest result range overflowed".into())
            })?;
            let bytes = memory
                .data(&store)
                .get(start..end)
                .ok_or_else(|| {
                    SandboxError::Guest("the Pine-Lite guest result is out of bounds".into())
                })?
                .to_vec();
            free.call(&mut store, (ptr, payload_len)).map_err(|error| {
                classify_pine_error(&store, self.limits, &error, started.elapsed())
            })?;
            if code != 0 {
                let message = String::from_utf8_lossy(&bytes).into_owned();
                return Err(SandboxError::Guest(message));
            }
            let preview = serde_json::from_slice(&bytes).map_err(|error| {
                SandboxError::Guest(format!("invalid Pine-Lite guest response: {error}"))
            })?;
            Ok(preview)
        })();

        let remaining = store.get_fuel().unwrap_or(0);
        let usage = ResourceUsage {
            fuel_budget: self.limits.fuel,
            fuel_consumed: self.limits.fuel.saturating_sub(remaining),
            elapsed: started.elapsed(),
            memory_bytes: memory.data_size(&store),
        };
        result.map(|preview| (preview, usage))
    }
}

fn classify_pine_error(
    store: &Store<HostState>,
    limits: SandboxLimits,
    error: &wasmtime::Error,
    elapsed: Duration,
) -> SandboxError {
    if store.get_fuel().is_ok_and(|fuel| fuel == 0) {
        return SandboxError::FuelExhausted {
            used: limits.fuel,
            budget: limits.fuel,
        };
    }
    if elapsed >= limits.timeout {
        return SandboxError::Timeout(limits.timeout);
    }
    SandboxError::Trap(error.to_string())
}

/// A live sandboxed interpreter, holding its state across many candles.
pub struct Session {
    /// The ceilings this session was started with, **copied out of** the
    /// [`Sandbox`] rather than borrowed from it.
    ///
    /// This one field is the difference between a session that borrows its
    /// sandbox and one that owns everything it needs, and it is what lets a
    /// *bot* run sandboxed. A `PaperBot` cannot hold a borrow of a `Sandbox` it
    /// does not own, and making it own both would be a self-referential struct;
    /// the alternative would be an `Arc<Sandbox>` per bot and a session that
    /// still borrowed it. Nothing else here refers to the sandbox -- the
    /// `Store` holds its own handle to the engine -- so copying the limits
    /// removes the borrow entirely rather than hiding it.
    limits: SandboxLimits,
    store: Store<HostState>,
    instance: Instance,
    memory: Memory,
    usage: ResourceUsage,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("limits", &self.limits)
            .field("usage", &self.usage)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Instantiate and initialise.
    fn start(sandbox: &Sandbox, document: &ValidatedStrategy) -> Result<Self, SandboxError> {
        let mut state = HostState::new(sandbox.limits);
        // The declared set comes from the document, so the host can tell a
        // timeframe that is merely warming up from one that was never declared.
        state.declare(document.document().timeframes.keys().cloned());

        let mut store = Store::new(&sandbox.engine, state);
        store.limiter(|state| state.limiter());

        let mut linker: Linker<HostState> = Linker::new(&sandbox.engine);
        host::link(&mut linker).map_err(|error| SandboxError::Instantiation(error.to_string()))?;

        let instance = linker
            .instantiate(&mut store, &sandbox.module)
            .map_err(|error| SandboxError::Instantiation(error.to_string()))?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| SandboxError::MissingExport("memory".into()))?;

        let mut session = Self {
            limits: sandbox.limits,
            store,
            instance,
            memory,
            usage: ResourceUsage::default(),
        };

        session.check_exports()?;
        session.check_abi()?;
        session.load(document)?;
        Ok(session)
    }

    /// Refuse a module that does not export the whole ABI, before any document
    /// is loaded into it.
    ///
    /// The alternative is to discover the gap at the first `evaluate` -- after
    /// the document has been accepted and the caller believes the session is
    /// live. A module that cannot answer a question is not a strategy that does
    /// nothing; it is a broken module, and the honest place to say so is at load
    /// time, beside the import check, for the same reason that check lives
    /// there: a refusal the caller sees before it has invested anything is worth
    /// more than one it sees later.
    ///
    /// Signatures are checked here too. A module exporting `sbx_eval` as
    /// `(i32) -> i32` has the name but not the function, and reporting that as
    /// "missing" would be the wrong word for the right refusal.
    fn check_exports(&mut self) -> Result<(), SandboxError> {
        fn absent(name: &str) -> SandboxError {
            SandboxError::MissingExport(format!(
                "{name} (absent, or exported with a different signature)"
            ))
        }

        self.instance
            .get_typed_func::<(), i32>(&mut self.store, "sbx_abi_version")
            .map_err(|_| absent("sbx_abi_version"))?;
        self.instance
            .get_typed_func::<i32, i32>(&mut self.store, "sbx_alloc")
            .map_err(|_| absent("sbx_alloc"))?;
        self.instance
            .get_typed_func::<(i32, i32), ()>(&mut self.store, "sbx_free")
            .map_err(|_| absent("sbx_free"))?;
        self.instance
            .get_typed_func::<(i32, i32), i32>(&mut self.store, "sbx_init")
            .map_err(|_| absent("sbx_init"))?;
        self.instance
            .get_typed_func::<(i32, i32), i32>(&mut self.store, "sbx_eval")
            .map_err(|_| absent("sbx_eval"))?;
        self.instance
            .get_typed_func::<(), i32>(&mut self.store, "sbx_error_ptr")
            .map_err(|_| absent("sbx_error_ptr"))?;
        self.instance
            .get_typed_func::<(), i32>(&mut self.store, "sbx_error_len")
            .map_err(|_| absent("sbx_error_len"))?;
        Ok(())
    }

    /// Refuse a module built against a protocol this host does not know.
    fn check_abi(&mut self) -> Result<(), SandboxError> {
        self.arm()?;
        let version = self
            .instance
            .get_typed_func::<(), i32>(&mut self.store, "sbx_abi_version")
            .map_err(|_| SandboxError::MissingExport("sbx_abi_version".into()))?
            .call(&mut self.store, ())
            .map_err(|error| self.classify(&error, Duration::ZERO))?;

        // Signed on the wire; the version is a small positive number.
        if version as u32 == ABI_VERSION {
            Ok(())
        } else {
            Err(SandboxError::AbiMismatch {
                found: version as u32,
                expected: ABI_VERSION,
            })
        }
    }

    /// Give the instance its budget and its deadline.
    ///
    /// Must run before *every* call into the guest, not just `sbx_eval`. A
    /// wasmtime store starts with no fuel at all, so a call made before arming
    /// traps with `OutOfFuel` on its first instruction -- which is how the
    /// sandbox reported an exhausted budget for a document it had not yet read.
    fn arm(&mut self) -> Result<(), SandboxError> {
        self.store
            .set_fuel(self.limits.fuel)
            .map_err(|error| SandboxError::Instantiation(error.to_string()))?;
        self.store.set_epoch_deadline(self.deadline_ticks());
        Ok(())
    }

    /// Hand the document over.
    fn load(&mut self, document: &ValidatedStrategy) -> Result<(), SandboxError> {
        let payload = sandbox_guest::interpret::InitPayload {
            document: document.document().clone(),
            runtime: document_runtime(document),
        };
        let json = serde_json::to_vec(&payload)
            .map_err(|error| SandboxError::Instantiation(error.to_string()))?;

        self.arm()?;
        let ptr = self.write(&json)?;
        let len = u32::try_from(json.len()).map_err(|_| {
            SandboxError::Instantiation("document payload is absurdly large".into())
        })?;
        let code = self.call_init(ptr, len)?;
        self.finish(code, Duration::ZERO)
    }

    /// Evaluate one candle.
    ///
    /// # Errors
    ///
    /// Any [`SandboxError`]: a refusal from the guest, an exhausted limit, or a
    /// trap. In every case the host is intact and this session can be inspected.
    pub fn evaluate(&mut self, context: &MarketContext) -> Result<Vec<Signal>, SandboxError> {
        self.store.data_mut().begin(context.clone());
        self.arm()?;

        let header = sandbox_guest::interpret::Header {
            symbol: context.symbol.clone(),
            now: context.now,
            equity: context.equity,
            position: context.position.clone(),
            registry_concepts: context.registry_concepts.clone(),
        };
        let payload = serde_json::to_vec(&header)
            .map_err(|error| SandboxError::Instantiation(error.to_string()))?;

        let ptr = self.write(&payload)?;
        let len = u32::try_from(payload.len())
            .map_err(|_| SandboxError::Instantiation("header is absurdly large".into()))?;

        let started = Instant::now();
        let eval = self
            .instance
            .get_typed_func::<(i32, i32), i32>(&mut self.store, "sbx_eval")
            .map_err(|_| SandboxError::MissingExport("sbx_eval".into()))?;
        let outcome = eval.call(&mut self.store, (ptr as i32, len as i32));
        let elapsed = started.elapsed();

        self.record(elapsed);

        let code = outcome.map_err(|error| self.classify(&error, elapsed))?;
        self.finish(code, elapsed)?;
        Ok(self.store.data_mut().take_signals())
    }

    /// What this session has consumed so far, summed over every evaluation.
    #[must_use]
    pub const fn usage(&self) -> ResourceUsage {
        self.usage
    }

    /// Requests the host refused during the last evaluation.
    #[must_use]
    pub fn denials(&self) -> &[String] {
        self.store.data().denials()
    }

    /// Writes the host refused for exceeding a ceiling.
    #[must_use]
    pub fn refusals(&self) -> &[String] {
        self.store.data().refusals()
    }

    /// The guest's scoped key-value store.
    #[must_use]
    pub fn scoped_state(&self) -> &std::collections::BTreeMap<String, String> {
        self.store.data().scoped_state()
    }

    /// Linear memory the instance is holding.
    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        self.memory.data_size(&self.store)
    }

    /// Turn a guest return code into a result.
    fn finish(&mut self, code: i32, elapsed: Duration) -> Result<(), SandboxError> {
        if code == 0 {
            return Ok(());
        }
        let message = self
            .read_error()
            .unwrap_or_else(|| format!("the guest returned {code} without leaving a message"));
        // A guest that reported an error *after* burning its fuel is a fuel
        // problem, not a logic problem; say the more actionable thing.
        if self.store.get_fuel().is_ok_and(|fuel| fuel == 0) {
            return Err(self.exhausted());
        }
        let _ = elapsed;
        Err(SandboxError::Guest(message))
    }

    /// Read the guest's last error message back out of its memory.
    fn read_error(&mut self) -> Option<String> {
        let len = self
            .instance
            .get_typed_func::<(), i32>(&mut self.store, "sbx_error_len")
            .ok()?
            .call(&mut self.store, ())
            .ok()?;
        let ptr = self
            .instance
            .get_typed_func::<(), i32>(&mut self.store, "sbx_error_ptr")
            .ok()?
            .call(&mut self.store, ())
            .ok()?;

        if ptr <= 0 || len <= 0 {
            return None;
        }
        let ceiling = self.limits.max_message_bytes;
        let len = usize::try_from(len as u32).ok()?.min(ceiling);
        let start = usize::try_from(ptr as u32).ok()?;
        let end = start.checked_add(len)?;

        let data = self.memory.data(&self.store);
        let bytes = data.get(start..end)?;
        Some(String::from_utf8_lossy(bytes).into_owned())
    }

    /// Allocate in the guest and copy `bytes` there.
    fn write(&mut self, bytes: &[u8]) -> Result<u32, SandboxError> {
        let len = u32::try_from(bytes.len())
            .map_err(|_| SandboxError::Instantiation("payload is absurdly large".into()))?;

        let alloc = self
            .instance
            .get_typed_func::<i32, i32>(&mut self.store, "sbx_alloc")
            .map_err(|_| SandboxError::MissingExport("sbx_alloc".into()))?;
        let ptr = alloc
            .call(&mut self.store, len as i32)
            .map_err(|error| self.classify(&error, Duration::ZERO))?;

        if ptr == 0 {
            return Err(SandboxError::MemoryLimitExceeded {
                limit: self.limits.max_memory_bytes,
            });
        }

        self.memory
            .write(&mut self.store, ptr as usize, bytes)
            .map_err(|error| SandboxError::Trap(error.to_string()))?;
        Ok(ptr as u32)
    }

    /// Call `sbx_init`, classifying a trap the same way `sbx_eval` does.
    fn call_init(&mut self, ptr: u32, len: u32) -> Result<i32, SandboxError> {
        let init = self
            .instance
            .get_typed_func::<(i32, i32), i32>(&mut self.store, "sbx_init")
            .map_err(|_| SandboxError::MissingExport("sbx_init".into()))?;
        init.call(&mut self.store, (ptr as i32, len as i32))
            .map_err(|error| self.classify(&error, Duration::ZERO))
    }

    /// Fold this evaluation's cost into the session totals.
    fn record(&mut self, elapsed: Duration) {
        let remaining = self.store.get_fuel().unwrap_or(0);
        self.usage.fuel_budget = self.limits.fuel;
        self.usage.fuel_consumed = self.limits.fuel.saturating_sub(remaining);
        self.usage.elapsed += elapsed;
        self.usage.memory_bytes = self.memory_bytes();
    }

    /// Epoch ticks that add up to the wall-clock ceiling.
    fn deadline_ticks(&self) -> u64 {
        let millis = u64::try_from(self.limits.timeout.as_millis()).unwrap_or(u64::MAX);
        (millis / TICK_MS).max(1)
    }

    fn exhausted(&self) -> SandboxError {
        SandboxError::FuelExhausted {
            used: self.usage.fuel_consumed,
            budget: self.limits.fuel,
        }
    }

    /// Work out *why* a call failed.
    ///
    /// Matching on wasmtime's trap variants would be more direct, but the
    /// variant names have moved between releases. Reading the store's own state
    /// -- was the fuel gone, was the deadline passed -- gives the same answer
    /// and does not depend on which version is in the lockfile.
    fn classify(&self, error: &wasmtime::Error, elapsed: Duration) -> SandboxError {
        if self.store.get_fuel().is_ok_and(|fuel| fuel == 0) {
            return self.exhausted();
        }
        if elapsed >= self.limits.timeout {
            return SandboxError::Timeout(self.limits.timeout);
        }
        if let Some(trap) = error.downcast_ref::<wasmtime::Trap>() {
            return SandboxError::Trap(trap.to_string());
        }
        SandboxError::Trap(error.to_string())
    }
}

/// The runtime configuration the native path would have used.
///
/// Defaults, because that is what `StrategyEngine::new` gets on the native path
/// in every caller in this repo. It is sent explicitly rather than defaulted
/// inside the guest so that the two cannot silently disagree if that changes.
fn document_runtime(_document: &ValidatedStrategy) -> strategy_runtime::RuntimeConfig {
    strategy_runtime::RuntimeConfig::default()
}

/// Advances the engine's epoch on a timer, so `set_epoch_deadline` has
/// something to count.
struct EpochTicker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl EpochTicker {
    fn start(engine: Engine) -> Result<Self, SandboxError> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);

        let handle = std::thread::Builder::new()
            .name("sandbox-epoch".into())
            .spawn(move || {
                let tick = Duration::from_millis(TICK_MS);
                while !flag.load(Ordering::Relaxed) {
                    std::thread::sleep(tick);
                    engine.increment_epoch();
                }
            })
            .map_err(|error| {
                SandboxError::Instantiation(format!(
                    "could not start the sandbox epoch ticker: {error}"
                ))
            })?;

        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
