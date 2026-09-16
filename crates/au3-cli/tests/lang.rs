//! End-to-end checks for `--lang` / `AU3_LANG` / locale selection.
//!
//! The language has to be decided **before** clap renders help, so the parsing
//! path is special (`i18n_cli::lang_from_args` scans `argv`); these tests drive
//! the real binary to pin the whole chain down, and one of them runs a battery
//! of commands with `AU3_I18N_STRICT=1` to catch a message that was wrapped but
//! never translated.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_au3");

/// Run `au3` with a clean locale: only the variables in `envs` are visible, so
/// the developer's own `LANG`/`AU3_LANG` cannot leak into the result.
fn au3(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .env_remove("AU3_LANG")
        .env_remove("AU3_I18N_STRICT")
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .env_remove("LANG")
        .stdin(Stdio::null());
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd.output().expect("running au3")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A scratch directory for the scripts the battery runs, cleaned per test.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("lang-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn write_script(dir: &Path, name: &str, source: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, source).expect("write script");
    path
}

#[test]
fn chinese_help_is_chinese() {
    let out = au3(&["--lang", "zh-CN", "--help"], &[]);
    let text = stdout(&out);
    assert!(text.contains("用法:"), "got:\n{text}");
    assert!(text.contains("子命令:"), "got:\n{text}");
    assert!(text.contains("选项:"), "got:\n{text}");
    assert!(!text.contains("Usage:"), "English heading left in:\n{text}");
}

#[test]
fn without_a_locale_the_tool_speaks_english() {
    let out = au3(&["--help"], &[]);
    let text = stdout(&out);
    assert!(text.contains("Usage:"), "got:\n{text}");
    assert!(!text.contains("用法:"), "got:\n{text}");
}

#[test]
fn the_locale_selects_the_language() {
    let zh = stdout(&au3(&["--help"], &[("LANG", "zh_CN.UTF-8")]));
    assert!(zh.contains("用法:"), "got:\n{zh}");

    let en = stdout(&au3(&["--help"], &[("LANG", "en_US.UTF-8")]));
    assert!(en.contains("Usage:"), "got:\n{en}");

    // Anything that is not English or Chinese falls back to English.
    let other = stdout(&au3(&["--help"], &[("LANG", "fr_FR.UTF-8")]));
    assert!(other.contains("Usage:"), "got:\n{other}");
}

#[test]
fn au3_lang_beats_the_locale_and_explicit_lang_beats_both() {
    let out = au3(&["--help"], &[("AU3_LANG", "zh-CN"), ("LANG", "en_US.UTF-8")]);
    assert!(stdout(&out).contains("用法:"), "AU3_LANG should win");

    let out = au3(
        &["--lang", "en", "--help"],
        &[("AU3_LANG", "zh-CN"), ("LANG", "zh_CN.UTF-8")],
    );
    assert!(stdout(&out).contains("Usage:"), "--lang should win");

    // `auto` explicitly asks for the environment again.
    let out = au3(&["--lang", "auto", "--help"], &[("AU3_LANG", "zh-CN")]);
    assert!(stdout(&out).contains("用法:"), "got:\n{}", stdout(&out));
}

#[test]
fn usage_errors_are_translated_too() {
    let out = au3(&["--lang", "zh-CN", "no-such-command"], &[]);
    let text = stderr(&out);
    assert_eq!(out.status.code(), Some(2));
    assert!(text.contains("错误:"), "got:\n{text}");
    assert!(text.contains("无法识别的子命令"), "got:\n{text}");
    assert!(text.contains("用法:"), "got:\n{text}");
    assert!(text.contains("更多信息请运行"), "got:\n{text}");
}

#[test]
fn runtime_errors_reach_the_user_in_chinese() {
    let dir = scratch("runtime");
    let path = write_script(
        dir.as_path(),
        "bound.au3",
        "Func Boom($n)\n    Local $list[2] = [\"a\", \"b\"]\n    Return $list[$n]\nEndFunc\n\nBoom(5)\n",
    );
    let out = au3(&["--lang", "zh-CN", "run", &path.to_string_lossy()], &[]);
    let text = stderr(&out);
    assert!(text.contains("索引 5 越界"), "got:\n{text}");
    assert!(text.contains("位于 3：12"), "the position line, got:\n{text}");
}

/// The regression guard: every message a whole battery of commands prints must
/// have a zh-CN translation. `AU3_I18N_STRICT=1` makes the toolkit name the
/// untranslated key on stderr instead of silently staying English.
#[test]
fn a_battery_of_commands_leaves_nothing_untranslated() {
    let dir = scratch("battery");
    let ok = write_script(&dir, "ok.au3", "ConsoleWrite(Add(1, 2) & @CRLF)\n\nFunc Add($a, $b)\n    Return $a + $b\nEndFunc\n");
    let bad = write_script(&dir, "bad.au3", "Func Bad(\n");
    let gui = write_script(&dir, "gui.au3", "GUICreate(\"hi\")\nGUIDelete()\n");
    let session = write_script(
        &dir,
        "session.au3dbg",
        "break 1\nrun\nbt\ninfo locals\nprint $a\ninfo breakpoints\ninfo functions\n\
         info globals\nwatch $r\nlist\nframe\nstopat MsgBox\nstopat\nuntilcall Add\n\
         untilret Add\nuntilgui\ntrace on\ntrace off\ncatch off\ncatch on\n\
         eval $z = 1\nset $r = 5\nignore 1 1\ncommands 1 do print $a\nenable 1\n\
         disable 1\ndelete 1\ntbreak 2\nbacktrace\nhelp\nhelp break\nnext\nstep\n\
         finish\nnostop\nunwatch $r\nquit\n",
    );

    let ok = ok.to_string_lossy().into_owned();
    let bad = bad.to_string_lossy().into_owned();
    let gui = gui.to_string_lossy().into_owned();
    let session = session.to_string_lossy().into_owned();
    let missing = dir.join("missing.au3").to_string_lossy().into_owned();

    let envs: &[(&str, &str)] = &[("AU3_LANG", "zh-CN"), ("AU3_I18N_STRICT", "1")];
    let mut runs: Vec<Vec<&str>> = Vec::new();
    for help in [
        "--help",
        "help parse",
        "parse -h",
        "pretty -h",
        "deobfuscate -h",
        "evaluate -h",
        "run -h",
        "debug -h",
        "unpack -h",
    ] {
        runs.push(help.split(' ').collect());
    }
    for args in [
        vec!["parse", &ok],
        vec!["parse", &bad],
        vec!["parse", &missing],
        vec!["pretty", &ok],
        vec!["pretty", &bad],
        vec!["deobfuscate", &ok],
        vec!["deobfuscate", &bad, "--evaluate"],
        vec!["evaluate", &ok],
        vec!["evaluate", &missing],
        vec!["run", &ok],
        vec!["run", &ok, "Add", "--arg", "2", "--arg", "3"],
        vec!["run", &ok, "--trace"],
        vec!["run", &gui, "--gui", "headless"],
        vec!["run", &ok, "--gui", "window"],
        vec!["run", &ok, "--no-elevate"],
        vec!["unpack", &ok],
        vec!["unpack", &missing],
        vec!["parse"],
        vec!["no-such-command"],
        vec!["debug", &ok, "-x", &session],
        // A session that never touches the script also exercises the prompt.
        vec!["debug", &ok, "-c", "help stopat", "-c", "quit"],
    ] {
        runs.push(args);
    }

    let mut output = String::new();
    for args in runs {
        let out = au3(&args, envs);
        output.push_str(&stdout(&out));
        output.push_str(&stderr(&out));
    }
    let misses: Vec<&str> = output
        .lines()
        .filter(|line| line.contains("[i18n] no zh-CN translation:"))
        .collect();
    assert!(
        misses.is_empty(),
        "{} message(s) printed by the battery have no translation:\n{}",
        misses.len(),
        misses.join("\n")
    );
    // And it really did print Chinese, so the check above is not vacuous.
    assert!(
        output.contains("错误") || output.contains("用法"),
        "the battery produced no Chinese output at all"
    );
}
