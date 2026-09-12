//! Tests for the platform stack: layering, selection, and the common
//! function set.
//!
//! The emulation layer has its own suite in `tests/winemu.rs`; here it only
//! matters as the first layer of the stack.

use autoitv3_platform::{
    host_platform, host_platform_with, CompositePlatform, LinuxPlatform, CommonPlatform,
    WindowsEmulation,
};
use autoitv3_runtime::platform::Platform;
use autoitv3_runtime::{ExecutionProfile, Runtime, Value};

fn parse(src: &str) -> autoitv3_ast::Program {
    autoitv3_ast::parse(src).expect("parses")
}

/// A runtime with an explicit, environment-independent platform stack: the
/// emulation layer (Windows 10 default) over common over linux.
fn runtime(prog: &autoitv3_ast::Program) -> Runtime {
    let mut rt = Runtime::with_program(prog);
    rt.set_platform(host_platform_with(WindowsEmulation::new()));
    rt
}

/// Run `Func F()` from `body` on a runtime with the full platform stack.
fn call(body: &str) -> Value {
    let src = format!("Func F()\n{body}\nEndFunc\n");
    let prog = parse(&src);
    let mut rt = runtime(&prog);
    rt.call_function("F", vec![]).expect("no runtime error")
}

fn text(body: &str) -> String {
    call(body).to_autoit_string()
}

/// A scratch directory unique to one test.
fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("au3-platform-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

// ---------------------------------------------------------------------------
// Layering
// ---------------------------------------------------------------------------

#[test]
fn host_platform_stacks_emulation_common_and_system() {
    // `host_platform()` reads the environment, so assert on shape rather than
    // the exact name; the explicit-stack test below pins the default.
    let p = host_platform();
    assert!(p.name().contains("common"), "got {}", p.name());
    assert!(p.macro_value("osversion").is_some());

    let p = host_platform_with(WindowsEmulation::new());
    let expected = if cfg!(windows) {
        "windows+common+winemu"
    } else {
        "winemu+common+linux"
    };
    assert_eq!(p.name(), expected);
}

#[test]
fn the_emulation_layer_can_be_left_out_of_the_stack() {
    let p = host_platform_with(WindowsEmulation::new().disabled());
    let expected = if cfg!(windows) {
        "windows+common"
    } else {
        "common+linux"
    };
    assert_eq!(p.name(), expected);
}

#[test]
fn common_layer_is_present_on_every_platform() {
    let p = CommonPlatform::new();
    assert_eq!(p.name(), "common");
    // Pure AutoIt behaviour, not OS behaviour — available everywhere.
    assert!(p.provides("FileOpen"));
    assert!(p.provides("EnvGet"));
    assert!(p.provides("StringLen") == false, "language builtins are not platform functions");
}

#[test]
fn linux_layer_only_answers_linux_questions() {
    let p = LinuxPlatform::new();
    assert_eq!(p.name(), "linux");
    assert!(p.provides("ProcessExists"));
    // Registry/COM are Windows-only; off Windows they must not be invented.
    assert!(!p.provides("RegRead"));
    assert!(!p.provides("FileOpen"), "file I/O belongs to the common layer");
}

#[test]
fn composite_tries_layers_in_order() {
    let composite = CompositePlatform::new(
        "test",
        vec![
            Box::new(CommonPlatform::new()),
            Box::new(LinuxPlatform::new()),
        ],
    );
    assert!(composite.provides("FileExists"));
    assert!(composite.provides("ProcessList"));
    assert!(!composite.provides("RegRead"));
    assert_eq!(composite.name(), "test");
    assert_eq!(composite.layers().len(), 2);
}

#[test]
fn runtime_helper_installs_the_stack() {
    let prog = parse("Func F()\n    Return 1\nEndFunc\n");
    let mut rt = autoitv3_platform::runtime_with_platform(&prog);
    assert!(rt.platform_name().contains("common"));
    assert!(matches!(rt.call_function("F", vec![]).unwrap(), Value::Int(1)));
}

// ---------------------------------------------------------------------------
// Common: files
// ---------------------------------------------------------------------------

#[test]
fn file_write_read_round_trip() {
    let dir = scratch("rw");
    let path = dir.join("a.txt");
    let body = format!(
        r#"Local $h = FileOpen("{p}", 2)
    FileWriteLine($h, "first")
    FileWrite($h, "second")
    FileClose($h)
    Local $h2 = FileOpen("{p}", 0)
    Local $all = FileRead($h2)
    FileClose($h2)
    Return $all"#,
        p = path.display()
    );
    // FileWriteLine appends @CRLF, FileWrite does not.
    assert_eq!(text(&body), "first\r\nsecond");
}

#[test]
fn file_read_line_is_one_based_and_reports_eof() {
    let dir = scratch("lines");
    let path = dir.join("b.txt");
    std::fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();
    let body = format!(
        r#"Local $h = FileOpen("{p}", 0)
    Local $two = FileReadLine($h, 2)
    Local $gone = FileReadLine($h, 99)
    Local $e = @error
    FileClose($h)
    Return $two & "|" & $gone & "|" & $e"#,
        p = path.display()
    );
    assert_eq!(text(&body), "beta||1");
}

#[test]
fn file_read_line_without_a_line_number_reads_sequentially() {
    let dir = scratch("seq-lines");
    let path = dir.join("c.txt");
    std::fs::write(&path, "one\r\ntwo\r\n\r\nfour").unwrap();
    let body = format!(
        r#"Local $h = FileOpen("{p}", 0)
    Local $a = FileReadLine($h)
    Local $b = FileReadLine($h)
    Local $c = FileReadLine($h)
    Local $d = FileReadLine($h)
    Local $e = FileReadLine($h)
    Local $err = @error
    FileClose($h)
    Return $a & "," & $b & "," & $c & "," & $d & "," & $e & "," & $err"#,
        p = path.display()
    );
    // Empty lines are real lines; only reading past the end is an error.
    assert_eq!(text(&body), "one,two,,four,,1");
}

#[test]
fn dirgetsize_reports_the_extended_triple_with_flag_one() {
    let dir = scratch("dirsize");
    let sub = dir.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(dir.join("a.txt"), vec![0u8; 10]).unwrap();
    std::fs::write(sub.join("b.bin"), vec![0u8; 20]).unwrap();
    let body = format!(
        r#"Local $plain = DirGetSize("{d}")
    Local $ext = DirGetSize("{d}", 1)
    Return $plain & ":" & $ext[0] & ":" & $ext[1] & ":" & $ext[2]"#,
        d = dir.display()
    );
    // 30 bytes in two files; the extended form counts the one subdirectory.
    assert_eq!(text(&body), "30:30:2:1");
}

#[test]
fn file_open_failure_returns_minus_one() {
    let body = r#"Local $h = FileOpen("/definitely/not/here.txt", 0)
    Return $h & ":" & @error"#;
    assert_eq!(text(body), "-1:1");
}

#[test]
fn file_exists_size_and_delete() {
    let dir = scratch("meta");
    let path = dir.join("c.bin");
    std::fs::write(&path, vec![0u8; 2048]).unwrap();
    let body = format!(
        r#"Local $e = FileExists("{p}")
    Local $kb = FileGetSize("{p}", "K")
    Local $del = FileDelete("{p}")
    Return $e & ":" & $kb & ":" & $del & ":" & FileExists("{p}")"#,
        p = path.display()
    );
    assert_eq!(text(&body), "1:2:1:0");
}

#[test]
fn file_get_time_uses_the_autio_layout() {
    let dir = scratch("time");
    let path = dir.join("d.txt");
    std::fs::write(&path, "x").unwrap();
    let got = text(&format!(
        r#"Return FileGetTime("{p}")"#,
        p = path.display()
    ));
    // YYYY/MM/DD HH:MM:SS
    assert_eq!(got.len(), 19, "unexpected layout: {got}");
    assert_eq!(&got[4..5], "/");
    assert_eq!(&got[10..11], " ");
    assert_eq!(&got[13..14], ":");
    assert!(got[..4].parse::<u32>().is_ok(), "year: {got}");
}

#[test]
fn file_copy_and_move() {
    let dir = scratch("copy");
    let a = dir.join("src.txt");
    let b = dir.join("dst.txt");
    let c = dir.join("moved.txt");
    std::fs::write(&a, "payload").unwrap();
    let body = format!(
        r#"Local $c = FileCopy("{a}", "{b}")
    Local $m = FileMove("{b}", "{c}")
    Local $h = FileOpen("{c}", 0)
    Local $s = FileRead($h)
    FileClose($h)
    Return $c & $m & ":" & $s"#,
        a = a.display(),
        b = b.display(),
        c = c.display()
    );
    assert_eq!(text(&body), "11:payload");
    assert!(!b.exists(), "source should have been moved");
}

#[test]
fn directory_create_size_and_remove() {
    let dir = scratch("dir");
    let sub = dir.join("nested/deep");
    let body = format!(
        r#"Local $c = DirCreate("{s}")
    Local $h = FileOpen("{s}/f.txt", 2 + 8)
    FileWrite($h, "12345")
    FileClose($h)
    Local $size = DirGetSize("{s}")
    Local $r = DirRemove("{d}", 1)
    Return $c & ":" & $size & ":" & $r"#,
        s = sub.display(),
        d = dir.join("nested").display()
    );
    assert_eq!(text(&body), "1:5:1");
}

#[test]
fn file_get_attrib_flags_directories() {
    let dir = scratch("attrib");
    let f = dir.join("plain.txt");
    std::fs::write(&f, "x").unwrap();
    assert_eq!(text(&format!(r#"Return FileGetAttrib("{}")"#, dir.display())), "D");
    assert_eq!(text(&format!(r#"Return FileGetAttrib("{}")"#, f.display())), "A");
}

// ---------------------------------------------------------------------------
// Common: environment, math, timers, console
// ---------------------------------------------------------------------------

#[test]
fn environment_get_and_set() {
    let body = r#"EnvSet("AU3_PLATFORM_TEST", "hello")
    Return EnvGet("AU3_PLATFORM_TEST")"#;
    assert_eq!(text(body), "hello");
}

#[test]
fn math_functions() {
    assert_eq!(text("Return Round(3.14159, 2)"), "3.14");
    assert_eq!(text("Return Round(2.5)"), "3");
    assert_eq!(text("Return Round(-2.5)"), "-3");
    assert_eq!(text("Return Sqrt(16)"), "4");
    // Trigonometry is in radians, as AutoIt documents.
    assert_eq!(text("Return Round(Sin(0), 6)"), "0");
    assert_eq!(text("Return Round(ASin(1) * 2, 6)"), "3.141593");
    assert_eq!(text("Return Log(1)"), "0");
    assert_eq!(text("Return Exp(0)"), "1");
    assert_eq!(text("Return Floor(2.9)"), "2");
    assert_eq!(text("Return Ceiling(2.1)"), "3");
}

#[test]
fn randomseed_pins_the_sequence_in_any_profile() {
    // Determinism is a *profile* choice now (see the `profile` test file);
    // what is unconditional is that an explicit seed reproduces exactly.
    let body = r#"RandomSeed(42)
    Local $a = Random(1, 1000)
    RandomSeed(42)
    Return $a = Random(1, 1000)"#;
    assert_eq!(text(body), "True");
}

#[test]
fn random_stays_within_bounds() {
    let body = r#"Local $ok = True
    For $i = 1 To 50
        Local $r = Random(5, 10)
        If $r < 5 Or $r > 10 Then $ok = False
    Next
    Return $ok"#;
    assert_eq!(text(body), "True");
}

#[test]
fn random_without_arguments_is_a_fraction() {
    let body = r#"Local $r = Random()
    Return ($r >= 0) And ($r < 1)"#;
    assert_eq!(text(body), "True");
}

#[test]
fn timers_measure_elapsed_time() {
    let body = r#"Local $t = TimerInit()
    Local $d = TimerDiff($t)
    Return $d >= 0"#;
    assert_eq!(text(body), "True");
}

#[test]
fn console_write_returns_the_character_count() {
    // Writes to the process stdout; the return value is what matters here.
    assert_eq!(text(r#"Return ConsoleWrite("")"#), "0");
    assert_eq!(text(r#"ConsoleWrite("")
    Return StringLen("abc")"#), "3");
}

// ---------------------------------------------------------------------------
// Linux layer
// ---------------------------------------------------------------------------

#[test]
#[cfg(not(windows))]
fn process_list_comes_from_proc() {
    let v = call("Return UBound(ProcessList())");
    let Value::Int(n) = v else { panic!("expected a count, got {v:?}") };
    assert!(n > 0, "expected at least one process, got {n}");
}

#[test]
#[cfg(not(windows))]
fn process_exists_accepts_pid_or_name() {
    // The test process itself is definitely running.
    let me = std::process::id();
    assert_eq!(text(&format!("Return ProcessExists({me})")), "1");
    assert_eq!(text("Return ProcessExists(999999999)"), "0");
}

#[test]
fn nothing_windows_only_is_silently_answered_without_the_emulation_layer() {
    // With the emulation layer switched off nothing emulated answers
    // Windows-only calls. On non-Windows the interpreter must report an
    // undefined function; on a Windows host the native layer answers
    // `RegRead` for real, and a missing value reports `@error = 1` honestly
    // rather than a fabricated value.
    let body = r#"Return RegRead("HKEY_LOCAL_MACHINE\SOFTWAREu3-no-such-key", "V") & @error"#;
    let prog = parse(&format!("Func F()
{body}
EndFunc
"));
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(host_platform_with(WindowsEmulation::new().disabled()));
    let result = rt.call_function("F", vec![]);
    if cfg!(windows) {
        let value = result.expect("native RegRead answers");
        assert_eq!(value.to_autoit_string(), "1", "got honest @error=1");
    } else {
        let err = result.unwrap_err();
        assert!(err.message().contains("undefined function"), "got: {}", err.message());
    }
}

#[test]
fn an_explicit_host_still_overrides_the_platform() {
    use autoitv3_runtime::host::{HostContext, NativeHost};

    let prog = parse("Func F()\n    Return FileExists(\"/x\")\nEndFunc\n");
    let mut rt = runtime(&prog);
    let mut host = NativeHost::new();
    host.register("FileExists", |_c: &mut dyn HostContext, _a: Vec<Value>| {
        Ok(Value::str("from-host"))
    });
    rt.set_host(Box::new(host));
    assert_eq!(
        rt.call_function("F", vec![]).unwrap().to_autoit_string(),
        "from-host"
    );
}

/// A runtime with no platform at all must not reach platform functions.
#[test]
fn without_a_platform_only_language_builtins_work() {
    let prog = parse("Func F()\n    Return FileExists(\"/x\")\nEndFunc\n");
    let mut rt = Runtime::with_program(&prog);
    assert_eq!(rt.platform_name(), "none");
    let err = rt.call_function("F", vec![]).unwrap_err();
    assert!(err.message().contains("undefined function"), "got: {}", err.message());
}
// ---------------------------------------------------------------------------
// Macros — the platform supplies the environment-dependent ones
// ---------------------------------------------------------------------------

#[test]
fn environment_macros_come_from_the_platform() {
    // These used to evaluate to empty strings; they are real values now.
    assert_eq!(text("Return @AutoItPID > 0"), "True");
    assert_eq!(text("Return StringLen(@TempDir) > 0"), "True");
    // The emulation layer answers with the Windows layout; with it disabled the
    // common layer's host paths are used instead (`with_host_paths` / the
    // `--no-win-emu` switch).
    assert!(text("Return @TempDir").replace('/', "\\").ends_with(if cfg!(windows) { "Temp\\" } else { "Temp" }));
    assert_eq!(text("Return StringLen(@WorkingDir) > 0"), "True");
    assert_eq!(text("Return StringLen(@AutoItEXE) > 0"), "True");
}

#[test]
fn host_paths_are_available_when_the_emulation_leaves_them_alone() {
    let src = "Func F()\n    Return StringRight(@TempDir, 1)\nEndFunc\n";
    let prog = parse(src);
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(host_platform_with(
        WindowsEmulation::new().with_host_paths(),
    ));
    assert_eq!(
        rt.call_function("F", vec![]).unwrap().to_autoit_string(),
        std::path::MAIN_SEPARATOR.to_string()
    );
}

#[test]
fn os_identity_macros_come_from_the_stack() {
    // The bare Linux layer still answers honestly ...
    assert_eq!(
        LinuxPlatform::new()
            .macro_value("osversion")
            .map(|v| v.to_autoit_string()),
        Some("LINUX".to_string())
    );
    // ... and the default stack presents the emulated Windows 10 machine.
    // On a Windows host the native layer answers with the real OS instead.
    let os = text("Return @OSVersion");
    assert!(
        os == "WIN_10" || (cfg!(windows) && os == "WIN_11"),
        "got {os}"
    );
    assert_eq!(text("Return @OSArch"), "X64");
    assert_eq!(text("Return @OSType"), "WIN32_NT");
}

#[test]
fn interpreter_macros_are_answered_by_the_core() {
    // @ScriptLineNumber is the line of the statement being executed ...
    assert_eq!(text("Return @ScriptLineNumber"), "2");
    // ... @NumParams counts the arguments the caller passed ...
    assert_eq!(text("Return @NumParams"), "0");
    // ... and @CRLF is universal.
    assert_eq!(text("Return StringLen(@CRLF)"), "2");
}

#[test]
fn numparams_reflects_the_call() {
    let src = "Func F($a, $b = 1)\n    Return @NumParams\nEndFunc\n";
    let prog = parse(src);
    let mut rt = runtime(&prog);
    assert!(matches!(
        rt.call_function("F", vec![Value::Int(1)]).unwrap(),
        Value::Int(1)
    ));
    assert!(matches!(
        rt.call_function("F", vec![Value::Int(1), Value::Int(2)]).unwrap(),
        Value::Int(2)
    ));
}

#[test]
fn composite_forwards_macro_lookups_to_its_layers() {
    let p = host_platform_with(WindowsEmulation::new());
    // Emulation layer.
    assert!(p.macro_value("osversion").is_some());
    // Common layer.
    assert!(p.macro_value("tempdir").is_some());
    // Unknown macro stays unknown rather than becoming a made-up value.
    assert!(p.macro_value("nothinglikethis").is_none());
}

#[test]
fn without_a_platform_environment_macros_are_null() {
    let prog = parse("Func F()\n    Return @TempDir & \"x\"\nEndFunc\n");
    let mut rt = Runtime::with_program(&prog);
    // `Null` concatenates as an empty string — no fabricated path.
    assert_eq!(rt.call_function("F", vec![]).unwrap().to_autoit_string(), "x");
}

// ---------------------------------------------------------------------------
// File position / encoding / search (common additions)
// ---------------------------------------------------------------------------

#[test]
fn file_get_and_set_pos() {
    let dir = scratch("pos");
    let path = dir.join("a.txt");
    std::fs::write(&path, "abcdef").unwrap();
    let body = format!(
        r#"Local $h = FileOpen("{p}", 0)
    Local $p0 = FileGetPos($h)
    Local $r = FileRead($h, 3)
    Local $p1 = FileGetPos($h)
    FileSetPos($h, 0)
    Local $p2 = FileGetPos($h)
    FileClose($h)
    Return $p0 & ":" & $r & ":" & $p1 & ":" & $p2"#,
        p = path.display()
    );
    assert_eq!(text(&body), "0:abc:3:0");
}

#[test]
fn file_set_end_truncates_at_the_cursor() {
    let dir = scratch("setend");
    let path = dir.join("b.txt");
    let body = format!(
        r#"Local $h = FileOpen("{p}", 2)
    FileWrite($h, "abcdef")
    FileSetPos($h, 3)
    FileSetEnd($h)
    FileClose($h)
    Local $h2 = FileOpen("{p}", 0)
    Local $all = FileRead($h2)
    FileClose($h2)
    Return $all"#,
        p = path.display()
    );
    assert_eq!(text(&body), "abc");
}

#[test]
fn file_get_encoding_reports_boms_and_utf8() {
    let dir = scratch("enc");
    let bom = dir.join("bom.txt");
    let utf16 = dir.join("u16.txt");
    let utf8 = dir.join("u8.txt");
    let ascii = dir.join("ascii.txt");
    std::fs::write(&bom, [0xEF, 0xBB, 0xBF]).unwrap();
    std::fs::write(&utf16, [0xFF, 0xFE, b'h', 0]).unwrap();
    std::fs::write(&utf8, "héllo".as_bytes()).unwrap();
    std::fs::write(&ascii, b"hello").unwrap();
    let body = format!(
        r#"Return FileGetEncoding("{a}") & ":" & FileGetEncoding("{b}") & ":" & FileGetEncoding("{c}") & ":" & FileGetEncoding("{d}")"#,
        a = bom.display(),
        b = utf16.display(),
        c = utf8.display(),
        d = ascii.display()
    );
    assert_eq!(text(&body), "128:32:256:0");
}

#[test]
fn file_read_to_array_counts_lines() {
    let dir = scratch("toarray");
    let path = dir.join("c.txt");
    std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let body = format!(
        r#"Local $a = FileReadToArray("{p}")
    Return $a[0] & ":" & $a[1] & ":" & $a[2] & ":" & $a[3]"#,
        p = path.display()
    );
    assert_eq!(text(&body), "3:one:two:three");
}

#[test]
fn file_find_first_and_next_walk_matches() {
    let dir = scratch("find");
    std::fs::write(dir.join("a.txt"), "").unwrap();
    std::fs::write(dir.join("b.txt"), "").unwrap();
    std::fs::write(dir.join("c.log"), "").unwrap();
    let body = format!(
        r#"Local $h = FileFindFirstFile("{d}/*.txt")
    If $h = -1 Then Return "none"
    Local $names = ""
    While 1
        Local $f = FileFindNextFile($h)
        If @error Then ExitLoop
        $names = $names & $f & ","
    WEnd
    FileClose($h)
    Return $names"#,
        d = dir.display()
    );
    assert_eq!(text(&body), "a.txt,b.txt,");
}

// ---------------------------------------------------------------------------
// INI files
// ---------------------------------------------------------------------------

#[test]
fn ini_write_read_section_and_delete() {
    let dir = scratch("ini");
    let path = dir.join("a.ini");
    let body = format!(
        r#"IniWrite("{p}", "Sec", "k1", "v1")
    IniWrite("{p}", "Sec", "k2", "v2")
    IniWrite("{p}", "Other", "x", "y")
    Local $v = IniRead("{p}", "Sec", "k1", "def")
    Local $missing = IniRead("{p}", "Sec", "nope", "def")
    Local $s = IniReadSection("{p}", "Sec")
    Local $names = IniReadSectionNames("{p}")
    Local $del = IniDelete("{p}", "Sec", "k2")
    Local $again = IniDelete("{p}", "Sec", "k2")
    Return $v & "|" & $missing & "|" & $s[0] & "|" & $s[1] & "|" & $names[0] & "|" & $names[1] & "|" & $del & "|" & $again"#,
        p = path.display()
    );
    assert_eq!(text(&body), "v1|def|2|k1=v1|2|Sec|1|0");
}

#[test]
fn ini_write_section_and_rename() {
    let dir = scratch("inirename");
    let path = dir.join("b.ini");
    let body = format!(
        r#"IniWriteSection("{p}", "A", "k1=v1" & @LF & "k2=v2")
    IniRenameSection("{p}", "A", "B")
    Local $n = IniReadSectionNames("{p}")
    Return $n[0] & ":" & $n[1] & ":" & IniRead("{p}", "B", "k2")"#,
        p = path.display()
    );
    assert_eq!(text(&body), "1:B:v2");
}

#[test]
fn file_set_time_round_trips() {
    let dir = scratch("settime");
    let path = dir.join("t.txt");
    std::fs::write(&path, "x").unwrap();
    let body = format!(
        r#"Local $ok = FileSetTime("{p}", "2020/01/02 03:04:05")
    Return $ok & ":" & FileGetTime("{p}")"#,
        p = path.display()
    );
    assert_eq!(text(&body), "1:2020/01/02 03:04:05");
}

// ---------------------------------------------------------------------------
// Process execution / standard IO (host OS layer)
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn run_captures_stdout() {
    let body = r#"
Local $pid = Run("/bin/sh -c ""printf hello""", "", 0, 2)
If $pid = 0 Then Return "spawn-failed"
ProcessWaitClose($pid, 5)
Local $out = ""
While 1
    Local $chunk = StdoutRead($pid)
    If @error Then ExitLoop
    $out = $out & $chunk
WEnd
StdioClose($pid)
Return $out
"#;
    assert_eq!(text(body), "hello");
}

#[cfg(unix)]
#[test]
fn runwait_returns_the_exit_code() {
    assert_eq!(text(r#"Return RunWait("/bin/sh -c ""exit 3""")"#), "3");
}

#[cfg(unix)]
#[test]
fn process_waits_and_stats() {
    let body = r#"
Local $pid = Run("/bin/sh -c ""sleep 1""", "", 0, 0)
If $pid = 0 Then Return "spawn-failed"
Local $found = ProcessWait("sh", 5)
Local $s = ProcessGetStats($pid)
Local $ok = ($s[0] > 0)
Local $priority = ProcessSetPriority($pid, 0)
Local $closed = ProcessWaitClose($pid, 5)
Return ($found > 0) & ":" & $ok & ":" & $priority & ":" & $closed
"#;
    assert_eq!(text(body), "True:True:1:1");
}

#[test]
fn deterministic_profile_refuses_to_spawn() {
    let body = r#"Local $pid = Run("/bin/sh -c ""echo hi""")
    Return $pid & ":" & @error"#;
    let prog = parse(&format!("Func F()\n{body}\nEndFunc\n"));
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(host_platform_with(WindowsEmulation::new()));
    rt.set_profile(ExecutionProfile::deterministic());
    assert_eq!(
        rt.call_function("F", vec![]).unwrap().to_autoit_string(),
        "0:1"
    );
}

// ---------------------------------------------------------------------------
// Networking (host OS layer)
// ---------------------------------------------------------------------------

#[test]
fn deterministic_profile_refuses_network() {
    let body = r#"Local $s = TCPConnect("127.0.0.1", 1)
    Return $s & ":" & @error"#;
    let prog = parse(&format!("Func F()\n{body}\nEndFunc\n"));
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(host_platform_with(WindowsEmulation::new()));
    rt.set_profile(ExecutionProfile::deterministic());
    assert_eq!(
        rt.call_function("F", vec![]).unwrap().to_autoit_string(),
        "-1:1"
    );
}

#[test]
fn tcp_name_to_ip_resolves_localhost() {
    assert_eq!(text(r#"Return TCPNameToIP("localhost")"#), "127.0.0.1");
}

#[cfg(unix)]
#[test]
fn tcp_loopback_round_trip() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let body = format!(
        r#"
Local $listen = TCPListen("127.0.0.1", {port})
If $listen = -1 Then Return "no-listen"
Local $client = TCPConnect("127.0.0.1", {port})
If $client = -1 Then Return "no-connect"
Local $server = TCPAccept($listen)
If $server = -1 Then Return "no-accept"
TCPSend($client, "ping")
Local $got = ""
For $i = 1 To 20
    Sleep(50)
    $got = TCPRecv($server, 64)
    If $got <> "" Then ExitLoop
Next
TCPCloseSocket($client)
TCPCloseSocket($server)
TCPCloseSocket($listen)
Return $got
"#
    );
    assert_eq!(text(&body), "ping");
}

#[cfg(unix)]
#[test]
fn udp_loopback_round_trip() {
    let port = {
        let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        s.local_addr().unwrap().port()
    };
    let body = format!(
        r#"
Local $server = UDPOpen("", 0)
UDPBind($server, "127.0.0.1", {port})
Local $client = UDPOpen("127.0.0.1", {port})
UDPSend($client, "ping")
Local $got = ""
For $i = 1 To 20
    Sleep(50)
    $got = UDPRecv($server, 64)
    If $got <> "" Then ExitLoop
Next
UDPCloseSocket($client)
UDPCloseSocket($server)
Return $got
"#
    );
    assert_eq!(text(&body), "ping");
}

#[test]
fn inet_read_rejects_https_without_tls() {
    // `https://` needs TLS, which this layer intentionally does not provide;
    // the failure is immediate and touches no network.
    let body = r#"Local $d = InetRead("https://example.com")
    Return BinaryLen($d) & ":" & @error"#;
    assert_eq!(text(body), "0:1");
}

#[test]
fn proxy_and_user_agent_settings_are_accepted() {
    let body = r#"Return HttpSetUserAgent("test") & HttpSetProxy(2, "http://127.0.0.1:1") & FtpSetProxy(2, "http://127.0.0.1:1")"#;
    assert_eq!(text(body), "111");
}

/// Every name a layer answers to has to be a function AutoIt actually has.
///
/// A layer's list is what `Platform::provides` reports and what `IsFunc` ends
/// up consulting, so a name AutoIt's vocabulary does not contain is either a
/// spelling nothing will ever match or a function that does not exist. The
/// vocabulary lives in `autoitv3_runtime::vocab`; checking the layers against
/// it is the point of having one table instead of one per layer.
#[test]
fn every_layer_answers_only_autoit_function_names() {
    // One deviation, recorded rather than silently skipped: `RandomSeed` is
    // this project's modern spelling of `SRandom`, and the vocabulary is AutoIt
    // v3.3.x's, which predates it. Both are answered.
    const KNOWN_DEVIATIONS: &[&str] = &["RandomSeed"];

    let layers: [(&str, &[&str]); 6] = [
        ("common", autoitv3_platform::common::FUNCTIONS),
        ("common::proc", autoitv3_platform::common::proc::FUNCTIONS),
        ("common::net", autoitv3_platform::common::net::FUNCTIONS),
        ("linux", autoitv3_platform::linux::FUNCTIONS),
        ("winemu", autoitv3_platform::winemu::FUNCTIONS),
        ("winemu::gui", autoitv3_platform::winemu::gui::FUNCTIONS),
    ];
    for (layer, names) in layers {
        for name in names {
            if KNOWN_DEVIATIONS.contains(name) {
                continue;
            }
            assert!(
                autoitv3_runtime::vocab::canonical_function(name).is_some(),
                "{layer} answers {name:?}, which is not an AutoIt function"
            );
        }
    }
}
