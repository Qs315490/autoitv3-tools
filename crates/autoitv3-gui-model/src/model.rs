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
///
/// The variants mirror the `$GUI_GR_*` types the official constants define
/// (`GUIConstantsEx.au3`): a line, a bezier with two control points, a straight
/// move, a rectangle, an ellipse, a pie wedge, a dot and a text. The colours and
/// the pen width are commands of their own because they apply to whatever is
/// drawn after them.
#[derive(Debug, Clone, PartialEq)]
pub enum DrawCmd {
    /// `$GUI_GR_LINE`: from the pen position to `(x2, y2)`.
    Line { x1: i32, y1: i32, x2: i32, y2: i32 },
    /// `$GUI_GR_BEZIER`: from `(x1, y1)` to `(x4, y4)` with two control points.
    Bezier {
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        x3: i32,
        y3: i32,
        x4: i32,
        y4: i32,
    },
    /// `$GUI_GR_RECT`.
    Rect { x: i32, y: i32, w: i32, h: i32 },
    /// `$GUI_GR_ELLIPSE`.
    Ellipse { x: i32, y: i32, w: i32, h: i32 },
    /// `$GUI_GR_PIE`: `(x, y)` is the centre, `r` the radius, and the angles are
    /// in degrees.
    Pie {
        x: i32,
        y: i32,
        r: i32,
        start: i32,
        sweep: i32,
    },
    /// `$GUI_GR_DOT`/`$GUI_GR_PIXEL`.
    Dot { x: i32, y: i32 },
    Text { x: i32, y: i32, text: String },
    /// `$GUI_GR_COLOR`'s first argument.
    SetColor(i64),
    /// `$GUI_GR_COLOR`'s second argument, or `$GUI_GR_NOBKCOLOR` for "no fill".
    SetBkColor(i64),
    /// `$GUI_GR_PENSIZE`.
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
    /// The `$GUI_*` state bits, plus [`GUI_PAGE_HIDDEN`].
    pub state: i64,
    /// The control this one belongs to.
    ///
    /// For a part (see [`ControlKind::is_part`]) that is the control it is a row,
    /// node or page of — a `TreeViewItem`'s parent is its `TreeView` or the item
    /// it hangs under. For a control on a tab page it is the `TabItem` whose page
    /// the control appears on, which is how a page is switched without moving
    /// anything.
    pub parent: Option<i64>,
    /// Where this part sits inside its owner: the row of a
    /// `ListView`/`TreeView` item, or the page number of a `TabItem`. Rows are
    /// re-numbered when an earlier one is deleted, so this stays the index into
    /// the owner's [`data`](Self::data).
    pub row: Option<usize>,
    /// Items for List/Combo/ListView/TreeView, and page titles for a Tab.
    pub data: Vec<String>,
    /// Which item is selected in a list-like control, when the backend knows.
    pub selection: Option<usize>,
    pub tip: String,
    /// The tooltip's title, empty when the script gave none. The icon needs a
    /// title to sit next to, so it is ignored on its own.
    pub tip_title: String,
    /// `$TIP_NOICON`/`$TIP_INFOICON`/`$TIP_WARNINGICON`/`$TIP_ERRORICON`.
    pub tip_icon: i64,
    /// `$TIP_BALLOON`/`$TIP_CENTER`, OR-ed together.
    pub tip_options: i64,
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

/// `$GUI_SHOW`.
pub const GUI_SHOW: i64 = 0x10;
/// `$GUI_HIDE`.
pub const GUI_HIDE: i64 = 0x20;
/// `$GUI_ENABLE`.
pub const GUI_ENABLE: i64 = 0x40;
/// `$GUI_DISABLE`.
pub const GUI_DISABLE: i64 = 0x80;
/// `$GUI_CHECKED`.
pub const GUI_CHECKED: i64 = 0x01;

/// `$GUI_BKCOLOR_TRANSPARENT`: the control keeps the window's own colour.
pub const GUI_BKCOLOR_TRANSPARENT: i64 = -2;
/// `$GUI_BKCOLOR_LV_ALTERNATE`: a ListView paints its rows in two colours.
pub const GUI_BKCOLOR_LV_ALTERNATE: i64 = 0x8000_0000;
/// `$TIP_BALLOON` from `GUICtrlSetTip`'s options word.
pub const TIP_BALLOON: i64 = 1;
/// `$TIP_CENTER` from `GUICtrlSetTip`'s options word.
pub const TIP_CENTER: i64 = 2;

/// Not an AutoIt constant: "this control is on a tab page that is not the
/// selected one".
///
/// AutoIt shows the controls of the selected page and hides the rest, but a
/// script can also hide a control on the page that *is* selected, so the two
/// reasons have to stay apart. Every backend reads one state word, so this is
/// where the page's own hiding goes; [`Control::is_visible`] treats it like
/// `$GUI_HIDE`.
pub const GUI_PAGE_HIDDEN: i64 = 1 << 30;

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
            // What the official interpreter answers for a control nobody has
            // touched: `$GUI_SHOW | $GUI_ENABLE`.
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
        }
    }

    pub fn is_visible(&self) -> bool {
        self.state & (GUI_HIDE | GUI_PAGE_HIDDEN) == 0
    }

    /// The `$GUI_*` word a script sees.
    ///
    /// The page bit is dropped rather than reported: the official interpreter
    /// answers `0x50` (`$GUI_SHOW | $GUI_ENABLE`) for a control on a tab page
    /// that is *not* selected, so `GUICtrlGetState` cannot tell the pages apart.
    /// Hiding the control stays the renderer's business.
    pub fn public_state(&self) -> i64 {
        self.state & !GUI_PAGE_HIDDEN
    }
    pub fn is_enabled(&self) -> bool {
        self.state & GUI_DISABLE == 0
    }
    pub fn is_checked(&self) -> bool {
        self.state & GUI_CHECKED != 0
    }

    /// The solid colour to paint behind this control, `None` when it has none.
    ///
    /// `$GUI_BKCOLOR_TRANSPARENT` asks for the window's own colour — the
    /// official interpreter hands that straight to the control, and the useful
    /// equivalent here is "paint nothing" — and a ListView's
    /// `$GUI_BKCOLOR_LV_ALTERNATE` travels *with* the colour rather than
    /// instead of it, so it is masked off before the value reaches a renderer.
    pub fn background(&self) -> Option<i64> {
        match self.bk_color {
            None | Some(GUI_BKCOLOR_TRANSPARENT) => None,
            Some(color) => Some(color & !GUI_BKCOLOR_LV_ALTERNATE),
        }
    }

    /// Whether this control is the ListView half of `$GUI_BKCOLOR_LV_ALTERNATE`.
    ///
    /// [`background`](Self::background) is then the colour of the *odd* rows;
    /// an even row takes its colour from the row's own item control, falling
    /// back to the ListView's colour when that item has none.
    pub fn alternating_rows(&self) -> bool {
        self.bk_color
            .is_some_and(|color| color & GUI_BKCOLOR_LV_ALTERNATE != 0)
    }

    /// The title `GUICtrlSetTip` gave this control's tooltip, if any.
    ///
    /// A balloon tip is drawn by the same tooltip window as a plain one, so the
    /// title and the icon ride along on the control instead of in the backend.
    pub fn tip_is_balloon(&self) -> bool {
        self.tip_options & TIP_BALLOON != 0
    }
}

impl ControlKind {
    /// Whether this kind is a *part* of another control rather than a window of
    /// its own.
    ///
    /// AutoIt's `ListViewItem`, `TreeViewItem`, `TabItem` and `MenuItem` are
    /// rows, nodes, pages and menu entries of the control that owns them: they
    /// have an identifier a script can use, but no window, and a renderer draws
    /// them through their owner.
    pub fn is_part(self) -> bool {
        matches!(
            self,
            Self::ListViewItem | Self::TreeViewItem | Self::TabItem | Self::MenuItem
        )
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
    /// The control with the input focus, when a script or a click set one.
    pub focus: Option<i64>,
    /// `WinSetOnTop`/`$GUI_ONTOP`: the window stays above the others.
    pub topmost: bool,
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
///
/// AutoIt's progress window has two labels: `ProgressOn` is
/// `(title, maintext, subtext, ...)` and `ProgressSet` is
/// `(percent, subtext, maintext)` — the two orders are indeed different, which is
/// why each label has its own field here.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub on: bool,
    pub text: String,
    pub sub: String,
    pub percent: i64,
}

/// Everything the headless GUI knows.
#[derive(Debug, Default)]
pub struct GuiModel {
    pub windows: Vec<Option<Window>>,
    pub controls: Vec<Option<Control>>,
    pub current_window: Option<i64>,
    /// The control the `-1` control identifier stands for: AutoIt lets every
    /// `GUICtrl*` function take `-1` for "the last created control".
    pub last_control: Option<i64>,
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

    /// Resolve a control identifier the way AutoIt does: `-1` means the last
    /// created control.
    pub fn control_id(&self, id: i64) -> Option<i64> {
        match id {
            -1 => self.last_control.filter(|id| self.control(*id).is_some()),
            id if id >= 1 => self.control(id).map(|control| control.id),
            _ => None,
        }
    }

    /// The control that owns the flat item list `id` is a part of.
    ///
    /// A `TreeViewItem` knows its structural parent — the item it hangs under,
    /// or the `TreeView` itself — but every item of one tree shares that tree's
    /// list, so the owner is the `TreeView` at the top of the chain. Everything
    /// else owns its own list.
    pub fn list_owner(&self, id: i64) -> Option<i64> {
        let control = self.control(id)?;
        if control.kind != ControlKind::TreeViewItem {
            return Some(id);
        }
        self.list_owner(control.parent?)
    }

    /// The controls that hang directly off `parent`.
    pub fn children_of(&self, parent: i64) -> Vec<i64> {
        self.controls
            .iter()
            .flatten()
            .filter(|control| control.parent == Some(parent))
            .map(|control| control.id)
            .collect()
    }

    /// The list a part belongs to: the owner of the control it hangs off.
    ///
    /// A `ListViewItem`'s parent *is* its list, but a tree node hangs off
    /// another node, so this walks to the `TreeView` at the top.
    pub fn part_owner(&self, id: i64) -> Option<i64> {
        let parent = self.control(id)?.parent?;
        self.list_owner(parent)
    }

    /// The part that stands for row `row` of `owner`, if any.
    pub fn part_at(&self, owner: i64, row: usize) -> Option<i64> {
        self.controls
            .iter()
            .flatten()
            .find(|control| control.row == Some(row) && self.part_owner(control.id) == Some(owner))
            .map(|control| control.id)
    }

    /// Add a control that is a part of another one and return its id.
    ///
    /// The part's text becomes a row of its owner's item list, which is the list
    /// `GUICtrlRead` and the item-count messages answer from: AutoIt's item
    /// controls *are* the rows of their owner, not windows beside it.
    pub fn add_part_control(&mut self, mut control: Control) -> i64 {
        if let Some(owner) = control.parent.and_then(|parent| self.list_owner(parent)) {
            if let Some(owner_control) = self.control_mut(owner) {
                owner_control.data.push(control.text.clone());
                control.row = Some(owner_control.data.len() - 1);
            }
        }
        self.add_control(control)
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
        self.last_control = Some(id);
        id
    }

    /// Remove a control, and with it the row or page it stood for.
    ///
    /// Deleting a `TreeViewItem` takes its children with it, the way deleting a
    /// node takes its subtree; the rows after a deleted one are re-numbered so
    /// `row` keeps pointing at the same item.
    pub fn remove_control(&mut self, id: i64) -> bool {
        let Some(index) = self.control_index(id) else {
            return false;
        };
        if self.controls[index].is_none() {
            return false;
        }
        // Everything that hangs off this control goes with it — deleting a tree
        // node deletes its subtree — and the rows each of them stood for are
        // read *before* any control disappears, because a row's owner is found
        // through the control that is about to be removed.
        let mut doomed = self.descendants_of(id);
        doomed.push(id);
        let mut rows: Vec<(i64, i64, usize)> = doomed
            .iter()
            .filter_map(|part| {
                let row = self.control(*part)?.row?;
                Some((*part, self.part_owner(*part)?, row))
            })
            .collect();
        rows.sort_by_key(|(_, _, row)| std::cmp::Reverse(*row));
        for doomed_id in &doomed {
            let Some(index) = self.control_index(*doomed_id) else {
                continue;
            };
            let Some(control) = self.controls[index].take() else {
                continue;
            };
            if let Some(window) = self.window_mut(control.window) {
                window.controls.retain(|c| *c != *doomed_id);
            }
            if self.last_control == Some(*doomed_id) {
                self.last_control = None;
            }
        }
        for (part, owner, row) in rows {
            if let Some(owner_control) = self.control_mut(owner) {
                if row < owner_control.data.len() {
                    owner_control.data.remove(row);
                }
            }
            let _ = part;
        }
        self.renumber_rows();
        true
    }

    /// Every control that hangs off `id`, directly or through other parts.
    fn descendants_of(&self, id: i64) -> Vec<i64> {
        let mut found = Vec::new();
        let mut frontier = vec![id];
        while let Some(parent) = frontier.pop() {
            for child in self.children_of(parent) {
                found.push(child);
                frontier.push(child);
            }
        }
        found
    }

    /// Make `row` the index into the owner's list again after a deletion.
    fn renumber_rows(&mut self) {
        let owners: Vec<i64> = self
            .controls
            .iter()
            .flatten()
            .filter(|control| {
                matches!(
                    control.kind,
                    ControlKind::ListView | ControlKind::TreeView | ControlKind::Tab
                )
            })
            .map(|control| control.id)
            .collect();
        for owner in owners {
            let mut parts: Vec<(usize, i64)> = self
                .children_of(owner)
                .into_iter()
                .chain(
                    // A tree's items hang off each other, so the parts of the
                    // whole tree are gathered through `list_owner` instead.
                    self.controls
                        .iter()
                        .flatten()
                        .map(|control| control.id)
                        .filter(|id| self.part_owner(*id) == Some(owner)),
                )
                .filter_map(|id| {
                    let control = self.control(id)?;
                    Some((control.row?, id))
                })
                .collect();
            parts.sort_unstable();
            parts.dedup();
            for (new_row, (_, id)) in parts.into_iter().enumerate() {
                if let Some(control) = self.control_mut(id) {
                    control.row = Some(new_row);
                }
            }
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
            focus: None,
            topmost: false,
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
