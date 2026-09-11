//! A **live** egui backend: a real window whose clicks feed back into
//! `GUIGetMsg`.
//!
//! This is the interactive counterpart to the offscreen [`crate::EguiBackend`].
//! eframe runs on its own thread; the interpreter keeps calling the synchronous
//! `GuiBackend` methods. The two sides share a small model mirror and two
//! channels:
//!
//! * GUI → script: a button click, a window close, or an edit becomes a
//!   [`GuiEvent`]/[`GuiUpdate`] the semantics layer drains on its next call.
//! * script → GUI: `on_window`/`on_control` update the mirror the window draws.
//!
//! # PoC scope
//!
//! Only `Label`, `Button`, `Input`/`Edit`, `Checkbox` and `Radio` are drawn;
//! everything else falls back to a text label. There is no menu/list/tab
//! fidelity yet, and the window opens on `present()` (i.e. `GUISetState`).
//!
//! # Platform note
//!
//! eframe is started on a spawned thread, which works on Linux/X11 and
//! Wayland. Some platforms (macOS in particular) require the event loop on the
//! main thread; on those, drive eframe from `main` and run the interpreter on a
//! worker thread instead.

use std::collections::BTreeMap;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use autoitv3_gui::{Control, ControlKind, GuiBackend, GuiEvent, GuiImage, GuiUpdate, Window};

/// The model copy the GUI thread reads.
#[derive(Default)]
struct Mirror {
    windows: BTreeMap<i64, Window>,
    controls: BTreeMap<i64, Control>,
}

/// A windowed egui backend.
pub struct LiveBackend {
    mirror: Arc<Mutex<Mirror>>,
    events_tx: mpsc::Sender<GuiEvent>,
    events_rx: mpsc::Receiver<GuiEvent>,
    updates: Arc<Mutex<Vec<GuiUpdate>>>,
    title: String,
    started: bool,
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
            mirror: Arc::new(Mutex::new(Mirror::default())),
            events_tx,
            events_rx,
            updates: Arc::new(Mutex::new(Vec::new())),
            title: title.into(),
            started: false,
        }
    }

    /// Queue an event as if the user had produced it (used by the window and by
    /// tests that cannot open one).
    pub fn queue(&self, event: GuiEvent) {
        let _ = self.events_tx.send(event);
    }

    /// How many windows the mirror currently holds.
    pub fn window_count(&self) -> usize {
        self.mirror.lock().unwrap().windows.len()
    }

    /// The text the mirror holds for `id`.
    pub fn control_text(&self, id: i64) -> Option<String> {
        self.mirror
            .lock()
            .unwrap()
            .controls
            .get(&id)
            .map(|c| c.text.clone())
    }

    /// Start the eframe window thread once.
    fn start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        let mirror = self.mirror.clone();
        let events_tx = self.events_tx.clone();
        let updates = self.updates.clone();
        let title = self.title.clone();
        std::thread::spawn(move || {
            let app = LiveApp {
                mirror,
                events_tx,
                updates,
            };
            let options = eframe::NativeOptions::default();
            if let Err(e) = eframe::run_native(&title, options, Box::new(|_cc| Ok(Box::new(app)))) {
                eprintln!("[winemu/gui] live window failed: {e}");
            }
        });
    }
}

impl GuiBackend for LiveBackend {
    fn on_window(&mut self, window: &Window) {
        self.mirror
            .lock()
            .unwrap()
            .windows
            .insert(window.handle, window.clone());
    }

    fn on_window_removed(&mut self, handle: i64) {
        let mut mirror = self.mirror.lock().unwrap();
        mirror.windows.remove(&handle);
        mirror.controls.retain(|_, control| control.window != handle);
    }

    fn on_control(&mut self, control: &Control) {
        self.mirror
            .lock()
            .unwrap()
            .controls
            .insert(control.id, control.clone());
    }

    fn on_control_removed(&mut self, id: i64) {
        self.mirror.lock().unwrap().controls.remove(&id);
    }

    /// `GUISetState` calls this, so showing a window opens the real one.
    fn present(&mut self) {
        self.start();
    }

    fn poll(&mut self) -> Vec<GuiEvent> {
        self.events_rx.try_iter().collect()
    }

    fn take_updates(&mut self) -> Vec<GuiUpdate> {
        std::mem::take(&mut *self.updates.lock().unwrap())
    }

    fn snapshot(&mut self) -> Option<GuiImage> {
        None
    }
}

struct LiveApp {
    mirror: Arc<Mutex<Mirror>>,
    events_tx: mpsc::Sender<GuiEvent>,
    updates: Arc<Mutex<Vec<GuiUpdate>>>,
}

impl eframe::App for LiveApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let (windows, controls, empty) = {
            let mirror = self.mirror.lock().unwrap();
            (
                mirror.windows.clone(),
                mirror.controls.clone(),
                mirror.windows.is_empty(),
            )
        };
        if empty {
            // The script deleted its last window; close the OS window too.
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }

        for window in windows.values() {
            if !window.visible {
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
                .default_size([
                    window.width.max(80) as f32,
                    window.height.max(60) as f32,
                ])
                .open(&mut open)
                .show(&ctx, |ui| {
                    for id in &window.controls {
                        let Some(control) = controls.get(id) else {
                            continue;
                        };
                        match control.kind {
                            ControlKind::Label => {
                                ui.label(&control.text);
                            }
                            ControlKind::Button => {
                                if ui.button(&control.text).clicked() {
                                    let _ = self.events_tx.send(GuiEvent::Control(control.id));
                                }
                            }
                            ControlKind::Input | ControlKind::Edit => {
                                let mut text = control.text.clone();
                                if ui.text_edit_singleline(&mut text).changed() {
                                    self.updates.lock().unwrap().push(GuiUpdate::SetText {
                                        id: control.id,
                                        text,
                                    });
                                }
                            }
                            ControlKind::Checkbox => {
                                let mut checked = control.is_checked();
                                if ui.checkbox(&mut checked, &control.text).changed() {
                                    self.updates.lock().unwrap().push(GuiUpdate::SetChecked {
                                        id: control.id,
                                        checked,
                                    });
                                }
                            }
                            ControlKind::Radio => {
                                let _ = ui.radio(control.is_checked(), &control.text);
                            }
                            _ => {
                                if !control.text.is_empty() {
                                    ui.label(&control.text);
                                }
                            }
                        }
                    }
                });
            if !open {
                let _ = self.events_tx.send(GuiEvent::Close(window.handle));
            }
        }

        // Re-read the mirror a few times a second so script-side updates show.
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}
