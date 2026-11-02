//! A dropdown's rows under two levels of headers (e.g. chapter, then shot) that stay at the top
//! while their rows scroll under them, in the dropdown's own scroll area.

use eframe::egui;

use crate::theme;

const HEADER: f32 = 22.0;

/// `rows` are (top header, second header, row), already in order; a header shows where it
/// changes. `tail` goes after the rows, e.g. Show all. Returns the popup to hand to `remeasure`
/// once the dropdown's drawn, when it's grown out of its size.
pub(crate) fn list<T>(
    ui: &mut egui::Ui,
    rows: &[(String, String, T)],
    mut row: impl FnMut(&mut egui::Ui, &T),
    tail: impl FnOnce(&mut egui::Ui),
) -> Option<egui::LayerId> {
    // Pinned under whatever comes before the rows, or at the top once that's scrolled away.
    let top = ui.clip_rect().top().max(ui.cursor().top());
    // Where each header was drawn, and what it says.
    let mut tops: Vec<(f32, String)> = vec![];
    let mut seconds: Vec<(f32, String)> = vec![];
    let mut last: Option<(&str, &str)> = None;
    for (a, b, t) in rows {
        if last != Some((a, b)) {
            if last.map(|l| l.0) != Some(a) {
                tops.push((header(ui, a, 0).top(), a.clone()));
            }
            seconds.push((header(ui, b, 1).top(), b.clone()));
            last = Some((a, b));
        }
        row(ui, t);
    }
    tail(ui);
    pin(ui, top, &tops, &seconds);
    // A popup keeps the size it had, so a list that just got longer (Show all) would stay as
    // short as it was; it's measured again once it's drawn, unless it's past MAX and scrolls.
    let all = ui.min_rect();
    (all.height() < MAX - 1.0 && all.bottom() > ui.clip_rect().bottom() + 0.5)
        .then(|| ui.layer_id())
}

/// The height to give the dropdown it's in: it scrolls past this.
pub(crate) const MAX: f32 = 360.0;

/// A row's button, its text to the left like a list's.
pub(crate) fn item<'a>(selected: bool, text: impl Into<egui::WidgetText>) -> egui::Button<'a> {
    egui::Button::selectable(
        selected,
        (egui::Atom::from(text.into()), egui::Atom::grow()),
    )
    .truncate()
}

/// A picture's row: its thumbnail, whole and centred on black, then its name. True when picked.
pub(crate) fn picture(
    ui: &mut egui::Ui,
    thumbs: &mut crate::Thumbs,
    path: &std::path::Path,
    name: &str,
    hover: &str,
) -> bool {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(44.0, 28.0), egui::Sense::hover());
        letterbox(ui, thumbs, path, name, rect);
        ui.add(item(false, name).min_size(egui::vec2(ui.available_width(), 28.0)))
            .on_hover_text(hover)
            .clicked()
    })
    .inner
}

/// A picture fitted into `rect` without stretching, the rest of it black.
pub(crate) fn letterbox(
    ui: &egui::Ui,
    thumbs: &mut crate::Thumbs,
    path: &std::path::Path,
    name: &str,
    rect: egui::Rect,
) {
    ui.painter().rect_filled(rect, 2.0, egui::Color32::BLACK);
    if let crate::Big::Ready(t) = thumbs.texture_later(ui, path, crate::THUMB, name) {
        let size = t.size_vec2();
        let k = (rect.width() / size.x).min(rect.height() / size.y);
        let fit = egui::Rect::from_center_size(rect.center(), size * k);
        ui.painter().image(
            t.id(),
            fit,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    }
}

fn header(ui: &mut egui::Ui, text: &str, level: u8) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), HEADER),
        egui::Sense::hover(),
    );
    paint(ui.painter(), ui, rect, text, level);
    rect
}

/// A header, with a strip of the popup's own colour along its top and bottom: headers stacked or
/// pushing each other read as apart, and rows sliding past go out of sight behind it.
fn paint(p: &egui::Painter, ui: &egui::Ui, rect: egui::Rect, text: &str, level: u8) {
    p.rect_filled(rect, 0.0, theme::RAISED);
    let gap = ui.visuals().window_fill;
    for y in [rect.top(), rect.bottom() - 2.0] {
        p.rect_filled(
            egui::Rect::from_min_size(egui::pos2(rect.left(), y), egui::vec2(rect.width(), 2.0)),
            0.0,
            gap,
        );
    }
    let (color, indent) = if level == 0 {
        (theme::PAPER, 6.0)
    } else {
        (theme::GRAPHITE, 16.0)
    };
    p.text(
        egui::pos2(rect.left() + indent, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        egui::TextStyle::Small.resolve(ui.style()),
        color,
    );
}

/// Pins the headers of what's scrolled under the top: the group's, and under it the subgroup's.
/// The next subgroup's header pushes the pinned one up; the next group's pushes both, as one
/// stack. Each pinned header covers the rows under it and takes their clicks.
fn pin(ui: &egui::Ui, top: f32, tops: &[(f32, String)], seconds: &[(f32, String)]) {
    let Some(c) = tops.iter().rposition(|(y, _)| *y < top) else {
        return;
    };
    let next_group = tops.get(c + 1).map(|(y, _)| *y);
    let sub = seconds
        .iter()
        .rposition(|(y, _)| *y < top + HEADER)
        // Only one of this group's, not the last of the group before.
        .filter(|&s| seconds[s].0 >= tops[c].0);
    let mut y1 = top + HEADER;
    if let Some(s) = sub {
        if let Some((n, _)) = seconds.get(s + 1) {
            y1 = y1.min(n - HEADER);
        }
        if let Some(n) = next_group {
            y1 = y1.min(n - HEADER);
        }
    }
    let mut y0 = top;
    if let Some(n) = next_group {
        y0 = y0.min(n - HEADER - if sub.is_some() { HEADER } else { 0.0 });
    }
    // The list's own width: a scroll area's clip spans the whole window sideways.
    let list = ui.min_rect();
    let band = |y: f32| {
        egui::Rect::from_min_max(
            egui::pos2(list.left(), y),
            egui::pos2(list.right(), y + HEADER),
        )
    };
    let clip = ui.clip_rect();
    // The subgroup's first, so as it's pushed up it slides under the group's.
    if let Some(s) = sub {
        let room = egui::Rect::from_min_max(
            egui::pos2(list.left(), y0 + HEADER),
            egui::pos2(list.right(), clip.bottom()),
        )
        .intersect(clip);
        let rect = band(y1);
        let _ = ui.interact(
            rect.intersect(room),
            ui.scope_id().with(("pinned", 1)),
            egui::Sense::click(),
        );
        paint(
            &ui.painter().with_clip_rect(room),
            ui,
            rect,
            &seconds[s].1,
            1,
        );
    }
    let rect = band(y0);
    let room = clip.intersect(egui::Rect::from_min_max(
        egui::pos2(list.left(), top),
        egui::pos2(list.right(), clip.bottom()),
    ));
    let _ = ui.interact(
        rect.intersect(room),
        ui.scope_id().with(("pinned", 0)),
        egui::Sense::click(),
    );
    paint(&ui.painter().with_clip_rect(room), ui, rect, &tops[c].1, 0);
}

fn pending() -> egui::Id {
    egui::Id::NULL.with("grouped remeasure")
}

/// Notes a popup that grew, from inside it.
pub(crate) fn remeasure_later(ui: &egui::Ui, popup: Option<egui::LayerId>) {
    if let Some(layer) = popup {
        ui.data_mut(|d| d.insert_temp(pending(), layer));
    }
}

/// Has a popup that grew size itself again next frame. Called once it's drawn, since it saves
/// its size then.
pub(crate) fn remeasure(ctx: &egui::Context) {
    let layer = ctx.data_mut(|d| {
        let l = d.get_temp::<egui::LayerId>(pending());
        d.remove::<egui::LayerId>(pending());
        l
    });
    if let Some(layer) = layer
        && let Some(mut state) = egui::AreaState::load(ctx, layer.id)
    {
        state.size = None;
        ctx.memory_mut(|m| m.areas_mut().set_state(layer, state));
        ctx.request_repaint();
    }
}
