//! End-to-end tests for `au3 run`'s argument order.
//!
//! `FILE` comes first and `FUNC` is optional: without it the whole script body
//! runs, with it one function is called.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A script that sets a global and defines a function to call.
const SCRIPT: &str = "\
Global $started = 0

Func Add($a, $b)
    Return $a + $b
EndFunc

$started = 1
";

/// Write `body` to a scratch file and return its path.
fn script(tag: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("au3-run-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let path = dir.join("script.au3");
    std::fs::write(&path, body).expect("write script");
    path
}

/// Run `au3 <args>`, returning stdout and stderr folded together.
fn au3(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_au3"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run au3");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn no_function_runs_the_whole_script_body() {
    let path = script("body", SCRIPT);
    let out = au3(&["run", path.to_str().unwrap()]);
    assert!(out.contains("script body ran to completion"), "got:\n{out}");
}

#[test]
fn a_trailing_function_is_called_with_its_args() {
    let path = script("call", SCRIPT);
    let out = au3(&["run", path.to_str().unwrap(), "Add", "--arg", "2", "--arg", "3"]);
    assert!(out.contains("Add() = 5"), "got:\n{out}");
}

#[test]
fn an_arg_without_a_function_becomes_the_script_cmdline() {
    let path = script(
        "cmdline",
        r#"
ConsoleWrite($CmdLine[0] & "|" & $CmdLine[1] & "|" & $CmdLine[2] & "|" & $CmdLineRaw & @CRLF)
"#,
    );
    let out = au3(&[
        "run",
        path.to_str().unwrap(),
        "--arg",
        "alpha",
        "--arg",
        "beta gamma",
    ]);
    assert!(
        out.contains("2|alpha|beta gamma|alpha \"beta gamma\""),
        "got:\n{out}"
    );
}

#[test]
fn a_function_call_leaves_the_script_cmdline_empty() {
    let path = script(
        "cmdline-func",
        r#"
Func Main()
    Return $CmdLine[0]
EndFunc
"#,
    );
    let out = au3(&["run", path.to_str().unwrap(), "Main"]);
    assert!(out.contains("Main() = 0"), "got:\n{out}");
}

#[test]
fn a_function_call_can_still_give_the_script_a_cmdline() {
    let path = script(
        "both",
        r#"
Func Main($x)
    Return $CmdLine[0] & "|" & $CmdLine[1] & "|" & $x
EndFunc
"#,
    );
    let out = au3(&[
        "run",
        path.to_str().unwrap(),
        "Main",
        "--cmdline",
        "s1",
        "--arg",
        "f1",
    ]);
    assert!(out.contains(r#"Main() = "1|s1|f1""#), "got:\n{out}");
}

#[test]
fn cmdline_and_arg_both_feed_the_script_when_no_function_is_named() {
    let path = script(
        "both-body",
        r#"
ConsoleWrite($CmdLine[0] & "|" & $CmdLine[1] & "|" & $CmdLine[2] & @CRLF)
"#,
    );
    let out = au3(&["run", path.to_str().unwrap(), "--cmdline", "a", "--arg", "b"]);
    assert!(out.contains("2|a|b"), "got:\n{out}");
}

#[test]
fn the_input_decides_compiled_and_the_flag_overrides_it() {
    let path = script("compiled", r#"
ConsoleWrite("[" & @Compiled & "]" & @CRLF)
"#);
    let source = au3(&["run", path.to_str().unwrap()]);
    assert!(source.contains("[0]"), "a .au3 answers 0: {source}");
    let forced = au3(&["run", path.to_str().unwrap(), "--compiled"]);
    assert!(forced.contains("[1]"), "--compiled was ignored: {forced}");
}

// ---------------------------------------------------------------------------
// GUI backend selection
// ---------------------------------------------------------------------------

/// `#AutoIt3Wrapper_Res_*` lines describe the version resource the build
/// carried: a run that *is* the build answers `FileGetVersion` for the script's
/// own file from them, an extracted `.au3` having no `RT_VERSION` of its own.
#[test]
fn the_builds_version_resource_answers_for_the_script_itself() {
    let body = "#AutoIt3Wrapper_Res_FileVersion=1.2.3.4\n\
                #AutoIt3Wrapper_Res_ProductName=Acme Tool\n\
                #AutoIt3Wrapper_Res_Field=CompanyName|Acme Inc\n\
                ConsoleWrite(FileGetVersion(@ScriptFullPath) & \"|\" & FileGetVersion(@ScriptFullPath, \"ProductName\") & \"|\" & FileGetVersion(@ScriptFullPath, \"CompanyName\") & @CRLF)\n";
    let path = script("wrapper-version", body);

    // A plain source run: the file has no version resource, and the wrapper
    // lines only ever took effect at build time.
    let out = au3(&["run", path.to_str().unwrap()]);
    assert!(out.contains("0.0.0.0|0.0.0.0|0.0.0.0"), "got:\n{out}");

    let out = au3(&["run", path.to_str().unwrap(), "--compiled"]);
    assert!(out.contains("1.2.3.4|Acme Tool|Acme Inc"), "got:\n{out}");
}

/// A script plus the payload its build script embedded: the
/// `#AutoIt3Wrapper_Res_File_Add` line names the file a `FindResourceW` for
/// `CFGDATA` should hand back, so the resource chain resolves without the
/// `.exe` the payload came out of.
const WRAPPER_RESOURCE_SCRIPT: &str = r#"
#AutoIt3Wrapper_Res_File_Add=payload.bin, RT_RCDATA, CFGDATA, 0
Local $h = DllCall("kernel32.dll", "handle", "FindResourceW", "handle", 0, "wstr", "CFGDATA", "wstr", 10)
Local $size = DllCall("kernel32.dll", "dword", "SizeofResource", "handle", 0, "handle", $h[0])
Local $res = DllCall("kernel32.dll", "handle", "LoadResource", "handle", 0, "handle", $h[0])
Local $ptr = DllCall("kernel32.dll", "ptr", "LockResource", "handle", $res[0])
Local $buf = DllStructCreate("byte[" & $size[0] & "]")
DllCall("kernel32.dll", "none", "RtlMoveMemory", "ptr", DllStructGetPtr($buf), "ptr", $ptr[0], "dword", $size[0])
ConsoleWrite($size[0] & "|" & BinaryToString(DllStructGetData($buf, 1)) & @CRLF)
"#;

#[test]
fn the_wrapper_resource_table_names_the_file_to_read() {
    let path = script("wrapper-resource", WRAPPER_RESOURCE_SCRIPT);
    let dir = path.parent().expect("script dir");
    std::fs::write(dir.join("payload.bin"), b"named payload").expect("write payload");

    let out = au3(&["run", path.to_str().unwrap(), "--wrapper-notes"]);
    assert!(out.contains("13|named payload"), "got:\n{out}");
    assert!(out.contains("Res_File_Add=payload.bin"), "the note names it: got:\n{out}");
}

#[test]
fn gui_calls_are_answered_with_and_without_a_backend() {
    // Without `--gui` the platform picks the backend; either way the GUI
    // functions answer, which is what this pins. The call stays windowless on
    // purpose: on Windows `auto` is the real Win32 backend, and a test must not
    // put a window on somebody's desktop.
    let body = "Func F()\n    Return GUIGetMsg() = 0\nEndFunc\n";
    let path = script("gui-headless", body);
    let out = au3(&["run", path.to_str().unwrap(), "F"]);
    assert!(out.contains("F() = true"), "got:\n{out}");

    let out = au3(&["run", path.to_str().unwrap(), "--gui", "headless", "F"]);
    assert!(out.contains("F() = true"), "got:\n{out}");

    let out = au3(&["run", path.to_str().unwrap(), "--gui", "auto", "F"]);
    assert!(out.contains("F() = true"), "got:\n{out}");
}

#[test]
fn gui_rejects_an_unknown_mode() {
    let path = script("gui-bad", SCRIPT);
    let out = au3(&["run", path.to_str().unwrap(), "--gui", "bogus"]);
    assert!(
        out.contains("invalid value") && out.contains("bogus"),
        "got:\n{out}"
    );
}

/// `#AutoIt3Wrapper_*` lines were consumed at build time, so nothing acts on
/// them — but the macros a script can check are answered from what they say,
/// and the settings themselves are the build's fingerprint.
#[test]
fn build_directives_answer_the_macros_and_show_up_in_the_notes() {
    let body = "#AutoIt3Wrapper_UseX64=N\n\
                #AutoIt3Wrapper_UseUpx=Y\n\
                #AutoIt3Wrapper_Res_FileVersion=1.2.3.4\n\
                ConsoleWrite(\"x64=\" & @AutoItX64 & \" unicode=\" & @Unicode & @CRLF)\n";
    let path = script("wrapper-facts", body);
    let out = au3(&["run", path.to_str().unwrap()]);
    assert!(out.contains("x64=0 unicode=1"), "the build says x86: got:\n{out}");
    // The fingerprint is diagnostic output: off unless asked for.
    assert!(!out.contains("AutoIt3Wrapper settings"), "off by default: got:\n{out}");

    let out = au3(&["run", path.to_str().unwrap(), "--wrapper-notes"]);
    assert!(out.contains("AutoIt3Wrapper settings (3)"), "got:\n{out}");
    assert!(out.contains("UseUpx=Y"), "got:\n{out}");

    // Without a directive the emulated machine answers `@AutoItX64`, which is
    // what `--win-arch` selects.
    let plain = script("wrapper-default", "ConsoleWrite(\"x64=\" & @AutoItX64 & @CRLF)\n");
    let out = au3(&["run", plain.to_str().unwrap()]);
    assert!(out.contains("x64=1"), "the default machine is x64: got:\n{out}");
    let out = au3(&["run", plain.to_str().unwrap(), "--win-arch", "x86"]);
    assert!(out.contains("x64=0"), "got:\n{out}");
}

/// `--gui egui` needs the eframe-backed build; without the feature the CLI has
/// to say how to get one rather than failing obscurely.
#[cfg(not(feature = "gui-egui"))]
#[test]
fn gui_egui_without_the_feature_says_how_to_build_it() {
    let path = script("gui-egui-off", SCRIPT);
    let out = au3(&["run", path.to_str().unwrap(), "--gui", "egui"]);
    assert!(out.contains("gui-egui"), "got:\n{out}");
    assert!(out.contains("--features gui-egui"), "got:\n{out}");
}

/// `--gui native` names the real Win32 backend, which only a Windows host has;
/// elsewhere the CLI has to say so rather than failing obscurely.
#[cfg(not(windows))]
#[test]
fn gui_native_needs_a_windows_host() {
    let path = script("gui-native-off", SCRIPT);
    let out = au3(&["run", path.to_str().unwrap(), "--gui", "native"]);
    assert!(out.contains("--gui native"), "got:\n{out}");
    assert!(out.contains("Windows host"), "got:\n{out}");
}

/// `FileWrite` takes "the text or binary data to write": a `Binary` value has to
/// land as its **bytes**, a string as its text. Writing the hex rendering
/// instead puts the ASCII `0x4D5A…` on disk where a loader expects `MZ`, which
/// is what a script that extracts an embedded file does.
#[test]
fn file_write_writes_binary_values_as_bytes() {
    let dir = std::env::temp_dir();
    let names = ["au3-bin-raw.bin", "au3-bin-line.bin", "au3-bin-text.bin"];
    for name in names {
        let _ = std::fs::remove_file(dir.join(name));
    }
    let path = script(
        "binary-write",
        "FileWrite(@TempDir & \"\\au3-bin-raw.bin\", Binary(\"0x4D5A9000\"))\n\
         FileWriteLine(@TempDir & \"\\au3-bin-line.bin\", Binary(\"0x4D5A\"))\n\
         FileWrite(@TempDir & \"\\au3-bin-text.bin\", \"0x4D5A9000\")\n\
         ConsoleWrite(\"done\" & @CRLF)\n",
    );
    let out = au3(&["run", &path.to_string_lossy()]);
    assert!(out.contains("done"), "the script ran:\n{out}");

    assert_eq!(
        std::fs::read(dir.join("au3-bin-raw.bin")).expect("raw written"),
        b"MZ\x90\x00",
        "the bytes, not the hex text"
    );
    // `FileWriteLine` appends its linefeed to binary data too, after the last
    // byte (which is not a CR/LF here).
    assert_eq!(
        std::fs::read(dir.join("au3-bin-line.bin")).expect("line written"),
        b"MZ\r\n"
    );
    // A string is still a string: `0x4D5A9000` is ten characters.
    assert_eq!(
        std::fs::read(dir.join("au3-bin-text.bin")).expect("text written"),
        b"0x4D5A9000"
    );
    for name in names {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

/// `IsPtr` answers from where a value came from, not from what it looks like.
#[test]
fn a_pointer_is_told_apart_from_a_number() {
    let body = "Local $s = DllStructCreate(\"byte[4]\")\n\
                Local $p = IsPtr(DllStructGetPtr($s)) ? 1 : 0\n\
                Local $n = IsPtr(1234) ? 1 : 0\n\
                Local $t = IsPtr(\"1234\") ? 1 : 0\n\
                ConsoleWrite($p & \"|\" & $n & \"|\" & $t & @CRLF)\n";
    let path = script("pointer", body);
    let out = au3(&["run", path.to_str().unwrap()]);
    assert!(out.contains("1|0|0"), "got:\n{out}");
}



/// A `Ptr` keeps its type: hex in strings, `Ptr` in `VarGetType`, and it is not
/// an integer (`IsInt` answers 0) - all measured on the official interpreter.
#[test]
fn a_pointer_prints_as_hex_and_reports_its_type() {
    let body = "Local $s = DllStructCreate(\"byte[4]\")\n\
                Local $p = DllStructGetPtr($s)\n\
                ConsoleWrite(String($p) & \"|\" & VarGetType($p) & \"|\" & IsInt($p) & @CRLF)\n";
    let path = script("pointer-type", body);
    let out = au3(&["run", path.to_str().unwrap()]);
    assert!(out.contains("0x") && out.contains("|Ptr|0"), "got:\n{out}");
}
