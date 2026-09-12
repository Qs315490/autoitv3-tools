//! Linux process probes backing the cross-platform `Run` family.
//!
//! The common process service ([`crate::common::proc`]) is portable
//! `std::process` code; the parts that need to look at *other* processes go
//! through `/proc` here, mirroring how the Windows layer backs the same
//! helpers with `Toolhelp32` and `K32GetProcessMemoryInfo`.

// The `linux` module compiles on every host, but on non-Linux builds these
// probes have no caller (the Windows layer answers through its own APIs).
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::path::Path;

/// The name of `pid` from `/proc/<pid>/comm`, empty when it is gone.
pub(crate) fn process_name(pid: i64) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|s| s.trim().to_string())
}

/// Every running process as `(pid, name)`, read from `/proc`.
pub(crate) fn system_processes() -> Vec<(i64, String)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i64>().ok())
        else {
            continue;
        };
        let name = process_name(pid).unwrap_or_default();
        out.push((pid, name));
    }
    out.sort_by_key(|(pid, _)| *pid);
    out
}

/// Whether `pid` is alive right now — a single `/proc` probe, which also
/// catches processes that appeared after the table was built.
pub(crate) fn pid_exists(pid: i64) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// `(VmRSS, VmHWM)` in bytes, from `/proc/<pid>/status`.
pub(crate) fn read_memory(pid: i64) -> (Option<i64>, Option<i64>) {
    let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
        return (None, None);
    };
    let field = |key: &str| -> Option<i64> {
        text.lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<i64>().ok())
            .map(|kb| kb * 1024)
    };
    (field("VmRSS:"), field("VmHWM:"))
}
