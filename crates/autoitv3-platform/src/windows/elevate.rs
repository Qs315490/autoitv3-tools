//! Starting an elevated copy of this process: `#RequireAdmin`, on Windows.
//!
//! Windows cannot raise the token of a process that is already running, so the
//! only way to get administrator rights is a *new* process. `ShellExecuteExW`
//! with the `runas` verb is what puts the UAC consent prompt on screen; this
//! module is that call and nothing else, so the interpreter can keep all of the
//! policy in `autoitv3-platform`'s [`elevate`](crate::elevate) module and in the
//! command line tool.
//!
//! Like the rest of the files under `windows/`, this one carries no `cfg` of
//! its own — the module declaration is what keeps it off other targets — so it
//! can be type-checked on a host without Windows (see the repository's
//! `guicheck` note). `windows-sys` is declarations only and compiles anywhere.

use std::ffi::OsString;
use std::path::Path;

use crate::elevate::{command_line, Elevated};

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

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    let file = wide(&exe.to_string_lossy());
    let params = wide(&command_line(args));
    let verb = wide("runas");
    let directory = dir.map(|d| wide(&d.to_string_lossy()));

    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    // No `SEE_MASK_NO_CONSOLE`: a console caller keeps the console it has, so
    // the script's `ConsoleWrite` output lands where the user is looking.
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
        return Err(format!(
            "ShellExecuteExW(runas) failed: {}",
            std::io::Error::from_raw_os_error(code as i32)
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
