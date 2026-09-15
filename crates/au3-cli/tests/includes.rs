//! End-to-end tests for `#include`.
//!
//! The loader expands the directives into the parsed program, so what these
//! check is the whole path: the file is found the way the help page's search
//! tables say, the constants and functions it defines are there for the script,
//! and a file that cannot be found is reported without stopping the run.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A scratch directory for one test.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("au3-include-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn write(dir: &std::path::Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("write script");
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
fn a_quoted_include_gives_the_script_its_constants_and_functions() {
    let dir = scratch("quoted");
    write(
        &dir,
        "consts.au3",
        "#include-once\nGlobal Const $ANSWER = 42\nFunc FromInclude()\n    Return \"included\"\nEndFunc\n",
    );
    let main = write(
        &dir,
        "main.au3",
        "#include \"consts.au3\"\nConsoleWrite($ANSWER & \":\" & FromInclude() & @CRLF)\n",
    );
    let (ok, out) = au3(&["run", main.to_str().unwrap()]);
    assert!(ok, "got:\n{out}");
    assert!(out.contains("42:included"), "got:\n{out}");
}

#[test]
fn an_angled_include_uses_the_include_path() {
    let dir = scratch("angled");
    let library = dir.join("library");
    std::fs::create_dir_all(&library).unwrap();
    write(&library, "Std.au3", "Global Const $STD = 7\n");
    // The script's own directory has a file of the same name, which `<...>`
    // must not prefer: the standard library is searched first.
    write(&dir, "Std.au3", "Global Const $STD = 0\n");
    let main = write(
        &dir,
        "main.au3",
        "#include <Std.au3>\nConsoleWrite($STD & @CRLF)\n",
    );
    let (ok, out) = au3(&[
        "run",
        "--include-path",
        library.to_str().unwrap(),
        main.to_str().unwrap(),
    ]);
    assert!(ok, "got:\n{out}");
    assert!(out.starts_with('7'), "got:\n{out}");
}

#[test]
fn a_missing_include_is_reported_and_the_script_still_runs() {
    let dir = scratch("missing");
    let main = write(
        &dir,
        "main.au3",
        "#include \"nowhere.au3\"\nConsoleWrite(\"ran\" & @CRLF)\n",
    );
    let (ok, out) = au3(&["run", main.to_str().unwrap()]);
    assert!(ok, "got:\n{out}");
    assert!(out.contains("nowhere.au3"), "got:\n{out}");
    assert!(out.contains("ran"), "got:\n{out}");
}

#[test]
fn no_includes_leaves_the_directives_alone() {
    let dir = scratch("none");
    write(&dir, "consts.au3", "Global Const $ANSWER = 42\n");
    let main = write(
        &dir,
        "main.au3",
        "#include \"consts.au3\"\nConsoleWrite(VarGetType($ANSWER) & @CRLF)\n",
    );
    let (ok, out) = au3(&["run", "--no-includes", main.to_str().unwrap()]);
    assert!(ok, "got:\n{out}");
    // `$ANSWER` was never declared, so it is not an Int32 the way it would be
    // with the include expanded.
    assert!(!out.contains("Int32"), "got:\n{out}");
}
