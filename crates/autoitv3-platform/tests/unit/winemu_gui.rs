//! Unit tests for `winemu::gui` — the `[winemu]` dialog notices.
//!
//! Reached through `#[path]` from `src/winemu/gui/mod.rs`, so the private
//! formatters can be tested directly. What they print is the only trace of a
//! dialog the emulation answered without showing anything, so the exact wording
//! is worth pinning.

use super::*;

#[test]
fn a_msgbox_notice_carries_flags_title_text_and_answer() {
    let notice = msgbox_notice(16, "Error", "cannot open input", 1);
    assert_eq!(
        notice,
        r#"[winemu] MsgBox(16, "Error", "cannot open input") -> 1"#
    );
}

#[test]
fn a_msgbox_answer_is_reported_not_assumed() {
    // A scripted answer is what the script actually sees, so the notice has to
    // say which one it was rather than always claiming OK.
    assert!(msgbox_notice(4, "t", "body", 7).ends_with("-> 7"));
}

#[test]
fn an_inputbox_notice_names_the_prompt_and_the_answer() {
    assert_eq!(
        inputbox_notice("Setup", "Reboot now?", "yes"),
        r#"[winemu] InputBox("Setup", "Reboot now?") -> yes"#
    );
    assert_eq!(
        inputbox_notice("Setup", "Reboot now?", "cancelled"),
        r#"[winemu] InputBox("Setup", "Reboot now?") -> cancelled"#
    );
}

#[test]
fn a_file_dialog_notice_keeps_the_script_spelling() {
    // `name` is the spelling the script used, which is what a reader will look
    // up; the answer is the path handed back, or `cancelled`.
    assert_eq!(
        file_dialog_notice("FileOpenDialog", "Pick a file", r#""C:\\tmp\\a.dat""#),
        r#"[winemu] FileOpenDialog("Pick a file") -> "C:\\tmp\\a.dat""#
    );
    assert_eq!(
        file_dialog_notice("FileSelectFolder", "Pick a folder", "cancelled"),
        r#"[winemu] FileSelectFolder("Pick a folder") -> cancelled"#
    );
}

#[test]
fn quotes_and_backslashes_are_escaped_so_the_line_stays_one_line() {
    let notice = msgbox_notice(0, r#"say "hi""#, "line\\slash", 1);
    assert!(!notice[..notice.len() - 4].contains('\n'), "{notice}");
    assert!(notice.contains(r#""say \"hi\"""#), "{notice}");
    assert!(notice.contains(r#""line\\slash""#), "{notice}");
}
