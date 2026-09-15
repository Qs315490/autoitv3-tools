//! The GUI backend that draws with **real Win32 windows and controls**.
//!
//! The AutoIt GUI *semantics* — handle numbering, `@error`, `GUICtrlRead`,
//! `$GUI_EVENT_*` — live in `winemu/gui` and stay platform-independent: every
//! host runs the same state machine. What differs is what that state machine is
//! attached to. Off Windows it is the headless model, optionally rendered by
//! `autoitv3-gui-egui`; **on Windows it is this**: the window is a real
//! `CreateWindowExW` top-level window and every control is a real child of it
//! (`BUTTON`, `EDIT`, `SysListView32`, …), drawn and hit-tested by the OS, so a
//! script's GUI looks and behaves like a native one.
//!
//! # Why a backend and not a second set of GUI functions
//!
//! 165 functions would otherwise have to be written twice — once against the
//! model, once against Win32 — and the two would drift. Instead the model stays
//! authoritative and this backend mirrors it (see
//! [`autoitv3_gui_model::GuiBackend`]):
//!
//! * `on_window`/`on_control` create or update the real window/control,
//! * `poll` pumps the thread's message queue and turns `WM_*` into
//!   [`GuiEvent`]s,
//! * `take_updates` reads back what the *user* changed (typed text, a toggled
//!   checkbox, a list selection, a resize) so the next `GUICtrlRead` sees it.
//!
//! Everything runs on the script's own thread, which is where Win32 insists a
//! window's message loop lives; `GUIGetMsg` is what pumps it.
//!
//! # Checking it without Windows
//!
//! The `windows` module is `#[cfg(windows)]`, so a `cargo check` on another host
//! never parses this file — but nothing here needs Windows to *type-check*:
//! `windows-sys`' declarations compile on any target, and only the calls need
//! Windows to link. A throwaway crate with this file as a module, the
//! `autoitv3-gui-model` path dependency, and `windows-sys` 0.61 carrying the
//! features `Cargo.toml` lists, therefore catches every type error before a
//! Windows build does:
//!
//! ```text
//! // src/lib.rs of the throwaway crate
//! #[path = "<platform>/src/windows/gui.rs"] pub mod gui;
//! ```
//!
//! # Known limits
//!
//! The pixels are real, but not every one of them is drawn yet:
//!
//! * tooltips (`GUICtrlSetTip`) and window icons (`GUISetIcon`) are not shown;
//! * a graphic control paints the *outlines* of its commands — the fill colour
//!   behind `$GUI_GR_COLOR`'s second argument is not painted, and the command
//!   types the model does not carry (bezier, pie, `$GUI_GR_PENSIZE`) are not
//!   drawn at all;
//! * a `ListView` row is one line with as many columns as it has cells; the
//!   icon views, sorting and `$LVS_EX_CHECKBOXES` are not there;
//! * `WinGetPos` answers with the size the model keeps, which is the client
//!   area; the frame is added for the real window, so the two differ from what
//!   the official interpreter reports there.
//!
//! The *semantics* of all of it still work: `GUICtrlRead` answers from the
//! model, so only the pixels stay behind.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::ffi::c_void;

use autoitv3_gui_model::{
    Control, ControlKind, DrawCmd, Font, GuiBackend, GuiEvent, GuiImage, GuiUpdate, Window,
    WindowState,
};
use windows_sys::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    FillRect, GetObjectW, BITMAP, HBRUSH, HBITMAP,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DeleteObject, Ellipse, EndPaint,
    GetStockObject, InvalidateRect, LineTo, MoveToEx, Rectangle, SelectObject, SetBkColor,
    SetTextColor, TextOutW, UpdateWindow, HDC, PAINTSTRUCT,
};
use windows_sys::Win32::Graphics::GdiPlus::{
    GdipCreateBitmapFromFile, GdipCreateHBITMAPFromBitmap, GdipDisposeImage, GdipGetImageHeight,
    GdipGetImageWidth, GdiplusStartup, GdiplusStartupInput, GpBitmap, GpImage,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    InitCommonControlsEx, INITCOMMONCONTROLSEX, LVCOLUMNW, LVITEMW, TCITEMW, TVINSERTSTRUCTW,
    TVITEMW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DestroyWindow, DispatchMessageW, GetClientRect, GetCursorPos, GetSystemMetrics,
    GetWindowRect, SetWindowLongW,
    GetWindowTextLengthW, GetWindowTextW, IsDialogMessageW, IsIconic, IsWindow, IsZoomed,
    LoadCursorW, LoadImageW, MoveWindow, PeekMessageW, RegisterClassExW, SendMessageW, SetCursor,
    SetMenu, SetWindowPos, SetWindowTextW, ShowWindow, TranslateMessage, WindowFromPoint, MSG,
    WNDCLASSEXW,
};

// ---------------------------------------------------------------------------
// Win32 constants
//
// Spelled out rather than imported: this module needs a few dozen of them, and
// a value with its header name as a comment is easier to audit than a path into
// `windows-sys` that moves between releases. The widths matter — styles,
// messages and notification codes are all 32-bit bit patterns.
// ---------------------------------------------------------------------------

// Window styles (`winuser.h`).
const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const WS_VISIBLE: u32 = 0x1000_0000;
const WS_CHILD: u32 = 0x4000_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_GROUP: u32 = 0x0002_0000;
const WS_BORDER: u32 = 0x0080_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const WS_EX_CLIENTEDGE: u32 = 0x0000_0200;

// `ShowWindow` commands.
const SW_HIDE: i32 = 0;
const SW_SHOWNORMAL: i32 = 1;
const SW_SHOWMAXIMIZED: i32 = 3;
const SW_SHOW: i32 = 5;
const SW_MINIMIZE: i32 = 6;

// Window messages (`winuser.h`).
const WM_COMMAND: u32 = 0x0111;
const WM_CLOSE: u32 = 0x0010;
const WM_SETFONT: u32 = 0x0030;
const WM_SYSCOMMAND: u32 = 0x0112;

// `WM_SYSCOMMAND` requests.
const SC_MINIMIZE: usize = 0xF020;
const SC_MAXIMIZE: usize = 0xF030;
const SC_RESTORE: usize = 0xF120;

// Control notification codes, in the high word of `WM_COMMAND`'s `wParam`.
const BN_CLICKED: u32 = 0;
const BN_DOUBLECLICKED: u32 = 5;
const EN_CHANGE: u32 = 0x0300;
const LBN_SELCHANGE: u32 = 1;
const LBN_DBLCLK: u32 = 2;
const CBN_SELCHANGE: u32 = 1;
const CBN_EDITCHANGE: u32 = 5;

// Button/edit/static/list/combo styles (`winuser.h`).
const BS_AUTOCHECKBOX: u32 = 0x0000_0003;
const BS_AUTORADIOBUTTON: u32 = 0x0000_0009;
const BS_GROUPBOX: u32 = 0x0000_0007;
const ES_MULTILINE: u32 = 0x0000_0004;
const ES_AUTOVSCROLL: u32 = 0x0000_0040;
const ES_AUTOHSCROLL: u32 = 0x0000_0080;
const ES_WANTRETURN: u32 = 0x0000_1000;
const SS_LEFT: u32 = 0x0000_0000;
const SS_ICON: u32 = 0x0000_0003;
const SS_BITMAP: u32 = 0x0000_000E;
const SS_CENTERIMAGE: u32 = 0x0000_0200;
const LBS_NOTIFY: u32 = 0x0000_0001;
const LBS_NOINTEGRALHEIGHT: u32 = 0x0000_0100;
const CBS_DROPDOWN: u32 = 0x0000_0002;
const CBS_AUTOHSCROLL: u32 = 0x0000_0040;
const CBS_HASSTRINGS: u32 = 0x0000_0200;
const LVS_REPORT: u32 = 0x0000_0001;
const LVS_SHOWSELALWAYS: u32 = 0x0000_0008;
const TVS_HASBUTTONS: u32 = 0x0000_0001;
const TVS_HASLINES: u32 = 0x0000_0002;
const TVS_LINESATROOT: u32 = 0x0000_0004;
const PBS_SMOOTH: u32 = 0x0000_0001;
const TBS_AUTOTICKS: u32 = 0x0000_0001;
const UDS_ALIGNRIGHT: u32 = 0x0000_0004;
const UDS_ARROWKEYS: u32 = 0x0000_0020;
const MCS_NOTODAY: u32 = 0x0000_0010;

// More window styles (`winuser.h`), the defaults AutoIt's help pages document
// for each control.
const SS_NOTIFY: u32 = 0x0000_0100;
const ES_READONLY: u32 = 0x0000_0800;
const WS_HSCROLL: u32 = 0x0010_0000;
const WS_CLIPSIBLINGS: u32 = 0x0400_0000;
const WS_EX_WINDOWEDGE: u32 = 0x0000_0100;
const LBS_SORT: u32 = 0x0000_0002;
const TCS_TOOLTIPS: u32 = 0x0000_0400;
const TVS_DISABLEDRAGDROP: u32 = 0x0000_0010;
const TVS_SHOWSELALWAYS: u32 = 0x0000_0020;
const LVS_SINGLESEL: u32 = 0x0000_0004;
const LVS_EX_FULLROWSELECT: u32 = 0x0000_0020;
const DTS_LONGDATEFORMAT: u32 = 0x0000_0004;
const UDS_SETBUDDYINT: u32 = 0x0000_0002;

// Control messages.
const BM_GETCHECK: u32 = 0x00F0;
const BM_SETCHECK: u32 = 0x00F1;
const EM_SETLIMITTEXT: u32 = 0x00C5;
const LB_ADDSTRING: u32 = 0x0180;
const LB_RESETCONTENT: u32 = 0x0184;
const LB_SETCURSEL: u32 = 0x0186;
const LB_GETCURSEL: u32 = 0x0188;
const CB_ADDSTRING: u32 = 0x0143;
const CB_RESETCONTENT: u32 = 0x014B;
const CB_SETCURSEL: u32 = 0x014E;
const CB_GETCURSEL: u32 = 0x0147;
const LVM_INSERTITEMW: u32 = 0x104D;
const LVM_INSERTCOLUMNW: u32 = 0x1061;
const LVM_SETCOLUMNW: u32 = 0x1060;
const LVM_SETCOLUMNWIDTH: u32 = 0x101E;
const LVM_SETITEMW: u32 = 0x1076;
const LVM_SETITEMSTATE: u32 = 0x102B;
const LVIS_SELECTED: u32 = 0x0002;
const LVIS_FOCUSED: u32 = 0x0001;
const BM_SETIMAGE: u32 = 0x00F7;
const BS_ICON: u32 = 0x0000_0040;
const LVCF_TEXT: u32 = 0x0004;
const LVCF_WIDTH: u32 = 0x0002;
/// `LVSCW_AUTOSIZE_USEHEADER`: size a column to its heading.
const LVSCW_AUTOSIZE_USEHEADER: isize = -2;
const LVM_DELETEALLITEMS: u32 = 0x1009;
const LVM_GETNEXTITEM: u32 = 0x100C;
const TVM_GETNEXTITEM: u32 = 0x110A;
const TVM_INSERTITEMW: u32 = 0x1132;
const TVM_DELETEITEM: u32 = 0x1101;
const TVM_EXPAND: u32 = 0x1102;
const TVM_SELECTITEM: u32 = 0x110B;
const TVM_SETITEMW: u32 = 0x113F;
const TCM_INSERTITEMW: u32 = 0x133E;
const TCM_DELETEALLITEMS: u32 = 0x1305;
const TCM_GETCURSEL: u32 = 0x130B;
const TCM_SETCURSEL: u32 = 0x130C;
const PBM_SETPOS: u32 = 0x0402;
const TBM_SETPOS: u32 = 0x0405;
const TBM_SETRANGE: u32 = 0x0406;
const UDM_SETBUDDY: u32 = 0x112A;
const UDM_SETRANGE32: u32 = 0x1136;
const UDM_SETPOS32: u32 = 0x1138;
const STM_SETICON: u32 = 0x0170;
const STM_SETIMAGE: u32 = 0x0172;
const LVIF_TEXT: u32 = 0x0001;
const LVNI_SELECTED: isize = 0x0002;
const TVIF_TEXT: u32 = 0x0001;
const TVI_ROOT: isize = 0;
const TVIS_BOLD: u32 = 0x0010;
const TVE_EXPAND: usize = 0x0002;
const TVGN_CARET: usize = 0x0009;
const TVIF_STATE: u32 = 0x0008;
const TVI_LAST: isize = -0x1_0000; // `(HTREEITEM)0xFFFF0000`, sign-extended
const TCIF_TEXT: u32 = 0x0001;

// `InitCommonControlsEx` bits.
const ICC_LISTVIEW_CLASSES: u32 = 0x0000_0001;
const ICC_TREEVIEW_CLASSES: u32 = 0x0000_0002;
const ICC_BAR_CLASSES: u32 = 0x0000_0004;
const ICC_TAB_CLASSES: u32 = 0x0000_0008;
const ICC_UPDOWN_CLASS: u32 = 0x0000_0010;
const ICC_PROGRESS_CLASS: u32 = 0x0000_0020;
const ICC_DATE_CLASSES: u32 = 0x0000_0100;

// Painting, colouring and cursor messages (`winuser.h`).
const WM_PAINT: u32 = 0x000F;
const WM_ERASEBKGND: u32 = 0x0014;
const WM_SETCURSOR: u32 = 0x0020;
const WM_CTLCOLOREDIT: u32 = 0x0133;
const WM_CTLCOLORLISTBOX: u32 = 0x0134;
const WM_CTLCOLORBTN: u32 = 0x0135;
const WM_CTLCOLORSTATIC: u32 = 0x0138;

// `SetWindowPos` and the pseudo handles it takes.
const HWND_TOPMOST: isize = -1;
const HWND_NOTOPMOST: isize = -2;
const SWP_NOSIZE: u32 = 0x0001;
const SWP_NOMOVE: u32 = 0x0002;
const SWP_NOACTIVATE: u32 = 0x0010;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_FRAMECHANGED: u32 = 0x0020;
/// `GetWindowLongW`/`SetWindowLongW`: the window's style word.
const GWL_STYLE: i32 = -16;

// `LoadImage` flags and types.
const LR_LOADFROMFILE: u32 = 0x0010;
const IMAGE_BITMAP: u32 = 0;
const IMAGE_ICON: u32 = 1;

// GDI arguments used by the graphic painter.
const NULL_BRUSH: i32 = 5;
const PS_SOLID: i32 = 0;

// `PeekMessageW` flag and the `GetSystemMetrics` indices used here.
const PM_REMOVE: u32 = 0x0001;
/// `WM_SETCURSOR`'s "over the client area" hit test.
const HTCLIENT: u32 = 1;
const SM_CXSCREEN: i32 = 0;
const SM_CYSCREEN: i32 = 1;

// `CreateFontW` arguments that are never anything else here.
const DEFAULT_CHARSET: u32 = 1;
const OUT_DEFAULT_PRECIS: u32 = 0;
const CLIP_DEFAULT_PRECIS: u32 = 0;
const CLEARTYPE_QUALITY: u32 = 5;
const DEFAULT_PITCH: u32 = 0;
const DEFAULT_GUI_FONT: i32 = 17;

// `AppendMenuW` flags.
const MF_STRING: u32 = 0x0000_0000;

/// The window class every emulated top-level window uses.
const WINDOW_CLASS: &str = "Au3EmulatedWindow";

// ---------------------------------------------------------------------------
// The table a window procedure can reach
// ---------------------------------------------------------------------------

/// Live mappings and everything a window procedure collected for the model.
#[derive(Default)]
struct Shared {
    /// `HWND` (as `usize`) → AutoIt window handle.
    window_ids: HashMap<usize, i64>,
    /// Win32 control/menu id → the AutoIt control id and its kind.
    ///
    /// The kind is here because notification codes are per control class and
    /// collide across classes: `BN_DOUBLECLICKED` and `CBN_EDITCHANGE` are both
    /// 5, and `LBN_SELCHANGE` and `CBN_SELCHANGE` are both 1.
    control_ids: HashMap<i32, (i64, ControlKind)>,
    /// Events for `GUIGetMsg` to drain.
    events: VecDeque<GuiEvent>,
    /// AutoIt control ids whose real state the user changed.
    dirty: BTreeSet<i64>,
    /// Windows the user minimised/maximised/restored.
    states: HashMap<i64, WindowState>,
    /// Control `HWND` → the text and background colours a script set.
    colors: HashMap<usize, (Option<i64>, Option<i64>)>,
    /// Window `HWND` → the background colour `GUISetBkColor` set.
    window_bk: HashMap<usize, i64>,
    /// Graphic control `HWND` → the commands to replay when it paints.
    drawings: HashMap<usize, Drawing>,
    /// Control `HWND` → the cursor identifier a script set.
    cursors: HashMap<usize, i64>,
    /// Brushes made for those colours, so a `WM_CTLCOLOR*` answer can hand one
    /// back. They live as long as the process: there are only as many as there
    /// are distinct colours, and Windows owns the brush a control is painted
    /// with until the window is destroyed.
    brushes: HashMap<i64, usize>,
}

thread_local! {
    /// One table per thread: a window's procedure runs on the thread that
    /// created it, and so does the `GUIGetMsg` that pumps it.
    static SHARED: RefCell<Shared> = RefCell::new(Shared::default());
}

/// Run `f` against this thread's table, unless it is already borrowed (a
/// message dispatched from inside `f` must not re-enter it).
fn with_shared<R>(f: impl FnOnce(&mut Shared) -> R) -> Option<R> {
    SHARED.with(|cell| cell.try_borrow_mut().ok().map(|mut state| f(&mut state)))
}

fn hwnd_key(hwnd: HWND) -> usize {
    hwnd as usize
}

/// The window procedure: Win32's way in, translated into model updates.
///
/// It never touches the model — it only records what happened; the semantics
/// layer turns that into `GUIGetMsg` answers and `GUICtrlRead` results on its
/// next call.
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let answered = with_shared(|state| -> Option<LRESULT> {
        let window = state.window_ids.get(&hwnd_key(hwnd)).copied();
        match message {
            WM_CLOSE => {
                if let Some(handle) = window {
                    state.events.push_back(GuiEvent::Close(handle));
                }
            }
            WM_SYSCOMMAND => {
                if let (Some(handle), true) = (
                    window,
                    matches!(wparam & 0xFFF0, SC_MINIMIZE | SC_MAXIMIZE | SC_RESTORE),
                ) {
                    let new = match wparam & 0xFFF0 {
                        SC_MINIMIZE => WindowState::Minimized,
                        SC_MAXIMIZE => WindowState::Maximized,
                        _ => WindowState::Normal,
                    };
                    state.states.insert(handle, new);
                }
            }
            WM_COMMAND => {
                let id = (wparam & 0xFFFF) as i32;
                let code = ((wparam >> 16) & 0xFFFF) as u32;
                if let Some(&(control, kind)) = state.control_ids.get(&id) {
                    if lparam == 0 {
                        // A menu item: `lParam` carries no control handle.
                        state.events.push_back(GuiEvent::Menu(control));
                    } else if let Some((event, dirty)) = notification(kind, code) {
                        if event {
                            state.events.push_back(GuiEvent::Control(control));
                        }
                        if dirty {
                            state.dirty.insert(control);
                        }
                    }
                }
            }
            // The parent is asked for the brush a child is painted with, which
            // is how `GUICtrlSetBkColor`/`GUICtrlSetColor` reach a real control.
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX | WM_CTLCOLORBTN => {
                let (foreground, background) = state.colors.get(&(lparam as usize)).copied()?;
                let hdc = wparam as HDC;
                if let Some(foreground) = foreground {
                    SetTextColor(hdc, colorref(foreground));
                }
                if let Some(background) = background {
                    SetBkColor(hdc, colorref(background));
                    return Some(brush_for(state, background) as LRESULT);
                }
            }
            // A window colour is painted by the window itself.
            WM_ERASEBKGND => {
                let background = *state.window_bk.get(&hwnd_key(hwnd))?;
                let brush = brush_for(state, background);
                let mut rect: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rect);
                FillRect(wparam as HDC, &rect, brush);
                return Some(1);
            }
            // The control under the pointer decides the cursor.
            WM_SETCURSOR if (lparam & 0xFFFF) as u32 == HTCLIENT => {
                let mut point: POINT = std::mem::zeroed();
                GetCursorPos(&mut point);
                let child = WindowFromPoint(point);
                if let Some(&cursor) = state.cursors.get(&hwnd_key(child)) {
                    let handle = LoadCursorW(std::ptr::null_mut(), cursor as *const u16);
                    if !handle.is_null() {
                        SetCursor(handle);
                        return Some(1);
                    }
                }
            }
            _ => {}
        }
        None
    })
    .flatten();
    answered.unwrap_or_else(|| DefWindowProcW(hwnd, message, wparam, lparam))
}

/// What a `WM_COMMAND` notification means: `(tell the script about it, the user
/// changed the control's state)`.
///
/// The class has to be part of the question. Win32 numbers notifications per
/// control class, and the numbers are reused: `BN_CLICKED` and `LBN_SELCHANGE`
/// are both 0, `LBN_SELCHANGE` and `CBN_SELCHANGE` are both 1,
/// `LBN_DBLCLK`/`CBN_EDITCHANGE` share 5 with `BN_DOUBLECLICKED`, and only the
/// class says which one arrived.
fn notification(kind: ControlKind, code: u32) -> Option<(bool, bool)> {
    match kind {
        ControlKind::Button | ControlKind::Checkbox | ControlKind::Radio => match code {
            BN_CLICKED | BN_DOUBLECLICKED => Some((true, true)),
            _ => None,
        },
        ControlKind::List => match code {
            LBN_SELCHANGE | LBN_DBLCLK => Some((true, true)),
            _ => None,
        },
        ControlKind::Combo => match code {
            CBN_SELCHANGE => Some((true, true)),
            CBN_EDITCHANGE | EN_CHANGE => Some((false, true)),
            _ => None,
        },
        ControlKind::Input | ControlKind::Edit => match code {
            EN_CHANGE => Some((false, true)),
            _ => None,
        },
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The backend
// ---------------------------------------------------------------------------

/// What a graphic control paints: its commands and the two colours they use.
#[derive(Clone)]
struct Drawing {
    commands: Vec<DrawCmd>,
    color: Option<i64>,
    background: Option<i64>,
}

/// An image loaded for a control, and whether it is an icon.
#[derive(Clone)]
struct LoadedImage {
    handle: *mut c_void,
    icon: bool,
    width: i32,
    height: i32,
}

/// One created control, plus the last state pushed into it.
#[derive(Clone)]
struct ControlState {
    /// A copy of the model control this mirrors, so a change to a child — a new
    /// tree node, a colour — can be re-applied without the semantics layer.
    control: Control,
    /// The window it lives in; `0` for the ones without an HWND.
    window: i64,
    hwnd: HWND,
    kind: ControlKind,
    /// The Win32 id the control answers `WM_COMMAND` under.
    win_id: i32,
    text: String,
    checked: bool,
    selection: Option<usize>,
    items: Vec<String>,
    visible: bool,
    enabled: bool,
    /// A font this backend created and therefore has to delete. `None` means the
    /// control carries the stock GUI font — a shared object that must *not* be
    /// deleted.
    font: Option<*mut c_void>,
    /// The model font [`font`](Self::font) was built from.
    font_request: Option<Font>,
    /// Whether a font has been installed at all: without it the first call
    /// cannot tell "stock, not installed yet" from "stock, installed".
    font_set: bool,
    /// The column headings the real `ListView` was built with.
    columns: Vec<String>,
    /// The real tree items, by row.
    tree_items: Vec<isize>,
    /// The image file last loaded into the control.
    image: Option<String>,
    /// The object that file produced, which this backend has to release.
    image_handle: Option<LoadedImage>,
    /// Whether a subclass procedure paints this control.
    subclassed: bool,
}

impl ControlState {
    /// The state for a control that has just been given `hwnd` (`0` when it has
    /// none of its own, as a menu entry or a list row does not).
    fn new(control: &Control, win_id: i32, hwnd: HWND) -> Self {
        Self {
            control: control.clone(),
            window: control.window,
            hwnd,
            kind: control.kind,
            win_id,
            text: control.text.clone(),
            checked: control.is_checked(),
            selection: control.selection,
            items: control.data.clone(),
            visible: control.is_visible(),
            enabled: control.is_enabled(),
            font: None,
            font_request: None,
            font_set: false,
            columns: Vec::new(),
            tree_items: Vec::new(),
            image: None,
            image_handle: None,
            subclassed: false,
        }
    }
}

/// The real-window GUI backend.
///
/// One instance drives one emulated machine: the windows it created, the
/// controls inside them, and the menu each window's items go on.
pub struct Win32Backend {
    windows: HashMap<i64, HWND>,
    controls: HashMap<i64, ControlState>,
    /// `GUICtrlCreateMenu`/`ContextMenu` handles, by AutoIt id (as a raw HMENU).
    menus: HashMap<i64, usize>,
    /// Where `GUICtrlCreateMenuItem` puts the next item, per window.
    current_menu: HashMap<i64, i64>,
    /// The model geometry last pushed into each window, as
    /// `(x, y, client width, client height)`.
    ///
    /// Win32 has no "the user did this" flag, so a window rect that differs from
    /// this is what a drag or a resize looks like from here.
    applied: HashMap<i64, (i32, i32, i32, i32)>,
    /// How much larger a window's frame is than its client area, per window.
    /// Dragging gives a window rectangle, but the model thinks in client sizes.
    frames: HashMap<i64, (i32, i32)>,
    /// Which windows were last pushed on top, so the z-order is only changed
    /// when the model asks for a different one.
    topmost: HashMap<i64, bool>,
    /// The control that was last given the input focus.
    focused: Option<i64>,
    next_win_id: i32,
    registered: bool,
}

impl Default for Win32Backend {
    fn default() -> Self {
        Self::new()
    }
}

impl Win32Backend {
    /// A backend with no windows yet.
    pub fn new() -> Self {
        Self {
            windows: HashMap::new(),
            controls: HashMap::new(),
            menus: HashMap::new(),
            current_menu: HashMap::new(),
            applied: HashMap::new(),
            frames: HashMap::new(),
            topmost: HashMap::new(),
            focused: None,
            next_win_id: 1000,
            registered: false,
        }
    }

    /// Register the window class and the common controls, once per instance.
    fn init(&mut self) {
        if self.registered {
            return;
        }
        self.registered = true;
        unsafe {
            let class_name = to_wide(WINDOW_CLASS);
            let mut class: WNDCLASSEXW = std::mem::zeroed();
            class.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
            class.style = 0x0002 | 0x0001; // CS_HREDRAW | CS_VREDRAW
            class.lpfnWndProc = Some(wnd_proc);
            class.hInstance = GetModuleHandleW(std::ptr::null()) as _;
            // A null class cursor: `DefWindowProcW` answers `WM_SETCURSOR`
            // with the arrow, which is what an AutoIt window shows.
            class.hCursor = 0 as _;
            // `(HBRUSH)(COLOR_WINDOW + 1)`: the system's window background.
            class.hbrBackground = 6 as _;
            class.lpszClassName = class_name.as_ptr();
            RegisterClassExW(&class);

            let mut common: INITCOMMONCONTROLSEX = std::mem::zeroed();
            common.dwSize = std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32;
            common.dwICC = ICC_LISTVIEW_CLASSES
                | ICC_TREEVIEW_CLASSES
                | ICC_BAR_CLASSES
                | ICC_TAB_CLASSES
                | ICC_UPDOWN_CLASS
                | ICC_PROGRESS_CLASS
                | ICC_DATE_CLASSES;
            InitCommonControlsEx(&common);
        }
    }

    /// Create or update the real window behind `window`.
    ///
    /// The model's width and height are the *client* area — that is what AutoIt
    /// means by `GUICreate`'s size — while Win32 wants the whole window, so the
    /// frame `AdjustWindowRectEx` reports is added around them. Everything below
    /// is idempotent: `GUICreate`, `WinMove` and a script's `GUISetState` all
    /// arrive as an updated window, and re-applying a title, a rectangle or a
    /// visibility costs one call each.
    fn sync_window(&mut self, window: &Window) {
        self.init();
        let style = window_style(window) | if window.visible { WS_VISIBLE } else { 0 };
        let exstyle = window.exstyle.max(0) as u32;
        let frame = frame_size(style, exstyle);
        let (client_width, client_height) = (window.width.max(1), window.height.max(1));
        let (width, height) = (client_width + frame.0, client_height + frame.1);
        let hwnd = match self.windows.get(&window.handle).copied() {
            Some(hwnd) => hwnd,
            None => {
                let title = to_wide(&window.title);
                let class = to_wide(WINDOW_CLASS);
                let hwnd = unsafe {
                    CreateWindowExW(
                        exstyle,
                        class.as_ptr(),
                        title.as_ptr(),
                        style,
                        window.x,
                        window.y,
                        width,
                        height,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        GetModuleHandleW(std::ptr::null()),
                        std::ptr::null(),
                    )
                };
                if hwnd.is_null() {
                    return;
                }
                self.windows.insert(window.handle, hwnd);
                let _ = with_shared(|state| state.window_ids.insert(hwnd_key(hwnd), window.handle));
                hwnd
            }
        };
        // Remember what was asked for, so a rectangle that differs later is a
        // user's drag rather than this call's own doing.
        self.frames.insert(window.handle, frame);
        self.applied.insert(
            window.handle,
            (window.x, window.y, client_width, client_height),
        );
        unsafe {
            // Title, geometry and visibility are idempotent and cheap.
            let title = to_wide(&window.title);
            SetWindowTextW(hwnd, title.as_ptr());
            MoveWindow(hwnd, window.x, window.y, width, height, 1);
            let command = if !window.visible {
                SW_HIDE
            } else {
                match window.state {
                    WindowState::Minimized => SW_MINIMIZE,
                    WindowState::Maximized => SW_SHOWMAXIMIZED,
                    WindowState::Normal => SW_SHOWNORMAL,
                }
            };
            ShowWindow(hwnd, command);
            EnableWindow(hwnd, i32::from(window.enabled));
        }
        // The z-order, the focus and the background are model state that only
        // costs a call when it changed.
        if self
            .topmost
            .get(&window.handle)
            .copied()
            .unwrap_or(false)
            != window.topmost
        {
            let insert_after = if window.topmost {
                HWND_TOPMOST
            } else {
                HWND_NOTOPMOST
            };
            unsafe {
                SetWindowPos(
                    hwnd,
                    insert_after as _,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                )
            };
            self.topmost.insert(window.handle, window.topmost);
        }
        if let Some(focus) = window.focus {
            if self.focused != Some(focus) {
                if let Some(state) = self.controls.get(&focus) {
                    if !state.hwnd.is_null() {
                        unsafe { SetFocus(state.hwnd) };
                        self.focused = Some(focus);
                    }
                }
            }
        }
        if let Some(background) = window.bk_color {
            let _ = with_shared(|shared| {
                shared.window_bk.insert(hwnd_key(hwnd), background);
            });
            unsafe { InvalidateRect(hwnd, std::ptr::null(), 1) };
        }
        let _ = &window.cursor;
    }

    /// Create or update the real control behind `control`.
    fn sync_control(&mut self, control: &Control) {
        self.init();
        if self.controls.contains_key(&control.id) {
            self.update_control(control);
            return;
        }
        let Some(parent) = self.windows.get(&control.window).copied() else {
            // The model keeps a control whose window does not exist; the OS has
            // nowhere to put it.
            return;
        };
        // Menus are not child windows.
        match control.kind {
            ControlKind::Menu | ControlKind::ContextMenu => {
                let menu = unsafe { CreatePopupMenu() };
                if menu.is_null() {
                    return;
                }
                self.menus.insert(control.id, menu as usize);
                self.current_menu.insert(control.window, control.id);
                if control.kind == ControlKind::Menu {
                    unsafe { SetMenu(parent, menu) };
                }
                self.controls.insert(
                    control.id,
                    ControlState::new(control, 0, std::ptr::null_mut()),
                );
                return;
            }
            ControlKind::MenuItem => {
                let Some(&menu) = self
                    .current_menu
                    .get(&control.window)
                    .and_then(|id| self.menus.get(id))
                else {
                    return;
                };
                let win_id = self.take_win_id();
                let text = to_wide(&control.text);
                unsafe { AppendMenuW(menu as _, MF_STRING, win_id as usize, text.as_ptr()) };
                let _ = with_shared(|state| {
                    state.control_ids.insert(win_id, (control.id, control.kind))
                });
                self.controls.insert(
                    control.id,
                    ControlState::new(control, win_id, std::ptr::null_mut()),
                );
                return;
            }
            // Parts of another control — a `ListView` row, a tree node, a tab
            // page — are drawn by the control that owns them; the rest have no
            // window of their own. The model still tracks them, and a new one
            // has to show up in its owner right away.
            ControlKind::ListViewItem
            | ControlKind::TreeViewItem
            | ControlKind::TabItem
            | ControlKind::Dummy
            | ControlKind::Avi
            | ControlKind::Obj => {
                self.controls.insert(
                    control.id,
                    ControlState::new(control, 0, std::ptr::null_mut()),
                );
                self.resync_list(control.parent);
                return;
            }
            _ => {}
        }

        let win_id = self.take_win_id();
        let class = to_wide(class_name(control.kind));
        let text = to_wide(&control.text);
        let hwnd = unsafe {
            CreateWindowExW(
                control_exstyle(control),
                class.as_ptr(),
                text.as_ptr(),
                control_style(control),
                control.x,
                control.y,
                control.width.max(1),
                control.height.max(1),
                parent,
                win_id as _,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return;
        }
        let _ = with_shared(|state| state.control_ids.insert(win_id, (control.id, control.kind)));
        let mut created = ControlState::new(control, win_id, hwnd);
        unsafe {
            EnableWindow(hwnd, i32::from(created.enabled));
            if matches!(created.kind, ControlKind::Checkbox | ControlKind::Radio) {
                let check = if created.checked { 1 } else { 0 };
                SendMessageW(hwnd, BM_SETCHECK, check as usize, 0);
            }
            if let Some((limit, _)) = control.limit {
                if limit > 0 {
                    SendMessageW(hwnd, EM_SETLIMITTEXT, limit as usize, 0);
                }
            }
        }
        self.push_items(&mut created, control);
        apply_font(&mut created, control);
        self.apply_buddy(&created, control);
        self.apply_image(&mut created, control);
        self.apply_value(&mut created, control);
        self.apply_colors(&mut created, control);
        self.apply_cursor(&mut created, control);
        self.apply_subclass(&mut created, control);
        self.controls.insert(control.id, created);
    }

    /// Re-apply a control's model state to its real HWND.
    fn update_control(&mut self, control: &Control) {
        if !self.controls.contains_key(&control.id) {
            return;
        }
        if let Some(state) = self.controls.get_mut(&control.id) {
            if state.hwnd.is_null() {
                // A part: its row or node lives in the owner, which is
                // refreshed below.
                state.control = control.clone();
                state.text = control.text.clone();
                state.items = control.data.clone();
                state.selection = control.selection;
                state.visible = control.is_visible();
                state.enabled = control.is_enabled();
            } else {
                let hwnd = state.hwnd;
                unsafe {
                    // A text the model changed has to reach the control —
                    // except where the control is the text the user types into
                    // or the items it shows.
                    // A style the script changed after creation is applied to
                    // the real control, which is how `GUICtrlSetStyle` and
                    // `ControlListView(... "ViewChange")` take effect.
                    let wanted_style = control_style(control);
                    if state.control.style != control.style {
                        SetWindowLongW(hwnd, GWL_STYLE, wanted_style as i32);
                        SetWindowPos(
                            hwnd,
                            std::ptr::null_mut(),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
                        );
                    }
                    if state.text != control.text
                        && !matches!(
                            control.kind,
                            ControlKind::Input
                                | ControlKind::Edit
                                | ControlKind::List
                                | ControlKind::Combo
                                | ControlKind::ListView
                                | ControlKind::TreeView
                                | ControlKind::Tab
                        )
                    {
                        let text = to_wide(&control.text);
                        SetWindowTextW(hwnd, text.as_ptr());
                    }
                    if state.visible != control.is_visible() {
                        ShowWindow(
                            hwnd,
                            if control.is_visible() {
                                SW_SHOW
                            } else {
                                SW_HIDE
                            },
                        );
                    }
                    if state.enabled != control.is_enabled() {
                        EnableWindow(hwnd, i32::from(control.is_enabled()));
                    }
                    MoveWindow(
                        hwnd,
                        control.x,
                        control.y,
                        control.width.max(1),
                        control.height.max(1),
                        1,
                    );
                    if state.checked != control.is_checked()
                        && matches!(control.kind, ControlKind::Checkbox | ControlKind::Radio)
                    {
                        let check = if control.is_checked() { 1 } else { 0 };
                        SendMessageW(hwnd, BM_SETCHECK, check as usize, 0);
                    }
                    if state.selection != control.selection {
                        match control.kind {
                            ControlKind::List => {
                                SendMessageW(
                                    hwnd,
                                    LB_SETCURSEL,
                                    control.selection.unwrap_or(usize::MAX),
                                    0,
                                );
                            }
                            ControlKind::Combo => {
                                SendMessageW(
                                    hwnd,
                                    CB_SETCURSEL,
                                    control.selection.unwrap_or(usize::MAX),
                                    0,
                                );
                            }
                            _ => {}
                        }
                    }
                }
                state.control = control.clone();
                state.text = control.text.clone();
                state.selection = control.selection;
                state.checked = control.is_checked();
                state.visible = control.is_visible();
                state.enabled = control.is_enabled();
            }
        }
        let Some(mut state) = self.controls.get(&control.id).cloned() else {
            return;
        };
        if state.hwnd.is_null() {
            self.resync_list(control.parent);
            return;
        }
        if state.items != control.data {
            self.push_items(&mut state, control);
            state.items = control.data.clone();
        }
        apply_font(&mut state, control);
        self.apply_image(&mut state, control);
        self.apply_value(&mut state, control);
        self.apply_colors(&mut state, control);
        self.apply_cursor(&mut state, control);
        self.apply_subclass(&mut state, control);
        self.controls.insert(control.id, state);
    }

    /// The parts of `owner`, as `(row, the row of the part they hang under)`.
    fn parts_of(&self, owner: i64) -> Vec<(usize, Option<usize>)> {
        let mut parts = Vec::new();
        for state in self.controls.values() {
            let part = &state.control;
            if part.parent.is_none() || self.list_owner_in(part.id) != Some(owner) {
                continue;
            }
            let Some(row) = part.row else {
                continue;
            };
            let parent_row = part
                .parent
                .and_then(|parent| self.controls.get(&parent))
                .and_then(|parent| parent.control.row);
            parts.push((row, parent_row));
        }
        parts
    }

    /// The control that owns the list `id` is a row of: a `TreeViewItem` hangs
    /// off other items, so the tree at the top of the chain is the owner.
    fn list_owner_in(&self, id: i64) -> Option<i64> {
        let control = &self.controls.get(&id)?.control;
        if control.kind != ControlKind::TreeViewItem {
            return Some(id);
        }
        self.list_owner_in(control.parent?)
    }

    /// Re-apply the control that owns a list after one of its rows changed.
    fn resync_list(&mut self, owner: Option<i64>) {
        let Some(owner) = owner else {
            return;
        };
        let Some(list) = self.list_owner_in(owner) else {
            return;
        };
        let Some(control) = self.controls.get(&list).map(|state| state.control.clone()) else {
            return;
        };
        let Some(mut state) = self.controls.get(&list).cloned() else {
            return;
        };
        if state.hwnd.is_null() {
            return;
        }
        self.push_items(&mut state, &control);
        self.apply_value(&mut state, &control);
        state.items = control.data.clone();
        self.controls.insert(list, state);
    }

    /// Push the model's items into the real control.
    ///
    /// A list's rows, a tree's nodes and a tab's pages are all *parts* of the
    /// control: the row texts live in its `data`, and the order, the nesting and
    /// the per-item state come from the parts the model holds. Both are read
    /// here, which is also what makes a row that was just added show up.
    fn push_items(&mut self, state: &mut ControlState, control: &Control) {
        let hwnd = state.hwnd;
        if hwnd.is_null() {
            return;
        }
        match control.kind {
            ControlKind::List => {
                unsafe {
                    SendMessageW(hwnd, LB_RESETCONTENT, 0, 0);
                    for item in &control.data {
                        let text = to_wide(item);
                        SendMessageW(hwnd, LB_ADDSTRING, 0, text.as_ptr() as isize);
                    }
                }
            }
            ControlKind::Combo => {
                unsafe {
                    SendMessageW(hwnd, CB_RESETCONTENT, 0, 0);
                    for item in &control.data {
                        let text = to_wide(item);
                        SendMessageW(hwnd, CB_ADDSTRING, 0, text.as_ptr() as isize);
                    }
                }
            }
            ControlKind::ListView => {
                self.push_columns(state, control);
                unsafe {
                    SendMessageW(hwnd, LVM_DELETEALLITEMS, 0, 0);
                    for (index, row) in control.data.iter().enumerate() {
                        let mut cells = row.split('|');
                        let mut first = to_wide(cells.next().unwrap_or(""));
                        let mut item: LVITEMW = std::mem::zeroed();
                        item.mask = LVIF_TEXT;
                        item.iItem = index as i32;
                        item.pszText = first.as_mut_ptr();
                        SendMessageW(hwnd, LVM_INSERTITEMW, 0, &mut item as *mut _ as isize);
                        // What is left of the row goes into the subitems, so a
                        // script that writes "a|b|c" gets three columns.
                        for (column, cell) in cells.enumerate() {
                            let mut text = to_wide(cell);
                            let mut sub: LVITEMW = std::mem::zeroed();
                            sub.mask = LVIF_TEXT;
                            sub.iItem = index as i32;
                            sub.iSubItem = column as i32 + 1;
                            sub.pszText = text.as_mut_ptr();
                            SendMessageW(hwnd, LVM_SETITEMW, 0, &mut sub as *mut _ as isize);
                        }
                    }
                    // Rebuilding the rows drops the selection with them.
                    if let Some(selected) = control.selection {
                        if selected < control.data.len() {
                            let mut item: LVITEMW = std::mem::zeroed();
                            item.state = LVIS_SELECTED | LVIS_FOCUSED;
                            item.stateMask = LVIS_SELECTED | LVIS_FOCUSED;
                            SendMessageW(
                                hwnd,
                                LVM_SETITEMSTATE,
                                selected,
                                &mut item as *mut _ as isize,
                            );
                        }
                    }
                }
            }
            ControlKind::TreeView => self.push_tree(state, control),
            ControlKind::Tab => unsafe {
                SendMessageW(hwnd, TCM_DELETEALLITEMS, 0, 0);
                for (index, title) in control.data.iter().enumerate() {
                    let mut text = to_wide(title);
                    let mut tab: TCITEMW = std::mem::zeroed();
                    tab.mask = TCIF_TEXT;
                    tab.pszText = text.as_mut_ptr();
                    SendMessageW(hwnd, TCM_INSERTITEMW, index, &mut tab as *mut _ as isize);
                }
            },
            _ => {}
        }
    }

    /// Give a `ListView` the column headings its text spells out.
    ///
    /// The heading text is used as it is (AutoIt lets a script pad it with
    /// blanks to choose the width), and the column is then sized to it.
    fn push_columns(&mut self, state: &mut ControlState, control: &Control) {
        let columns: Vec<String> = if control.text.is_empty() {
            Vec::new()
        } else {
            control.text.split('|').map(|heading| heading.to_string()).collect()
        };
        // Report view shows nothing without a column, so a `ListView` that was
        // given rows but no headings gets one.
        let columns = if columns.is_empty() && !control.data.is_empty() {
            vec![String::new()]
        } else {
            columns
        };
        if state.columns == columns {
            return;
        }
        let hwnd = state.hwnd;
        for (index, heading) in columns.iter().enumerate() {
            let mut text = to_wide(heading);
            let mut column: LVCOLUMNW = unsafe { std::mem::zeroed() };
            let message = if index < state.columns.len() {
                column.mask = LVCF_TEXT;
                LVM_SETCOLUMNW
            } else {
                column.mask = LVCF_TEXT | LVCF_WIDTH;
                column.cx = 90;
                LVM_INSERTCOLUMNW
            };
            column.pszText = text.as_mut_ptr();
            unsafe {
                SendMessageW(hwnd, message, index, &mut column as *mut _ as isize);
                // `LVSCW_AUTOSIZE_USEHEADER`: size the column to its heading.
                SendMessageW(hwnd, LVM_SETCOLUMNWIDTH, index, LVSCW_AUTOSIZE_USEHEADER);
            }
        }
        state.columns = columns;
    }

    /// Give a `TreeView` the nodes the model holds, nesting them the way the
    /// item controls do.
    fn push_tree(&mut self, state: &mut ControlState, control: &Control) {
        let parents: HashMap<usize, Option<usize>> = self.parts_of(control.id).into_iter().collect();
        let hwnd = state.hwnd;
        unsafe { SendMessageW(hwnd, TVM_DELETEITEM, 0, TVI_ROOT) };
        state.tree_items = vec![TVI_ROOT; control.data.len()];
        for row in 0..control.data.len() {
            let parent_row = parents.get(&row).copied().flatten();
            let Some(text) = control.data.get(row) else {
                continue;
            };
            let mut text = to_wide(text);
            let mut insert: TVINSERTSTRUCTW = unsafe { std::mem::zeroed() };
            insert.hParent = match parent_row.and_then(|row| state.tree_items.get(row)).copied() {
                Some(item) => item,
                None => TVI_ROOT,
            };
            insert.hInsertAfter = TVI_LAST;
            insert.Anonymous.item.mask = TVIF_TEXT;
            insert.Anonymous.item.pszText = text.as_mut_ptr();
            let item = unsafe {
                SendMessageW(hwnd, TVM_INSERTITEMW, 0, &mut insert as *mut _ as isize) as isize
            };
            if let Some(slot) = state.tree_items.get_mut(row) {
                *slot = item;
            }
            // An item carries its own state: bold (`$GUI_DEFBUTTON`), expanded
            // (`$GUI_EXPAND`) and, when it is the selected one, the caret.
            let part = self
                .controls
                .values()
                .find(|candidate| {
                    candidate.control.row == Some(row)
                        && self.list_owner_in(candidate.control.id) == Some(control.id)
                })
                .map(|candidate| candidate.control.clone());
            if let Some(part) = part {
                if part.state & 0x200 != 0 {
                    unsafe {
                        let mut item: TVITEMW = std::mem::zeroed();
                        item.mask = TVIF_STATE;
                        item.hItem = self.tree_item(state, row);
                        item.state = TVIS_BOLD;
                        item.stateMask = TVIS_BOLD;
                        SendMessageW(hwnd, TVM_SETITEMW, 0, &mut item as *mut _ as isize);
                    }
                }
                if part.state & 0x400 != 0 {
                    unsafe {
                        SendMessageW(hwnd, TVM_EXPAND, TVE_EXPAND, self.tree_item(state, row))
                    };
                }
            }
        }
        if let Some(caret) = control.selection.and_then(|row| state.tree_items.get(row)) {
            unsafe { SendMessageW(hwnd, TVM_SELECTITEM, TVGN_CARET, *caret) };
        }
    }

    fn tree_item(&self, state: &ControlState, row: usize) -> isize {
        state.tree_items.get(row).copied().unwrap_or(TVI_ROOT)
    }

    /// Give a `Progress`, `Slider`, `Updown` or `Tab` the value the model holds.
    fn apply_value(&mut self, state: &mut ControlState, control: &Control) {
        if state.hwnd.is_null() {
            return;
        }
        let hwnd = state.hwnd;
        match control.kind {
            ControlKind::Progress => {
                let value = control.text.trim().parse::<i64>().unwrap_or(0);
                unsafe { SendMessageW(hwnd, PBM_SETPOS, value.max(0) as usize, 0) };
            }
            ControlKind::Slider => {
                let (min, max) = control.limit.unwrap_or((0, 100));
                let value = control.text.trim().parse::<i64>().unwrap_or(min);
                unsafe {
                    SendMessageW(hwnd, TBM_SETRANGE, 1, pack16(min as i32, max as i32));
                    SendMessageW(hwnd, TBM_SETPOS, 1, value as isize);
                }
            }
            ControlKind::Updown => {
                let (min, max) = control.limit.unwrap_or((0, 100));
                let value = control.text.trim().parse::<i64>().unwrap_or(min);
                unsafe {
                    SendMessageW(hwnd, UDM_SETRANGE32, min.max(0) as usize, max as isize);
                    SendMessageW(hwnd, UDM_SETPOS32, 0, value as isize);
                }
            }
            ControlKind::Tab => {
                if let Some(index) = control.selection {
                    unsafe { SendMessageW(hwnd, TCM_SETCURSEL, index, 0) };
                }
            }
            _ => {}
        }
    }

    /// Load the picture a `Pic`/`Icon`/`Button` control asks for.
    ///
    /// A static control takes `STM_SETIMAGE`/`STM_SETICON`; a button — and a
    /// checkbox with `$BS_PUSHLIKE`, the only other kind the help page names —
    /// takes `BM_SETIMAGE`, and its `$BS_ICON`/`$BS_BITMAP` style says which.
    fn apply_image(&mut self, state: &mut ControlState, control: &Control) {
        if state.hwnd.is_null() {
            return;
        }
        let wanted = match control.kind {
            // A `Pic`/`Icon` is created with its filename as the caption.
            ControlKind::Pic | ControlKind::Icon => control
                .image
                .clone()
                .filter(|path| !path.is_empty())
                .or_else(|| (!control.text.is_empty()).then(|| control.text.clone())),
            _ => control.image.clone(),
        };
        if state.image == wanted {
            return;
        }
        if let Some(old) = state.image_handle.take() {
            unsafe {
                if old.icon {
                    DestroyIcon(old.handle);
                } else {
                    DeleteObject(old.handle);
                }
            }
        }
        let button = matches!(control.kind, ControlKind::Button | ControlKind::Checkbox);
        let styled_icon =
            button && control.style > 0 && control.style as u32 & BS_ICON != 0;
        let icon = control.kind == ControlKind::Icon || styled_icon;
        let message = if button {
            BM_SETIMAGE
        } else if icon {
            STM_SETICON
        } else {
            STM_SETIMAGE
        };
        let kind = if icon { IMAGE_ICON } else { IMAGE_BITMAP };
        state.image = wanted.clone();
        let Some(path) = wanted else {
            unsafe { SendMessageW(state.hwnd, message, kind as usize, 0) };
            return;
        };
        let Some(loaded) = load_image(&path, icon) else {
            return;
        };
        unsafe { SendMessageW(state.hwnd, message, kind as usize, loaded.handle as isize) };
        // A picture with no size of its own takes the file's size.
        if !icon && control.width == 0 && control.height == 0 && loaded.width > 0 {
            unsafe {
                MoveWindow(
                    state.hwnd,
                    control.x,
                    control.y,
                    loaded.width,
                    loaded.height,
                    1,
                )
            };
        }
        state.image_handle = Some(loaded);
    }

    /// Remember the colours a control was given, which its parent's window
    /// procedure hands back on `WM_CTLCOLOR*`.
    fn apply_colors(&mut self, state: &mut ControlState, control: &Control) {
        if state.hwnd.is_null() {
            return;
        }
        let hwnd = state.hwnd;
        let _ = with_shared(|shared| {
            shared.colors.insert(hwnd_key(hwnd), (control.color, control.bk_color));
        });
        unsafe { InvalidateRect(hwnd, std::ptr::null(), 1) };
    }

    /// Remember the cursor a script put over a control.
    fn apply_cursor(&mut self, state: &mut ControlState, control: &Control) {
        if state.hwnd.is_null() {
            return;
        }
        if let Some(cursor) = control.cursor {
            let hwnd = state.hwnd;
            let _ = with_shared(|shared| {
                shared.cursors.insert(hwnd_key(hwnd), cursor);
            });
        }
    }

    /// Attach an updown to the input control it was created for.
    ///
    /// `GUICtrlCreateUpdown($input)` takes an input rather than a position, and
    /// the arrow pair is positioned by the control itself once it knows its
    /// buddy.
    fn apply_buddy(&mut self, state: &ControlState, control: &Control) {
        if control.kind != ControlKind::Updown || state.hwnd.is_null() {
            return;
        }
        let Some(buddy) = control
            .parent
            .and_then(|parent| self.controls.get(&parent))
            .filter(|buddy| !buddy.hwnd.is_null())
            .map(|buddy| buddy.hwnd)
        else {
            return;
        };
        unsafe { SendMessageW(state.hwnd, UDM_SETBUDDY, buddy as usize, 0) };
    }

    /// Keep the drawing commands of a graphic control where its paint procedure
    /// can reach them.
    fn apply_subclass(&mut self, state: &mut ControlState, control: &Control) {
        if state.hwnd.is_null() || control.kind != ControlKind::Graphic {
            return;
        }
        let hwnd = state.hwnd;
        let _ = with_shared(|shared| {
            shared.drawings.insert(
                hwnd_key(hwnd),
                Drawing {
                    commands: control.draw.clone(),
                    color: control.color,
                    background: control.bk_color,
                },
            );
        });
        if !state.subclassed {
            unsafe { SetWindowSubclass(hwnd, Some(graphic_proc), 1, 0) };
            state.subclassed = true;
        }
        unsafe { InvalidateRect(hwnd, std::ptr::null(), 1) };
    }

    fn take_win_id(&mut self) -> i32 {
        let id = self.next_win_id;
        self.next_win_id += 1;
        id
    }

    /// Pump this thread's queue: the only way a real window makes progress.
    fn pump() {
        unsafe {
            let mut message: MSG = std::mem::zeroed();
            while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                // `IsDialogMessageW` is what gives a GUI its keyboard habits:
                // Tab and the arrow keys move between controls, Return presses
                // the default button and Escape closes the window. AutoIt's own
                // message loop does the same, so a script sees the same events.
                let mut handled = false;
                let _ = with_shared(|shared| {
                    for hwnd in shared.window_ids.keys() {
                        let hwnd = *hwnd as HWND;
                        if IsDialogMessageW(hwnd, &message) != 0 {
                            handled = true;
                            break;
                        }
                    }
                });
                if !handled {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
    }

    /// The selections a tree or a tab made that `WM_COMMAND` does not carry.
    fn selection_updates(&mut self) -> Vec<GuiUpdate> {
        let mut updates = Vec::new();
        let ids: Vec<i64> = self.controls.keys().copied().collect();
        for id in ids {
            let Some(state) = self.controls.get(&id) else {
                continue;
            };
            if state.hwnd.is_null() {
                continue;
            }
            let index = match state.kind {
                ControlKind::TreeView => {
                    let item = unsafe {
                        SendMessageW(state.hwnd, TVM_GETNEXTITEM, TVGN_CARET, 0) as isize
                    };
                    state
                        .tree_items
                        .iter()
                        .position(|candidate| *candidate == item)
                }
                ControlKind::Tab => {
                    let index = unsafe { SendMessageW(state.hwnd, TCM_GETCURSEL, 0, 0) };
                    (index >= 0).then_some(index as usize)
                }
                _ => None,
            };
            if let Some(index) = index {
                if let Some(state) = self.controls.get_mut(&id) {
                    if state.selection != Some(index) {
                        state.selection = Some(index);
                        updates.push(GuiUpdate::Select { id, index });
                    }
                }
            }
        }
        updates
    }

    /// The text currently in a real edit control.
    fn read_text(hwnd: HWND) -> String {
        unsafe {
            let len = GetWindowTextLengthW(hwnd);
            if len <= 0 {
                return String::new();
            }
            let mut buffer = vec![0u16; len as usize + 1];
            let written = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
            String::from_utf16_lossy(&buffer[..written.max(0) as usize])
        }
    }

    /// Drop everything this backend holds for one control.
    ///
    /// A control's HWND is normally already gone — a window destroys its
    /// children — so the destroy is best-effort; the font, the menu and the id
    /// mapping are not, and leaking the mapping would leave a stale Win32 id
    /// answering `WM_COMMAND`.
    fn forget_control(&mut self, id: i64) {
        let Some(state) = self.controls.remove(&id) else {
            return;
        };
        if !state.hwnd.is_null() && unsafe { IsWindow(state.hwnd) } != 0 {
            unsafe { DestroyWindow(state.hwnd) };
        }
        if let Some(menu) = self.menus.remove(&id) {
            unsafe { DestroyMenu(menu as _) };
        }
        if let Some(font) = state.font {
            if !font.is_null() {
                unsafe { DeleteObject(font as _) };
            }
        }
        if let Some(image) = state.image_handle {
            unsafe {
                if image.icon {
                    DestroyIcon(image.handle);
                } else {
                    DeleteObject(image.handle);
                }
            }
        }
        let _ = with_shared(|shared| {
            shared.control_ids.remove(&state.win_id);
            shared.dirty.remove(&id);
            shared.colors.remove(&hwnd_key(state.hwnd));
            shared.drawings.remove(&hwnd_key(state.hwnd));
            shared.cursors.remove(&hwnd_key(state.hwnd));
        });
    }

    /// The moves and resizes the *user* made since the last call.
    ///
    /// Win32 tags nothing as user-made, so this asks each window for its
    /// rectangle and compares it with the one this backend last pushed down: a
    /// difference is a drag or a resize. Working from the rectangle rather than
    /// from `WM_MOVE`/`WM_SIZE` also means the two never argue — those messages
    /// also fire for this backend's own `MoveWindow`, and a client size taken
    /// from them would disagree with the model's, resizing the window a little
    /// more on every frame.
    fn geometry_updates(&mut self) -> Vec<GuiUpdate> {
        let mut updates = Vec::new();
        for (&handle, &hwnd) in &self.windows {
            if hwnd.is_null() || unsafe { IsWindow(hwnd) } == 0 {
                continue;
            }
            // A minimised window is parked at -32000, and a maximised one covers
            // the desktop: neither is a user's drag, and both report a rectangle
            // the model did not ask for. The state change itself arrives through
            // `WM_SYSCOMMAND` instead.
            if unsafe { IsIconic(hwnd) } != 0 || unsafe { IsZoomed(hwnd) } != 0 {
                continue;
            }
            let mut rect: RECT = unsafe { std::mem::zeroed() };
            if unsafe { GetWindowRect(hwnd, &mut rect) } == 0 {
                continue;
            }
            let frame = self.frames.get(&handle).copied().unwrap_or((0, 0));
            let (x, y) = (rect.left, rect.top);
            let width = (rect.right - rect.left - frame.0).max(1);
            let height = (rect.bottom - rect.top - frame.1).max(1);
            let previous = self.applied.insert(handle, (x, y, width, height));
            let (moved, resized) = match previous {
                Some((ax, ay, aw, ah)) => {
                    if (ax, ay, aw, ah) == (x, y, width, height) {
                        continue;
                    }
                    (ax != x || ay != y, aw != width || ah != height)
                }
                // No geometry was ever pushed: this is the first look at it.
                None => (true, true),
            };
            if moved {
                updates.push(GuiUpdate::Move { handle, x, y });
            }
            if resized {
                updates.push(GuiUpdate::Resize {
                    handle,
                    width,
                    height,
                });
            }
        }
        updates
    }
}

impl GuiBackend for Win32Backend {
    fn on_window(&mut self, window: &Window) {
        self.sync_window(window);
    }

    fn on_window_removed(&mut self, handle: i64) {
        if let Some(hwnd) = self.windows.remove(&handle) {
            unsafe { DestroyWindow(hwnd) };
            let _ = with_shared(|state| state.window_ids.remove(&hwnd_key(hwnd)));
        }
        self.applied.remove(&handle);
        self.frames.remove(&handle);
        // Its children went with it, but their fonts, their id table entries and
        // any menu they own are this side's to release.
        let children: Vec<i64> = self
            .controls
            .iter()
            .filter(|(_, control)| control.window == handle)
            .map(|(id, _)| *id)
            .collect();
        for id in children {
            self.forget_control(id);
        }
        self.current_menu.remove(&handle);
    }

    fn on_control(&mut self, control: &Control) {
        self.sync_control(control);
    }

    fn on_control_removed(&mut self, id: i64) {
        self.forget_control(id);
    }

    /// `GUISetState` calls this: with real windows the frame has to be painted
    /// before the script's next line can rely on what is on screen.
    fn present(&mut self) {
        Self::pump();
        for hwnd in self.windows.values() {
            if unsafe { IsWindow(*hwnd) } != 0 {
                unsafe {
                    InvalidateRect(*hwnd, std::ptr::null(), 1);
                    UpdateWindow(*hwnd);
                }
            }
        }
    }

    fn poll(&mut self) -> Vec<GuiEvent> {
        Self::pump();
        with_shared(|state| state.events.drain(..).collect()).unwrap_or_default()
    }

    fn take_updates(&mut self) -> Vec<GuiUpdate> {
        let (dirty, states) = match with_shared(|state| {
            (
                std::mem::take(&mut state.dirty),
                std::mem::take(&mut state.states),
            )
        }) {
            Some(parts) => parts,
            None => return Vec::new(),
        };
        let mut updates = Vec::new();
        for (handle, state) in states {
            updates.push(GuiUpdate::SetWindowState { handle, state });
        }
        updates.extend(self.geometry_updates());
        // A tree or a tab reports a selection change through `WM_NOTIFY`, which
        // the window procedure does not read; both are asked instead.
        updates.extend(self.selection_updates());
        for id in dirty {
            let Some(state) = self.controls.get_mut(&id) else {
                continue;
            };
            if state.hwnd.is_null() {
                continue;
            }
            match state.kind {
                ControlKind::Input | ControlKind::Edit => {
                    let text = Self::read_text(state.hwnd);
                    if state.text != text {
                        state.text = text.clone();
                        updates.push(GuiUpdate::SetText { id, text });
                    }
                }
                ControlKind::Checkbox | ControlKind::Radio => {
                    let checked = unsafe { SendMessageW(state.hwnd, BM_GETCHECK, 0, 0) } != 0;
                    if state.checked != checked {
                        state.checked = checked;
                        updates.push(GuiUpdate::SetChecked { id, checked });
                    }
                }
                ControlKind::List => {
                    let index = unsafe { SendMessageW(state.hwnd, LB_GETCURSEL, 0, 0) };
                    let index = usize::try_from(index).ok();
                    if index.is_some() && state.selection != index {
                        state.selection = index;
                        updates.push(GuiUpdate::Select {
                            id,
                            index: index.unwrap_or(0),
                        });
                    }
                }
                ControlKind::Combo => {
                    let index = unsafe { SendMessageW(state.hwnd, CB_GETCURSEL, 0, 0) };
                    let index = usize::try_from(index).ok();
                    if index.is_some() && state.selection != index {
                        state.selection = index;
                        updates.push(GuiUpdate::Select {
                            id,
                            index: index.unwrap_or(0),
                        });
                    }
                }
                ControlKind::ListView => {
                    let index = unsafe {
                        SendMessageW(state.hwnd, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED)
                    };
                    let index = usize::try_from(index).ok();
                    if index.is_some() && state.selection != index {
                        state.selection = index;
                        updates.push(GuiUpdate::Select {
                            id,
                            index: index.unwrap_or(0),
                        });
                    }
                }
                _ => {}
            }
        }
        updates
    }

    /// Ask the real control, which knows what the model only approximates.
    ///
    /// A message whose `lParam` is a pointer is *not* forwarded: the pointer a
    /// script holds addresses the emulation's own memory, not the real control's,
    /// so the model answers those instead.
    fn send_message(
        &mut self,
        id: i64,
        message: u32,
        wparam: usize,
        lparam: isize,
    ) -> Option<i64> {
        let state = self.controls.get(&id)?;
        if state.hwnd.is_null() {
            return None;
        }
        let result = unsafe { SendMessageW(state.hwnd, message, wparam, lparam) };
        Some(result as i64)
    }

    fn frame_size(&self, window: &Window) -> (i32, i32) {
        let style = window_style(window) | if window.visible { WS_VISIBLE } else { 0 };
        frame_size(style, window.exstyle.max(0) as u32)
    }

    fn snapshot(&mut self) -> Option<GuiImage> {
        // A real window can be screenshotted, but nothing asks for it yet.
        None
    }

    fn desktop_size(&self) -> Option<(i32, i32)> {
        let (width, height) =
            unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        (width > 0 && height > 0).then_some((width, height))
    }
}

impl Drop for Win32Backend {
    fn drop(&mut self) {
        for (_, control) in self.controls.drain() {
            if !control.hwnd.is_null() {
                unsafe { DestroyWindow(control.hwnd) };
            }
            if let Some(font) = control.font {
                if !font.is_null() {
                    unsafe { DeleteObject(font as _) };
                }
            }
            if let Some(image) = control.image_handle {
                unsafe {
                    if image.icon {
                        DestroyIcon(image.handle);
                    } else {
                        DeleteObject(image.handle);
                    }
                }
            }
        }
        for (_, menu) in self.menus.drain() {
            unsafe { DestroyMenu(menu as _) };
        }
        for hwnd in self.windows.drain().map(|(_, hwnd)| hwnd) {
            unsafe { DestroyWindow(hwnd) };
            with_shared(|state| state.window_ids.remove(&hwnd_key(hwnd)));
        }
    }
}

/// The style a top-level window is created with.
///
/// AutoIt's `$GUI_SS_DEFAULT_GUI` *is* `WS_OVERLAPPEDWINDOW`, so a script's
/// style goes straight through; `-1`/`0` (AutoIt's "default") becomes it.
fn window_style(window: &Window) -> u32 {
    if window.style > 0 {
        window.style as u32
    } else {
        WS_OVERLAPPEDWINDOW
    }
}

/// The Win32 window class behind a control kind.
fn class_name(kind: ControlKind) -> &'static str {
    match kind {
        ControlKind::Label | ControlKind::Graphic => "STATIC",
        ControlKind::Group | ControlKind::Button | ControlKind::Checkbox | ControlKind::Radio => {
            "BUTTON"
        }
        ControlKind::Input | ControlKind::Edit => "EDIT",
        ControlKind::List => "LISTBOX",
        ControlKind::Combo => "COMBOBOX",
        ControlKind::ListView => "SysListView32",
        ControlKind::TreeView => "SysTreeView32",
        ControlKind::Tab => "SysTabControl32",
        ControlKind::Progress => "msctls_progress32",
        ControlKind::Slider => "msctls_trackbar32",
        ControlKind::Updown => "msctls_updown32",
        ControlKind::Date => "SysDateTimePick32",
        ControlKind::MonthCal => "SysMonthCal32",
        ControlKind::Pic => "STATIC",
        ControlKind::Icon => "STATIC",
        // The rest never reach `CreateWindowExW` (see `sync_control`).
        ControlKind::ListViewItem
        | ControlKind::TreeViewItem
        | ControlKind::TabItem
        | ControlKind::Menu
        | ControlKind::MenuItem
        | ControlKind::ContextMenu
        | ControlKind::Dummy
        | ControlKind::Avi
        | ControlKind::Obj => "STATIC",
    }
}

/// The `WS_*`/class styles a child control is created with.
///
/// The defaults and the forced styles are the ones the help page of each
/// `GUICtrlCreate*` names, so a control a script gave no style to still looks
/// the way AutoIt would have made it.
fn control_style(control: &Control) -> u32 {
    let script = if control.style > 0 {
        control.style as u32
    } else {
        0
    };
    let readonly = script & ES_READONLY != 0;
    let base = match control.kind {
        ControlKind::Label => SS_NOTIFY | SS_LEFT,
        ControlKind::Group => WS_GROUP | BS_GROUPBOX,
        ControlKind::Button => 0,
        ControlKind::Checkbox => BS_AUTOCHECKBOX,
        ControlKind::Radio => BS_AUTORADIOBUTTON,
        // An input is single-line; `$ES_MULTILINE` is reset for it.
        ControlKind::Input => ES_AUTOHSCROLL | WS_BORDER,
        ControlKind::Edit => {
            ES_MULTILINE | ES_AUTOVSCROLL | ES_AUTOHSCROLL | ES_WANTRETURN | WS_VSCROLL
                | WS_HSCROLL
                | WS_BORDER
        }
        ControlKind::List => LBS_NOTIFY | LBS_SORT | LBS_NOINTEGRALHEIGHT | WS_BORDER | WS_VSCROLL,
        ControlKind::Combo => CBS_DROPDOWN | CBS_AUTOHSCROLL | CBS_HASSTRINGS | WS_VSCROLL,
        ControlKind::ListView => LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | WS_BORDER,
        ControlKind::TreeView => {
            TVS_HASBUTTONS | TVS_HASLINES | TVS_LINESATROOT | TVS_DISABLEDRAGDROP
                | TVS_SHOWSELALWAYS
        }
        ControlKind::Tab => TCS_TOOLTIPS | WS_CLIPSIBLINGS,
        ControlKind::Progress => PBS_SMOOTH,
        ControlKind::Slider => TBS_AUTOTICKS,
        ControlKind::Updown => UDS_ALIGNRIGHT | UDS_ARROWKEYS | UDS_SETBUDDYINT,
        ControlKind::Date => DTS_LONGDATEFORMAT,
        ControlKind::MonthCal => MCS_NOTODAY,
        ControlKind::Pic => SS_NOTIFY | SS_BITMAP | SS_CENTERIMAGE,
        ControlKind::Icon => SS_NOTIFY | SS_ICON | SS_CENTERIMAGE,
        ControlKind::Graphic => SS_NOTIFY,
        _ => 0,
    };
    // A read-only input or edit is skipped by the tab key, the way AutoIt
    // leaves it out of the tab order.
    let tabstop = if matches!(
        control.kind,
        ControlKind::Input
            | ControlKind::Edit
            | ControlKind::List
            | ControlKind::Combo
            | ControlKind::Button
            | ControlKind::Icon
            | ControlKind::Tab
            | ControlKind::TreeView
            | ControlKind::Date
            | ControlKind::MonthCal
    ) && !readonly
    {
        WS_TABSTOP
    } else {
        0
    };
    let radio_group = if control.kind == ControlKind::Radio {
        WS_GROUP
    } else {
        0
    };
    // A child that is visible has to say so at creation: `ShowWindow` after the
    // fact would flash.
    let visible = if control.is_visible() { WS_VISIBLE } else { 0 };
    WS_CHILD | visible | tabstop | radio_group | base | script
}

/// A control's extended style: the per-kind default unless the script named one.
fn control_exstyle(control: &Control) -> u32 {
    if control.exstyle > 0 {
        return control.exstyle as u32;
    }
    match control.kind {
        ControlKind::Button => WS_EX_WINDOWEDGE,
        ControlKind::Input
        | ControlKind::Edit
        | ControlKind::List
        | ControlKind::Combo
        | ControlKind::Date
        | ControlKind::MonthCal => WS_EX_CLIENTEDGE,
        ControlKind::ListView => LVS_EX_FULLROWSELECT | WS_EX_CLIENTEDGE,
        _ => 0,
    }
}

/// Give a control the font the model asks for, else the stock GUI font.
///
/// The two are not interchangeable: a font from `CreateFontW` is this backend's
/// to delete, while `GetStockObject`'s is shared and must not be. Only the
/// created one is kept, and only that one is deleted here.
fn apply_font(state: &mut ControlState, control: &Control) {
    if state.hwnd.is_null() {
        return;
    }
    let wanted = control.font.clone();
    if state.font_set && state.font_request == wanted {
        return;
    }
    let (font, created) = match &wanted {
        Some(font) => {
            let name = to_wide(&font.name);
            // AutoIt's attribute bits: `$GUI_FONTITALIC` 2, `$GUI_FONTUNDER` 4,
            // `$GUI_FONTSTRIKE` 8.
            let italic = u32::from(font.attribute & 0x02 != 0);
            let underline = u32::from(font.attribute & 0x04 != 0);
            let strike = u32::from(font.attribute & 0x08 != 0);
            let handle = unsafe {
                CreateFontW(
                    // Negative: a character height rather than a cell height,
                    // which is the point size AutoIt means.
                    -font.size.max(1),
                    0,
                    0,
                    0,
                    font.weight,
                    italic,
                    underline,
                    strike,
                    DEFAULT_CHARSET,
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    DEFAULT_PITCH,
                    name.as_ptr(),
                )
            };
            (handle, true)
        }
        None => (unsafe { GetStockObject(DEFAULT_GUI_FONT) }, false),
    };
    if font.is_null() {
        return;
    }
    if let Some(old) = state.font.take() {
        if !old.is_null() {
            unsafe { DeleteObject(old as _) };
        }
    }
    // A stock object is shared, so it is never recorded as owned.
    state.font = if created { Some(font) } else { None };
    state.font_request = wanted;
    state.font_set = true;
    unsafe { SendMessageW(state.hwnd, WM_SETFONT, font as usize, 1) };
}

/// `MAKELPARAM`: two 16-bit values in the `lParam` of a range message.
fn pack16(low: i32, high: i32) -> isize {
    ((((high as i64) & 0xFFFF) << 16) | ((low as i64) & 0xFFFF)) as isize
}

/// An AutoIt colour (`0xRRGGBB`) as GDI wants it.
fn colorref(value: i64) -> COLORREF {
    (value as u32) & 0x00FF_FFFF
}

/// The brush for a colour, made once and kept.
///
/// `WM_CTLCOLOR*` and the graphic painter both hand the handle back to Windows,
/// which is why it cannot be deleted after the call that made it.
fn brush_for(shared: &mut Shared, color: i64) -> HBRUSH {
    if let Some(brush) = shared.brushes.get(&color) {
        return *brush as HBRUSH;
    }
    let brush = unsafe { CreateSolidBrush(colorref(color)) };
    shared.brushes.insert(color, brush as usize);
    brush
}

/// Start GDI+ once per process, for the JPEG and GIF files `LoadImage` cannot
/// read.
///
/// The token is deliberately left alone: GDI+ is shut down with the process, and
/// a token that outlives every backend is what keeps the next window from
/// starting it a second time.
fn ensure_gdiplus() -> bool {
    static STARTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *STARTED.get_or_init(|| unsafe {
        let mut input: GdiplusStartupInput = std::mem::zeroed();
        input.GdiplusVersion = 1;
        let mut token = 0usize;
        GdiplusStartup(&mut token, &input, std::ptr::null_mut()) == 0
    })
}

/// Decode a picture file with GDI+, which reads what `LoadImage` does not.
fn load_bitmap_gdiplus(path: &[u16]) -> Option<(*mut c_void, i32, i32)> {
    if !ensure_gdiplus() {
        return None;
    }
    unsafe {
        let mut bitmap: *mut GpBitmap = std::ptr::null_mut();
        if GdipCreateBitmapFromFile(path.as_ptr(), &mut bitmap) != 0 || bitmap.is_null() {
            return None;
        }
        let mut width = 0u32;
        let mut height = 0u32;
        GdipGetImageWidth(bitmap as *mut GpImage, &mut width);
        GdipGetImageHeight(bitmap as *mut GpImage, &mut height);
        let mut handle: HBITMAP = std::ptr::null_mut();
        let status = GdipCreateHBITMAPFromBitmap(bitmap, &mut handle, 0);
        GdipDisposeImage(bitmap as *mut GpImage);
        if status != 0 || handle.is_null() {
            return None;
        }
        Some((handle, width as i32, height as i32))
    }
}

/// Load the picture file a control asks for.
///
/// `LoadImage` reads BMP, CUR and ICO; JPEG and GIF come from GDI+, which is what
/// AutoIt itself uses. The size comes back with the object because a picture
/// control with no size of its own takes the file's.
fn load_image(path: &str, icon: bool) -> Option<LoadedImage> {
    let wide = to_wide(path);
    let kind = if icon { IMAGE_ICON } else { IMAGE_BITMAP };
    let handle = unsafe {
        LoadImageW(
            std::ptr::null_mut(),
            wide.as_ptr(),
            kind,
            0,
            0,
            LR_LOADFROMFILE,
        )
    };
    if !handle.is_null() {
        let (width, height) = if icon {
            (0, 0)
        } else {
            image_size(handle as HBITMAP)
        };
        return Some(LoadedImage {
            handle,
            icon,
            width,
            height,
        });
    }
    if icon {
        // An icon file the loader refused is not a bitmap: GDI+ would hand back
        // something `STM_SETICON` cannot use.
        return None;
    }
    let (handle, width, height) = load_bitmap_gdiplus(&wide)?;
    Some(LoadedImage {
        handle,
        icon: false,
        width,
        height,
    })
}

/// The pixel size of a bitmap GDI loaded.
fn image_size(handle: HBITMAP) -> (i32, i32) {
    unsafe {
        let mut bitmap: BITMAP = std::mem::zeroed();
        let size = std::mem::size_of::<BITMAP>() as i32;
        if GetObjectW(handle as _, size, &mut bitmap as *mut _ as *mut c_void) == 0 {
            return (0, 0);
        }
        (bitmap.bmWidth.max(0), bitmap.bmHeight.max(0))
    }
}

/// The procedure that paints a graphic control.
///
/// AutoIt draws `GUICtrlSetGraphic` itself, and so does this: the static control
/// created for a `GUICtrlCreateGraphic` gets a subclass procedure that replays
/// the command list on every paint. The list and the colours live in the
/// thread's table because a window procedure has no other way back to the
/// backend that made the window.
unsafe extern "system" fn graphic_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _subclass: usize,
    _data: usize,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => {
            let filled = with_shared(|shared| {
                let drawing = shared.drawings.get(&hwnd_key(hwnd))?;
                let color = drawing.background?;
                let brush = brush_for(shared, color);
                let mut rect: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rect);
                FillRect(wparam as HDC, &rect, brush);
                Some(())
            })
            .flatten()
            .is_some();
            if filled {
                return 1;
            }
        }
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut paint);
            let _ = with_shared(|shared| {
                let Some(drawing) = shared.drawings.get(&hwnd_key(hwnd)).cloned() else {
                    return;
                };
                let mut rect: RECT = std::mem::zeroed();
                GetClientRect(hwnd, &mut rect);
                if let Some(background) = drawing.background {
                    let brush = brush_for(shared, background);
                    FillRect(hdc, &rect, brush);
                }
                paint_commands(hdc, &drawing.commands, drawing.color);
            });
            EndPaint(hwnd, &paint);
            return 0;
        }
        _ => {}
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

/// Replay `GUICtrlSetGraphic`'s command list onto a device context.
///
/// The pen is made per shape: a short command list is the normal case, and
/// keeping a pen alive would mean tracking when the colour or the width changes.
unsafe fn paint_commands(hdc: HDC, commands: &[DrawCmd], default_color: Option<i64>) {
    let mut color = default_color.unwrap_or(0);
    let mut width = 1i32;
    for command in commands {
        match command {
            DrawCmd::SetColor(value) => color = *value,
            // The fill colour only matters to closed shapes, and AutoIt's
            // default is "no fill" (`$GUI_GR_NOBKCOLOR`), which the framed
            // shapes below already draw.
            DrawCmd::SetBkColor(_) => {}
            DrawCmd::SetWidth(value) => width = (*value).max(1),
            DrawCmd::SetStyle(_) => {}
            DrawCmd::Clear => {
                // The canvas is the control: clearing means painting over it,
                // which the next `SetBkColor`-less paint does not do, so this
                // only resets the pen.
                color = default_color.unwrap_or(0);
                width = 1;
            }
            DrawCmd::Line { x1, y1, x2, y2 } => {
                let pen = CreatePen(PS_SOLID, width, colorref(color));
                let old_pen = SelectObject(hdc, pen);
                let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                MoveToEx(hdc, *x1, *y1, std::ptr::null_mut());
                LineTo(hdc, *x2, *y2);
                SelectObject(hdc, old_brush);
                SelectObject(hdc, old_pen);
                DeleteObject(pen);
            }
            DrawCmd::Rect { x, y, w, h } => {
                let pen = CreatePen(PS_SOLID, width, colorref(color));
                let old_pen = SelectObject(hdc, pen);
                let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                Rectangle(hdc, *x, *y, x + w, y + h);
                SelectObject(hdc, old_brush);
                SelectObject(hdc, old_pen);
                DeleteObject(pen);
            }
            DrawCmd::Ellipse { x, y, w, h } => {
                let pen = CreatePen(PS_SOLID, width, colorref(color));
                let old_pen = SelectObject(hdc, pen);
                let old_brush = SelectObject(hdc, GetStockObject(NULL_BRUSH));
                Ellipse(hdc, *x, *y, x + w, y + h);
                SelectObject(hdc, old_brush);
                SelectObject(hdc, old_pen);
                DeleteObject(pen);
            }
            DrawCmd::Text { x, y, text } => {
                let wide = to_wide(text);
                SetTextColor(hdc, colorref(color));
                TextOutW(hdc, *x, *y, wide.as_ptr(), wide.len() as i32 - 1);
            }
        }
    }
}

/// A NUL-terminated UTF-16 copy of `text`.
fn to_wide(text: &str) -> Vec<u16> {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    wide
}

// ---------------------------------------------------------------------------
// Model → Win32 measurements
// ---------------------------------------------------------------------------

/// How much wider and taller a window with this style is than its client area.
///
/// `AdjustWindowRectEx` answers the same thing, but it wants a rectangle and
/// reports a rectangle; a zero-sized client area gives the border, the title bar
/// and the scroll bars as a pair of deltas, which is all this needs.
fn frame_size(style: u32, exstyle: u32) -> (i32, i32) {
    let mut rect: RECT = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe { AdjustWindowRectEx(&mut rect, style, 0, exstyle) };
    (rect.right - rect.left, rect.bottom - rect.top)
}
