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
//! Bitmaps (`GUICtrlSetImage`) are not loaded into the real static controls,
//! `GUICtrlSetGraphic` drawing stays in the model, and `ListView` items only get
//! a first column (no headers). The *semantics* of all of those still work —
//! `GUICtrlRead` answers from the model — it is the pixels that stay behind.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::ffi::c_void;

use autoitv3_gui_model::{
    Control, ControlKind, Font, GuiBackend, GuiEvent, GuiImage, GuiUpdate, Window, WindowState,
};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontW, DeleteObject, GetStockObject, InvalidateRect, UpdateWindow,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    InitCommonControlsEx, INITCOMMONCONTROLSEX, LVITEMW, TCITEMW, TVINSERTSTRUCTW,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetSystemMetrics, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, IsIconic, IsWindow, IsZoomed, MoveWindow, PeekMessageW, RegisterClassExW,
    SendMessageW, SetMenu, SetWindowTextW, ShowWindow, TranslateMessage, MSG, WNDCLASSEXW,
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
const LVM_DELETEALLITEMS: u32 = 0x1009;
const LVM_GETNEXTITEM: u32 = 0x100C;
const TVM_INSERTITEMW: u32 = 0x1132;
const TVM_DELETEITEM: u32 = 0x1101;
const TCM_INSERTITEMW: u32 = 0x133E;
const TCM_DELETEALLITEMS: u32 = 0x1305;
const LVIF_TEXT: u32 = 0x0001;
const LVNI_SELECTED: isize = 0x0002;
const TVIF_TEXT: u32 = 0x0001;
const TVI_ROOT: isize = 0;
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

// `PeekMessageW` flag and the `GetSystemMetrics` indices used here.
const PM_REMOVE: u32 = 0x0001;
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
    with_shared(|state| {
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
            _ => {}
        }
    });
    DefWindowProcW(hwnd, message, wparam, lparam)
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

/// One created control, plus the last state pushed into it.
struct ControlState {
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
                self.controls
                    .insert(control.id, Self::placeholder(control, win_id));
                return;
            }
            // Parts of another control — a `ListView` row, a tree node, a tab
            // page — are drawn by the control that owns them, and the rest have
            // no window of their own; the model still tracks them all.
            ControlKind::ListViewItem
            | ControlKind::TreeViewItem
            | ControlKind::TabItem
            | ControlKind::Dummy
            | ControlKind::Avi
            | ControlKind::Obj => {
                self.controls
                    .insert(control.id, Self::placeholder(control, 0));
                return;
            }
            _ => {}
        }

        let win_id = self.take_win_id();
        let class = to_wide(class_name(control.kind));
        let text = to_wide(&control.text);
        let style = control_style(control);
        let hwnd = unsafe {
            CreateWindowExW(
                control_exstyle(control),
                class.as_ptr(),
                text.as_ptr(),
                style,
                control.x,
                control.y,
                control.width.max(1),
                control.height.max(1),
                parent,
                win_id as _,
                GetModuleHandleW(std::ptr::null()) as _,
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return;
        }
        let _ = with_shared(|state| state.control_ids.insert(win_id, (control.id, control.kind)));
        let mut created = ControlState {
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
        };
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
            push_items(&mut created, &control.data);
            apply_font(&mut created, control);
        }
        self.controls.insert(control.id, created);
    }

    /// Re-apply a control's model state to its real HWND.
    fn update_control(&mut self, control: &Control) {
        let Some(state) = self.controls.get_mut(&control.id) else {
            return;
        };
        let hwnd = state.hwnd;
        if hwnd.is_null() {
            return;
        }
        unsafe {
            // A text the model changed has to reach the control — except where
            // the control is the text the user types into or the items it shows.
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
                // Only the two simple lists: a ListView or TreeView selection is
                // a state bit per item, which is more than one call and is left
                // to the user's own clicks.
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
        if state.items != control.data {
            state.items = control.data.clone();
            push_items(state, &control.data);
        }
        state.text = control.text.clone();
        state.checked = control.is_checked();
        state.selection = control.selection;
        state.visible = control.is_visible();
        state.enabled = control.is_enabled();
        apply_font(state, control);
    }

    /// A control with no window of its own (a menu item, a dummy, an AVI).
    fn placeholder(control: &Control, win_id: i32) -> ControlState {
        ControlState {
            window: control.window,
            hwnd: std::ptr::null_mut(),
            kind: control.kind,
            win_id,
            text: control.text.clone(),
            checked: false,
            selection: None,
            items: Vec::new(),
            visible: control.is_visible(),
            enabled: control.is_enabled(),
            font: None,
            font_request: None,
            font_set: false,
        }
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
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
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
        let _ = with_shared(|shared| {
            shared.control_ids.remove(&state.win_id);
            shared.dirty.remove(&id);
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

// ---------------------------------------------------------------------------
// Model → Win32 translation
// ---------------------------------------------------------------------------

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
fn control_style(control: &Control) -> u32 {
    let script = if control.style > 0 {
        control.style as u32
    } else {
        0
    };
    let base = match control.kind {
        ControlKind::Label => SS_LEFT,
        ControlKind::Group => BS_GROUPBOX,
        ControlKind::Button => 0,
        ControlKind::Checkbox => BS_AUTOCHECKBOX,
        ControlKind::Radio => BS_AUTORADIOBUTTON,
        ControlKind::Input => ES_AUTOHSCROLL | WS_BORDER,
        ControlKind::Edit => ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN | WS_VSCROLL | WS_BORDER,
        ControlKind::List => LBS_NOTIFY | LBS_NOINTEGRALHEIGHT | WS_VSCROLL,
        ControlKind::Combo => CBS_DROPDOWN | CBS_AUTOHSCROLL | CBS_HASSTRINGS,
        ControlKind::ListView => LVS_REPORT | LVS_SHOWSELALWAYS | WS_BORDER,
        ControlKind::TreeView => TVS_HASBUTTONS | TVS_HASLINES | TVS_LINESATROOT | WS_BORDER,
        ControlKind::Tab => 0,
        ControlKind::Progress => PBS_SMOOTH,
        ControlKind::Slider => TBS_AUTOTICKS,
        ControlKind::Updown => UDS_ALIGNRIGHT | UDS_ARROWKEYS,
        ControlKind::Date => 0,
        ControlKind::MonthCal => MCS_NOTODAY,
        ControlKind::Pic => SS_BITMAP | SS_CENTERIMAGE | WS_BORDER,
        ControlKind::Icon => SS_ICON | SS_CENTERIMAGE,
        ControlKind::Graphic => SS_LEFT | WS_BORDER,
        _ => 0,
    };
    let radio_group = if control.kind == ControlKind::Radio {
        WS_GROUP
    } else {
        0
    };
    let tabstop = if matches!(control.kind, ControlKind::Input | ControlKind::Edit) {
        WS_TABSTOP
    } else {
        0
    };
    WS_CHILD
        | if control.is_visible() { WS_VISIBLE } else { 0 }
        | radio_group
        | tabstop
        | base
        | script
}

/// A control's extended style (only `WS_EX_CLIENTEDGE` is honoured).
fn control_exstyle(control: &Control) -> u32 {
    if control.exstyle > 0 {
        control.exstyle as u32
    } else {
        WS_EX_CLIENTEDGE
    }
}

/// Push the model's items into the real control.
fn push_items(state: &mut ControlState, items: &[String]) {
    let hwnd = state.hwnd;
    if hwnd.is_null() {
        return;
    }
    unsafe {
        match state.kind {
            ControlKind::List => {
                SendMessageW(hwnd, LB_RESETCONTENT, 0, 0);
                for item in items {
                    let text = to_wide(item);
                    SendMessageW(hwnd, LB_ADDSTRING, 0, text.as_ptr() as isize);
                }
            }
            ControlKind::Combo => {
                SendMessageW(hwnd, CB_RESETCONTENT, 0, 0);
                for item in items {
                    let text = to_wide(item);
                    SendMessageW(hwnd, CB_ADDSTRING, 0, text.as_ptr() as isize);
                }
            }
            ControlKind::ListView => {
                SendMessageW(hwnd, LVM_DELETEALLITEMS, 0, 0);
                for (index, item) in items.iter().enumerate() {
                    let mut text = to_wide(item);
                    // A single-column report row: the header is never created,
                    // so this is the first column.
                    let mut row: LVITEMW = std::mem::zeroed();
                    row.mask = LVIF_TEXT;
                    row.iItem = index as i32;
                    row.pszText = text.as_mut_ptr();
                    SendMessageW(hwnd, LVM_INSERTITEMW, 0, &mut row as *mut _ as isize);
                }
            }
            ControlKind::TreeView => {
                SendMessageW(hwnd, TVM_DELETEITEM, 0, TVI_ROOT);
                for item in items {
                    let mut text = to_wide(item);
                    // Every item is a root: the model's list is flat, so the
                    // real tree is a list of top-level nodes.
                    let mut insert: TVINSERTSTRUCTW = std::mem::zeroed();
                    // `HTREEITEM` is `isize` in `windows-sys`, and a root's
                    // parent is `TVI_ROOT` (0) rather than a null pointer.
                    insert.hParent = TVI_ROOT;
                    insert.hInsertAfter = TVI_LAST;
                    insert.Anonymous.item.mask = TVIF_TEXT;
                    insert.Anonymous.item.pszText = text.as_mut_ptr();
                    SendMessageW(hwnd, TVM_INSERTITEMW, 0, &mut insert as *mut _ as isize);
                }
            }
            ControlKind::Tab => {
                SendMessageW(hwnd, TCM_DELETEALLITEMS, 0, 0);
                for (index, item) in items.iter().enumerate() {
                    let mut text = to_wide(item);
                    let mut tab: TCITEMW = std::mem::zeroed();
                    tab.mask = TCIF_TEXT;
                    tab.pszText = text.as_mut_ptr();
                    SendMessageW(hwnd, TCM_INSERTITEMW, index, &mut tab as *mut _ as isize);
                }
            }
            _ => {}
        }
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
