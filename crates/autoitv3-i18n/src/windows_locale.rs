//! The Windows UI language, which is what `auto` asks when no POSIX locale
//! variable says anything.
//!
//! Windows has no `LANG`: a GUI process inherits the user's *UI language* from
//! the OS, not a POSIX locale, so a Chinese Windows used to print English here
//! until the environment happened to carry `zh_CN.UTF-8`.
//!
//! `lib.rs` includes this file with `#[cfg(windows)]`, but nothing inside it is
//! Windows-specific to *type-check*: `windows-sys`' declarations compile on any
//! host, which is how `/tmp/guicheck` covers it from a non-Windows one.

use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;

/// `LOCALE_NAME_MAX_LENGTH` — the longest locale name Windows will produce,
/// NUL included. Not in the `windows-sys` bindings.
const LOCALE_NAME_MAX_LENGTH: usize = 85;

/// The user's UI locale as a tag (`"zh-CN"`), if Windows reports one.
pub fn locale_tag() -> Option<String> {
    let mut buffer = [0u16; LOCALE_NAME_MAX_LENGTH];
    // Returns the number of UTF-16 units written, NUL included, or 0 on
    // failure.
    let written = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
    let written = usize::try_from(written).ok()?;
    if written <= 1 || written > buffer.len() {
        return None;
    }
    Some(String::from_utf16_lossy(&buffer[..written - 1]))
}
