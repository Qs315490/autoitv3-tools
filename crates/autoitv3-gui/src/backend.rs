//! The GUI backend seam: what a renderer has to implement.
//!
//! The AutoIt semantics (handle numbering, `@error`, `$GUI_EVENT_*`) live in
//! [`super`]; a backend only mirrors the model and supplies events. The default
//! [`HeadlessBackend`] does neither — it is a no-op renderer — so analysis runs
//! need no toolkit at all.

use crate::model::{Control, Window};

/// A GUI event a backend can deliver to `GUIGetMsg`/`TrayGetMsg`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuiEvent {
    /// `$GUI_EVENT_CLOSE` for this window handle.
    Close(i64),
    /// A control was clicked.
    Control(i64),
    /// A menu item was chosen.
    Menu(i64),
    /// A tray item was chosen.
    Tray(i64),
    /// A system event, already as its `$GUI_EVENT_*` negative value.
    System(i64),
    /// A canned dialog answer (MsgBox button, dialog result).
    Dialog(i64),
}

/// An image a backend can hand back for offscreen capture.
#[derive(Debug, Clone, Default)]
pub struct GuiImage {
    pub width: u32,
    pub height: u32,
    /// Raw RGBA rows, top to bottom.
    pub rgba: Vec<u8>,
}

/// A change the *backend* made in a live window that the semantics layer
/// should apply on its next GUI call.
///
/// The model normally flows one way (semantics → backend). A live window is the
/// exception: the user types into an `Input` or toggles a `Checkbox`, and that
/// has to reach `GUICtrlRead`. A backend queues these and the semantics layer
/// drains them via [`GuiBackend::take_updates`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuiUpdate {
    /// The text of an Input/Edit control changed.
    SetText { id: i64, text: String },
    /// A Checkbox/Radio was toggled.
    SetChecked { id: i64, checked: bool },
    /// A list-like control (List/Combo/ListView/TreeView) selected an item.
    Select { id: i64, index: usize },
}

/// A renderer/event source for the GUI model.
///
/// Every method has a no-op default, so a backend only implements what it can
/// actually do; [`HeadlessBackend`] implements none of them.
pub trait GuiBackend {
    /// A window was created or changed.
    fn on_window(&mut self, _window: &Window) {}
    /// A window was destroyed.
    fn on_window_removed(&mut self, _handle: i64) {}
    /// A control was created or changed.
    fn on_control(&mut self, _control: &Control) {}
    /// A control was destroyed.
    fn on_control_removed(&mut self, _id: i64) {}
    /// Flush pending drawing to the screen.
    fn present(&mut self) {}
    /// Collect events that happened since the last call. Never blocks.
    fn poll(&mut self) -> Vec<GuiEvent> {
        Vec::new()
    }
    /// Edits made in a live window, applied by the semantics layer.
    fn take_updates(&mut self) -> Vec<GuiUpdate> {
        Vec::new()
    }
    /// Render the current model to an offscreen buffer, when supported.
    fn snapshot(&mut self) -> Option<GuiImage> {
        None
    }
}

/// The default backend: no rendering, no events of its own.
///
/// Scripted events (see `WindowsEmulation::with_gui_events` and
/// `with_gui_auto_close`) are held by the semantics layer, not here, so they
/// work with any backend.
#[derive(Debug, Default)]
pub struct HeadlessBackend;

impl HeadlessBackend {
    pub fn new() -> Self {
        Self
    }
}

impl GuiBackend for HeadlessBackend {}

// ---------------------------------------------------------------------------
// `$GUI_EVENT_*` values GUIGetMsg returns
// ---------------------------------------------------------------------------

pub const GUI_EVENT_CLOSE: i64 = -3;
pub const GUI_EVENT_MINIMIZE: i64 = -4;
pub const GUI_EVENT_RESTORE: i64 = -5;
pub const GUI_EVENT_MAXIMIZE: i64 = -6;
pub const GUI_EVENT_PRIMARYDOWN: i64 = -7;
pub const GUI_EVENT_PRIMARYUP: i64 = -8;
pub const GUI_EVENT_SECONDARYDOWN: i64 = -9;
pub const GUI_EVENT_SECONDARYUP: i64 = -10;
pub const GUI_EVENT_MOUSEMOVE: i64 = -11;
pub const GUI_EVENT_RESIZED: i64 = -12;
pub const GUI_EVENT_DROPPED: i64 = -13;

impl GuiEvent {
    /// The value `GUIGetMsg`/`TrayGetMsg` should return for this event.
    pub fn message(&self) -> i64 {
        match self {
            GuiEvent::Close(_) => GUI_EVENT_CLOSE,
            GuiEvent::System(value) => *value,
            GuiEvent::Control(id)
            | GuiEvent::Menu(id)
            | GuiEvent::Tray(id)
            | GuiEvent::Dialog(id) => *id,
        }
    }
}
