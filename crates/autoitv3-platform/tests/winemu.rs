//! Tests for the Windows emulation layer.
//!
//! The suite is `not(windows)` only: on a Windows build the real
//! [`autoitv3_platform::WindowsPlatform`] is installed instead, and these
//! expectations would be answered by the host OS.

#![cfg(not(windows))]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use autoitv3_platform::host_platform_with;
use autoitv3_platform::winemu::{
    Control, FileRegistry, GuiBackend, GuiEvent, GuiUpdate, MemoryRegistry, RegistryData,
    RegistryStore, Window, WindowsArch, WindowsEmulation, WindowsPaths, WindowsVersion,
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
    rt.set_platform(host_platform_with(emu));
    rt.set_profile(profile);
    rt.call_function("F", vec![]).expect("no runtime error")
}

fn text(emu: WindowsEmulation, body: &str) -> String {
    run(emu, body).to_autoit_string()
}

/// The default machine: Windows 10 x64.
fn win10() -> WindowsEmulation {
    WindowsEmulation::new()
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
    assert_eq!(
        text(win10(), "Return @TempDir"),
        format!(r"{profile}\AppData\Local\Temp")
    );
    assert!(text(win10(), "Return @StartupDir").ends_with(r"Start Menu\Programs\Startup"));
}

#[test]
fn with_host_paths_lets_the_common_layer_answer() {
    let body = "Return StringRight(@TempDir, 1)";
    let emu = win10().with_host_paths();
    assert_eq!(
        text(emu, body),
        std::path::MAIN_SEPARATOR.to_string()
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
    let body = r#"Local $d = DriveGetDrive("FIXED")
    Return $d[0] & "|" & DriveGetType("C:\") & "|" & DriveGetFilesystem("C:\") & "|" & _
        (DriveSpaceTotal("C:\") > 0) & "|" & (DriveSpaceFree("C:\") > 0) & "|" & DriveStatus("C:\")"#;
    assert_eq!(
        text(win10(), body),
        r"C:\|FIXED|NTFS|True|True|READY"
    );
}

#[test]
fn an_unknown_drive_reports_error() {
    assert_eq!(
        text(win10(), r#"Return DriveGetType("Z:\") & ":" & @error"#),
        ":1"
    );
    assert_eq!(text(win10(), r#"Return UBound(DriveGetDrive("NETWORK"))"#), "0");
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
    rt.set_platform(host_platform_with(emu));
    let err = rt.call_function("F", vec![]).unwrap_err();
    assert!(
        err.message().contains("undefined function"),
        "got: {}",
        err.message()
    );
    // The macros fall through to the host, which reports Linux.
    assert_eq!(
        rt.platform_name(),
        if cfg!(windows) {
            "common+windows"
        } else {
            "common+linux"
        }
    );
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
    let body = r#"Local $o = ObjCreate("Scripting.Dictionary")
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
fn gui_control_messages_answer_edit_and_listview() {
    let body = r#"
GUICreate("T", 100, 100)
Local $edit = GUICtrlCreateEdit("", 0, 0, 100, 50)
GUICtrlSetData($edit, "a" & @CRLF & "b" & @CRLF & "c")
Local $lines = GUICtrlSendMsg($edit, 0x00BA, 0, 0)
Local $list = GUICtrlCreateListView("", 0, 60, 100, 40)
GUICtrlSetData($list, "row1")
GUICtrlSetData($list, "row2")
Local $count = GUICtrlSendMsg($list, 0x1004, 0, 0)
Local $unknown = GUICtrlSendMsg($list, 0x1234, 0, 0)
Local $err = @error
Return $lines & ":" & $count & ":" & $unknown & ":" & $err
"#;
    assert_eq!(text(win10(), body), "3:2:0:1");
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
