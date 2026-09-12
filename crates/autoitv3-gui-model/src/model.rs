//! The in-memory widget model AutoIt's GUI functions operate on.
//!
//! Everything here is plain state — no windowing, no handles from an OS. The
//! [`crate::GuiBackend`] trait is what a renderer implements; this module is the
//! authoritative model both the headless and the rendered backends agree on.

/// What kind of control a `GUICtrlCreate*` produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Label,
    Button,
    Checkbox,
    Radio,
    Group,
    Input,
    Edit,
    List,
    Combo,
    ListView,
    ListViewItem,
    TreeView,
    TreeViewItem,
    Tab,
    TabItem,
    Menu,
    MenuItem,
    ContextMenu,
    Pic,
    Icon,
    Graphic,
    Progress,
    Slider,
    Updown,
    Date,
    MonthCal,
    Dummy,
    Avi,
    Obj,
}

impl ControlKind {
    /// Map a `guictrlcreatexxx` dispatch key to its kind.
    pub fn from_create(key: &str) -> Option<Self> {
        Some(match key {
            "guictrlcreatelabel" => Self::Label,
            "guictrlcreatebutton" => Self::Button,
            "guictrlcreatecheckbox" => Self::Checkbox,
            "guictrlcreateradio" => Self::Radio,
            "guictrlcreategroup" => Self::Group,
            "guictrlcreateinput" => Self::Input,
            "guictrlcreateedit" => Self::Edit,
            "guictrlcreatelist" => Self::List,
            "guictrlcreatecombo" => Self::Combo,
            "guictrlcreatelistview" => Self::ListView,
            "guictrlcreatelistviewitem" => Self::ListViewItem,
            "guictrlcreatetreeview" => Self::TreeView,
            "guictrlcreatetreeviewitem" => Self::TreeViewItem,
            "guictrlcreatetab" => Self::Tab,
            "guictrlcreatetabitem" => Self::TabItem,
            "guictrlcreatemenu" => Self::Menu,
            "guictrlcreatemenuitem" => Self::MenuItem,
            "guictrlcreatecontextmenu" => Self::ContextMenu,
            "guictrlcreatepic" => Self::Pic,
            "guictrlcreateicon" => Self::Icon,
            "guictrlcreategraphic" => Self::Graphic,
            "guictrlcreateprogress" => Self::Progress,
            "guictrlcreateslider" => Self::Slider,
            "guictrlcreateupdown" => Self::Updown,
            "guictrlcreatedate" => Self::Date,
            "guictrlcreatemonthcal" => Self::MonthCal,
            "guictrlcreatedummy" => Self::Dummy,
            "guictrlcreateavi" => Self::Avi,
            "guictrlcreateobj" => Self::Obj,
            _ => return None,
        })
    }
}

/// A drawing command recorded by `GUICtrlSetGraphic`.
#[derive(Debug, Clone, PartialEq)]
pub enum DrawCmd {
    Line { x1: i32, y1: i32, x2: i32, y2: i32 },
    Rect { x: i32, y: i32, w: i32, h: i32 },
    Ellipse { x: i32, y: i32, w: i32, h: i32 },
    Text { x: i32, y: i32, text: String },
    SetColor(i64),
    SetBkColor(i64),
    SetWidth(i32),
    SetStyle(i64),
    Clear,
}

/// A control font, as `GUISetFont`/`GUICtrlSetFont` record it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Font {
    pub name: String,
    pub size: i32,
    pub weight: i32,
    pub attribute: i32,
}

/// One control in a window.
#[derive(Debug, Clone)]
pub struct Control {
    pub id: i64,
    pub window: i64,
    pub kind: ControlKind,
    pub text: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub style: i64,
    pub exstyle: i64,
    /// The `$GUI_*` state bits.
    pub state: i64,
    /// Items for List/Combo/ListView/TreeView.
    pub data: Vec<String>,
    /// Which item is selected in a list-like control, when the backend knows.
    pub selection: Option<usize>,
    pub tip: String,
    pub on_event: Option<String>,
    pub bk_color: Option<i64>,
    pub color: Option<i64>,
    pub font: Option<Font>,
    pub cursor: Option<i64>,
    pub image: Option<String>,
    pub limit: Option<(i64, i64)>,
    pub resizing: i64,
    pub draw: Vec<DrawCmd>,
}

/// The display mode the emulation assumes when no backend has reported a
/// desktop: `(width, height)` in pixels.
///
/// A live window's viewport and an offscreen renderer's canvas are real
/// desktops and take precedence; this is what a headless run answers for
/// `@DesktopWidth`/`@DesktopHeight` and what the live window asks its native
/// viewport to be, so the size is the same before and after the first frame.
pub const DEFAULT_DESKTOP_SIZE: (i32, i32) = (1024, 768);

/// `$GUI_HIDE`.
pub const GUI_HIDE: i64 = 0x20;
/// `$GUI_DISABLE`.
pub const GUI_DISABLE: i64 = 0x80;
/// `$GUI_CHECKED`.
pub const GUI_CHECKED: i64 = 0x01;

impl Control {
    /// A control with default geometry and state.
    pub fn new(id: i64, window: i64, kind: ControlKind) -> Self {
        Self {
            id,
            window,
            kind,
            text: String::new(),
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            style: 0,
            exstyle: 0,
            state: 0,
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
        }
    }

    pub fn is_visible(&self) -> bool {
        self.state & GUI_HIDE == 0
    }
    pub fn is_enabled(&self) -> bool {
        self.state & GUI_DISABLE == 0
    }
    pub fn is_checked(&self) -> bool {
        self.state & GUI_CHECKED != 0
    }
}

/// How a window is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowState {
    Normal,
    Minimized,
    Maximized,
}

/// One top-level window.
#[derive(Debug, Clone)]
pub struct Window {
    pub handle: i64,
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub style: i64,
    pub exstyle: i64,
    pub visible: bool,
    pub enabled: bool,
    pub active: bool,
    pub state: WindowState,
    /// The rectangle to go back to when a maximised window is restored, the way
    /// Windows keeps a window's normal placement. `None` unless maximised.
    pub restore: Option<(i32, i32, i32, i32)>,
    pub bk_color: Option<i64>,
    pub font: Option<Font>,
    pub cursor: Option<i64>,
    pub icon: Option<String>,
    pub resizing: i64,
    pub on_event: Option<String>,
    pub controls: Vec<i64>,
}

/// A tray item created with `TrayCreateItem`/`TrayCreateMenu`.
#[derive(Debug, Clone)]
pub struct TrayItem {
    pub id: i64,
    pub text: String,
    pub state: i64,
    pub on_event: Option<String>,
    pub menu: bool,
}

/// Feedback windows: splash, progress and tooltip.
#[derive(Debug, Clone, Default)]
pub struct Splash {
    pub text: String,
    pub image: Option<String>,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub visible: bool,
}

/// Progress window state.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub on: bool,
    pub text: String,
    pub percent: i64,
}

/// Everything the headless GUI knows.
#[derive(Debug, Default)]
pub struct GuiModel {
    pub windows: Vec<Option<Window>>,
    pub controls: Vec<Option<Control>>,
    pub current_window: Option<i64>,
    next_window: i64,
    next_control: i64,
    next_menu: i64,
    next_tray: i64,
    pub tray: Vec<TrayItem>,
    pub splash: Splash,
    pub progress: Progress,
    pub tooltip: String,
    pub tooltip_visible: bool,
    pub mouse: (i32, i32),
    pub hotkeys: Vec<(u16, String)>,
    pub block_input: bool,
    pub notice_handlers: Vec<(u32, String)>,
    pub autoit_win_title: String,
    pub sounds: u32,
    pub cd_open: bool,
    draw_color: i64,
    draw_bk: i64,
    draw_width: i32,
    draw_style: i64,
}

/// The first window handle; control ids start at 1.
const WINDOW_BASE: i64 = 0x0001_0000;

impl GuiModel {
    /// Create an empty model.
    pub fn new() -> Self {
        Self {
            autoit_win_title: "AutoIt v3".to_string(),
            ..Self::default()
        }
    }

    pub fn alloc_window(&mut self) -> i64 {
        let handle = WINDOW_BASE + self.next_window;
        self.next_window += 1;
        self.windows.push(None);
        handle
    }

    pub fn window_index(&self, handle: i64) -> Option<usize> {
        if handle < WINDOW_BASE {
            return None;
        }
        let index = (handle - WINDOW_BASE) as usize;
        (index < self.windows.len()).then_some(index)
    }

    pub fn window(&self, handle: i64) -> Option<&Window> {
        self.window_index(handle)
            .and_then(|i| self.windows[i].as_ref())
    }

    pub fn window_mut(&mut self, handle: i64) -> Option<&mut Window> {
        self.window_index(handle)
            .and_then(|i| self.windows[i].as_mut())
    }

    /// Insert a window and return its handle; it also becomes current.
    pub fn add_window(&mut self, window: Window) -> i64 {
        let handle = window.handle;
        let index = self.window_index(handle).expect("allocated");
        self.windows[index] = Some(window);
        self.current_window = Some(handle);
        handle
    }

    /// Remove a window and all of its controls.
    pub fn remove_window(&mut self, handle: i64) -> bool {
        let Some(index) = self.window_index(handle) else {
            return false;
        };
        let removed = self.windows[index].take();
        if let Some(window) = removed {
            for id in window.controls {
                if let Some(slot) = self.control_index(id) {
                    self.controls[slot] = None;
                }
            }
            if self.current_window == Some(handle) {
                self.current_window = None;
            }
            true
        } else {
            false
        }
    }

    /// The current window, or the first existing one.
    pub fn active_window(&self) -> Option<i64> {
        if let Some(handle) = self.current_window {
            if self.window(handle).is_some() {
                return Some(handle);
            }
        }
        self.windows.iter().flatten().map(|w| w.handle).next()
    }

    /// Resolve an AutoIt window specifier: a handle number, or a title matched
    /// case-insensitively (empty means the current window).
    pub fn resolve_window(&self, spec: &str) -> Option<i64> {
        let spec = spec.trim();
        if spec.is_empty() {
            return self.active_window();
        }
        if let Ok(handle) = spec.parse::<i64>() {
            if self.window(handle).is_some() {
                return Some(handle);
            }
        }
        let needle = spec.to_ascii_lowercase();
        self.windows
            .iter()
            .flatten()
            .find(|w| w.title.to_ascii_lowercase().contains(&needle))
            .map(|w| w.handle)
    }

    pub fn alloc_control_id(&mut self) -> i64 {
        self.next_control += 1;
        self.next_control
    }

    pub fn control_index(&self, id: i64) -> Option<usize> {
        if id < 1 {
            return None;
        }
        let index = (id - 1) as usize;
        (index < self.controls.len()).then_some(index)
    }

    pub fn control(&self, id: i64) -> Option<&Control> {
        self.control_index(id)
            .and_then(|i| self.controls[i].as_ref())
    }

    pub fn control_mut(&mut self, id: i64) -> Option<&mut Control> {
        self.control_index(id)
            .and_then(|i| self.controls[i].as_mut())
    }

    /// Add a control to `window` and return its id.
    pub fn add_control(&mut self, mut control: Control) -> i64 {
        let id = self.alloc_control_id();
        control.id = id;
        if let Some(window) = self.window_mut(control.window) {
            window.controls.push(id);
        }
        // Fill gaps so `control_index` stays a direct lookup.
        while self.controls.len() < (id - 1) as usize {
            self.controls.push(None);
        }
        self.controls.push(Some(control));
        id
    }

    pub fn remove_control(&mut self, id: i64) -> bool {
        let Some(index) = self.control_index(id) else {
            return false;
        };
        let removed = self.controls[index].take();
        match removed {
            Some(control) => {
                if let Some(window) = self.window_mut(control.window) {
                    window.controls.retain(|c| *c != id);
                }
                true
            }
            None => false,
        }
    }

    /// Find a control in `window` by id, or by its text/tip/data (the "text"
    /// argument `Control*` functions take).
    pub fn find_control(&self, window: i64, text: &str) -> Option<i64> {
        let window = self.window(window)?;
        let text = text.trim();
        if let Ok(id) = text.parse::<i64>() {
            if window.controls.contains(&id) {
                return Some(id);
            }
        }
        let needle = text.to_ascii_lowercase();
        window
            .controls
            .iter()
            .filter_map(|id| self.control(*id))
            .find(|c| {
                c.text.to_ascii_lowercase() == needle
                    || c.tip.to_ascii_lowercase() == needle
                    || c.data.iter().any(|d| d.to_ascii_lowercase() == needle)
            })
            .map(|c| c.id)
    }

    pub fn draw(&mut self, id: i64, cmd: DrawCmd) {
        match &cmd {
            DrawCmd::SetColor(c) => self.draw_color = *c,
            DrawCmd::SetBkColor(c) => self.draw_bk = *c,
            DrawCmd::SetWidth(w) => self.draw_width = *w,
            DrawCmd::SetStyle(s) => self.draw_style = *s,
            _ => {}
        }
        if let Some(control) = self.control_mut(id) {
            match cmd {
                DrawCmd::Clear => control.draw.clear(),
                other => control.draw.push(other),
            }
        }
    }

    pub fn alloc_menu_id(&mut self) -> i64 {
        self.next_menu += 1;
        self.next_menu
    }

    pub fn alloc_tray_id(&mut self) -> i64 {
        self.next_tray += 1;
        self.next_tray
    }
}

// ---------------------------------------------------------------------------
// `WinGetState` / `WinSetState` bit constants
// ---------------------------------------------------------------------------

/// `$WIN_STATE_EXISTS`.
pub const WIN_EXISTS: i64 = 1;
/// `$WIN_STATE_VISIBLE`.
pub const WIN_VISIBLE: i64 = 2;
/// `$WIN_STATE_ENABLED`.
pub const WIN_ENABLED: i64 = 4;
/// `$WIN_STATE_ACTIVE`.
pub const WIN_ACTIVE: i64 = 8;
/// `$WIN_STATE_MINIMIZED`.
pub const WIN_MINIMIZED: i64 = 16;
/// `$WIN_STATE_MAXIMIZED`.
pub const WIN_MAXIMIZED: i64 = 32;

impl Window {
    /// A visible, enabled window with default state.
    pub fn new(handle: i64, title: impl Into<String>, width: i32, height: i32) -> Self {
        Self {
            handle,
            title: title.into(),
            x: 0,
            y: 0,
            width,
            height,
            style: 0,
            exstyle: 0,
            visible: true,
            enabled: true,
            active: true,
            state: WindowState::Normal,
            restore: None,
            bk_color: None,
            font: None,
            cursor: None,
            icon: None,
            resizing: 0,
            on_event: None,
            controls: Vec::new(),
        }
    }

    /// The `WinGetState` bitmask.
    pub fn state_bits(&self) -> i64 {
        let mut bits = WIN_EXISTS;
        if self.visible {
            bits |= WIN_VISIBLE;
        }
        if self.enabled {
            bits |= WIN_ENABLED;
        }
        if self.active {
            bits |= WIN_ACTIVE;
        }
        match self.state {
            WindowState::Minimized => bits |= WIN_MINIMIZED,
            WindowState::Maximized => bits |= WIN_MAXIMIZED,
            WindowState::Normal => {}
        }
        bits
    }
}
