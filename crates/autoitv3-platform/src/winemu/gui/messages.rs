//! Defaults for the control messages `GUICtrlSendMsg`/`GUICtrlRecvMsg` use.
//!
//! Windows' common controls answer a large, mostly optional message set. The
//! headless model answers the ones scripts actually rely on — Edit (`$EM_*`),
//! List/Combo (`$LB_*`/`$CB_*`), ListView (`$LVM_*`) and Tab (`$TCM_*`) — from
//! the widget state, and reports anything else as "unknown" so the caller can
//! set `@error` instead of inventing a value.
//!
//! A message that *changes* a control is applied here too, because the model is
//! what every backend renders from: a `$LVM_DELETEALLITEMS` that only emptied a
//! real ListView would leave the two disagreeing. Messages whose `lParam` is a
//! pointer cannot be applied — the pointer a script holds addresses the
//! emulation's own memory — so those are reported as unknown rather than read.

use super::model::Control;

/// `(result, known)` for one message against `control`.
pub fn send(control: &mut Control, msg: u32, wparam: i64, lparam: i64) -> (i64, bool) {
    let _ = lparam;
    let result = match msg {
        // ---- Edit ----
        0x00B0 => 0, // EM_GETSEL (a pointer pair)
        0x00B1 => 0, // EM_SETSEL: the caret is not model state
        0x00C1 => control.text.chars().count() as i64, // EM_LINELENGTH (approximate)
        0x00C2 => 0, // EM_REPLACESEL (a pointer)
        0x00C5 => {
            // EM_SETLIMITTEXT: the old limit comes back, as Windows does.
            let old = control.limit.map(|(max, _)| max.max(0)).unwrap_or(0);
            let min = control.limit.map(|(_, min)| min).unwrap_or(0);
            control.limit = Some((wparam.max(0), min));
            old
        }
        0x00C6 => 0, // EM_CANUNDO
        0x00C7 => 0, // EM_UNDO
        0x00C9 => 0, // EM_LINEFROMCHAR
        0x00BA => control.text.matches('\n').count() as i64 + 1, // EM_GETLINECOUNT
        0x00CE => 1, // EM_GETFIRSTVISIBLELINE
        0x00D5 => control.limit.map(|(max, _)| max.max(0)).unwrap_or(0), // EM_GETLIMITTEXT
        // ---- Button ----
        0x00F0 => i64::from(control.is_checked()), // BM_GETCHECK
        0x00F1 => {
            // BM_SETCHECK
            if wparam != 0 {
                control.state |= 0x01;
            } else {
                control.state &= !0x01;
            }
            0
        }
        // ---- List and combo ----
        0x0143 | 0x0180 => 0, // CB_ADDSTRING / LB_ADDSTRING (a pointer)
        0x0146 | 0x018B => control.data.len() as i64, // CB_GETCOUNT / LB_GETCOUNT
        0x0147 | 0x0188 => control // CB_GETCURSEL / LB_GETCURSEL
            .selection
            .map(|index| index as i64)
            .unwrap_or(-1),
        0x014B | 0x0184 => {
            // CB_RESETCONTENT / LB_RESETCONTENT
            control.data.clear();
            control.selection = None;
            0
        }
        0x014E | 0x0186 => {
            // CB_SETCURSEL / LB_SETCURSEL
            control.selection = (wparam >= 0).then_some(wparam as usize);
            0
        }
        // ---- ListView ----
        0x1004 => control.data.len() as i64, // LVM_GETITEMCOUNT
        0x1009 => {
            // LVM_DELETEALLITEMS
            control.data.clear();
            control.selection = None;
            0
        }
        0x100C => control // LVM_GETNEXTITEM
            .selection
            .map(|index| index as i64)
            .unwrap_or(-1),
        0x1013 | 0x1014 | 0x102A => 0, // LVM_ENSUREVISIBLE / SCROLL / UPDATE
        0x1032 => i64::from(control.selection.is_some()), // LVM_GETSELECTEDCOUNT
        0x102B | 0x104D | 0x1061 | 0x1073 | 0x1074 => 0, // pointer-based
        // ---- Tree and tab ----
        0x1100..=0x11FF => 0, // TVM_*
        0x1304 => control.data.len() as i64, // TCM_GETITEMCOUNT
        0x130B => control // TCM_GETCURSEL
            .selection
            .map(|index| index as i64)
            .unwrap_or(-1),
        0x130C => {
            // TCM_SETCURSEL
            control.selection = (wparam >= 0).then_some(wparam as usize);
            0
        }
        _ => return (0, false),
    };
    (result, true)
}
