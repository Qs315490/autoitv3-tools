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
    let path = scratch("binary", "blob.bin", &[0xff, 0xfe, 0x00, 0x01]);
    let (ok, out) = au3(&["parse", path.to_str().unwrap()]);
    assert!(!ok, "got:\n{out}");
    assert!(out.contains("not UTF-8 source"), "got:\n{out}");
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
