//! Execution profiles — how faithfully the interpreter reproduces AutoIt.
//!
//! The interpreter has two very different callers, and they want opposite
//! things from the same script:
//!
//! * **Deobfuscation** runs untrusted code to *learn* what it computes. It
//!   wants to be fast, reproducible and harmless: `Sleep(60000)` must not
//!   actually wait a minute, `Random` must not change the answer between runs,
//!   and evaluating an untrusted script should not touch the disk.
//! * **A normal runtime** is running the script *for real*. It must behave like
//!   AutoIt: real delays, real entropy, real side effects.
//!
//! A single hard-coded choice for both would be wrong for one of them, so the
//! behaviour is a value — [`ExecutionProfile`] — that the embedder picks.
//!
//! ```
//! use autoitv3_runtime::{ExecutionProfile, Runtime};
//!
//! // Probing an obfuscated script: fast, reproducible, no side effects.
//! let mut rt = Runtime::new();
//! rt.set_profile(ExecutionProfile::deterministic());
//!
//! // Running a script for its effects: AutoIt semantics.
//! let mut rt = Runtime::new();
//! rt.set_profile(ExecutionProfile::faithful());
//! ```
//!
//! The interpreter's default is [`ExecutionProfile::faithful`]: a library
//! should not silently change what a script does. Deobfuscation opts in to
//! [`ExecutionProfile::deterministic`] explicitly.

use std::time::Duration;

/// What `Sleep()` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepPolicy {
    /// Return immediately. Nothing in the script's *result* depends on how long
    /// it waited, so deobfuscation skips the wait entirely.
    Skip,
    /// Sleep for the requested time, as AutoIt does.
    Real,
    /// Sleep, but never longer than this. Useful for a fast-but-still-timed
    /// run (for example a UI script under test).
    Capped(Duration),
}

/// How `Random()` is seeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RandomPolicy {
    /// Start from a fixed seed, so the same script produces the same values on
    /// every run. This is what makes a deobfuscation result reproducible.
    Deterministic(u64),
    /// Seed from the operating system, as AutoIt does.
    Entropy,
}

/// Whether calls with external effects are allowed to happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectPolicy {
    /// Writes, deletes, environment changes and console output all happen.
    Allow,
    /// Reads work as usual, but anything that would modify state fails: the
    /// call returns its failure value and sets `@error` to 1. Use this to
    /// evaluate untrusted code without letting it touch the machine.
    ReadOnly,
}

/// How faithfully the interpreter reproduces AutoIt's observable behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionProfile {
    /// What `Sleep()` does.
    pub sleep: SleepPolicy,
    /// How `Random()` is seeded.
    pub random: RandomPolicy,
    /// Whether external effects are allowed.
    pub effects: EffectPolicy,
}

impl ExecutionProfile {
    /// AutoIt semantics: real delays, real entropy, real side effects.
    ///
    /// This is the interpreter's default — running a script should do what the
    /// script says.
    pub const fn faithful() -> Self {
        Self {
            sleep: SleepPolicy::Real,
            random: RandomPolicy::Entropy,
            effects: EffectPolicy::Allow,
        }
    }

    /// Fast, reproducible and harmless — the deobfuscation profile.
    ///
    /// * `Sleep` returns immediately
    /// * `Random` starts from a fixed seed, so repeated runs agree
    /// * external effects are refused, so evaluating a sample cannot damage
    ///   the machine
    ///
    /// Reading is unaffected: `FileRead`, `FileExists`, `EnvGet` and friends
    /// still work, because deobfuscation needs them to follow the code.
    pub const fn deterministic() -> Self {
        Self {
            sleep: SleepPolicy::Skip,
            random: RandomPolicy::Deterministic(DEFAULT_RANDOM_SEED),
            effects: EffectPolicy::ReadOnly,
        }
    }

    /// Everything faithful except the delay, which is capped.
    pub const fn capped(max_sleep: Duration) -> Self {
        Self {
            sleep: SleepPolicy::Capped(max_sleep),
            ..Self::faithful()
        }
    }

    /// True when the profile trades fidelity for reproducibility.
    pub const fn is_deterministic(&self) -> bool {
        matches!(self.random, RandomPolicy::Deterministic(_))
            && matches!(self.effects, EffectPolicy::ReadOnly)
            && matches!(self.sleep, SleepPolicy::Skip)
    }
}

impl Default for ExecutionProfile {
    /// [`ExecutionProfile::faithful`] — a library must not silently change what
    /// a script does.
    fn default() -> Self {
        Self::faithful()
    }
}

/// The seed [`RandomPolicy::Deterministic`] uses by default.
pub const DEFAULT_RANDOM_SEED: u64 = 0x2545_F491_4F6C_DD1D;