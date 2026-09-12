//! The one mapping from an AutoIt [`Control`] to an egui widget.
//!
//! Both the offscreen [`crate::EguiBackend`] and the windowed
//! [`crate::LiveBackend`] lay the same model out, so the translation lives here
//! once instead of twice. Drawing returns the [`Interaction`]s the user
//! produced during the frame: the offscreen renderer discards them, while the
//! live window turns them into `GuiEvent`/`GuiUpdate`s for the semantics layer.
//!
//! # Fidelity
//!
//! AutoIt's real controls are Win32 (or scripts' own) widgets, so this is an
//! approximation, not a clone. Two deliberate simplifications:
//!
//! * The widget model has no parent links (AutoIt passes a parent handle to
//!   `GUICtrlCreateMenuItem`/`GUICtrlCreateTabItem`, which the model does not
//!   retain), so a `Menu` lists the window's `MenuItem` controls — every menu
//!   shows every item. Tab items are drawn as a row of tabs.
//! * Colors are read as AutoIt documents them, `0xRRGGBB`.

use autoitv3_gui::{Control, ControlKind, DrawCmd, Window, WindowState};
use egui::{
    vec2, Align2, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Sense, Stroke, StrokeKind,
    TextStyle,
};

/// What the user did to a control while it was drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// A Button/ListViewItem/etc. was clicked.
    Clicked,
    /// A MenuItem was chosen.
    Menu,
    /// The text of an Input/Edit changed.
    Text(String),
    /// A Checkbox/Radio was toggled.
    Checked(bool),
    /// A list-like control selected `index`.
    Selected(usize),
}

/// An [`Action`] and the control that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interaction {
    pub id: i64,
    pub action: Action,
}

/// Draw `control` and report any interaction with it.
///
/// Hidden controls are skipped; disabled ones are drawn greyed out and inert;
/// a non-empty `tip` becomes a hover tooltip.
pub fn draw_control(ui: &mut egui::Ui, control: &Control) -> Vec<Interaction> {
    let mut actions: Vec<Action> = Vec::new();
    if !control.is_visible() {
        return Vec::new();
    }
    let response = ui
        .push_id(control.id, |ui| {
            ui.add_enabled_ui(control.is_enabled(), |ui| {
                apply_style(ui, control);
                let mut body = |ui: &mut egui::Ui| draw_kind(ui, control, &mut actions);
                match control.bk_color {
                    // A background color paints behind whatever the control draws.
                    Some(color) => {
                        egui::Frame::new()
                            .fill(autoit_color(color))
                            .inner_margin(2.0)
                            .show(ui, &mut body);
                    }
                    None => body(ui),
                }
            });
        })
        .response;
    if !control.tip.is_empty() {
        response.on_hover_text(&control.tip);
    }
    actions
        .into_iter()
        .map(|action| Interaction {
            id: control.id,
            action,
        })
        .collect()
}

/// The geometry of a window as the script last set it. A change between frames
/// means `WinMove`/`WinSetState` moved it and the window has to follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub state: WindowState,
}

impl WindowGeometry {
    pub fn of(window: &Window) -> Self {
        Self {
            x: window.x,
            y: window.y,
            width: window.width,
            height: window.height,
            state: window.state,
        }
    }

    /// The client area the script asked for.
    fn client(self) -> egui::Vec2 {
        vec2(self.width.max(1) as f32, self.height.max(1) as f32)
    }
}

/// How a minimised window is presented.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MinimizeStyle {
    /// Faithful emulation: a minimised window is off screen, exactly as Windows
    /// hides it. The way back has to come from somewhere else — a taskbar, a
    /// script, or `WinSetState(@SW_RESTORE)`.
    #[default]
    Hidden,
    /// Keep the title bar and hide the body, so the window stays on screen as
    /// something the user can click. Double-clicking the title restores it.
    ///
    /// This is *not* what Windows does; it is the friendlier option when the
    /// backend has no taskbar.
    TitleBar,
}

/// What drawing one AutoIt window produced this frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct DrawnWindow {
    /// Client area drawn, or `None` when the window is not on screen.
    pub client: Option<egui::Vec2>,
    /// The user asked for a state change (double-clicked the title bar, a
    /// window control, ...). The caller passes it to the semantics layer so the
    /// script hears about it.
    pub state_request: Option<WindowState>,
    /// Where the minimise/maximise controls ended up, the maximise one on the
    /// right. `None` when the window was not drawn.
    pub controls: Option<egui::Rect>,
}

/// What the previous frame drew for one window; the caller keeps one per handle.
#[derive(Clone, Copy, Debug, Default)]
pub struct LastWindow {
    /// Client size drawn last frame (`None` on the first frame, when the
    /// `GUICreate` size seeds it).
    pub client: Option<egui::Vec2>,
    /// The geometry that frame was drawn from, so a script-side change is
    /// recognisable.
    pub geometry: Option<WindowGeometry>,
}

/// The egui area id of an AutoIt window. Titles are not unique (two windows may
/// share one), the handle is.
pub fn window_area_id(handle: i64) -> egui::Id {
    egui::Id::new(("autoit-window", handle))
}

/// Title bar + frame, before one has been measured. Only used to maximise.
const DEFAULT_CHROME: egui::Vec2 = vec2(16.0, 56.0);

/// Draw one AutoIt window as a floating egui window.
///
/// `last` is what the previous frame drew for this handle. Returns the client
/// size drawn now — what `WinGetClientSize` reports — or `None` when the window
/// is hidden or minimised, i.e. when nothing was drawn.
///
/// # Sizing, and why it is done by hand
///
/// `egui::Window` takes its size from its *content*: `Resize::end` falls back to
/// the content size for windows, so a window whose body does not fill it snaps
/// back on the frame after a drag. A short body therefore could be widened (the
/// inner `ScrollArea` fills the width) but never made taller. The fix is to give
/// the content a minimum size:
///
/// * first frame: the size the script passed to `GUICreate`, so the client area
///   is exactly what the script asked for instead of shrinking to its controls;
/// * afterwards: whatever we drew last, which keeps the window stable;
/// * while the pointer is down: nothing, so a drag — including one that shrinks
///   the window — is in charge;
/// * when the script moved or resized it: the script's size, and the script's
///   position, since `WinMove`/`WinSetState` own the geometry then.
///
/// The caller reports a changed client size back to the model, which is how a
/// user drag reaches `WinGetPos`; see `LiveBackend`.
pub fn show_autoit_window(
    ctx: &egui::Context,
    window: &Window,
    controls: &[Control],
    open: &mut bool,
    last: LastWindow,
    minimize: MinimizeStyle,
    actions: &mut Vec<Interaction>,
) -> DrawnWindow {
    if !window.visible {
        return DrawnWindow::default();
    }
    let minimized = window.state == WindowState::Minimized;
    if minimized && minimize == MinimizeStyle::Hidden {
        // Off screen: AutoIt keeps the window, the screen does not show it.
        return DrawnWindow::default();
    }
    let title = if window.title.is_empty() {
        "AutoIt"
    } else {
        window.title.as_str()
    };
    let geometry = WindowGeometry::of(window);
    let wanted = geometry.client();
    let script_moved = last.geometry != Some(geometry);

    // Title bar + frame, measured as outer minus client on an earlier frame.
    let chrome = last
        .client
        .and_then(|client| {
            ctx.memory(|memory| memory.area_rect(window_area_id(window.handle)))
                .map(|rect| rect.size() - client)
        })
        .unwrap_or(DEFAULT_CHROME);

    // How big the client area should be this frame, and where the window goes.
    let (decided, pos) = if minimized {
        // Title-bar style: keep the window where it is, with no body.
        (vec2(wanted.x, 0.0), None)
    } else if geometry.state == WindowState::Maximized {
        // A floating egui window cannot really be maximised, so fill the
        // viewport less the title bar and frame.
        let screen = ctx.content_rect();
        (
            (screen.size() - chrome).max(vec2(80.0, 80.0)),
            // Only on the frame the state changed: moving it every frame would
            // mean switching the drag mode every frame, and egui only creates
            // the title-bar widget (the double-click target) in title-bar mode.
            script_moved.then_some(screen.min),
        )
    } else if script_moved {
        (
            wanted,
            // AutoIt uses a negative coordinate for "leave that axis alone".
            (geometry.x >= 0 && geometry.y >= 0)
                .then(|| Pos2::new(geometry.x as f32, geometry.y as f32)),
        )
    } else {
        // What we drew last; the first frame falls back to `GUICreate`.
        (last.client.unwrap_or(wanted), None)
    };

    // The release frame still belongs to the drag: egui only writes the final
    // size into its own state then, so clamping on that frame would drop the
    // last stretch of the drag.
    let dragging = ctx.input(|input| input.pointer.any_down() || input.pointer.any_released());
    let area_id = window_area_id(window.handle);
    // Where the window controls end up; a click there is theirs, not the title
    // bar's (which would read it as a double-click).
    let mut controls_rect = None;
    let mut state_request = None;
    // `Window::max_size` is the one lever that shrinks the size egui keeps for
    // the window: `Resize` clamps its `desired_size` to it and the body fills
    // that, so without this a window that was maximised once keeps the width
    // even after the script restores it. It is in *outer* coordinates, hence
    // adding the chrome we measured.
    let max_size = if dragging {
        egui::Vec2::splat(f32::INFINITY)
    } else {
        decided + chrome
    };
    let mut drawn = None;
    let mut frame = egui::Window::new(title)
        .id(window_area_id(window.handle))
        .max_size(max_size)
        // egui collapses a window when its title is double-clicked. Windows
        // maximises instead, so turn the collapse off and handle the
        // double-click below (the title-bar widget stays: it is also what
        // drags the window).
        .collapsible(false)
        .default_pos([geometry.x as f32, geometry.y as f32])
        .default_size(wanted);
    if let Some(pos) = pos {
        // `Window`'s default drag mode (`TitleBar`) restores the *stored*
        // position every frame, which makes it ignore `current_pos` after the
        // very first frame — the script could never move a window. Asking for
        // `Anywhere` on this one frame lets the new position win; the next
        // frame goes back to title-bar dragging, from the new spot.
        frame = frame.current_pos(pos).drag_area(egui::WindowDrag::Anywhere);
    }
    frame.open(open).show(ctx, |ui| {
        // Pin the content to the intended client size in *both* directions: the
        // minimum stops a drag from snapping back, and the maximum stops the
        // content from stretching the window — AutoIt clips what does not fit,
        // it does not grow the window. While the pointer is down the drag is in
        // charge instead, which is what lets the user shrink the window.
        let target = if dragging {
            ui.available_size()
        } else {
            decided
        };
        ui.set_min_size(target);
        ui.set_max_size(target);
        if !minimized {
            actions.append(&mut draw_window_body(ui, controls));
        }
        // The client area is what the body took, measured before the window
        // controls (which live outside it, in the title bar).
        drawn = Some(ui.min_rect().size());

        // Minimise / maximise buttons in the title bar. egui only offers a
        // close button there, so draw ours in the window's own layer, created
        // after the title bar is built (later widgets win the click).
        if let Some(title) = ui.ctx().read_response(area_id.with("__title_click")) {
            let rect = title_controls_rect(title.rect);
            controls_rect = Some(rect);
            let side = rect.height();
            // Drawn and interacted with by hand: `ui.interact` does not take
            // part in the layout, so the body keeps its measured size. The
            // picture belongs to the same layer as the title bar, so a set
            // clip rect is what lets it paint above the body.
            let painter = ui.painter().clone().with_clip_rect(rect.expand(2.0));
            let restore = geometry.state != WindowState::Normal;
            let control = |rect: egui::Rect, glyph: &str, hover: &str| {
                let response =
                    ui.interact(rect, area_id.with(("control", glyph)), egui::Sense::click());
                let visuals = ui.style().interact(&response);
                painter.rect(
                    rect.shrink(1.0),
                    egui::CornerRadius::same(2),
                    visuals.bg_fill,
                    visuals.bg_stroke,
                    egui::StrokeKind::Inside,
                );
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    glyph,
                    egui::FontId::proportional(side * 0.6),
                    visuals.text_color(),
                );
                response.on_hover_text(hover).clicked()
            };
            // Right to left, the way Windows orders them.
            let maximise = egui::Rect::from_min_size(
                egui::pos2(rect.right() - side, rect.top()),
                egui::vec2(side, side),
            );
            let minimise = egui::Rect::from_min_size(
                egui::pos2(rect.right() - 2.0 * side, rect.top()),
                egui::vec2(side, side),
            );
            if control(
                maximise,
                if restore { "\u{1F5D7}" } else { "\u{1F5D6}" },
                if restore { "Restore" } else { "Maximise" },
            ) {
                state_request = Some(if restore {
                    WindowState::Normal
                } else {
                    WindowState::Maximized
                });
            }
            if control(minimise, "\u{1F5D5}", "Minimise") {
                state_request = Some(WindowState::Minimized);
            }
        }
    });

    // Windows maximises a window when its title bar is double-clicked, and
    // restores it when it is already maximised or minimised. A click that went
    // to the window controls is not a title-bar double-click.
    if state_request.is_none() && !pointer_over(ctx, controls_rect) {
        state_request = ctx
            .read_response(area_id.with("__title_click"))
            .filter(|response| response.double_clicked())
            .map(|_| match geometry.state {
                WindowState::Normal => WindowState::Maximized,
                WindowState::Maximized | WindowState::Minimized => WindowState::Normal,
            });
    }

    DrawnWindow {
        client: drawn,
        state_request,
        controls: controls_rect,
    }
}

/// The strip at the right of a title bar that the window controls occupy.
fn title_controls_rect(title: egui::Rect) -> egui::Rect {
    let side = title.height().min(28.0);
    let width = (2.0 * side).min(title.width());
    egui::Rect::from_min_max(
        egui::pos2(title.right() - width, title.top()),
        title.right_bottom(),
    )
}

/// Whether the pointer is inside `rect` right now.
fn pointer_over(ctx: &egui::Context, rect: Option<egui::Rect>) -> bool {
    match (rect, ctx.input(|input| input.pointer.latest_pos())) {
        (Some(rect), Some(pos)) => rect.contains(pos),
        _ => false,
    }
}

/// Draw an entire window body: the menu bar, then the controls that are not
/// drawn by it.
///
/// Returns the interactions produced by the whole window, so a live backend can
/// report menu clicks as menu events.
pub fn draw_window_body(ui: &mut egui::Ui, controls: &[Control]) -> Vec<Interaction> {
    let mut actions: Vec<Interaction> = Vec::new();
    let menus: Vec<&Control> = controls
        .iter()
        .filter(|control| control.kind == ControlKind::Menu && control.is_visible())
        .collect();
    let has_menus = !menus.is_empty();
    if has_menus {
        let items: Vec<&Control> = controls
            .iter()
            .filter(|control| {
                matches!(
                    control.kind,
                    ControlKind::MenuItem | ControlKind::ContextMenu
                ) && control.is_visible()
            })
            .collect();
        ui.horizontal(|ui| {
            for menu in menus {
                let title = text_or(&menu.text, "Menu");
                ui.menu_button(title, |ui| {
                    if items.is_empty() {
                        ui.label("(no items)");
                    }
                    for item in &items {
                        // (The model keeps no menu→item link, so each menu
                        // lists the window's items.)
                        if ui
                            .add_enabled(
                                item.is_enabled(),
                                egui::Button::new(text_or(&item.text, "Item")),
                            )
                            .clicked()
                        {
                            actions.push(Interaction {
                                id: item.id,
                                action: Action::Menu,
                            });
                        }
                    }
                });
            }
        });
        ui.separator();
    }
    for control in controls {
        // `Menu` is the bar itself. `MenuItem`/`ContextMenu` are drawn by the
        // bar when one exists, and standalone (as buttons) when it does not.
        let drawn_by_bar = has_menus
            && matches!(
                control.kind,
                ControlKind::MenuItem | ControlKind::ContextMenu
            );
        if control.kind == ControlKind::Menu || drawn_by_bar {
            continue;
        }
        actions.append(&mut draw_control(ui, control));
    }
    actions
}

/// Apply the control's font and text color to every text style inside `ui`.
fn apply_style(ui: &mut egui::Ui, control: &Control) {
    if let Some(font) = &control.font {
        let name = font.name.to_ascii_lowercase();
        let family = if name.contains("mono") || name.contains("consol") || name.contains("courier")
        {
            FontFamily::Monospace
        } else {
            FontFamily::Proportional
        };
        let size = if font.size > 0 {
            font.size as f32
        } else {
            12.0
        };
        let mut style: egui::Style = (**ui.style()).clone();
        for text_style in [
            TextStyle::Body,
            TextStyle::Button,
            TextStyle::Small,
            TextStyle::Heading,
            TextStyle::Monospace,
        ] {
            style
                .text_styles
                .insert(text_style, FontId::new(size, family.clone()));
        }
        *ui.style_mut() = style;
    }
    if let Some(color) = control.color {
        ui.style_mut().visuals.override_text_color = Some(autoit_color(color));
    }
}

fn draw_kind(ui: &mut egui::Ui, control: &Control, actions: &mut Vec<Action>) {
    match control.kind {
        ControlKind::Label => {
            ui.label(&control.text);
        }
        ControlKind::Button => {
            let label = text_or(&control.text, "Button").to_string();
            if place(ui, control, egui::Button::new(label)).clicked() {
                actions.push(Action::Clicked);
            }
        }
        ControlKind::Checkbox => {
            let mut checked = control.is_checked();
            if ui.checkbox(&mut checked, &control.text).changed() {
                actions.push(Action::Checked(checked));
            }
        }
        ControlKind::Radio => {
            // `ui.radio` needs a target value; a radio can only turn on.
            if ui.radio(control.is_checked(), &control.text).clicked() && !control.is_checked() {
                actions.push(Action::Checked(true));
            }
        }
        ControlKind::Group => {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                if !control.text.is_empty() {
                    ui.strong(&control.text);
                }
                ui.label(" ");
            });
        }
        ControlKind::Input => {
            let mut text = control.text.clone();
            let widget = egui::TextEdit::singleline(&mut text);
            if place(ui, control, widget).changed() {
                actions.push(Action::Text(text));
            }
        }
        ControlKind::Edit => {
            let mut text = control.text.clone();
            let widget = egui::TextEdit::multiline(&mut text);
            if place(ui, control, widget).changed() {
                actions.push(Action::Text(text));
            }
        }
        ControlKind::List => draw_list(ui, control, actions),
        ControlKind::Combo => draw_combo(ui, control, actions),
        ControlKind::ListView => draw_listview(ui, control, actions),
        ControlKind::ListViewItem => draw_listview_item(ui, control, actions),
        ControlKind::TreeView => draw_treeview(ui, control, actions),
        ControlKind::TreeViewItem => {
            let selected = control.selection == Some(0);
            if ui
                .selectable_label(selected, indent(&control.text))
                .clicked()
            {
                actions.push(Action::Selected(0));
            }
        }
        ControlKind::Tab => {
            ui.strong(text_or(&control.text, "[tabs]"));
        }
        ControlKind::TabItem => {
            if ui
                .selectable_label(control.is_checked(), text_or(&control.text, "Tab"))
                .clicked()
            {
                actions.push(Action::Clicked);
            }
        }
        ControlKind::Menu => {
            // Drawn by `draw_window_body`; standalone menus get a placeholder.
            ui.strong(text_or(&control.text, "Menu"));
        }
        ControlKind::MenuItem | ControlKind::ContextMenu => {
            if ui.button(text_or(&control.text, "Item")).clicked() {
                actions.push(Action::Menu);
            }
        }
        ControlKind::Pic | ControlKind::Icon => draw_placeholder(
            ui,
            control,
            control.image.as_deref().unwrap_or(&control.text),
            "[image]",
        ),
        ControlKind::Graphic => draw_graphic(ui, control),
        ControlKind::Progress => {
            let percent = parse_number(control).unwrap_or(0.0).clamp(0.0, 100.0);
            ui.add(egui::ProgressBar::new(percent / 100.0).text(format!("{}%", percent as i64)));
        }
        ControlKind::Slider => {
            let (min, max) = control.limit.unwrap_or((0, 100));
            let (min, max) = (min as f32, max as f32);
            let mut value = parse_number(control)
                .unwrap_or(min)
                .clamp(min, max.max(min));
            if ui
                .add(egui::Slider::new(&mut value, min..=max.max(min)))
                .changed()
            {
                actions.push(Action::Text(format!("{}", value.round() as i64)));
            }
        }
        ControlKind::Updown => {
            let mut value = parse_number(control).unwrap_or(0.0) as i64;
            if ui.add(egui::DragValue::new(&mut value)).changed() {
                actions.push(Action::Text(value.to_string()));
            }
        }
        ControlKind::Date => {
            ui.label(format!("[date] {}", text_or(&control.text, "")));
        }
        ControlKind::MonthCal => {
            ui.label("[month calendar]");
        }
        ControlKind::Avi => {
            ui.label(text_or(&control.text, "[animation]"));
        }
        ControlKind::Obj => {
            ui.label(text_or(&control.text, "[obj]"));
        }
        ControlKind::Dummy => {}
    }
}

fn draw_list(ui: &mut egui::Ui, control: &Control, actions: &mut Vec<Action>) {
    if control.data.is_empty() {
        ui.label(text_or(&control.text, "[empty list]"));
        return;
    }
    egui::ScrollArea::vertical()
        .max_height(120.0)
        .id_salt(control.id)
        .show(ui, |ui| {
            for (index, item) in control.data.iter().enumerate() {
                let selected = control.selection == Some(index);
                if ui.selectable_label(selected, item).clicked() {
                    actions.push(Action::Selected(index));
                }
            }
        });
}

fn draw_combo(ui: &mut egui::Ui, control: &Control, actions: &mut Vec<Action>) {
    let selected = control.selection.unwrap_or(0);
    let current = control
        .data
        .get(selected)
        .cloned()
        .unwrap_or_else(|| control.text.clone());
    egui::ComboBox::from_id_salt(control.id)
        .selected_text(current)
        .show_ui(ui, |ui| {
            for (index, item) in control.data.iter().enumerate() {
                if ui.selectable_label(selected == index, item).clicked() {
                    actions.push(Action::Selected(index));
                }
            }
        });
}

fn draw_listview(ui: &mut egui::Ui, control: &Control, actions: &mut Vec<Action>) {
    let columns: Vec<&str> = control.text.split('|').collect();
    egui::ScrollArea::vertical()
        .max_height(160.0)
        .id_salt(control.id)
        .show(ui, |ui| {
            egui::Grid::new(("listview", control.id))
                .striped(true)
                .show(ui, |ui| {
                    if !control.text.is_empty() {
                        for column in &columns {
                            ui.strong(*column);
                        }
                        ui.end_row();
                    }
                    for (index, row) in control.data.iter().enumerate() {
                        let selected = control.selection == Some(index);
                        let cells: Vec<&str> = row.split('|').collect();
                        let width = columns.len().max(cells.len()).max(1);
                        for cell in 0..width {
                            let text = cells.get(cell).copied().unwrap_or("");
                            if ui.selectable_label(selected, text).clicked() {
                                actions.push(Action::Selected(index));
                            }
                        }
                        ui.end_row();
                    }
                });
        });
}

fn draw_listview_item(ui: &mut egui::Ui, control: &Control, actions: &mut Vec<Action>) {
    let selected = control.selection == Some(0);
    let mut clicked = false;
    ui.horizontal(|ui| {
        for cell in control.text.split('|') {
            clicked |= ui.selectable_label(selected, cell).clicked();
        }
    });
    if clicked {
        actions.push(Action::Selected(0));
    }
}

fn draw_treeview(ui: &mut egui::Ui, control: &Control, actions: &mut Vec<Action>) {
    if control.data.is_empty() {
        ui.label(text_or(&control.text, "[empty tree]"));
        return;
    }
    egui::ScrollArea::vertical()
        .max_height(160.0)
        .id_salt(control.id)
        .show(ui, |ui| {
            for (index, item) in control.data.iter().enumerate() {
                let selected = control.selection == Some(index);
                if ui.selectable_label(selected, indent(item)).clicked() {
                    actions.push(Action::Selected(index));
                }
            }
        });
}

/// Render a stand-in box for controls whose real content (a bitmap, an icon, a
/// video frame) is not in the model: a framed rectangle with a caption.
fn draw_placeholder(ui: &mut egui::Ui, control: &Control, caption: &str, fallback: &str) {
    let size = vec2(control.width.max(48) as f32, control.height.max(24) as f32);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    let radius = CornerRadius::same(2);
    painter.rect_filled(rect, radius, ui.visuals().extreme_bg_color);
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text_or(caption, fallback),
        FontId::proportional(11.0),
        ui.visuals().text_color(),
    );
}

/// Replay the `GUICtrlSetGraphic` command list onto a framed canvas.
fn draw_graphic(ui: &mut egui::Ui, control: &Control) {
    let size = vec2(control.width.max(64) as f32, control.height.max(48) as f32);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::from_gray(24));
    let mut color = ui.visuals().text_color();
    let mut width = 1.0f32;
    for command in &control.draw {
        match command {
            DrawCmd::SetColor(value) => color = autoit_color(*value),
            DrawCmd::SetWidth(value) => width = (*value).max(1) as f32,
            DrawCmd::SetBkColor(_) | DrawCmd::SetStyle(_) | DrawCmd::Clear => {}
            DrawCmd::Line { x1, y1, x2, y2 } => {
                painter.line_segment(
                    [point(rect, *x1, *y1), point(rect, *x2, *y2)],
                    Stroke::new(width, color),
                );
            }
            DrawCmd::Rect { x, y, w, h } => {
                painter.rect_stroke(
                    Rect::from_min_size(point(rect, *x, *y), vec2(*w as f32, *h as f32)),
                    CornerRadius::ZERO,
                    Stroke::new(width, color),
                    StrokeKind::Inside,
                );
            }
            DrawCmd::Ellipse { x, y, w, h } => {
                painter.circle_stroke(
                    point(rect, *x + *w / 2, *y + *h / 2),
                    (*w).min(*h).max(0) as f32 / 2.0,
                    Stroke::new(width, color),
                );
            }
            DrawCmd::Text { x, y, text } => {
                painter.text(
                    point(rect, *x, *y),
                    Align2::LEFT_TOP,
                    text,
                    FontId::proportional(12.0),
                    color,
                );
            }
        }
    }
}

/// Add a widget at the control's AutoIt pixel size when the script gave one.
///
/// Controls are laid out in creation order rather than at their absolute
/// `x`/`y` (egui lays out in flow), but an explicit width/height is honored so
/// an Input or Button keeps the size the script asked for.
fn place(ui: &mut egui::Ui, control: &Control, widget: impl egui::Widget) -> egui::Response {
    if control.width > 0 && control.height > 0 {
        ui.add_sized(vec2(control.width as f32, control.height as f32), widget)
    } else {
        ui.add(widget)
    }
}

/// AutoIt GUI colors are documented as `0xRRGGBB`.
fn autoit_color(value: i64) -> Color32 {
    let value = value as u32;
    Color32::from_rgb(
        ((value >> 16) & 0xFF) as u8,
        ((value >> 8) & 0xFF) as u8,
        (value & 0xFF) as u8,
    )
}

/// `control.text` as a number, tolerating the empty string.
fn parse_number(control: &Control) -> Option<f32> {
    control.text.trim().parse::<f32>().ok()
}

fn point(rect: Rect, x: i32, y: i32) -> Pos2 {
    rect.min + vec2(x as f32, y as f32)
}

/// Indent a tree item by its level, encoded as leading tabs (or a `|` prefix).
fn indent(text: &str) -> String {
    let mut level = 0usize;
    let mut rest = text;
    while let Some(stripped) = rest
        .strip_prefix('\t')
        .or_else(|| rest.strip_prefix("  "))
        .or_else(|| rest.strip_prefix("| "))
        .or_else(|| rest.strip_prefix('|'))
    {
        level += 1;
        rest = stripped;
    }
    if level == 0 {
        text.to_string()
    } else {
        format!("{}{}", "    ".repeat(level), rest)
    }
}

fn text_or<'a>(text: &'a str, fallback: &'a str) -> &'a str {
    if text.is_empty() {
        fallback
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoitv3_gui::{Control, ControlKind};
    use egui::{vec2, Context, Event, Modifiers, PointerButton, RawInput};

    fn frame() -> RawInput {
        RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 200.0))),
            // A fixed time keeps the layout and the animations deterministic.
            time: Some(0.0),
            ..Default::default()
        }
    }

    /// One headless pass. `TexturesDelta` must be handled or dropped empty.
    fn run_ui(ctx: &Context, raw: RawInput, run: impl FnMut(&mut egui::Ui)) {
        let mut output = ctx.run_ui(raw, run);
        output.textures_delta.clear();
    }

    /// The rectangle a control occupies, measured with a first pass.
    fn probe(ctx: &Context, control: &Control) -> Rect {
        let mut rect = Rect::NOTHING;
        run_ui(ctx, frame(), |ui| {
            rect = ui.scope(|ui| draw_control(ui, control)).response.rect;
        });
        rect
    }

    /// Run one frame whose pointer clicks at `pos`, collecting interactions.
    fn click(ctx: &Context, control: &Control, pos: Pos2) -> Vec<Interaction> {
        let mut raw = frame();
        raw.events = vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            },
        ];
        let mut seen = Vec::new();
        run_ui(ctx, raw, |ui| {
            seen.extend(ui.scope(|ui| draw_control(ui, control)).inner);
        });
        seen
    }

    #[test]
    fn a_click_on_a_button_reports_clicked() {
        let ctx = Context::default();
        let mut button = Control::new(3, 1, ControlKind::Button);
        button.text = "Click".to_string();

        let rect = probe(&ctx, &button);
        assert!(rect.width() > 0.0, "the button was laid out");

        let seen = click(&ctx, &button, rect.center());
        assert!(
            seen.contains(&Interaction {
                id: 3,
                action: Action::Clicked
            }),
            "a click on the button produced {seen:?}"
        );
    }

    #[test]
    fn a_click_on_a_checkbox_reports_checked() {
        let ctx = Context::default();
        let mut checkbox = Control::new(4, 1, ControlKind::Checkbox);
        checkbox.text = "Enable".to_string();

        let rect = probe(&ctx, &checkbox);
        let seen = click(&ctx, &checkbox, rect.center());
        assert!(
            seen.contains(&Interaction {
                id: 4,
                action: Action::Checked(true)
            }),
            "a click on the checkbox produced {seen:?}"
        );
    }

    #[test]
    fn typing_in_an_input_reports_the_new_text() {
        let ctx = Context::default();
        let mut input = Control::new(6, 1, ControlKind::Input);
        input.text = String::new();

        // Focus it, then type.
        let rect = probe(&ctx, &input);
        let _ = click(&ctx, &input, rect.center());

        let mut raw = frame();
        raw.events = vec![Event::Text("hello".to_string())];
        let mut seen = Vec::new();
        run_ui(&ctx, raw, |ui| {
            seen.extend(ui.scope(|ui| draw_control(ui, &input)).inner);
        });
        assert!(
            seen.contains(&Interaction {
                id: 6,
                action: Action::Text("hello".to_string())
            }),
            "typing produced {seen:?}"
        );
    }

    #[test]
    fn clicking_a_list_item_reports_the_selection() {
        let ctx = Context::default();
        let mut list = Control::new(7, 1, ControlKind::List);
        list.data = vec!["only".to_string()];

        let rect = probe(&ctx, &list);
        let seen = click(&ctx, &list, rect.center());
        assert!(
            seen.contains(&Interaction {
                id: 7,
                action: Action::Selected(0)
            }),
            "clicking the item produced {seen:?}"
        );
    }

    #[test]
    fn a_hidden_or_disabled_control_reports_nothing() {
        let ctx = Context::default();
        let mut button = Control::new(5, 1, ControlKind::Button);
        button.text = "Click".to_string();
        let rect = probe(&ctx, &button);

        button.state = autoitv3_gui::GUI_HIDE;
        let hidden = click(&ctx, &button, rect.center());
        assert!(hidden.is_empty(), "a hidden control produced {hidden:?}");

        button.state = autoitv3_gui::GUI_DISABLE;
        let disabled = click(&ctx, &button, rect.center());
        assert!(
            disabled.is_empty(),
            "a disabled control produced {disabled:?}"
        );
    }
}
