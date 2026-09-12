//! File attribute / short-name / environment-broadcast calls that only make
//! sense against the real Windows filesystem.
//!
//! The common layer implements `FileGetAttrib`/`FileSetAttrib`/
//! `FileGetShortName`/`EnvUpdate` with portable approximations; the native
//! Windows layer answers first with the real thing, so a script sees AutoIt's
//! documented behaviour:
//!
//! * `FileGetAttrib` — the `FILE_ATTRIBUTE_*` bits rendered as AutoIt's
//!   letters (`R`, `A`, `S`, `H`, `D`, `N`), in AutoIt's order.
//! * `FileSetAttrib` — `+`/`-`/no-prefix per-letter changes through
//!   `SetFileAttributesW`; `N` clears the settable bits.
//! * `FileGetShortName` — the real 8.3 name via `GetShortPathNameW` (the long
//!   path itself where 8.3 generation is disabled, which is what the API
//!   returns there).
//! * `EnvUpdate` — the documented `WM_SETTINGCHANGE` broadcast.

use autoitv3_runtime::value::Value;

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Storage::FileSystem::{
    GetFileAttributesW, GetShortPathNameW, SetFileAttributesW, FILE_ATTRIBUTE_ARCHIVE,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL,
    FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_SYSTEM, INVALID_FILE_ATTRIBUTES,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `FileGetAttrib` — AutoIt's letters for the Windows attribute bits.
pub(crate) fn file_get_attrib(path: &str) -> Option<String> {
    let p = wide(path);
    let raw = unsafe { GetFileAttributesW(p.as_ptr()) };
    if raw == INVALID_FILE_ATTRIBUTES {
        return None;
    }
    let mut a = String::new();
    if raw & FILE_ATTRIBUTE_READONLY != 0 {
        a.push('R');
    }
    if raw & FILE_ATTRIBUTE_ARCHIVE != 0 {
        a.push('A');
    }
    if raw & FILE_ATTRIBUTE_SYSTEM != 0 {
        a.push('S');
    }
    if raw & FILE_ATTRIBUTE_HIDDEN != 0 {
        a.push('H');
    }
    // The directory bit rides on the same attribute word, and "normal" means
    // none of the settable bits are set.
    if raw & FILE_ATTRIBUTE_DIRECTORY != 0 {
        a.push('D');
    }
    if a.is_empty() {
        a.push('N');
    }
    Some(a)
}

/// `FileSetAttrib` — `["+RS", "-H", "A"]` spellings applied through
/// `SetFileAttributesW`. Returns success and the final attribute string.
pub(crate) fn file_set_attrib(path: &str, changes: &str) -> bool {
    let p = wide(path);
    let raw = unsafe { GetFileAttributesW(p.as_ptr()) };
    let mut attrs = if raw == INVALID_FILE_ATTRIBUTES {
        FILE_ATTRIBUTE_NORMAL
    } else {
        raw
    };
    let (mut plus, mut minus) = (0u32, 0u32);
    let mut sign = '+';
    for c in changes.chars() {
        match c {
            '+' | '-' => sign = c,
            'R' | 'A' | 'S' | 'H' => {
                let bit = match c {
                    'R' => FILE_ATTRIBUTE_READONLY,
                    'A' => FILE_ATTRIBUTE_ARCHIVE,
                    'S' => FILE_ATTRIBUTE_SYSTEM,
                    _ => FILE_ATTRIBUTE_HIDDEN,
                };
                match sign {
                    '+' => plus |= bit,
                    _ => minus |= bit,
                }
            }
            // `N` resets the settable bits before any `+` that follows.
            'N' => {
                attrs &= !(FILE_ATTRIBUTE_READONLY
                    | FILE_ATTRIBUTE_ARCHIVE
                    | FILE_ATTRIBUTE_SYSTEM
                    | FILE_ATTRIBUTE_HIDDEN);
            }
            _ => {}
        }
    }
    attrs = (attrs | plus) & !minus;
    if attrs == 0 {
        attrs = FILE_ATTRIBUTE_NORMAL;
    }
    unsafe { SetFileAttributesW(p.as_ptr(), attrs) != 0 }
}

/// `FileGetShortName` — the real 8.3 path where one exists.
pub(crate) fn file_get_short_name(path: &str) -> String {
    let p = wide(path);
    let len = unsafe { GetShortPathNameW(p.as_ptr(), std::ptr::null_mut(), 0) };
    if len == 0 {
        return path.to_string();
    }
    let mut out = vec![0u16; len as usize];
    let written = unsafe { GetShortPathNameW(p.as_ptr(), out.as_mut_ptr(), len) };
    if written == 0 {
        return path.to_string();
    }
    out.truncate(written as usize);
    String::from_utf16_lossy(&out)
}

/// `EnvUpdate` — tell running applications the environment changed.
pub(crate) fn env_update() -> Value {
    let setting = wide("Environment");
    let delivered = unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST as HWND,
            WM_SETTINGCHANGE,
            0,
            setting.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            2_000,
            std::ptr::null_mut(),
        )
    };
    Value::Int(i64::from(delivered != 0))
}
