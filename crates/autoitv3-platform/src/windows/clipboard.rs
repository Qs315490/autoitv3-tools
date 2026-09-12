//! `ClipGet` / `ClipPut` through the real Windows clipboard.
//!
//! Text is exchanged as `CF_UNICODETEXT`. `ClipGet` reports `@error = 1` when
//! the clipboard is empty and `2` when it holds no text (AutoIt's documented
//! semantics); `ClipPut` refuses under the deterministic analysis profile.

use std::time::{Duration, Instant};

use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::value::Value;

use windows_sys::Win32::Foundation::GlobalFree;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

use autoitv3_runtime::profile::EffectKind;

/// How long to keep retrying `OpenClipboard` while another process holds it.
const OPEN_TIMEOUT: Duration = Duration::from_millis(500);

fn open_clipboard() -> bool {
    let deadline = Instant::now() + OPEN_TIMEOUT;
    loop {
        if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The clipboard text, or `@error` 1 (empty) / 2 (no text on the clipboard).
pub(crate) fn clip_get(ctx: &mut dyn HostContext) -> Value {
    if !open_clipboard() {
        ctx.set_error(1, 0);
        return Value::str("");
    }
    let (text, err) = unsafe {
        let mut out = String::new();
        let mut err = 1i64;
        if IsClipboardFormatAvailable(CF_UNICODETEXT as u32) != 0 {
            let handle = GetClipboardData(CF_UNICODETEXT as u32);
            if !handle.is_null() {
                let size = GlobalSize(handle as _);
                let locked = GlobalLock(handle as _);
                if !locked.is_null() {
                    let units: Vec<u16> = std::slice::from_raw_parts(locked as *const u16, size / 2)
                        .iter()
                        .copied()
                        .take_while(|u| *u != 0)
                        .collect();
                    out = String::from_utf16_lossy(&units);
                    GlobalUnlock(handle as _);
                    err = 0;
                }
            }
            if err == 1 {
                err = 2;
            }
        }
        CloseClipboard();
        (out, err)
    };
    ctx.set_error(err, 0);
    Value::Str(text)
}

/// Replace the clipboard text; `1` on success, `0` with `@error = 1` on
/// refusal (analysis profile) or failure to open/claim the clipboard.
pub(crate) fn clip_put(args: &[Value], ctx: &mut dyn HostContext) -> Value {
    if !ctx.effect_allowed(EffectKind::ClipboardWrite) {
        ctx.set_error(1, 0);
        return Value::Int(0);
    }
    let text = args
        .first()
        .map(|v| v.to_autoit_string())
        .unwrap_or_default();
    let mut units: Vec<u16> = text.encode_utf16().collect();
    units.push(0);
    let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();

    if !open_clipboard() {
        ctx.set_error(1, 0);
        return Value::Int(0);
    }
    let ok = unsafe {
        let mut claimed = false;
        if EmptyClipboard() != 0 {
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
            if !handle.is_null() {
                let locked = GlobalLock(handle);
                if !locked.is_null() {
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), locked as *mut u8, bytes.len());
                    GlobalUnlock(handle);
                    // The clipboard owns the allocation once this succeeds.
                    if !SetClipboardData(CF_UNICODETEXT as u32, handle as _).is_null() {
                        claimed = true;
                    }
                }
                if !claimed {
                    GlobalFree(handle);
                }
            }
        }
        CloseClipboard();
        claimed
    };
    ctx.set_error(if ok { 0 } else { 1 }, 0);
    Value::Int(i64::from(ok))
}
