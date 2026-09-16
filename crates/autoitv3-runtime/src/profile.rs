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

/// One class of externally observable effect, for per-kind overrides.
///
/// The [`EffectPolicy`] is the base decision; [`ExecutionProfile::overrides`]
/// can then allow or deny individual kinds on top of it -- for example a
/// deterministic deobfuscation run that may write the registry it probes, or
/// a faithful run that must never actually call `Shutdown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectKind {
    /// Creating, writing, deleting or moving files, directories and INI
    /// contents; file attributes and timestamps.
    FileWrite,
    /// `EnvSet`/`EnvUpdate`.
    EnvWrite,
    /// `RegWrite`/`RegDelete` (real or emulated registry).
    RegistryWrite,
    /// `ClipPut` (real or emulated clipboard).
    ClipboardWrite,
    /// Starting another process: `Run`, `ShellExecute*`, `RunAs*`, and the
    /// blocking `ProcessWait*` that presupposes one.
    Spawn,
    /// `Shutdown` -- the one effect that can end a session, so it can be
    /// denied even inside an otherwise faithful run.
    Shutdown,
    /// Network and drive-mapping side effects: `TCP*`/`UDP*`/`Inet*`/`Ping`,
    /// `DriveMapAdd`/`DriveMapDel`.
    NetAccess,
    /// Acting on other processes: `ProcessClose`, `ProcessSetPriority`.
    ProcessControl,
}

impl EffectKind {
    /// Every kind, for exhaustive iteration (the CLI's `--allow`/`--deny`
    /// tables and tests).
    pub const ALL: &'static [EffectKind] = &[
        EffectKind::FileWrite,
        EffectKind::EnvWrite,
        EffectKind::RegistryWrite,
        EffectKind::ClipboardWrite,
        EffectKind::Spawn,
        EffectKind::Shutdown,
        EffectKind::NetAccess,
        EffectKind::ProcessControl,
    ];

    /// The canonical CLI/env spelling of this kind (`file`, `registry`,
    /// `spawn`, ...), the one [`EffectKind::from_name`] documents.
    pub const fn name(self) -> &'static str {
        match self {
            EffectKind::FileWrite => "file",
            EffectKind::EnvWrite => "env",
            EffectKind::RegistryWrite => "registry",
            EffectKind::ClipboardWrite => "clipboard",
            EffectKind::Spawn => "spawn",
            EffectKind::Shutdown => "shutdown",
            EffectKind::NetAccess => "net",
            EffectKind::ProcessControl => "process",
        }
    }

    /// Parse a CLI/env spelling (`file`, `registry`, `spawn`, `shutdown`, ...).
    pub fn from_name(name: &str) -> Option<EffectKind> {
        Some(match name.to_ascii_lowercase().as_str() {
            "file" | "filewrite" | "fs" => EffectKind::FileWrite,
            "env" | "envwrite" | "environment" => EffectKind::EnvWrite,
            "registry" | "reg" | "registrywrite" => EffectKind::RegistryWrite,
            "clipboard" | "clip" | "clipboardwrite" => EffectKind::ClipboardWrite,
            "spawn" | "run" => EffectKind::Spawn,
            "shutdown" => EffectKind::Shutdown,
            "net" | "netaccess" | "network" => EffectKind::NetAccess,
            "process" | "processcontrol" => EffectKind::ProcessControl,
            _ => return None,
        })
    }
}

/// Per-kind allow/deny decisions layered over the base [`EffectPolicy`].
///
/// `None` means "ask the base policy"; `Some(true)` allows the kind even
/// under the read-only profile, `Some(false)` denies it even under the
/// faithful one. The struct is `Copy` so [`ExecutionProfile`] stays `Copy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EffectOverrides {
    /// File/directory/INI writes.
    pub file_write: Option<bool>,
    /// Environment changes.
    pub env_write: Option<bool>,
    /// Registry writes and deletes.
    pub registry_write: Option<bool>,
    /// Clipboard writes.
    pub clipboard_write: Option<bool>,
    /// Starting processes.
    pub spawn: Option<bool>,
    /// `Shutdown`.
    pub shutdown: Option<bool>,
    /// Network access and drive mappings.
    pub net_access: Option<bool>,
    /// Killing / reprioritising other processes.
    pub process_control: Option<bool>,
}

impl EffectOverrides {
    /// No overrides -- every kind defers to the base policy.
    pub const fn new() -> Self {
        Self {
            file_write: None,
            env_write: None,
            registry_write: None,
            clipboard_write: None,
            spawn: None,
            shutdown: None,
            net_access: None,
            process_control: None,
        }
    }

    /// Builder: set one kind's decision.
    pub const fn with(mut self, kind: EffectKind, allowed: bool) -> Self {
        let slot = match kind {
            EffectKind::FileWrite => &mut self.file_write,
            EffectKind::EnvWrite => &mut self.env_write,
            EffectKind::RegistryWrite => &mut self.registry_write,
            EffectKind::ClipboardWrite => &mut self.clipboard_write,
            EffectKind::Spawn => &mut self.spawn,
            EffectKind::Shutdown => &mut self.shutdown,
            EffectKind::NetAccess => &mut self.net_access,
            EffectKind::ProcessControl => &mut self.process_control,
        };
        *slot = Some(allowed);
        self
    }

    /// The decision for `kind`, or `None` to defer to the base policy.
    pub const fn get(&self, kind: EffectKind) -> Option<bool> {
        match kind {
            EffectKind::FileWrite => self.file_write,
            EffectKind::EnvWrite => self.env_write,
            EffectKind::RegistryWrite => self.registry_write,
            EffectKind::ClipboardWrite => self.clipboard_write,
            EffectKind::Spawn => self.spawn,
            EffectKind::Shutdown => self.shutdown,
            EffectKind::NetAccess => self.net_access,
            EffectKind::ProcessControl => self.process_control,
        }
    }

    /// Whether any kind is overridden.
    pub const fn is_empty(&self) -> bool {
        self.get(EffectKind::FileWrite).is_none()
            && self.get(EffectKind::EnvWrite).is_none()
            && self.get(EffectKind::RegistryWrite).is_none()
            && self.get(EffectKind::ClipboardWrite).is_none()
            && self.get(EffectKind::Spawn).is_none()
            && self.get(EffectKind::Shutdown).is_none()
            && self.get(EffectKind::NetAccess).is_none()
            && self.get(EffectKind::ProcessControl).is_none()
    }
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
    /// Per-kind allow/deny decisions layered over `effects`. Defaults to
    /// empty: the presets' behaviour is exactly the base policy.
    pub overrides: EffectOverrides,
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
            overrides: EffectOverrides::new(),
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
            overrides: EffectOverrides::new(),
        }
    }

    /// Everything faithful except the delay, which is capped.
    pub const fn capped(max_sleep: Duration) -> Self {
        Self {
            sleep: SleepPolicy::Capped(max_sleep),
            overrides: EffectOverrides::new(),
            ..Self::faithful()
        }
    }

    /// Builder: allow or deny one [`EffectKind`] on top of the base policy.
    ///
    /// ```no_run
    /// use autoitv3_runtime::{ExecutionProfile, profile::EffectKind};
    ///
    /// // A deobfuscation run that may write the registry it probes, but
    /// // still cannot touch anything else.
    /// let profile = ExecutionProfile::deterministic()
    ///     .with_effect(EffectKind::RegistryWrite, true);
    /// ```
    pub const fn with_effect(mut self, kind: EffectKind, allowed: bool) -> Self {
        self.overrides = self.overrides.with(kind, allowed);
        self
    }

    /// The effective decision for `kind`: the per-kind override when set,
    /// otherwise the base policy.
    pub const fn effect_allowed(&self, kind: EffectKind) -> bool {
        match self.overrides.get(kind) {
            Some(allowed) => allowed,
            None => matches!(self.effects, EffectPolicy::Allow),
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