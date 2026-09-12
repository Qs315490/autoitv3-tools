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

use autoitv3_gui::{Control, GuiBackend, GuiEvent, GuiImage, GuiUpdate, Window};

use crate::widgets::{draw_window_body, Action, Interaction};

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
}

/// A windowed egui backend.
///
/// Cloning is cheap and every clone talks to the same window, so the script
/// thread can hold one while the caller keeps its own handle.
#[derive(Clone)]
pub struct LiveBackend {
    shared: Arc<Shared>,
    title: String,
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
            shared: Arc::new(Shared {
                mirror: Mutex::new(Mirror::default()),
                events_tx,
                events_rx: Mutex::new(events_rx),
                updates: Mutex::new(Vec::new()),
                finished: AtomicBool::new(false),
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
            closed: std::collections::HashSet::new(),
            pending: Pending::new(),
            ever_had_window: false,
            empty_frames: 0,
        };
        eframe::run_native(
            &self.title,
            eframe::NativeOptions::default(),
            Box::new(|_cc| Ok(Box::new(app))),
        )
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
}

/// Marks the script as finished, on the normal path *and* on a panic.
struct FinishGuard(Arc<Shared>);

impl Drop for FinishGuard {
    fn drop(&mut self) {
        self.0.finished.store(true, Ordering::SeqCst);
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
    /// Windows the user closed, so they are not drawn again while the script
    /// keeps running (egui's `open` flag is app-owned, not persisted by egui).
    closed: std::collections::HashSet<i64>,
    /// In-flight text edits; see [`Pending`].
    pending: Pending,
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
        let (windows, controls) = {
            let mirror = self.shared.mirror.lock().unwrap();
            (mirror.windows.clone(), mirror.controls.clone())
        };
        self.closed.retain(|handle| windows.contains_key(handle));
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

        // Disjoint field borrows: the closure draws with `shared` while
        // recording in-flight edits in `pending`.
        let shared = &self.shared;
        let pending = &mut self.pending;
        for window in windows.values() {
            if !window.visible || self.closed.contains(&window.handle) {
                continue;
            }
            let title = if window.title.is_empty() {
                "AutoIt"
            } else {
                window.title.as_str()
            };
            let mut open = true;
            egui::Window::new(title)
                .default_pos([window.x as f32, window.y as f32])
                .default_size([window.width.max(80) as f32, window.height.max(60) as f32])
                .open(&mut open)
                .show(&ctx, |ui| {
                    // Snapshot this window's controls (in creation order) so
                    // the shared widget layer can lay them out.
                    let mut body: Vec<Control> = window
                        .controls
                        .iter()
                        .filter_map(|id| controls.get(id).cloned())
                        .collect();
                    overlay_pending(&mut body, pending);
                    for interaction in draw_window_body(ui, &body) {
                        if let Action::Text(text) = &interaction.action {
                            pending.insert(interaction.id, (text.clone(), PENDING_FRAMES));
                        }
                        dispatch(shared, interaction);
                    }
                });
            if !open {
                // The user closed this window; the script decides what to do
                // with `$GUI_EVENT_CLOSE` (if it keeps running, the window
                // stays closed rather than reappearing).
                self.closed.insert(window.handle);
                let _ = self.shared.events_tx.send(GuiEvent::Close(window.handle));
            }
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
