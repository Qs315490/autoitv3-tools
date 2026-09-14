//! The native Windows platform layer, exercised against the real OS.
//!
//! These tests only run on Windows targets (`#![cfg(windows)]`); on every
//! other host the suite is empty and the emulation suite (`winemu.rs`) covers
//! the same semantics against the simulated machine.

#![cfg(windows)]

use autoitv3_ast::parse;
use autoitv3_platform::{
    host_platform, host_platform_with, host_platform_with_options, PlatformOptions,
    WindowsEmulation,
};
use autoitv3_runtime::{ExecutionProfile, Runtime, Value};

/// Run `F()` on the default stack (native + common + emulation fallback).
/// A bare statement list is wrapped in `Func F()` for convenience.
fn call(src: &str) -> Value {
    let prog = parse(&wrap(src)).expect("parses");
    let mut rt = autoitv3_platform::runtime_with_platform(&prog);
    rt.call_function("F", vec![]).expect("runs")
}

/// Run `F()` with the emulation fallback removed — native + common only.
fn call_native_only(src: &str) -> Value {
    let prog = parse(&wrap(src)).expect("parses");
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(host_platform_with(WindowsEmulation::new().disabled()));
    rt.call_function("F", vec![]).expect("runs")
}

fn call_with_profile(src: &str, profile: ExecutionProfile) -> Value {
    let prog = parse(&wrap(src)).expect("parses");
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(host_platform());
    rt.set_profile(profile);
    rt.call_function("F", vec![]).expect("runs")
}

fn int(src: &str) -> i64 {
    call(src).to_int()
}

fn text(src: &str) -> String {
    call(src).to_autoit_string()
}

/// Wrap a bare statement list in `Func F() … EndFunc` when needed.
fn wrap(src: &str) -> String {
    if src.contains("Func F()") {
        src.to_string()
    } else {
        format!("Func F()
{src}
EndFunc
")
    }
}

// ---------------------------------------------------------------------------
// stack shape
// ---------------------------------------------------------------------------

#[test]
fn the_default_stack_answers_windows_only_names_without_the_emulation() {
    let p = host_platform_with(WindowsEmulation::new().disabled());
    assert_eq!(p.name(), "windows+common");
    for name in [
        "DllCall",
        "DllCallAddress",
        "DllStructCreate",
        "ClipGet",
        "ProcessList",
        "DriveGetType",
        "RegRead",
        "RegWrite",
        "RegDelete",
        "RegEnumKey",
        "RegEnumVal",
        "MemGetStats",
        "IsAdmin",
        "ShellExecute",
        "RunAs",
        "DriveMapGet",
        "ObjCreate",
        "IsObj",
        "ObjName",
    ] {
        assert!(p.provides(name), "{name} not provided natively");
    }
}

// ---------------------------------------------------------------------------
// DllStruct: real memory, real pointers
// ---------------------------------------------------------------------------

#[test]
fn dllstruct_layouts_and_round_trips_data() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $t = DllStructCreate("struct;dword MajorVersion;dword MinorVersion;wchar Tag[10];endstruct")
    Return DllStructGetSize($t)
EndFunc
"#
        ),
        28 // 4 + 4 + 10*2, no padding needed
    );
    assert_eq!(
        int(
            r#"
Func F()
    Local $t = DllStructCreate("dword a;int b;float c;double d")
    DllStructSetData($t, "a", 0x11223344)
    DllStructSetData($t, "c", 1.5)
    DllStructSetData($t, "d", -2.25)
    Return DllStructGetData($t, "a") + (DllStructGetData($t, "c") = 1.5) * 1000 _
        + (DllStructGetData($t, "d") = -2.25) * 10000
EndFunc
"#
        ),
        0x11223344 + 1000 + 10000
    );
}

#[test]
fn dllstructcreate_over_a_getptr_aliases_the_same_memory() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $a = DllStructCreate("dword x")
    DllStructSetData($a, "x", 7)
    Local $b = DllStructCreate("dword y", DllStructGetPtr($a))
    DllStructSetData($b, "y", 9)
    Return DllStructGetData($a, "x")
EndFunc
"#
        ),
        9
    );
}

#[test]
fn isdllstruct_and_error_paths_behave() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $t = DllStructCreate("byte b")
    Local $bad = DllStructCreate("nosuchtype x")
    Return (IsDllStruct($t) And (Not IsDllStruct($bad))) + 0
EndFunc
"#
        ),
        1
    );
}

// ---------------------------------------------------------------------------
// DllCall: real kernel32/user32 targets
// ---------------------------------------------------------------------------

#[test]
fn dllcall_takes_no_arguments_and_returns_the_word() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $r = DllCall("kernel32.dll", "dword", "GetCurrentProcessId")
    Return (($r[0] > 0) And (UBound($r) = 1)) + 0
EndFunc
"#
        ),
        1
    );
}

#[test]
fn a_null_module_handle_names_the_configured_resource_image() {
    // `GetModuleHandleW(NULL)` normally names the host `au3`/test image, whose
    // resources are not the analysed script's. With a resource module named,
    // the handle must expose *that* image's resources instead.
    let system_root = std::env::var_os("SystemRoot").expect("SystemRoot");
    let mut kernel32 = std::path::PathBuf::from(system_root);
    kernel32.push("System32");
    kernel32.push("kernel32.dll");

    let src = wrap(
        r#"
Func F()
    Local $r = DllCall("kernel32.dll", "handle", "GetModuleHandleW", "ptr", 0)
    If $r[0] = 0 Then Return 0
    ; RT_VERSION (16), name 1 — every system image carries it.
    Local $v = DllCall("kernel32.dll", "handle", "FindResourceW", "handle", $r[0], "int", 1, "int", 16)
    Return (($v[0] <> 0)) + 0
EndFunc
"#,
    );
    let prog = parse(&src).expect("parses");

    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(host_platform_with(
        WindowsEmulation::new().with_module_file(kernel32),
    ));
    assert_eq!(rt.call_function("F", vec![]).expect("runs").to_int(), 1);
}

#[test]
fn dllcall_with_a_struct_pointer_sees_native_writes() {
    // GetVersionExW fills the struct for real; without a manifest it reports
    // the 6.2 floor, so assert on the size field round-tripping instead.
    assert_eq!(
        int(
            r#"
Func F()
    Local $t = DllStructCreate("struct;dword OSVersionInfoSize;dword MajorVersion;dword MinorVersion;dword BuildNumber;dword PlatformId;wchar CSDVersion[128];endstruct")
    DllStructSetData($t, "OSVersionInfoSize", DllStructGetSize($t))
    Local $r = DllCall("kernel32.dll", "int", "GetVersionExW", "ptr", $t)
    Return (($r[0] = 1) And (DllStructGetData($t, "OSVersionInfoSize") = 276)) + 0
EndFunc
"#
        ),
        1
    );
}

#[test]
fn dllcall_copies_through_real_memory_with_rtlmovememory() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $src = DllStructCreate("byte buf[8]")
    Local $dst = DllStructCreate("byte out[8]")
    DllStructSetData($src, 1, Binary("0x0102030405060708"))
    DllCall("kernel32.dll", "none", "RtlMoveMemory", "ptr", $dst, "ptr", $src, "ulong_ptr", 8)
    Return (DllStructGetData($dst, 1) = Binary("0x0102030405060708")) + 0
EndFunc
"#
        ),
        1
    );
}

#[test]
fn dllcall_by_reference_and_string_arguments() {
    // lstrlenW on a wstr argument.
    assert_eq!(
        int(
            r#"
Func F()
    Local $r = DllCall("kernel32.dll", "int", "lstrlenW", "wstr", "abcdef")
    Return $r[0]
EndFunc
"#
        ),
        6
    );
    // `uint64*` writeback: the returned array echoes the callee's value —
    // GetSystemTimeAsFileTime writes the current 64-bit file time through the
    // pointer.
    assert_eq!(
        int(
            r#"
Func F()
    Local $v = 0
    Local $r = DllCall("kernel32.dll", "none", "GetSystemTimeAsFileTime", "uint64*", $v)
    Return ($r[1] > 100000000000000000) + 0
EndFunc
"#
        ),
        1
    );
}

#[test]
fn dllcall_reports_unresolvable_targets_with_error() {
    // A missing export is @error 3; a missing DLL is @error 1.
    assert_eq!(
        int(
            r#"
Func F()
    Local $r = DllCall("kernel32.dll", "int", "NoSuchExportInTheDll")
    Return @error
EndFunc
"#
        ),
        3
    );
    assert_eq!(
        int(
            r#"
Func F()
    Local $r = DllCall("no-such-dll-xyz.dll", "int", "Whatever")
    Return @error
EndFunc
"#
        ),
        1
    );
}

#[test]
fn dllopen_and_dllclose_manage_real_modules() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $h = DllOpen("kernel32.dll")
    Local $ok = ($h > 0)
    Local $r = DllCall($h, "dword", "GetCurrentProcessId")
    $ok = $ok And ($r[0] > 0)
    DllClose($h)
    Return $ok + 0
EndFunc
"#
        ),
        1
    );
}

// ---------------------------------------------------------------------------
// clipboard
// ---------------------------------------------------------------------------

#[test]
fn clipput_then_clipget_round_trips_text() {
    assert_eq!(
        text(
            r#"
Func F()
    ClipPut("autoitv3-native-клип")
    Return ClipGet()
EndFunc
"#
        ),
        "autoitv3-native-клип"
    );
}

#[test]
fn clipput_refuses_under_the_deterministic_profile() {
    assert_eq!(
        call_with_profile(
            r#"
Func F()
    Return ClipPut("nope")
EndFunc
"#,
            ExecutionProfile::deterministic()
        )
        .to_int(),
        0
    );
}

// ---------------------------------------------------------------------------
// process
// ---------------------------------------------------------------------------

#[test]
fn processlist_includes_this_process() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $list = ProcessList()
    Local $me = @AutoItPID
    For $i = 1 To $list[0][0]
        If $list[$i][1] = $me Then Return 1
    Next
    Return 0
EndFunc
"#
        ),
        1
    );
}

#[test]
fn processexists_resolves_pid_and_name() {
    assert_eq!(
        int(
            r#"
Func F()
    Return (ProcessExists(@AutoItPID) > 0) + 0
EndFunc
"#
        ),
        1
    );
    // Our own executable name resolves by name too.
    let exe = std::env::current_exe()
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    assert!(!exe.is_empty());
    assert_eq!(
        int(&format!(
            r#"
Func F()
    Return ProcessExists("{exe}")
EndFunc
"#
        )),
        std::process::id() as i64
    );
}

// ---------------------------------------------------------------------------
// drives
// ---------------------------------------------------------------------------

#[test]
fn drivegetdrive_lists_at_least_the_system_drive() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $drives = DriveGetDrive("ALL")
    Return (($drives[0] >= 1) And (StringLen($drives[1]) = 3)) + 0
EndFunc
"#
        ),
        1
    );
}

#[test]
fn driveget_type_and_space_answer_for_the_system_drive() {
    let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
    assert_eq!(
        text(&format!(
            r#"
Func F()
    Return DriveGetType("{sys}\\")
EndFunc
"#
        )),
        "FIXED"
    );
    assert_eq!(
        int(&format!(
            r#"
Func F()
    Return DriveGetSpaceTotal("{sys}\\") > 0
EndFunc
"#
        )),
        1
    );
    assert_eq!(
        int(&format!(
            r#"
Func F()
    Return DriveGetSpaceFree("{sys}\\") > 0
EndFunc
"#
        )),
        1
    );
    assert_eq!(
        text(&format!(
            r#"
Func F()
    Return DriveGetStatus("{sys}\\")
EndFunc
"#
        )),
        "READY"
    );
}

#[test]
fn driveget_reports_invalid_drives_with_error() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $r = DriveGetType("Q:\")
    Return @error
EndFunc
"#
        ),
        1
    );
}

// ---------------------------------------------------------------------------
// system macros
// ---------------------------------------------------------------------------

#[test]
fn windows_identity_macros_answer_from_the_real_os() {
    assert!(text("Return @WindowsDir").to_lowercase().contains("windows"));
    assert!(text("Return @SystemDir").to_lowercase().contains("system32"));
    assert!(text("Return @AppDataDir").contains("\\"));
    assert!(!text("Return @ComputerName").is_empty());
    let os = text("Return @OSVersion");
    assert!(os == "WIN_10" || os == "WIN_11", "got {os}");
    assert_eq!(text("Return @OSArch"), "X64");
    assert!(int("Return @OSBuild > 0") == 1);
}

// ---------------------------------------------------------------------------
// emulation fallback
// ---------------------------------------------------------------------------

#[test]
fn registry_answers_natively_with_and_without_the_fallback() {
    // Reg* is native, so the emulation fallback is irrelevant to it: a
    // missing value reports `@error = 1` on either stack.
    let src = r#"
Func F()
    Local $r = RegRead("HKEY_CURRENT_USER\Software\au3-native-missing", "V")
    Return @error
EndFunc
"#;
    for build in [
        host_platform(),
        host_platform_with(WindowsEmulation::new().disabled()),
    ] {
        let prog = parse(src).expect("parses");
        let mut rt = Runtime::with_program(&prog);
        rt.set_platform(build);
        let v = rt.call_function("F", vec![]).expect("runs");
        assert_eq!(v.to_int(), 1);
    }
}

// ---------------------------------------------------------------------------
// registry
// ---------------------------------------------------------------------------

#[test]
fn regwrite_regread_round_trip_all_scalars() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $k = "HKEY_CURRENT_USER\Software\au3-native-sz"
    RegWrite($k, "Sz", "REG_SZ", "hello world")
    RegWrite($k, "Dw", "REG_DWORD", 123456)
    RegWrite($k, "Qw", "REG_QWORD", 1234567890123)
    RegWrite($k, "Bin", "REG_BINARY", Binary("0x0102AA"))
    Local $a = RegRead($k, "Sz")
    Local $b = RegRead($k, "Dw")
    Local $c = RegRead($k, "Qw")
    Local $d = RegRead($k, "Bin")
    RegDelete($k)
    Return (RegRead($k, "Sz") = "") And ($a = "hello world") And ($b = 123456) _
        And ($c = 1234567890123) And ($d = Binary("0x0102AA"))
EndFunc
"#
        ),
        1
    );
}

#[test]
fn regwrite_creates_keys_and_regenum_lists_them() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $k = "HKEY_CURRENT_USER\Software\au3-native-enum"
    RegWrite($k & "\Sub", "V", "REG_SZ", "x")
    Local $seen = 0
    For $i = 1 To 10
        Local $n = RegEnumKey($k, $i)
        If @error <> 0 Then ExitLoop
        If $n = "Sub" Then $seen = 1
    Next
    Local $v = RegEnumVal($k, 1)
    RegDelete($k)
    Return $seen & (RegEnumKey($k, 1) = "")
EndFunc
"#
        ),
        1
    );
}

#[test]
fn regread_of_a_missing_value_sets_error() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $r = RegRead("HKEY_CURRENT_USER\Software\au3-native-missing", "V")
    Return @error
EndFunc
"#
        ),
        1
    );
}

#[test]
fn regread_reports_the_type_code_in_extended() {
    assert_eq!(
        text(
            r#"
Func F()
    Local $k = "HKEY_CURRENT_USER\Software\au3-native-ext"
    RegWrite($k, "Sz", "REG_SZ", "t")
    RegWrite($k, "Dw", "REG_DWORD", 5)
    RegWrite($k, "Qw", "REG_QWORD", 5)
    RegWrite($k, "Bin", "REG_BINARY", Binary("0x01"))
    RegWrite($k, "Mul", "REG_MULTI_SZ", "a" & @LF & "b")
    RegRead($k, "Sz")
    Local $sz = @extended
    RegRead($k, "Dw")
    Local $dw = @extended
    RegRead($k, "Qw")
    Local $qw = @extended
    RegRead($k, "Bin")
    Local $bin = @extended
    RegRead($k, "Mul")
    Local $mul = @extended
    RegDelete($k)
    Return $sz & "/" & $dw & "/" & $qw & "/" & $bin & "/" & $mul
EndFunc
"#
        ),
        "1/4/11/3/7"
    );
}

#[test]
fn regread_error_ladder_matches_autoit() {
    // An invalid main key is 2; a missing value inside a real key is -1.
    assert_eq!(
        int(
            r#"
Func F()
    RegRead("NOSUCHHIVE\Whatever", "V")
    Return @error
EndFunc
"#
        ),
        2
    );
    assert_eq!(
        int(
            r#"
Func F()
    Local $k = "HKEY_CURRENT_USER\Software\au3-native-ladder"
    RegWrite($k & "\Sub", "V", "REG_SZ", "t")
    RegRead($k & "\Sub", "Missing")
    Local $e = @error
    RegDelete($k)
    Return $e
EndFunc
"#
        ),
        -1
    );
}

#[test]
fn regmulti_sz_round_trips_through_an_lf_joined_string() {
    assert_eq!(
        text(
            r#"
Func F()
    Local $k = "HKEY_CURRENT_USER\Software\au3-native-multi"
    RegWrite($k, "Mul", "REG_MULTI_SZ", "one" & @LF & "two" & @LF & "three")
    Local $v = RegRead($k, "Mul")
    RegDelete($k)
    Return $v
EndFunc
"#
        ),
        "one\ntwo\nthree"
    );
}

#[test]
fn regdelete_with_a_value_argument_spares_the_key() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $k = "HKEY_CURRENT_USER\Software\au3-native-delval"
    RegWrite($k, "", "REG_SZ", "def")
    RegWrite($k, "V", "REG_SZ", "x")
    RegWrite($k, "Keep", "REG_SZ", "y")
    RegDelete($k, "")
    Local $def_gone = (RegRead($k, "") = "") And (@error <> 0)
    RegDelete($k, "V")
    Local $v_gone = (RegRead($k, "V") = "") And (@error <> 0)
    ; The key itself survived both value deletions.
    Local $key_alive = (RegRead($k, "Keep") = "y") And (@error = 0)
    RegDelete($k)
    Return ($def_gone And $v_gone And $key_alive) + 0
EndFunc
"#
        ),
        1
    );
}

// ---------------------------------------------------------------------------
// DllCallAddress
// ---------------------------------------------------------------------------

#[test]
fn dllcalladdress_invokes_a_resolved_address() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $h = DllOpen("kernel32.dll")
    Local $a = DllCall("kernel32.dll", "ptr", "GetProcAddress", "ptr", $h, "str", "GetCurrentProcessId")
    Local $r = DllCallAddress("dword", $a[0])
    Return $r[0] > 0
EndFunc
"#
        ),
        1
    );
}

// ---------------------------------------------------------------------------
// system / shell
// ---------------------------------------------------------------------------

#[test]
fn memgetstats_reports_the_seven_element_array() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $m = MemGetStats()
    Return (UBound($m) = 7) And ($m[0] >= 0) And ($m[0] <= 100) And ($m[1] > 0)
EndFunc
"#
        ),
        1
    );
}

#[test]
fn isadmin_answers_without_error() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $a = IsAdmin()
    Return (@error = 0) And ($a = 0 Or $a = 1)
EndFunc
"#
        ),
        1
    );
}

#[test]
fn drivemapget_of_an_unmapped_drive_sets_error() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $r = DriveMapGet("Q:")
    Return @error
EndFunc
"#
        ),
        1
    );
}

// ---------------------------------------------------------------------------
// COM
// ---------------------------------------------------------------------------

fn objcreate_and_late_bound_calls_hit_a_real_automation_server() {
    let src = r#"
Func F()
    Local $o = ObjCreate("Scripting.FileSystemObject")
    If Not IsObj($o) Then Return "no-obj"
    Local $exists = $o.DriveExists("C:\\")
    Local $drive = $o.GetDrive("C:\\")
    Local $ready = $drive.IsReady
    Local $folder = $o.GetSpecialFolder(0)
    Local $win = $folder.Path
    Return (VarGetType($o) = "Object") & "/" & $exists & "/" & $ready & "/" & $win & "/" & ObjName($o)
EndFunc
"#;
    let v = call(src);
    let diag = v.to_autoit_string();
    assert_eq!(v.to_int(), 1, "diag: {diag}");
}

// ---------------------------------------------------------------------------
// real Windows filesystem semantics overriding the common approximations
// ---------------------------------------------------------------------------

#[test]
fn filegetattrib_and_filesetattrib_use_real_windows_attributes() {
    let dir = std::env::temp_dir().join(format!("au3-native-attrib-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("attrib.txt");
    std::fs::write(&path, b"x").unwrap();
    let p = path.to_string_lossy().replace('\\', "\\\\");
    let got = text(&format!(
        r#"
Func F()
    Local $before = FileGetAttrib("{p}")
    FileSetAttrib("{p}", "+SH")
    Local $hidden = FileGetAttrib("{p}")
    FileSetAttrib("{p}", "-SH")
    Local $cleared = FileGetAttrib("{p}")
    FileDelete("{p}")
    DirRemove("{dir}")
    Return $before & "|" & $hidden & "|" & $cleared
EndFunc
"#,
        p = p,
        dir = dir.to_string_lossy().replace('\\', "\\\\")
    ));
    // A fresh file is "normal" (A is set by default on new files), +S+H adds
    // both letters, -S-H leaves none of the settable bits.
    assert!(
        got.starts_with('|') || got.starts_with("A|") || got.starts_with("R|") || got.starts_with("N|"),
        "before: {got}"
    );
    let parts: Vec<&str> = got.split('|').collect();
    assert!(parts[1].contains('S') && parts[1].contains('H'), "hidden: {got}");
    assert!(!parts[2].contains('S') && !parts[2].contains('H'), "cleared: {got}");
}

#[test]
fn filegetshortname_answers_from_the_real_filesystem() {
    let got = text(
        r#"
Func F()
    Return FileGetShortName(@WindowsDir & "explorer.exe")
EndFunc
"#,
    );
    // Where 8.3 names are disabled the API returns the long path itself; the
    // one thing it must never do is come back empty.
    assert!(!got.is_empty(), "short name empty");
    assert!(got.to_lowercase().contains("explorer.exe"), "got {got}");
}

#[test]
fn envupdate_reports_whether_the_broadcast_was_delivered() {
    assert_eq!(int("Return EnvUpdate()"), 1);
}

#[test]
fn objcreate_failure_sets_error() {
    assert_eq!(
        int(
            r#"
Func F()
    Local $o = ObjCreate("NoSuch.ProgID.Anywhere")
    Return @error
EndFunc
"#
        ),
        1
    );
}

// ---------------------------------------------------------------------------
// fine-grained behaviour control (per-effect overrides, forced emulation)
// ---------------------------------------------------------------------------

#[test]
fn an_effect_override_lets_a_deterministic_run_write_the_registry() {
    let src = r#"
Func F()
    Local $k = "HKEY_CURRENT_USER\Software\au3-native-override"
    RegWrite($k, "V", "REG_SZ", "probed")
    Local $v = RegRead($k, "V")
    RegDelete($k)
    Return ($v = "probed") + 0
EndFunc
"#;
    let prog = parse(src).expect("parses");
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(host_platform_with(WindowsEmulation::new().disabled()));
    // Deterministic everywhere except the registry the run probes.
    rt.set_profile(
        autoitv3_runtime::ExecutionProfile::deterministic()
            .with_effect(autoitv3_runtime::profile::EffectKind::RegistryWrite, true),
    );
    let v = rt.call_function("F", vec![]).expect("runs");
    assert_eq!(v.to_int(), 1);
    // The same run still refuses everything else: FileDelete stays gated.
    assert_eq!(
        call_with_profile(
            r#"
Func F()
    Local $r = FileDelete("C:\Windows\nonexistent-au3-probe.txt")
    Return @error
EndFunc
"#,
            autoitv3_runtime::ExecutionProfile::deterministic()
                .with_effect(autoitv3_runtime::profile::EffectKind::RegistryWrite, true),
        )
        .to_int(),
        1
    );
}

#[test]
fn a_denied_shutdown_refuses_even_in_a_faithful_run() {
    let src = r#"
Func F()
    Local $r = Shutdown(1)
    Return $r & ":" & @error
EndFunc
"#;
    let prog = parse(src).expect("parses");
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(host_platform_with(WindowsEmulation::new().disabled()));
    rt.set_profile(
        autoitv3_runtime::ExecutionProfile::faithful()
            .with_effect(autoitv3_runtime::profile::EffectKind::Shutdown, false),
    );
    // The gate refuses before ExitWindowsEx is ever reached.
    assert_eq!(rt.call_function("F", vec![]).expect("runs").to_autoit_string(), "0:1");
}

#[test]
fn forced_emulation_routes_registry_to_the_emulation_layer() {
    let key = format!("HKEY_CURRENT_USER\\Software\\au3-emu-force-{}", std::process::id());
    let src = format!(
        r#"
Func F()
    RegWrite("{k}", "V", "REG_SZ", "emulated")
    Return RegRead("{k}", "V")
EndFunc
"#,
        k = key
    );
    let prog = parse(&src).expect("parses");
    let mut rt = Runtime::with_program(&prog);
    let mut emu = WindowsEmulation::new();
    emu = emu.with_memory_registry();
    rt.set_platform(host_platform_with_options(PlatformOptions {
        emulation: emu,
        force_emulated: vec!["RegRead".into(), "RegWrite".into(), "RegDelete".into()],
    }));
    rt.set_profile(autoitv3_runtime::ExecutionProfile::faithful());
    // The emulated (in-memory) registry answers the round trip.
    assert_eq!(
        rt.call_function("F", vec![]).expect("runs").to_autoit_string(),
        "emulated"
    );
    // ...and the real registry never saw the key.
    let probe = format!(
        r#"
Func G()
    RegRead("{k}", "V")
    Return @error
EndFunc
"#,
        k = key
    );
    let prog = parse(&probe).expect("parses");
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(host_platform_with(WindowsEmulation::new().disabled()));
    assert_ne!(
        rt.call_function("G", vec![]).expect("runs").to_int(),
        0,
        "key leaked into the real registry"
    );
}

#[test]
fn member_access_on_a_non_object_is_an_unsupported_error() {
    let src = "Func F()
    Local $x = 1
    Return $x.Foo
EndFunc
";
    let prog = parse(src).expect("parses");
    let mut rt = autoitv3_platform::runtime_with_platform(&prog);
    assert!(rt.call_function("F", vec![]).is_err());
}
