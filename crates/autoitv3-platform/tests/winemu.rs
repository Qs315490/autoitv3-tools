//! Tests for the Windows emulation layer.
//!
//! The suite runs on **every** host: the platform stack here is the
//! emulation layer alone, so the same expectations hold whether the host is
//! Linux (winemu is the whole story) or Windows (winemu is what a script
//! gets when the native layer is absent).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use autoitv3_platform::{CommonPlatform, CompositePlatform};
use autoitv3_platform::winemu::{
    Control, ControlKind, FileRegistry, GuiBackend, GuiEvent, GuiUpdate, HeadlessBackend,
    MemoryRegistry,
    Progress, RegistryData, RegistryStore, Splash, Window, WindowsArch, WindowsEmulation,
    WindowsPaths, WindowsVersion,
};
use autoitv3_runtime::profile::ExecutionProfile;
use autoitv3_runtime::{Runtime, Value};

/// Run `Func F()` from `body` against a specific emulated machine.
fn run(emu: WindowsEmulation, body: &str) -> Value {
    run_profiled(emu, ExecutionProfile::faithful(), body)
}

/// As [`run`], with an explicit execution profile.
fn run_profiled(emu: WindowsEmulation, profile: ExecutionProfile, body: &str) -> Value {
    let src = format!("Func F()\n{body}\nEndFunc\n");
    let prog = autoitv3_ast::parse(&src).expect("parses");
    let mut rt = Runtime::with_program(&prog);
    // The emulation layer first, the common layer beneath it — the same
    // composition a Linux analysis run gets, on every host.
    rt.set_platform(Box::new(CompositePlatform::new(
        "winemu+common",
        vec![Box::new(emu), Box::new(CommonPlatform::new())],
    )));
    rt.set_profile(profile);
    rt.call_function("F", vec![]).expect("no runtime error")
}

fn text(emu: WindowsEmulation, body: &str) -> String {
    run(emu, body).to_autoit_string()
}

/// As [`run`], but with the emulation's drive map wired into the common layer
/// the way `host_platform_with` does it — so `C:\...` arguments reach the host
/// as host paths.
fn run_mapped(emu: WindowsEmulation, body: &str) -> Value {
    let src = format!("Func F()\n{body}\nEndFunc\n");
    let prog = autoitv3_ast::parse(&src).expect("parses");
    let mut rt = Runtime::with_program(&prog);
    let common = match emu.path_map() {
        Some(map) => CommonPlatform::new().with_path_map(map.clone()),
        None => CommonPlatform::new(),
    };
    rt.set_platform(Box::new(CompositePlatform::new(
        "winemu+common",
        vec![Box::new(emu), Box::new(common)],
    )));
    rt.set_profile(ExecutionProfile::faithful());
    rt.call_function("F", vec![]).expect("no runtime error")
}

/// The default machine: Windows 10 x64, with no rendering.
///
/// The backend is spelled out because the platform's own default is a real
/// Win32 window on Windows: these tests are about the emulation's semantics, and
/// one that opened a window would flash on a desktop and need a display session
/// to run at all. The seam itself is covered by the tests that install a
/// recording backend below, and by `gui_egui.rs`, which install their own.
fn win10() -> WindowsEmulation {
    WindowsEmulation::new().with_gui_backend(Box::new(HeadlessBackend::new()))
}

/// A Windows 10 machine whose registry is a file unique to one test, so a
/// write never lands in the working directory.
fn win10_with_registry(tag: &str) -> (WindowsEmulation, std::path::PathBuf) {
    let file = scratch(tag).join("registry.txt");
    (win10().with_registry_file(file.clone()), file)
}

/// The default emulated directory layout, for direct store tests.
fn default_paths() -> WindowsPaths {
    WindowsPaths::new("User", "PC", WindowsArch::X64)
}

/// A scratch directory unique to one test.
fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("au3-winemu-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

// ---------------------------------------------------------------------------
// Version selection
// ---------------------------------------------------------------------------

#[test]
fn the_default_machine_is_windows_10_x64() {
    let emu = win10();
    assert_eq!(emu.version(), WindowsVersion::Win10);
    assert_eq!(emu.arch(), WindowsArch::X64);
    assert_eq!(text(win10(), "Return @OSVersion"), "WIN_10");
    assert_eq!(text(win10(), "Return @OSBuild"), "19045");
    assert_eq!(text(win10(), "Return @OSType"), "WIN32_NT");
    assert_eq!(text(win10(), "Return @OSArch"), "X64");
    assert_eq!(text(win10(), "Return @ProcessorArch"), "X64");
    assert_eq!(text(win10(), "Return @AutoItX64"), "1");
}

#[test]
fn every_selectable_version_has_a_consistent_identity() {
    let cases = [
        (WindowsVersion::Win7, "WIN_7", "7601", "Service Pack 1"),
        (WindowsVersion::Win8, "WIN_8", "9200", ""),
        (WindowsVersion::Win81, "WIN_81", "9600", ""),
        (WindowsVersion::Win10, "WIN_10", "19045", ""),
        (WindowsVersion::Win11, "WIN_11", "22631", ""),
    ];
    for (version, macro_name, build, service_pack) in cases {
        let emu = WindowsEmulation::new().with_version(version);
        assert_eq!(text(emu, "Return @OSVersion"), macro_name);
        assert_eq!(text(win10().with_version(version), "Return @OSBuild"), build);
        assert_eq!(
            text(win10().with_version(version), "Return @OSServicePack"),
            service_pack
        );
    }
}

#[test]
fn version_names_accept_the_loose_spellings_a_user_types() {
    let parse = |s: &str| WindowsVersion::from_name(s);
    assert_eq!(parse("win10"), Some(WindowsVersion::Win10));
    assert_eq!(parse("WIN_11"), Some(WindowsVersion::Win11));
    assert_eq!(parse("Windows 10"), Some(WindowsVersion::Win10));
    assert_eq!(parse("win8.1"), Some(WindowsVersion::Win81));
    assert_eq!(parse("win10x64"), Some(WindowsVersion::Win10));
    assert_eq!(parse("7"), Some(WindowsVersion::Win7));
    // A full `major.minor.build` selects the release with that build too, so
    // Win11 does not collapse into Win10.
    assert_eq!(parse("10.0.22631"), Some(WindowsVersion::Win11));
    assert_eq!(parse("10.0.19045"), Some(WindowsVersion::Win10));
    assert_eq!(parse("10.0"), Some(WindowsVersion::Win10));
    assert_eq!(parse("solaris"), None);
}

#[test]
fn x86_changes_the_architecture_and_the_pointer_size() {
    let emu = WindowsEmulation::new().with_arch(WindowsArch::X86);
    assert_eq!(text(emu, "Return @OSArch"), "X86");
    // A pointer is 4 bytes on x86, 8 on x64 — and the struct layout follows.
    let body = r#"Local $t = DllStructCreate("struct;ptr p;dword d;endstruct")
    Return DllStructGetSize($t)"#;
    assert_eq!(text(WindowsEmulation::new().with_arch(WindowsArch::X86), body), "8");
    assert_eq!(text(WindowsEmulation::new(), body), "12");
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

#[test]
fn directory_macros_use_the_windows_layout() {
    let body = r#"Return @WindowsDir & "|" & @SystemDir & "|" & @ProgramFilesDir & "|" & @HomeDrive & "|" & @ComSpec"#;
    assert_eq!(
        text(win10(), body),
        r"C:\Windows|C:\Windows\System32|C:\Program Files|C:|C:\Windows\cmd.exe"
    );
}

#[test]
fn per_user_macros_share_one_profile() {
    let emu = win10();
    let profile = emu.paths().user_profile.clone();
    assert_eq!(text(win10(), "Return @UserProfileDir"), profile);
    assert_eq!(text(win10(), "Return @HomePath"), profile);
    assert_eq!(
        text(win10(), "Return @AppDataDir"),
        format!(r"{profile}\AppData\Roaming")
    );
    assert!(text(win10(), "Return @StartupDir").ends_with(r"Start Menu\Programs\Startup"));
}

#[test]
fn the_temp_dir_macro_names_a_directory_the_host_actually_has() {
    // `@TempDir` is where a script writes its scratch files, so it is the one
    // directory macro that answers the *host's* temporary directory rather than
    // a spot in the emulated Windows layout — under the emulated drive, so it
    // still maps back to the same place.
    let seen = text(win10(), "Return @TempDir");
    let host = std::env::temp_dir();
    let mapped = autoitv3_platform::PathMap::host_root().to_host(&seen);
    let flatten = |s: &str| s.replace('\\', "/").trim_end_matches('/').to_string();
    assert!(
        mapped.as_deref() == Some(host.as_path()) || flatten(&seen) == flatten(&host.to_string_lossy()),
        "got {seen:?}, host {host:?}"
    );
}

#[test]
fn with_host_paths_lets_the_common_layer_answer() {
    // A directory macro carries no trailing separator — it is the host's own
    // temporary directory, spelled the host's way.
    let seen = text(win10().with_host_paths(), "Return @TempDir");
    let host = std::env::temp_dir().to_string_lossy().into_owned();
    assert_eq!(
        seen.trim_end_matches(std::path::MAIN_SEPARATOR),
        host.trim_end_matches(std::path::MAIN_SEPARATOR),
        "got {seen:?}, host {host:?}"
    );
    // Windows-only macros are still emulated, because the host cannot answer
    // them at all.
    assert_eq!(text(win10().with_host_paths(), "Return @WindowsDir"), r"C:\Windows");
}

// ---------------------------------------------------------------------------
// DllStruct
// ---------------------------------------------------------------------------

#[test]
fn dllstruct_layout_and_typed_access() {
    let body = r#"Local $t = DllStructCreate("struct;dword a;char name[8];endstruct")
    DllStructSetData($t, "a", 42)
    DllStructSetData($t, "name", "hi")
    Return DllStructGetSize($t) & "|" & DllStructGetData($t, "a") & "|" & DllStructGetData($t, "name")"#;
    assert_eq!(text(win10(), body), "12|42|hi");
}

#[test]
fn dllstruct_type_keywords_are_case_insensitive() {
    // AutoIt accepts `byte`, `BYTE`, `Byte` and `ULONG` alike, and generated
    // scripts use the upper-case spelling throughout.
    let body = r#"Local $t = DllStructCreate("struct;BYTE data[4];ULONG flag;ENDSTRUCT")
    Local $u = DllStructCreate("byte [8]")
    Return DllStructGetSize($t) & "|" & DllStructGetSize($u) & "|" & @error"#;
    assert_eq!(text(win10(), body), "8|8|0");
}

#[test]
fn dllstruct_wide_strings_and_index_access() {
    let body = r#"Local $t = DllStructCreate("struct;wchar text[16];dword flag;endstruct")
    DllStructSetData($t, "text", "wide")
    DllStructSetData($t, 2, 7)
    Return DllStructGetSize($t) & "|" & DllStructGetData($t, "text") & "|" & DllStructGetData($t, 2)"#;
    // 16 wide chars = 32 bytes, then a 4-byte dword.
    assert_eq!(text(win10(), body), "36|wide|7");
}

#[test]
fn dllstruct_array_elements_are_addressable() {
    // `DllStructGetData` takes the index as its 3rd argument and
    // `DllStructSetData` as its 4th.
    let body = r#"Local $t = DllStructCreate("struct;wchar text[8];endstruct")
    DllStructSetData($t, "text", "abcdef")
    DllStructSetData($t, "text", "Z", 2)
    Return DllStructGetData($t, "text") & "|" & DllStructGetData($t, "text", 2) & "|" & DllStructGetData($t, "text", 3)"#;
    assert_eq!(text(win10(), body), "aZcdef|Z|c");
}

#[test]
fn dllstruct_reports_bad_definitions_and_handles() {
    assert_eq!(
        text(win10(), r#"Local $t = DllStructCreate("nonsense x")
    Return $t & ":" & @error"#),
        "0:1"
    );
    assert_eq!(
        text(win10(), r#"Local $bad = DllStructGetSize(99)
    Return $bad & ":" & @error"#),
        "0:1"
    );
    assert_eq!(text(win10(), "Return IsDllStruct(1)"), "0");
    assert_eq!(
        text(win10(), r#"Local $t = DllStructCreate("dword x")
    Return IsDllStruct($t)"#),
        "1"
    );
}

// ---------------------------------------------------------------------------
// DllCall version queries — the reason the layer exists
// ---------------------------------------------------------------------------

/// The `OSVERSIONINFOW` definition the reference sample builds at run time.
const VERSION_STRUCT: &str = "struct;dword OSVersionInfoSize;dword MajorVersion;\
dword MinorVersion;dword BuildNumber;dword PlatformId;wchar CSDVersion[128];endstruct";

#[test]
fn getversionexw_fills_the_struct_from_the_selected_version() {
    let body = format!(
        r#"Local $t = DllStructCreate("{def}")
    DllStructSetData($t, "OSVersionInfoSize", DllStructGetSize($t))
    Local $ret = DllCall("kernel32.dll", "int", "GetVersionExW", "ptr", $t)
    Return $ret[0] & "|" & DllStructGetData($t, "MajorVersion") & "." & _
        DllStructGetData($t, "MinorVersion") & "." & DllStructGetData($t, "BuildNumber") & _
        "|" & DllStructGetData($t, "PlatformId") & "|" & DllStructGetData($t, "CSDVersion")"#,
        def = VERSION_STRUCT
    );
    assert_eq!(text(win10(), &body), "1|10.0.19045|2|");
    assert_eq!(
        text(win10().with_version(WindowsVersion::Win11), &body),
        "1|10.0.22631|2|"
    );
    assert_eq!(
        text(win10().with_version(WindowsVersion::Win7), &body),
        "1|6.1.7601|2|Service Pack 1"
    );
}

#[test]
fn dllcall_returns_an_array_like_autio() {
    // AutoIt returns `[return value, byref args...]`; scripts index it
    // (`Local $r = DllCall(...)` / `If Not $r[0] Then ...`).
    let body = r#"Local $r = DllCall("kernel32.dll", "dword", "GetVersion")
    Return IsArray($r) & "|" & UBound($r) & "|" & $r[0]"#;
    let expected = format!(
        "True|1|{}",
        WindowsVersion::Win10.packed_get_version()
    );
    assert_eq!(text(win10(), body), expected);
}

#[test]
fn rtlgetversion_and_getversion_agree_with_the_version() {
    let body = format!(
        r#"Local $t = DllStructCreate("{def}")
    DllCall("ntdll.dll", "long", "RtlGetVersion", "ptr", $t)
    Local $gv = DllCall("kernel32.dll", "dword", "GetVersion")
    Return DllStructGetData($t, "BuildNumber") & "|" & $gv[0]"#,
        def = VERSION_STRUCT
    );
    // `GetVersion` packs major | minor << 8 | build << 16.
    let expected = format!("22631|{}", WindowsVersion::Win11.packed_get_version());
    assert_eq!(text(win10().with_version(WindowsVersion::Win11), &body), expected);
}

#[test]
fn getsysteminfo_writes_the_processor_architecture() {
    let body = r#"Local $t = DllStructCreate("struct;word wProcessorArchitecture;word wReserved;dword dwPageSize;endstruct")
    DllCall("kernel32.dll", "none", "GetSystemInfo", "ptr", $t)
    Return DllStructGetData($t, "wProcessorArchitecture") & "|" & DllStructGetData($t, "dwPageSize")"#;
    assert_eq!(text(win10(), body), "9|4096");
    assert_eq!(
        text(win10().with_arch(WindowsArch::X86), body),
        "0|4096"
    );
}

#[test]
fn an_unimplemented_dllcall_sets_error_and_returns_zero() {
    // The emulation does not invent a result for a call it cannot make: the
    // script's own error handling stays in charge.
    let body = r#"Local $r = DllCall("ntdll.dll", "int", "NtQuerySystemInformation")
    Return $r & ":" & @error"#;
    assert_eq!(text(win10(), body), "0:1");
}

#[test]
fn a_bare_ansi_name_answers_its_a_arm() {
    // AutoIt appends the ANSI suffix when the bare name has no export
    // (`lstrlen` → `lstrlenA`); the emulation must do the same, or a script
    // that omits the suffix diverges from Windows.
    let body = r#"Local $r = DllCall("kernel32.dll", "int", "lstrlen", "str", "abc")
    Return $r[0] & ":" & @error"#;
    assert_eq!(text(win10(), body), "3:0");
}

#[test]
fn a_bare_name_removed_from_its_arm_still_hits_the_a_variant() {
    // `GetVersionEx`/`FindResource`/`GetModuleHandle`/`LoadLibrary`/
    // `CryptAcquireContext` only list their A/W arms; the ANSI fallback has to
    // carry the bare spelling.
    let body = format!(
        r#"Local $t = DllStructCreate("{def}")
    DllStructSetData($t, "OSVersionInfoSize", DllStructGetSize($t))
    Local $r = DllCall("kernel32.dll", "int", "GetVersionEx", "ptr", $t)
    Return $r[0] & "|" & DllStructGetData($t, "MajorVersion")"#,
        def = VERSION_STRUCT
    );
    assert_eq!(text(win10(), &body), "1|10");
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

#[test]
fn registry_is_seeded_consistently_with_the_version() {
    let body = r#"Return RegRead("HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion", "CurrentBuild") & "|" & _
        RegRead("HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows NT\CurrentVersion", "ProductName") & "|" & _
        RegRead("HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion", "ProgramFilesDir")"#;
    assert_eq!(
        text(win10(), body),
        r"19045|Windows 10 Pro|C:\Program Files"
    );
    assert_eq!(
        text(win10().with_version(WindowsVersion::Win11), body),
        r"22631|Windows 11 Pro|C:\Program Files"
    );
}

#[test]
fn registry_write_read_enumerate_delete() {
    let (emu, _file) = win10_with_registry("reg-rw");
    let body = r#"RegWrite("HKCU\Software\Au3Test", "Greeting", 1, "hello")
    RegWrite("HKCU\Software\Au3Test", "Count", 4, 7)
    Local $s = RegRead("HKCU\Software\Au3Test", "Greeting") & "|" & _
        RegRead("HKCU\Software\Au3Test", "Count") & "|" & _
        RegEnumVal("HKCU\Software\Au3Test", 1) & "|" & RegEnumVal("HKCU\Software\Au3Test", 2)
    Local $d = RegDelete("HKCU\Software\Au3Test", "Greeting")
    Return $s & "|" & $d & "|" & RegRead("HKCU\Software\Au3Test", "Greeting") & "|" & @error"#;
    // Enumeration is sorted, so "Count" comes before "Greeting".
    assert_eq!(text(emu, body), "hello|7|Count|Greeting|1||1");
}

#[test]
fn the_default_registry_is_a_file_whose_writes_persist() {
    let (emu, file) = win10_with_registry("reg-persist");
    let body = r#"RegWrite("HKCU\Software\Persist", "Value", 1, "kept")
    Return RegRead("HKCU\Software\Persist", "Value")"#;
    assert_eq!(text(emu, body), "kept");
    // `text()` consumes the emulation, whose drop flushes the store to disk.
    assert!(file.exists(), "a write should have created the registry file");
    let stored = std::fs::read_to_string(&file).unwrap();
    assert!(stored.starts_with("# au3-registry v1"), "{stored}");
    assert!(stored.contains("REG_SZ\tkept"), "{stored}");
    // The file holds the *records*, not a dump of the per-version seed.
    assert!(!stored.contains("Windows 10 Pro"), "{stored}");

    // A brand-new emulation pointed at the same file sees the value: the file
    // *is* the registry, not just a log.
    let reopened = win10().with_registry_file(file.clone());
    assert_eq!(
        text(reopened, r#"Return RegRead("HKCU\Software\Persist", "Value")"#),
        "kept"
    );
}

#[test]
fn a_persisted_file_does_not_pin_the_seed_to_one_version() {
    // Because only the file's own records are written back, the seeded keys
    // follow the *selected* version on the next run.
    let (emu, file) = win10_with_registry("reg-version");
    assert_eq!(
        text(
            emu,
            r#"RegWrite("HKCU\Software\Persist", "Value", 1, "kept")
    Return 1"#
        ),
        "1"
    );
    let body = r#"Return RegRead("HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion", "ProductName") & "|" & _
        RegRead("HKCU\Software\Persist", "Value")"#;
    let win11 = win10()
        .with_registry_file(file)
        .with_version(WindowsVersion::Win11);
    assert_eq!(text(win11, body), "Windows 11 Pro|kept");
}

#[test]
fn a_registry_file_is_overlaid_on_the_per_version_seed() {
    // A hand-written snapshot only has to mention what it cares about: the
    // standard keys are still there underneath.
    let file = scratch("reg-overlay").join("registry.txt");
    std::fs::write(
        &file,
        "# au3-registry v1\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Captured\tBuild\tREG_SZ\tfrom-snapshot\n",
    )
    .unwrap();
    let emu = win10().with_registry_file(file);
    let body = r#"Return RegRead("HKLM\SOFTWARE\Captured", "Build") & "|" & _
        RegRead("HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion", "CurrentBuild")"#;
    assert_eq!(text(emu, body), "from-snapshot|19045");
}

#[test]
fn the_memory_registry_stays_available_and_touches_no_file() {
    let file = scratch("reg-memory").join("registry.txt");
    // The path is never used once the memory store is selected.
    let emu = win10().with_registry_file(file.clone()).with_memory_registry();
    let body = r#"RegWrite("HKCU\Software\Mem", "V", 1, "x")
    Return RegRead("HKCU\Software\Mem", "V")"#;
    assert_eq!(text(emu, body), "x");
    assert!(!file.exists(), "the memory store must not write anything");
}

#[test]
fn deleting_a_seeded_value_does_not_create_a_file() {
    let file = scratch("reg-seed-delete").join("registry.txt");
    let emu = win10().with_registry_file(file.clone());
    let body = r#"Local $d = RegDelete("HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion", "ProductName")
    Return $d & "|" & RegRead("HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion", "ProductName") & "|" & @error"#;
    // The delete succeeds in-run; only the seed knew the value, so there is
    // nothing to record on disk and no file appears.
    assert_eq!(text(emu, body), "1||1");
    assert!(!file.exists(), "a seed-only delete should not create the file");
}

#[test]
fn a_registry_file_round_trips_every_value_type() {
    let file = scratch("reg-format").join("registry.txt");
    let paths = default_paths();
    let open = |file: &std::path::Path| {
        FileRegistry::seeded(WindowsVersion::Win10, WindowsArch::X64, &paths, file)
    };
    let key = r"HKCU\Software\Types";
    let mut store = open(&file);
    assert!(store.write(key, "S", RegistryData::Sz("text".into())));
    assert!(store.write(key, "E", RegistryData::ExpandSz(r"%TEMP%\x".into())));
    assert!(store.write(key, "D", RegistryData::Dword(u32::MAX)));
    assert!(store.write(key, "Q", RegistryData::Qword(1_234_567_890_123)));
    // An item containing the separator must survive: it is escaped on the way
    // out and split on unescaped separators on the way in.
    assert!(store.write(
        key,
        "M",
        RegistryData::MultiSz(vec!["a".into(), "b|c".into(), "d\ne".into()]),
    ));
    assert!(store.write(key, "B", RegistryData::Binary(vec![0x0a, 0x0b, 0xff])));
    // A key with no values has to survive a round trip too.
    store.write(r"HKCU\Software\EmptyKey", "", RegistryData::Sz(String::new()));

    // Writes batch in memory; flush (or drop) puts them on disk.
    store.flush().expect("flush the registry store to disk");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains(&format!("REG_EXPAND_SZ\t{}", r"%TEMP%\\x")), "{text}");
    assert!(text.contains("REG_DWORD\t4294967295"), "{text}");
    assert!(text.contains("REG_BINARY\t0a0bff"), "{text}");

    let reopened = open(&file);
    let read = |name: &str| reopened.read(key, name);
    assert_eq!(read("S"), Some(RegistryData::Sz("text".into())));
    assert_eq!(read("E"), Some(RegistryData::ExpandSz(r"%TEMP%\x".into())));
    assert_eq!(read("D"), Some(RegistryData::Dword(u32::MAX)));
    assert_eq!(read("Q"), Some(RegistryData::Qword(1_234_567_890_123)));
    assert_eq!(
        read("M"),
        Some(RegistryData::MultiSz(vec![
            "a".into(),
            "b|c".into(),
            "d\ne".into()
        ]))
    );
    assert_eq!(read("B"), Some(RegistryData::Binary(vec![0x0a, 0x0b, 0xff])));
    // The key that held only a default value, and the keys seeded underneath,
    // are both present.
    assert!(reopened.key_exists(r"HKCU\Software\EmptyKey"));
    assert!(reopened.key_exists(r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion"));
    assert!(reopened.enum_keys(r"HKCU\Software").contains(&"Types".to_string()));
}

#[test]
fn registry_missing_values_report_error() {
    assert_eq!(
        text(win10(), r#"RegRead("HKLM\SOFTWARE\NothingHere", "X")
    Return @error"#),
        "1"
    );
    assert_eq!(
        text(win10(), r#"Return RegEnumKey("HKLM\SOFTWARE", 999) & ":" & @error"#),
        ":1"
    );
}

#[test]
fn registry_enumeration_lists_seeded_subkeys() {
    assert_eq!(
        text(win10(), r#"Return RegEnumKey("HKCU\Software", 1)"#),
        "Microsoft"
    );
    assert_eq!(
        text(win10(), r#"Local $v = RegEnumVal("HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion", 1)
    Return StringLen($v) > 0"#),
        "True"
    );
}

#[test]
fn a_custom_registry_store_can_be_plugged_in() {
    // The trait is the extension point: an embedder supplies its own view.
    #[derive(Debug)]
    struct Recording {
        inner: MemoryRegistry,
        writes: Rc<Cell<usize>>,
    }
    impl RegistryStore for Recording {
        fn read(&self, key: &str, value: &str) -> Option<RegistryData> {
            self.inner.read(key, value)
        }
        fn write(&mut self, key: &str, value: &str, data: RegistryData) -> bool {
            self.writes.set(self.writes.get() + 1);
            self.inner.write(key, value, data)
        }
        fn delete_value(&mut self, key: &str, value: &str) -> bool {
            self.inner.delete_value(key, value)
        }
        fn delete_key(&mut self, key: &str, recurse: bool) -> bool {
            self.inner.delete_key(key, recurse)
        }
        fn key_exists(&self, key: &str) -> bool {
            self.inner.key_exists(key)
        }
        fn enum_keys(&self, key: &str) -> Vec<String> {
            self.inner.enum_keys(key)
        }
        fn enum_values(&self, key: &str) -> Vec<String> {
            self.inner.enum_values(key)
        }
    }

    let writes = Rc::new(Cell::new(0));
    let mut inner = MemoryRegistry::new();
    inner.set_sz(r"HKLM\SOFTWARE\Fixture", "Injected", "from-fixture");
    let emu = win10().with_registry(Box::new(Recording {
        inner,
        writes: writes.clone(),
    }));

    let body = r#"Local $v = RegRead("HKLM\SOFTWARE\Fixture", "Injected")
    RegWrite("HKLM\SOFTWARE\Fixture", "Added", 1, "x")
    Return $v & "|" & RegRead("HKLM\SOFTWARE\Fixture", "Added")"#;
    assert_eq!(text(emu, body), "from-fixture|x");
    assert_eq!(writes.get(), 1);
}

#[test]
fn the_read_only_profile_refuses_registry_writes() {
    let (emu, _file) = win10_with_registry("reg-readonly");
    let body = r#"Local $w = RegWrite("HKCU\Software\Au3Test", "X", 1, "y")
    Return $w & ":" & @error"#;
    // The default (faithful) profile lets the write happen ...
    assert_eq!(text(emu, body), "1:0");
    // ... while the deterministic analysis profile refuses it, like it refuses
    // file and environment writes.
    let read_only = run_profiled(win10(), ExecutionProfile::deterministic(), body);
    assert_eq!(read_only.to_autoit_string(), "0:1");
}

// ---------------------------------------------------------------------------
// Clipboard and drives
// ---------------------------------------------------------------------------

#[test]
fn clipboard_round_trips_through_a_file() {
    let dir = scratch("clip");
    let file = dir.join("clipboard.txt");
    let emu = || win10().with_clipboard_file(file.clone());

    assert_eq!(text(emu(), r#"Return ClipPut("copied") & "|" & ClipGet()"#), "1|copied");
    assert!(file.exists(), "the clipboard should be a file on disk");
    // A second run in a fresh runtime still sees the stored text.
    assert_eq!(text(emu(), "Return ClipGet()"), "copied");
}

#[test]
fn drive_queries_answer_for_the_emulated_c_drive() {
    // `$d[0]` is the number of drives and the letters start at `$d[1]`; a
    // drive is spelled `c:` — lower-case letter and colon, no separator (the
    // official implementation strips the one it probed with).
    let body = r#"Local $d = DriveGetDrive("FIXED")
    Return $d[0] & "|" & $d[1] & "|" & DriveGetType("C:\") & "|" & DriveGetFileSystem("C:\") & "|" & _
        (DriveSpaceTotal("C:\") > 0) & "|" & (DriveSpaceFree("C:\") > 0) & "|" & DriveStatus("C:\")"#;
    assert_eq!(
        text(win10(), body),
        r"1|c:|FIXED|NTFS|True|True|READY"
    );
}

#[test]
fn an_unknown_drive_reports_error() {
    assert_eq!(
        text(win10(), r#"Return DriveGetType("Z:\") & ":" & @error"#),
        ":1"
    );
    // No drives of the type: `@error` 1 and an empty *string*, which is what
    // the official interpreter answers for a type nothing matches (measured
    // through the "BOGUS" path in `docs/drive-probe.au3`).
    assert_eq!(
        text(
            win10(),
            r#"Local $d = DriveGetDrive("NETWORK")
    Local $err = @error
    Return VarGetType($d) & ":" & $d & ":" & $err"#
        ),
        "String::1"
    );
}

#[test]
fn a_drive_type_list_takes_either_kind() {
    // `"FIXED,REMOVABLE"` asks for either kind, which is how a script looks for
    // "somewhere I can read a Windows directory from".
    let body = r#"Local $either = DriveGetDrive("FIXED,REMOVABLE")
    Local $either_err = @error
    Local $none = DriveGetDrive("CDROM,NETWORK")
    Local $none_err = @error
    Return $either[0] & "|" & $either[1] & "|" & $either_err & "|" & $none & "|" & $none_err"#;
    assert_eq!(text(win10(), body), r"1|c:|0||1");
}

#[test]
fn the_system_drive_macro_names_the_windows_drive() {
    assert_eq!(text(win10(), "Return @SystemDrive"), "C:");
}

// ---------------------------------------------------------------------------
// Switching the layer off
// ---------------------------------------------------------------------------

#[test]
fn a_disabled_layer_leaves_windows_calls_undefined() {
    let emu = win10().disabled();
    assert!(!emu.is_enabled());
    let src = "Func F()\n    Return DllStructCreate(\"dword x\")\nEndFunc\n";
    let prog = autoitv3_ast::parse(src).unwrap();
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(Box::new(emu));
    let err = rt.call_function("F", vec![]).unwrap_err();
    assert!(
        err.message().contains("undefined function"),
        "got: {}",
        err.message()
    );
    assert_eq!(rt.platform_name(), "windows-emulation");
}

#[test]
fn a_second_struct_over_a_pointer_aliases_the_first() {
    // `DllStructCreate($def, $ptr)` maps onto memory that already exists. This
    // is how a script reads back a buffer a `DllCall` filled in: it hands the
    // struct to the call, then re-reads the same bytes as `byte[]`.
    let body = r#"Local $t = DllStructCreate("byte[8]")
    DllStructSetData($t, 1, Binary("0x4142434445464748"))
    Local $alias = DllStructCreate("byte[4]", DllStructGetPtr($t))
    Local $before = BinaryToString(DllStructGetData($alias, 1))
    DllStructSetData($alias, 1, Binary("0x31323334"))
    Return $before & "|" & BinaryToString(DllStructGetData($t, 1))"#;
    assert_eq!(text(win10(), body), "ABCD|1234EFGH");
}

#[test]
fn a_decrypted_buffer_can_be_re_read_through_the_pointer() {
    // The shape `_Crypt_DecryptData` uses: a scratch struct far larger than the
    // ciphertext, and a second struct over its address that reads the plaintext
    // back out at its real length.
    let body = r#"Local $buf = DllStructCreate("byte[8]")
    DllStructSetData($buf, 1, Binary("0x01020304"))
    Local $view = DllStructCreate("byte[4]", DllStructGetPtr($buf))
    Return BinaryLen(DllStructGetData($view, 1)) & ":" & BinaryToString(DllStructGetData($view, 1))"#;
    assert_eq!(text(win10(), body), "4:\u{1}\u{2}\u{3}\u{4}");
}

#[test]
fn resources_extracted_next_to_the_script_answer_find_resource() {
    // `AutoIt3Wrapper_Res_File_Add` leaves the payload next to the script
    // (`__NAME`, `__Res64/NAME`, `__ResImage/_NAME`), which is what lets an
    // analysis read a build without the `.exe` it came from. These files are
    // consulted before any image.
    let dir = scratch("staged-resources");
    std::fs::create_dir_all(dir.join("__Res64")).unwrap();
    std::fs::write(dir.join("__PAYLOAD"), b"from a file").unwrap();
    std::fs::write(dir.join("__Res64").join("BIG"), b"bigger payload").unwrap();

    let emu = win10().with_resource_dirs([dir.clone()]);
    let body = r#"Local $h = DllCall("kernel32.dll", "handle", "FindResourceW", "handle", 0, "wstr", "PAYLOAD", "wstr", 10)
    Local $size = DllCall("kernel32.dll", "dword", "SizeofResource", "handle", 0, "handle", $h[0])
    Local $ptr = DllCall("kernel32.dll", "ptr", "LockResource", "handle", DllCall("kernel32.dll", "handle", "LoadResource", "handle", 0, "handle", $h[0])[0])
    Local $buf = DllStructCreate("byte[" & $size[0] & "]")
    DllCall("kernel32.dll", "none", "RtlMoveMemory", "ptr", DllStructGetPtr($buf), "ptr", $ptr[0], "dword", $size[0])
    Local $h2 = DllCall("kernel32.dll", "handle", "FindResourceW", "handle", 0, "wstr", "BIG", "wstr", 10)
    Local $s2 = DllCall("kernel32.dll", "dword", "SizeofResource", "handle", 0, "handle", $h2[0])
    Local $miss = DllCall("kernel32.dll", "handle", "FindResourceW", "handle", 0, "wstr", "NOPE", "wstr", 10)
    Local $err = @error
    Return $size[0] & "|" & BinaryToString(DllStructGetData($buf, 1)) & "|" & $s2[0] & "|" & IsArray($miss) & "|" & $err"#;
    let got = text(emu, body);
    assert_eq!(got, "11|from a file|14|False|1");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Windows files / PE, callbacks, COM, system info, shell (column B)
// ---------------------------------------------------------------------------

#[test]
fn file_create_and_read_shortcut_round_trips() {
    let dir = scratch("shortcut");
    let lnk = dir.join("app.lnk");
    let body = format!(
        r#"Local $ok = FileCreateShortcut("C:\Tools\app.exe", "{lnk}", "C:\Tools", "/silent", "My app", "C:\Tools\app.exe", "", 2, 3)
    Local $a = FileGetShortcut("{lnk}")
    If @error Then Return "error"
    Return $ok & "|" & $a[0] & "|" & $a[1] & "|" & $a[2] & "|" & $a[3] & "|" & $a[4] & "|" & $a[5] & "|" & $a[6]"#,
        lnk = lnk.display()
    );
    assert_eq!(
        text(win10(), &body),
        "1|C:\\Tools\\app.exe|C:\\Tools|/silent|My app|C:\\Tools\\app.exe|2|3"
    );
    // A real shell link starts with HeaderSize 0x4C.
    let bytes = std::fs::read(&lnk).unwrap();
    assert_eq!(&bytes[..4], &[0x4C, 0x00, 0x00, 0x00]);
}

#[test]
fn file_get_version_reports_a_missing_resource() {
    let dir = scratch("version");
    let plain = dir.join("plain.txt");
    std::fs::write(&plain, b"not a PE").unwrap();
    let body = format!(
        r#"Local $v = FileGetVersion("{p}")
    Return $v & ":" & @error"#,
        p = plain.display()
    );
    assert_eq!(text(win10(), &body), "0.0.0.0:1");
}

#[cfg(unix)]
#[test]
fn file_create_ntfs_link_makes_a_hard_link() {
    let dir = scratch("hardlink");
    let target = dir.join("orig.txt");
    let link = dir.join("link.txt");
    std::fs::write(&target, "payload").unwrap();
    let body = format!(
        r#"Return FileCreateNTFSLink("{link}", "{target}") & ":" & FileExists("{link}")"#,
        link = link.display(),
        target = target.display()
    );
    assert_eq!(text(win10(), &body), "1:1");
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "payload");
}

#[test]
fn file_recycle_moves_then_empties() {
    let dir = scratch("recycle");
    let victim = dir.join("victim.txt");
    std::fs::write(&victim, "x").unwrap();
    let emu = win10().with_recycle_dir(dir.join("trash"));
    let body = format!(
        r#"Local $ok = FileRecycle("{v}")
    Local $gone = FileExists("{v}")
    Local $empty = FileRecycleEmpty()
    Return $ok & ":" & $gone & ":" & $empty"#,
        v = victim.display()
    );
    assert_eq!(text(emu, &body), "1:0:1");
    assert!(!victim.exists());
}

#[test]
fn file_install_copies_from_disk() {
    let dir = scratch("install");
    let src = dir.join("payload.bin");
    let dst = dir.join("out.bin");
    std::fs::write(&src, b"data").unwrap();
    let body = format!(
        r#"Local $a = FileInstall("{s}", "{d}")
    Local $b = FileInstall("{s}", "{d}", 1)
    Return $a & ":" & $b"#,
        s = src.display(),
        d = dst.display()
    );
    assert_eq!(text(win10(), &body), "1:1");
    assert_eq!(std::fs::read(&dst).unwrap(), b"data".to_vec());
}

#[test]
fn dll_callbacks_register_get_and_free() {
    let body = r#"
Local $h1 = DllCallbackRegister("OnTick", "none", "int")
Local $h2 = DllCallbackRegister("OnTick", "none", "int")
Local $ptr = DllCallbackGetPtr($h1)
Local $free = DllCallbackFree($h1)
Local $after = DllCallbackGetPtr($h1)
Return ($h2 > $h1) & ":" & ($ptr = $h1) & ":" & $free & ":" & $after & ":" & @error
"#;
    assert_eq!(text(win10(), body), "True:True:1:0:1");
}

#[test]
fn dll_call_address_fails_without_a_loader() {
    let body = r#"Local $r = DllCallAddress("int", 0x1234)
    Return $r & ":" & @error"#;
    assert_eq!(text(win10(), body), "0:1");
}

#[test]
fn com_calls_fail_predictably() {
    let body = r#"Local $o = ObjCreate("NoSuch.ProgID.Anywhere")
    Local $e1 = @error
    Local $g = ObjGet("", "Some.Object")
    Local $e2 = @error
    Local $n = ObjName(0)
    Local $e3 = @error
    Return $o & ":" & $e1 & ":" & $g & ":" & $e2 & ":" & $n & ":" & $e3 & ":" & IsObj(0)"#;
    assert_eq!(text(win10(), body), "0:1:0:1::1:0");
}

#[test]
fn mem_get_stats_has_seven_elements() {
    let body = r#"Local $a = MemGetStats()
    Return UBound($a) & ":" & $a[0] & ":" & ($a[1] > 0)"#;
    assert_eq!(text(win10(), body), "7:50:True");
}

#[test]
fn is_admin_follows_the_machine() {
    assert_eq!(text(win10(), "Return IsAdmin()"), "1");
    assert_eq!(text(win10().with_admin(false), "Return IsAdmin()"), "0");
}

#[test]
fn drive_maps_add_get_delete_and_label() {
    let body = r#"
Local $add = DriveMapAdd("X:", "\\srv\share")
Local $got = DriveMapGet("X:")
Local $dup = DriveMapAdd("X:", "\\srv\other")
Local $dup_err = @error
Local $del = DriveMapDel("X:")
Local $missing = DriveMapGet("X:")
Local $missing_err = @error
Local $label = DriveSetLabel("C:", "DATA")
Local $read = DriveGetLabel("C:")
Return $add & "|" & $got & "|" & $dup & $dup_err & "|" & $del & "|" & $missing & ":" & $missing_err & "|" & $label & "|" & $read
"#;
    assert_eq!(text(win10(), body), "1|\\\\srv\\share|03|1|:1|1|DATA");
}

#[test]
fn drive_map_star_picks_a_letter() {
    let body = r#"Local $d = DriveMapAdd("*", "\\srv\share")
    Return StringLen($d) & ":" & StringRight($d, 1)"#;
    assert_eq!(text(win10(), body), "2::");
}

#[cfg(unix)]
#[test]
fn shell_execute_runs_a_program() {
    assert_eq!(text(win10(), r#"Return ShellExecute("/bin/true")"#), "1");
}

#[cfg(unix)]
#[test]
fn shell_execute_wait_returns_the_exit_code() {
    let body = r#"Return ShellExecuteWait("/bin/sh", "-c ""exit 3""")"#;
    assert_eq!(text(win10(), body), "3");
}

#[cfg(unix)]
#[test]
fn run_as_delegates_to_the_host() {
    // Credentials are accepted, not applied; the program still runs.
    let body = r#"Local $pid = RunAs("u", "d", "p", "/bin/true")
    Local $code = RunAsWait("u", "d", "p", "/bin/sh", "", 0, 0)
    Return ($pid > 0) & ":" & $code"#;
    assert_eq!(text(win10(), body), "True:0");
}

#[test]
fn shutdown_is_recorded_not_acted_on() {
    let body = r#"Local $a = Shutdown(1)
    Local $b = Shutdown(0)
    Return $a & ":" & $b"#;
    assert_eq!(text(win10(), body), "1:1");
}

#[test]
fn the_read_only_profile_refuses_side_effecting_calls() {
    let dir = scratch("readonly");
    let lnk = dir.join("x.lnk");
    let body = format!(
        r#"Local $a = FileCreateShortcut("C:\a.exe", "{lnk}")
    Local $b = FileRecycle("C:\nope.txt")
    Local $c = DriveMapAdd("X:", "\\srv\share")
    Local $d = ShellExecute("/bin/true")
    Return $a & ":" & $b & ":" & $c & ":" & $d"#,
        lnk = lnk.display()
    );
    let value = run_profiled(win10(), ExecutionProfile::deterministic(), &body);
    assert_eq!(value.to_autoit_string(), "0:0:0:0");
}

// ---------------------------------------------------------------------------
// Headless GUI: model, events, dialogs, backend seam
// ---------------------------------------------------------------------------

#[test]
fn gui_creates_controls_and_reads_them() {
    let body = r#"
GUICreate("T", 200, 100)
Local $label = GUICtrlCreateLabel("hello", 0, 0)
Local $input = GUICtrlCreateInput("start", 0, 20)
Local $check = GUICtrlCreateCheckbox("Enable", 0, 40)
Local $r0 = GUICtrlRead($check)
GUICtrlSetState($check, 1)
GUICtrlSetData($input, "typed")
Return GUICtrlRead($label) & "|" & GUICtrlRead($input) & "|" & $r0 & "|" & GUICtrlRead($check)
"#;
    assert_eq!(text(win10(), body), "hello|typed|4|1");
}

#[test]
fn gui_message_loop_drains_scripted_events() {
    let body = r#"
GUICreate("T", 100, 50)
Local $btn = GUICtrlCreateButton("Go", 0, 0)
Local $seen = ""
For $i = 1 To 5
    Local $msg = GUIGetMsg()
    If $msg = -3 Then
        $seen = $seen & "close"
        ExitLoop
    ElseIf $msg <> 0 Then
        $seen = $seen & $msg
    EndIf
Next
Return $seen & ":" & ($btn = 1)
"#;
    let emu = win10().with_gui_events(vec![GuiEvent::Control(1), GuiEvent::Close(0)]);
    assert_eq!(text(emu, body), "1close:True");
}

#[test]
fn gui_auto_close_terminates_an_ignoring_loop() {
    let body = r#"
GUICreate("T", 10, 10)
Local $n = 0
While 1
    $n = $n + 1
    If GUIGetMsg() = -3 Then ExitLoop
    If $n > 10 Then ExitLoop
WEnd
Return $n
"#;
    assert_eq!(text(win10().with_gui_auto_close(3), body), "3");
}

#[test]
fn gui_window_state_and_geometry() {
    let body = r#"
Local $win = GUICreate("My Window", 200, 100, 10, 20)
Local $before = WinGetState($win)
GUISetState(5, $win)
Local $after = WinGetState($win)
Local $pos = WinGetPos($win)
WinMove($win, "", 5, 6, 300, 150)
Local $pos2 = WinGetPos($win)
Local $title = WinGetTitle($win)
Local $exists = WinExists("My Window")
Local $closed = WinClose($win)
Return $before & ":" & $after & ":" & $pos[0] & "," & $pos[2] & ":" & $pos2[2] & "," & $pos2[3] & ":" & $title & ":" & $exists & ":" & $closed & ":" & WinExists($win)
"#;
    assert_eq!(
        text(win10(), body),
        "13:15:10,200:300,150:My Window:1:1:0"
    );
}

#[test]
fn listview_items_are_rows_of_their_listview() {
    // `GUICtrlCreateListViewItem` is how AutoIt adds a row, so the row has to
    // reach the ListView: its item count, the identifier `GUICtrlRead` answers
    // with and the text an item reads back are all the same row.
    let body = r#"
GUICreate("T", 300, 200)
Local $list = GUICtrlCreateListView("name|age", 0, 0, 200, 100)
Local $bob = GUICtrlCreateListViewItem("bob|30", $list)
Local $sue = GUICtrlCreateListViewItem("sue|25", $list)
Local $count = GUICtrlSendMsg($list, 0x1004, 0, 0)
Local $none = GUICtrlRead($list)
GUICtrlSetState($bob, 256)
Local $selected = GUICtrlRead($list)
Local $text = GUICtrlRead($bob)
Local $advanced = GUICtrlRead($bob, 1)
Local $other = GUICtrlRead($sue)
Return $count & ":" & $none & ":" & $selected & ":" & $bob & ":" & $text & ":" & $advanced & ":" & $other
"#;
    let value = text(win10(), body);
    let fields: Vec<&str> = value.split(':').collect();
    assert_eq!(fields[0], "2", "both items are rows: {value}");
    assert_eq!(fields[1], "0", "nothing is selected yet: {value}");
    assert_eq!(fields[2], fields[3], "selecting the item selects its row: {value}");
    assert_eq!(
        fields[4], "bob|30|",
        "the item reads back its own row, a separator after every cell: {value}"
    );
    // Without `$LVS_EX_CHECKBOXES` the advanced read is the text again — the
    // official interpreter answered the same.
    assert_eq!(fields[5], "bob|30|", "the advanced read is the text: {value}");
    assert_eq!(fields[6], "sue|25|", "the other row is untouched: {value}");
}

#[test]
fn deleting_an_item_removes_its_row() {
    let body = r#"
GUICreate("T", 300, 200)
Local $list = GUICtrlCreateListView("name", 0, 0, 200, 100)
Local $bob = GUICtrlCreateListViewItem("bob", $list)
Local $sue = GUICtrlCreateListViewItem("sue", $list)
GUICtrlDelete($bob)
Local $count = GUICtrlSendMsg($list, 0x1004, 0, 0)
Local $sue_row = GUICtrlRead($sue)
GUICtrlSetState($sue, 256)
Local $selected = GUICtrlRead($list)
Return $count & ":" & $sue_row & ":" & $selected & ":" & $sue
"#;
    let value = text(win10(), body);
    let fields: Vec<&str> = value.split(':').collect();
    assert_eq!(fields[0], "1", "the deleted row is gone: {value}");
    assert_eq!(
        fields[1], "sue|",
        "the row that is left kept its text: {value}"
    );
    assert_eq!(fields[2], fields[3], "the row was re-numbered: {value}");
}

#[test]
fn an_item_update_touches_only_the_columns_it_names() {
    let body = r#"
GUICreate("T", 300, 200)
Local $list = GUICtrlCreateListView("a|b|c", 0, 0, 200, 100)
Local $item = GUICtrlCreateListViewItem("1|2|3", $list)
GUICtrlSetData($item, "||9")
Local $third = GUICtrlRead($item)
GUICtrlSetData($item, "x")
Local $first = GUICtrlRead($item)
Return $third & ":" & $first
"#;
    // Only the cells the update names are written: the probe against the
    // official interpreter kept the first two columns for `"||9"`, and the read
    // comes back with a separator after every cell.
    assert_eq!(text(win10(), body), "1|2|9|:x|2|9|");
}

#[test]
fn a_minus_one_control_id_is_the_last_created_control() {
    let body = r#"
GUICreate("T", 200, 150)
Local $label = GUICtrlCreateLabel("first", 0, 0)
GUICtrlSetData(-1, "second")
GUICtrlSetState(-1, 32)
Return GUICtrlRead($label) & ":" & GUICtrlGetState($label)
"#;
    // A control nobody has touched reports `$GUI_SHOW | $GUI_ENABLE` (0x50),
    // and hiding it clears `$GUI_SHOW` and sets `$GUI_HIDE`: the official
    // interpreter answers 96 (0x60) here.
    assert_eq!(text(win10(), body), "second:96");
}

#[test]
fn treeview_items_nest_under_the_item_they_name() {
    let body = r#"
GUICreate("T", 200, 150)
Local $tree = GUICtrlCreateTreeView(0, 0, 100, 100)
Local $root = GUICtrlCreateTreeViewItem("root", $tree)
Local $child = GUICtrlCreateTreeViewItem("child", $root)
Local $other = GUICtrlCreateTreeViewItem("other", $tree)
GUICtrlSetState($child, 256)
Local $selected = GUICtrlRead($tree)
Local $child_text = GUICtrlRead($child, 1)
Local $other_text = GUICtrlRead($other, 1)
Local $focused = GUICtrlRead($child)
GUICtrlSetState($child, 512)
Local $bold = GUICtrlRead($child)
GUICtrlSetState($child, 0)
Local $plain = GUICtrlRead($child)
Return $selected & ":" & $child & ":" & $child_text & ":" & $other_text & ":" & $focused & ":" & $bold & ":" & $plain
"#;
    let value = text(win10(), body);
    let fields: Vec<&str> = value.split(':').collect();
    assert_eq!(fields[0], fields[1], "focusing an item selects it: {value}");
    assert_eq!(fields[2], "child");
    assert_eq!(fields[3], "other");
    assert_eq!(fields[4], "256", "the focused item keeps its focus bit: {value}");
    assert_eq!(fields[5], "768", "bold is added on top: {value}");
    assert_eq!(fields[6], "256", "setting the state to 0 clears bold: {value}");
}

#[test]
fn tabitems_are_pages_of_their_tab() {
    let body = r#"
GUICreate("T", 300, 200)
Local $tab = GUICtrlCreateTab(0, 0, 200, 150)
Local $one = GUICtrlCreateTabItem("one")
Local $first = GUICtrlCreateLabel("first page", 10, 30)
Local $two = GUICtrlCreateTabItem("two")
Local $second = GUICtrlCreateLabel("second page", 10, 30)
GUICtrlCreateTabItem("")
Local $outside = GUICtrlCreateLabel("outside", 0, 0)
Local $index = GUICtrlRead($tab)
Local $advanced = GUICtrlRead($tab, 1)
Local $hidden_second = GUICtrlGetState($second)
GUICtrlSetState($two, 16)
Local $index2 = GUICtrlRead($tab)
Local $shown_second = GUICtrlGetState($second)
Local $hidden_first = GUICtrlGetState($first)
Local $outside_state = GUICtrlGetState($outside)
Return $index & ":" & $advanced & ":" & $one & ":" & $hidden_second & ":" & $index2 & ":" & $shown_second & ":" & $hidden_first & ":" & $outside_state
"#;
    let value = text(win10(), body);
    let fields: Vec<&str> = value.split(':').collect();
    assert_eq!(fields[0], "0", "the first page is the selected one: {value}");
    assert_eq!(
        fields[1], fields[2],
        "the advanced read is the page's id: {value}"
    );
    // A control on a page nobody selected still reports
    // `$GUI_SHOW | $GUI_ENABLE`: the official interpreter answers 0x50 for both
    // pages, so which page is on screen is the renderer's business, not the
    // state word's.
    assert_eq!(fields[3], "80", "the other page's control reads 0x50: {value}");
    assert_eq!(fields[4], "1", "showing the second page selects it: {value}");
    assert_eq!(fields[5], "80", "and its control still reads 0x50: {value}");
    assert_eq!(fields[6], "80", "the first page's too: {value}");
    assert_eq!(
        fields[7], "80",
        "a control created after the structure closed is a normal control: {value}"
    );
}

#[test]
fn control_listview_commands_read_and_change_the_rows() {
    let body = r#"
GUICreate("T", 300, 200)
Local $list = GUICtrlCreateListView("name|age", 0, 0, 200, 100)
Local $bob = GUICtrlCreateListViewItem("bob|30", $list)
Local $sue = GUICtrlCreateListViewItem("sue|25", $list)
Local $count = ControlListView("T", "", $list, "GetItemCount")
Local $cell = ControlListView("T", "", $list, "GetText", 1, 0)
Local $subs = ControlListView("T", "", $list, "GetSubItemCount")
ControlListView("T", "", $list, "Select", 1)
Local $selected = ControlListView("T", "", $list, "GetSelected")
Local $is = ControlListView("T", "", $list, "IsSelected", 1)
Local $found = ControlListView("T", "", $list, "FindItem", "sue")
Local $missing = ControlListView("T", "", $list, "FindItem", "nobody")
Return $count & ":" & $cell & ":" & $subs & ":" & $selected & ":" & $is & ":" & $found & ":" & $missing
"#;
    // `GetSubItemCount` is the column count (two columns here), which is what
    // the official interpreter answers.
    assert_eq!(text(win10(), body), "2:sue:2:1:1:1:-1");
}

#[test]
fn control_treeview_commands_take_item_references() {
    let body = r##"
GUICreate("T", 300, 200)
Local $tree = GUICtrlCreateTreeView(0, 0, 200, 150)
Local $root = GUICtrlCreateTreeViewItem("root", $tree)
Local $child = GUICtrlCreateTreeViewItem("child", $root)
Local $other = GUICtrlCreateTreeViewItem("other", $tree)
Local $roots = ControlTreeView("T", "", $tree, "GetItemCount", "")
Local $kids = ControlTreeView("T", "", $tree, "GetItemCount", "root")
Local $text = ControlTreeView("T", "", $tree, "GetText", "root|child")
Local $by_index = ControlTreeView("T", "", $tree, "GetText", "#1")
ControlTreeView("T", "", $tree, "Select", "root|child")
Local $selected = ControlTreeView("T", "", $tree, "GetSelected")
Local $exists = ControlTreeView("T", "", $tree, "Exists", "root|child")
Local $missing = ControlTreeView("T", "", $tree, "Exists", "nope")
Return $roots & ":" & $kids & ":" & $text & ":" & $by_index & ":" & $selected & ":" & $exists & ":" & $missing
"##;
    assert_eq!(text(win10(), body), "2:1:child:other:child:1:0");
}

#[test]
fn control_command_and_sendmsg_change_the_model() {
    let body = r#"
GUICreate("T", 300, 200)
Local $combo = GUICtrlCreateCombo("", 0, 0)
GUICtrlSetData($combo, "a|b")
Local $count = ControlCommand("T", "", $combo, "GetCount")
ControlCommand("T", "", $combo, "SelectString", "b")
Local $current = ControlCommand("T", "", $combo, "GetCurrentSelection")
Local $check = GUICtrlCreateCheckbox("c", 0, 30)
ControlCommand("T", "", $check, "Check")
Local $checked = ControlCommand("T", "", $check, "IsChecked")
Local $list = GUICtrlCreateListView("a", 0, 60, 100, 50)
GUICtrlCreateListViewItem("row1", $list)
GUICtrlCreateListViewItem("row2", $list)
Local $before = GUICtrlSendMsg($list, 0x1004, 0, 0)
GUICtrlSendMsg($list, 0x1009, 0, 0)
Local $after = GUICtrlSendMsg($list, 0x1004, 0, 0)
Local $unknown = GUICtrlSendMsg($list, 0x1234, 0, 0)
Return $count & ":" & $current & ":" & $checked & ":" & $before & ":" & $after & ":" & $unknown & ":" & @error
"#;
    assert_eq!(text(win10(), body), "2:b:1:2:0:0:1");
}

/// A backend that records whether each control it was shown is visible.
#[derive(Clone, Default)]
struct VisibilityBackend {
    log: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
}

impl GuiBackend for VisibilityBackend {
    fn on_control(&mut self, control: &Control) {
        self.log
            .borrow_mut()
            .push(format!("{}:{}", control.text, control.is_visible()));
    }
}

#[test]
fn an_expanded_tree_item_does_not_say_so_when_it_is_read() {
    // The probe against the official interpreter: a fresh item answers
    // `$GUI_SHOW | $GUI_ENABLE` to `GUICtrlGetState`, while `GUICtrlRead` gives
    // its own state — 0 even after `$GUI_EXPAND`, because expanding a node is
    // applied to the real tree rather than remembered in the state word.
    let body = r#"
GUICreate("T", 300, 200)
Local $tree = GUICtrlCreateTreeView(0, 0, 200, 150)
Local $root = GUICtrlCreateTreeViewItem("root", $tree)
Local $child = GUICtrlCreateTreeViewItem("child", $root)
Local $fresh = GUICtrlGetState($root)
GUICtrlSetState($child, 1024)
Local $expanded = GUICtrlRead($child)
GUICtrlSetState($child, 256)
Local $focused = GUICtrlRead($child)
GUICtrlSetState($child, 512)
Local $bold = GUICtrlRead($child)
GUICtrlSetState($child, 0)
Local $plain = GUICtrlRead($child)
Return $fresh & ":" & $expanded & ":" & $focused & ":" & $bold & ":" & $plain
"#;
    assert_eq!(text(win10(), body), "80:0:256:768:256");
}

#[test]
fn a_tabitem_has_no_readable_value_of_its_own() {
    let body = r#"
GUICreate("T", 300, 200)
Local $tab = GUICtrlCreateTab(0, 0, 200, 150)
Local $page = GUICtrlCreateTabItem("one")
GUICtrlCreateTabItem("")
Local $read = GUICtrlRead($page)
Local $advanced = GUICtrlRead($page, 1)
Local $state = GUICtrlGetState($page)
GUICtrlSetData($page, "renamed")
Local $after = GUICtrlRead($page)
Return $read & ":" & $advanced & ":" & $state & ":" & $after
"#;
    assert_eq!(text(win10(), body), "::80:");
}

#[test]
fn an_item_update_never_grows_the_row() {
    // `"|||"` leaves a three-column row alone: an empty field only erases a cell
    // that is there. `"x||z"` writes the first and third, the official way.
    let body = r#"
GUICreate("T", 300, 200)
Local $list = GUICtrlCreateListView("a|b|c", 0, 0, 200, 100)
Local $item = GUICtrlCreateListViewItem("1|2|3", $list)
GUICtrlSetData($item, "|||")
Local $seps = GUICtrlRead($item)
GUICtrlSetData($item, "x||z")
Local $sparse = GUICtrlRead($item)
GUICtrlSetData($item, "x|")
Local $erase = GUICtrlRead($item)
Return $seps & ":" & $sparse & ":" & $erase
"#;
    assert_eq!(text(win10(), body), "1|2|3|:x|2|z|:x||z|");
}

/// A backend that records the drawing commands a graphic control collects.
#[derive(Clone, Default)]
struct DrawBackend {
    log: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
}

impl GuiBackend for DrawBackend {
    fn on_control(&mut self, control: &Control) {
        if control.kind == ControlKind::Graphic {
            self.log.borrow_mut().push(format!("{:?}", control.draw));
        }
    }
}

#[test]
fn graphic_types_are_the_official_ones() {
    // The type numbers are `GUIConstantsEx.au3`'s: even, in the order the help
    // page lists them. `$GUI_GR_BEZIER` really is 4, not 3 or 5.
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let body = r#"
GUICreate("T", 200, 200)
Local $g = GUICtrlCreateGraphic(0, 0, 100, 100)
GUICtrlSetGraphic($g, 8, 0xFF0000, 0x00FF00)
GUICtrlSetGraphic($g, 10, 5, 5, 20, 30)
GUICtrlSetGraphic($g, 6, 50, 50)
GUICtrlSetGraphic($g, 2, 60, 60)
GUICtrlSetGraphic($g, 4, 70, 70, 72, 72, 74, 74)
GUICtrlSetGraphic($g, 14, 40, 40, 10, 0, 90)
GUICtrlSetGraphic($g, 16, 1, 2)
GUICtrlSetGraphic($g, 24, 3)
Return "done"
"#;
    let emu = win10().with_gui_backend(Box::new(DrawBackend { log: log.clone() }));
    assert_eq!(text(emu, body), "done");
    let seen = log.borrow().join("|");
    assert!(seen.contains("SetColor(16711680)"), "{seen}");
    assert!(seen.contains("SetBkColor(65280)"), "{seen}");
    assert!(seen.contains("Rect { x: 5, y: 5, w: 20, h: 30 }"), "{seen}");
    assert!(seen.contains("Line { x1: 50, y1: 50, x2: 60, y2: 60 }"), "{seen}");
    assert!(
        seen.contains(
            "Bezier { x1: 60, y1: 60, x2: 72, y2: 72, x3: 74, y3: 74, x4: 70, y4: 70 }"
        ),
        "{seen}"
    );
    assert!(
        seen.contains("Pie { x: 40, y: 40, r: 10, start: 0, sweep: 90 }"),
        "{seen}"
    );
    assert!(seen.contains("Dot { x: 1, y: 2 }"), "{seen}");
    assert!(seen.contains("SetWidth(3)"), "{seen}");
}

#[test]
fn controls_follow_a_window_resize_the_way_their_docking_asks() {
    // `$GUI_DOCKAUTO` scales with the window, `$GUI_DOCKRIGHT` keeps the
    // control's right edge where it was, and a control nobody touched keeps
    // both its place and its size.
    let body = r#"
GUICreate("T", 300, 200)
Local $stay = GUICtrlCreateLabel("stays", 10, 10, 50, 20)
Local $right = GUICtrlCreateLabel("right", 10, 50, 50, 20)
GUICtrlSetResizing($right, 4)
Local $bar = GUICtrlCreateProgress(10, 100, 100, 20)
WinMove("T", "", -1, -1, 500, 300)
Local $a = ControlGetPos("T", "", $stay)
Local $b = ControlGetPos("T", "", $right)
Local $c = ControlGetPos("T", "", $bar)
Return $a[0] & "," & $a[1] & "," & $a[2] & "," & $a[3] & ":" & $b[0] & "," & $b[1] & ":" & $c[0] & "," & $c[1] & "," & $c[2] & "," & $c[3]
"#;
    assert_eq!(text(win10(), body), "10,10,50,20:210,50:17,150,167,30");
}

#[test]
fn a_tab_page_hides_the_controls_of_the_pages_nobody_selected() {
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let body = r#"
GUICreate("T", 300, 200)
Local $tab = GUICtrlCreateTab(0, 0, 200, 150)
Local $one = GUICtrlCreateTabItem("one")
Local $first = GUICtrlCreateLabel("first", 10, 30)
Local $two = GUICtrlCreateTabItem("two")
Local $second = GUICtrlCreateLabel("second", 10, 30)
GUICtrlCreateTabItem("")
GUICtrlSetState($two, 16)
Return "done"
"#;
    let emu = win10().with_gui_backend(Box::new(VisibilityBackend { log: log.clone() }));
    assert_eq!(text(emu, body), "done");
    // `GUICtrlGetState` cannot show this — the official interpreter answers the
    // same 0x50 for both pages — so what a backend is told is the check.
    let seen = log.borrow();
    assert!(seen.iter().any(|line| line == "second:false"), "{seen:?}");
    assert!(seen.iter().any(|line| line == "first:true"), "{seen:?}");
    // Showing the second page swaps which one is on screen.
    assert!(seen.iter().any(|line| line == "second:true"), "{seen:?}");
    assert!(seen.iter().any(|line| line == "first:false"), "{seen:?}");
}

#[test]
fn a_list_or_combo_appends_until_it_is_told_to_start_over() {
    let body = r#"
GUICreate("T", 200, 150)
Local $combo = GUICtrlCreateCombo("", 0, 0)
GUICtrlSetData($combo, "a|b", "b")
Local $selected = GUICtrlRead($combo)
GUICtrlSetData($combo, "|c")
Local $after_reset = GUICtrlRead($combo)
Return $selected & ":" & $after_reset
"#;
    assert_eq!(text(win10(), body), "b:c");
}

#[test]
fn a_checkbox_reads_back_its_three_states() {
    let body = r#"
GUICreate("T", 200, 150)
Local $check = GUICtrlCreateCheckbox("c", 0, 0)
Local $unchecked = GUICtrlRead($check)
GUICtrlSetState($check, 1)
Local $checked = GUICtrlRead($check)
; A plain check box has no third state: the official interpreter answers
; "checked" for `$GUI_INDETERMINATE` unless the box is a three-state one.
GUICtrlSetState($check, 2)
Local $indeterminate = GUICtrlRead($check)
Local $advanced = GUICtrlRead($check, 1)
GUICtrlSetState($check, 4)
Local $unchecked_again = GUICtrlRead($check)
Return $unchecked & ":" & $checked & ":" & $indeterminate & ":" & $advanced & ":" & $unchecked_again
"#;
    assert_eq!(text(win10(), body), "4:1:1:c:4");
}

#[test]
fn a_three_state_checkbox_does_have_an_indeterminate_read() {
    let body = r#"
GUICreate("T", 200, 150)
Local $check = GUICtrlCreateCheckbox("c", 0, 0, 0, 0, 0x0006)
GUICtrlSetState($check, 2)
GUICtrlSetState($check, 1)
Local $checked = GUICtrlRead($check)
GUICtrlSetState($check, 2)
Local $indeterminate = GUICtrlRead($check)
Return $checked & ":" & $indeterminate
"#;
    assert_eq!(text(win10(), body), "1:2");
}

#[test]
fn gui_control_messages_answer_edit_and_listview() {
    let body = r#"
GUICreate("T", 100, 100)
Local $edit = GUICtrlCreateEdit("", 0, 0, 100, 50)
GUICtrlSetData($edit, "a" & @CRLF & "b" & @CRLF & "c")
Local $lines = GUICtrlSendMsg($edit, 0x00BA, 0, 0)
Local $list = GUICtrlCreateListView("", 0, 60, 100, 40)
; `GUICtrlSetData` on a `ListView` does *not* add rows — the official
; interpreter answers 0 for the count after two of those calls — so the rows
; come from `GUICtrlCreateListViewItem`.
Local $ignored = GUICtrlSendMsg($list, 0x1004, 0, 0)
GUICtrlSetData($list, "row1")
GUICtrlCreateListViewItem("row1", $list)
GUICtrlCreateListViewItem("row2", $list)
Local $count = GUICtrlSendMsg($list, 0x1004, 0, 0)
Local $unknown = GUICtrlSendMsg($list, 0x1234, 0, 0)
Local $err = @error
Return $lines & ":" & $count & ":" & $unknown & ":" & $err
"#;
    assert_eq!(text(win10(), body), "3:2:0:1");
}

/// A backend that answers the dialogs itself and records the feedback windows
/// a script opened.
#[derive(Clone, Default)]
struct DialogBackend {
    log: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
}

impl GuiBackend for DialogBackend {
    fn message_box(&mut self, flags: i64, title: &str, text: &str, _timeout: i64) -> Option<i64> {
        self.log
            .borrow_mut()
            .push(format!("msgbox:{flags}:{title}:{text}"));
        Some(7)
    }

    fn input_box(
        &mut self,
        title: &str,
        _prompt: &str,
        _default: &str,
        password: bool,
        _timeout: i64,
    ) -> Option<Option<String>> {
        self.log.borrow_mut().push(format!("input:{title}:{password}"));
        Some(Some("typed".to_string()))
    }

    fn file_dialog(
        &mut self,
        kind: i64,
        title: &str,
        initial: &str,
        filter: &str,
        default: &str,
        options: i64,
    ) -> Option<Option<String>> {
        self.log.borrow_mut().push(format!(
            "file:{kind}:{title}:{initial}:{filter}:{default}:{options}"
        ));
        Some(Some("C:\\picked.txt".to_string()))
    }

    fn splash(&mut self, splash: &Splash, off: bool) -> bool {
        self.log
            .borrow_mut()
            .push(format!("splash:{}:{}", splash.text, off));
        true
    }

    fn progress(&mut self, progress: &Progress, off: bool) -> bool {
        self.log.borrow_mut().push(format!(
            "progress:{}:{}:{}:{}",
            progress.text, progress.sub, progress.percent, off
        ));
        true
    }

    fn tooltip_window(&mut self, text: &str, x: i32, y: i32) -> bool {
        self.log.borrow_mut().push(format!("tooltip:{text}:{x}:{y}"));
        true
    }
}

#[test]
fn a_backend_can_answer_the_dialogs_and_show_the_feedback_windows() {
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let body = r#"
Local $answer = MsgBox(4, "Question", "Go on?")
Local $typed = InputBox("Ask", "Name", "default", "P")
Local $file = FileOpenDialog("Open", "C:\temp", "Text (*.txt)")
SplashTextOn("Splash", "working", 100, 50)
ProgressOn("Progress", "main", "sub", 10, 10)
ProgressSet(50, "half")
ToolTip("hello", 5, 6)
ProgressOff()
ToolTip("")
SplashOff()
Return $answer & ":" & $typed & ":" & $file
"#;
    let emu = win10().with_gui_backend(Box::new(DialogBackend { log: log.clone() }));
    assert_eq!(text(emu, body), "7:typed:C:\\picked.txt");
    let seen = log.borrow().join("|");
    assert!(seen.contains("msgbox:4:Question:Go on?"), "{seen}");
    assert!(seen.contains("input:Ask:true"), "{seen}");
    assert!(
        seen.contains("file:0:Open:C:\\temp:Text (*.txt)::0"),
        "{seen}"
    );
    assert!(seen.contains("splash:working:false"), "{seen}");
    // `ProgressSet`'s subtext comes before its main text, so the main one is
    // the label `ProgressOn` set.
    assert!(seen.contains("progress:main:half:50:false"), "{seen}");
    assert!(seen.contains("tooltip:hello:5:6"), "{seen}");
    assert!(seen.contains("progress:main:half:50:true"), "{seen}");
    assert!(seen.contains("splash:working:true"), "{seen}");
}

#[test]
fn gui_dialogs_use_scripted_answers_and_fail_otherwise() {
    let body = r#"
Local $m = MsgBox(0, "t", "x")
Local $i = InputBox("t", "prompt", "default")
Local $f = FileOpenDialog("open", "", "All (*.*)")
Local $c = FileOpenDialog("open", "", "All (*.*)")
Local $cerr = @error
Return $m & ":" & $i & ":" & $f & ":" & $c & ":" & $cerr
"#;
    let emu = win10()
        .with_gui_events(vec![GuiEvent::Dialog(2)])
        .with_gui_answers(vec!["typed".to_string(), "C:\\file.txt".to_string()]);
    assert_eq!(text(emu, body), "2:typed:C:\\file.txt::1");
}

#[test]
fn control_functions_and_control_click_events() {
    let body = r#"
GUICreate("T", 100, 100)
GUICtrlCreateLabel("hello", 0, 0)
Local $txt = ControlGetText("T", "hello")
ControlSetText("T", "hello", "world")
Local $txt2 = ControlGetText("T", "world")
ControlClick("T", "world")
Local $msg = GUIGetMsg()
Return $txt & ":" & $txt2 & ":" & $msg
"#;
    assert_eq!(text(win10(), body), "hello:world:1");
}

#[derive(Clone, Default)]
struct Recorder {
    log: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
}

impl GuiBackend for Recorder {
    fn on_window(&mut self, window: &Window) {
        self.log.borrow_mut().push(format!("win:{}", window.title));
    }
    fn on_control(&mut self, control: &Control) {
        self.log
            .borrow_mut()
            .push(format!("ctrl:{}:{}", control.id, control.text));
    }
}

#[test]
fn a_custom_backend_sees_model_updates() {
    let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let emu = win10().with_gui_backend(Box::new(Recorder { log: log.clone() }));
    let body = r#"
GUICreate("Main", 100, 100)
GUICtrlCreateLabel("hello", 0, 0)
Return 1
"#;
    assert_eq!(text(emu, body), "1");
    let entries = log.borrow().clone();
    assert!(entries.iter().any(|e| e == "win:Main"), "{entries:?}");
    assert!(
        entries.iter().any(|e| e.ends_with(":hello")),
        "{entries:?}"
    );
}

/// A backend that plays the part of a live window: it only reports an edit
/// once the control exists, which is when a window could produce one.
#[derive(Default)]
struct TypingBackend {
    pending: Vec<GuiUpdate>,
    input_created: bool,
}

impl GuiBackend for TypingBackend {
    fn on_control(&mut self, control: &Control) {
        if control.kind == autoitv3_platform::winemu::ControlKind::Input {
            self.input_created = true;
        }
    }
    fn take_updates(&mut self) -> Vec<GuiUpdate> {
        if self.input_created {
            std::mem::take(&mut self.pending)
        } else {
            Vec::new()
        }
    }
}

/// A live-window backend that also records every value it is shown.
#[derive(Default)]
struct RecordingBackend {
    pending: Vec<GuiUpdate>,
    input_created: bool,
    seen: Rc<RefCell<Vec<(i64, String)>>>,
}

impl GuiBackend for RecordingBackend {
    fn on_control(&mut self, control: &Control) {
        if control.kind == autoitv3_platform::winemu::ControlKind::Input {
            self.input_created = true;
        }
        self.seen
            .borrow_mut()
            .push((control.id, control.text.clone()));
    }
    fn take_updates(&mut self) -> Vec<GuiUpdate> {
        if self.input_created {
            std::mem::take(&mut self.pending)
        } else {
            Vec::new()
        }
    }
}

/// A backend that reports one window resize once the window exists.
#[derive(Default)]
struct ResizingBackend {
    pending: Vec<GuiUpdate>,
    window_created: bool,
    seen: Rc<RefCell<Vec<(i64, i32, i32)>>>,
}

impl GuiBackend for ResizingBackend {
    fn on_window(&mut self, window: &Window) {
        self.window_created = true;
        self.seen
            .borrow_mut()
            .push((window.handle, window.width, window.height));
    }
    fn take_updates(&mut self) -> Vec<GuiUpdate> {
        if self.window_created {
            std::mem::take(&mut self.pending)
        } else {
            Vec::new()
        }
    }
}

#[test]
fn win_set_state_takes_the_show_flags_a_script_uses() {
    // `@SW_*`, not the `WinGetState` bits: @SW_MINIMIZE is 6.
    assert_eq!(text(win10(), "Return @SW_MINIMIZE"), "6");
    assert_eq!(text(win10(), "Return @SW_SHOW"), "5");
    assert_eq!(text(win10(), "Return @SW_RESTORE"), "9");

    let body = r#"
GUICreate("T", 380, 170)
WinSetState("T", "", @SW_MINIMIZE)
Local $minimized = WinGetState("T")
WinSetState("T", "", @SW_RESTORE)
Local $restored = WinGetState("T")
WinSetState("T", "", @SW_MAXIMIZE)
Local $maximized = WinGetState("T")
WinSetState("T", "", @SW_HIDE)
Local $hidden = WinGetState("T")
Return $minimized & "/" & $restored & "/" & $maximized & "/" & $hidden
"#;
    // visible(2) + enabled(4) + active(8) + exists(1) = 15, plus the state bit.
    // Hidden: exists(1) + enabled(4) + active(8), the visible bit cleared.
    assert_eq!(text(win10(), body), "31/15/47/13");
}

#[test]
fn a_script_move_reaches_the_backend() {
    // The live window only learns about `WinMove` through `on_window`, so the
    // backend has to be told.
    let seen = Rc::new(RefCell::new(Vec::new()));
    let emu = win10().with_gui_backend(Box::new(SizingBackend { seen: seen.clone() }));
    let body = r#"
GUICreate("T", 380, 170)
WinMove("T", "", 120, 90, 300, 260)
Return WinGetClientSize("T")[0] & "x" & WinGetClientSize("T")[1]
"#;
    assert_eq!(text(emu, body), "300x260");
    assert!(
        seen.borrow().iter().any(|(_, w, h)| *w == 300 && *h == 260),
        "WinMove never reached the backend: {:?}",
        seen.borrow()
    );
}

/// A backend that records the size of every window it is shown.
#[derive(Default)]
struct SizingBackend {
    seen: Rc<RefCell<Vec<(i64, i32, i32)>>>,
}

impl GuiBackend for SizingBackend {
    fn on_window(&mut self, window: &Window) {
        self.seen
            .borrow_mut()
            .push((window.handle, window.width, window.height));
    }
}

/// A backend that reports a user's window-state change (double-clicking a
/// title bar, clicking a taskbar button) once the window is ready for it.
struct StateBackend {
    pending: Vec<GuiUpdate>,
    /// Hand the change over once the window is in this state.
    release_after: autoitv3_platform::winemu::WindowState,
    ready: bool,
    seen: Rc<RefCell<Vec<(i64, autoitv3_platform::winemu::WindowState)>>>,
}

impl Default for StateBackend {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            release_after: autoitv3_platform::winemu::WindowState::Normal,
            ready: false,
            seen: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

impl GuiBackend for StateBackend {
    fn on_window(&mut self, window: &Window) {
        self.seen.borrow_mut().push((window.handle, window.state));
        if window.visible && window.state == self.release_after {
            self.ready = true;
        }
    }
    fn take_updates(&mut self) -> Vec<GuiUpdate> {
        if self.ready {
            std::mem::take(&mut self.pending)
        } else {
            Vec::new()
        }
    }
}

#[test]
fn a_user_move_reaches_wingetpos() {
    // Dragging a window's title bar is a user move: the script sees it through
    // WinGetPos (AutoIt surfaces no message for it).
    let seen = Rc::new(RefCell::new(Vec::new()));
    let emu = win10().with_gui_backend(Box::new(ResizingBackend {
        pending: vec![GuiUpdate::Move {
            handle: 0x1_0000,
            x: 300,
            y: 200,
        }],
        window_created: false,
        seen: seen.clone(),
    }));
    let body = r#"
GUICreate("T", 380, 170, 10, 20)
GUISetState()
Local $p = WinGetPos("T")
Return $p[0] & "," & $p[1]
"#;
    assert_eq!(text(emu, body), "300,200");
    assert!(
        seen.borrow().iter().any(|(_, w, h)| *w == 380 && *h == 170),
        "the window was never shown to the backend: {:?}",
        seen.borrow()
    );
}

#[test]
fn a_user_state_change_reaches_the_script() {
    use autoitv3_platform::winemu::WindowState;

    // `$GUI_EVENT_MINIMIZE` is -4, `$GUI_EVENT_RESTORE` -5,
    // `$GUI_EVENT_MAXIMIZE` -6. WinGetState keeps its own bit vocabulary:
    // exists(1) + visible(2) + enabled(4) + active(8) + minimised(16) or
    // maximised(32).
    fn case(state: WindowState, release_after: WindowState, script: &str) -> String {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let emu = win10().with_gui_backend(Box::new(StateBackend {
            pending: vec![GuiUpdate::SetWindowState { handle: 0x1_0000, state }],
            release_after,
            ready: false,
            seen: seen.clone(),
        }));
        let result = text(emu, script);
        assert!(
            seen.borrow().iter().any(|(_, seen)| *seen == state),
            "the new state never reached the backend: {:?}",
            seen.borrow()
        );
        result
    }

    const MINIMISE_FIRST: &str = "WinSetState(\"T\", \"\", @SW_MINIMIZE)\n";
    let body = |first: &str| {
        format!(
            "GUICreate(\"T\", 380, 170)\nGUISetState()\n{first}Local $msg = GUIGetMsg()\nReturn WinGetState(\"T\") & \" msg \" & $msg"
        )
    };

    assert_eq!(case(WindowState::Minimized, WindowState::Normal, &body("")), "31 msg -4");
    assert_eq!(case(WindowState::Maximized, WindowState::Normal, &body("")), "47 msg -6");
    assert_eq!(
        case(WindowState::Normal, WindowState::Minimized, &body(MINIMISE_FIRST)),
        "15 msg -5"
    );
}

/// A backend with a desktop size, which can switch to a bigger one after a
/// few frames — the way a user resizes a live window's viewport.
struct DesktopBackend {
    sizes: Vec<(i32, i32)>,
    /// Move to the next size once this many window updates have been seen.
    change_after: usize,
    seen: std::cell::Cell<usize>,
}

impl DesktopBackend {
    fn fixed(size: (i32, i32)) -> Self {
        Self {
            sizes: vec![size],
            change_after: usize::MAX,
            seen: std::cell::Cell::new(0),
        }
    }
}

impl GuiBackend for DesktopBackend {
    fn on_window(&mut self, _window: &Window) {
        self.seen.set(self.seen.get() + 1);
    }
    fn desktop_size(&self) -> Option<(i32, i32)> {
        let switches = self.seen.get() / self.change_after.max(1);
        Some(self.sizes[switches.min(self.sizes.len() - 1)])
    }
}

#[test]
fn the_desktop_comes_from_the_backend() {
    // No backend desktop: the emulated display mode.
    assert_eq!(text(win10(), "Return @DesktopWidth"), "1024");
    assert_eq!(text(win10(), "Return @DesktopHeight"), "768");

    // A live window's viewport (or an offscreen canvas) is the desktop.
    let emu = win10().with_gui_backend(Box::new(DesktopBackend::fixed((800, 600))));
    assert_eq!(
        text(emu, "Return @DesktopWidth & \"x\" & @DesktopHeight"),
        "800x600"
    );
}

#[test]
fn maximising_takes_the_desktop_rectangle_and_restoring_gives_it_back() {
    let emu = win10().with_gui_backend(Box::new(DesktopBackend::fixed((800, 600))));
    let body = r#"
GUICreate("T", 380, 170, 10, 20)
WinSetState("T", "", @SW_MAXIMIZE)
Local $max = WinGetPos("T")
WinSetState("T", "", @SW_RESTORE)
Local $back = WinGetPos("T")
Return $max[0] & "," & $max[1] & " " & $max[2] & "x" & $max[3] & " -> " & $back[0] & "," & $back[1] & " " & $back[2] & "x" & $back[3]
"#;
    // Windows puts a maximised window at the top-left of the desktop and
    // reports the desktop size, then restores the normal placement.
    assert_eq!(text(emu, body), "0,0 800x600 -> 10,20 380x170");
}

#[test]
fn a_maximised_window_follows_a_desktop_change() {
    // The first desktop, then a bigger one: the window has to follow, and the
    // script hears about the resize.
    let mut backend = DesktopBackend::fixed((800, 600));
    backend.sizes.push((1280, 720));
    backend.change_after = 3;
    let emu = win10().with_gui_backend(Box::new(backend));
    let body = r#"
GUICreate("T", 380, 170)
GUISetState()
WinSetState("T", "", @SW_MAXIMIZE)
Local $msg = GUIGetMsg()
Local $p = WinGetPos("T")
Return $p[2] & "x" & $p[3] & " msg " & $msg
"#;
    // $GUI_EVENT_RESIZED is -12.
    assert_eq!(text(emu, body), "1280x720 msg -12");
}

#[test]
fn a_user_resize_reaches_wingetpos_and_guigetmsg() {
    // What a live window sends after the user drags an edge.
    let seen = Rc::new(RefCell::new(Vec::new()));
    let emu = win10().with_gui_backend(Box::new(ResizingBackend {
        pending: vec![GuiUpdate::Resize {
            handle: 0x1_0000,
            width: 500,
            height: 400,
        }],
        window_created: false,
        seen: seen.clone(),
    }));
    let body = r#"
GUICreate("T", 380, 170)
Local $pos = WinGetPos("T")
Local $client = WinGetClientSize("T")
Local $msg = GUIGetMsg()
Return $pos[2] & "x" & $pos[3] & " client " & $client[0] & "x" & $client[1] & " msg " & $msg
"#;
    assert_eq!(text(emu, body), "500x400 client 500x400 msg -12");
    assert!(
        seen.borrow().iter().any(|(_, w, h)| *w == 500 && *h == 400),
        "the model's new size never reached the backend: {:?}",
        seen.borrow()
    );
}

#[test]
fn an_applied_edit_is_reported_back_to_the_backend() {
    // The window sends the edit; the model has to answer with the new value,
    // or the next frame redraws the old text and the typed characters vanish.
    let seen = Rc::new(RefCell::new(Vec::new()));
    let emu = win10().with_gui_backend(Box::new(RecordingBackend {
        pending: vec![GuiUpdate::SetText {
            id: 1,
            text: "typed".to_string(),
        }],
        input_created: false,
        seen: seen.clone(),
    }));
    let body = r#"
GUICreate("T", 120, 80)
Local $e = GUICtrlCreateInput("start", 0, 0)
Return GUICtrlRead($e)
"#;
    assert_eq!(text(emu, body), "typed");
    let seen = seen.borrow();
    assert!(
        seen.iter().any(|(id, text)| *id == 1 && text == "typed"),
        "backend never saw the applied edit: {seen:?}"
    );
}

#[test]
fn edits_from_a_live_window_reach_guictrlread() {
    // What the window thread would queue after the user types into the Input.
    let emu = win10().with_gui_backend(Box::new(TypingBackend {
        pending: vec![GuiUpdate::SetText {
            id: 1,
            text: "typed".to_string(),
        }],
        input_created: false,
    }));
    let body = r#"
GUICreate("T", 120, 80)
Local $e = GUICtrlCreateInput("start", 0, 0)
Return GUICtrlRead($e)
"#;
    assert_eq!(text(emu, body), "typed");
}


#[test]
fn emulated_modules_round_trip() {
    let body = r#"
Local $h = DllCall("kernel32.dll", "ptr", "LoadLibraryW", "wstr", "kernel32.dll")
Local $p = DllCall("kernel32.dll", "ptr", "GetProcAddress", "ptr", $h[0], "wstr", "GetCurrentProcessId")
Local $t = DllStructCreate("wchar buf[260]")
DllCall("kernel32.dll", "dword", "GetModuleFileNameW", "ptr", $h[0], "ptr", DllStructGetPtr($t), "dword", 260)
Local $bad = DllCall("kernel32.dll", "ptr", "GetProcAddress", "ptr", 9999, "wstr", "Nope")
Local $e = @error
Return $h[0] & ":" & ($p[0] > 0) & ":" & DllStructGetData($t, 1) & ":" & $bad & ":" & $e
"#;
    let got = run(win10(), body).to_autoit_string();
    assert!(
        got.starts_with(r"1:True:C:\Windows\kernel32.dll:0:1"),
        "got {got}"
    );
}

#[test]
fn emulated_memory_apis_allocate_and_round_trip() {
    let body = r#"
Local $mem = DllCall("kernel32.dll", "ptr", "VirtualAlloc", "ptr", 0, "ulong_ptr", 16, "dword", 0x3000, "dword", 4)
Local $src = DllStructCreate("byte buf[4]")
DllStructSetData($src, 1, Binary("0x01020304"))
DllCall("kernel32.dll", "none", "RtlMoveMemory", "ptr", $mem[0], "ptr", DllStructGetPtr($src), "ulong_ptr", 4)
Local $out = DllStructCreate("byte out[4]", $mem[0])
Return DllStructGetData($out, 1) = Binary("0x01020304")
"#;
    assert_eq!(
        run(win10(), body).to_autoit_string(),
        "True"
    );
}

#[test]
fn emulated_file_apis_read_a_seeded_sandbox_file() {
    let emu = WindowsEmulation::new().with_file(r"C:\probe\data.txt", b"payload".to_vec());
    let body = r#"
Local $h = DllCall("kernel32.dll", "ptr", "CreateFileW", "wstr", "C:\probe\data.txt", "dword", 0x80000000, "dword", 0, "ptr", 0)
Local $buf = DllStructCreate("byte buf[16]")
Local $got = DllStructCreate("dword read")
Local $r = DllCall("kernel32.dll", "bool", "ReadFile", "ptr", $h[0], "ptr", DllStructGetPtr($buf), "dword", 16, "ptr", DllStructGetPtr($got), "ptr", 0)
DllCall("kernel32.dll", "bool", "CloseHandle", "ptr", $h[0])
Return ($r[0] = 1) & ":" & DllStructGetData($got, 1) & ":" & BinaryMid(DllStructGetData($buf, 1), 1, 7)
"#;
    assert_eq!(run(emu, body).to_autoit_string(), "True:7:0x7061796C6F6164");
}

#[test]
fn emulated_file_apis_write_and_read_back() {
    let body = r#"
Local $h = DllCall("kernel32.dll", "ptr", "CreateFileW", "wstr", "C:\probe\out.txt", "dword", 0x40000000, "dword", 0, "ptr", 0)
Local $buf = DllStructCreate("char buf[5]")
DllStructSetData($buf, 1, "hello")
Local $w = DllCall("kernel32.dll", "bool", "WriteFile", "ptr", $h[0], "ptr", DllStructGetPtr($buf), "dword", 5, "ptr", 0, "ptr", 0)
DllCall("kernel32.dll", "bool", "CloseHandle", "ptr", $h[0])
Local $h2 = DllCall("kernel32.dll", "ptr", "CreateFileW", "wstr", "C:\probe\out.txt", "dword", 0x80000000, "dword", 0, "ptr", 0)
Local $size = DllCall("kernel32.dll", "dword", "GetFileSize", "ptr", $h2[0], "ptr", 0)
Local $buf2 = DllStructCreate("char buf[5]")
DllCall("kernel32.dll", "bool", "ReadFile", "ptr", $h2[0], "ptr", DllStructGetPtr($buf2), "dword", 5, "ptr", 0, "ptr", 0)
DllCall("kernel32.dll", "bool", "CloseHandle", "ptr", $h2[0])
Return ($w[0] = 1) & ":" & $size[0] & ":" & DllStructGetData($buf2, 1)
"#;
    assert_eq!(run(win10(), body).to_autoit_string(), "True:5:hello");
}

#[test]
fn emulated_crt_strings_work_over_struct_memory() {
    let body = r#"
Local $t = DllStructCreate("wchar s[8]")
DllStructSetData($t, 1, "abc")
Local $n = DllCall("kernel32.dll", "int", "lstrlenW", "ptr", DllStructGetPtr($t))
Local $d = DllStructCreate("wchar s[8]")
DllCall("kernel32.dll", "ptr", "lstrcpyW", "ptr", DllStructGetPtr($d), "ptr", DllStructGetPtr($t))
Return $n[0] & ":" & DllStructGetData($d, 1)
"#;
    assert_eq!(run(win10(), body).to_autoit_string(), "3:abc");
}

#[test]
fn enumwindows_drives_a_registered_callback() {
    let emu = WindowsEmulation::new().with_scripted_windows(vec![0x1001, 0x1002, 0x1003]);
    let src = r#"
Global $g_Calls = 0
Global $g_Last = 0

Func F()
    Local $cb = DllCallbackRegister("OnWindow", "int", "int;int")
    Local $r = DllCall("user32.dll", "int", "EnumWindows", "ptr", $cb, "int", 42)
    Return $g_Calls & ":" & $g_Last & ":" & $r[0]
EndFunc

Func OnWindow($hwnd, $lparam)
    $g_Calls += 1
    $g_Last = $hwnd
    Return 1
EndFunc
"#;
    // The callback really runs — once per scripted handle, with the handle as
    // the first argument and the EnumWindows lparam as the second.
    let prog = autoitv3_ast::parse(src).expect("parses");
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(Box::new(emu));
    // Execute the top level so the `Global` counters exist.
    rt.run_script().expect("script body");
    assert_eq!(
        rt.call_function("F", vec![]).expect("runs").to_autoit_string(),
        "3:4099:True"
    );
}

// ---------------------------------------------------------------------------
// pseudo COM (the emulation answering ObjCreate for well-known ProgIDs)
// ---------------------------------------------------------------------------

#[test]
fn scripting_dictionary_behaves_like_the_real_one() {
    let emu = WindowsEmulation::new();
    let src = r#"
Global $g_Count = 0
Global $g_Seen = ""

Func F()
    Local $d = ObjCreate("Scripting.Dictionary")
    $d.Add("name", "payload")
    $d.Add("count", 42)
    $g_Count = $d.Count
    $g_Seen = $d.Item("name") & "/" & $d.Exists("count") & "/" & $d.Exists("nope")
    $d.Remove("count")
    Local $keys = $d.Keys
    Return $g_Count & ":" & $g_Seen & ":" & $keys[0] & ":" & $d.Count
EndFunc
"#;
    let prog = autoitv3_ast::parse(src).expect("parses");
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(Box::new(emu));
    rt.run_script().expect("script body");
    assert_eq!(
        rt.call_function("F", vec![]).expect("runs").to_autoit_string(),
        "2:payload/True/False:name:1"
    );
}

#[test]
fn wscript_shell_bridges_to_the_emulated_registry() {
    // In-memory registry: keeps the shared default `.au3_registry` clean.
    let emu = WindowsEmulation::new().with_memory_registry();
    let src = r#"
Global $g_Val = ""

Func F()
    Local $w = ObjCreate("WScript.Shell")
    $w.RegWrite("HKCU\Software\Au3PseudoCom\Answer", "Answer", "REG_SZ")
    $g_Val = $w.RegRead("HKCU\Software\Au3PseudoCom\Answer")
    $w.RegDelete("HKCU\Software\Au3PseudoCom\Answer")
    Local $env = $w.ExpandEnvironmentStrings("%USERNAME%")
    Return $g_Val & ":" & ($env <> "%USERNAME%")
EndFunc
"#;
    let prog = autoitv3_ast::parse(src).expect("parses");
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(Box::new(emu));
    rt.run_script().expect("script body");
    assert_eq!(
        rt.call_function("F", vec![]).expect("runs").to_autoit_string(),
        "Answer:True"
    );
}

#[test]
fn expandenvironmentstrings_maps_windows_identity_names() {
    // `%USERNAME%` / `%COMPUTERNAME%` are the Windows spellings; on a Unix host
    // the emulation maps them onto the host identity rather than leaving them
    // unexpanded (which a Windows-targeted script would not expect).
    let emu = WindowsEmulation::new();
    let src = r#"
Func F()
    Local $w = ObjCreate("WScript.Shell")
    Local $user = $w.ExpandEnvironmentStrings("%USERNAME%")
    Local $host = $w.ExpandEnvironmentStrings("%COMPUTERNAME%")
    Return ($user <> "%USERNAME%") & "|" & ($host <> "%COMPUTERNAME%")
EndFunc
"#;
    let prog = autoitv3_ast::parse(src).expect("parses");
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(Box::new(emu));
    rt.run_script().expect("script body");
    assert_eq!(
        rt.call_function("F", vec![]).expect("runs").to_autoit_string(),
        "True|True"
    );
}

#[test]
fn filesystemobject_answers_pure_path_arithmetic() {
    let emu = WindowsEmulation::new()
        .with_file(r"C:\probe\payload.bin", b"x".to_vec());
    let src = r#"
Global $g_Ext = ""
Global $g_Exists = 0

Func F()
    Local $fso = ObjCreate("Scripting.FileSystemObject")
    $g_Ext = $fso.GetExtensionName("C:\dir\archive.tar.gz")
    $g_Exists = ($fso.FileExists("C:\probe\payload.bin") = 1) And ($fso.FileExists("C:\probe\nope.bin") = 0)
    Local $spec = $fso.GetSpecialFolder(0)
    Return $g_Ext & ":" & ($g_Exists = 1) & ":" & $spec
EndFunc
"#;
    let prog = autoitv3_ast::parse(src).expect("parses");
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(Box::new(emu));
    rt.run_script().expect("script body");
    assert_eq!(
        rt.call_function("F", vec![]).expect("runs").to_autoit_string(),
        r"gz:True:C:\Windows"
    );
}

#[test]
fn isobj_and_objname_agree_with_the_emulated_com() {
    // `ObjCreate` really does hand out an object for the well-known ProgIDs, so
    // `IsObj` has to say `1` and `ObjName` has to echo the ProgID — otherwise a
    // script that guards on `IsObj` takes the "no COM here" path.
    let body = r#"
Local $d = ObjCreate("Scripting.Dictionary")
Local $bad = ObjCreate("NoSuch.ProgID.Here")
Local $e = @error
Return IsObj($d) & ":" & IsObj($bad) & ":" & IsObj(0) & ":" & ObjName($d) & ":" & $e
"#;
    assert_eq!(
        run(win10(), body).to_autoit_string(),
        "1:0:0:Scripting.Dictionary:1"
    );
}

#[test]
fn unknown_progid_still_fails_honestly() {
    let body = r#"
Local $o = ObjCreate("NoSuch.ProgID.Here")
Return @error
"#;
    assert_eq!(run(win10(), body).to_int(), 1);
}


// ---------------------------------------------------------------------------
// The drive map (`C:` -> the host filesystem)
// ---------------------------------------------------------------------------

#[test]
fn the_script_macros_describe_the_script_in_windows_form() {
    // The interpreter never tells the platform which file it is running; the
    // CLI hands it over, and the emulation reports it the way the script's own
    // path arithmetic expects.
    //
    // The fixture is built from the emulated drive's root, not from `temp_dir`:
    // the drive stands for the host root, and a host whose temporary files live
    // on another drive (`D:\tmp`) would put the fixture outside the mapping and
    // legitimately get the host spelling back.
    let root = autoitv3_platform::PathMap::host_root();
    let script = root
        .root()
        .join("tmp")
        .join("demo dir")
        .join("run.au3")
        .to_string_lossy()
        .into_owned();
    // No trailing separator: the help page gives one to `@ScriptDir` only when
    // the script sits in the root of a drive (this one is in `tmp`).
    assert_eq!(
        run_mapped(win10().with_script_path(&script), "Return @ScriptDir").to_autoit_string(),
        r"C:\tmp\demo dir"
    );
    assert_eq!(
        text(win10().with_script_path(&script), "Return @ScriptName"),
        "run.au3"
    );
    assert_eq!(
        run_mapped(win10().with_script_path(&script), "Return @ScriptFullPath").to_autoit_string(),
        r"C:\tmp\demo dir\run.au3"
    );
}

#[test]
fn a_relative_script_path_is_resolved_before_it_is_reported() {
    // `./run.au3` must not report `C:\.`: every join the script does with
    // `@ScriptDir` would land in the drive root.
    let dir = run_mapped(win10().with_script_path("run.au3"), "Return @ScriptDir")
        .to_autoit_string();
    assert!(!dir.contains(r"\."), "got {dir}");
    // Not a drive root, so no trailing separator.
    assert!(!dir.ends_with('\\') || dir.len() == 3, "got {dir}");
    assert_eq!(text(win10().with_script_path("run.au3"), "Return @ScriptName"), "run.au3");
}

#[test]
fn the_drive_map_reads_host_files_through_c_paths() {
    let dir = scratch("drive-map");
    let host = dir.join("data.txt");
    std::fs::write(&host, b"payload").expect("write fixture");
    let win = autoitv3_platform::PathMap::host_root().to_windows(&host);

    let body = format!(
        r#"
Local $p = "{win}"
If Not FileExists($p) Then Return "no-file"
Local $h = FileOpen($p, 16)
If $h = -1 Then Return "no-open"
Local $b = FileRead($h)
FileClose($h)
Return BinaryToString($b)
"#
    );
    assert_eq!(run_mapped(win10(), &body).to_autoit_string(), "payload");
}

#[test]
fn the_drive_map_is_on_by_default_and_can_be_turned_off() {
    assert!(win10().path_map().is_some(), "the map defaults to on");

    let dir = text(
        win10().with_script_path("/tmp/demo.au3").without_path_map(),
        "Return @ScriptDir",
    );
    assert!(!dir.starts_with("C:"), "got {dir}");
    // ... and a drive path is just a filename.
    #[cfg(not(windows))]
    assert_eq!(
        run_mapped(
            win10().without_path_map(),
            r#"Return FileExists("C:\etc\hostname")"#
        )
        .to_int(),
        0
    );
}

#[test]
fn a_custom_drive_root_is_honoured() {
    let dir = scratch("drive-root");
    std::fs::write(dir.join("payload.txt"), b"42").expect("write fixture");
    let emu = win10().with_drive_root(&dir);
    let body = r#"
Local $h = FileOpen("C:\payload.txt", 16)
If $h = -1 Then Return "no-open"
Local $b = FileRead($h)
FileClose($h)
Return BinaryToString($b)
"#;
    assert_eq!(run_mapped(emu, body).to_autoit_string(), "42");
}

#[test]
fn rtl_compute_crc32_matches_the_check_value() {
    // A digest is folded through `ntdll`'s CRC-32 and compared with the short
    // check value a data file advertises; a missing
    // implementation used to make that call fail and the load report an error.
    let body = r#"
Local $t = DllStructCreate("byte[9]")
DllStructSetData($t, 1, "123456789")
Local $r = DllCall("ntdll.dll", "dword", "RtlComputeCrc32", "dword", 0, "ptr", DllStructGetPtr($t), "dword", 9)
If @error Then Return "call-failed"
Return Hex($r[0], 8)
"#;
    assert_eq!(run(win10(), body).to_autoit_string(), "CBF43926");
}

/// A backend that keeps the last state it was shown for every control, so a
/// test can read what the model handed a renderer.
#[derive(Clone, Default)]
struct ModelRecorder {
    seen: std::rc::Rc<std::cell::RefCell<Vec<(i64, String)>>>,
}

impl GuiBackend for ModelRecorder {
    fn on_control(&mut self, control: &Control) {
        self.seen.borrow_mut().push((
            control.id,
            format!(
                "kind={:?} bk={:?} bg={:?} alt={} tip={:?}/{:?}/{}",
                control.kind,
                control.bk_color,
                control.background(),
                control.alternating_rows(),
                control.tip,
                control.tip_title,
                control.tip_options,
            ),
        ));
    }
}

#[test]
fn alternate_listview_colors_and_tip_options_reach_the_model() {
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let emu = win10().with_gui_backend(Box::new(ModelRecorder { seen: seen.clone() }));
    let body = r#"
GUICreate("t", 300, 200)
Local $lv = GUICtrlCreateListView("a|b", 0, 0, 200, 100)
Local $item = GUICtrlCreateListViewItem("1|2", $lv)
GUICtrlSetBkColor($lv, 0x80000000)
GUICtrlSetBkColor($lv, 0x00FF00)
GUICtrlSetBkColor($item, 0x0000FF)
Local $label = GUICtrlCreateLabel("x", 0, 120)
GUICtrlSetTip($label, "text", "title", 2, 3)
Return 1
"#;
    assert_eq!(text(emu, body), "1");
    let seen = seen.borrow();
    // The flag survives the colour that follows it, and the colour a renderer
    // gets is the one without the flag.
    assert!(
        seen.iter().any(|(_, state)| state
            == "kind=ListView bk=Some(2147548928) bg=Some(65280) alt=true tip=\"\"/\"\"/0"),
        "{seen:?}"
    );
    assert!(
        seen.iter().any(|(_, state)| state
            == "kind=ListViewItem bk=Some(255) bg=Some(255) alt=false tip=\"\"/\"\"/0"),
        "{seen:?}"
    );
    assert!(
        seen.iter().any(|(_, state)| state
            == "kind=Label bk=None bg=None alt=false tip=\"text\"/\"title\"/3"),
        "{seen:?}"
    );
}

/// Run a whole script — its own top-level statements and its own functions —
/// against `emu`, and hand back the runtime so a test can read a global.
///
/// [`run`] wraps its body in `Func F()`, which is exactly what an `OnEvent`
/// handler cannot live inside: those have to be top-level functions.
fn run_whole(emu: WindowsEmulation, src: &str) -> Runtime {
    let prog = autoitv3_ast::parse(src).expect("parses");
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(Box::new(CompositePlatform::new(
        "winemu+common",
        vec![Box::new(emu), Box::new(CommonPlatform::new())],
    )));
    rt.set_profile(ExecutionProfile::faithful());
    rt.run_script().expect("no runtime error");
    rt
}

/// A global's value as the script left it.
fn global(rt: &Runtime, name: &str) -> String {
    rt.get_global(name)
        .map(|value| value.to_autoit_string())
        .unwrap_or_default()
}

#[test]
fn on_event_mode_calls_the_registered_function() {
    // The switch is `Opt("GUIOnEventMode", 1)`, the handler is registered per
    // control, and `GUIGetMsg` never sees the click: the function runs instead.
    // An empty function name disables the handler, as the help page says.
    let src = r#"
Global $log = ""
Func Clicked()
    $log = "@GUI_CtrlId=" & @GUI_CtrlId
EndFunc
Opt("GUIOnEventMode", 1)
GUICreate("t", 100, 100)
Local $id = GUICtrlCreateButton("go", 0, 0)
GUICtrlSetOnEvent($id, "Clicked")
GUICtrlSetOnEvent($id, "")
GUICtrlSetOnEvent($id, "Clicked")
Local $msg = GUIGetMsg()
$log = $msg & ":" & $log
"#;
    let emu = win10().with_gui_events(vec![GuiEvent::Control(1)]);
    let rt = run_whole(emu, src);
    assert_eq!(global(&rt, "log"), "0:@GUI_CtrlId=1");
}

#[test]
fn on_event_mode_dispatches_a_window_event() {
    let src = r#"
Global $closed = 0
Global $result = ""
Func Closing()
    $closed = 1
EndFunc
Opt("GUIOnEventMode", 1)
Local $h = GUICreate("t", 100, 100)
GUISetOnEvent(-3, "Closing", $h)
Local $msg = GUIGetMsg()
$result = $msg & ":" & $closed
"#;
    let emu = win10().with_gui_events(vec![GuiEvent::Close(0x10000)]);
    let rt = run_whole(emu, src);
    assert_eq!(global(&rt, "result"), "0:1");
}

#[test]
fn a_window_event_without_a_handler_is_still_returned() {
    // `GUISetOnEvent` registered a minimise handler, not a close one: the close
    // reaches `GUIGetMsg` as usual.
    let src = r#"
Global $result = ""
Func Minimising()
    $result = "called"
EndFunc
Opt("GUIOnEventMode", 1)
Local $h = GUICreate("t", 100, 100)
GUISetOnEvent(-4, "Minimising", $h)
Local $msg = GUIGetMsg()
$result = $msg & ":" & $result
"#;
    let emu = win10().with_gui_events(vec![GuiEvent::Close(0x10000)]);
    let rt = run_whole(emu, src);
    assert_eq!(global(&rt, "result"), "-3:");
}

#[test]
fn without_on_event_mode_the_event_is_still_returned() {
    // The handler is registered but the option is not: the click is an ordinary
    // `GUIGetMsg` answer, and the function is not called.
    let src = r#"
Global $log = ""
Func Clicked()
    $log = "called"
EndFunc
GUICreate("t", 100, 100)
Local $id = GUICtrlCreateButton("go", 0, 0)
GUICtrlSetOnEvent($id, "Clicked")
Local $msg = GUIGetMsg()
$log = $msg & ":" & $log
"#;
    let emu = win10().with_gui_events(vec![GuiEvent::Control(1)]);
    let rt = run_whole(emu, src);
    assert_eq!(global(&rt, "log"), "1:");
}
