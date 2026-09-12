//! System-information and shell-launch functions with real Win32 semantics.
//!
//! * `MemGetStats` — `GlobalMemoryStatusEx`.
//! * `IsAdmin` — BUILTIN\Administrators token membership.
//! * `ShellExecute`/`ShellExecuteWait` — `ShellExecuteExW`, so document
//!   associations and verbs work; the wait form returns the exit code.
//! * `RunAs`/`RunAsWait` — `CreateProcessWithLogonW`, so the credentials are
//!   actually applied (the emulation accepts them but stays on the current
//!   user).
//! * `DriveMapAdd`/`DriveMapDel`/`DriveMapGet` — the WNet network-provider
//!   calls.
//! * `Shutdown` — `ExitWindowsEx` with AutoIt's flag mapping; refuse `32`
//!   (standby) and `64` (hibernate), which have no ExitWindowsEx equivalent.
//!
//! Every side effect is gated by the execution profile like the rest of the
//! native layer.

use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::value::Value;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_MORE_DATA, ERROR_SUCCESS, WAIT_OBJECT_0};
use windows_sys::Win32::NetworkManagement::WNet::{
    WNetAddConnection2W, WNetCancelConnection2W, WNetGetConnectionW, NETRESOURCEW,
    CONNECT_UPDATE_PROFILE, RESOURCETYPE_DISK,
};
use windows_sys::Win32::Security::{
    AllocateAndInitializeSid, CheckTokenMembership, FreeSid, PSID, SID_IDENTIFIER_AUTHORITY,
};
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows_sys::Win32::System::SystemServices::{
    DOMAIN_ALIAS_RID_ADMINS, SECURITY_BUILTIN_DOMAIN_RID,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessWithLogonW, GetExitCodeProcess, GetProcessId, WaitForSingleObject,
    CREATE_UNICODE_ENVIRONMENT, INFINITE, LOGON_NETCREDENTIALS_ONLY, LOGON_WITH_PROFILE,
    PROCESS_INFORMATION, PROCESS_QUERY_INFORMATION, STARTUPINFOW,
};
use windows_sys::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};

use autoitv3_runtime::profile::EffectKind;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn arg(args: &[Value], i: usize) -> String {
    args.get(i).map(|v| v.to_autoit_string()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// memory / identity
// ---------------------------------------------------------------------------

/// `MemGetStats()` — the seven-element memory array AutoIt documents.
pub(crate) fn mem_get_stats(ctx: &mut dyn HostContext) -> Value {
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        ctx.set_error(1, 0);
        return Value::Int(0);
    }
    let kb = |bytes: u64| (bytes / 1024) as i64;
    ctx.set_error(0, 0);
    Value::array(vec![
        Value::Int(status.dwMemoryLoad as i64),
        Value::Int(kb(status.ullTotalPhys)),
        Value::Int(kb(status.ullAvailPhys)),
        Value::Int(kb(status.ullTotalPageFile)),
        Value::Int(kb(status.ullAvailPageFile)),
        Value::Int(kb(status.ullTotalVirtual)),
        Value::Int(kb(status.ullAvailVirtual)),
    ])
}

/// `IsAdmin()` — membership in the BUILTIN\Administrators group.
pub(crate) fn is_admin() -> Value {
    let authority = SID_IDENTIFIER_AUTHORITY { Value: [0, 0, 0, 0, 0, 5] };
    let mut sid: PSID = std::ptr::null_mut();
    let allocated = unsafe {
        AllocateAndInitializeSid(
            &authority,
            2,
            SECURITY_BUILTIN_DOMAIN_RID as u32,
            DOMAIN_ALIAS_RID_ADMINS as u32,
            0,
            0,
            0,
            0,
            0,
            0,
            &mut sid,
        )
    };
    if allocated == 0 {
        return Value::Int(0);
    }
    let mut member = 0;
    let ok = unsafe { CheckTokenMembership(std::ptr::null_mut(), sid, &mut member) } != 0;
    unsafe { FreeSid(sid) };
    Value::Int(i64::from(ok && member != 0))
}

// ---------------------------------------------------------------------------
// shell execution
// ---------------------------------------------------------------------------

/// `ShellExecute(file [, parameters [, workingdir [, verb [, showflag]]]])` —
/// the PID on success (0 with `@error` when the launch failed or the profile
/// refuses).
pub(crate) fn shell_execute(args: &[Value], ctx: &mut dyn HostContext) -> Value {
    match shell_execute_inner(args) {
        Some(pid) => {
            ctx.set_error(0, 0);
            Value::Int(pid)
        }
        None => {
            ctx.set_error(1, 0);
            Value::Int(0)
        }
    }
}

/// `ShellExecuteWait(...)` — the process exit code.
pub(crate) fn shell_execute_wait(args: &[Value], ctx: &mut dyn HostContext) -> Value {
    if !ctx.effect_allowed(EffectKind::Spawn) {
        ctx.set_error(1, 0);
        return Value::Int(0);
    }
    match shell_execute_inner(args) {
        Some(pid) => {
            // The info's `hProcess` was closed already; reopen by PID to wait.
            let code = open_process(pid)
                .map(|handle| {
                    let code = unsafe { WaitForSingleObject(handle, INFINITE) };
                    let mut exit: u32 = 0;
                    unsafe { GetExitCodeProcess(handle, &mut exit) };
                    unsafe { CloseHandle(handle) };
                    if code == WAIT_OBJECT_0 {
                        exit as i64
                    } else {
                        0
                    }
                })
                .unwrap_or(0);
            ctx.set_error(0, 0);
            Value::Int(code)
        }
        None => {
            ctx.set_error(1, 0);
            Value::Int(0)
        }
    }
}

/// Launch through `ShellExecuteExW`; `Some(pid)` on success.
fn shell_execute_inner(args: &[Value]) -> Option<i64> {
    let file = arg(args, 0);
    if file.is_empty() {
        return None;
    }
    let parameters = arg(args, 1);
    let directory = arg(args, 2);
    let verb = arg(args, 3);
    let show = args.get(4).map(|v| v.to_int()).unwrap_or(1) as i32;

    let file_w = wide(&file);
    let params_w = wide(&parameters);
    let dir_w = wide(&directory);
    let verb_w = wide(&verb);

    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpFile = file_w.as_ptr();
    info.lpParameters = params_w.as_ptr();
    info.lpDirectory = dir_w.as_ptr();
    info.lpVerb = verb_w.as_ptr();
    info.nShow = show;
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return None;
    }
    let pid = if info.hProcess.is_null() {
        0
    } else {
        let pid = unsafe { GetProcessId(info.hProcess) } as i64;
        unsafe { CloseHandle(info.hProcess) };
        pid
    };
    Some(pid)
}

fn open_process(pid: i64) -> Option<windows_sys::Win32::Foundation::HANDLE> {
    let handle = unsafe {
        windows_sys::Win32::System::Threading::OpenProcess(
            SYNCHRONIZE | PROCESS_QUERY_INFORMATION,
            0,
            pid as u32,
        )
    };
    (!handle.is_null()).then_some(handle)
}

/// `RunAs(user, domain, password, logon_flags, program [, workingdir [,
/// showflag [, optflag]]])` — the child PID; the credentials are real.
pub(crate) fn run_as(args: &[Value], ctx: &mut dyn HostContext, wait: bool) -> Value {
    if !ctx.effect_allowed(EffectKind::Spawn) {
        ctx.set_error(1, 0);
        return Value::Int(0);
    }
    let user = arg(args, 0);
    let domain = arg(args, 1);
    let password = arg(args, 2);
    let logon_flags = args.get(3).map(|v| v.to_int()).unwrap_or(0) as u32;
    let program = arg(args, 4);
    let working_dir = arg(args, 5);
    if program.is_empty() {
        ctx.set_error(1, 0);
        return Value::Int(0);
    }
    let flags: u32 = if logon_flags & 2 != 0 {
        LOGON_NETCREDENTIALS_ONLY
    } else {
        LOGON_WITH_PROFILE
    };
    // The whole command line lives in lpCommandLine.
    let mut cmd: Vec<u16> = wide(&program);
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let user_w = wide(&user);
    let domain_w = wide(&domain);
    let password_w = wide(&password);
    let dir_w = wide(&working_dir);
    let started = unsafe {
        CreateProcessWithLogonW(
            user_w.as_ptr(),
            if domain.is_empty() { std::ptr::null() } else { domain_w.as_ptr() },
            password_w.as_ptr(),
            flags,
            std::ptr::null(),
            cmd.as_mut_ptr(),
            CREATE_UNICODE_ENVIRONMENT,
            std::ptr::null(),
            if working_dir.is_empty() { std::ptr::null() } else { dir_w.as_ptr() },
            &startup,
            &mut info,
        )
    };
    if started == 0 {
        ctx.set_error(1, 0);
        return Value::Int(0);
    }
    let pid = unsafe { GetProcessId(info.hProcess) } as i64;
    if wait {
        unsafe { WaitForSingleObject(info.hProcess, INFINITE) };
        let mut exit: u32 = 0;
        unsafe { GetExitCodeProcess(info.hProcess, &mut exit) };
        unsafe { CloseHandle(info.hProcess) };
        unsafe { CloseHandle(info.hThread) };
        ctx.set_error(0, 0);
        return Value::Int(exit as i64);
    }
    unsafe { CloseHandle(info.hProcess) };
    unsafe { CloseHandle(info.hThread) };
    ctx.set_error(0, 0);
    Value::Int(pid)
}

// ---------------------------------------------------------------------------
// drive mappings
// ---------------------------------------------------------------------------

/// `DriveMapAdd(device, share [, flags [, user [, password]]])`.
pub(crate) fn drive_map_add(args: &[Value], ctx: &mut dyn HostContext) -> Value {
    if !ctx.effect_allowed(EffectKind::NetAccess) {
        ctx.set_error(1, 0);
        return Value::Int(0);
    }
    let device = arg(args, 0).trim().to_ascii_uppercase();
    let share = arg(args, 1);
    if share.is_empty() {
        ctx.set_error(5, 0);
        return Value::Int(0);
    }
    if !device.is_empty() && device != "*" && !is_drive_device(&device) {
        ctx.set_error(4, 0);
        return Value::Int(0);
    }
    let user = arg(args, 3);
    let password = arg(args, 4);
    let local = if device == "*" { String::new() } else { device.clone() };
    let local_w = wide(&local);
    let remote_w = wide(&share);
    let user_w = wide(&user);
    let password_w = wide(&password);
    let mut resource: NETRESOURCEW = unsafe { std::mem::zeroed() };
    resource.dwType = RESOURCETYPE_DISK;
    resource.lpLocalName = local_w.as_ptr() as _;
    resource.lpRemoteName = remote_w.as_ptr() as _;
    let status = unsafe {
        WNetAddConnection2W(
            &resource,
            if password.is_empty() { std::ptr::null() } else { password_w.as_ptr() },
            if user.is_empty() { std::ptr::null() } else { user_w.as_ptr() },
            CONNECT_UPDATE_PROFILE,
        )
    };
    ctx.set_error(map_wnet_error(status), status as i64);
    if status == ERROR_SUCCESS {
        if device == "*" {
            // The provider picked a letter; ask which one.
            Value::Str(query_connection(&local).unwrap_or_default())
        } else {
            Value::Int(1)
        }
    } else {
        Value::Int(0)
    }
}

/// `DriveMapDel(device)`.
pub(crate) fn drive_map_del(args: &[Value], ctx: &mut dyn HostContext) -> Value {
    if !ctx.effect_allowed(EffectKind::NetAccess) {
        ctx.set_error(1, 0);
        return Value::Int(0);
    }
    let device = arg(args, 0).trim().to_ascii_uppercase();
    let device_w = wide(&device);
    let status =
        unsafe { WNetCancelConnection2W(device_w.as_ptr(), CONNECT_UPDATE_PROFILE, 1) };
    let ok = status == ERROR_SUCCESS;
    ctx.set_error(if ok { 0 } else { 1 }, 0);
    Value::Int(i64::from(ok))
}

/// `DriveMapGet(device)` — the share path.
pub(crate) fn drive_map_get(args: &[Value], ctx: &mut dyn HostContext) -> Value {
    let device = arg(args, 0).trim().to_ascii_uppercase();
    match query_connection(&device) {
        Some(share) => {
            ctx.set_error(0, 0);
            Value::Str(share)
        }
        None => {
            ctx.set_error(1, 0);
            Value::str("")
        }
    }
}

fn query_connection(device: &str) -> Option<String> {
    let device_w = wide(device);
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let status = unsafe { WNetGetConnectionW(device_w.as_ptr(), buf.as_mut_ptr(), &mut len) };
    if status == ERROR_SUCCESS {
        let end = buf.iter().position(|u| *u == 0).unwrap_or(len as usize);
        Some(String::from_utf16_lossy(&buf[..end]))
    } else if status == ERROR_MORE_DATA {
        // Longer share paths: retry with a growable buffer.
        let mut buf = vec![0u16; len as usize];
        let status = unsafe {
            WNetGetConnectionW(device_w.as_ptr(), buf.as_mut_ptr(), &mut len)
        };
        if status == ERROR_SUCCESS {
            let end = buf.iter().position(|u| *u == 0).unwrap_or(len as usize);
            Some(String::from_utf16_lossy(&buf[..end]))
        } else {
            None
        }
    } else {
        None
    }
}

/// WNet failures to AutoIt's documented `DriveMapAdd` `@error` codes.
fn map_wnet_error(status: u32) -> i64 {
    match status {
        ERROR_SUCCESS => 0,
        windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED
        | windows_sys::Win32::Foundation::ERROR_LOGON_FAILURE
        | windows_sys::Win32::Foundation::ERROR_INVALID_PASSWORD => 6,
        windows_sys::Win32::Foundation::ERROR_DEVICE_ALREADY_REMEMBERED => 3,
        windows_sys::Win32::Foundation::ERROR_BAD_NET_NAME | windows_sys::Win32::Foundation::ERROR_NO_NETWORK => 1,
        _ => 1,
    }
}

/// `C:`-style device check, mirroring the emulation's validation.
fn is_drive_device(device: &str) -> bool {
    let bytes = device.as_bytes();
    bytes.len() == 2
        && bytes[0].is_ascii_alphabetic()
        && (bytes[1] == b':' || bytes[1] == b'\\' || bytes[1] == b'/')
}

// ---------------------------------------------------------------------------
// shutdown family
// ---------------------------------------------------------------------------

/// `Shutdown(flag)` — AutoIt's code mapping onto `ExitWindowsEx`. Standby (32)
/// and hibernate (64) are refused with `@error = 1`; there is no
/// `ExitWindowsEx` equivalent and `SetSuspendState` needs elevated rights.
pub(crate) fn shutdown(args: &[Value], ctx: &mut dyn HostContext) -> Value {
    if !ctx.effect_allowed(EffectKind::Shutdown) {
        ctx.set_error(1, 0);
        return Value::Int(0);
    }
    let flag = args.first().map(|v| v.to_int()).unwrap_or(0);
    use windows_sys::Win32::System::Shutdown::{EWX_FORCE, EWX_FORCEIFHUNG, EWX_LOGOFF, EWX_POWEROFF, EWX_REBOOT, EWX_SHUTDOWN};
    let base: u32 = match flag & !4 & !16 {
        0 => EWX_LOGOFF,
        1 => EWX_SHUTDOWN,
        _ if flag & 8 != 0 => EWX_POWEROFF,
        _ if flag & 1 != 0 => EWX_SHUTDOWN,
        2 => EWX_REBOOT,
        _ => return {
            ctx.set_error(1, 0);
            Value::Int(0)
        },
    };
    let mut flags = base;
    if flag & 4 != 0 {
        flags |= EWX_FORCE;
    }
    if flag & 16 != 0 {
        flags |= EWX_FORCEIFHUNG;
    }
    let ok = unsafe {
        windows_sys::Win32::System::Shutdown::ExitWindowsEx(flags, 0)
    } != 0;
    ctx.set_error(if ok { 0 } else { 1 }, 0);
    Value::Int(i64::from(ok))
}
