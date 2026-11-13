//! The pieces every page is built from, so they look and behave the same everywhere: icon
//! buttons, badges, cards, the danger button and the running ring.

use eframe::egui::{self, Color32, CornerRadius, Response, RichText, Sense, Stroke};

use crate::theme;

/// A square button showing only an icon, lit on hover. `label` is what it's called, for screen
/// readers and tests; callers say more on hover.
pub(crate) fn icon_button(ui: &mut egui::Ui, icon: &str, label: &str) -> Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(32.0, 32.0), Sense::click());
    let enabled = ui.is_enabled();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    if ui.is_rect_visible(rect) {
        let lit = enabled && (r.hovered() || r.is_pointer_button_down_on());
        if lit {
            ui.painter()
                .rect_filled(rect, theme::R_CONTROL, theme::RAISED);
        }
        let color = match (enabled, lit) {
            (false, _) => theme::FAINT,
            (true, true) => theme::PAPER,
            (true, false) => theme::DUST,
        };
        glyph(ui.painter(), rect.center(), icon, 18.0, color);
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A quiet button of an icon and a few words, for bars: no fill until hovered.
pub(crate) fn ghost_button(ui: &mut egui::Ui, icon: &str, text: &str) -> Response {
    let font = egui::FontId::proportional(12.5);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font, theme::GRAPHITE);
    let size = egui::vec2(galley.size().x + 40.0, 26.0);
    let (rect, r) = ui.allocate_exact_size(size, Sense::click());
    let enabled = ui.is_enabled();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, text));
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if enabled && r.hovered() {
            p.rect_filled(rect, theme::R_CONTROL, theme::RAISED);
        }
        let color = if enabled {
            theme::GRAPHITE
        } else {
            theme::FAINT
        };
        glyph(
            p,
            egui::pos2(rect.left() + 16.0, rect.center().y),
            icon,
            14.0,
            color,
        );
        p.galley(
            egui::pos2(rect.left() + 30.0, rect.center().y - galley.size().y / 2.0),
            galley,
            color,
        );
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// An icon centred on `at`.
pub(crate) fn glyph(p: &egui::Painter, at: egui::Pos2, icon: &str, size: f32, color: Color32) {
    p.text(
        at,
        egui::Align2::CENTER_CENTER,
        icon,
        theme::icon(size),
        color,
    );
}

/// A count on a rounded tag, centred on `at`.
pub(crate) fn badge(p: &egui::Painter, at: egui::Pos2, n: usize, fill: Color32) -> egui::Rect {
    let font = egui::FontId::new(11.0, egui::FontFamily::Name(theme::SEMIBOLD.into()));
    let text = p.layout_no_wrap(n.to_string(), font, theme::RACK);
    let size = egui::vec2((text.size().x + 10.0).max(18.0), 18.0);
    let rect = egui::Rect::from_center_size(at, size);
    p.rect_filled(rect, 9.0, fill);
    p.galley(rect.center() - text.size() / 2.0, text, theme::RACK);
    rect
}

/// A word on a tinted pill, like "recorded" or "out of date".
/// `width` keeps a column of them lined up.
pub(crate) fn chip(ui: &mut egui::Ui, text: &str, color: Color32, width: f32) -> Response {
    let font = egui::FontId::proportional(11.5);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
    let size = egui::vec2((galley.size().x + 14.0).max(width), 20.0);
    let (rect, r) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 10.0, color.gamma_multiply(0.14));
    p.galley(rect.center() - galley.size() / 2.0, galley, color);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, text));
    r
}

/// Part of a ring going round, for something running. `t` is the time in seconds.
pub(crate) fn ring(p: &egui::Painter, c: egui::Pos2, r: f32, width: f32, color: Color32, t: f64) {
    p.circle_stroke(c, r, Stroke::new(width, theme::EDGE));
    let start = (t * 5.0) as f32;
    let arc: Vec<egui::Pos2> = (0..=20)
        .map(|i| {
            let a = start + i as f32 / 20.0 * std::f32::consts::PI * 1.3;
            c + r * egui::vec2(a.cos(), a.sin())
        })
        .collect();
    p.add(egui::Shape::line(arc, Stroke::new(width, color)));
}

/// A card: a rounded panel a step lighter than the page.
pub(crate) fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(theme::CARD)
        .stroke(Stroke::new(1.0, theme::LINE))
        .corner_radius(CornerRadius::same(theme::R_CARD))
        .inner_margin(egui::Margin::same(theme::PAD as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// A card's heading, with something quiet on the right like "2 / 9".
pub(crate) fn card_title(ui: &mut egui::Ui, title: &str, meta: Option<&str>) -> Response {
    ui.horizontal(|ui| {
        let r = ui.label(
            RichText::new(title)
                .font(theme::section())
                .color(theme::PAPER),
        );
        if let Some(m) = meta {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(m).small().color(theme::FAINT));
            });
        }
        r
    })
    .inner
}

/// An icon then words, for a label or a button. Icons can't go in plain text: Inter has its own
/// glyphs at many of Phosphor's code points.
pub(crate) fn with_icon(
    icon: &str,
    text: &str,
    family: egui::FontFamily,
    size: f32,
    color: Color32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let fmt = |font| egui::TextFormat {
        font_id: font,
        color,
        valign: egui::Align::Center,
        ..Default::default()
    };
    job.append(icon, 0.0, fmt(theme::icon(size + 1.0)));
    job.append(text, 7.0, fmt(egui::FontId::new(size, family)));
    job
}

/// A button of an icon and words; screen readers and tests get the words alone.
pub(crate) fn icon_text_button(ui: &mut egui::Ui, icon: &str, text: &str) -> Response {
    let color = ui.visuals().widgets.inactive.fg_stroke.color;
    let job = with_icon(icon, text, egui::FontFamily::Proportional, 13.5, color);
    let r = ui.add(egui::Button::new(job));
    let enabled = ui.is_enabled();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, text));
    r
}

/// The main thing to do here that doesn't spend, like Keep or Render: paper, dark text.
pub(crate) fn primary_button(ui: &mut egui::Ui, icon: Option<&str>, text: &str) -> Response {
    let enabled = ui.is_enabled();
    let color = if enabled { theme::RACK } else { theme::FAINT };
    let semibold = egui::FontFamily::Name(theme::SEMIBOLD.into());
    let rich: egui::WidgetText = match icon {
        Some(i) => with_icon(i, text, semibold, 13.5, color).into(),
        None => RichText::new(text).family(semibold).color(color).into(),
    };
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        for (state, f) in [
            (&mut w.inactive, theme::PAPER),
            (&mut w.hovered, Color32::from_rgb(0xf7, 0xf4, 0xec)),
            (&mut w.active, Color32::from_rgb(0xd8, 0xd4, 0xca)),
        ] {
            state.weak_bg_fill = f;
            state.bg_fill = f;
            state.bg_stroke = Stroke::NONE;
        }
        let b = egui::Button::new(rich).min_size(egui::vec2(0.0, 32.0));
        let r = ui.add(if enabled { b } else { b.fill(theme::RAISED) });
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, text));
        r
    })
    .inner
}

/// A button that removes or throws away: red text, no fill until hovered.
pub(crate) fn danger_button(ui: &mut egui::Ui, text: &str) -> Response {
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = Stroke::new(1.0, theme::EDGE);
        w.hovered.weak_bg_fill = Color32::from_rgb(0x3a, 0x2a, 0x28);
        w.hovered.bg_stroke = Stroke::new(1.0, theme::FLAG);
        ui.add(egui::Button::new(RichText::new(text).color(theme::FLAG)))
    })
    .inner
}

/// A page's heading: what it is big, and above it something smaller like the film's name.
pub(crate) fn page_title(ui: &mut egui::Ui, title: &str, over: Option<&str>) {
    if let Some(o) = over {
        ui.label(RichText::new(o).small().color(theme::DUST));
    }
    ui.label(
        RichText::new(title)
            .font(theme::title())
            .color(theme::PAPER),
    );
}

/// What a page shows while there's nothing on it yet: an icon and a line, in the middle.
pub(crate) fn empty(ui: &mut egui::Ui, icon: &str, text: &str) {
    let h = ui.available_height().max(240.0);
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), h),
        egui::Layout::centered_and_justified(egui::Direction::TopDown),
        |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((h / 2.0 - 60.0).max(0.0));
                let (rect, _) = ui.allocate_exact_size(egui::vec2(56.0, 56.0), Sense::hover());
                ui.painter().rect_filled(rect, theme::R_CARD, theme::CARD);
                glyph(ui.painter(), rect.center(), icon, 26.0, theme::FAINT);
                ui.add_space(12.0);
                ui.label(RichText::new(text).color(theme::DUST));
            });
        },
    );
}

/// A popup's width: `share` of the window between `min` and `max`, and never wider than the
/// window leaves room for.
pub(crate) fn modal_width(ctx: &egui::Context, share: f32, min: f32, max: f32) -> f32 {
    let window = ctx.content_rect().width();
    // The popup's own margins, and a gap to the window's edge.
    let room = window - 2.0 * 20.0 - 2.0 * 24.0;
    (window * share).clamp(min, max).min(room).max(200.0)
}

/// A button that opens a menu: its words and a small down triangle.
pub(crate) fn drop_button(
    ui: &mut egui::Ui,
    text: impl Into<egui::WidgetText>,
    enabled: bool,
) -> Response {
    let text: egui::WidgetText = text.into();
    let name = text.text().to_string();
    let r = ui.add_enabled(enabled, egui::Button::new(text).right_text("   "));
    let c = egui::pos2(r.rect.right() - 13.0, r.rect.center().y);
    let color = if enabled {
        ui.visuals().widgets.inactive.fg_stroke.color
    } else {
        ui.visuals().widgets.noninteractive.fg_stroke.color
    };
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            c + egui::vec2(-4.0, -2.0),
            c + egui::vec2(4.0, -2.0),
            c + egui::vec2(0.0, 3.0),
        ],
        color,
        egui::Stroke::NONE,
    ));
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, &name));
    r
}
