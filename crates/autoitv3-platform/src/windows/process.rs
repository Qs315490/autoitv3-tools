//! Process enumeration on Windows — the Toolhelp snapshot.
//!
//! `ProcessList` / `ProcessExists` / `ProcessClose` are native here; the
//! common layer's `Run`/wait family reuses [`system_processes`] and
//! [`process_memory`] for its own name lookups and `ProcessGetStats`.

use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::value::Value;

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
    TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::ProcessStatus::{
    K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, TerminateProcess, PROCESS_QUERY_INFORMATION, PROCESS_TERMINATE,
};

use autoitv3_runtime::profile::EffectKind;

/// `(pid, exe name)` for every process on the system, pid-ordered.
pub(crate) fn system_processes() -> Vec<(i64, String)> {
    let mut out = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() || snapshot == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|u| *u == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
                out.push((entry.th32ProcessID as i64, name));
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }
    out.sort_by_key(|(pid, _)| *pid);
    out
}

/// `(working set, peak working set)` in bytes for `pid`, if it is running and
/// readable.
pub(crate) fn process_memory(pid: i64) -> (Option<i64>, Option<i64>) {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid as u32);
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return (None, None);
        }
        let mut counters: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = K32GetProcessMemoryInfo(handle, &mut counters, counters.cb) != 0;
        CloseHandle(handle);
        if !ok {
            return (None, None);
        }
        (
            Some(counters.WorkingSetSize as i64),
            Some(counters.PeakWorkingSetSize as i64),
        )
    }
}

/// `(pid, name)` for a name-or-pid argument, AutoIt-style (`.exe` optional).
fn resolve(args: &[Value]) -> Option<(i64, String)> {
    let value = args.first()?;
    if value.is_number() {
        let pid = value.to_int();
        if pid <= 0 {
            return None;
        }
        let name = system_processes()
            .into_iter()
            .find(|(p, _)| *p == pid)
            .map(|(_, n)| n)?;
        return Some((pid, name));
    }
    let wanted = value.to_autoit_string();
    let mut needle = wanted.to_ascii_lowercase();
    if let Some(stripped) = needle.strip_suffix(".exe") {
        needle = stripped.to_string();
    }
    system_processes()
        .into_iter()
        .find(|(_, name)| name.to_ascii_lowercase().starts_with(&needle))
}

/// `ProcessList()` — `[[count], [name, pid], …]` like AutoIt's 2D result.
pub(crate) fn process_list() -> Value {
    let procs = system_processes();
    let mut rows = Vec::with_capacity(procs.len() + 1);
    rows.push(Value::array(vec![
        Value::Int(procs.len() as i64),
        Value::Int(0),
    ]));
    for (pid, name) in procs {
        rows.push(Value::array(vec![Value::Str(name), Value::Int(pid)]));
    }
    Value::array(rows)
}

/// `ProcessExists` — the PID when found, `0` otherwise.
pub(crate) fn process_exists(args: &[Value]) -> Value {
    Value::Int(resolve(args).map(|(pid, _)| pid).unwrap_or(0))
}

/// `ProcessClose` — terminate by name or PID; effect-gated.
pub(crate) fn process_close(args: &[Value], ctx: &mut dyn HostContext) -> Value {
    if !ctx.effect_allowed(EffectKind::ProcessControl) {
        ctx.set_error(1, 0);
        return Value::Int(0);
    }
    let Some((pid, _)) = resolve(args) else {
        ctx.set_error(1, 0);
        return Value::Int(0);
    };
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid as u32);
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let ok = TerminateProcess(handle, 1) != 0;
        CloseHandle(handle);
        ctx.set_error(if ok { 0 } else { 1 }, 0);
        Value::Int(i64::from(ok))
    }
}
