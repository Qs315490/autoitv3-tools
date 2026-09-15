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

use autoitv3_gui_model::{Control, ControlKind, DrawCmd, GuiUpdate, Window, WindowState};
use egui::{
    vec2, Align2, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Sense, Stroke, StrokeKind,
    TextStyle, Vec2,
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
    draw_control_with(ui, control, &[])
}

/// As [`draw_control`], but told about the window's other controls.
///
/// A tree draws its own rows, and the row hierarchy lives in the item controls,
/// so the renderer needs to see them; a control drawn on its own (as the unit
/// tests do) falls back to reading the row text.
pub fn draw_control_with(
    ui: &mut egui::Ui,
    control: &Control,
    siblings: &[Control],
) -> Vec<Interaction> {
    let mut actions: Vec<Action> = Vec::new();
    if !control.is_visible() {
        return Vec::new();
    }
    let response = ui
        .push_id(control.id, |ui| {
            ui.add_enabled_ui(control.is_enabled(), |ui| {
                apply_style(ui, control);
                let mut body = |ui: &mut egui::Ui| draw_kind(ui, control, &mut actions, siblings);
                match control.background() {
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
        // A title goes above the text, and is the only thing an icon can sit
        // next to — `GUICtrlSetTip` says as much.
        let text = if control.tip_title.is_empty() {
            control.tip.clone()
        } else {
            format!("{}\n{}", control.tip_title, control.tip)
        };
        response.on_hover_text(text);
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

    /// The top-left the script asked for.
    fn pos(self) -> egui::Pos2 {
        egui::pos2(self.x.max(0) as f32, self.y.max(0) as f32)
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
    /// Title bar + frame, measured this frame (outer minus client).
    pub chrome: Option<egui::Vec2>,
    /// Where the window ended up, as the model's `x`/`y` see it.
    pub pos: Option<egui::Pos2>,
    /// The pointer was dragging *this* window this frame, so its geometry is
    /// the user's: the caller must report a change as theirs even when the
    /// model looks like it moved (the model is only echoing the last report).
    pub pointer_owns: bool,
    /// The pointer is dragging this window's title bar *for us*: it grabbed a
    /// maximised window and the window is being carried by the drag (see
    /// `show_autoit_window`).
    pub title_drag: bool,
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
    /// Title bar + frame, measured as outer minus drawn client. Kept here
    /// rather than re-measured by the caller, so a frame that draws a window at
    /// some other size (minimised, maximised) cannot corrupt it.
    pub chrome: Option<egui::Vec2>,
    /// Where the window was drawn.
    pub pos: Option<egui::Pos2>,
    /// The pointer was dragging this window, so it owns the geometry until it
    /// is released — a fast drag can leave the pointer outside the rectangle
    /// the last frame drew, and the drag must not change hands mid-way.
    pub pointer_owns: bool,
    /// The title-bar drag that took this window out of a maximised state is
    /// still going: it belongs to us until the pointer is released, because
    /// after the restore the grab point is no longer over the window at all.
    pub title_drag: bool,
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
///   position, since `WinMove`/`WinSetState` own the geometry then — *unless*
///   the pointer is dragging this window right now, because then the model is
///   only echoing back the place the drag reported, one round trip behind the
///   pointer. Applying it would make the window lag and stutter under the drag.
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
    // Is the model's geometry news since the frame we drew? The live backend
    // folds what the user dragged *into the model* right away, and the script's
    // echo carries the same numbers back, so "what we drew last" is what the
    // model now says: an echo is not news, a `WinMove` is.
    //
    // Comparing the *drawn* place and size (rather than the geometry that frame
    // was drawn from) is what lets a place the pointer skipped be applied later:
    // a frame that skipped it still records where the window really is, so the
    // next frame sees it as news instead of leaving the window behind.
    let place_is_news = last
        .pos
        .is_none_or(|drawn| drawn.distance(geometry.pos()) > 0.5);
    let size_is_news = last
        .client
        .is_none_or(|drawn| (drawn - wanted).length() > 0.5);

    // Title bar + frame, as measured on an earlier frame.
    let chrome = last.chrome.unwrap_or(DEFAULT_CHROME);

    // The release frame still belongs to the drag: egui only writes the final
    // size into its own state then, so clamping on that frame would drop the
    // last stretch of the drag.
    let dragging = ctx.input(|input| input.pointer.any_down() || input.pointer.any_released());

    // Is the pointer on the window we drew last? Only the window a drag started
    // on belongs to the pointer, so a script moving *another* window mid-drag
    // still gets its move.
    //
    // While the pointer is on ours it owns the place: the script applies the
    // position we reported a moment ago, so taking the model's place here would
    // snap the window back to where it was one frame — or one poll — ago. That
    // is what makes a drag lag behind the pointer and stutter.
    let pointer_owns_geometry = dragging
        && (last.pointer_owns
            || ctx
                .input(|input| input.pointer.interact_pos())
                .zip(last.pos.zip(last.client))
                .is_some_and(|(pointer, (pos, client))| {
                    egui::Rect::from_min_size(pos, client + chrome)
                        .expand(4.0)
                        .contains(pointer)
                }));
    let area_id = window_area_id(window.handle);

    // Is the pointer dragging this window by its *title bar*? The pointer's own
    // state says so (a click or a double-click does not move it), and the grab
    // point has to be in the title-bar band of the window we drew last. egui's
    // `Response` for the title is only registered inside `Window::show`, so
    // asking for it here would be a frame late.
    let title_grab = ctx
        .input(|input| {
            let press = input.pointer.press_origin()?;
            let pointer = input.pointer.interact_pos()?;
            (input.pointer.is_decidedly_dragging() && input.pointer.any_down())
                .then_some((press, pointer))
        })
        .filter(|(press, _)| {
            last.pos.zip(last.client).is_some_and(|(pos, client)| {
                let outer = Rect::from_min_size(pos, client + chrome);
                let title_bar =
                    Rect::from_min_max(outer.min, egui::pos2(outer.max.x, outer.min.y + chrome.y));
                title_bar.contains(*press)
            })
        });

    // Windows restores a maximised window as soon as its title bar is dragged:
    // it takes the size the script will restore it to, and puts the window where
    // the pointer keeps the relative spot it grabbed.
    //
    // The drag then belongs to us (`title_drag`) until the pointer is released:
    // after the restore the grab point is not over the window any more, and egui
    // cannot carry a window that is still as big as the viewport (it clamps it
    // back into the corner), so we place it ourselves — under the pointer, at
    // the relative spot it grabbed.
    let restoring_by_title_bar = title_grab.is_some_and(|_| {
        last.geometry
            .is_some_and(|previous| previous.state == WindowState::Maximized)
            || geometry.state == WindowState::Maximized
    });
    let title_drag = (restoring_by_title_bar || last.title_drag) && dragging;
    let title_place = (title_drag || restoring_by_title_bar)
        .then(|| {
            ctx.input(|input| Some((input.pointer.press_origin()?, input.pointer.interact_pos()?)))
        })
        .flatten()
        .map(|(press, pointer)| {
            let screen = ctx.content_rect();
            // The size the window is restored to: the model's once it has it,
            // the rectangle stored for the restore until then.
            let (width, height) = window
                .restore
                .map(|(_, _, width, height)| (width as f32, height as f32))
                .unwrap_or((wanted.x, wanted.y));
            let size = vec2(width, height);
            let grabbed =
                ((press - screen.min) / screen.size()).clamp(Vec2::ZERO, Vec2::splat(1.0));
            (size, pointer - grabbed * (size + chrome))
        });

    // How big the client area should be this frame, and where the window goes.
    let (decided, pos) = if let Some((size, place)) = title_place {
        (size, Some(place))
    } else if minimized {
        // Title-bar style: keep the window where it is, with no body.
        (vec2(wanted.x, 0.0), None)
    } else if geometry.state == WindowState::Maximized {
        // A floating egui window cannot really be maximised, so fill the
        // viewport less the title bar and frame.
        let screen = ctx.content_rect();
        let news = last
            .pos
            .is_none_or(|drawn| drawn.distance(screen.min) > 0.5);
        (
            (screen.size() - chrome).max(vec2(80.0, 80.0)),
            // Only when the window is not already there: moving it every frame
            // would mean switching the drag mode every frame, and egui only
            // creates the title-bar widget (the double-click target) in
            // title-bar mode.
            (news && !pointer_owns_geometry).then_some(screen.min),
        )
    } else if pointer_owns_geometry {
        // The pointer owns a normal window: egui's own drag handling decides
        // the place (we must not move it back to the model's), and `dragging`
        // below releases the size for the drag to decide.
        (last.client.unwrap_or(wanted), None)
    } else if place_is_news || size_is_news {
        (
            wanted,
            // AutoIt uses a negative coordinate for "leave that axis alone".
            (place_is_news && geometry.x >= 0 && geometry.y >= 0)
                .then(|| Pos2::new(geometry.x as f32, geometry.y as f32)),
        )
    } else {
        // Nothing new: keep what we drew; the first frame falls back to
        // `GUICreate`.
        (last.client.unwrap_or(wanted), None)
    };
    // Where the window controls end up; a click there is theirs, not the title
    // bar's (which would read it as a double-click).
    let mut controls_rect = None;
    let mut state_request = None;
    // `Window::max_size` is the one lever that shrinks the size egui keeps for
    // the window: `Resize` clamps its `desired_size` to it and the body fills
    // that, so without this a window that was maximised once keeps the width
    // even after the script restores it. It is in *outer* coordinates, hence
    // adding the chrome we measured.
    if let Some((_, anchor)) = title_place {
        // egui recomputes a dragged window's place as "where the drag started,
        // plus everything it has dragged since" (`Area` keeps that start in its
        // own temp data), so re-positioning the window now would be undone on
        // the next frame. Move the drag's start instead — to where the window
        // belongs now, minus the drag so far — and the drag carries on from the
        // restored title bar.
        let dragged = ctx
            .input(|input| input.pointer.total_drag_delta())
            .unwrap_or_default();
        ctx.data_mut(|data| {
            data.insert_temp(area_id.with("pivot_at_drag_start"), anchor - dragged);
        });
    }

    // While the pointer is down the drag decides the size; a title-bar drag that
    // took a maximised window is the exception — that size comes from the model.
    let drag_decides_size = dragging && title_place.is_none();
    let max_size = if drag_decides_size {
        Vec2::splat(f32::INFINITY)
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
        if title_place.is_some() {
            // A window still as big as the viewport cannot be moved at all —
            // egui clamps it back into the corner — and the place is ours for
            // the whole drag anyway.
            frame = frame.constrain(false);
        }
    }
    frame.open(open).show(ctx, |ui| {
        // Pin the content to the intended client size in *both* directions: the
        // minimum stops a drag from snapping back, and the maximum stops the
        // content from stretching the window — AutoIt clips what does not fit,
        // it does not grow the window. While the pointer is down the drag is in
        // charge instead, which is what lets the user shrink the window.
        let target = if drag_decides_size {
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

        // Minimise / maximise buttons in the title bar, drawn the way egui
        // draws its own close button: line art in the text colour, no
        // background, a little bigger on hover. egui only offers the close
        // button, so ours are laid out by hand where the title bar's atoms
        // would put them (they do not take part in the layout, which would
        // change the size egui measures for the body).
        if let Some(title) = ui.ctx().read_response(area_id.with("__title_click")) {
            let style = ui.style().clone();
            let margin = egui::Frame::window(&style).total_margin();
            let icon = style.spacing.icon_width;
            let gap = style.spacing.item_spacing.x;
            // The title-bar rect is the heading row plus the frame around it.
            let row_height = (title.rect.height() - margin.sum().y).max(icon);
            let center_y = title.rect.top() + margin.top + row_height / 2.0;
            // `title.rect.right()` is the close button's left edge.
            let slot = |index: f32| {
                egui::Rect::from_center_size(
                    egui::pos2(
                        title.rect.right() - gap * (index + 1.0) - icon * (index + 0.5),
                        center_y,
                    ),
                    egui::Vec2::splat(icon),
                )
            };
            let maximise = slot(0.0);
            let minimise = slot(1.0);
            controls_rect = Some(maximise.union(minimise));
            let restore = geometry.state != WindowState::Normal;

            let (response, rect, stroke) = title_bar_button(
                ui,
                area_id.with("maximise"),
                maximise,
                if restore {
                    "Restore window"
                } else {
                    "Maximise window"
                },
            );
            let painter = ui.painter().clone().with_clip_rect(rect.expand(4.0));
            if restore {
                // Two overlapping squares, the way Windows draws restore.
                let inner = rect.shrink(2.0);
                let back = egui::Rect::from_min_size(
                    inner.min + egui::vec2(inner.width() * 0.3, 0.0),
                    inner.size() * 0.7,
                );
                let front = egui::Rect::from_min_size(
                    inner.min + egui::vec2(0.0, inner.height() * 0.3),
                    inner.size() * 0.7,
                );
                painter.rect_stroke(
                    back,
                    egui::CornerRadius::ZERO,
                    stroke,
                    egui::StrokeKind::Inside,
                );
                painter.rect_stroke(
                    front,
                    egui::CornerRadius::ZERO,
                    stroke,
                    egui::StrokeKind::Inside,
                );
            } else {
                painter.rect_stroke(
                    rect.shrink(2.0),
                    egui::CornerRadius::ZERO,
                    stroke,
                    egui::StrokeKind::Inside,
                );
            }
            if response
                .on_hover_text(if restore { "Restore" } else { "Maximise" })
                .clicked()
            {
                state_request = Some(if restore {
                    WindowState::Normal
                } else {
                    WindowState::Maximized
                });
            }

            let (response, rect, stroke) =
                title_bar_button(ui, area_id.with("minimise"), minimise, "Minimise window");
            let painter = ui.painter().clone().with_clip_rect(rect.expand(4.0));
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 2.0, rect.center().y),
                    egui::pos2(rect.right() - 2.0, rect.center().y),
                ],
                stroke,
            );
            if response.on_hover_text("Minimise").clicked() {
                state_request = Some(WindowState::Minimized);
            }
        }
    });

    let pos = ctx
        .memory(|memory| memory.area_rect(area_id))
        .map(|rect| rect.min);

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

    // Dragging a maximised window's border or title bar un-maximises it, the
    // way Windows puts the window back to its normal placement and lets the
    // pointer take it from there.
    if geometry.state == WindowState::Maximized && dragging {
        let resized = drawn.is_some_and(|client| (client - decided).length() > 0.5);
        let moved = pos.is_some_and(|pos| pos.distance(geometry.pos()) > 0.5);
        if resized || moved {
            state_request = Some(WindowState::Normal);
        }
    }

    DrawnWindow {
        client: drawn,
        state_request,
        controls: controls_rect,
        pos,
        pointer_owns: pointer_owns_geometry,
        title_drag,
        // Outer minus what we drew: exact, whatever the window's state.
        chrome: drawn
            .and_then(|client| {
                ctx.memory(|memory| memory.area_rect(area_id))
                    .map(|rect| rect.size() - client)
            })
            .or(last.chrome),
    }
}

/// The window to draw this frame: the model's, with the state the user asked
/// for (and the rectangle the script is about to restore it to) applied.
///
/// Using the restore rectangle matters: a window that was maximised sits at
/// (0, 0) with the desktop's size, so drawing it from the model until the script
/// catches up would put it in the wrong place and size for a frame.
pub fn effective_window(window: &Window, requested: Option<(WindowState, u8)>) -> Window {
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

/// Fold what the user just did into the window the model holds, at once.
///
/// The live backend does this the moment a frame reports a drag: the script may
/// not poll for another second, and until it does the model has to agree with
/// what the window shows — otherwise the next frame reads the place the drag
/// just left as the model's and snaps the window back to it.
pub fn fold_user_update(window: &mut Window, update: &GuiUpdate) {
    match *update {
        GuiUpdate::Move { handle, x, y } if handle == window.handle => {
            // The user took the window somewhere the pending restore would not
            // have put it: the rectangle it came from is history, or the frame
            // after the pointer lets go would pull the window back there.
            if window
                .restore
                .is_some_and(|(rx, ry, ..)| (x, y) != (rx, ry))
            {
                window.restore = None;
            }
            window.x = x;
            window.y = y;
        }
        GuiUpdate::Resize {
            handle,
            width,
            height,
        } if handle == window.handle => {
            window.width = width;
            window.height = height;
        }
        _ => {}
    }
}

/// Fold one drawn frame into the record the next frame is drawn against, and
/// report a size the *user* dragged as a [`GuiUpdate::Resize`].
///
/// Every caller must fold frames the same way: forgetting to carry the drawn
/// size (or the chrome) over a minimised or maximised frame is what makes a
/// window oscillate between two sizes.
///
/// `user_state` says `geometry` carries a state the *user* asked for and the
/// script has not applied yet, or that the pointer is dragging the window
/// (see [`DrawnWindow::pointer_owns`]): a size or place change on such a frame
/// is the user's, even though the model looks like it moved the window itself.
pub fn record_drawn(
    window: &Window,
    geometry: WindowGeometry,
    last: LastWindow,
    drawn: DrawnWindow,
    user_state: bool,
) -> (LastWindow, Vec<GuiUpdate>) {
    let chrome = drawn.chrome.or(last.chrome);
    let record = |client| LastWindow {
        client,
        geometry: Some(geometry),
        chrome,
        pos: drawn.pos,
        pointer_owns: drawn.pointer_owns,
        title_drag: drawn.title_drag,
    };
    let Some(client) = drawn.client else {
        // Nothing was drawn: keep the size for when the window comes back.
        return (record(last.client), Vec::new());
    };
    // A drag is the only thing worth reporting: a size or place the script
    // chose is already in the model, and a minimised or maximised window is
    // sized by its state and the desktop rather than by the pointer.
    //
    let script_moved = last.geometry != Some(geometry);
    // A window we are carrying ourselves (`title_drag`) is the user's even when
    // the model still calls it maximised: the drag has already restored it.
    let user_dragged = (geometry.state == WindowState::Normal || drawn.title_drag)
        && (user_state || !script_moved)
        && last.client.is_some();
    let mut updates = Vec::new();
    if user_dragged {
        if last
            .client
            .is_some_and(|previous| (client - previous).length() > 0.5)
            && client.x >= 1.0
            && client.y >= 1.0
        {
            updates.push(GuiUpdate::Resize {
                handle: window.handle,
                width: client.x.round() as i32,
                height: client.y.round() as i32,
            });
        }
        if let Some(pos) = drawn.pos {
            let (x, y) = (pos.x.round() as i32, pos.y.round() as i32);
            // Where the *user* put it is where it moved to since the last frame
            // we drew. Comparing against the model instead would report the
            // place a maximised window occupies while a border drag un-maximises
            // it — (0, 0) — and wipe out the place the script restores to.
            let moved = last
                .pos
                .map(|previous| (pos - previous).length() > 0.5)
                .unwrap_or(false);
            if moved {
                updates.push(GuiUpdate::Move {
                    handle: window.handle,
                    x,
                    y,
                });
            }
        }
    }
    (record(Some(client)), updates)
}

/// A title-bar window button: clicked, described for accessibility, and
/// returned with the rect and stroke egui's own close button would use.
fn title_bar_button(
    ui: &egui::Ui,
    id: egui::Id,
    rect: egui::Rect,
    label: &str,
) -> (egui::Response, egui::Rect, egui::Stroke) {
    let response = ui.interact(rect, id, egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let visuals = ui.style().interact(&response);
    let rect = rect.shrink(2.0).expand(visuals.expansion);
    (response, rect, visuals.fg_stroke)
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
        .filter(|control| {
            control.kind == ControlKind::Menu && control.is_visible() && control.parent.is_none()
        })
        .collect();
    let has_menus = !menus.is_empty();
    if has_menus {
        ui.horizontal(|ui| {
            for menu in menus {
                let title = text_or(&menu.text, "Menu");
                ui.menu_button(title, |ui| {
                    draw_menu_entries(ui, menu.id, controls, &mut actions, 0);
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
        // A list row or tree node belongs to the control that owns it, which
        // draws every row itself: drawing the item here as well would show the
        // same row twice.
        if matches!(control.kind, ControlKind::ListViewItem | ControlKind::TreeViewItem)
            && control.parent.is_some()
        {
            continue;
        }
        actions.append(&mut draw_control_with(ui, control, controls));
    }
    actions
}

/// Draw one menu level: the entries that hang under `menu`.
///
/// AutoIt keeps the menu structure in the model — a `MenuItem` (or a submenu
/// `Menu`) names the menu it belongs to — so this walks it the same way the
/// script built it.
fn draw_menu_entries(
    ui: &mut egui::Ui,
    menu: i64,
    controls: &[Control],
    actions: &mut Vec<Interaction>,
    depth: usize,
) {
    let entries: Vec<&Control> = controls
        .iter()
        .filter(|control| {
            control.parent == Some(menu)
                && control.is_visible()
                && matches!(
                    control.kind,
                    ControlKind::Menu | ControlKind::MenuItem | ControlKind::ContextMenu
                )
        })
        .collect();
    if entries.is_empty() {
        ui.label("(no items)");
        return;
    }
    for entry in entries {
        if entry.kind == ControlKind::Menu && depth < 4 {
            // A submenu opens the menu it names.
            let title = text_or(&entry.text, "Menu");
            ui.menu_button(title, |ui| {
                draw_menu_entries(ui, entry.id, controls, actions, depth + 1);
            });
        } else if ui
            .add_enabled(
                entry.is_enabled(),
                egui::Button::new(text_or(&entry.text, "Item")),
            )
            .clicked()
        {
            actions.push(Interaction {
                id: entry.id,
                action: Action::Menu,
            });
        }
    }
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

fn draw_kind(
    ui: &mut egui::Ui,
    control: &Control,
    actions: &mut Vec<Action>,
    siblings: &[Control],
) {
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
        ControlKind::ListView => draw_listview(ui, control, actions, siblings),
        ControlKind::ListViewItem => draw_listview_item(ui, control, actions),
        ControlKind::TreeView => draw_treeview(ui, control, actions, siblings),
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

fn draw_listview(
    ui: &mut egui::Ui,
    control: &Control,
    actions: &mut Vec<Action>,
    siblings: &[Control],
) {
    let columns: Vec<&str> = control.text.split('|').collect();
    egui::ScrollArea::vertical()
        .max_height(160.0)
        .id_salt(control.id)
        .show(ui, |ui| {
            // `$GUI_BKCOLOR_LV_ALTERNATE` paints every second row with the row's
            // own colour, which a grid cannot do per row — so that mode draws
            // one frame per row instead.
            if control.alternating_rows() {
                if !control.text.is_empty() {
                    ui.horizontal(|ui| {
                        for column in &columns {
                            ui.strong(*column);
                        }
                    });
                }
                for (index, row) in control.data.iter().enumerate() {
                    let selected = control.selection == Some(index);
                    let cells: Vec<&str> = row.split('|').collect();
                    let width = columns.len().max(cells.len()).max(1);
                    let fill = row_color(control, index, siblings).map(autoit_color);
                    let mut draw = |ui: &mut egui::Ui| {
                        ui.horizontal(|ui| {
                            for cell in 0..width {
                                let text = cells.get(cell).copied().unwrap_or("");
                                if ui.selectable_label(selected, text).clicked() {
                                    actions.push(Action::Selected(index));
                                }
                            }
                        });
                    };
                    match fill {
                        Some(color) => {
                            egui::Frame::new().fill(color).show(ui, &mut draw);
                        }
                        None => draw(ui),
                    }
                }
                return;
            }
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

/// The colour of one alternating `ListView` row.
///
/// The help page counts lines from one: the odd ones take the ListView's own
/// colour and the even ones the colour of the row's item, with the ListView's
/// colour as the fallback an item without one gets.
fn row_color(listview: &Control, row: usize, siblings: &[Control]) -> Option<i64> {
    let listview_color = listview.background();
    if row % 2 == 0 {
        return listview_color;
    }
    siblings
        .iter()
        .find(|control| {
            control.kind == ControlKind::ListViewItem
                && control.row == Some(row)
                && control.parent == Some(listview.id)
        })
        .and_then(Control::background)
        .or(listview_color)
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

fn draw_treeview(
    ui: &mut egui::Ui,
    control: &Control,
    actions: &mut Vec<Action>,
    siblings: &[Control],
) {
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
                // What an item hangs under is a fact about the item control, so
                // a tree drawn on its own falls back to the row text.
                let label = if siblings.is_empty() {
                    indent(item)
                } else {
                    format!("{}{}", "    ".repeat(tree_depth(control.id, index, siblings)), item)
                };
                if ui.selectable_label(selected, label).clicked() {
                    actions.push(Action::Selected(index));
                }
            }
        });
}

/// The item control that stands for row `row` of the tree `tree`.
fn tree_item(tree: i64, row: usize, siblings: &[Control]) -> Option<&Control> {
    siblings.iter().find(|control| {
        control.kind == ControlKind::TreeViewItem
            && control.row == Some(row)
            && tree_owns(tree, control, siblings)
    })
}

/// Whether `item` is part of the tree `tree`.
fn tree_owns(tree: i64, item: &Control, siblings: &[Control]) -> bool {
    let mut parent = item.parent;
    while let Some(id) = parent {
        if id == tree {
            return true;
        }
        match siblings.iter().find(|control| control.id == id) {
            Some(control) => parent = control.parent,
            None => return false,
        }
    }
    false
}

/// How deeply a tree row hangs, counted through the item controls.
fn tree_depth(tree: i64, row: usize, siblings: &[Control]) -> usize {
    let Some(item) = tree_item(tree, row, siblings) else {
        return 0;
    };
    let mut depth = 0;
    let mut parent = item.parent;
    while let Some(id) = parent {
        match siblings.iter().find(|control| control.id == id) {
            Some(control) if control.kind == ControlKind::TreeViewItem => {
                depth += 1;
                parent = control.parent;
            }
            _ => break,
        }
    }
    depth
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
    // The help page is explicit about the order: "Due to design constraints RECT,
    // ELLIPSE and PIE graphics are drawn first", so the closed shapes get a pass
    // of their own and the colours are replayed in both.
    for closed_pass in [true, false] {
        let mut color = ui.visuals().text_color();
        let mut background: Option<Color32> = None;
        let mut width = 1.0f32;
        for command in &control.draw {
            match command {
                DrawCmd::SetColor(value) => color = autoit_color(*value),
                // `$GUI_GR_NOBKCOLOR` is negative, which is "do not fill".
                DrawCmd::SetBkColor(value) => {
                    background = (*value >= 0).then(|| autoit_color(*value));
                }
                DrawCmd::SetWidth(value) => width = (*value).max(1) as f32,
                DrawCmd::SetStyle(_) | DrawCmd::Clear => {}
                DrawCmd::Rect { x, y, w, h } if closed_pass => {
                    let shape = Rect::from_min_size(point(rect, *x, *y), vec2(*w as f32, *h as f32));
                    match background {
                        Some(fill) => {
                            painter.rect_filled(shape, CornerRadius::ZERO, fill);
                        }
                        None => {
                            painter.rect_stroke(
                                shape,
                                CornerRadius::ZERO,
                                Stroke::new(width, color),
                                StrokeKind::Inside,
                            );
                        }
                    }
                }
                DrawCmd::Ellipse { x, y, w, h } if closed_pass => {
                    let centre = point(rect, *x + *w / 2, *y + *h / 2);
                    let radius = (*w).min(*h).max(0) as f32 / 2.0;
                    match background {
                        Some(fill) => {
                            painter.circle_filled(centre, radius, fill);
                        }
                        None => {
                            painter.circle_stroke(centre, radius, Stroke::new(width, color));
                        }
                    }
                }
                DrawCmd::Pie {
                    x,
                    y,
                    r,
                    start,
                    sweep,
                } if closed_pass => {
                    let centre = point(rect, *x, *y);
                    let steps = ((*sweep).unsigned_abs() / 6).clamp(2, 90) as usize;
                    let mut points = Vec::with_capacity(steps + 2);
                    points.push(centre);
                    for step in 0..=steps {
                        let angle = (*start as f32
                            + *sweep as f32 * step as f32 / steps as f32)
                            .to_radians();
                        // Screen `y` grows downwards, and `$GUI_GR_PIE`'s angles
                        // count upwards from the positive x axis.
                        points.push(egui::pos2(
                            centre.x + *r as f32 * angle.cos(),
                            centre.y - *r as f32 * angle.sin(),
                        ));
                    }
                    painter.add(egui::epaint::PathShape::convex_polygon(
                        points,
                        background.unwrap_or(Color32::TRANSPARENT),
                        Stroke::new(if background.is_some() { 0.0 } else { width }, color),
                    ));
                }
                DrawCmd::Line { x1, y1, x2, y2 } if !closed_pass => {
                    painter.line_segment(
                        [point(rect, *x1, *y1), point(rect, *x2, *y2)],
                        Stroke::new(width, color),
                    );
                }
                DrawCmd::Bezier {
                    x1,
                    y1,
                    x2,
                    y2,
                    x3,
                    y3,
                    x4,
                    y4,
                } if !closed_pass => {
                    painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                        [
                            point(rect, *x1, *y1),
                            point(rect, *x2, *y2),
                            point(rect, *x3, *y3),
                            point(rect, *x4, *y4),
                        ],
                        false,
                        Color32::TRANSPARENT,
                        Stroke::new(width, color),
                    ));
                }
                DrawCmd::Dot { x, y } if !closed_pass => {
                    let size = vec2(width.max(1.0), width.max(1.0));
                    painter.rect_filled(
                        Rect::from_min_size(point(rect, *x, *y), size),
                        CornerRadius::ZERO,
                        color,
                    );
                }
                DrawCmd::Text { x, y, text } if !closed_pass => {
                    painter.text(
                        point(rect, *x, *y),
                        Align2::LEFT_TOP,
                        text,
                        FontId::proportional(12.0),
                        color,
                    );
                }
                _ => {}
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

// Unit tests live in `tests/unit/` so this file reads as implementation;
// `#[path]` pulls them back in as a test module, which is what keeps their
// access to the private state below.
#[cfg(test)]
#[path = "../tests/unit/widgets.rs"]
mod tests;
