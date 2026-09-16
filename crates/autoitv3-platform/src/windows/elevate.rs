//! Starting an elevated copy of this process: `#RequireAdmin`, on Windows.
//!
//! Windows cannot raise the token of a process that is already running, so the
//! only way to get administrator rights is a *new* process. `ShellExecuteExW`
//! with the `runas` verb is what puts the UAC consent prompt on screen; this
//! module is that call and nothing else, so the interpreter can keep all of the
//! policy in `autoitv3-platform`'s [`elevate`](crate::elevate) module and in the
//! command line tool.
//!
//! The other half is [`attach_console`]: the shell service that creates the
//! elevated process gives it a console of its own, which for a command line run
//! means the script's output opens in a *second* window. A process can only be
//! attached to one console, so the copy frees its own and attaches to the
//! console of the process that started it — the one the user is looking at.
//!
//! Like the rest of the files under `windows/`, this one carries no `cfg` of
//! its own — the module declaration is what keeps it off other targets — so a
//! scratch crate that `#[path]`-includes it (plus `crate::elevate`) and depends
//! on `windows-sys` type-checks all of it on a host without Windows, which is
//! how the two calls below were checked before a Windows build ever saw them.

use std::ffi::OsString;
use std::path::Path;

use autoitv3_i18n::msg;

use crate::elevate::{command_line, Elevated};

/// A NUL-terminated UTF-16 copy of `text`, for the wide Win32 entry points.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// Start `exe` with `args` as an administrator, and wait for it.
///
/// `args` are the arguments *after* the program name; `dir` is the working
/// directory to start in (`None` means "let the shell pick", so a caller that
/// cares about relative paths should pass its own).
///
/// The child is launched with the `runas` verb through `ShellExecuteExW`, which
/// is what puts the OS consent prompt on screen. `Err` carries a message for
/// the cases the caller cannot do anything about (the shell refused to start
/// the process at all); a user saying "no" is not an error.
pub fn relaunch_elevated(
    exe: &Path,
    args: &[OsString],
    dir: Option<&Path>,
) -> Result<Elevated, String> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_CANCELLED};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, WaitForSingleObject, INFINITE,
    };
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};

    let file = wide(&exe.to_string_lossy());
    let params = wide(&command_line(args));
    let verb = wide("runas");
    let directory = dir.map(|d| wide(&d.to_string_lossy()));

    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    // No `SEE_MASK_NO_CONSOLE`: the child gets its own console either way, and
    // hiding it would only hide the output — `attach_console` is what brings
    // the output back to the caller's window.
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.lpDirectory = match &directory {
        Some(d) => d.as_ptr(),
        None => std::ptr::null(),
    };
    info.nShow = 1; // SW_SHOWNORMAL

    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let code = unsafe { GetLastError() };
        if code == ERROR_CANCELLED {
            return Ok(Elevated::Declined);
        }
        let error = std::io::Error::from_raw_os_error(code as i32);
        return Err(msg!(
            "ShellExecuteExW(runas) failed: {error}",
            error = error
        ));
    }

    let handle = info.hProcess;
    if handle.is_null() {
        // The verb ran, but the shell did not hand back a process (it does not
        // have to). Nothing left to wait for.
        return Ok(Elevated::Finished(0));
    }
    unsafe { WaitForSingleObject(handle, INFINITE) };
    let mut exit: u32 = 0;
    unsafe { GetExitCodeProcess(handle, &mut exit) };
    unsafe { CloseHandle(handle) };
    Ok(Elevated::Finished(exit))
}

/// Put this process back on the console of `pid`, the process that started it.
///
/// The shell service creates an elevated process with a console of its own, so
/// a run started from a command line would print into a second window. Freeing
/// that console and attaching to the caller's puts the output — and the prompt
/// the tool prints — where the command was typed. Returns `false` when the
/// caller has no console to attach to (redirected output, a GUI parent), in
/// which case a fresh one is allocated so the output still goes somewhere.
///
/// `CONOUT$`/`CONIN$` have to be reopened and installed with `SetStdHandle`:
/// attaching does not update the standard handles, they still name the console
/// that was just freed. Rust's own stdout is no obstacle — it calls
/// `GetStdHandle` on every write rather than caching it, exactly so that a
/// process can do this.
pub fn attach_console(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        AllocConsole, AttachConsole, FreeConsole, SetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE,
    };

    /// Open a console pseudo-file (`CONOUT$`, `CONIN$`).
    unsafe fn open(name: &str) -> windows_sys::Win32::Foundation::HANDLE {
        unsafe {
            CreateFileW(
                wide(name).as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        }
    }

    unsafe {
        // A process can be attached to at most one console, so the one the
        // elevated copy was given has to go first.
        FreeConsole();
        if AttachConsole(pid) == 0 {
            // Rather a window of its own than output that goes nowhere.
            AllocConsole();
            return false;
        }
        let out = open("CONOUT$");
        if out != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_OUTPUT_HANDLE, out);
            SetStdHandle(STD_ERROR_HANDLE, out);
        }
        let input = open("CONIN$");
        if input != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_INPUT_HANDLE, input);
        }
    }
    true
}
