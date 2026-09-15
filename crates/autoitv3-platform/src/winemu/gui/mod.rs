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

mod messages;

/// The widget model and backend seam live in the dependency-free
/// `autoitv3-gui-model` crate, so a renderer only has to depend on that.
pub use autoitv3_gui_model as model;
pub use autoitv3_gui_model::{
    Control, ControlKind, DrawCmd, Font, GuiBackend, GuiEvent, GuiImage, GuiModel, GuiUpdate,
    HeadlessBackend, TrayItem, Window, WindowState, GUI_EVENT_CLOSE, GUI_EVENT_DROPPED,
    GUI_EVENT_MAXIMIZE, GUI_EVENT_MINIMIZE, GUI_EVENT_MOUSEMOVE, GUI_EVENT_PRIMARYDOWN,
    GUI_EVENT_PRIMARYUP, GUI_EVENT_RESTORE, GUI_EVENT_RESIZED, GUI_EVENT_SECONDARYDOWN,
    GUI_EVENT_SECONDARYUP,
};

use std::collections::VecDeque;

use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::value::Value;

use autoitv3_gui_model::{GUI_CHECKED, GUI_DISABLE, GUI_HIDE, GUI_PAGE_HIDDEN};

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
        }
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
        let Some(window) = self.model.window_mut(handle) else {
            return false;
        };
        let mut changed = window.visible != visible || window.state != state;
        window.visible = visible;
        match state {
            WindowState::Maximized => {
                if window.state != WindowState::Maximized {
                    window.restore = Some((window.x, window.y, window.width, window.height));
                }
                changed |= (window.width, window.height) != (desktop_width, desktop_height);
                window.x = 0;
                window.y = 0;
                window.width = desktop_width;
                window.height = desktop_height;
            }
            WindowState::Normal => {
                if let Some((x, y, width, height)) = window.restore.take() {
                    changed = true;
                    window.x = x;
                    window.y = y;
                    window.width = width;
                    window.height = height;
                }
            }
            WindowState::Minimized => {}
        }
        window.state = state;
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
                    // through WinGetPos/WinGetClientSize and $GUI_EVENT_RESIZED.
                    let changed = match self.model.window_mut(handle) {
                        Some(window) => {
                            let changed = window.width != width || window.height != height;
                            window.width = width.max(1);
                            window.height = height.max(1);
                            changed
                        }
                        None => false,
                    };
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
    fn poll_message(&mut self) -> i64 {
        for event in self.backend.poll() {
            self.events.push_back(event);
        }
        self.polls += 1;
        if let Some(event) = self.events.pop_front() {
            return event.message();
        }
        if let Some(limit) = self.auto_close {
            if self.polls >= limit {
                self.auto_close = None;
                return GUI_EVENT_CLOSE;
            }
        }
        0
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
                    on_event: None,
                    controls: Vec::new(),
                };
                self.model.add_window(window);
                self.notify_window(handle);
                ctx.set_error(0, 0);
                Value::Int(handle)
            }
            "guidelete" => {
                let handle = self.window_arg(args, 0);
                match handle {
                    Some(handle) => {
                        let ok = self.model.remove_window(handle);
                        self.backend.on_window_removed(handle);
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
                Value::Int(previous)
            }
            "guigetmsg" => {
                ctx.set_error(0, 0);
                Value::Int(self.poll_message())
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
                    let handler = arg_str(args, 1);
                    if let Some(window) = self.model.window_mut(handle) {
                        window.on_event = Some(handler);
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
                    if state & 0x10 != 0 {
                        control.state &= !GUI_HIDE;
                    }
                    if state & GUI_HIDE != 0 {
                        control.state |= GUI_HIDE;
                    }
                    if state & 0x40 != 0 {
                        control.state &= !GUI_DISABLE;
                    }
                    if state & GUI_DISABLE != 0 {
                        control.state |= GUI_DISABLE;
                    }
                    // `$GUI_CHECKED` (1), `$GUI_INDETERMINATE` (2) and
                    // `$GUI_UNCHECKED` (4) describe one three-way state, so
                    // asking for one of them clears the other two.
                    if state & (GUI_CHECKED | 0x02 | 0x04) != 0 {
                        control.state &= !(GUI_CHECKED | 0x02);
                        if state & GUI_CHECKED != 0 {
                            control.state |= GUI_CHECKED;
                        }
                        if state & 0x02 != 0 {
                            control.state |= 0x02;
                        }
                    }
                    if state & 0x08 != 0 {
                        control.state |= 0x08;
                    }
                    if state & 0x1000 != 0 {
                        control.state &= !0x08;
                    }
                    // A `TreeViewItem` is painted bold while `$GUI_DEFBUTTON`
                    // is set, and the documented way to turn that off again is
                    // to set the state to 0.
                    if state & 0x200 != 0 {
                        control.state |= 0x200;
                    } else if state == 0 {
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
                    control.bk_color = Some(color);
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
                if let Some(control) = self.model.control_mut(id) {
                    control.tip = tip;
                }
                Value::Int(1)
            }
            "guictrlsetgraphic" => self.set_graphic(args, ctx),
            "guictrlsendmsg" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                let msg = arg_int(args, 1) as u32;
                let wparam = arg_int(args, 2);
                match self.model.control(id) {
                    Some(control) => {
                        let (result, known) = messages::send(control, msg, wparam);
                        ctx.set_error(if known { 0 } else { 1 }, 0);
                        Value::Int(result)
                    }
                    None => {
                        ctx.set_error(1, 0);
                        Value::Int(0)
                    }
                }
            }
            "guictrlrecvmsg" => {
                let Some(id) = self.resolve_control(args, 0) else {
                    return Some(Self::no_such_control(ctx));
                };
                let msg = arg_int(args, 1) as u32;
                let result = match self.model.control(id) {
                    Some(control) => messages::send(control, msg, 0).0,
                    None => {
                        ctx.set_error(1, 0);
                        return Some(Value::array(vec![Value::Int(0)]));
                    }
                };
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
                ctx.set_error(0, 0);
                Value::Int(self.window_arg(args, 0).unwrap_or(0))
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
                        ctx.set_error(0, 0);
                        Value::array(vec![
                            Value::Int(i64::from(w.x)),
                            Value::Int(i64::from(w.y)),
                            Value::Int(i64::from(w.width)),
                            Value::Int(i64::from(w.height)),
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
                        if let Some(width) = coordinate(4) {
                            window.width = width as i32;
                        }
                        if let Some(height) = coordinate(5) {
                            window.height = height as i32;
                        }
                    }
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
                for window in self.model.windows.iter().flatten() {
                    if window.visible {
                        items.push(Value::array(vec![
                            Value::Str(window.title.clone()),
                            Value::Int(window.handle),
                        ]));
                    }
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
                let id = self.control_arg(args).map(|c| c.id);
                if let Some(id) = id {
                    let text = arg_str(args, 2);
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
            "controlcommand" => {
                let command = arg_str(args, 2).to_ascii_lowercase();
                let control = self.control_arg(args);
                match command.as_str() {
                    "isvisible" => Value::Str(
                        if control.map(|c| c.is_visible()).unwrap_or(false) {
                            "1"
                        } else {
                            "0"
                        }
                        .to_string(),
                    ),
                    "isenabled" => Value::Str(
                        if control.map(|c| c.is_enabled()).unwrap_or(false) {
                            "1"
                        } else {
                            "0"
                        }
                        .to_string(),
                    ),
                    _ => Value::str(""),
                }
            }
            "controldisable" | "controlenable" | "controlfocus" | "controlhide"
            | "controlshow" | "controlmove" | "controlsend" | "controllistview"
            | "controltreeview" => {
                let id = self.control_arg(args).map(|c| c.id);
                if let Some(id) = id {
                    if let Some(control) = self.model.control_mut(id) {
                        match key.as_str() {
                            "controldisable" => control.state |= GUI_DISABLE,
                            "controlenable" => control.state &= !GUI_DISABLE,
                            "controlhide" => control.state |= GUI_HIDE,
                            "controlshow" => control.state &= !GUI_HIDE,
                            "controlmove" => {
                                control.x = arg_int(args, 2) as i32;
                                control.y = arg_int(args, 3) as i32;
                                control.width = arg_int(args, 4) as i32;
                                control.height = arg_int(args, 5) as i32;
                            }
                            _ => {}
                        }
                    }
                    self.notify_control(id);
                }
                Value::Int(1)
            }
            "controlgetfocus" => Value::str(""),

            // ---------------- dialogs ----------------
            "msgbox" => {
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
                Value::Int(1)
            }
            "splashoff" => {
                self.model.splash.visible = false;
                Value::Int(1)
            }
            "progresson" => {
                self.model.progress = model::Progress {
                    on: true,
                    text: arg_str(args, 1),
                    percent: 0,
                };
                Value::Int(1)
            }
            "progressset" => {
                self.model.progress.percent = arg_int(args, 0);
                if args.len() > 1 {
                    self.model.progress.text = arg_str(args, 1);
                }
                Value::Int(1)
            }
            "progressoff" => {
                self.model.progress.on = false;
                Value::Int(1)
            }
            "tooltip" => {
                self.model.tooltip = arg_str(args, 0);
                self.model.tooltip_visible = !self.model.tooltip.is_empty();
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
                Value::Int(self.poll_message())
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
            state: 0,
            parent: None,
            row: None,
            data: Vec::new(),
            selection: None,
            tip: String::new(),
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
            state: 0,
            parent,
            row: None,
            data: Vec::new(),
            selection: None,
            tip: String::new(),
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
                if advanced {
                    Value::Int(if control.is_checked() { 1 } else { 4 })
                } else {
                    Value::Str(self.item_text(id, control))
                }
            }
            ControlKind::TreeViewItem => {
                if advanced {
                    Value::Str(self.item_text(id, control))
                } else {
                    Value::Int(control.public_state())
                }
            }
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
    fn item_text(&self, id: i64, control: &Control) -> String {
        let Some(row) = control.row else {
            return control.text.clone();
        };
        self.model
            .part_owner(id)
            .and_then(|owner| self.model.control(owner))
            .and_then(|owner| owner.data.get(row))
            .cloned()
            .unwrap_or_else(|| control.text.clone())
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
            // An item's text *is* its row, and `data` is that row's columns:
            // the separator positions name the columns, a cell that is present
            // but empty is erased, and the columns after the last separator keep
            // what they had. A `ListView` control given data this way appends a
            // row — the documented way to add rows is
            // `GUICtrlCreateListViewItem`, and a test probe against the official
            // interpreter is what settles what this form does there.
            ControlKind::ListView if !data.is_empty() => {
                if let Some(control) = self.model.control_mut(id) {
                    control.data.push(data);
                }
            }
            ControlKind::ListView => {
                if let Some(control) = self.model.control_mut(id) {
                    control.data.clear();
                    control.selection = None;
                }
            }
            ControlKind::ListViewItem => {
                let cells: Vec<&str> = data.split('|').collect();
                let owner = self.model.part_owner(id);
                let row = self.model.control(id).and_then(|control| control.row);
                if let (Some(owner), Some(row)) = (owner, row) {
                    let updated = match self.model.control(owner).and_then(|o| o.data.get(row)) {
                        Some(existing) => {
                            let mut cells_now: Vec<String> =
                                existing.split('|').map(|s| s.to_string()).collect();
                            for (index, cell) in cells.iter().enumerate() {
                                if index >= cells_now.len() {
                                    cells_now.push((*cell).to_string());
                                } else if !cell.is_empty() {
                                    cells_now[index] = (*cell).to_string();
                                } else {
                                    cells_now[index] = String::new();
                                }
                            }
                            cells_now.join("|")
                        }
                        None => data.clone(),
                    };
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
    fn set_graphic(&mut self, args: &[Value], ctx: &mut dyn HostContext) -> Value {
        let Some(id) = self.resolve_control(args, 0) else {
            return Self::no_such_control(ctx);
        };
        let kind = arg_int(args, 1);
        let (pen_x, pen_y) = self.draw_pen;
        let cmd = match kind {
            0 => {
                // $GUI_GR_MOVE
                self.draw_pen = (arg_int(args, 2) as i32, arg_int(args, 3) as i32);
                None
            }
            1 => Some(DrawCmd::SetColor(arg_int(args, 2))),
            2 => {
                let end = (arg_int(args, 2) as i32, arg_int(args, 3) as i32);
                self.draw_pen = end;
                Some(DrawCmd::Line {
                    x1: pen_x,
                    y1: pen_y,
                    x2: end.0,
                    y2: end.1,
                })
            }
            6 => Some(DrawCmd::Rect {
                x: arg_int(args, 2) as i32,
                y: arg_int(args, 3) as i32,
                w: arg_int(args, 4) as i32,
                h: arg_int(args, 5) as i32,
            }),
            7 | 8 => Some(DrawCmd::Ellipse {
                x: arg_int(args, 2) as i32,
                y: arg_int(args, 3) as i32,
                w: arg_int(args, 4) as i32,
                h: arg_int(args, 5) as i32,
            }),
            9 | 10 => Some(DrawCmd::Rect {
                x: arg_int(args, 2) as i32,
                y: arg_int(args, 3) as i32,
                w: 1,
                h: 1,
            }),
            13 => Some(DrawCmd::Clear),
            _ => None,
        };
        if let Some(cmd) = cmd {
            self.model.draw(id, cmd);
        }
        // A text command follows the same entry point in AutoIt via p1..p4;
        // strings are not used, so nothing more to record here.
        self.notify_control(id);
        ctx.set_error(0, 0);
        Value::Int(1)
    }

    /// Resolve the window argument at `index`, defaulting to the current one.
    fn window_arg(&self, args: &[Value], index: usize) -> Option<i64> {
        let spec = args
            .get(index)
            .map(|v| v.to_autoit_string())
            .unwrap_or_default();
        self.model.resolve_window(&spec)
    }

    /// Resolve the `(window, control text)` pair `Control*` functions take.
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

    fn control_arg(&self, args: &[Value]) -> Option<&Control> {
        let window = self.window_arg(args, 0)?;
        let text = args.get(1).map(|v| v.to_autoit_string()).unwrap_or_default();
        let id = self.model.find_control(window, &text)?;
        self.model.control(id)
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
/// text (the text is usually the whole reason the script took a branch).
fn msgbox_notice(flags: i64, title: &str, text: &str, answer: i64) -> String {
    format!("[winemu] MsgBox({flags}, {title:?}, {text:?}) -> {answer}")
}

/// The `[winemu]` line for an `InputBox` the emulation answered or cancelled.
fn inputbox_notice(title: &str, prompt: &str, answer: &str) -> String {
    format!("[winemu] InputBox({title:?}, {prompt:?}) -> {answer}")
}

/// The `[winemu]` line for a `FileOpenDialog`/`FileSaveDialog`/
/// `FileSelectFolder` the emulation answered or cancelled.
fn file_dialog_notice(name: &str, title: &str, answer: &str) -> String {
    format!("[winemu] {name}({title:?}) -> {answer}")
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

#[cfg(all(test, feature = "gui-egui"))]
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
