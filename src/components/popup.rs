use egui::{Align, Area, Frame, Id, Key, Layout, NumExt, Order, Rect, Response, Ui, Vec2};

#[derive(Clone, Default)]
struct State {
    size: Vec2,
}

/// Like [`egui::popup_below_widget`], but pops up to the left, so that the popup doesn't go off the screen
pub(crate) fn popup_under_widget<R>(
    ui: &Ui,
    popup_id: Id,
    widget_response: &Response,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    if ui.memory().is_popup_open(popup_id) {
        let state: Option<State> = ui.data().get_temp(popup_id);

        // If this is the first draw, we don't know the popup size yet, so we don't know how to
        // position the popup
        if state.is_none() {
            ui.ctx().request_repaint();
        }

        let mut state = state.unwrap_or_default();

        let rect = Rect {
            min: widget_response.rect.left_bottom(),
            max: widget_response.rect.left_bottom() + state.size,
        };

        let inner = Area::new(popup_id)
            .order(Order::Foreground)
            .fixed_pos(constrain_window_rect_to_area(ui.ctx(), rect, None).min)
            .movable(true)
            .show(ui.ctx(), |ui| {
                // Note: we use a separate clip-rect for this area, so the popup can be outside the parent.
                // See https://github.com/emilk/egui/issues/825
                let frame = Frame::popup(ui.style());
                let frame_margin = frame.inner_margin + frame.outer_margin;
                let result = frame
                    .show(ui, |ui| {
                        ui.with_layout(Layout::top_down_justified(Align::LEFT), |ui| {
                            ui.set_width(widget_response.rect.width() - frame_margin.sum().x);
                            add_contents(ui)
                        })
                        .inner
                    })
                    .inner;

                state.size = ui.min_rect().size();

                result
            })
            .inner;

        *ui.data().get_temp_mut_or_default(popup_id) = state;

        if ui.input().key_pressed(Key::Escape) || widget_response.clicked_elsewhere() {
            ui.memory().close_popup();
        }
        Some(inner)
    } else {
        None
    }
}

/// Constrain the position of a window/area so it fits within the provided boundary.
///
/// If area is `None`, will constrain to [`ctx::available_rect`].
pub(crate) fn constrain_window_rect_to_area(
    ctx: &egui::Context,
    window: Rect,
    area: Option<Rect>,
) -> Rect {
    let mut area = area.unwrap_or_else(|| ctx.available_rect());

    if window.width() > area.width() {
        // Allow overlapping side bars.
        // This is important for small screens, e.g. mobiles running the web demo.
        area.max.x = ctx.input().screen_rect().max.x;
        area.min.x = ctx.input().screen_rect().min.x;
    }
    if window.height() > area.height() {
        // Allow overlapping top/bottom bars:
        area.max.y = ctx.input().screen_rect().max.y;
        area.min.y = ctx.input().screen_rect().min.y;
    }

    let mut pos = window.min;

    // Constrain to screen, unless window is too large to fit:
    let margin_x = (window.width() - area.width()).at_least(0.0);
    let margin_y = (window.height() - area.height()).at_least(0.0);

    pos.x = pos.x.at_most(area.right() + margin_x - window.width()); // move left if needed
    pos.x = pos.x.at_least(area.left() - margin_x); // move right if needed
    pos.y = pos.y.at_most(area.bottom() + margin_y - window.height()); // move right if needed
    pos.y = pos.y.at_least(area.top() - margin_y); // move down if needed

    // pos = ctx.round_pos_to_pixels(pos);

    Rect::from_min_size(pos, window.size())
}
