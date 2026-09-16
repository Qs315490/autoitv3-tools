//! One-line notices for the dialogs a script shows.
//!
//! A dialog is the one thing a run does that leaves no trace in a terminal:
//! the emulation answers it invisibly, and the native Windows backend puts it
//! in a window that only the person in front of the machine can read (and that
//! nothing can grep afterwards). Both therefore print the same one-line summary
//! on stderr — `[winemu]` when the emulation answered, `[win32]` when the real
//! dialog was shown:
//!
//! ```text
//! [win32] MsgBox(16, "错误", "SQLite 装载失败") -> 1
//! [winemu] InputBox("Name", "Driver:") -> "nvlddmkm"
//! ```
//!
//! They are deliberately **not translated**: they are structured traces whose
//! fields (flags, paths, dialog text) are data, and the same shape has to be
//! greppable from a log.

/// The line for a `MsgBox`, with the button the user (or the scripted answer)
/// chose.
pub fn msgbox_notice(prefix: &str, flags: i64, title: &str, text: &str, answer: i64) -> String {
    format!("{prefix} MsgBox({flags}, {title:?}, {text:?}) -> {answer}")
}

/// The line for an `InputBox`: `answer` is the quoted text the user typed, or
/// `cancelled`.
pub fn inputbox_notice(prefix: &str, title: &str, prompt: &str, answer: &str) -> String {
    format!("{prefix} InputBox({title:?}, {prompt:?}) -> {answer}")
}

/// The line for a `FileOpenDialog` / `FileSaveDialog` / `FileSelectFolder`:
/// `answer` is the quoted path, or `cancelled`.
pub fn file_dialog_notice(prefix: &str, name: &str, title: &str, answer: &str) -> String {
    format!("{prefix} {name}({title:?}) -> {answer}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_shapes_read_back_the_fields_worth_having() {
        assert_eq!(
            msgbox_notice("[win32]", 16, "错误", "装载失败", 1),
            "[win32] MsgBox(16, \"错误\", \"装载失败\") -> 1"
        );
        assert_eq!(
            inputbox_notice("[winemu]", "Name", "Driver:", "\"nvlddmkm\""),
            "[winemu] InputBox(\"Name\", \"Driver:\") -> \"nvlddmkm\""
        );
        assert_eq!(
            file_dialog_notice("[win32]", "FileSelectFolder", "Pick", "cancelled"),
            "[win32] FileSelectFolder(\"Pick\") -> cancelled"
        );
    }
}
