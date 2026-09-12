//! Pure Windows file-format and binary-layout machinery.
//!
//! This is the crate's **mechanism layer**: parsers and layout engines whose
//! behaviour is identical on every host because they touch no operating-system
//! API — they read and write bytes. The answering layers give the mechanisms
//! their semantics:
//!
//! * [`crate::winemu`] pairs them with its emulation policies (fake addresses,
//!   file-backed state) so a Windows-targeted script runs off Windows;
//! * [`crate::windows`] pairs the same layout engine with real heap memory, so
//!   `DllCall` hands callees genuine pointers.
//!
//! Nothing here knows about the platform *stack* (the runtime dispatch order);
//! both system layers depending on this module is what keeps "who answers a
//! call" separate from "how the answer's bytes are produced".
//!
//! * [`dllstruct`] — AutoIt `DllStruct` definition parser, layout and typed
//!   access over a shared byte buffer
//! * [`pe`] — PE resource-directory reader (`RT_RCDATA`, `RT_VERSION`, …)
//! * [`verinfo`] — `RT_VERSION` / `VS_VERSIONINFO` reader (`FileGetVersion`)
//! * [`shortcut`] — Shell Link (`.lnk`) binary format writer/reader

pub mod dllstruct;
pub mod pe;
pub mod shortcut;
pub mod verinfo;

pub use dllstruct::{DllStruct, FieldSelector};
pub use pe::{PeImage, Resource, Selector};
pub use shortcut::Shortcut;

/// The processor architecture a Windows binary layout targets.
///
/// This lives with the layout machinery because the pointer-sized fields of
/// `DllStruct` layouts depend on it; the emulation layer additionally uses it
/// for `@OSArch`-style macros and `GetSystemInfo` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowsArch {
    /// 32-bit x86.
    X86,
    /// 64-bit x64 — the default.
    #[default]
    X64,
    /// 64-bit ARM.
    Arm64,
}

impl WindowsArch {
    /// The spelling AutoIt uses for `@OSArch`/`@ProcessorArch`.
    pub const fn as_str(self) -> &'static str {
        match self {
            WindowsArch::X86 => "X86",
            WindowsArch::X64 => "X64",
            WindowsArch::Arm64 => "ARM64",
        }
    }

    /// Size of a pointer in bytes, as `DllStruct` layouts need it.
    pub const fn pointer_size(self) -> usize {
        match self {
            WindowsArch::X86 => 4,
            WindowsArch::X64 | WindowsArch::Arm64 => 8,
        }
    }

    /// The `PROCESSOR_ARCHITECTURE` environment value.
    pub const fn env_value(self) -> &'static str {
        match self {
            WindowsArch::X86 => "x86",
            WindowsArch::X64 => "AMD64",
            WindowsArch::Arm64 => "ARM64",
        }
    }

    /// The `PROCESSOR_ARCHITECTURE_*` id `GetSystemInfo` writes.
    pub const fn system_info_id(self) -> u16 {
        match self {
            WindowsArch::X86 => 0,
            WindowsArch::X64 => 9,
            WindowsArch::Arm64 => 12,
        }
    }

    /// Parse `x86`/`x64`/`arm64` (and the usual aliases).
    pub fn from_name(name: &str) -> Option<Self> {
        // Lowercase and drop separators, so `x64`, `X64`, `amd64` all match.
        let lowered: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        match lowered.as_str() {
            "x86" | "i386" | "i486" | "i586" | "i686" | "win32" | "32" => Some(Self::X86),
            "x64" | "amd64" | "x8664" | "64" => Some(Self::X64),
            "arm64" | "aarch64" => Some(Self::Arm64),
            _ => None,
        }
    }
}
