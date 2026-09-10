//! The directory layout a Windows script expects to see.
//!
//! A Windows-targeted AutoIt script builds almost every path it uses out of
//! macros — `@WindowsDir & "\System32\..."`, `@AppDataDir & "\..."` — so an
//! emulation that reports `WIN_10` but a POSIX `/tmp` would produce strings
//! that look wrong. This module is the single source of those paths, shared by
//! the macro table and the registry seed.
//!
//! The roots are the conventional `C:` layout. Only the parts that genuinely
//! vary are parameterised: the user name and whether the build is 32- or
//! 64-bit (`System32` vs `SysWOW64`, `Program Files` vs `Program Files (x86)`).

use super::version::WindowsArch;

/// Windows directory layout for one emulated machine.
#[derive(Debug, Clone)]
pub struct WindowsPaths {
    /// The emulated Windows drive letter, without a separator (`C:`).
    pub home_drive: String,
    /// `C:\Windows`.
    pub windows_dir: String,
    /// `C:\Windows\System32`.
    pub system_dir: String,
    /// `C:\Windows\SysWOW64` on x64, `System32` on x86.
    pub system_x86_dir: String,
    /// `C:\Program Files`.
    pub program_files: String,
    /// `C:\Program Files (x86)`.
    pub program_files_x86: String,
    /// `C:\Program Files\Common Files`.
    pub common_files: String,
    /// `C:\Program Files (x86)\Common Files`.
    pub common_files_x86: String,
    /// `C:\ProgramData`.
    pub program_data: String,
    /// `C:\Users\<user>`.
    pub user_profile: String,
    /// The account name, e.g. `User`.
    pub user_name: String,
    /// The NetBIOS computer name.
    pub computer_name: String,
}

impl WindowsPaths {
    /// Build the layout for `user` on `computer` with the given architecture.
    pub fn new(user: &str, computer: &str, arch: WindowsArch) -> Self {
        let user = if user.trim().is_empty() { "User" } else { user.trim() }.to_string();
        let computer = if computer.trim().is_empty() {
            "DESKTOP-EMULATED".to_string()
        } else {
            computer.trim().to_string()
        };
        let windows_dir = r"C:\Windows".to_string();
        let (system_dir, system_x86_dir, program_files_x86, common_files_x86) = match arch {
            // A 32-bit build has no WOW64 redirection and no separate x86
            // program-files tree.
            WindowsArch::X86 => (
                r"C:\Windows\System32".to_string(),
                r"C:\Windows\System32".to_string(),
                r"C:\Program Files".to_string(),
                r"C:\Program Files\Common Files".to_string(),
            ),
            _ => (
                r"C:\Windows\System32".to_string(),
                r"C:\Windows\SysWOW64".to_string(),
                r"C:\Program Files (x86)".to_string(),
                r"C:\Program Files (x86)\Common Files".to_string(),
            ),
        };
        Self {
            home_drive: "C:".to_string(),
            system_dir,
            system_x86_dir,
            program_files: r"C:\Program Files".to_string(),
            program_files_x86,
            common_files: r"C:\Program Files\Common Files".to_string(),
            common_files_x86,
            program_data: r"C:\ProgramData".to_string(),
            user_profile: format!(r"C:\Users\{user}"),
            user_name: user,
            computer_name: computer,
            windows_dir,
        }
    }

    /// `C:\Users\<user>\AppData\Roaming`.
    pub fn appdata(&self) -> String {
        format!(r"{}\AppData\Roaming", self.user_profile)
    }

    /// `C:\Users\<user>\AppData\Local`.
    pub fn local_appdata(&self) -> String {
        format!(r"{}\AppData\Local", self.user_profile)
    }

    /// `...\AppData\Local\Temp`.
    pub fn temp(&self) -> String {
        format!(r"{}\Temp", self.local_appdata())
    }

    /// `C:\Users\<user>\Desktop`.
    pub fn desktop(&self) -> String {
        format!(r"{}\Desktop", self.user_profile)
    }

    /// `C:\Users\<user>\Documents`.
    pub fn documents(&self) -> String {
        format!(r"{}\Documents", self.user_profile)
    }

    /// `...\AppData\Roaming\Microsoft\Windows\Start Menu`.
    pub fn start_menu(&self) -> String {
        format!(r"{}\Microsoft\Windows\Start Menu", self.appdata())
    }

    /// The all-users start menu under `C:\ProgramData`.
    pub fn start_menu_common(&self) -> String {
        format!(r"{}\Microsoft\Windows\Start Menu", self.program_data)
    }

    /// `...\AppData\Roaming\Microsoft\Windows\Recent`.
    pub fn recent(&self) -> String {
        format!(r"{}\Microsoft\Windows\Recent", self.appdata())
    }

    /// `...\AppData\Roaming\Microsoft\Windows\SendTo`.
    pub fn send_to(&self) -> String {
        format!(r"{}\Microsoft\Windows\SendTo", self.appdata())
    }

    /// `...\AppData\Roaming\Microsoft\Windows\Templates`.
    pub fn templates(&self) -> String {
        format!(r"{}\Microsoft\Windows\Templates", self.appdata())
    }

    /// `...\AppData\Local\Microsoft\Windows\INetCache`.
    pub fn internet_cache(&self) -> String {
        format!(r"{}\Microsoft\Windows\INetCache", self.local_appdata())
    }

    /// `...\AppData\Local\Microsoft\Windows\History`.
    pub fn history(&self) -> String {
        format!(r"{}\Microsoft\Windows\History", self.local_appdata())
    }

    /// The UNC home share, e.g. `\\COMPUTER\User`.
    pub fn home_share(&self) -> String {
        format!(r"\\{}\{}", self.computer_name, self.user_name)
    }

    /// The `PROCESSOR_ARCHITECTURE` value for `arch`.
    pub fn processor_architecture(&self, arch: WindowsArch) -> &'static str {
        arch.env_value()
    }
}
