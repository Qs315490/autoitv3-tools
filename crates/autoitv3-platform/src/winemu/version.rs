//! Which Windows release (and architecture) the emulation layer presents.
//!
//! Everything the layer reports about the operating system is derived from one
//! of these values: the `@OSVersion`/`@OSBuild` macros, what
//! `GetVersionExW`/`RtlGetVersion` write into an `OSVERSIONINFO` struct, and
//! the registry values under
//! `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`. Keeping it in one
//! place means "select the emulated system version" is a single knob.

pub use crate::winfmt::WindowsArch;

/// A Windows release the emulation layer can present.
///
/// The variants are ordered oldest to newest so a script's "at least Win7"
/// checks can be mirrored with `>=` in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum WindowsVersion {
    /// Windows XP (5.1).
    WinXp,
    /// Windows Vista (6.0).
    WinVista,
    /// Windows 7 (6.1).
    Win7,
    /// Windows 8 (6.2).
    Win8,
    /// Windows 8.1 (6.3).
    Win81,
    /// Windows 10 (10.0) — the default.
    #[default]
    Win10,
    /// Windows 11 (10.0).
    Win11,
}

/// The numbers behind one [`WindowsVersion`]. Kept in a table so the public
/// API stays a set of small accessors.
struct Spec {
    /// Canonical short name, e.g. `win10`.
    name: &'static str,
    /// What AutoIt's `@OSVersion` returns, e.g. `WIN_10`.
    macro_name: &'static str,
    major: u32,
    minor: u32,
    build: u32,
    service_pack_major: u16,
    service_pack_minor: u16,
    /// `szCSDVersion` — `"Service Pack 1"` or empty.
    csd_version: &'static str,
    /// `wSuiteMask` that `GetVersionEx` reports.
    suite_mask: u16,
    /// `wProductType`; `1` is `VER_NT_WORKSTATION`.
    product_type: u8,
    /// `HKLM\...\CurrentVersion\ProductName`.
    product_name: &'static str,
    /// `HKLM\...\CurrentVersion\CurrentVersion` (the legacy 6.x string).
    current_version: &'static str,
    /// `HKLM\...\CurrentVersion\DisplayVersion` (or the service pack).
    display_version: &'static str,
    /// `HKLM\...\CurrentVersion\ReleaseId`.
    release_id: &'static str,
}

/// `VER_NT_WORKSTATION`.
const VER_NT_WORKSTATION: u8 = 1;
/// `VER_SUITE_SINGLEUSERTS`, what a modern client SKU reports.
const VER_SUITE_SINGLEUSERTS: u16 = 0x0100;

/// The spec table, indexed by `WindowsVersion as usize`.
const SPECS: [Spec; 7] = [
    Spec {
        name: "winxp",
        macro_name: "WIN_XP",
        major: 5,
        minor: 1,
        build: 2600,
        service_pack_major: 3,
        service_pack_minor: 0,
        csd_version: "Service Pack 3",
        suite_mask: 0,
        product_type: VER_NT_WORKSTATION,
        product_name: "Windows XP Professional",
        current_version: "5.1",
        display_version: "Service Pack 3",
        release_id: "",
    },
    Spec {
        name: "winvista",
        macro_name: "WIN_VISTA",
        major: 6,
        minor: 0,
        build: 6002,
        service_pack_major: 2,
        service_pack_minor: 0,
        csd_version: "Service Pack 2",
        suite_mask: 0,
        product_type: VER_NT_WORKSTATION,
        product_name: "Windows Vista Ultimate",
        current_version: "6.0",
        display_version: "Service Pack 2",
        release_id: "",
    },
    Spec {
        name: "win7",
        macro_name: "WIN_7",
        major: 6,
        minor: 1,
        build: 7601,
        service_pack_major: 1,
        service_pack_minor: 0,
        csd_version: "Service Pack 1",
        suite_mask: 0,
        product_type: VER_NT_WORKSTATION,
        product_name: "Windows 7 Professional",
        current_version: "6.1",
        display_version: "Service Pack 1",
        release_id: "",
    },
    Spec {
        name: "win8",
        macro_name: "WIN_8",
        major: 6,
        minor: 2,
        build: 9200,
        service_pack_major: 0,
        service_pack_minor: 0,
        csd_version: "",
        suite_mask: 0,
        product_type: VER_NT_WORKSTATION,
        product_name: "Windows 8",
        current_version: "6.2",
        display_version: "",
        release_id: "",
    },
    Spec {
        name: "win81",
        macro_name: "WIN_81",
        major: 6,
        minor: 3,
        build: 9600,
        service_pack_major: 0,
        service_pack_minor: 0,
        csd_version: "",
        suite_mask: 0,
        product_type: VER_NT_WORKSTATION,
        product_name: "Windows 8.1 Pro",
        current_version: "6.3",
        display_version: "",
        release_id: "",
    },
    Spec {
        name: "win10",
        macro_name: "WIN_10",
        major: 10,
        minor: 0,
        build: 19045,
        service_pack_major: 0,
        service_pack_minor: 0,
        csd_version: "",
        suite_mask: VER_SUITE_SINGLEUSERTS,
        product_type: VER_NT_WORKSTATION,
        product_name: "Windows 10 Pro",
        current_version: "6.3",
        display_version: "22H2",
        release_id: "2009",
    },
    Spec {
        name: "win11",
        macro_name: "WIN_11",
        major: 10,
        minor: 0,
        build: 22631,
        service_pack_major: 0,
        service_pack_minor: 0,
        csd_version: "",
        suite_mask: VER_SUITE_SINGLEUSERTS,
        product_type: VER_NT_WORKSTATION,
        product_name: "Windows 11 Pro",
        current_version: "6.3",
        display_version: "23H2",
        release_id: "2009",
    },
];

impl WindowsVersion {
    /// Every selectable version, oldest first.
    pub const ALL: &'static [WindowsVersion] = &[
        WindowsVersion::WinXp,
        WindowsVersion::WinVista,
        WindowsVersion::Win7,
        WindowsVersion::Win8,
        WindowsVersion::Win81,
        WindowsVersion::Win10,
        WindowsVersion::Win11,
    ];

    fn spec(self) -> &'static Spec {
        &SPECS[self as usize]
    }

    /// Canonical short name (`win10`), as accepted by
    /// [`from_name`](Self::from_name) and printed by `--win-version` help.
    pub fn as_str(self) -> &'static str {
        self.spec().name
    }

    /// What AutoIt's `@OSVersion` returns (`WIN_10`).
    pub fn os_version_macro(self) -> &'static str {
        self.spec().macro_name
    }

    /// `@OSBuild`.
    pub fn build(self) -> u32 {
        self.spec().build
    }

    /// `@OSServicePack` (empty on versions without one).
    pub fn service_pack(self) -> &'static str {
        self.spec().csd_version
    }

    /// `dwMajorVersion` written by `GetVersionEx`.
    pub fn major(self) -> u32 {
        self.spec().major
    }

    /// `dwMinorVersion` written by `GetVersionEx`.
    pub fn minor(self) -> u32 {
        self.spec().minor
    }

    /// `dwPlatformId`; `2` is `VER_PLATFORM_WIN32_NT`.
    pub fn platform_id(self) -> u32 {
        2
    }

    /// `wServicePackMajor`.
    pub fn service_pack_major(self) -> u16 {
        self.spec().service_pack_major
    }

    /// `wServicePackMinor`.
    pub fn service_pack_minor(self) -> u16 {
        self.spec().service_pack_minor
    }

    /// `wSuiteMask`.
    pub fn suite_mask(self) -> u16 {
        self.spec().suite_mask
    }

    /// `wProductType`.
    pub fn product_type(self) -> u8 {
        self.spec().product_type
    }

    /// `BaseGetVersion()`'s packed `DWORD`: `major | minor << 8 | build << 16`.
    pub fn packed_get_version(self) -> u32 {
        (self.spec().major & 0xff) | ((self.spec().minor & 0xff) << 8) | (self.spec().build << 16)
    }

    /// A human-readable name, for summaries and `RegRead` seeds.
    pub fn display_name(self) -> &'static str {
        self.spec().product_name
    }

    /// `ProductName` in the registry's `CurrentVersion` key.
    pub fn registry_product_name(self) -> &'static str {
        self.spec().product_name
    }

    /// `CurrentVersion` in the registry (the legacy `6.3`-style string).
    pub fn registry_current_version(self) -> &'static str {
        self.spec().current_version
    }

    /// `DisplayVersion` in the registry (`22H2`, a service pack, ...).
    pub fn registry_display_version(self) -> &'static str {
        self.spec().display_version
    }

    /// `ReleaseId` in the registry.
    pub fn registry_release_id(self) -> &'static str {
        self.spec().release_id
    }

    /// Parse a version name. Accepts the canonical `win10`, the registry-style
    /// `10.0.19045`, and the loose spellings a user is likely to type
    /// (`Windows 10`, `win_10`, `10`).
    pub fn from_name(name: &str) -> Option<Self> {
        // Normalise once: this drops the `win`/`windows` prefix and the
        // separators, so `Windows 10`, `win_10` and `WIN10` all reduce to `10`
        // (and `win8.1` to `8.1`) before anything else looks at them.
        let mut key = normalize(name);
        // "8.1" is a marketing name, not a version number — the kernel reports
        // 6.3 — so it is matched before the dotted rule.
        if key == "8.1" {
            return Some(WindowsVersion::Win81);
        }
        // A `major.minor[.build]` spelling selects by the numbers.
        let dotted: Vec<u32> = key
            .split('.')
            .filter_map(|p| p.trim().parse::<u32>().ok())
            .collect();
        if dotted.len() >= 2 {
            return Self::ALL.iter().copied().find(|v| {
                v.major() == dotted[0]
                    && v.minor() == dotted[1]
                    // A full `10.0.19045` also has to match the build, or a
                    // Win11 request would silently become Win10.
                    && (dotted.len() < 3 || dotted[2] == 0 || v.build() == dotted[2])
            });
        }
        // `win10x64` and friends name a version *and* an architecture; the
        // architecture is selected separately, so only the version is read.
        for suffix in ["x64", "x86", "arm64", "amd64", "32", "64"] {
            if let Some(stripped) = key.strip_suffix(suffix) {
                if !stripped.is_empty() {
                    key = stripped.to_string();
                    break;
                }
            }
        }
        match key.as_str() {
            "xp" => Some(WindowsVersion::WinXp),
            "vista" => Some(WindowsVersion::WinVista),
            "7" | "seven" => Some(WindowsVersion::Win7),
            "8" => Some(WindowsVersion::Win8),
            "81" | "8point1" => Some(WindowsVersion::Win81),
            "10" => Some(WindowsVersion::Win10),
            "11" => Some(WindowsVersion::Win11),
            // Windows Server releases are recognised so a request is not
            // silently ignored; they present as the matching client kernel.
            "2003" | "2003r2" | "xp64" => Some(WindowsVersion::WinXp),
            "2008" => Some(WindowsVersion::WinVista),
            "2008r2" => Some(WindowsVersion::Win7),
            "2012" => Some(WindowsVersion::Win8),
            "2012r2" => Some(WindowsVersion::Win81),
            "2016" | "2019" | "2022" | "2025" => Some(WindowsVersion::Win10),
            _ => None,
        }
    }
}

/// Lowercase and drop separators/prefixes so `Windows 10`, `win_10` and
/// `WIN10` all compare equal.
fn normalize(name: &str) -> String {
    let lowered: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.')
        .collect::<String>()
        .to_ascii_lowercase();
    lowered
        .strip_prefix("windows")
        .or_else(|| lowered.strip_prefix("win"))
        .unwrap_or(&lowered)
        .to_string()
}
