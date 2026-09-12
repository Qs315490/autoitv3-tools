//! Process / execution service — the `Run` family of AutoIt builtins.
//!
//! This module is the **unified interface** for the family on every operating
//! system; the parts that differ per OS are implemented in the system modules
//! and *called from here*:
//!
//! * the portable `std::process` machinery (spawn, stream capture, waits)
//!   lives in this module and is shared by every host;
//! * the platform-divergent observation points — process tables, liveness,
//!   memory — are implemented in the system modules
//!   ([`crate::linux::proc_support`] on Linux, [`crate::windows::process`] on
//!   Windows) behind three per-host hooks (`system_processes`, `pid_alive`,
//!   `read_memory`) that this module dispatches to. Adding a new host means
//!   implementing those hooks in its system module, not touching the family's
//!   interface here.
//!
//! The functions answered through this unified interface:
//!
//! * `Run` / `RunWait` — spawn a child and return its PID / exit code.
//! * `StdoutRead` / `StderrRead` / `StdinWrite` / `StdioClose` — the redirected
//!   standard streams, using a reader thread per stream so reads never block.
//! * `ProcessWait` / `ProcessWaitClose` — poll for a process to appear / exit.
//! * `ProcessGetStats` / `ProcessSetPriority` — process information.
//!
//! # Execution profile
//!
//! Starting a process changes the machine, so the whole family is gated by
//! [`EffectPolicy`](autoitv3_runtime::profile::EffectPolicy): under the
//! deterministic analysis profile they fail with `@error = 1` instead of
//! launching anything, and the blocking waits return immediately rather than
//! risking a hang.
//!
//! # Deliberate approximations
//!
//! * AutoIt's `show_flag` (a window state) has no meaning off Windows and is
//!   ignored; stdin/stdout/stderr are `null` unless the `opt_flag` asks for them.
//! * `$STDERR_MERGED` (8) is emulated by concatenating the stderr buffer into
//!   `StdoutRead` rather than dup'ing the OS handle.
//! * `ProcessSetPriority` reports success without changing the nice level.
//! * The platform-divergent probes — process tables, liveness and memory —
//!   live in the system layers (`crate::linux::proc_support`,
//!   `crate::windows::process`); this module only calls them.

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::profile::EffectKind;
use autoitv3_runtime::value::Value;

/// Every function this service implements.
pub const FUNCTIONS: &[&str] = &[
    "Run",
    "RunWait",
    "ProcessWait",
    "ProcessWaitClose",
    "ProcessGetStats",
    "ProcessSetPriority",
    "StdoutRead",
    "StderrRead",
    "StdinWrite",
    "StdioClose",
];

/// AutoIt's `$STDIN_CHILD`.
const STDIN_CHILD: i64 = 1;
/// AutoIt's `$STDOUT_CHILD`.
const STDOUT_CHILD: i64 = 2;
/// AutoIt's `$STDERR_CHILD`.
const STDERR_CHILD: i64 = 4;
/// AutoIt's `$STDERR_MERGED`.
const STDERR_MERGED: i64 = 8;

/// How long `ProcessWait`/`ProcessWaitClose` sleep between polls, matching
/// AutoIt's documented ~250 ms.
const POLL: Duration = Duration::from_millis(250);

struct ProcEntry {
    child: Child,
    pid: u32,
    stdout: Option<Arc<Mutex<Vec<u8>>>>,
    stderr: Option<Arc<Mutex<Vec<u8>>>>,
    stdout_join: Option<JoinHandle<()>>,
    stderr_join: Option<JoinHandle<()>>,
    merged: bool,
    stdio_closed: bool,
}

impl ProcEntry {
    /// Wait for the reader threads to finish so their buffers are complete.
    /// Called once the child has exited.
    fn join_streams(&mut self) {
        if let Some(j) = self.stdout_join.take() {
            let _ = j.join();
        }
        if let Some(j) = self.stderr_join.take() {
            let _ = j.join();
        }
    }
}

/// The process service.
#[derive(Default)]
pub struct ProcessService {
    procs: Vec<ProcEntry>,
}

impl ProcessService {
    /// Create an empty process service.
    pub fn new() -> Self {
        Self { procs: Vec::new() }
    }

    /// Whether this service provides `name`.
    pub fn provides(name: &str) -> bool {
        FUNCTIONS.iter().any(|f| f.eq_ignore_ascii_case(name))
    }

    /// Dispatch a call; `None` means "not mine".
    pub fn call(
        &mut self,
        name: &str,
        args: &[Value],
        ctx: &mut dyn HostContext,
    ) -> Option<Value> {
        Some(match name {
            "run" => self.run(args, false, ctx),
            "runwait" => self.run(args, true, ctx),
            "processwait" => self.process_wait(args, ctx),
            "processwaitclose" => self.process_wait_close(args, ctx),
            "processgetstats" => self.process_stats(args, ctx),
            "processsetpriority" => self.process_set_priority(args, ctx),
            "stdoutread" => self.read_stream(args, ctx, true),
            "stderrread" => self.read_stream(args, ctx, false),
            "stdinwrite" => self.stdin_write(args, ctx),
            "stdioclose" => self.stdio_close(args, ctx),
            _ => return None,
        })
    }

    fn index_of(&self, pid: i64) -> Option<usize> {
        self.procs.iter().position(|p| p.pid as i64 == pid)
    }

    /// Whether a process is still running. Children we spawned are reaped with
    /// `try_wait`; anything else is probed on the host (`/proc` on Linux, the
    /// process snapshot on Windows).
    fn alive(&mut self, pid: i64) -> bool {
        if let Some(i) = self.index_of(pid) {
            return matches!(self.procs[i].child.try_wait(), Ok(None));
        }
        pid_alive(pid)
    }

    fn run(&mut self, args: &[Value], wait: bool, ctx: &mut dyn HostContext) -> Value {
        if !ctx.effect_allowed(EffectKind::Spawn) {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let program = arg_str(args, 0);
        let workdir = arg_str(args, 1);
        let opt = arg_int(args, 3);
        let tokens = split_command_line(&program);
        if tokens.is_empty() {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let mut cmd = Command::new(&tokens[0]);
        cmd.args(&tokens[1..]);
        if !workdir.is_empty() {
            cmd.current_dir(&workdir);
        }
        let merged = opt & STDERR_MERGED != 0;
        let want_stdout = opt & STDOUT_CHILD != 0 || merged;
        let want_stderr = opt & STDERR_CHILD != 0 || merged;
        cmd.stdin(if opt & STDIN_CHILD != 0 {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        cmd.stdout(if want_stdout {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        cmd.stderr(if want_stderr {
            Stdio::piped()
        } else {
            Stdio::null()
        });

        match cmd.spawn() {
            Ok(mut child) => {
                let pid = child.id();
                let (stdout, stdout_join) = match child.stdout.take() {
                    Some(s) => {
                        let (b, j) = spawn_reader(s);
                        (Some(b), Some(j))
                    }
                    None => (None, None),
                };
                let (stderr, stderr_join) = match child.stderr.take() {
                    Some(s) => {
                        let (b, j) = spawn_reader(s);
                        (Some(b), Some(j))
                    }
                    None => (None, None),
                };
                if wait {
                    let status = child.wait();
                    ctx.set_error(if status.is_ok() { 0 } else { 1 }, 0);
                    let code = status.ok().and_then(|s| s.code()).unwrap_or(0);
                    return Value::Int(i64::from(code));
                }
                self.procs.push(ProcEntry {
                    child,
                    pid,
                    stdout,
                    stderr,
                    stdout_join,
                    stderr_join,
                    merged,
                    stdio_closed: false,
                });
                ctx.set_error(0, 0);
                Value::Int(i64::from(pid))
            }
            Err(_) => {
                ctx.set_error(1, 0);
                Value::Int(0)
            }
        }
    }

    fn process_wait(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        // A blocking wait could hang an analysis run, so the deterministic
        // profile answers immediately.
        if !ctx.effect_allowed(EffectKind::Spawn) {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let wanted = arg_str(args, 0);
        let timeout = arg_int(args, 1);
        let deadline = deadline_after(timeout);
        loop {
            if let Some((pid, _)) = find_process(&wanted) {
                ctx.set_error(0, 0);
                return Value::Int(pid);
            }
            if expired(deadline) {
                ctx.set_error(1, 0);
                return Value::Int(0);
            }
            std::thread::sleep(POLL);
        }
    }

    fn process_wait_close(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        if !ctx.effect_allowed(EffectKind::Spawn) {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let target = args.first().map(|v| v.to_autoit_string()).unwrap_or_default();
        let timeout = arg_int(args, 1);
        let deadline = deadline_after(timeout);
        let pid = target.parse::<i64>().ok();
        loop {
            let running = match pid {
                Some(p) => self.alive(p),
                None => find_process(&target).is_some(),
            };
            if !running {
                if let Some(p) = pid {
                    if let Some(i) = self.index_of(p) {
                        self.procs[i].join_streams();
                    }
                }
                ctx.set_error(0, 0);
                return Value::Int(1);
            }
            if expired(deadline) {
                ctx.set_error(1, 0);
                return Value::Int(0);
            }
            std::thread::sleep(POLL);
        }
    }

    fn process_stats(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let Some(pid) = resolve_pid(args, 0) else {
            ctx.set_error(1, 0);
            return Value::array(vec![Value::Int(-1)]);
        };
        let (rss, peak) = read_memory(pid);
        if rss.is_none() && peak.is_none() {
            ctx.set_error(1, 0);
            return Value::array(vec![Value::Int(-1)]);
        }
        ctx.set_error(0, 0);
        Value::array(vec![
            Value::Int(rss.unwrap_or(0)),
            Value::Int(peak.unwrap_or(0)),
        ])
    }

    fn process_set_priority(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let Some(pid) = resolve_pid(args, 0) else {
            ctx.set_error(1, 0);
            return Value::Int(0);
        };
        // The priority argument is accepted but not applied: there is no
        // portable, unprivileged way to change another process's nice level.
        if self.alive(pid) {
            ctx.set_error(0, 0);
            Value::Int(1)
        } else {
            ctx.set_error(1, 0);
            Value::Int(0)
        }
    }

    fn read_stream(&mut self, args: &[Value], ctx: &mut dyn HostContext, out: bool) -> Value {
        let pid = arg_int(args, 0);
        let peek = args.get(1).map(|v| v.is_truthy()).unwrap_or(false);
        let binary = args.get(2).map(|v| v.is_truthy()).unwrap_or(false);
        let Some(i) = self.index_of(pid) else {
            ctx.set_error(1, 0);
            return Value::str("");
        };
        if self.procs[i].stdio_closed {
            ctx.set_error(1, 0);
            return Value::str("");
        }
        // Once the child has exited, wait for its reader threads so the buffers
        // hold everything they captured before we hand it to the script.
        let exited = matches!(self.procs[i].child.try_wait(), Ok(Some(_)) | Err(_));
        if exited {
            self.procs[i].join_streams();
        }
        let (mut data, merged) = {
            let entry = &self.procs[i];
            (
                drain(if out { &entry.stdout } else { &entry.stderr }, peek),
                entry.merged,
            )
        };
        if out && merged {
            data.extend(drain(&self.procs[i].stderr, peek));
        }
        if data.is_empty() {
            ctx.set_error(if exited { 1 } else { 0 }, 0);
            return if binary {
                Value::Binary(Rc::new(Vec::new()))
            } else {
                Value::str("")
            };
        }
        let n = data.len() as i64;
        ctx.set_error(0, n);
        if binary {
            Value::Binary(Rc::new(data))
        } else {
            Value::Str(String::from_utf8_lossy(&data).into_owned())
        }
    }

    fn stdin_write(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let pid = arg_int(args, 0);
        let data = arg_str(args, 1);
        let Some(i) = self.index_of(pid) else {
            ctx.set_error(1, 0);
            return Value::Int(0);
        };
        if self.procs[i].stdio_closed {
            ctx.set_error(1, 0);
            return Value::Int(0);
        }
        let n = match self.procs[i].child.stdin.as_mut() {
            Some(s) => match s.write_all(data.as_bytes()).and_then(|()| s.flush()) {
                Ok(()) => data.len() as i64,
                Err(_) => {
                    ctx.set_error(1, 0);
                    return Value::Int(0);
                }
            },
            None => {
                ctx.set_error(1, 0);
                return Value::Int(0);
            }
        };
        ctx.set_error(0, n);
        Value::Int(n)
    }

    fn stdio_close(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let pid = arg_int(args, 0);
        let Some(i) = self.index_of(pid) else {
            ctx.set_error(1, 0);
            return Value::Int(0);
        };
        let entry = &mut self.procs[i];
        entry.child.stdin.take();
        entry.stdout = None;
        entry.stderr = None;
        entry.stdio_closed = true;
        ctx.set_error(0, 0);
        Value::Int(1)
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn arg_str(args: &[Value], i: usize) -> String {
    args.get(i).map(|v| v.to_autoit_string()).unwrap_or_default()
}

fn arg_int(args: &[Value], i: usize) -> i64 {
    args.get(i).map(|v| v.to_int()).unwrap_or(0)
}

/// Resolve a PID-or-name argument to a PID.
fn resolve_pid(args: &[Value], i: usize) -> Option<i64> {
    let v = args.get(i)?;
    if v.is_number() {
        let pid = v.to_int();
        return (pid > 0).then_some(pid);
    }
    find_process(&v.to_autoit_string()).map(|(pid, _)| pid)
}

/// A deadline for a wait given a timeout in **seconds** (0 = no deadline, as
/// AutoIt documents for `ProcessWait`).
fn deadline_after(seconds: i64) -> Option<Instant> {
    (seconds > 0).then(|| Instant::now() + Duration::from_secs(seconds as u64))
}

fn expired(deadline: Option<Instant>) -> bool {
    matches!(deadline, Some(d) if Instant::now() >= d)
}

/// Consume (or peek) a stream buffer.
fn drain(buf: &Option<Arc<Mutex<Vec<u8>>>>, peek: bool) -> Vec<u8> {
    let Some(b) = buf else {
        return Vec::new();
    };
    let mut g = b.lock().unwrap();
    if peek {
        g.clone()
    } else {
        std::mem::take(&mut *g)
    }
}

/// Read a child stream on a background thread so `StdoutRead` never blocks.
fn spawn_reader<R: Read + Send + 'static>(mut r: R) -> (Arc<Mutex<Vec<u8>>>, JoinHandle<()>) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let sink = buf.clone();
    let handle = std::thread::spawn(move || {
        let mut tmp = [0u8; 4096];
        loop {
            match r.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => sink.lock().unwrap().extend_from_slice(&tmp[..n]),
            }
        }
    });
    (buf, handle)
}

/// Split an AutoIt program line (`"C:\a.exe" -x`) into argv, honouring quotes.
fn split_command_line(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `(pid, name)` for a process named `wanted`, or a numeric PID probe.
fn find_process(wanted: &str) -> Option<(i64, String)> {
    if let Ok(pid) = wanted.parse::<i64>() {
        if pid <= 0 {
            return None;
        }
        return system_processes()
            .into_iter()
            .find(|(p, _)| *p == pid)
            .filter(|_| pid_alive(pid));
    }
    system_processes()
        .into_iter()
        .find(|(_, name)| name_matches(name, wanted))
}

fn name_matches(candidate: &str, wanted: &str) -> bool {
    let c = candidate.to_ascii_lowercase();
    let mut w = wanted.to_ascii_lowercase();
    if let Some(stripped) = w.strip_suffix(".exe") {
        w = stripped.to_string();
    }
    c == w
}

// --- per-platform probes ---------------------------------------------------
// The listing/memory helpers are the platform-divergent part of this service:
// Linux answers through `/proc` (`crate::linux::proc_support`), Windows
// through `Toolhelp32`/`K32GetProcessMemoryInfo` (`crate::windows::process`).

#[cfg(target_os = "linux")]
fn system_processes() -> Vec<(i64, String)> {
    crate::linux::proc_support::system_processes()
}

#[cfg(target_os = "linux")]
fn pid_alive(pid: i64) -> bool {
    crate::linux::proc_support::pid_exists(pid)
}

#[cfg(target_os = "linux")]
fn read_memory(pid: i64) -> (Option<i64>, Option<i64>) {
    crate::linux::proc_support::read_memory(pid)
}

#[cfg(windows)]
fn system_processes() -> Vec<(i64, String)> {
    crate::windows::process::system_processes()
}

#[cfg(windows)]
fn pid_alive(pid: i64) -> bool {
    crate::windows::process::system_processes()
        .iter()
        .any(|(p, _)| *p == pid)
}

#[cfg(windows)]
fn read_memory(pid: i64) -> (Option<i64>, Option<i64>) {
    crate::windows::process::process_memory(pid)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn system_processes() -> Vec<(i64, String)> {
    Vec::new()
}

#[cfg(not(any(target_os = "linux", windows)))]
fn pid_alive(_pid: i64) -> bool {
    false
}

#[cfg(not(any(target_os = "linux", windows)))]
fn read_memory(_pid: i64) -> (Option<i64>, Option<i64>) {
    (None, None)
}
