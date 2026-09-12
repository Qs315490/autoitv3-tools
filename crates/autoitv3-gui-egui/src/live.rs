//! A **live** egui backend: a real window whose clicks feed back into
//! `GUIGetMsg`.
//!
//! This is the interactive counterpart to the offscreen [`crate::EguiBackend`].
//! The egui event loop owns the **main** thread and the interpreter runs on a
//! worker thread, so the two sides have to talk through shared state: a small
//! model mirror and two channels.
//!
//! * script → GUI: `on_window`/`on_control` update the mirror the window draws.
//! * GUI → script: a button click, a menu pick, a window close or a list
//!   selection becomes a [`GuiEvent`] the semantics layer drains in `poll()`;
//!   typed text and toggles become [`GuiUpdate`]s drained in `take_updates()`.
//!
//! Drawing is shared with the offscreen renderer (see [`crate::widgets`]), so
//! the window shows the same full control set.
//!
//! # Usage
//!
//! [`LiveBackend::run`] must be called from the thread that owns the process —
//! `main`. It starts the script on a worker thread and then blocks in
//! `eframe::run_native`:
//!
//! ```no_run
//! # #[cfg(feature = "window")]
//! # fn main() {
//! use autoitv3_gui_egui::LiveBackend;
//!
//! LiveBackend::new("AutoIt GUI").run(|backend| {
//!     // Build the emulation with `backend` and run the script here.
//!     let _ = backend;
//! }).expect("the window loop failed");
//! # }
//! ```
//!
//! # Platform note
//!
//! winit panics if an event loop is created off the main thread ("significant
//! cross-platform compatibility hazard"), so this module never spawns the
//! window. Instead it puts the event loop where every platform wants it and
//! moves the interpreter to a worker thread, which is plain Rust and portable.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use autoitv3_gui::{Control, GuiBackend, GuiEvent, GuiImage, GuiUpdate, Window, WindowState};

use crate::widgets::{
    record_drawn, show_autoit_window, Action, Interaction, LastWindow, MinimizeStyle,
    WindowGeometry,
};

/// The model copy the GUI thread reads.
#[derive(Default)]
struct Mirror {
    windows: BTreeMap<i64, Window>,
    controls: BTreeMap<i64, Control>,
}

/// Everything the window thread and the script thread share.
struct Shared {
    mirror: Mutex<Mirror>,
    events_tx: mpsc::Sender<GuiEvent>,
    /// A `Receiver` is not `Sync`, so the mutex is what makes `poll(&mut self)`
    /// callable while the window thread is alive.
    events_rx: Mutex<mpsc::Receiver<GuiEvent>>,
    updates: Mutex<Vec<GuiUpdate>>,
    /// Set by the script thread when it returns, so the window can close itself
    /// instead of leaving the process stuck in the event loop.
    finished: AtomicBool,
    /// The native viewport's size, in pixels: the desktop the emulated machine
    /// sees. Seeded with the size the viewport is asked to open at, then
    /// corrected from what it actually drew.
    desktop: Mutex<(i32, i32)>,
}

/// A windowed egui backend.
///
/// Cloning is cheap and every clone talks to the same window, so the script
/// thread can hold one while the caller keeps its own handle.
#[derive(Clone)]
pub struct LiveBackend {
    shared: Arc<Shared>,
    title: String,
    minimize: MinimizeStyle,
}

impl Default for LiveBackend {
    fn default() -> Self {
        Self::new("AutoIt")
    }
}

impl LiveBackend {
    /// A backend whose window is titled `title`.
    pub fn new(title: impl Into<String>) -> Self {
        let (events_tx, events_rx) = mpsc::channel();
        Self {
            minimize: MinimizeStyle::Hidden,
            shared: Arc::new(Shared {
                mirror: Mutex::new(Mirror::default()),
                events_tx,
                events_rx: Mutex::new(events_rx),
                updates: Mutex::new(Vec::new()),
                finished: AtomicBool::new(false),
                desktop: Mutex::new(autoitv3_gui::DEFAULT_DESKTOP_SIZE),
            }),
            title: title.into(),
        }
    }

    /// Open the window and run `script` on a worker thread.
    ///
    /// **Call this from `main`**: the egui/winit event loop must own the main
    /// thread. `script` receives a [`LiveBackend`] clone to hand to the
    /// emulation, and runs on its own thread while this function blocks in the
    /// event loop. It returns once the window closes — because the script
    /// finished, or because the user closed it (then the script's
    /// `GUIGetMsg` loop gets `$GUI_EVENT_CLOSE`).
    ///
    /// The worker is detached: when the window closes, `run` returns and the
    /// process may exit even if the script still ignores the close.
    pub fn run<F>(&self, script: F) -> Result<(), eframe::Error>
    where
        F: FnOnce(LiveBackend) + Send + 'static,
    {
        let shared = self.shared.clone();
        let backend = self.clone();
        std::thread::spawn(move || {
            // A guard, not a plain store: if the script panics the window must
            // still close, or `run` would never return.
            let _finished = FinishGuard(shared);
            script(backend);
        });

        let app = LiveApp {
            shared: self.shared.clone(),
            minimize: self.minimize,
            closed: std::collections::HashSet::new(),
            pending: Pending::new(),
            seen: HashMap::new(),
            requested: HashMap::new(),
            ever_had_window: false,
            empty_frames: 0,
        };
        // The native window *is* the emulated desktop, so ask for the display
        // mode up front: that way `@DesktopWidth` is already right before the
        // first frame, instead of falling back and then jumping.
        let (width, height) = autoitv3_gui::DEFAULT_DESKTOP_SIZE;
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([width as f32, height as f32]),
            ..Default::default()
        };
        eframe::run_native(
            &self.title,
            options,
            Box::new(|cc| {
                // Before the first frame: egui's bundled fonts cover Latin and
                // emoji only, so Chinese in a script's controls needs a font
                // from the machine.
                crate::fonts::install_cjk_font(&cc.egui_ctx);
                Ok(Box::new(app))
            }),
        )
    }

    /// How a minimised window is shown; see [`MinimizeStyle`].
    ///
    /// The default, [`MinimizeStyle::Hidden`], is faithful: the window leaves
    /// the screen and the window itself draws a "minimised" strip along the
    /// bottom of the viewport so the user can bring it back.
    pub fn with_minimize_style(mut self, minimize: MinimizeStyle) -> Self {
        self.minimize = minimize;
        self
    }

    /// Queue an event as if the user had produced it (used by the window and by
    /// tests that cannot open one).
    pub fn queue(&self, event: GuiEvent) {
        let _ = self.shared.events_tx.send(event);
    }

    /// Report one widget interaction exactly as the window loop would.
    ///
    /// A click becomes a `GuiEvent` for `GUIGetMsg`; an edit becomes a
    /// `GuiUpdate` for `GUICtrlRead`. This is what the tests drive.
    pub fn simulate(&self, interaction: Interaction) {
        dispatch(&self.shared, interaction);
    }

    /// How many windows the mirror currently holds.
    pub fn window_count(&self) -> usize {
        self.shared.mirror.lock().unwrap().windows.len()
    }

    /// The text the mirror holds for `id`.
    pub fn control_text(&self, id: i64) -> Option<String> {
        self.shared
            .mirror
            .lock()
            .unwrap()
            .controls
            .get(&id)
            .map(|c| c.text.clone())
    }
}

impl GuiBackend for LiveBackend {
    fn on_window(&mut self, window: &Window) {
        self.shared
            .mirror
            .lock()
            .unwrap()
            .windows
            .insert(window.handle, window.clone());
    }

    fn on_window_removed(&mut self, handle: i64) {
        let mut mirror = self.shared.mirror.lock().unwrap();
        mirror.windows.remove(&handle);
        mirror
            .controls
            .retain(|_, control| control.window != handle);
    }

    fn on_control(&mut self, control: &Control) {
        self.shared
            .mirror
            .lock()
            .unwrap()
            .controls
            .insert(control.id, control.clone());
    }

    fn on_control_removed(&mut self, id: i64) {
        self.shared.mirror.lock().unwrap().controls.remove(&id);
    }

    /// `GUISetState` calls this. The event loop started by [`run`](Self::run)
    /// re-reads the mirror ~20×/s, so there is nothing to flush here.
    fn present(&mut self) {}

    fn poll(&mut self) -> Vec<GuiEvent> {
        self.shared.events_rx.lock().unwrap().try_iter().collect()
    }

    fn take_updates(&mut self) -> Vec<GuiUpdate> {
        std::mem::take(&mut *self.shared.updates.lock().unwrap())
    }

    fn snapshot(&mut self) -> Option<GuiImage> {
        None
    }

    /// The live window's viewport is the emulated desktop.
    fn desktop_size(&self) -> Option<(i32, i32)> {
        let desktop = *self.shared.desktop.lock().unwrap();
        (desktop.0 > 0 && desktop.1 > 0).then_some(desktop)
    }
}

/// The window to draw this frame: the model's, with the state the user asked
/// for (and the rectangle the script is about to restore it to) applied.
///
/// Using the restore rectangle matters: a window that was maximised sits at
/// (0, 0) with the desktop's size, so drawing it from the model until the script
/// catches up would put it in the wrong place and size for a frame.
fn effective_window(window: &Window, requested: Option<(WindowState, u8)>) -> Window {
    let mut effective = window.clone();
    let Some((state, frames)) = requested else {
        return effective;
    };
    if window.state == state || frames == 0 {
        return effective;
    }
    effective.state = state;
    if state == WindowState::Normal {
        // Where the script is about to put it back.
        if let Some((x, y, width, height)) = window.restore {
            effective.x = x;
            effective.y = y;
            effective.width = width;
            effective.height = height;
        }
    }
    effective
}

/// Marks the script as finished, on the normal path *and* on a panic.
struct FinishGuard(Arc<Shared>);

impl Drop for FinishGuard {
    fn drop(&mut self) {
        self.0.finished.store(true, Ordering::SeqCst);
    }
}

impl Shared {
    /// Publish the viewport size the window just drew at.
    fn set_desktop(&self, size: egui::Vec2) {
        let size = (size.x.round() as i32, size.y.round() as i32);
        if size.0 > 0 && size.1 > 0 {
            let mut desktop = self.desktop.lock().unwrap();
            if *desktop != size {
                *desktop = size;
            }
        }
    }
}

/// Turn one widget interaction into the event/update the semantics layer reads.
fn dispatch(shared: &Shared, interaction: Interaction) {
    let id = interaction.id;
    match interaction.action {
        Action::Clicked => {
            let _ = shared.events_tx.send(GuiEvent::Control(id));
        }
        Action::Menu => {
            let _ = shared.events_tx.send(GuiEvent::Menu(id));
        }
        Action::Text(text) => {
            shared
                .updates
                .lock()
                .unwrap()
                .push(GuiUpdate::SetText { id, text });
        }
        Action::Checked(checked) => {
            shared
                .updates
                .lock()
                .unwrap()
                .push(GuiUpdate::SetChecked { id, checked });
        }
        Action::Selected(index) => {
            shared
                .updates
                .lock()
                .unwrap()
                .push(GuiUpdate::Select { id, index });
        }
    }
}

/// How long a live-window edit is shown before the script's version wins.
/// 40 frames is ~2 s at the 50 ms repaint the window asks for, and every
/// keystroke repaints sooner than that.
const PENDING_FRAMES: u8 = 40;

/// Text edits the user made that the script has not echoed back yet.
///
/// A `TextEdit` draws from the string it is handed, so the frame right after a
/// keystroke would otherwise redraw the model's older value and the character
/// would vanish. Each edit is shown until the model agrees (or the entry
/// expires, in case the script rewrites the text to something else).
type Pending = HashMap<i64, (String, u8)>;

struct LiveApp {
    shared: Arc<Shared>,
    minimize: MinimizeStyle,
    /// Windows the user closed, so they are not drawn again while the script
    /// keeps running (egui's `open` flag is app-owned, not persisted by egui).
    closed: std::collections::HashSet<i64>,
    /// In-flight text edits; see [`Pending`].
    pending: Pending,
    /// What we last drew for each window handle; see [`LastWindow`].
    seen: HashMap<i64, LastWindow>,
    /// A state the user asked for (un-maximising by dragging, say) that the
    /// model has not confirmed yet, with a frame budget so a script that
    /// overrides it cannot pin the window forever.
    requested: HashMap<i64, (WindowState, u8)>,
    /// Set once the mirror has held a visible window, so a slow script is not
    /// mistaken for a finished one.
    ever_had_window: bool,
    /// Consecutive frames with no visible window; guards the case where the
    /// script deleted its last window without exiting. ~1s at 50 ms/frame.
    empty_frames: u32,
}

impl eframe::App for LiveApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // The native window is the emulated desktop.
        self.shared.set_desktop(ctx.content_rect().size());
        let (windows, controls) = {
            let mirror = self.shared.mirror.lock().unwrap();
            (mirror.windows.clone(), mirror.controls.clone())
        };
        self.closed.retain(|handle| windows.contains_key(handle));
        self.seen.retain(|handle, _| windows.contains_key(handle));
        self.requested.retain(|handle, (state, frames)| {
            *frames = frames.saturating_sub(1);
            windows
                .get(handle)
                .is_none_or(|window| window.state != *state)
                && *frames > 0
        });
        expire_pending(&mut self.pending, &controls);

        if windows.values().any(|window| window.visible) {
            self.ever_had_window = true;
            self.empty_frames = 0;
        } else if self.ever_had_window {
            self.empty_frames += 1;
        }
        // The script is done, or it deleted its last window and left:
        // close the app so `run` returns instead of spinning on a mirror
        // nobody updates.
        if self.shared.finished.load(Ordering::SeqCst) || self.empty_frames > 20 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        // Faithful minimise: the window is off screen, so the only way back is
        // here — a strip along the bottom, the way Windows has a taskbar.
        if self.minimize == MinimizeStyle::Hidden {
            let minimized: Vec<(i64, String)> = windows
                .values()
                .filter(|window| window.visible && window.state == WindowState::Minimized)
                .map(|window| {
                    let title = if window.title.is_empty() {
                        "AutoIt".to_string()
                    } else {
                        window.title.clone()
                    };
                    (window.handle, title)
                })
                .collect();
            if !minimized.is_empty() {
                let mut restore = Vec::new();
                egui::Panel::bottom("autoit-minimized").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Minimised:");
                        for (handle, title) in &minimized {
                            if ui.button(title).clicked() {
                                restore.push(*handle);
                            }
                        }
                    });
                });
                let mut updates = self.shared.updates.lock().unwrap();
                for handle in restore {
                    updates.push(GuiUpdate::SetWindowState {
                        handle,
                        state: WindowState::Normal,
                    });
                }
            }
        }

        // Disjoint field borrows: the widget layer draws with `shared`, the
        // overlay edits live in `pending`, and geometry is in `seen`.
        let shared = &self.shared;
        let pending = &mut self.pending;
        let seen_windows = &mut self.seen;
        let requested_states = &mut self.requested;
        let minimize = self.minimize;
        for window in windows.values() {
            if !window.visible || self.closed.contains(&window.handle) {
                continue;
            }
            let last = seen_windows
                .get(&window.handle)
                .copied()
                .unwrap_or_default();
            // A minimised/maximised window the user has dragged out of its
            // state keeps that state locally until the script applies it,
            // instead of snapping back for a frame.
            let requested = requested_states.get(&window.handle).copied();
            let effective = effective_window(window, requested);

            let mut open = true;
            let mut actions = Vec::new();
            // Snapshot this window's controls (in creation order) so the shared
            // widget layer can lay them out.
            let mut body: Vec<Control> = window
                .controls
                .iter()
                .filter_map(|id| controls.get(id).cloned())
                .collect();
            overlay_pending(&mut body, pending);
            let drawn = show_autoit_window(
                &ctx,
                &effective,
                &body,
                &mut open,
                last,
                minimize,
                &mut actions,
            );

            // A double-click on the title bar, a window control, or a drag out
            // of the maximised state: tell the script what the user asked for.
            if let Some(state) = drawn.state_request {
                shared
                    .updates
                    .lock()
                    .unwrap()
                    .push(GuiUpdate::SetWindowState {
                        handle: window.handle,
                        state,
                    });
                if state != window.state {
                    requested_states.insert(window.handle, (state, PENDING_FRAMES));
                }
            }

            for interaction in actions {
                if let Action::Text(text) = &interaction.action {
                    pending.insert(interaction.id, (text.clone(), PENDING_FRAMES));
                }
                dispatch(shared, interaction);
            }
            if !open {
                // The user closed this window; the script decides what to do
                // with `$GUI_EVENT_CLOSE` (if it keeps running, the window
                // stays closed rather than reappearing).
                self.closed.insert(window.handle);
                let _ = shared.events_tx.send(GuiEvent::Close(window.handle));
            }

            // One place decides what to remember and what the user changed; the
            // same function is what the tests drive.
            // A state the user asked for makes the frame's geometry theirs to
            // report, even though the model has not caught up yet.
            let user_state = requested_states.contains_key(&window.handle);
            let geometry = WindowGeometry::of(&effective);
            let (next, updates) = record_drawn(&effective, geometry, last, drawn, user_state);
            if !updates.is_empty() {
                shared.updates.lock().unwrap().extend(updates);
            }
            seen_windows.insert(window.handle, next);
        }

        // Re-read the mirror a few times a second so script-side updates show.
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

/// Show in-flight edits on top of the model the window just read.
fn overlay_pending(controls: &mut [Control], pending: &Pending) {
    for control in controls {
        if let Some((text, _)) = pending.get(&control.id) {
            control.text = text.clone();
        }
    }
}

/// Drop edits the script has applied (or that aged out) and count the rest down.
fn expire_pending(pending: &mut Pending, controls: &BTreeMap<i64, Control>) {
    pending.retain(|id, (text, frames)| {
        *frames = frames.saturating_sub(1);
        *frames > 0 && controls.get(id).map(|control| &control.text) != Some(text)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoitv3_gui::ControlKind;

    fn input(id: i64, text: &str) -> Control {
        let mut control = Control::new(id, 1, ControlKind::Input);
        control.text = text.to_string();
        control
    }

    #[test]
    fn an_in_flight_edit_is_shown_until_the_script_echoes_it() {
        let mut controls = BTreeMap::new();
        controls.insert(1, input(1, "world"));

        let mut pending = Pending::new();
        pending.insert(1, ("world!".to_string(), PENDING_FRAMES));

        // The window draws what the user typed, not the model's stale text.
        let mut body = vec![input(1, "world")];
        overlay_pending(&mut body, &pending);
        assert_eq!(body[0].text, "world!");

        // The script has not applied it yet, so it stays pending.
        expire_pending(&mut pending, &controls);
        assert_eq!(pending.len(), 1);

        // Once GUICtrlRead's write-back reaches the model, the overlay is gone.
        controls.insert(1, input(1, "world!"));
        expire_pending(&mut pending, &controls);
        assert!(pending.is_empty());
    }

    #[test]
    fn the_viewport_is_the_desktop() {
        let backend = LiveBackend::new("W");
        // Before the first frame it reports the size the viewport was asked to
        // open at, so a script that reads the desktop early sees no change.
        assert_eq!(
            backend.desktop_size(),
            Some(autoitv3_gui::DEFAULT_DESKTOP_SIZE)
        );

        backend.shared.set_desktop(egui::vec2(1024.0, 640.0));
        assert_eq!(backend.desktop_size(), Some((1024, 640)));

        // A degenerate size is not a desktop.
        backend.shared.set_desktop(egui::vec2(0.0, 0.0));
        assert_eq!(backend.desktop_size(), Some((1024, 640)));
    }

    #[test]
    fn a_window_the_user_un_maximised_is_drawn_where_it_will_be_restored_to() {
        let mut window = Window::new(1, "W", 960, 540);
        window.x = 0;
        window.y = 0;
        window.state = WindowState::Maximized;
        window.restore = Some((220, 140, 520, 300));

        // While the script has not applied the state yet, draw the rectangle it
        // is about to restore to — not the maximised one at (0, 0), which would
        // both look wrong and be reported back as the window's place.
        let effective = effective_window(&window, Some((WindowState::Normal, PENDING_FRAMES)));
        assert_eq!((effective.x, effective.y), (220, 140));
        assert_eq!((effective.width, effective.height), (520, 300));
        assert_eq!(effective.state, WindowState::Normal);

        // A script that never applies the state cannot pin the window: the
        // frame budget expiring falls back to the model.
        let expired = effective_window(&window, Some((WindowState::Normal, 0)));
        assert_eq!((expired.x, expired.y), (0, 0));
        assert_eq!(expired.state, WindowState::Maximized);

        // Nothing requested: the model is the truth.
        let plain = effective_window(&window, None);
        assert_eq!((plain.x, plain.y), (0, 0));
        assert_eq!(plain.state, WindowState::Maximized);
    }

    #[test]
    fn an_edit_the_script_never_echoes_expires() {
        let mut controls = BTreeMap::new();
        controls.insert(1, input(1, "world"));

        let mut pending = Pending::new();
        pending.insert(1, ("typed".to_string(), 2));
        expire_pending(&mut pending, &controls);
        assert_eq!(pending.len(), 1, "still fresh");
        expire_pending(&mut pending, &controls);
        assert!(
            pending.is_empty(),
            "the script rewrote it to something else"
        );
    }
}
