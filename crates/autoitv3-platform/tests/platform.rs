//! Tests for the platform stack: layering, selection, and the portable
//! function set.

use autoitv3_platform::{
    host_platform, runtime_with_platform, CompositePlatform, LinuxPlatform, PortablePlatform,
};
use autoitv3_runtime::platform::Platform;
use autoitv3_runtime::{Runtime, Value};

fn parse(src: &str) -> autoitv3_ast::Program {
    autoitv3_ast::parse(src).expect("parses")
}

/// Run `Func F()` from `body` on a runtime with the full platform stack.
fn call(body: &str) -> Value {
    let src = format!("Func F()\n{body}\nEndFunc\n");
    let prog = parse(&src);
    let mut rt = runtime_with_platform(&prog);
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
fn host_platform_stacks_portable_under_the_system_layer() {
    let p = host_platform();
    let expected = if cfg!(windows) {
        "portable+windows"
    } else {
        "portable+linux"
    };
    assert_eq!(p.name(), expected);
}

#[test]
fn portable_layer_is_present_on_every_platform() {
    let p = PortablePlatform::new();
    assert_eq!(p.name(), "portable");
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
    assert!(!p.provides("FileOpen"), "file I/O belongs to the portable layer");
}

#[test]
fn composite_tries_layers_in_order() {
    let mut composite = CompositePlatform::new(
        "test",
        vec![
            Box::new(PortablePlatform::new()),
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
    let mut rt = runtime_with_platform(&prog);
    assert!(rt.platform_name().starts_with("portable+"));
    assert!(matches!(rt.call_function("F", vec![]).unwrap(), Value::Int(1)));
}

// ---------------------------------------------------------------------------
// Portable: files
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
// Portable: environment, math, timers, console
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
fn nothing_windows_only_is_silently_answered() {
    // Registry/COM/DllCall are Windows concerns; off Windows the interpreter
    // must report an undefined function rather than invent a value.
    let body = r#"Return RegRead("HKEY_LOCAL_MACHINE\SOFTWARE\X", "Y")"#;
    let prog = parse(&format!("Func F()\n{body}\nEndFunc\n"));
    let mut rt = runtime_with_platform(&prog);
    let err = rt.call_function("F", vec![]).unwrap_err();
    assert!(err.message().contains("undefined function"), "got: {}", err.message());
}

#[test]
fn an_explicit_host_still_overrides_the_platform() {
    use autoitv3_runtime::host::{HostContext, NativeHost};

    let prog = parse("Func F()\n    Return FileExists(\"/x\")\nEndFunc\n");
    let mut rt = runtime_with_platform(&prog);
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
    assert_eq!(text("Return StringRight(@TempDir, 1)"), std::path::MAIN_SEPARATOR.to_string());
    assert_eq!(text("Return StringLen(@WorkingDir) > 0"), "True");
    assert_eq!(text("Return StringLen(@AutoItEXE) > 0"), "True");
}

#[test]
fn os_identity_macros_come_from_the_system_layer() {
    let expected = if cfg!(windows) { "" } else { "LINUX" };
    if !expected.is_empty() {
        assert_eq!(text("Return @OSVersion"), expected);
        assert_eq!(text("Return @OSArch"), "X64");
    }
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
    let mut rt = runtime_with_platform(&prog);
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
    let p = host_platform();
    // Portable layer.
    assert!(p.macro_value("tempdir").is_some());
    // System layer.
    assert!(p.macro_value("osversion").is_some());
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
