//! Widget model and backend seam for AutoIt v3 GUI emulation.
//!
//! This crate is deliberately dependency-free: the AutoIt *semantics* live in
//! `autoitv3-platform`'s `winemu::gui`, while a renderer only has to depend on
//! this crate to implement [`GuiBackend`]. Keeping the seam here also avoids a
//! dependency cycle when a renderer (such as `autoitv3-gui-egui`) is plugged
//! back into the platform as an optional feature.

pub mod backend;
pub mod model;

pub use backend::{
    GuiBackend, GuiEvent, GuiImage, GuiUpdate, HeadlessBackend, GUI_EVENT_CLOSE, GUI_EVENT_DROPPED,
    GUI_EVENT_MAXIMIZE, GUI_EVENT_MINIMIZE, GUI_EVENT_MOUSEMOVE, GUI_EVENT_PRIMARYDOWN,
    GUI_EVENT_PRIMARYUP, GUI_EVENT_RESIZED, GUI_EVENT_RESTORE, GUI_EVENT_SECONDARYDOWN,
    GUI_EVENT_SECONDARYUP,
};
pub use model::{
    Control, ControlKind, DrawCmd, Font, GuiModel, Progress, Splash, TrayItem, Window, WindowState,
    DEFAULT_DESKTOP_SIZE, GUI_BKCOLOR_LV_ALTERNATE, GUI_BKCOLOR_TRANSPARENT, GUI_CHECKED,
    GUI_DISABLE, GUI_ENABLE, GUI_HIDE, GUI_PAGE_HIDDEN, GUI_SHOW, GUI_WS_EX_PARENTDRAG,
    TIP_BALLOON, TIP_CENTER,
    WIN_ACTIVE, WIN_ENABLED, WIN_EXISTS, WIN_MAXIMIZED, WIN_MINIMIZED, WIN_VISIBLE,
};
