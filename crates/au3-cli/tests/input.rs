//! End-to-end tests for the shared input loader.
//!
//! Every command's `FILE` accepts either `.au3` source or a compiled build, so
//! the loader has to pick the path from the bytes' header rather than the file
//! name. A real build is not needed to check that dispatch: a text file still
//! goes down the source path, a header-only file must report the missing
//! compiled script instead of trying to parse binary, and a binary that is
//! neither says so.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Write `bytes` to a scratch file and return its path.
fn scratch(tag: &str, name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("au3-input-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("write input");
    path
}

/// Run `au3 <args>` and return `(succeeded, stdout and stderr folded together)`.
fn au3(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_au3"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run au3");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn a_source_file_still_goes_down_the_text_path() {
    let path = scratch(
        "source",
        "script.au3",
        b"Func Main()\n    Return 1\nEndFunc\n",
    );
    let (ok, out) = au3(&["parse", path.to_str().unwrap()]);
    assert!(ok, "got:\n{out}");
    assert!(out.contains("1 functions"), "got:\n{out}");
}

/// A UPX-packed build keeps its script inside the packed data, so there is no
/// `AU3!EA` chunk to read: the loader has to say that instead of the generic
/// "no compiled script", which sends an analyst looking for the wrong thing.
#[test]
fn a_upx_packed_build_says_so() {
    let path = scratch("upx-packed", "packed.exe", &upx_looking_image());
    let (ok, out) = au3(&["parse", path.to_str().unwrap()]);
    assert!(!ok, "a packed build has no script to parse:\n{out}");
    assert!(out.contains("UPX-packed"), "got:\n{out}");
    assert!(out.contains("upx -d"), "the message says what to do: got:\n{out}");
}

/// Enough of a PE for the packer check: two sections carrying UPX's names and
/// the stub's magic.
fn upx_looking_image() -> Vec<u8> {
    let mut image = vec![0u8; 0x400];
    image[0..2].copy_from_slice(b"MZ");
    image[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
    image[0x40..0x44].copy_from_slice(b"PE\0\0");
    image[0x44..0x46].copy_from_slice(&0x14cu16.to_le_bytes()); // i386
    image[0x46..0x48].copy_from_slice(&2u16.to_le_bytes()); // two sections
    image[0x54..0x56].copy_from_slice(&0xe0u16.to_le_bytes()); // optional size
    image[0x58..0x5a].copy_from_slice(&0x10bu16.to_le_bytes()); // PE32
    let table = 0x58 + 0xe0;
    image[table..table + 4].copy_from_slice(b"UPX0");
    image[table + 40..table + 44].copy_from_slice(b"UPX1");
    image[0x3f0..0x3f4].copy_from_slice(b"UPX!");
    image
}

#[test]
fn a_build_header_without_a_script_reports_the_build_path() {
    // A PE header but no `AU3!EA…` chunk: the loader must treat it as a build
    // and say the compiled script is missing, not try to parse binary source.
    let path = scratch("fake-pe", "build.exe", b"MZ\x90\x00this is not a PE");
    let (ok, out) = au3(&["parse", path.to_str().unwrap()]);
    assert!(!ok, "got:\n{out}");
    assert!(out.contains("no compiled script"), "got:\n{out}");
}

#[test]
fn a_binary_that_is_neither_source_nor_build_says_so() {
    // A stray continuation byte and a NUL: not UTF-8, and no BOM to say it is
    // UTF-16 either.
    let path = scratch("binary", "blob.bin", &[0x9f, 0xff, 0x00, 0x01]);
    let (ok, out) = au3(&["parse", path.to_str().unwrap()]);
    assert!(!ok, "got:\n{out}");
    assert!(out.contains("not UTF-8 or UTF-16 source"), "got:\n{out}");
}

#[test]
fn a_utf16_script_is_read() {
    // AutoIt accepts a script saved as UTF-16 with a BOM, and so must the
    // loader: `parse` has to see the function, not two NULs per character.
    let mut bytes: Vec<u8> = vec![0xff, 0xfe];
    for unit in "Func Main()\r\n    Return 1\r\nEndFunc\r\n".encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let path = scratch("utf16", "script.au3", &bytes);
    let (ok, out) = au3(&["parse", path.to_str().unwrap()]);
    assert!(ok, "got:\n{out}");
    assert!(out.contains("1 functions"), "got:\n{out}");
}

/// The whole path on a real build, when one is pointed at.
///
/// `aut2exe` is not reimplemented here, so a build cannot be synthesised in the
/// test; `AU3_BUILD` names one to unpack instead. Skipped when unset, like the
/// other optional-sample tests.
#[test]
fn a_real_build_can_be_used_as_the_input() {
    let Some(build) = std::env::var_os("AU3_BUILD") else {
        return;
    };
    let build = PathBuf::from(build);
    let (ok, out) = au3(&["parse", build.to_str().unwrap()]);
    assert!(ok, "got:\n{out}");
    assert!(out.contains("input build"), "got:\n{out}");
    assert!(out.contains("parsed OK"), "got:\n{out}");
}
