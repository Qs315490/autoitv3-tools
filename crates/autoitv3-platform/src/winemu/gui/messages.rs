//! Defaults for the control messages `GUICtrlSendMsg`/`GUICtrlRecvMsg` use.
//!
//! Windows' common controls answer a large, mostly optional message set. The
//! headless model answers the ones scripts actually rely on — Edit (`$EM_*`)
//! and ListView (`$LVM_*`) — from the widget state, and reports anything else as
//! "unknown" so the caller can set `@error` instead of inventing a value.

use super::model::Control;

/// `(result, known)` for one message against `control`.
pub fn send(control: &Control, msg: u32, wparam: i64) -> (i64, bool) {
    let result = match msg {
        // ---- Edit ----
        0x00B0 => 0,          // EM_GETSEL
        0x00B1 => 0,          // EM_SETSEL
        0x00C1 => control.text.chars().count() as i64, // EM_LINELENGTH (approximate)
        0x00C2 => 0,          // EM_REPLACESEL
        0x00C5 => control.limit.map(|(max, _)| max.max(0)).unwrap_or(0), // EM_SETLIMITTEXT
        0x00C6 => 0,          // EM_CANUNDO
        0x00C7 => 0,          // EM_UNDO
        0x00C9 => 0,          // EM_LINEFROMCHAR
        0x00BA => control.text.matches('\n').count() as i64 + 1, // EM_GETLINECOUNT
        0x00CE => 1,          // EM_GETFIRSTVISIBLELINE
        0x00D5 => control.limit.map(|(max, _)| max.max(0)).unwrap_or(0), // EM_GETLIMITTEXT
        // ---- ListView / TreeView ----
        0x1004 => control.data.len() as i64, // LVM_GETITEMCOUNT
        0x1009 => 0,                         // LVM_DELETEALLITEMS
        0x100C => -1,                        // LVM_GETNEXTITEM
        0x101F => 0,                         // LVM_GETHEADER
        0x1032 => 0,                         // LVM_GETSELECTEDCOUNT
        0x104D => 0,                         // LVM_INSERTITEMW
        0x1061 => 0,                         // LVM_INSERTCOLUMNW
        0x1073 => 0,                         // LVM_GETITEMTEXTW
        0x1074 => 0,                         // LVM_SETITEMTEXTW
        0x1100..=0x11FF => 0,                // TVM_*
        _ => return (0, false),
    };
    let _ = wparam;
    (result, true)
}
