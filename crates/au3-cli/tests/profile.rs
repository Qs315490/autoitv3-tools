//! The execution profile's refusals are visible.
//!
//! A refused side effect looks exactly like the machine refusing it — the call
//! returns its failure value and sets `@error = 1` — so the profile says which
//! kind it turned down and how to overrule it, once per kind. That is the
//! difference between "`DirCreate` failed because of ACLs" (an afternoon) and
//! "the deterministic profile refused a file write" (one line).

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Write `body` to a scratch script and return its path.
fn script(tag: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("au3-profile-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let path = dir.join("script.au3");
    std::fs::write(&path, body).expect("write script");
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
fn a_refused_side_effect_is_named_once() {
    // Two writes, one note: the second attempt is the same kind.
    let path = script(
        "note",
        "DirCreate(@TempDir & \"\\au3-profile-a\")\nDirCreate(@TempDir & \"\\au3-profile-b\")\n",
    );
    let (ok, out) = au3(&["run", &path.to_string_lossy()]);
    assert!(ok, "got:\n{out}");
    assert!(
        out.contains("note: a file side effect was refused by the execution profile"),
        "got:\n{out}"
    );
    assert!(out.contains("--allow file"), "the note says how to allow it:\n{out}");
    assert_eq!(
        out.matches("note: a file side effect").count(),
        1,
        "reported once per kind:\n{out}"
    );
}

#[test]
fn a_faithful_run_does_the_side_effect_and_says_nothing() {
    let path = script(
        "faithful",
        "Local $dir = @TempDir & \"\\au3-profile-faithful\"\n\
         Local $r = DirCreate($dir)\n\
         ConsoleWrite(\"created=\" & $r & \" err=\" & @error & @CRLF)\n\
         DirRemove($dir)\n",
    );
    let (ok, out) = au3(&["run", &path.to_string_lossy(), "--faithful"]);
    assert!(ok, "got:\n{out}");
    assert!(out.contains("created=1 err=0"), "the write happened:\n{out}");
    assert!(!out.contains("was refused"), "nothing was refused:\n{out}");
}

#[test]
fn allowing_one_kind_silences_only_that_kind() {
    let path = script(
        "allow-file",
        "DirCreate(@TempDir & \"\\au3-profile-allowed\")\n\
         EnvSet(\"AU3_PROFILE_TEST\", \"1\")\n",
    );
    let (ok, out) = au3(&["run", &path.to_string_lossy(), "--allow", "file"]);
    assert!(ok, "got:\n{out}");
    assert!(!out.contains("a file side effect"), "file writes are allowed:\n{out}");
    assert!(
        out.contains("note: a env side effect was refused"),
        "the environment write is not:\n{out}"
    );
}
