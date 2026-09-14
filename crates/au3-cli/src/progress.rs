//! A once-a-second heartbeat for the long evaluations.
//!
//! The interpreter offers every statement to a [`Debugger`], and that is the
//! only hook that fires while `evaluate_with_options` is blocked inside
//! `run_script`, so a progress reporter is a debugger that answers `Continue`.
//! The counts are what is known *during* the run — the globals and how many of
//! them are array/map tables; the substitution counts only exist afterwards.

use std::time::{Duration, Instant};

use autoitv3_ast::span::Span;
use autoitv3_runtime::debug::{DebugAction, DebugHost, Debugger};
use autoitv3_runtime::Value;

/// Prints an `evaluating: …` line to stderr at most once per second.
///
/// The clock is only consulted every few thousand statements: an `Instant::now()`
/// per statement would be measurable over the millions of steps a real script
/// runs.
pub struct ProgressDebugger {
    start: Instant,
    last: Instant,
    checks: u64,
    interval: Duration,
}

impl ProgressDebugger {
    /// A reporter that speaks up once a second.
    pub fn new() -> Self {
        let now = Instant::now();
        Self { start: now, last: now, checks: 0, interval: Duration::from_secs(1) }
    }
}

impl Default for ProgressDebugger {
    fn default() -> Self {
        Self::new()
    }
}

impl Debugger for ProgressDebugger {
    fn on_statement(
        &mut self,
        _span: Span,
        _depth: usize,
        host: &mut dyn DebugHost,
    ) -> DebugAction {
        self.checks = self.checks.wrapping_add(1);
        // Cheap early-out: most statements just bump the counter.
        if self.checks % 4096 != 0 || self.last.elapsed() < self.interval {
            return DebugAction::Continue;
        }
        self.last = Instant::now();
        let globals = host.globals();
        let tables = globals
            .iter()
            .filter(|(_, v)| matches!(v, Value::Array(_) | Value::Map(_)))
            .count();
        eprintln!(
            "evaluating: {} globals, {} tables ({:.1}s)",
            globals.len(),
            tables,
            self.start.elapsed().as_secs_f32()
        );
        DebugAction::Continue
    }
}
