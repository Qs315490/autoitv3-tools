//! Linux platform — the functions that genuinely differ from other systems.
//!
//! Everything in the common layer lives in [`crate::common`] and is installed on every
//! platform. What remains here is what a Linux build has to answer for itself:
//!
//! * `ProcessExists`, `ProcessList`, `ProcessClose` — resolved through `/proc`,
//!   which is the Linux equivalent of the Windows process APIs
//! * `ProcessWait`/`ProcessWaitClose` and the `Run`/`StdoutRead` family live in
//!   the cross-platform common layer (`crate::common::proc`), so they are not
//!   repeated here
//!
//! Windows-only areas (registry, COM, `DllCall`, GUI, clipboard) are **not**
//! stubbed here. AutoIt is a Windows tool; on Linux the honest answer is "not
//! provided", which the interpreter turns into an undefined-function error
//! instead of a silently invented value.

use std::fs;
use std::path::Path;

use autoitv3_runtime::error::RuntimeError;
use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::platform::Platform;
use autoitv3_runtime::value::Value;

/// Every function this layer implements.
pub const FUNCTIONS: &[&str] = &["ProcessExists", "ProcessList", "ProcessClose"];

/// `/proc` probes shared with the common process service. The `linux` module
/// compiles on every host (the `lib.rs` stack is chosen at compile time) and
/// these are pure filesystem reads, so no cfg gate is needed.
pub(crate) mod proc_support;

/// The Linux platform.
#[derive(Debug, Default)]
pub struct LinuxPlatform {
    _private: (),
}

impl LinuxPlatform {
    /// Create the Linux platform.
    pub fn new() -> Self {
        Self { _private: () }
    }
}

/// Every running process as `(pid, name)`, read from `/proc`.
fn process_table() -> Vec<(i64, String)> {
    proc_support::system_processes()
}

/// Match a process name the way AutoIt does: case-insensitively, with or
/// without the `.exe` suffix.
fn name_matches(candidate: &str, wanted: &str) -> bool {
    let c = candidate.to_ascii_lowercase();
    let mut w = wanted.to_ascii_lowercase();
    if let Some(stripped) = w.strip_suffix(".exe") {
        w = stripped.to_string();
    }
    c == w
}

impl Platform for LinuxPlatform {
    fn name(&self) -> &'static str {
        "linux"
    }

    fn provides(&self, name: &str) -> bool {
        FUNCTIONS.iter().any(|f| f.eq_ignore_ascii_case(name))
    }

    /// OS identity, which only the system layer can answer.
    fn macro_value(&self, name: &str) -> Option<Value> {
        let v = match name {
            "osversion" | "ostype" => Value::Str("LINUX".into()),
            "osarch" | "processorarch" => Value::Str(
                match std::env::consts::ARCH {
                    "x86_64" => "X64",
                    "x86" => "X86",
                    "aarch64" => "ARM64",
                    other => other,
                }
                .to_string(),
            ),
            // Unix has a single root rather than drive letters.
            "homedrive" => Value::Str("/".into()),
            "computername" => Value::Str(
                fs::read_to_string("/proc/sys/kernel/hostname")
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default(),
            ),
            _ => return None,
        };
        Some(v)
    }

    fn call(
        &mut self,
        name: &str,
        args: Vec<Value>,
        _ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        let key = name.to_ascii_lowercase();
        let arg = |i: usize| -> String {
            args.get(i).map(|v| v.to_autoit_string()).unwrap_or_default()
        };
        let v = match key.as_str() {
            "processexists" => {
                let wanted = arg(0);
                // AutoIt accepts either a PID or a process name.
                if let Ok(pid) = wanted.parse::<i64>() {
                    Value::Int(i64::from(Path::new("/proc").join(pid.to_string()).exists()))
                } else {
                    let hit = process_table()
                        .iter()
                        .any(|(_, n)| name_matches(n, &wanted));
                    Value::Int(i64::from(hit))
                }
            }
            "processlist" => {
                let procs = process_table();
                let mut out = vec![Value::Int(procs.len() as i64)];
                for (pid, pname) in procs {
                    let mut inner = vec![Value::Str(pname), Value::Int(pid)];
                    inner.shrink_to_fit();
                    out.push(Value::array(inner));
                }
                Value::array(out)
            }
            "processclose" => {
                // Without a signal API in the interpreter, report only whether
                // the process exists and would have been targeted.
                let wanted = arg(0);
                let exists = if let Ok(pid) = wanted.parse::<i64>() {
                    Path::new("/proc").join(pid.to_string()).exists()
                } else {
                    process_table().iter().any(|(_, n)| name_matches(n, &wanted))
                };
                Value::Int(i64::from(exists))
            }
            _ => return Ok(None),
        };
        Ok(Some(v))
    }
}