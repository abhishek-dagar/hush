//! The QuickJS-ng engine behind [`script::ScriptEngine`] (spec 0004, child
//! 0006). The only crate that names `rquickjs` (spec 0001, script
//! boundary).
//!
//! One runtime and one context per page document. The heap is capped at
//! [`HEAP_LIMIT`], and an interrupt handler stops any task that runs past
//! [`TASK_DEADLINE`] (AC-35). The DOM API is `prelude.js`, built on the
//! natives in `bindings`; the engine keeps one handle into it, the
//! internals it calls for timers, events and sweeping.

mod bindings;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use dom::NodeRef;
use rquickjs::context::EvalOptions;
use rquickjs::{CatchResultExt, CaughtError, Context, Ctx, Function, Object, Persistent, Runtime};
use script::{EventTarget, ScriptEngine, ScriptError, ScriptHost, TimerId};
use tracing::warn;

use crate::bindings::{Host, Unpins};

/// The most memory one page's scripts may use (AC-35).
pub const HEAP_LIMIT: usize = 256 * 1024 * 1024;
/// How long one task may run before it is stopped (AC-35).
pub const TASK_DEADLINE: Duration = Duration::from_secs(5);

const PRELUDE: &str = include_str!("prelude.js");

/// A page's QuickJS runtime and its global object.
pub struct QuickJsEngine {
    /// What `prelude.js` returned. Dropped before the runtime: QuickJS
    /// aborts if a handle outlives it.
    internals: Option<Persistent<Object<'static>>>,
    context: Context,
    runtime: Runtime,
    host: Host,
    unpins: Unpins,
    /// When the running task must stop, read by the interrupt handler.
    deadline: Rc<Cell<Option<Instant>>>,
}

impl QuickJsEngine {
    /// A fresh engine whose DOM is `host`'s document.
    pub fn new(host: Rc<RefCell<dyn ScriptHost>>) -> Result<Self, ScriptError> {
        let engine_error = |err: rquickjs::Error| ScriptError::Engine(err.to_string());
        let runtime = Runtime::new().map_err(engine_error)?;
        runtime.set_memory_limit(HEAP_LIMIT);
        let deadline: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
        let watched = Rc::clone(&deadline);
        runtime.set_interrupt_handler(Some(Box::new(move || {
            watched.get().is_some_and(|at| Instant::now() >= at)
        })));
        let context = Context::full(&runtime).map_err(engine_error)?;
        let unpins: Unpins = Rc::new(RefCell::new(Vec::new()));
        let internals = context.with(|ctx| -> rquickjs::Result<_> {
            let natives = bindings::natives(&ctx, &host, &unpins)?;
            let setup: Function<'_> = ctx.eval(PRELUDE)?;
            let internals: Object<'_> = setup.call((natives,))?;
            Ok(Persistent::save(&ctx, internals))
        });
        let internals = internals.map_err(engine_error)?;
        Ok(Self {
            internals: Some(internals),
            context,
            runtime,
            host,
            unpins,
            deadline,
        })
    }

    /// Calls the prelude's internal `name` with `args` as one piece of a
    /// task.
    fn call_internal<A>(&mut self, name: &str, args: A, url: &str) -> Result<(), ScriptError>
    where
        A: for<'js> rquickjs::function::IntoArgs<'js>,
    {
        self.start_task();
        let Some(internals) = self.internals.clone() else {
            return Ok(());
        };
        let outcome = self.context.with(|ctx| {
            let called = internals.restore(&ctx).and_then(|internals| {
                let function: Function<'_> = internals.get(name)?;
                function.call::<_, ()>(args)
            });
            called.catch(&ctx).map_err(|err| self.error(&ctx, err, url))
        });
        self.end_task();
        outcome
    }

    fn start_task(&self) {
        self.deadline.set(Some(Instant::now() + TASK_DEADLINE));
    }

    fn end_task(&self) {
        self.deadline.set(None);
    }

    /// What a caught error means for the page, logged at `warn` (AC-35).
    fn error(&self, ctx: &Ctx<'_>, err: CaughtError<'_>, url: &str) -> ScriptError {
        let interrupted = self.deadline.get().is_some_and(|at| Instant::now() >= at);
        let error = match err {
            _ if interrupted => ScriptError::Interrupted,
            CaughtError::Exception(exception) => {
                let name: Option<String> = exception.get("name").ok();
                let message = exception.message().unwrap_or_default();
                let message = match name {
                    Some(name) if !name.is_empty() => format!("{name}: {message}"),
                    _ => message,
                };
                let line = exception.stack().map_or(0, |stack| line_in(&stack, url));
                ScriptError::Exception {
                    message,
                    url: url.to_owned(),
                    line,
                }
            }
            CaughtError::Value(value) => ScriptError::Exception {
                message: value
                    .as_string()
                    .and_then(|s| s.to_string().ok())
                    .unwrap_or_else(|| format!("{value:?}")),
                url: url.to_owned(),
                line: 0,
            },
            CaughtError::Error(rquickjs::Error::Allocation) => ScriptError::OutOfMemory,
            CaughtError::Error(err) => ScriptError::Engine(err.to_string()),
        };
        let error = match error {
            ScriptError::Exception { message, .. } if message.contains("out of memory") => {
                ScriptError::OutOfMemory
            }
            other => other,
        };
        let _ = ctx;
        warn!(%error, url, "script stopped");
        error
    }
}

/// The line `url` was at in a QuickJS stack trace (`at f (url:line:col)`),
/// or 0.
fn line_in(stack: &str, url: &str) -> u32 {
    let needle = format!("{url}:");
    stack
        .match_indices(&needle)
        .find_map(|(at, _)| {
            let rest = &stack[at + needle.len()..];
            let digits = rest.split(|c: char| !c.is_ascii_digit()).next()?;
            digits.parse().ok()
        })
        .unwrap_or(0)
}

impl ScriptEngine for QuickJsEngine {
    fn eval_classic(&mut self, source: &str, url: &str) -> Result<(), ScriptError> {
        self.start_task();
        let outcome = self.context.with(|ctx| {
            let mut options = EvalOptions::default();
            // Classic scripts are sloppy unless they say otherwise.
            options.strict = false;
            options.filename = Some(url.to_owned());
            let ran = ctx.eval_with_options::<(), _>(source, options);
            ran.catch(&ctx).map_err(|err| self.error(&ctx, err, url))
        });
        self.end_task();
        outcome
    }

    fn fire_event(&mut self, target: EventTarget, event: &str) -> Result<(), ScriptError> {
        let target = match target {
            EventTarget::Window => "window",
            EventTarget::Document => "document",
        };
        self.call_internal("fire", (target, event), event)
    }

    fn run_timer(&mut self, id: TimerId) -> Result<(), ScriptError> {
        self.call_internal("runTimer", (id.0,), "setTimeout")
    }

    fn run_microtasks(&mut self) -> Result<(), ScriptError> {
        self.start_task();
        let mut first = None;
        loop {
            match self.runtime.execute_pending_job() {
                Ok(true) => {}
                Ok(false) => break,
                Err(job) => {
                    let error = job.0.with(|ctx| {
                        let caught = CaughtError::from_error(&ctx, rquickjs::Error::Exception);
                        self.error(&ctx, caught, "promise job")
                    });
                    let stop = matches!(error, ScriptError::Interrupted);
                    first.get_or_insert(error);
                    if stop {
                        break;
                    }
                }
            }
        }
        self.end_task();
        first.map_or(Ok(()), Err)
    }

    fn sweep_wrappers(&mut self) {
        let unpinned: Vec<NodeRef> = std::mem::take(&mut *self.unpins.borrow_mut());
        if !unpinned.is_empty() {
            let mut host = self.host.borrow_mut();
            for node in unpinned {
                host.unpin(node);
            }
        }
        if let Err(err) = self.call_internal("sweep", (), "sweep") {
            warn!(%err, "wrapper sweep failed");
        }
    }

    fn collect_garbage(&mut self) {
        self.runtime.run_gc();
    }

    fn heap_bytes(&self) -> Option<u64> {
        Some(self.runtime.memory_usage().malloc_size as u64)
    }
}

impl Drop for QuickJsEngine {
    fn drop(&mut self) {
        // The handle goes first, while the runtime still lives.
        self.internals = None;
    }
}

#[cfg(test)]
mod tests;
