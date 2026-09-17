//! AutoIt's GUI builtins, emulated.
//!
//! The AutoIt semantics live here — handle numbering, `@error`, `$GUI_EVENT_*`,
//! `$GUI_*` state bits — over the in-memory [`model::GuiModel`]. Rendering and
//! event delivery are a pluggable [`GuiBackend`]: [`HeadlessBackend`] renders
//! nothing, so a script can build its whole UI and run its message loop with no
//! toolkit and no display, and that is what a fresh emulation gets on every host
//! without a native window system. On Windows the platform stack installs the
//! real Win32 backend instead (see `autoitv3_platform::windows::gui`), which
//! drives the same model through actual controls; `au3 run --gui headless`
//! forces this one back.
//!
//! # Scripted events
//!
//! Analysis runs need the script's `While 1 ... GUIGetMsg() ... WEnd` to end.
//! Two knobs do that deterministically:
//!
//! * [`GuiState::with_events`] seeds a queue of events (`Close`, `Control(id)`,
//!   …) that `GUIGetMsg`/`TrayGetMsg` consume.
//! * [`GuiState::with_auto_close`] delivers `$GUI_EVENT_CLOSE` on the *n*-th
//!   poll, so a script that ignores everything still terminates.
//!
//! Dialog answers (`InputBox`, `File*Dialog`) come from
//! [`GuiState::with_answers`]; with none queued they fail like a cancelled
//! dialog, which is what a real UI-less run would observe. `MsgBox` takes its
//! answer from the event queue, defaulting to OK.
//!
//! None of them block or show anything, so each dialog the emulation answers is
//! also reported on stderr — `[winemu] MsgBox(16, "Error", "cannot open input") -> 1`.
//! Without that line a script that took an error branch and exited looks like it
//! "just finished", which is the wrong thing for an analysis run to say. A
//! dialog is usually the only visible reason for such a branch, so it is worth
//! the noise; repeats of the same dialog are folded.
//!
//! # OnEvent mode
//!
//! `Opt("GUIOnEventMode", 1)` turns the events around: `GUISetOnEvent` and
//! `GUICtrlSetOnEvent` handlers are called instead of the event reaching
//! `GUIGetMsg`, which is what the help page means by "when in this mode
//! `GUIGetMsg()` is NOT used at all". A window's close/minimise/… handler, a
//! control's click handler and a tray *item*'s handler are all dispatched, and
//! `@GUI_CtrlId`/`@GUI_WinHandle` describe the event while the function runs.
//! An event with no handler is still returned, so a script that mixes the two
//! styles keeps working.
//!
//! `TraySetOnEvent` (the tray *icon*), `GUIRegisterMsg` (a window message) and
//! `GUICtrlRegisterListViewSort` (a column click) are recorded but not called:
//! the first needs a tray-icon event table the model does not have, and the
//! other two name functions a real window would have to ask for from inside its
//! own procedure, which is the piece that is missing.

mod messages;

/// The widget model and backend seam live in the dependency-free
/// `autoitv3-gui-model` crate, so a renderer only has to depend on that.
pub use autoitv3_gui_model as model;
pub use autoitv3_gui_model::{
    Control, ControlKind, DrawCmd, Font, GuiBackend, GuiEvent, GuiImage, GuiModel, GuiUpdate,
    HeadlessBackend, Progress, Splash, TrayItem, Window, WindowState, GUI_EVENT_CLOSE, GUI_EVENT_DROPPED,
    GUI_EVENT_MAXIMIZE, GUI_EVENT_MINIMIZE, GUI_EVENT_MOUSEMOVE, GUI_EVENT_PRIMARYDOWN,
    GUI_EVENT_PRIMARYUP, GUI_EVENT_RESTORE, GUI_EVENT_RESIZED, GUI_EVENT_SECONDARYDOWN,
    GUI_EVENT_SECONDARYUP,
};

use std::collections::VecDeque;

use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::value::Value;

use autoitv3_gui_model::{
    GUI_CHECKED, GUI_DISABLE, GUI_ENABLE, GUI_HIDE, GUI_PAGE_HIDDEN, GUI_SHOW,
};

/// The docking a control gets when a script never called `GUICtrlSetResizing`.
///
/// The help pages name these defaults per control: a tab, a picture and an icon
/// keep their size, a progress bar follows the window, and everything else keeps
/// both its place and its size (which is what AutoIt does — a control does not
/// move by itself).
fn default_dock(kind: ControlKind) -> i64 {
    match kind {
        ControlKind::Tab | ControlKind::Pic | ControlKind::Icon => GUI_DOCKSIZE,
        ControlKind::Progress => GUI_DOCKAUTO,
        _ => GUI_DOCKALL,
    }
}

/// Where a control ends up when its window changes size.
///
/// `dock` is a `$GUI_DOCK*` word: each flag names a side that does *not* move, so
/// a control anchored to both sides of an axis grows along it instead.
#[allow(clippy::too_many_arguments)]
fn docked(
    control: &Control,
    dock: i64,
    dx: i32,
    dy: i32,
    old_width: i32,
    old_height: i32,
    new_width: i32,
    new_height: i32,
) -> (i32, i32, i32, i32) {
    if dock & GUI_DOCKAUTO != 0 {
        // Everything scales with the window.
        let scale_x = new_width as f32 / old_width as f32;
        let scale_y = new_height as f32 / old_height as f32;
        return (
            (control.x as f32 * scale_x).round() as i32,
            (control.y as f32 * scale_y).round() as i32,
            (control.width as f32 * scale_x).round().max(1.0) as i32,
            (control.height as f32 * scale_y).round().max(1.0) as i32,
        );
    }
    let (left, right) = (dock & GUI_DOCKLEFT != 0, dock & GUI_DOCKRIGHT != 0);
    let (top, bottom) = (dock & GUI_DOCKTOP != 0, dock & GUI_DOCKBOTTOM != 0);
    let mut x = control.x;
    let mut y = control.y;
    let mut width = control.width;
    let mut height = control.height;
    if right && !left {
        x += dx;
    } else if !left && !right && dock & GUI_DOCKHCENTER != 0 {
        x += dx / 2;
    } else if left && right && dock & GUI_DOCKWIDTH == 0 {
        width += dx;
    }
    if bottom && !top {
        y += dy;
    } else if !top && !bottom && dock & GUI_DOCKVCENTER != 0 {
        y += dy / 2;
    } else if top && bottom && dock & GUI_DOCKHEIGHT == 0 {
        height += dy;
    }
    (x, y, width.max(1), height.max(1))
}

/// `$LVS_EX_CHECKBOXES`: a `ListView` whose items carry a check box.
const LVS_EX_CHECKBOXES: i64 = 0x0000_0004;

/// The bits a part reports through `GUICtrlRead`: its own checked, focus and
/// default-button state.
///
/// `$GUI_EXPAND` is not one of them — the official interpreter answers 0 for an
/// expanded item — and neither is the generic `$GUI_SHOW | $GUI_ENABLE` word,
/// which only `GUICtrlGetState` answers.
const ITEM_STATE_MASK: i64 = GUI_CHECKED | 0x02 | 0x04 | 0x100 | 0x200;

/// `$GUI_DOCK*`: what a control keeps when its window is resized.
const GUI_DOCKAUTO: i64 = 1;
const GUI_DOCKLEFT: i64 = 2;
const GUI_DOCKRIGHT: i64 = 4;
const GUI_DOCKHCENTER: i64 = 8;
const GUI_DOCKTOP: i64 = 32;
const GUI_DOCKBOTTOM: i64 = 64;
const GUI_DOCKVCENTER: i64 = 128;
const GUI_DOCKWIDTH: i64 = 256;
const GUI_DOCKHEIGHT: i64 = 512;
const GUI_DOCKSIZE: i64 = 768;
const GUI_DOCKALL: i64 = 802;

/// Messages a script can pass that change nothing the model keeps, so only a
/// real control can act on them. Everything else is answered (and, where it
/// mutates, applied) by the model, because a `lParam` that points at script
/// memory addresses the emulation's memory rather than the control's.
const NATIVE_ONLY_MESSAGES: &[u32] = &[
    0x000B, // WM_SETREDRAW
    0x00B1, // EM_SETSEL
    0x1013, // LVM_ENSUREVISIBLE
    0x1014, // LVM_SCROLL
    0x014F, // CB_SHOWDROPDOWN
    0x1115, // TVM_ENSUREVISIBLE
];

/// Every GUI function this layer answers.
pub const FUNCTIONS: &[&str] = &[
    // GUI window / controls
    "GUICreate", "GUIDelete", "GUISetState", "GUISwitch", "GUIGetMsg", "GUIGetCursorInfo",
    "GUIRegisterMsg", "GUISetBkColor", "GUISetFont", "GUISetIcon", "GUISetCursor",
    "GUISetOnEvent", "GUISetStyle", "GUIGetStyle", "GUISetCoord", "GUISetAccelerators",
    "GUISetHelp", "GUIStartGroup",
    "GUICtrlCreateLabel", "GUICtrlCreateButton", "GUICtrlCreateCheckbox", "GUICtrlCreateRadio",
    "GUICtrlCreateGroup", "GUICtrlCreateInput", "GUICtrlCreateEdit", "GUICtrlCreateList",
    "GUICtrlCreateCombo", "GUICtrlCreateListView", "GUICtrlCreateListViewItem",
    "GUICtrlCreateTreeView", "GUICtrlCreateTreeViewItem", "GUICtrlCreateTab",
    "GUICtrlCreateTabItem", "GUICtrlCreateMenu", "GUICtrlCreateMenuItem",
    "GUICtrlCreateContextMenu", "GUICtrlCreatePic", "GUICtrlCreateIcon",
    "GUICtrlCreateGraphic", "GUICtrlCreateProgress", "GUICtrlCreateSlider",
    "GUICtrlCreateUpdown", "GUICtrlCreateDate", "GUICtrlCreateMonthCal",
    "GUICtrlCreateDummy", "GUICtrlCreateAvi", "GUICtrlCreateObj",
    "GUICtrlDelete", "GUICtrlGetHandle", "GUICtrlGetState", "GUICtrlRead", "GUICtrlRecvMsg",
    "GUICtrlRegisterListViewSort", "GUICtrlSendMsg", "GUICtrlSendToDummy",
    "GUICtrlSetBkColor", "GUICtrlSetColor", "GUICtrlSetCursor", "GUICtrlSetData",
    "GUICtrlSetDefBkColor", "GUICtrlSetDefColor", "GUICtrlSetFont", "GUICtrlSetGraphic",
    "GUICtrlSetImage", "GUICtrlSetLimit", "GUICtrlSetOnEvent", "GUICtrlSetPos",
    "GUICtrlSetResizing", "GUICtrlSetState", "GUICtrlSetStyle", "GUICtrlSetTip",
    // windows
    "WinActivate", "WinActive", "WinClose", "WinExists", "WinGetCaretPos", "WinGetClassList",
    "WinGetClientSize", "WinGetHandle", "WinGetPos", "WinGetProcess", "WinGetState",
    "WinGetText", "WinGetTitle", "WinKill", "WinList", "WinMenuSelectItem", "WinMinimizeAll",
    "WinMinimizeAllUndo", "WinMove", "WinSetOnTop", "WinSetState", "WinSetTitle", "WinWait",
    "WinWaitActive", "WinWaitClose", "WinWaitNotActive", "StatusbarGetText",
    // controls
    "ControlClick", "ControlCommand", "ControlDisable", "ControlEnable", "ControlFocus",
    "ControlGetFocus", "ControlGetHandle", "ControlGetPos", "ControlGetText", "ControlHide",
    "ControlListView", "ControlMove", "ControlSend", "ControlSetText", "ControlShow",
    "ControlTreeView",
    // dialogs
    "MsgBox", "InputBox", "FileOpenDialog", "FileSaveDialog", "FileSelectFolder",
    // feedback
    "SplashTextOn", "SplashImageOn", "SplashOff", "ProgressOn", "ProgressSet", "ProgressOff",
    "ToolTip",
    // tray
    "TrayCreateItem", "TrayCreateMenu", "TrayGetMsg", "TrayItemDelete", "TrayItemGetHandle",
    "TrayItemGetState", "TrayItemGetText", "TrayItemSetOnEvent", "TrayItemSetState",
    "TrayItemSetText", "TraySetClick", "TraySetIcon", "TraySetOnEvent", "TraySetPauseIcon",
    "TraySetState", "TraySetToolTip", "TrayTip",
    // input
    "Send", "SendKeepActive", "MouseClick", "MouseClickDrag", "MouseDown", "MouseGetCursor",
    "MouseGetPos", "MouseMove", "MouseUp", "MouseWheel", "HotKeySet", "BlockInput",
    // pixel
    "PixelChecksum", "PixelGetColor", "PixelSearch",
    // misc
    "Beep", "SoundPlay", "SoundSetWaveVolume", "CDTray", "AutoItWinGetTitle",
    "AutoItWinSetTitle", "Break",
];

/// `FUNCTIONS` as a lookup set, built once on first use.
///
/// Every builtin call passes through `GuiState::call`; the set turns the
/// "is this even a GUI function?" gate into a hash lookup instead of an
/// O(165) scan plus lets the non-GUI majority skip the state-machine prelude.
/// Keys are lower-cased: callers hand in the already-lowered call name.
static FUNCTIONS_SET: std::sync::OnceLock<std::collections::HashSet<String>> =
    std::sync::OnceLock::new();

fn functions_set() -> &'static std::collections::HashSet<String> {
    FUNCTIONS_SET.get_or_init(|| {
        FUNCTIONS
            .iter()
            .map(|f| f.to_ascii_lowercase())
            .collect()
    })
}

/// The GUI half of the emulation: the model, a backend and scripted events.
pub struct GuiState {
    /// The widget model all backends agree on.
    pub model: GuiModel,
    backend: Box<dyn GuiBackend>,
    events: VecDeque<GuiEvent>,
    answers: VecDeque<String>,
    auto_close: Option<u64>,
    polls: u64,
    /// The drawing pen `GUICtrlSetGraphic` moves around.
    draw_pen: (i32, i32),
    /// The desktop size last seen, so a change can be noticed; `(0, 0)` until
    /// the first GUI call.
    desktop: (i32, i32),
    /// Dialog notices already reported, so a script that loops on a dialog
    /// cannot flood stderr — an emulated dialog answers instantly where a real
    /// one would block on the user.
    dialogs_seen: std::collections::HashSet<String>,
    /// The `TabItem` new controls belong to, until `GUICtrlCreateTabItem("")`
    /// closes the tab structure or `GUISwitch` names another page.
    current_tabitem: Option<i64>,
    /// The `GUISetOnEvent`/`GUICtrlSetOnEvent` functions an event asked for,
    /// waiting for the interpreter to call them once the current GUI call has
    /// returned.
    ///
    /// A host cannot re-enter the interpreter from inside a call, so the name
    /// is left here and the runtime runs it at the next boundary — see
    /// `WindowsEmulation::take_pending_callbacks`.
    pending: Vec<(String, Vec<Value>)>,
    /// `@GUI_CtrlId`/`@GUI_WinHandle`/`@GUI_CtrlHandle` for the callback that
    /// was queued last: the event the helper function is running for.
    event_macros: Option<(i64, i64)>,
    /// Every window/control handle this layer has minted, shared with the
    /// runtime so `IsHWnd`/`HWnd` can answer for the values.
    hwnds: std::rc::Rc<std::cell::RefCell<std::collections::HashSet<i64>>>,
}

impl Default for GuiState {
    fn default() -> Self {
        Self::new()
    }
}

impl GuiState {
    /// A headless GUI state.
    pub fn new() -> Self {
        Self {
            model: GuiModel::new(),
            backend: Box::new(HeadlessBackend::new()),
            events: VecDeque::new(),
            answers: VecDeque::new(),
            auto_close: None,
            polls: 0,
            draw_pen: (0, 0),
            desktop: (0, 0),
            dialogs_seen: std::collections::HashSet::new(),
            current_tabitem: None,
            pending: Vec::new(),
            event_macros: None,
            hwnds: std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashSet::new())),
        }
    }

    /// Record a minted handle so the runtime's `IsHWnd`/`HWnd` answer for
    /// it; `forget` drops one the script deleted.
    fn register_hwnd(&mut self, ctx: &mut dyn HostContext, handle: i64) {
        self.hwnds.borrow_mut().insert(handle);
        ctx.register_hwnd(handle);
    }

    fn forget_hwnd(&mut self, ctx: &mut dyn HostContext, handle: i64) {
        self.hwnds.borrow_mut().remove(&handle);
        ctx.forget_hwnd(handle);
    }

    /// The `GUISetOnEvent`/`GUICtrlSetOnEvent` functions the events asked for.
    ///
    /// Drained by the platform once the call that raised the events returns:
    /// `OnEvent` mode means the script never sees the event, only the call.
    pub fn take_pending_callbacks(&mut self) -> Vec<(String, Vec<Value>)> {
        std::mem::take(&mut self.pending)
    }

    /// `@GUI_CtrlId` — the control the last queued callback is for.
    pub fn event_control_id(&self) -> i64 {
        self.event_macros.map(|(control, _)| control).unwrap_or(0)
    }

    /// `@GUI_WinHandle` — the window the last queued callback is for.
    pub fn event_window(&self) -> i64 {
        self.event_macros.map(|(_, window)| window).unwrap_or(0)
    }

    /// A GUI state that starts with `backend` installed.
    ///
    /// The platform stack uses this to give a fresh emulation the host's own
    /// backend — on Windows the real Win32 one, elsewhere the headless default.
    pub fn with_backend(backend: Box<dyn GuiBackend>) -> Self {
        let mut state = Self::new();
        state.backend = backend;
        state
    }

    /// Install a rendering backend.
    pub fn set_backend(&mut self, backend: Box<dyn GuiBackend>) {
        self.backend = backend;
    }

    /// Report a dialog the emulation answered on the script's behalf.
    ///
    /// The emulated dialog never blocks and its text goes nowhere, so a script
    /// that branched on it looks like it "just finished" — and the message is
    /// usually the only clue to why. Print it on stderr next to the other
    /// `[winemu]` notices; repeats of the same dialog are folded, because the
    /// instant answer means a loop around one would otherwise print without
    /// bound.
    fn report_dialog(&mut self, notice: String) {
        if self.dialogs_seen.insert(notice.clone()) {
            eprintln!("{notice}");
        }
    }

    /// Seed the event queue `GUIGetMsg` drains.
    pub fn with_events(mut self, events: Vec<GuiEvent>) -> Self {
        self.events.extend(events);
        self
    }

    /// Queue answers for `InputBox` and the `File*Dialog` functions.
    pub fn with_answers(mut self, answers: Vec<String>) -> Self {
        self.answers.extend(answers);
        self
    }

    /// Make the *n*-th `GUIGetMsg` return `$GUI_EVENT_CLOSE`.
    pub fn with_auto_close(mut self, polls: u64) -> Self {
        self.auto_close = Some(polls);
        self
    }

    /// Capture the current frame from the backend, when it can render one.
    pub fn snapshot(&mut self) -> Option<GuiImage> {
        self.backend.snapshot()
    }

    /// The desktop the emulated machine has: the backend's viewport/canvas, or
    /// [`DEFAULT_DESKTOP_SIZE`] when it has none.
    pub fn desktop_size(&self) -> (i32, i32) {
        self.backend
            .desktop_size()
            .unwrap_or(autoitv3_gui_model::DEFAULT_DESKTOP_SIZE)
    }

    /// Put a window into the state a `@SW_*` flag asks for, and return whether
    /// anything changed.
    ///
    /// Maximising takes the window's rectangle as well, the way Windows does:
    /// the desktop's rectangle replaces it, and the old one is kept for
    /// `@SW_RESTORE`.
    fn apply_show_flag(&mut self, handle: i64, flag: i64) -> bool {
        let (visible, state) = show_flag(flag);
        let (desktop_width, desktop_height) = self.desktop_size();
        let Some(window) = self.model.window(handle) else {
            return false;
        };
        let mut changed = window.visible != visible || window.state != state;
        let mut move_to: Option<(i32, i32)> = None;
        let mut resize_to: Option<(i32, i32)> = None;
        match state {
            WindowState::Maximized => {
                if window.state != WindowState::Maximized {
                    let restore = (window.x, window.y, window.width, window.height);
                    if let Some(window) = self.model.window_mut(handle) {
                        window.restore = Some(restore);
                    }
                }
                move_to = Some((0, 0));
                resize_to = Some((desktop_width, desktop_height));
            }
            WindowState::Normal => {
                if let Some((x, y, width, height)) = window.restore {
                    move_to = Some((x, y));
                    resize_to = Some((width, height));
                    if let Some(window) = self.model.window_mut(handle) {
                        window.restore = None;
                    }
                    changed = true;
                }
            }
            WindowState::Minimized => {}
        }
        if let Some(window) = self.model.window_mut(handle) {
            window.visible = visible;
            window.state = state;
            if let Some((x, y)) = move_to {
                window.x = x;
                window.y = y;
            }
        }
        // Maximising and restoring change the size, so the controls dock the
        // same way they do for a hand-dragged window.
        if let Some((width, height)) = resize_to {
            changed |= self.resize_window(handle, width, height);
        }
        changed
    }

    /// A maximised window follows the desktop, the way Windows resizes it when
    /// the display mode changes (a live window's viewport can be resized).
    fn sync_desktop(&mut self) {
        let desktop = self.desktop_size();
        if self.desktop == desktop {
            return;
        }
        self.desktop = desktop;
        let (width, height) = desktop;
        let maximized: Vec<i64> = self
            .model
            .windows
            .iter()
            .flatten()
            .filter(|window| window.state == WindowState::Maximized)
            .map(|window| window.handle)
            .collect();
        for handle in maximized {
            if let Some(window) = self.model.window_mut(handle) {
                window.x = 0;
                window.y = 0;
                window.width = width;
                window.height = height;
            }
            self.notify_window(handle);
            self.events.push_back(GuiEvent::System(GUI_EVENT_RESIZED));
        }
    }

    /// Apply edits a live window queued (typed text, toggled checkbox).
    fn apply_updates(&mut self) {
        for update in self.backend.take_updates() {
            let id = match update {
                GuiUpdate::SetText { id, text } => {
                    if let Some(control) = self.model.control_mut(id) {
                        control.text = text;
                    }
                    id
                }
                GuiUpdate::SetChecked { id, checked } => {
                    if let Some(control) = self.model.control_mut(id) {
                        if checked {
                            control.state |= 0x01;
                        } else {
                            control.state &= !0x01;
                        }
                    }
                    id
                }
                GuiUpdate::Select { id, index } => {
                    let tab = self
                        .model
                        .control(id)
                        .filter(|control| control.kind == ControlKind::Tab)
                        .map(|control| control.id);
                    if let Some(control) = self.model.control_mut(id) {
                        control.selection = Some(index);
                    }
                    // A tab click shows another page's controls and hides the
                    // ones that were on screen.
                    if let Some(tab) = tab {
                        self.apply_tab_visibility(tab);
                    }
                    id
                }
                GuiUpdate::SetWindowState { handle, state } => {
                    // The same path a `@SW_*` flag takes, so a user's maximise
                    // also remembers where to restore to.
                    let flag = match state {
                        WindowState::Minimized => 6,
                        WindowState::Maximized => 3,
                        WindowState::Normal => 9,
                    };
                    let changed = self.apply_show_flag(handle, flag);
                    if changed {
                        self.notify_window(handle);
                        let event = match state {
                            WindowState::Minimized => GUI_EVENT_MINIMIZE,
                            WindowState::Maximized => GUI_EVENT_MAXIMIZE,
                            WindowState::Normal => GUI_EVENT_RESTORE,
                        };
                        self.events.push_back(GuiEvent::System(event));
                    }
                    continue;
                }
                GuiUpdate::Move { handle, x, y } => {
                    // A user's drag. AutoIt has no message for this (scripts
                    // poll WinGetPos), so the model is all that changes.
                    let changed = match self.model.window_mut(handle) {
                        Some(window) => {
                            let changed = window.x != x || window.y != y;
                            window.x = x;
                            window.y = y;
                            changed
                        }
                        None => false,
                    };
                    if changed {
                        self.notify_window(handle);
                    }
                    continue;
                }
                GuiUpdate::Resize {
                    handle,
                    width,
                    height,
                } => {
                    // The user dragged a window edge. AutoIt scripts see this
                    // through WinGetPos/WinGetClientSize and `$GUI_EVENT_RESIZED`,
                    // and the controls move the way their docking asks.
                    let changed = self.resize_window(handle, width, height);
                    if changed {
                        self.notify_window(handle);
                        self.events.push_back(GuiEvent::System(GUI_EVENT_RESIZED));
                    }
                    continue;
                }
            };
            // Tell the backend what the model now holds. Without this the
            // window that sent the edit keeps drawing the old value, so typed
            // text snaps back on the next frame.
            self.notify_control(id);
        }
    }

    /// Whether this layer answers `name`.
    pub fn provides(name: &str) -> bool {
        functions_set().contains(name.to_ascii_lowercase().as_str())
    }

    /// Drain backend events, then hand out the next scripted one.
    ///
    /// With `Opt("GUIOnEventMode", 1)` an event that has a handler registered
    /// goes to that function instead of to the script: the help page is explicit
    /// that "when in this mode `GUIGetMsg()` is NOT used at all". The call is
    /// queued rather than made here — a host cannot re-enter the interpreter
    /// from inside a call — and the runtime runs it as soon as this one returns.
    /// An event nobody registered for is still returned, which is what keeps a
    /// script that mixes the two styles working.
    /// [`poll_message`](Self::poll_message) with provenance: the event value
    /// plus where it came from — the window handle for a close or system event,
    /// the model control id for a control or menu one. The idle answer is
    /// `(0, None, None)`, the same "no message" the plain mode reports as 0.
    fn poll_message_source(
        &mut self,
        ctx: &mut dyn HostContext,
    ) -> (i64, Option<i64>, Option<i64>) {
        for event in self.backend.poll() {
            self.events.push_back(event);
        }
        self.polls += 1;
        if let Some(event) = self.events.pop_front() {
            if self.dispatch_event(&event, ctx) {
                return (0, None, None);
            }
            let control = match &event {
                GuiEvent::Control(id) | GuiEvent::Menu(id) => Some(*id),
                _ => None,
            };
            let window = match &event {
                GuiEvent::Close(handle) => Some(*handle),
                GuiEvent::System(_) => self.model.current_window,
                _ => control
                    .and_then(|id| self.model.control(id))
                    .map(|control| control.window),
            };
            return (event.message(), window, control);
        }
        if let Some(limit) = self.auto_close {
            if self.polls >= limit {
                self.auto_close = None;
                if !self.dispatch_event(&GuiEvent::System(GUI_EVENT_CLOSE), ctx) {
                    return (GUI_EVENT_CLOSE, self.model.current_window, None);
                }
            }
        }
        (0, None, None)
    }

    fn poll_message(&mut self, ctx: &mut dyn HostContext) -> i64 {
        for event in self.backend.poll() {
            self.events.push_back(event);
        }
        self.polls += 1;
        if let Some(event) = self.events.pop_front() {
            if self.dispatch_event(&event, ctx) {
                return 0;
            }
            return event.message();
        }
        if let Some(limit) = self.auto_close {
            if self.polls >= limit {
                self.auto_close = None;
                if !self.dispatch_event(&GuiEvent::System(GUI_EVENT_CLOSE), ctx) {
                    return GUI_EVENT_CLOSE;
                }
            }
        }
        0
    }

    /// Whether the script asked to be called back instead of polling.
    fn on_event_mode(ctx: &dyn HostContext) -> bool {
        ctx.option("GUIOnEventMode")
            .is_some_and(|value| value.to_int() != 0)
    }

    /// Hand `event` to the function registered for it, if there is one.
    ///
    /// Answers whether a call was queued; `false` means the event should go on
    /// to `GUIGetMsg` as usual.
    fn dispatch_event(&mut self, event: &GuiEvent, ctx: &mut dyn HostContext) -> bool {
        if !Self::on_event_mode(ctx) {
            return false;
        }
        let control = match event {
            GuiEvent::Control(id) | GuiEvent::Menu(id) => Some(*id),
            _ => None,
        };
        let window = match event {
            GuiEvent::Close(handle) => Some(*handle),
            GuiEvent::System(_) => self.model.current_window,
            _ => control
                .and_then(|id| self.model.control(id))
                .map(|control| control.window),
        };
        let handler = match event {
            GuiEvent::Close(handle) => self
                .model
                .window(*handle)
                .and_then(|window| window.on_event(GUI_EVENT_CLOSE))
                .map(str::to_string),
            GuiEvent::System(id) => window
                .and_then(|handle| self.model.window(handle))
                .and_then(|window| window.on_event(*id))
                .map(str::to_string),
            GuiEvent::Control(id) | GuiEvent::Menu(id) => self
                .model
                .control(*id)
                .and_then(|control| control.on_event.clone()),
            GuiEvent::Tray(id) => self
                .model
                .tray
                .iter()
                .find(|item| item.id == *id)
                .and_then(|item| item.on_event.clone()),
            GuiEvent::Dialog(_) => None,
        };
        let Some(handler) = handler else {
            return false;
        };
        // `@GUI_CtrlId` and friends are macros of the interpreter, which has no
        // idea a GUI exists; the values are kept here for it to read back.
        self.event_macros = Some((
            control.unwrap_or(0),
            window.unwrap_or(0),
        ));
        self.pending.push((handler, Vec::new()));
        ctx.set_error(0, 0);
        true
    }

    fn notify_window(&mut self, handle: i64) {
        if let Some(window) = self.model.window(handle).cloned() {
            self.backend.on_window(&window);
        }
    }

    fn notify_control(&mut self, id: i64) {
        if let Some(control) = self.model.control(id).cloned() {
            self.backend.on_control(&control);
        }
    }

    /// Dispatch a GUI call; `None` means "not a GUI function".
    ///
    /// Any edits a live window queued are applied first, so a `GUICtrlRead`
    /// after typing sees the new text. Non-GUI names exit before the prelude:
    /// every builtin call passes through here, and only GUI names need the
    /// backend sync.
    pub fn call(
        &mut self,
        name: &str,
        args: &[Value],
        ctx: &mut dyn HostContext,
    ) -> Option<Value> {
        let key = name.to_ascii_lowercase();
        if !functions_set().contains(key.as_str()) {
            return None;
        }
        self.apply_updates();
        self.sync_desktop();
        if let Some(kind) = ControlKind::from_create(&key) {
            return Some(self.create_control(kind, args, ctx));
        }
        Some(match key.as_str() {
            // ---------------- GUI window ----------------
            "guicreate" => {
                let handle = self.model.alloc_window();
                let window = Window {
                    handle,
                    title: arg_str(args, 0),
                    width: arg_int(args, 1) as i32,
                    height: arg_int(args, 2) as i32,
                    x: arg_int(args, 3) as i32,
                    y: arg_int(args, 4) as i32,
                    style: arg_int(args, 5),
                    exstyle: arg_int(args, 6),
                    visible: false,
                    enabled: true,
                    active: true,
                    state: WindowState::Normal,
                    restore: None,
                    bk_color: None,
                    font: None,
                    cursor: None,
                    icon: None,
                    focus: None,
                    topmost: false,
                    resizing: 0,
                    on_events: std::collections::HashMap::new(),
                    controls: Vec::new(),
                };
                self.model.add_window(window);
                self.notify_window(handle);
                self.register_hwnd(ctx, handle);
                ctx.set_error(0, 0);
                Value::Ptr(handle)
            }
            "guidelete" => {
                let handle = self.window_arg(args, 0);
                match handle {
                    Some(handle) => {
                        let ok = self.model.remove_window(handle);
                        self.backend.on_window_removed(handle);
                        self.forget_hwnd(ctx, handle);
                        ctx.set_error(if ok { 0 } else { 1 }, 0);
                        Value::Int(i64::from(ok))
                    }
                    None => {
                        ctx.set_error(1, 0);
                        Value::Int(0)
                    }
                }
            }
            "guisetstate" => {
                // AutoIt's default flag is `@SW_SHOW`, not "hide".
                let state = if args.is_empty() { 5 } else { arg_int(args, 0) };
                let handle = self.window_arg(args, 1);
                let Some(handle) = handle else {
                    ctx.set_error(1, 0);
                    return Some(Value::Int(0));
                };
                self.apply_show_flag(handle, state);
                self.notify_window(handle);
                self.backend.present();
                ctx.set_error(0, 0);
                Value::Int(1)
            }
            "guiswitch" => {
                let handle = self.window_arg(args, 0);
                let previous = self.model.active_window().unwrap_or(0);
                self.model.current_window = handle;
                // `GUISwitch($win, $tabitem)` also says which page the controls
                // created next belong to.
                self.current_tabitem = self
                    .model
                    .control_id(arg_int(args, 1))
                    .filter(|page| {
                        self.model.control(*page).map(|control| control.kind)
                            == Some(ControlKind::TabItem)
                    });
                ctx.set_error(if handle.is_some() { 0 } else { 1 }, 0);
                if previous != 0 {
                    self.register_hwnd(ctx, previous);
                    Value::Ptr(previous)
                } else {
                    Value::Int(0)
                }
            }
            "guigetmsg" => {
                ctx.set_error(0, 0);
                // `GUIGetMsg(1)` is the advanced mode: the event comes back as
                // an array — [event, window, control-hwnd, control-id] and a
                // trailing slot the interpreter fills with 0 — instead of the
                // bare event value. Measured on the official x64 interpreter:
                // with no message pending the array is five zeros, @error 0.
                if arg_int(args, 0) != 0 {
                    // Measured on the official x64 interpreter: with no message
                    // pending the array is five zeros (all Int); with one, [1]
                    // is the window as a `Ptr`, [2] the control's HWND as a
                    // `Ptr` (0 for a close event) and [3]/[4] the cursor
                    // position — `GUIGetCursorInfo`'s first two answers.
                    let (event, window, control) = self.poll_message_source(ctx);
                    let ctrl_id = control
                        .and_then(|id| self.model.control(id))
                        .map(|control| control.id)
                        .unwrap_or(0);
                    let ctrl_window = control
                        .and_then(|id| self.model.control(id))
                        .map(|control| control.window)
                        .or(window)
                        .unwrap_or(0);
                    let (cx, cy) = self.model.mouse;
                    let window_value = if ctrl_window != 0 {
                        self.register_hwnd(ctx, ctrl_window);
                        Value::Ptr(ctrl_window)
                    } else {
                        Value::Int(0)
                    };
                    let control_value = if ctrl_id != 0 {
                        self.register_hwnd(ctx, ctrl_id);
                        Value::Ptr(ctrl_id)
                    } else {
                        Value::Int(0)
                    };
                    return Some(Value::array(vec![
                        Value::Int(event),
                        window_value,
                        control_value,
                        Value::Int(i64::from(cx)),
                        Value::Int(i64::from(cy)),
                    ]));
                }
                Value::Int(self.poll_message(ctx))
            }
            "guigetcursorinfo" => {
                let (x, y) = self.model.mouse;
                ctx.set_error(0, 0);
                Value::array(vec![
                    Value::Int(i64::from(x)),
                    Value::Int(i64::from(y)),
                    Value::Int(0),
                    Value::Int(0),
                    Value::Int(0),
                ])
            }
            "guiregistermsg" => {
                let msg = arg_int(args, 0) as u32;
                let handler = arg_str(args, 1);
                if handler.is_empty() {
                    self.model.notice_handlers.retain(|(m, _)| *m != msg);
                } else {
                    self.model.notice_handlers.retain(|(m, _)| *m != msg);
                    self.model.notice_handlers.push((msg, handler));
                }
                ctx.set_error(0, 0);
                Value::Int(1)
            }
            "guisetbkcolor" => {
                let color = arg_int(args, 0);
                if let Some(handle) = self.window_arg(args, 1) {
                    if let Some(window) = self.model.window_mut(handle) {
                        window.bk_color = Some(color);
                    }
                    self.notify_window(handle);
                }
                Value::Int(1)
            }
            "guisetfont" => {
                let font = Font {
                    name: arg_str(args, 3),
                    size: arg_int(args, 0) as i32,
                    weight: arg_int(args, 1) as i32,
                    attribute: arg_int(args, 2) as i32,
                };
                if let Some(handle) = self.window_arg(args, 4) {
                    if let Some(window) = self.model.window_mut(handle) {
                        window.font = Some(font);
                    }
                    self.notify_window(handle);
                }
                Value::Int(1)
            }
            "guiseticon" => {
                if let Some(handle) = self.window_arg(args, 1) {
                    let icon = arg_str(args, 0);
                    if let Some(window) = self.model.window_mut(handle) {
                        window.icon = Some(icon);
                    }
                    self.notify_window(handle);
                }
                Value::Int(1)
            }
            "guisetcursor" => {
                if let Some(handle) = self.window_arg(args, 1) {
                    let cursor = arg_int(args, 0);
                    if let Some(window) = self.model.window_mut(handle) {
                        window.cursor = Some(cursor);
                    }
                }
                Value::Int(1)
            }
            "guisetonevent" => {
                if let Some(handle) = self.window_arg(args, 2) {
                    let event = arg_int(args, 0);
                    let handler = arg_str(args, 1);
                    if let Some(window) = self.model.window_mut(handle) {
                        window.set_on_event(event, Some(handler));
                    }
                }
                Value::Int(1)
            }
            "guisetstyle" => {
                if let Some(handle) = self.window_arg(args, 2) {
                    let (style, exstyle) = (arg_int(args, 0), arg_int(args, 1));
                    if let Some(window) = self.model.window_mut(handle) {
                        window.style = style;
                        window.exstyle = exstyle;
                    }
                }
                Value::Int(1)
            }
            "guigetstyle" => {
                let handle = self.window_arg(args, 0);
                match handle.and_then(|h| self.model.window(h)) {
                    Some(window) => {
                        ctx.set_error(0, 0);
                        Value::array(vec![
                            Value::Int(window.style),
                            Value::Int(window.exstyle),
                        ])
                    }
                    None => {
                        ctx.set_error(1, 0);
                        Value::array(vec![Value::Int(0), Value::Int(0)])
                    }
                }
            }
            "guisetcoord" | "guisetaccelerators" | "guisethelp" | "guistartgroup" => Value::Int(1),

            // ---------------- control state ----------------
            "guictrldelete" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                let ok = self.model.remove_control(id);
                self.backend.on_control_removed(id);
                self.forget_hwnd(ctx, id);
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(i64::from(ok))
            }
            "guictrlgethandle" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                ctx.set_error(0, 0);
                Value::Int(id)
            }
            "guictrlgetstate" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                match self.model.control(id) {
                    Some(control) => {
                        ctx.set_error(0, 0);
                        Value::Int(control.public_state())
                    }
                    None => {
                        ctx.set_error(1, 0);
                        Value::Int(0)
                    }
                }
            }
            "guictrlread" => self.read_control(args, ctx),
            "guictrlsetstate" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                let state = arg_int(args, 1);
                let kind = self.model.control(id).map(|control| control.kind);
                let window = self.model.control(id).map(|control| control.window);
                if let Some(control) = self.model.control_mut(id) {
                    // The official interpreter keeps these in pairs: hiding a
                    // control clears its `$GUI_SHOW` (a fresh control answers
                    // `0x50`, and after `$GUI_HIDE` it answers `0x60`), and
                    // disabling it clears `$GUI_ENABLE` (`0x90`).
                    if state & GUI_SHOW != 0 {
                        control.state |= GUI_SHOW;
                        control.state &= !GUI_HIDE;
                    }
                    if state & GUI_HIDE != 0 {
                        control.state |= GUI_HIDE;
                        control.state &= !GUI_SHOW;
                    }
                    if state & GUI_ENABLE != 0 {
                        control.state |= GUI_ENABLE;
                        control.state &= !GUI_DISABLE;
                    }
                    if state & GUI_DISABLE != 0 {
                        control.state |= GUI_DISABLE;
                        control.state &= !GUI_ENABLE;
                    }
                    // `$GUI_CHECKED` (1), `$GUI_INDETERMINATE` (2) and
                    // `$GUI_UNCHECKED` (4) describe one three-way state, so
                    // asking for one of them clears the other two. The middle
                    // one needs a `$BS_3STATE`/`$BS_AUTO3STATE` box: the probe
                    // against the official interpreter showed a plain check box
                    // answering 1 for it.
                    if state & (GUI_CHECKED | 0x02 | 0x04) != 0 {
                        control.state &= !(GUI_CHECKED | 0x02);
                        let three_state = control.style > 0
                            && matches!(control.style as u32 & 0x0F, 0x05 | 0x06);
                        if state & 0x02 != 0 && three_state {
                            control.state |= 0x02;
                        } else if state & (GUI_CHECKED | 0x02) != 0 {
                            control.state |= GUI_CHECKED;
                        }
                    }
                    if state & 0x08 != 0 {
                        control.state |= 0x08;
                    }
                    if state & 0x1000 != 0 {
                        control.state &= !0x08;
                    }
                    // A `TreeViewItem` is painted bold while `$GUI_DEFBUTTON`
                    // is set, and any call that does not ask for it turns it off
                    // again — the official interpreter clears it for
                    // `GUICtrlSetState($item, 0)` even though the control's state
                    // word still carries `$GUI_SHOW | $GUI_ENABLE`.
                    if state & 0x200 != 0 {
                        control.state |= 0x200;
                    } else {
                        control.state &= !0x200;
                    }
                    if state & 0x400 != 0 {
                        control.state |= 0x400;
                    }
                    if state & 0x800 != 0 {
                        control.state &= !0x800;
                    }
                }
                // `$GUI_FOCUS` selects an item, `$GUI_SHOW` shows a page, and
                // for a control that is a window of its own it takes the input
                // focus.
                if state & 0x100 != 0 {
                    if let Some(control) = self.model.control_mut(id) {
                        control.state |= 0x100;
                    }
                    match kind {
                        Some(ControlKind::TreeViewItem) | Some(ControlKind::ListViewItem) => {
                            let owner = self.model.part_owner(id);
                            let row = self.model.control(id).and_then(|control| control.row);
                            if let (Some(owner), Some(row)) = (owner, row) {
                                if let Some(owner_control) = self.model.control_mut(owner) {
                                    owner_control.selection = Some(row);
                                }
                            }
                        }
                        _ => {
                            if let Some(window) = window.and_then(|w| self.model.window_mut(w)) {
                                window.focus = Some(id);
                            }
                        }
                    }
                }
                if state & 0x2000 != 0 {
                    if let Some(window) = window.and_then(|w| self.model.window_mut(w)) {
                        window.focus = None;
                    }
                }
                if state & 0x800 != 0 {
                    if let Some(window) = window.and_then(|w| self.model.window_mut(w)) {
                        window.topmost = true;
                    }
                }
                if state & 0x10 != 0 && kind == Some(ControlKind::TabItem) {
                    if let Some(tab) = self.model.control(id).and_then(|control| control.parent) {
                        self.select_tab(tab, id);
                    }
                }
                self.notify_control(id);
                ctx.set_error(0, 0);
                Value::Int(1)
            }
            "guictrlsetdata" => self.set_control_data(args, ctx),
            "guictrlsetbkcolor" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                let color = arg_int(args, 1);
                if let Some(control) = self.model.control_mut(id) {
                    // The help page's own example sets `$GUI_BKCOLOR_LV_ALTERNATE`
                    // on a ListView and then the colour to alternate with, so the
                    // flag has to survive that second call.
                    let alternate = control
                        .bk_color
                        .is_some_and(|old| old & model::GUI_BKCOLOR_LV_ALTERNATE != 0)
                        && color != model::GUI_BKCOLOR_TRANSPARENT
                        && color & model::GUI_BKCOLOR_LV_ALTERNATE == 0;
                    control.bk_color = Some(if alternate {
                        color | model::GUI_BKCOLOR_LV_ALTERNATE
                    } else {
                        color
                    });
                }
                self.notify_control(id);
                Value::Int(1)
            }
            "guictrlsetcolor" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                let color = arg_int(args, 1);
                if let Some(control) = self.model.control_mut(id) {
                    control.color = Some(color);
                }
                self.notify_control(id);
                Value::Int(1)
            }
            "guictrlsetcursor" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                let cursor = arg_int(args, 1);
                if let Some(control) = self.model.control_mut(id) {
                    control.cursor = Some(cursor);
                }
                Value::Int(1)
            }
            "guictrlsetdefbkcolor" | "guictrlsetdefcolor" => Value::Int(1),
            "guictrlsetfont" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                let font = Font {
                    name: arg_str(args, 4),
                    size: arg_int(args, 1) as i32,
                    weight: arg_int(args, 2) as i32,
                    attribute: arg_int(args, 3) as i32,
                };
                if let Some(control) = self.model.control_mut(id) {
                    control.font = Some(font);
                }
                self.notify_control(id);
                Value::Int(1)
            }
            "guictrlsetimage" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                let image = arg_str(args, 1);
                if let Some(control) = self.model.control_mut(id) {
                    control.image = Some(image);
                }
                self.notify_control(id);
                Value::Int(1)
            }
            "guictrlsetlimit" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                let limit = (arg_int(args, 1), arg_int(args, 2));
                if let Some(control) = self.model.control_mut(id) {
                    control.limit = Some(limit);
                }
                Value::Int(1)
            }
            "guictrlsetonevent" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                let handler = arg_str(args, 1);
                if let Some(control) = self.model.control_mut(id) {
                    control.on_event = Some(handler);
                }
                Value::Int(1)
            }
            "guictrlsetpos" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                if let Some(control) = self.model.control_mut(id) {
                    control.x = arg_int(args, 1) as i32;
                    control.y = arg_int(args, 2) as i32;
                    control.width = arg_int(args, 3) as i32;
                    control.height = arg_int(args, 4) as i32;
                }
                self.notify_control(id);
                Value::Int(1)
            }
            "guictrlsetresizing" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                let resizing = arg_int(args, 1);
                if let Some(control) = self.model.control_mut(id) {
                    control.resizing = resizing;
                }
                Value::Int(1)
            }
            "guictrlsetstyle" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                let (style, exstyle) = (arg_int(args, 1), arg_int(args, 2));
                if let Some(control) = self.model.control_mut(id) {
                    control.style = style;
                    control.exstyle = exstyle;
                }
                self.notify_control(id);
                Value::Int(1)
            }
            "guictrlsettip" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                let tip = arg_str(args, 1);
                // `Default` (or no argument at all) leaves a parameter as it was;
                // the help page calls that "skip an optional parameter".
                let given = |i: usize| match args.get(i) {
                    None | Some(Value::Default) => None,
                    Some(_) => Some(i),
                };
                let title = given(2).map(|i| arg_str(args, i));
                let icon = given(3).map(|i| arg_int(args, i));
                let options = given(4).map(|i| arg_int(args, i));
                if let Some(control) = self.model.control_mut(id) {
                    control.tip = tip;
                    if let Some(title) = title {
                        control.tip_title = title;
                    }
                    if let Some(icon) = icon {
                        control.tip_icon = icon;
                    }
                    if let Some(options) = options {
                        control.tip_options = options;
                    }
                }
                // The tip is a backend's business: a real tooltip window has to
                // be told, and the tooltip text is the whole of what it shows.
                self.notify_control(id);
                Value::Int(1)
            }
            "guictrlsetgraphic" => self.set_graphic(args, ctx),
            "guictrlsendmsg" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                let msg = arg_int(args, 1) as u32;
                let wparam = arg_int(args, 2);
                let lparam = arg_int(args, 3);
                let sent = self
                    .model
                    .control_mut(id)
                    .map(|control| messages::send(control, msg, wparam, lparam));
                if let Some((result, true)) = sent {
                    // The model answered, and may well have changed: what it
                    // says is what every backend renders.
                    self.notify_control(id);
                    ctx.set_error(0, 0);
                    return Some(Value::Int(result));
                }
                // Nothing the model knows. A real control can still act on it,
                // as long as the message carries no pointer: a pointer a script
                // holds addresses the emulation's memory, not the control's.
                if NATIVE_ONLY_MESSAGES.contains(&msg) {
                    if let Some(result) = self.backend.send_message(id, msg, wparam as usize, lparam as isize)
                    {
                        ctx.set_error(0, 0);
                        return Some(Value::Int(result));
                    }
                }
                ctx.set_error(1, 0);
                Value::Int(0)
            }
            "guictrlrecvmsg" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                let msg = arg_int(args, 1) as u32;
                let lparam = arg_int(args, 2);
                let sent = self
                    .model
                    .control_mut(id)
                    .map(|control| messages::send(control, msg, 0, lparam));
                let Some((result, _known)) = sent else {
                    ctx.set_error(1, 0);
                    return Some(Value::array(vec![Value::Int(0)]));
                };
                self.notify_control(id);
                ctx.set_error(0, 0);
                Value::array(vec![Value::Int(result)])
            }
            "guictrlregisterlistviewsort" => Value::Int(1),
            "guictrlsendtodummy" => {
                // Tell the script's own handler by queueing a control event.
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                if args.len() > 1 {
                    self.events.push_back(GuiEvent::Control(id));
                }
                Value::Int(1)
            }

            // ---------------- windows ----------------
            "winexists" => Value::Int(i64::from(self.window_arg(args, 0).is_some())),
            "wingethandle" => {
                match self.window_arg(args, 0) {
                    Some(handle) => {
                        self.register_hwnd(ctx, handle);
                        ctx.set_error(0, 0);
                        Value::Ptr(handle)
                    }
                    None => {
                        // A failed lookup answers a plain `Ptr` — not a GUI
                        // handle — with `@error` 1 (measured).
                        ctx.set_error(1, 0);
                        Value::Ptr(0)
                    }
                }
            }
            "wingettitle" => {
                let title = self
                    .window_arg(args, 0)
                    .and_then(|h| self.model.window(h))
                    .map(|w| w.title.clone())
                    .unwrap_or_default();
                Value::Str(title)
            }
            "wingetstate" => {
                let bits = self
                    .window_arg(args, 0)
                    .and_then(|h| self.model.window(h))
                    .map(|w| w.state_bits())
                    .unwrap_or(0);
                ctx.set_error(if bits == 0 { 1 } else { 0 }, 0);
                Value::Int(bits)
            }
            "wingetpos" => {
                let window = self.window_arg(args, 0).and_then(|h| self.model.window(h));
                match window {
                    Some(w) => {
                        // The model's rectangle is the *client* area, which is
                        // what `GUICreate` means by its width and height; the
                        // frame around it is the backend's to know, and a
                        // headless one has none.
                        let (frame_width, frame_height) = self.backend.frame_size(w);
                        ctx.set_error(0, 0);
                        Value::array(vec![
                            Value::Int(i64::from(w.x)),
                            Value::Int(i64::from(w.y)),
                            Value::Int(i64::from(w.width + frame_width)),
                            Value::Int(i64::from(w.height + frame_height)),
                        ])
                    }
                    None => {
                        ctx.set_error(1, 0);
                        Value::array(vec![Value::Int(0); 4])
                    }
                }
            }
            "wingetclientsize" => {
                let window = self.window_arg(args, 0).and_then(|h| self.model.window(h));
                match window {
                    Some(w) => {
                        ctx.set_error(0, 0);
                        Value::array(vec![
                            Value::Int(i64::from(w.width)),
                            Value::Int(i64::from(w.height)),
                        ])
                    }
                    None => {
                        ctx.set_error(1, 0);
                        Value::array(vec![Value::Int(0), Value::Int(0)])
                    }
                }
            }
            "winmove" => {
                // `WinMove("title", "text", x, y [, width [, height]])`. A
                // missing argument, or `-1`, leaves that part of the geometry
                // alone — the text argument is why the coordinates start at 2.
                if let Some(handle) = self.window_arg(args, 0) {
                    let coordinate = |i: usize| match args.get(i) {
                        Some(value) => match value.to_int() {
                            -1 => None,
                            number => Some(number),
                        },
                        None => None,
                    };
                    if let Some(window) = self.model.window_mut(handle) {
                        if let Some(x) = coordinate(2) {
                            window.x = x as i32;
                        }
                        if let Some(y) = coordinate(3) {
                            window.y = y as i32;
                        }
                    }
                    // A size change moves the controls, so it goes through the
                    // same path a user's drag does.
                    let width = coordinate(4).unwrap_or_else(|| {
                        self.model.window(handle).map(|w| i64::from(w.width)).unwrap_or(0)
                    });
                    let height = coordinate(5).unwrap_or_else(|| {
                        self.model.window(handle).map(|w| i64::from(w.height)).unwrap_or(0)
                    });
                    self.resize_window(handle, width as i32, height as i32);
                    self.notify_window(handle);
                }
                Value::Int(1)
            }
            "winactivate" => {
                if let Some(handle) = self.window_arg(args, 0) {
                    for window in self.model.windows.iter_mut().flatten() {
                        window.active = window.handle == handle;
                    }
                    self.notify_window(handle);
                    ctx.set_error(0, 0);
                    Value::Int(handle)
                } else {
                    ctx.set_error(1, 0);
                    Value::Int(0)
                }
            }
            "winactive" => {
                let handle = self.window_arg(args, 0);
                let active = self
                    .model
                    .windows
                    .iter()
                    .flatten()
                    .find(|w| w.active)
                    .map(|w| w.handle);
                ctx.set_error(0, 0);
                Value::Int(if handle.is_some() && handle == active {
                    handle.unwrap_or(0)
                } else {
                    0
                })
            }
            "winclose" | "winkill" => {
                match self.window_arg(args, 0) {
                    Some(handle) => {
                        self.model.remove_window(handle);
                        self.backend.on_window_removed(handle);
                        Value::Int(1)
                    }
                    None => Value::Int(0),
                }
            }
            "winsetstate" => {
                // `WinSetState` takes a `@SW_*` flag; the `WIN_*` bits belong to
                // `WinGetState`, which keeps reporting those.
                let flag = arg_int(args, 2);
                match self.window_arg(args, 0) {
                    Some(handle) => {
                        self.apply_show_flag(handle, flag);
                        self.notify_window(handle);
                        self.backend.present();
                        ctx.set_error(0, 0);
                        Value::Int(1)
                    }
                    None => {
                        ctx.set_error(1, 0);
                        Value::Int(0)
                    }
                }
            }
            "winsettitle" => {
                if let Some(handle) = self.window_arg(args, 0) {
                    let title = arg_str(args, 2);
                    if let Some(window) = self.model.window_mut(handle) {
                        window.title = title;
                    }
                    self.notify_window(handle);
                }
                Value::Int(1)
            }
            "winlist" => {
                let mut out: Vec<Value> = Vec::new();
                let mut items: Vec<Value> = Vec::new();
                let visible: Vec<i64> = self
                    .model
                    .windows
                    .iter()
                    .flatten()
                    .filter(|window| window.visible)
                    .map(|window| window.handle)
                    .collect();
                for handle in visible {
                    self.register_hwnd(ctx, handle);
                    let title = self
                        .model
                        .window(handle)
                        .map(|w| w.title.clone())
                        .unwrap_or_default();
                    items.push(Value::array(vec![Value::Str(title), Value::Ptr(handle)]));
                }
                out.push(Value::Int(items.len() as i64));
                out.extend(items);
                Value::array(out)
            }
            "winminimizeall" => {
                for window in self.model.windows.iter_mut().flatten() {
                    window.state = WindowState::Minimized;
                }
                Value::Int(1)
            }
            "winminimizeallundo" => {
                for window in self.model.windows.iter_mut().flatten() {
                    window.state = WindowState::Normal;
                }
                Value::Int(1)
            }
            "wingettext" | "wingetclasslist" | "statusbargettext" => Value::str(""),
            "wingetprocess" => Value::Int(i64::from(std::process::id())),
            "wingetcaretpos" => Value::array(vec![Value::Int(0), Value::Int(0)]),
            "winmenuselectitem" | "winsetontop" => Value::Int(1),
            "winwait" | "winwaitactive" => {
                let handle = self.window_arg(args, 0);
                let ok = handle.is_some();
                ctx.set_error(if ok { 0 } else { 1 }, 0);
                Value::Int(if ok { handle.unwrap_or(0) } else { 0 })
            }
            "winwaitclose" | "winwaitnotactive" => {
                ctx.set_error(0, 0);
                Value::Int(i64::from(self.window_arg(args, 0).is_none()))
            }

            // ---------------- controls ----------------
            "controlgetpos" => {
                let control = self.control_arg(args);
                match control {
                    Some(control) => {
                        ctx.set_error(0, 0);
                        Value::array(vec![
                            Value::Int(i64::from(control.x)),
                            Value::Int(i64::from(control.y)),
                            Value::Int(i64::from(control.width)),
                            Value::Int(i64::from(control.height)),
                        ])
                    }
                    None => {
                        ctx.set_error(1, 0);
                        Value::array(vec![Value::Int(0); 4])
                    }
                }
            }
            "controlgettext" => {
                let text = self.control_arg(args).map(|c| c.text.clone()).unwrap_or_default();
                ctx.set_error(if text.is_empty() { 1 } else { 0 }, 0);
                Value::Str(text)
            }
            "controlsettext" => {
                let id = self
                    .control_at(args, 3)
                    .map(|(id, offset)| (id, offset))
                    .map(|(id, offset)| (id, arg_str(args, offset)));
                if let Some((id, text)) = id {
                    if let Some(control) = self.model.control_mut(id) {
                        control.text = text;
                    }
                    self.notify_control(id);
                }
                Value::Int(1)
            }
            "controlgethandle" => {
                let id = self.control_arg(args).map(|c| c.id).unwrap_or(0);
                ctx.set_error(if id == 0 { 1 } else { 0 }, 0);
                Value::Int(id)
            }
            "controlclick" => {
                // A click becomes a control event, so a script's GUIGetMsg sees it.
                if let Some(id) = self.control_arg(args).map(|c| c.id) {
                    self.events.push_back(GuiEvent::Control(id));
                }
                Value::Int(1)
            }
            "controlcommand" => self.control_command(args, ctx),
            "controllistview" => self.control_listview(args, ctx),
            "controltreeview" => self.control_treeview(args, ctx),
            "controldisable" | "controlenable" | "controlfocus" | "controlhide"
            | "controlshow" | "controlmove" | "controlsend" => {
                // `ControlMove` and `ControlSend` take their own arguments after
                // the control, so the control's place decides where those start.
                let short = match key.as_str() {
                    "controlmove" => 6,
                    "controlsend" => 3,
                    _ => 2,
                };
                let Some((id, offset)) = self.control_at(args, short) else {
                    ctx.set_error(1, 0);
                    return Some(Self::no_such_control(ctx));
                };
                if let Some(control) = self.model.control_mut(id) {
                    match key.as_str() {
                        "controldisable" => control.state |= GUI_DISABLE,
                        "controlenable" => control.state &= !GUI_DISABLE,
                        "controlhide" => control.state |= GUI_HIDE,
                        "controlshow" => control.state &= !GUI_HIDE,
                        "controlfocus" => {
                            let window = control.window;
                            if let Some(window) = self.model.window_mut(window) {
                                window.focus = Some(id);
                            }
                        }
                        "controlmove" => {
                            control.x = arg_int(args, offset) as i32;
                            control.y = arg_int(args, offset + 1) as i32;
                            control.width = arg_int(args, offset + 2) as i32;
                            control.height = arg_int(args, offset + 3) as i32;
                        }
                        "controlsend" => {
                            let text = arg_str(args, offset);
                            if !text.is_empty() {
                                self.events.push_back(GuiEvent::Control(id));
                            }
                        }
                        _ => {}
                    }
                }
                self.notify_control(id);
                ctx.set_error(0, 0);
                Value::Int(1)
            }
            "controlgetfocus" => {
                let focus = self
                    .window_arg(args, 0)
                    .and_then(|window| self.model.window(window))
                    .and_then(|window| window.focus)
                    .unwrap_or(0);
                ctx.set_error(if focus == 0 { 1 } else { 0 }, 0);
                Value::Int(focus)
            }

            // ---------------- dialogs ----------------
            "msgbox" => {
                // A backend with a user in front of it shows a real box and
                // waits; without one the scripted answer below stands in, so an
                // analysis run cannot hang on a question nobody will answer.
                let flags = arg_int(args, 0);
                let title = arg_str(args, 1);
                let text = arg_str(args, 2);
                let timeout = arg_int(args, 3);
                if let Some(answer) =
                    self.backend
                        .message_box(flags, &title, &text, timeout)
                {
                    ctx.set_error(0, 0);
                    return Some(Value::Int(answer));
                }
                // Consume a scripted answer, else OK; never blocks.
                let answer = self
                    .events
                    .iter()
                    .position(|e| matches!(e, GuiEvent::Dialog(_)))
                    .and_then(|i| self.events.remove(i))
                    .map(|e| e.message())
                    .unwrap_or(1);
                let notice = msgbox_notice(
                    arg_int(args, 0),
                    &arg_str(args, 1),
                    &arg_str(args, 2),
                    answer,
                );
                self.report_dialog(notice);
                ctx.set_error(0, 0);
                Value::Int(answer)
            }
            "inputbox" => {
                let title = arg_str(args, 0);
                let prompt = arg_str(args, 1);
                let default = arg_str(args, 2);
                let password = arg_str(args, 3);
                let timeout = arg_int(args, 4);
                if let Some(answer) = self.backend.input_box(
                    &title,
                    &prompt,
                    &default,
                    !password.is_empty(),
                    timeout,
                ) {
                    return Some(match answer {
                        Some(text) => {
                            ctx.set_error(0, 0);
                            Value::Str(text)
                        }
                        None => {
                            ctx.set_error(1, 0);
                            Value::Str(String::new())
                        }
                    });
                }
                match self.answers.pop_front() {
                    Some(text) => {
                        let notice =
                            inputbox_notice(&title, &prompt, &format!("{text:?}"));
                        self.report_dialog(notice);
                        ctx.set_error(0, 0);
                        Value::Str(text)
                    }
                    None => {
                        // A real dialog with no user: cancelled.
                        let notice = inputbox_notice(&title, &prompt, "cancelled");
                        self.report_dialog(notice);
                        ctx.set_error(1, 0);
                        Value::Str(arg_str(args, 2))
                    }
                }
            }
            "fileopendialog" | "filesavedialog" | "fileselectfolder" => {
                let title = arg_str(args, 0);
                let (kind, initial, filter, default) = match key.as_str() {
                    "fileopendialog" => (0, arg_str(args, 1), arg_str(args, 2), String::new()),
                    "filesavedialog" => (1, arg_str(args, 1), arg_str(args, 2), arg_str(args, 3)),
                    _ => (2, arg_str(args, 1), String::new(), String::new()),
                };
                let options = arg_int(args, 4);
                if let Some(answer) = self.backend.file_dialog(
                    kind, &title, &initial, &filter, &default, options,
                ) {
                    return Some(match answer {
                        Some(path) => {
                            ctx.set_error(0, 0);
                            Value::Str(path)
                        }
                        None => {
                            ctx.set_error(1, 0);
                            Value::Str(String::new())
                        }
                    });
                }
                match self.answers.pop_front() {
                    Some(path) if !path.is_empty() => {
                        let notice = file_dialog_notice(name, &title, &format!("{path:?}"));
                        self.report_dialog(notice);
                        ctx.set_error(0, 0);
                        Value::Str(path)
                    }
                    _ => {
                        let notice = file_dialog_notice(name, &title, "cancelled");
                        self.report_dialog(notice);
                        ctx.set_error(1, 0);
                        Value::str("")
                    }
                }
            }

            // ---------------- feedback ----------------
            "splashtexton" => {
                self.model.splash = model::Splash {
                    text: arg_str(args, 1),
                    x: arg_int(args, 4) as i32,
                    y: arg_int(args, 5) as i32,
                    width: arg_int(args, 2) as i32,
                    height: arg_int(args, 3) as i32,
                    visible: true,
                    ..Default::default()
                };
                self.show_splash(false);
                Value::Int(1)
            }
            "splashimageon" => {
                self.model.splash = model::Splash {
                    image: Some(arg_str(args, 1)),
                    x: arg_int(args, 4) as i32,
                    y: arg_int(args, 5) as i32,
                    width: arg_int(args, 2) as i32,
                    height: arg_int(args, 3) as i32,
                    visible: true,
                    ..Default::default()
                };
                self.show_splash(false);
                Value::Int(1)
            }
            "splashoff" => {
                self.model.splash.visible = false;
                self.show_splash(true);
                Value::Int(1)
            }
            "progresson" => {
                self.model.progress = model::Progress {
                    on: true,
                    text: arg_str(args, 1),
                    sub: arg_str(args, 2),
                    percent: 0,
                };
                self.show_progress(false);
                Value::Int(1)
            }
            "progressset" => {
                // `ProgressSet(percent, subtext, maintext)`: the subtext really
                // does come before the main text here, unlike `ProgressOn`.
                self.model.progress.percent = arg_int(args, 0);
                if args.len() > 1 {
                    self.model.progress.sub = arg_str(args, 1);
                }
                if args.len() > 2 {
                    self.model.progress.text = arg_str(args, 2);
                }
                self.show_progress(false);
                Value::Int(1)
            }
            "progressoff" => {
                self.model.progress.on = false;
                self.show_progress(true);
                Value::Int(1)
            }
            "tooltip" => {
                let text = arg_str(args, 0);
                let x = arg_int(args, 1) as i32;
                let y = arg_int(args, 2) as i32;
                self.model.tooltip = text.clone();
                self.model.tooltip_visible = !text.is_empty();
                self.backend.tooltip_window(&text, x, y);
                Value::Int(1)
            }

            // ---------------- tray ----------------
            "traycreateitem" | "traycreatemenu" => {
                let id = self.model.alloc_tray_id();
                self.model.tray.push(TrayItem {
                    id,
                    text: arg_str(args, 0),
                    state: 0,
                    on_event: None,
                    menu: key == "traycreatemenu",
                });
                Value::Int(id)
            }
            "traygetmsg" => {
                ctx.set_error(0, 0);
                Value::Int(self.poll_message(ctx))
            }
            "trayitemdelete" => {
                let id = arg_int(args, 0);
                let before = self.model.tray.len();
                self.model.tray.retain(|t| t.id != id);
                Value::Int(i64::from(self.model.tray.len() != before))
            }
            "trayitemgethandle" => Value::Int(arg_int(args, 0)),
            "trayitemgetstate" => Value::Int(
                self.model
                    .tray
                    .iter()
                    .find(|t| t.id == arg_int(args, 0))
                    .map(|t| t.state)
                    .unwrap_or(0),
            ),
            "trayitemgettext" => Value::Str(
                self.model
                    .tray
                    .iter()
                    .find(|t| t.id == arg_int(args, 0))
                    .map(|t| t.text.clone())
                    .unwrap_or_default(),
            ),
            "trayitemsetonevent" => {
                let id = arg_int(args, 0);
                let handler = arg_str(args, 1);
                if let Some(item) = self.model.tray.iter_mut().find(|t| t.id == id) {
                    item.on_event = Some(handler);
                }
                Value::Int(1)
            }
            "trayitemsetstate" => {
                let id = arg_int(args, 0);
                let state = arg_int(args, 1);
                if let Some(item) = self.model.tray.iter_mut().find(|t| t.id == id) {
                    item.state = state;
                }
                Value::Int(1)
            }
            "trayitemsettext" => {
                let id = arg_int(args, 0);
                let text = arg_str(args, 1);
                if let Some(item) = self.model.tray.iter_mut().find(|t| t.id == id) {
                    item.text = text;
                }
                Value::Int(1)
            }
            "traysetclick" | "trayseticon" | "traysetonevent" | "traysetpauseicon"
            | "traysetstate" | "traysettooltip" | "traytip" => Value::Int(1),

            // ---------------- input ----------------
            "send" | "sendkeepactive" | "mouseclick" | "mouseclickdrag" | "mousedown"
            | "mouseup" | "mousewheel" => {
                if key.starts_with("mouse") {
                    let x = arg_int(args, 1);
                    let y = arg_int(args, 2);
                    if x != 0 || y != 0 {
                        self.model.mouse = (x as i32, y as i32);
                    }
                }
                Value::Int(1)
            }
            "mousemove" => {
                self.model.mouse = (arg_int(args, 0) as i32, arg_int(args, 1) as i32);
                Value::Int(1)
            }
            "mousegetpos" => {
                let (x, y) = self.model.mouse;
                Value::array(vec![Value::Int(i64::from(x)), Value::Int(i64::from(y))])
            }
            "mousegetcursor" => Value::Int(0),
            "hotkeyset" => {
                let key_code = parse_hotkey(&arg_str(args, 0));
                let handler = arg_str(args, 1);
                if handler.is_empty() {
                    self.model.hotkeys.retain(|(k, _)| *k != key_code);
                } else {
                    self.model.hotkeys.retain(|(k, _)| *k != key_code);
                    self.model.hotkeys.push((key_code, handler));
                }
                Value::Int(1)
            }
            "blockinput" => {
                self.model.block_input = arg_int(args, 0) != 0;
                Value::Int(1)
            }

            // ---------------- pixel ----------------
            // No real screen to sample; report "nothing" rather than a colour.
            "pixelgetcolor" => Value::Int(0),
            "pixelchecksum" => Value::Int(0),
            "pixelsearch" => {
                ctx.set_error(1, 0);
                Value::Int(0)
            }

            // ---------------- misc ----------------
            "beep" | "soundplay" | "soundsetwavevolume" => {
                self.model.sounds += 1;
                Value::Int(1)
            }
            "cdtray" => {
                self.model.cd_open = !self.model.cd_open;
                Value::Int(1)
            }
            "autoitwingettitle" => Value::Str(self.model.autoit_win_title.clone()),
            "autoitwinsettitle" => {
                self.model.autoit_win_title = arg_str(args, 0);
                Value::Int(1)
            }
            "break" => Value::Int(1),

            _ => return None,
        })
    }

    /// `GUICtrlCreate*`: build a control on the current window.
    /// Create a control of `kind` from a `GUICtrlCreate...` call.
    ///
    /// Where the caption and the geometry sit in `args` depends on the kind, and
    /// the four "part" kinds ([`ControlKind::is_part`]) have no geometry at all:
    /// they are rows, nodes, pages and menu entries of another control, so they
    /// go through [`Self::create_part`].
    fn create_control(
        &mut self,
        kind: ControlKind,
        args: &[Value],
        ctx: &mut dyn HostContext,
    ) -> Value {
        let window = match self.model.active_window() {
            Some(window) => window,
            None => {
                ctx.set_error(1, 0);
                return Value::Int(0);
            }
        };
        if kind.is_part() {
            return self.create_part(kind, window, args, ctx);
        }
        // Most creators take a caption or a filename first, then the geometry.
        // `GUICtrlCreateAvi` and `GUICtrlCreateIcon` carry one more value in
        // front of it, `GUICtrlCreateObj` starts with the object itself, and
        // `GUICtrlCreateUpdown` is handed an input control instead of a
        // position. The geometry-less kinds (a `Tab`, a `Graphic`, a `Dummy`)
        // start at `left`.
        let (text, base) = match kind {
            ControlKind::Avi | ControlKind::Icon => (arg_str(args, 0), 2),
            ControlKind::Obj => (String::new(), 1),
            ControlKind::Label
            | ControlKind::Button
            | ControlKind::Checkbox
            | ControlKind::Radio
            | ControlKind::Group
            | ControlKind::Input
            | ControlKind::Edit
            | ControlKind::List
            | ControlKind::Combo
            | ControlKind::ListView
            | ControlKind::Pic
            | ControlKind::Date
            | ControlKind::MonthCal
            | ControlKind::Menu
            | ControlKind::ContextMenu => (arg_str(args, 0), 1),
            _ => (String::new(), 0),
        };
        let mut control = Control {
            id: 0,
            window,
            kind,
            text,
            x: arg_int(args, base) as i32,
            y: arg_int(args, base + 1) as i32,
            width: arg_int(args, base + 2) as i32,
            height: arg_int(args, base + 3) as i32,
            style: arg_int(args, base + 4),
            exstyle: arg_int(args, base + 5),
            // The official interpreter answers `$GUI_SHOW | $GUI_ENABLE` for a
            // control nobody has touched yet — parts included: a fresh
            // `TabItem`/`TreeViewItem` answers `GUICtrlGetState` with 0x50 even
            // though `GUICtrlRead` masks those bits out.
            state: GUI_SHOW | GUI_ENABLE,
            parent: None,
            row: None,
            data: Vec::new(),
            selection: None,
            tip: String::new(),
            tip_title: String::new(),
            tip_icon: 0,
            tip_options: 0,
            on_event: None,
            bk_color: None,
            color: None,
            font: None,
            cursor: None,
            image: None,
            limit: None,
            resizing: 0,
            draw: Vec::new(),
        };
        match kind {
            // `GUICtrlCreateUpdown($input)` is an arrow pair growing onto an
            // input control: there is no position in its arguments.
            ControlKind::Updown => {
                control.parent = self.model.control_id(arg_int(args, 0));
            }
            // A menu entry hangs under the menu it names, or under the one made
            // last; a top-level menu of the bar names no parent.
            ControlKind::Menu | ControlKind::ContextMenu => {
                control.parent = self.menu_parent(window, Some(arg_int(args, 1)));
            }
            _ => {
                // A control created while a tab page is current belongs to that
                // page, which is how `GUICtrlCreateTabItem` selects where the
                // following controls go.
                control.parent = self.current_page(window);
            }
        }
        let page = control.parent;
        let id = self.model.add_control(control);
        // A control created on a page takes that page's visibility with it.
        if let Some(tab) = page.and_then(|page| self.model.control(page)?.parent) {
            if self.model.control(tab).map(|control| control.kind) == Some(ControlKind::Tab) {
                self.apply_tab_visibility(tab);
            }
        }
        self.notify_control(id);
        ctx.set_error(0, 0);
        Value::Int(id)
    }

    /// Create one of the "part" kinds: a row, node, page or menu entry of
    /// another control.
    ///
    /// None of them takes a position. `GUICtrlCreateListViewItem` names the
    /// `ListView` that holds the row, `GUICtrlCreateTreeViewItem` names either
    /// its `TreeView` or the item it hangs under, `GUICtrlCreateMenuItem` names
    /// the menu, and `GUICtrlCreateTabItem("")` is not a control at all: it ends
    /// the tab structure, so later controls belong to the window again.
    fn create_part(
        &mut self,
        kind: ControlKind,
        window: i64,
        args: &[Value],
        ctx: &mut dyn HostContext,
    ) -> Value {
        let text = arg_str(args, 0);
        if kind == ControlKind::TabItem && text.is_empty() {
            self.current_tabitem = None;
            ctx.set_error(0, 0);
            return Value::Int(0);
        }
        let parent = match kind {
            ControlKind::ListViewItem | ControlKind::MenuItem => {
                match self.model.control_id(arg_int(args, 1)) {
                    Some(parent) => Some(parent),
                    None => {
                        ctx.set_error(1, 0);
                        return Value::Int(0);
                    }
                }
            }
            ControlKind::TreeViewItem => {
                let named = if args.len() > 1 {
                    self.model.control_id(arg_int(args, 1))
                } else {
                    None
                };
                match named.or_else(|| self.last_tree_view(window)) {
                    Some(parent) => Some(parent),
                    None => {
                        ctx.set_error(1, 0);
                        return Value::Int(0);
                    }
                }
            }
            ControlKind::TabItem => match self.current_tab(window) {
                Some(tab) => Some(tab),
                None => {
                    ctx.set_error(1, 0);
                    return Value::Int(0);
                }
            },
            _ => None,
        };
        // The row is a row of the *owner's* window, which is where a renderer
        // has to put it.
        let window = parent
            .and_then(|parent| self.model.control(parent))
            .map(|control| control.window)
            .unwrap_or(window);
        let control = Control {
            id: 0,
            window,
            kind,
            text,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            style: 0,
            exstyle: 0,
            // Parts answer `GUICtrlGetState` with the same `0x50` a control
            // does; `GUICtrlRead` is what masks those bits out again.
            state: GUI_SHOW | GUI_ENABLE,
            parent,
            row: None,
            data: Vec::new(),
            selection: None,
            tip: String::new(),
            tip_title: String::new(),
            tip_icon: 0,
            tip_options: 0,
            on_event: None,
            bk_color: None,
            color: None,
            font: None,
            cursor: None,
            image: None,
            limit: None,
            resizing: 0,
            draw: Vec::new(),
        };
        let id = self.model.add_part_control(control);
        if kind == ControlKind::TabItem {
            // A page becomes the current one for the controls that follow it,
            // and the first page is the one the tab control shows.
            let tab = self.model.control(id).and_then(|control| control.parent);
            if let Some(tab) = tab {
                self.current_tabitem = Some(id);
                if self.model.control(tab).and_then(|control| control.selection).is_none() {
                    self.select_tab(tab, id);
                }
            }
        }
        self.notify_control(id);
        // Page membership decides visibility, and the new row may be on a page
        // that is not the selected one.
        if let Some(tab) = self.model.control(id).and_then(|control| control.parent) {
            if self
                .model
                .control(tab)
                .map(|control| control.kind)
                == Some(ControlKind::Tab)
            {
                self.apply_tab_visibility(tab);
            }
        }
        ctx.set_error(0, 0);
        Value::Int(id)
    }

    /// The tab page new controls belong to, when one is current.
    fn current_page(&self, window: i64) -> Option<i64> {
        let page = self.current_tabitem?;
        let control = self.model.control(page)?;
        (control.window == window).then_some(page)
    }

    /// The tab control of `window`: AutoIt's GUI holds at most one.
    fn current_tab(&self, window: i64) -> Option<i64> {
        self.model
            .window(window)?
            .controls
            .iter()
            .filter_map(|id| self.model.control(*id))
            .find(|control| control.kind == ControlKind::Tab)
            .map(|control| control.id)
    }

    /// The `TreeView` a new item goes under when the script did not name one:
    /// the one made last.
    fn last_tree_view(&self, window: i64) -> Option<i64> {
        self.model
            .window(window)?
            .controls
            .iter()
            .rev()
            .filter_map(|id| self.model.control(*id))
            .find(|control| control.kind == ControlKind::TreeView)
            .map(|control| control.id)
    }

    /// Where a menu entry goes: the menu it names, else the one made last.
    fn menu_parent(&self, window: i64, named: Option<i64>) -> Option<i64> {
        if let Some(id) = named.and_then(|id| self.model.control_id(id)) {
            return Some(id);
        }
        self.model
            .window(window)?
            .controls
            .iter()
            .rev()
            .filter_map(|id| self.model.control(*id))
            .find(|control| {
                matches!(
                    control.kind,
                    ControlKind::Menu | ControlKind::ContextMenu | ControlKind::MenuItem
                )
            })
            .map(|control| control.id)
    }

    /// Make `page` the selected page of `tab`.
    fn select_tab(&mut self, tab: i64, page: i64) {
        let row = self.model.control(page).and_then(|control| control.row);
        if let (Some(row), Some(tab_control)) = (row, self.model.control_mut(tab)) {
            tab_control.selection = Some(row);
        }
        self.apply_tab_visibility(tab);
    }

    /// Reveal the controls of the selected `Tab` page and hide the rest.
    ///
    /// A page that is not selected hides its controls with
    /// [`GUI_PAGE_HIDDEN`](autoitv3_gui_model::GUI_PAGE_HIDDEN), which keeps a
    /// script's own `$GUI_HIDE` on a control on the visible page intact.
    fn apply_tab_visibility(&mut self, tab: i64) {
        let Some(selected) = self.model.control(tab).and_then(|control| control.selection) else {
            return;
        };
        for page in self.model.children_of(tab) {
            let row = self.model.control(page).and_then(|control| control.row);
            let hidden = row != Some(selected);
            for child in self.model.children_of(page) {
                if let Some(control) = self.model.control_mut(child) {
                    if hidden {
                        control.state |= GUI_PAGE_HIDDEN;
                    } else {
                        control.state &= !GUI_PAGE_HIDDEN;
                    }
                }
                self.notify_control(child);
            }
        }
    }

    /// `GUICtrlRead`: the state or data of a control.
    ///
    /// The second argument selects AutoIt's "advanced" value, which is a
    /// different thing for almost every control: an item's own text where the
    /// default read gives the state bits, or the selected item's identifier
    /// where the default read gives its text.
    fn read_control(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let Some(id) = self.resolve_control(args, 0) else {
            ctx.set_error(1, 0);
            return Value::Int(0);
        };
        let advanced = arg_int(args, 1) != 0;
        let Some(control) = self.model.control(id) else {
            ctx.set_error(1, 0);
            return Value::Int(0);
        };
        ctx.set_error(0, 0);
        let selected = control.selection.unwrap_or(0);
        match control.kind {
            ControlKind::Checkbox | ControlKind::Radio => {
                if advanced {
                    Value::Str(control.text.clone())
                } else if control.state & 0x02 != 0 {
                    Value::Int(0x02)
                } else if control.is_checked() {
                    Value::Int(GUI_CHECKED)
                } else {
                    Value::Int(0x04)
                }
            }
            ControlKind::Progress | ControlKind::Slider | ControlKind::Updown => {
                Value::Int(control.text.trim().parse().unwrap_or(0))
            }
            ControlKind::List | ControlKind::Combo => match control.data.get(selected) {
                Some(text) => Value::Str(text.clone()),
                None => Value::Int(0),
            },
            // AutoIt answers these with the *identifier* of the selected item,
            // which is how a script turns a click into the item it selects.
            ControlKind::ListView => match control.selection {
                Some(row) => Value::Int(self.model.part_at(id, row).unwrap_or(0)),
                None => Value::Int(0),
            },
            ControlKind::TreeView => match control.selection {
                Some(row) => match self.model.part_at(id, row) {
                    Some(item) => {
                        if advanced {
                            Value::Str(control.data.get(row).cloned().unwrap_or_default())
                        } else {
                            Value::Int(item)
                        }
                    }
                    None => Value::Int(0),
                },
                None => Value::Int(0),
            },
            ControlKind::ListViewItem => {
                // The advanced value is the item's check state, but only where
                // the `ListView` was given `$LVS_EX_CHECKBOXES`; without it the
                // official interpreter answers the text again.
                let checkboxes = self
                    .model
                    .part_owner(id)
                    .and_then(|owner| self.model.control(owner))
                    .map(|owner| owner.exstyle & LVS_EX_CHECKBOXES != 0)
                    .unwrap_or(false);
                if advanced && checkboxes {
                    Value::Int(if control.is_checked() { 1 } else { 4 })
                } else {
                    Value::Str(self.item_text(id, control))
                }
            }
            ControlKind::TreeViewItem => {
                if advanced {
                    Value::Str(self.item_text(id, control))
                } else {
                    Value::Int(control.state & ITEM_STATE_MASK)
                }
            }
            // A tab page has no readable value of its own: the official
            // interpreter answers an empty string in both modes, and the page's
            // title is what its label shows.
            ControlKind::TabItem => Value::str(""),
            ControlKind::Tab => {
                let tab = id;
                match control.selection {
                    Some(index) if advanced => Value::Int(
                        self.model
                            .part_at(tab, index)
                            .unwrap_or(0),
                    ),
                    Some(index) => Value::Int(index as i64),
                    None => Value::Int(-1),
                }
            }
            ControlKind::Menu | ControlKind::MenuItem => {
                if advanced {
                    Value::Str(control.text.clone())
                } else {
                    Value::Int(control.public_state())
                }
            }
            _ => Value::Str(control.text.clone()),
        }
    }

    /// The text of an item control: its owner's row, which is where the text
    /// lives once the item was created.
    ///
    /// A `ListView` item is read back with a separator after every cell,
    /// including the last — the official interpreter answers
    /// `"i1a|i1b|i1c|"` for a three-column row. A tree node is plain text.
    fn item_text(&self, id: i64, control: &Control) -> String {
        let Some(row) = control.row else {
            return control.text.clone();
        };
        let owner = self
            .model
            .part_owner(id)
            .and_then(|owner| self.model.control(owner));
        let text = owner
            .and_then(|owner| owner.data.get(row))
            .cloned()
            .unwrap_or_else(|| control.text.clone());
        if control.kind != ControlKind::ListViewItem {
            return text;
        }
        // The read is the row as wide as the ListView: the official interpreter
        // answers `"solo|||"` for a one-cell row in a three-column list, which is
        // the row padded to the columns with a separator after each.
        let columns = owner
            .map(|owner| owner.text.split('|').count())
            .unwrap_or(0);
        let mut cells: Vec<String> = text.split('|').map(|cell| cell.to_string()).collect();
        while cells.len() < columns {
            cells.push(String::new());
        }
        format!("{}|", cells.join("|"))
    }

    /// `GUICtrlSetData`.
    fn set_control_data(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let Some(id) = self.resolve_control(args, 0) else {
            ctx.set_error(1, 0);
            return Value::Int(0);
        };
        let data = arg_str(args, 1);
        let default = (args.len() > 2).then(|| arg_str(args, 2));
        let Some(kind) = self.model.control(id).map(|c| c.kind) else {
            ctx.set_error(1, 0);
            return Value::Int(0);
        };
        match kind {
            // `data` is the items to add; a leading separator (or nothing at
            // all) throws the old list away first.
            ControlKind::List | ControlKind::Combo => {
                let items: Vec<String> = data
                    .split('|')
                    .filter(|item| !(item.is_empty() && data.ends_with('|')))
                    .map(|item| item.to_string())
                    .collect();
                let Some(control) = self.model.control_mut(id) else {
                    ctx.set_error(1, 0);
                    return Value::Int(0);
                };
                if data.is_empty() || data.starts_with('|') {
                    control.data.clear();
                    control.selection = None;
                }
                let added: Vec<String> = items.into_iter().filter(|item| !item.is_empty()).collect();
                control.data.extend(added);
                if let Some(default) = default.filter(|value| !value.is_empty()) {
                    if let Some(index) = control.data.iter().position(|item| *item == default) {
                        control.selection = Some(index);
                    }
                }
            }
            // `GUICtrlSetData` on a `ListView` itself does *nothing*: the probe
            // against the official interpreter showed neither a row added
            // (`lv.setdata_count=0`) nor a selected row changed. Rows and their
            // cells go through `GUICtrlCreateListViewItem` and the items.
            ControlKind::ListView => {}
            // An item writes the cells it names: an empty cell leaves that
            // column alone (the probe's `||9` kept the first two), and an
            // entirely empty `data` erases the first, as the help page says.
            ControlKind::ListViewItem => {
                let owner = self.model.part_owner(id);
                let row = self.model.control(id).and_then(|control| control.row);
                if let (Some(owner), Some(row)) = (owner, row) {
                    let updated = self.updated_row(owner, row, &data);
                    if let Some(owner_control) = self.model.control_mut(owner) {
                        if row < owner_control.data.len() {
                            owner_control.data[row] = updated.clone();
                        }
                    }
                    if let Some(control) = self.model.control_mut(id) {
                        control.text = updated;
                    }
                }
            }
            ControlKind::Progress | ControlKind::Slider | ControlKind::Updown => {
                // The control's own value; `ProgressOn`/`ProgressSet` is a
                // separate popup window, not this control.
                if let Some(control) = self.model.control_mut(id) {
                    control.text = data;
                }
            }
            _ => {
                // For a part — a tree node, a tab page, a menu entry — the text
                // lives in the owner's item list as well.
                if let Some(control) = self.model.control_mut(id) {
                    control.text = data.clone();
                }
                self.set_part_text(id, &data);
            }
        }
        self.notify_control(id);
        ctx.set_error(0, 0);
        Value::Int(1)
    }

    /// The row `data` leaves behind when it is written into row `row` of the
    /// list `owner`.
    ///
    /// The separator positions are the column numbers, and an empty field
    /// leaves its column alone *unless it is the last field* — which is what the
    /// official interpreter does with the calls the probes made:
    ///
    /// * `""` (one field, the last) erases the first cell,
    /// * `"|"` (two fields, the last one empty) erases the second cell,
    /// * `"||9"` keeps the first two and writes the third,
    /// * `"only|two"` writes the first two.
    fn updated_row(&self, owner: i64, row: usize, data: &str) -> String {
        let existing = self
            .model
            .control(owner)
            .and_then(|control| control.data.get(row))
            .cloned()
            .unwrap_or_default();
        let mut cells: Vec<String> = existing.split('|').map(|cell| cell.to_string()).collect();
        let fields: Vec<&str> = data.split('|').collect();
        let last = fields.len().saturating_sub(1);
        for (index, field) in fields.iter().enumerate() {
            if field.is_empty() && index != last {
                continue;
            }
            if index >= cells.len() {
                // An empty field only ever *erases* a cell that is there: the
                // official interpreter answers `"a|b|c|"` for `"|||"`, so a row
                // is never grown by an empty last field.
                if field.is_empty() {
                    continue;
                }
                while cells.len() < index {
                    cells.push(String::new());
                }
                cells.push((*field).to_string());
            } else {
                cells[index] = (*field).to_string();
            }
        }
        cells.join("|")
    }

    /// Write a part's new text through to its owner's item list.
    fn set_part_text(&mut self, id: i64, text: &str) {
        let Some(owner) = self.model.part_owner(id) else {
            return;
        };
        let Some(row) = self.model.control(id).and_then(|control| control.row) else {
            return;
        };
        if let Some(owner_control) = self.model.control_mut(owner) {
            if row < owner_control.data.len() {
                owner_control.data[row] = text.to_string();
            }
        }
    }

    /// `GUICtrlSetGraphic`: record a drawing command.
    ///
    /// The types are the ones `GUIConstantsEx.au3` defines — even numbers, in the
    /// order the help page lists them — and the pen keeps its own position so a
    /// line starts where the last one ended.
    fn set_graphic(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let Some(id) = self.resolve_control(args, 0) else {
            return Self::no_such_control(ctx);
        };
        let kind = arg_int(args, 1);
        let (pen_x, pen_y) = self.draw_pen;
        let end = (arg_int(args, 2) as i32, arg_int(args, 3) as i32);
        let commands: Vec<DrawCmd> = match kind {
            // $GUI_GR_CLOSE: the current drawing is closed by a line back to
            // where it started, which the pen position already describes.
            1 => Vec::new(),
            // $GUI_GR_LINE
            2 => {
                self.draw_pen = end;
                vec![DrawCmd::Line {
                    x1: pen_x,
                    y1: pen_y,
                    x2: end.0,
                    y2: end.1,
                }]
            }
            // $GUI_GR_BEZIER: x, y, x1, y1, x2, y2
            4 => {
                let command = DrawCmd::Bezier {
                    x1: pen_x,
                    y1: pen_y,
                    x2: arg_int(args, 4) as i32,
                    y2: arg_int(args, 5) as i32,
                    x3: arg_int(args, 6) as i32,
                    y3: arg_int(args, 7) as i32,
                    x4: end.0,
                    y4: end.1,
                };
                self.draw_pen = end;
                vec![command]
            }
            // $GUI_GR_MOVE: the pen moves without drawing.
            6 => {
                self.draw_pen = end;
                Vec::new()
            }
            // $GUI_GR_COLOR: colour [, background]; `$GUI_GR_NOBKCOLOR` (-2)
            // means the closed shapes are not filled.
            8 => {
                let mut commands = vec![DrawCmd::SetColor(arg_int(args, 2))];
                if let Some(background) = args.get(3) {
                    commands.push(DrawCmd::SetBkColor(background.to_int()));
                }
                commands
            }
            // $GUI_GR_RECT
            10 => vec![DrawCmd::Rect {
                x: end.0,
                y: end.1,
                w: arg_int(args, 4) as i32,
                h: arg_int(args, 5) as i32,
            }],
            // $GUI_GR_ELLIPSE
            12 => vec![DrawCmd::Ellipse {
                x: end.0,
                y: end.1,
                w: arg_int(args, 4) as i32,
                h: arg_int(args, 5) as i32,
            }],
            // $GUI_GR_PIE: x, y, r, startangle, sweepangle
            14 => vec![DrawCmd::Pie {
                x: end.0,
                y: end.1,
                r: arg_int(args, 4) as i32,
                start: arg_int(args, 5) as i32,
                sweep: arg_int(args, 6) as i32,
            }],
            // $GUI_GR_DOT and $GUI_GR_PIXEL
            16 | 18 => vec![DrawCmd::Dot { x: end.0, y: end.1 }],
            // $GUI_GR_HINT: control points are not drawn.
            20 => Vec::new(),
            // $GUI_GR_REFRESH: a redraw, which the notification below is.
            22 => Vec::new(),
            // $GUI_GR_PENSIZE
            24 => vec![DrawCmd::SetWidth(arg_int(args, 2) as i32)],
            _ => Vec::new(),
        };
        for command in commands {
            self.model.draw(id, command);
        }
        self.notify_control(id);
        ctx.set_error(0, 0);
        Value::Int(1)
    }

    /// Resolve the window argument at `index`, defaulting to the current one.
    fn window_arg(&self, args: &[Value], index: usize) -> Option<i64> {
        // A `Ptr` is a *handle*: `WinGetState($fg[0])` on a `DllCall`
        // `hwnd` answers the window the pointer names, while an `Int` or a
        // string is a title (and `WinGetState(Int($h))` fails — measured).
        if let Some(Value::Ptr(handle)) = args.get(index) {
            return self.model.window(*handle).map(|_| *handle);
        }
        let spec = args
            .get(index)
            .map(|v| v.to_autoit_string())
            .unwrap_or_default();
        self.model.resolve_window(&spec)
    }

    /// Resolve the `(window, control text)` pair `Control*` functions take.
    /// Resize a window and move its controls the way their docking asks.
    ///
    /// AutoIt's `$GUI_DOCK*` flags say what does *not* change when the window
    /// does; a control nobody set them on keeps its place. Returns whether the
    /// size actually changed.
    fn resize_window(&mut self, handle: i64, width: i32, height: i32) -> bool {
        let Some(window) = self.model.window(handle) else {
            return false;
        };
        let (old_width, old_height) = (window.width.max(1), window.height.max(1));
        let (new_width, new_height) = (width.max(1), height.max(1));
        if (old_width, old_height) == (new_width, new_height) {
            return false;
        }
        let (dx, dy) = (new_width - old_width, new_height - old_height);
        let controls = window.controls.clone();
        for id in controls {
            let Some(control) = self.model.control(id) else {
                continue;
            };
            // A part is a row or a page, not a window of its own.
            if control.kind.is_part() || control.parent.is_some() {
                continue;
            }
            let dock = if control.resizing != 0 {
                control.resizing
            } else {
                default_dock(control.kind)
            };
            let (x, y, control_width, control_height) = docked(
                control,
                dock,
                dx,
                dy,
                old_width.max(1),
                old_height.max(1),
                new_width,
                new_height,
            );
            if let Some(control) = self.model.control_mut(id) {
                control.x = x;
                control.y = y;
                control.width = control_width;
                control.height = control_height;
            }
            self.notify_control(id);
        }
        if let Some(window) = self.model.window_mut(handle) {
            window.width = new_width;
            window.height = new_height;
        }
        true
    }

    /// Hand the splash window's state to the backend, which may show one.
    fn show_splash(&mut self, off: bool) {
        let splash = self.model.splash.clone();
        self.backend.splash(&splash, off);
    }

    /// Hand the progress window's state to the backend, which may show one.
    fn show_progress(&mut self, off: bool) {
        let progress = self.model.progress.clone();
        self.backend.progress(&progress, off);
    }

    /// Resolve a `controlID` argument the way AutoIt's functions do: `-1` is
    /// the control created last, anything else must exist.
    fn resolve_control(&self, args: &[Value], index: usize) -> Option<i64> {
        self.model.control_id(arg_int(args, index))
    }

    /// The error path shared by every function that takes a `controlID`.
    fn no_such_control(ctx: &mut dyn HostContext) -> Value {
        ctx.set_error(1, 0);
        Value::Int(0)
    }

    /// Resolve the control a `Control*` function names, and where its own
    /// arguments start.
    ///
    /// AutoIt's shape is `("title", "text", controlID, ...)`, where `text` is
    /// the *window's* text and `controlID` is a control identifier or the
    /// control's own text. The shorter `("title", controlID, ...)` form is
    /// accepted as well — it is what a call without a window text looks like —
    /// so the control is tried third first and second afterwards. `short` is how
    /// many arguments the call has without the window text, which is what tells
    /// the two shapes apart when optional arguments follow.
    fn control_at(&self, args: &[Value], short: usize) -> Option<(i64, usize)> {
        let window = self.window_arg(args, 0)?;
        let candidates: &[(usize, usize)] = if args.len() > short {
            &[(2, 3), (1, 2)]
        } else {
            &[(1, 2)]
        };
        for (index, offset) in candidates {
            let text = args
                .get(*index)
                .map(|value| value.to_autoit_string())
                .unwrap_or_default();
            if let Some(id) = self.model.find_control(window, &text) {
                return Some((id, *offset));
            }
        }
        None
    }

    fn control_arg(&self, args: &[Value]) -> Option<&Control> {
        let (id, _) = self.control_at(args, 2)?;
        self.model.control(id)
    }

    /// `ControlCommand`: the commands the help page lists.
    fn control_command(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let Some((id, offset)) = self.control_at(args, 3) else {
            ctx.set_error(1, 0);
            return Value::str("");
        };
        let command = args
            .get(offset)
            .map(|value| value.to_autoit_string().to_ascii_lowercase())
            .unwrap_or_default();
        // The option is the argument after the command; the short shape puts its
        // own arguments one place earlier.
        let option_arg = args
            .get(offset + 1)
            .map(|value| value.to_autoit_string())
            .unwrap_or_default();
        let Some(control) = self.model.control(id) else {
            ctx.set_error(1, 0);
            return Value::str("");
        };
        let visible = control.is_visible();
        let enabled = control.is_enabled();
        let selected = control.selection.unwrap_or(0);
        let current = control.data.get(selected).cloned().unwrap_or_default();
        let count = control.data.len();
        ctx.set_error(0, 0);
        match command.as_str() {
            "isvisible" => Value::Int(i64::from(visible)),
            "isenabled" => Value::Int(i64::from(enabled)),
            "getcount" => Value::Int(count as i64),
            "getcurrentselection" => Value::Str(current),
            "getlinecount" => Value::Int(
                self.model
                    .control(id)
                    .map(|control| control.text.matches('\n').count() as i64 + 1)
                    .unwrap_or(0),
            ),
            "getline" => {
                let line: usize = option_arg.trim().parse().unwrap_or(0);
                let text = self.model.control(id).map(|c| c.text.clone()).unwrap_or_default();
                Value::Str(text.split('\n').nth(line).unwrap_or("").trim_end().to_string())
            }
            "addstring" => {
                if let Some(control) = self.model.control_mut(id) {
                    control.data.push(option_arg);
                }
                self.notify_control(id);
                Value::Int(1)
            }
            "delstring" => {
                let occurrence: usize = option_arg.trim().parse().unwrap_or(0);
                if let Some(control) = self.model.control_mut(id) {
                    if occurrence < control.data.len() {
                        control.data.remove(occurrence);
                        control.selection = None;
                    }
                }
                self.notify_control(id);
                Value::Int(1)
            }
            "findstring" => {
                let index = self
                    .model
                    .control(id)
                    .and_then(|control| control.data.iter().position(|item| *item == option_arg))
                    .map(|index| index as i64)
                    .unwrap_or(-1);
                Value::Int(index)
            }
            "setcurrentselection" | "selectstring" => {
                let index = if command == "selectstring" {
                    self.model
                        .control(id)
                        .and_then(|control| control.data.iter().position(|item| *item == option_arg))
                } else {
                    option_arg.trim().parse::<usize>().ok()
                };
                if let Some(control) = self.model.control_mut(id) {
                    control.selection = index;
                }
                self.notify_control(id);
                Value::Int(1)
            }
            "ischecked" => Value::Int(i64::from(
                self.model.control(id).map(|c| c.is_checked()).unwrap_or(false),
            )),
            "check" | "uncheck" => {
                let check = command == "check";
                if let Some(control) = self.model.control_mut(id) {
                    if check {
                        control.state |= 0x01;
                    } else {
                        control.state &= !0x01;
                    }
                }
                self.notify_control(id);
                Value::Int(1)
            }
            "editpaste" => {
                if let Some(control) = self.model.control_mut(id) {
                    control.text.push_str(&option_arg);
                }
                self.notify_control(id);
                Value::Int(1)
            }
            "currenttab" => Value::Int(
                self.model
                    .control(id)
                    .and_then(|control| control.selection)
                    .map(|index| index as i64)
                    .unwrap_or(-1),
            ),
            "tabright" | "tableft" => {
                let step = if command == "tabright" { 1 } else { usize::MAX };
                if let Some(control) = self.model.control_mut(id) {
                    let pages = control.data.len();
                    if pages > 0 {
                        let current = control.selection.unwrap_or(0);
                        control.selection = Some((current + step) % pages);
                    }
                }
                self.notify_control(id);
                Value::Int(1)
            }
            "getcurrentline" | "getcurrentcol" | "getselected" => Value::str(""),
            "showdropdown" | "hidedropdown" | "sendcommandid" => Value::Int(1),
            _ => {
                ctx.set_error(1, 0);
                Value::str("")
            }
        }
    }

    /// `ControlListView`: the commands the help page lists.
    ///
    /// The model keeps one selection per list, so the multi-select commands
    /// answer for that one item.
    fn control_listview(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let Some((id, offset)) = self.control_at(args, 3) else {
            ctx.set_error(1, 0);
            return Value::str("");
        };
        let command = args
            .get(offset)
            .map(|value| value.to_autoit_string().to_ascii_lowercase())
            .unwrap_or_default();
        let option1 = arg_int(args, offset + 1);
        let option2 = arg_int(args, offset + 2);
        let Some(control) = self.model.control(id) else {
            ctx.set_error(1, 0);
            return Value::str("");
        };
        let rows = control.data.clone();
        let selection = control.selection;
        ctx.set_error(0, 0);
        match command.as_str() {
            "getitemcount" => Value::Int(rows.len() as i64),
            // The official interpreter answers the ListView's *column* count
            // here, not the widest row minus its first cell.
            "getsubitemcount" => Value::Int(
                self.model
                    .control(id)
                    .map(|control| control.text.split('|').count() as i64)
                    .unwrap_or(0),
            ),
            "getselectedcount" => Value::Int(i64::from(selection.is_some())),
            "getselected" => {
                let all = args.len() > offset + 1 && arg_int(args, offset + 1) == 1;
                match selection {
                    Some(index) if all => Value::Str(index.to_string()),
                    Some(index) => Value::Int(index as i64),
                    None => Value::str(""),
                }
            }
            "isselected" => Value::Int(i64::from(selection == Some(option1.max(0) as usize))),
            "gettext" => {
                let item = option1.max(0) as usize;
                let subitem = option2.max(0) as usize;
                Value::Str(
                    rows.get(item)
                        .and_then(|row| row.split('|').nth(subitem))
                        .unwrap_or("")
                        .to_string(),
                )
            }
            "finditem" => {
                let needle = args
                    .get(offset + 1)
                    .map(|value| value.to_autoit_string())
                    .unwrap_or_default();
                let subitem = option2.max(0) as usize;
                let index = rows
                    .iter()
                    .position(|row| {
                        row.split('|')
                            .nth(subitem)
                            .map(|cell| cell == needle)
                            .unwrap_or(false)
                    })
                    .map(|index| index as i64)
                    .unwrap_or(-1);
                Value::Int(index)
            }
            "select" | "deselect" => {
                let from = option1.max(0) as usize;
                let to = if option2 >= option1 { option2 as usize } else { from };
                let wanted = command == "select";
                if let Some(control) = self.model.control_mut(id) {
                    control.selection = wanted.then_some(from).filter(|index| {
                        // A range selection in a single-selection list is the
                        // first item of the range.
                        *index < rows.len() && to >= from
                    });
                }
                self.notify_control(id);
                Value::Int(1)
            }
            "selectall" => {
                if let Some(control) = self.model.control_mut(id) {
                    control.selection = (!rows.is_empty()).then_some(0);
                }
                self.notify_control(id);
                Value::Int(1)
            }
            "selectclear" => {
                if let Some(control) = self.model.control_mut(id) {
                    control.selection = None;
                }
                self.notify_control(id);
                Value::Int(1)
            }
            "selectinvert" => {
                if let Some(control) = self.model.control_mut(id) {
                    control.selection = match selection {
                        Some(_) => None,
                        None if !rows.is_empty() => Some(0),
                        None => None,
                    };
                }
                self.notify_control(id);
                Value::Int(1)
            }
            "viewchange" => {
                let view = args
                    .get(offset + 1)
                    .map(|value| value.to_autoit_string().to_ascii_lowercase())
                    .unwrap_or_default();
                let style = match view.as_str() {
                    "list" => 0x0000_0001,
                    "details" | "report" => 0x0000_0001,
                    "smallicons" => 0x0000_0002,
                    "largeicons" | "icon" => 0x0000_0000,
                    _ => 0x0000_0001,
                };
                if let Some(control) = self.model.control_mut(id) {
                    control.style = style as i64;
                }
                self.notify_control(id);
                Value::Int(1)
            }
            _ => {
                ctx.set_error(1, 0);
                Value::str("")
            }
        }
    }

    /// `ControlTreeView`: the commands the help page lists.
    ///
    /// An item is named level by level, `|` between them, by its text or by an
    /// index with `#` in front (`"#0|#1"`); an empty reference is the tree's
    /// root.
    fn control_treeview(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let Some((id, offset)) = self.control_at(args, 3) else {
            ctx.set_error(1, 0);
            return Value::str("");
        };
        let command = args
            .get(offset)
            .map(|value| value.to_autoit_string().to_ascii_lowercase())
            .unwrap_or_default();
        let reference = args
            .get(offset + 1)
            .map(|value| value.to_autoit_string())
            .unwrap_or_default();
        let tree = id;
        let row = if command == "getselected" {
            self.model.control(tree).and_then(|control| control.selection)
        } else if command == "getitemcount" && reference.trim().is_empty() {
            None
        } else {
            self.tree_row(tree, &reference)
        };
        if row.is_none() && !(command == "getitemcount" && reference.trim().is_empty()) {
            if !self
                .model
                .control(tree)
                .map(|control| !control.data.is_empty())
                .unwrap_or(false)
            {
                ctx.set_error(1, 0);
                return Value::str("");
            }
        }
        ctx.set_error(0, 0);
        let item_id = row.and_then(|row| self.model.part_at(tree, row));
        match command.as_str() {
            "exists" => Value::Int(i64::from(row.is_some())),
            "getitemcount" => Value::Int(self.tree_children(tree, row).len() as i64),
            "gettext" => Value::Str(
                row.and_then(|row| {
                    self.model
                        .control(tree)
                        .and_then(|control| control.data.get(row).cloned())
                })
                .unwrap_or_default(),
            ),
            "getselected" => {
                let by_index = args.len() > offset + 1 && arg_int(args, offset + 1) == 1;
                match row {
                    Some(row) if by_index => Value::Int(row as i64),
                    Some(row) => Value::Str(
                        self.model
                            .control(tree)
                            .and_then(|control| control.data.get(row).cloned())
                            .unwrap_or_default(),
                    ),
                    None => Value::str(""),
                }
            }
            "select" => {
                if let Some(row) = row {
                    if let Some(control) = self.model.control_mut(tree) {
                        control.selection = Some(row);
                    }
                    self.notify_control(tree);
                }
                Value::Int(1)
            }
            "expand" | "collapse" | "check" | "uncheck" => {
                let bit = match command.as_str() {
                    "expand" => 0x400,
                    "collapse" => 0x400,
                    "check" => 0x01,
                    _ => 0x01,
                };
                let set = matches!(command.as_str(), "expand" | "check");
                if let Some(item) = item_id {
                    if let Some(control) = self.model.control_mut(item) {
                        if set {
                            control.state |= bit;
                        } else {
                            control.state &= !bit;
                        }
                    }
                    self.notify_control(item);
                }
                Value::Int(1)
            }
            "ischecked" => Value::Int(match item_id {
                Some(item) => i64::from(
                    self.model
                        .control(item)
                        .map(|control| control.is_checked())
                        .unwrap_or(false),
                ),
                None => -1,
            }),
            _ => {
                ctx.set_error(1, 0);
                Value::str("")
            }
        }
    }

    /// The rows that hang under `parent` (a row, or the tree's root).
    fn tree_children(&self, tree: i64, parent: Option<usize>) -> Vec<usize> {
        let parent_id = match parent {
            Some(row) => match self.model.part_at(tree, row) {
                Some(id) => id,
                None => return Vec::new(),
            },
            None => tree,
        };
        let mut rows: Vec<usize> = self
            .model
            .children_of(parent_id)
            .into_iter()
            .filter_map(|id| self.model.control(id).and_then(|control| control.row))
            .collect();
        rows.sort_unstable();
        rows
    }

    /// Resolve a `ControlTreeView` item reference to a row.
    fn tree_row(&self, tree: i64, reference: &str) -> Option<usize> {
        let reference = reference.trim();
        if reference.is_empty() {
            return None;
        }
        let mut parent: Option<usize> = None;
        for level in reference.split('|') {
            let level = level.trim();
            let children = self.tree_children(tree, parent);
            let row = match level.strip_prefix('#') {
                Some(index) => *children.get(index.trim().parse::<usize>().ok()?)?,
                None => *children.iter().find(|row| {
                    self.model
                        .control(tree)
                        .and_then(|control| control.data.get(**row))
                        .map(|text| text == level)
                        .unwrap_or(false)
                })?,
            };
            parent = Some(row);
        }
        parent
    }
}

/// How a `@SW_*` show flag maps onto the model: whether the window is visible
/// and in which state.
///
/// `GUISetState` and `WinSetState` both take these flags. The values are the
/// Win32 `SW_*` ones, which is what AutoIt's `@SW_*` macros expand to — note
/// that they are *not* the `WinGetState` bit flags (`WIN_MINIMIZED` is 16, but
/// `@SW_MINIMIZE` is 6).
fn show_flag(flag: i64) -> (bool, WindowState) {
    match flag {
        // @SW_HIDE
        0 => (false, WindowState::Normal),
        // @SW_SHOWMAXIMIZED
        3 => (true, WindowState::Maximized),
        // @SW_SHOWMINIMIZED / @SW_MINIMIZE / @SW_SHOWMINNOACTIVE / @SW_FORCEMINIMIZE
        2 | 6 | 7 | 11 => (true, WindowState::Minimized),
        // @SW_SHOWNORMAL and the other "just show it" flags (@SW_SHOW,
        // @SW_SHOWNOACTIVATE, @SW_SHOWNA, @SW_SHOWDEFAULT, @SW_RESTORE, ...).
        _ => (true, WindowState::Normal),
    }
}

fn arg_str(args: &[Value], i: usize) -> String {
    args.get(i).map(|v| v.to_autoit_string()).unwrap_or_default()
}

fn arg_int(args: &[Value], i: usize) -> i64 {
    args.get(i).map(|v| v.to_int()).unwrap_or(0)
}

/// The `[winemu]` line for a `MsgBox` the emulation answered.
///
/// The fields are the ones worth reading back: the flags, the title and the
/// text (the text is usually the whole reason the script took a branch). The
/// shape is shared with the native backend, which prints the same line with a
/// `[win32]` prefix.
fn msgbox_notice(flags: i64, title: &str, text: &str, answer: i64) -> String {
    crate::dialog_notice::msgbox_notice("[winemu]", flags, title, text, answer)
}

/// The `[winemu]` line for an `InputBox` the emulation answered or cancelled.
fn inputbox_notice(title: &str, prompt: &str, answer: &str) -> String {
    crate::dialog_notice::inputbox_notice("[winemu]", title, prompt, answer)
}

/// The `[winemu]` line for a `FileOpenDialog`/`FileSaveDialog`/
/// `FileSelectFolder` the emulation answered or cancelled.
fn file_dialog_notice(name: &str, title: &str, answer: &str) -> String {
    crate::dialog_notice::file_dialog_notice("[winemu]", name, title, answer)
}

// Unit tests live in `tests/unit/` so this file reads as implementation;
// `#[path]` pulls the file back in as a module, so they can reach private state.
#[cfg(test)]
#[path = "../../../tests/unit/winemu_gui.rs"]
mod tests;

/// AutoIt's `^!+` hotkey notation -> a packed HOTKEY word.
fn parse_hotkey(s: &str) -> u16 {
    let mut modifiers: u16 = 0;
    let mut vk: u16 = 0;
    for c in s.chars() {
        match c {
            '^' => modifiers |= 2,
            '!' => modifiers |= 4,
            '+' => modifiers |= 1,
            c => {
                let up = c.to_ascii_uppercase();
                if up.is_ascii_alphanumeric() {
                    vk = up as u16;
                }
            }
        }
    }
    (modifiers << 8) | vk
}

#[cfg(all(test, feature = "gui-egui-offscreen"))]
mod egui_render_tests {
    use super::*;
    use autoitv3_runtime::host::HostContext;
    use autoitv3_runtime::profile::ExecutionProfile;
    use std::collections::HashMap;

    /// A minimal `HostContext` so `GuiState::call` can be driven directly.
    struct Ctx {
        profile: ExecutionProfile,
        error: i64,
        extended: i64,
        globals: HashMap<String, Value>,
    }

    impl HostContext for Ctx {
        fn get_global(&self, name: &str) -> Option<Value> {
            self.globals.get(name).cloned()
        }
        fn set_global(&mut self, name: &str, value: Value) {
            self.globals.insert(name.to_string(), value);
        }
        fn error(&self) -> i64 {
            self.error
        }
        fn set_error(&mut self, error: i64, extended: i64) {
            self.error = error;
            self.extended = extended;
        }
        fn profile(&self) -> &ExecutionProfile {
            &self.profile
        }
    }

    #[test]
    fn the_egui_backend_renders_a_script_built_window() {
        let mut state = GuiState::new();
        state.set_backend(Box::new(
            autoitv3_gui_egui::EguiBackend::new().with_size(200, 120),
        ));
        let mut ctx = Ctx {
            profile: ExecutionProfile::faithful(),
            error: 0,
            extended: 0,
            globals: HashMap::new(),
        };
        state
            .call(
                "guicreate",
                &[Value::str("T"), Value::Int(100), Value::Int(60)],
                &mut ctx,
            )
            .unwrap();
        state
            .call(
                "guictrlcreatelabel",
                &[Value::str("hello"), Value::Int(0), Value::Int(0)],
                &mut ctx,
            )
            .unwrap();
        state
            .call("guisetstate", &[Value::Int(5)], &mut ctx)
            .unwrap();

        let image = state.snapshot().expect("egui snapshot");
        assert_eq!((image.width, image.height), (200, 120));
        let opaque = image.rgba.chunks_exact(4).filter(|p| p[3] > 0).count();
        assert!(opaque > 50, "expected a rendered window, got {opaque} opaque px");
    }
}
