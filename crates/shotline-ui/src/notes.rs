//! What the app has to say: short toasts that go by themselves, errors that stay until closed,
//! and the Activity list of everything said since the film was opened.

use std::collections::VecDeque;

use eframe::egui::{self, RichText};

use crate::{icon, kit, theme};

/// How long a toast that isn't an error stays up.
const SHOWN: f64 = 4.0;
const KEPT: usize = 60;
const TOASTS: usize = 3;

/// Where a dialog leaves the frame it was last drawn in.
pub(crate) fn modal_mark() -> egui::Id {
    egui::Id::unique("modal shown")
}

#[derive(Clone)]
pub(crate) struct Note {
    pub msg: String,
    pub err: bool,
    pub at: jiff::Zoned,
    /// When it was first drawn, in egui's time; None until then.
    shown: Option<f64>,
}

#[derive(Default)]
pub(crate) struct Notes {
    toasts: Vec<Note>,
    pub activity: VecDeque<Note>,
}

impl Notes {
    pub fn say(&mut self, msg: String, err: bool) {
        // Some errors come back every frame until something changes.
        if let Some(last) = self.toasts.last_mut()
            && last.msg == msg
            && last.err == err
        {
            last.shown = None;
            return;
        }
        let note = Note {
            msg,
            err,
            at: jiff::Zoned::now(),
            shown: None,
        };
        if self.activity.front().is_none_or(|n| n.msg != note.msg) {
            self.activity.push_front(note.clone());
            self.activity.truncate(KEPT);
        }
        self.toasts.push(note);
        let over = self.toasts.len().saturating_sub(TOASTS);
        self.toasts.drain(..over);
    }

    pub fn clear(&mut self) {
        self.toasts.clear();
    }

    /// Toasts stacked up from the bottom right, over the bar. Under a dialog while one is up,
    /// as they'd cover its buttons.
    pub fn toasts(&mut self, ctx: &egui::Context) {
        let frame = ctx.cumulative_frame_nr();
        let modal = ctx
            .data(|d| d.get_temp::<u64>(modal_mark()))
            .is_some_and(|f| f + 1 >= frame);
        let now = ctx.input(|i| i.time);
        self.toasts
            .retain(|n| n.err || n.shown.is_none_or(|t| now - t < SHOWN));
        if self.toasts.is_empty() {
            return;
        }
        let mut close = None;
        egui::Area::new(egui::Id::unique("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -46.0))
            .order(if modal {
                egui::Order::Middle
            } else {
                egui::Order::Foreground
            })
            .interactable(true)
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                let width = (ctx.content_rect().width() - 32.0).clamp(200.0, 380.0);
                for (i, n) in self.toasts.iter_mut().enumerate() {
                    let t = *n.shown.get_or_insert(now);
                    if !n.err {
                        ctx.request_repaint_after(std::time::Duration::from_secs_f64(
                            (SHOWN - (now - t)).max(0.05),
                        ));
                    }
                    if toast(ui, n, width) {
                        close = Some(i);
                    }
                }
            });
        if let Some(i) = close {
            self.toasts.remove(i);
        }
    }
}

/// One toast; true when its × is clicked.
fn toast(ui: &mut egui::Ui, n: &Note, width: f32) -> bool {
    let (mark, color) = if n.err {
        (icon::WARNING, theme::FLAG)
    } else {
        (icon::CHECK, theme::GO)
    };
    let mut close = false;
    egui::Frame::new()
        .fill(theme::RAISED)
        .stroke(egui::Stroke::new(1.0, theme::EDGE))
        .corner_radius(theme::R_CARD)
        .inner_margin(egui::Margin {
            left: 12,
            right: 6,
            top: 8,
            bottom: 8,
        })
        .shadow(egui::Shadow {
            offset: [0, 4],
            blur: 16,
            spread: 0,
            color: egui::Color32::from_black_alpha(90),
        })
        .show(ui, |ui| {
            ui.set_width(width);
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
                kit::glyph(ui.painter(), r.center(), mark, 16.0, color);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = kit::icon_button(ui, icon::CLOSE, "Close the message")
                        .on_hover_text("Close")
                        .clicked();
                    if kit::icon_button(ui, icon::COPY, "Copy the message")
                        .on_hover_text("Copy")
                        .clicked()
                    {
                        ui.ctx().copy_text(n.msg.clone());
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let text = if n.err { theme::PAPER } else { theme::GRAPHITE };
                        ui.add(egui::Label::new(RichText::new(&n.msg).color(text)).wrap());
                    });
                });
            });
        });
    close
}

/// The bar's Activity button and its list, newest first.
pub(crate) fn activity_button(ui: &mut egui::Ui, notes: &Notes) {
    let errors = notes.activity.iter().take_while(|n| n.err).count();
    let r = ui.add_enabled_ui(!notes.activity.is_empty(), |ui| {
        kit::ghost_button(ui, icon::LIST, "Activity")
    });
    let r = r
        .inner
        .on_hover_text("what happened since the film was opened");
    if errors > 0 {
        let at = egui::pos2(r.rect.right() - 2.0, r.rect.top() + 6.0);
        kit::badge(ui.painter(), at, errors, theme::FLAG);
    }
    egui::Popup::menu(&r)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_width(420.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Activity")
                        .font(theme::section())
                        .color(theme::PAPER),
                );
            });
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .show(ui, |ui| {
                    for (i, n) in notes.activity.iter().enumerate() {
                        ui.push_id(i, |ui| activity_row(ui, n));
                    }
                });
        });
}

/// What was said, its mark and time, and Copy on hover.
fn activity_row(ui: &mut egui::Ui, n: &Note) {
    let (mark, color) = if n.err {
        (icon::WARNING, theme::FLAG)
    } else {
        (icon::CHECK, theme::GO)
    };
    let width = ui.available_width();
    let r = egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(6, 5))
        .corner_radius(theme::R_CONTROL)
        .show(ui, |ui| {
            ui.set_width(width - 12.0);
            ui.horizontal_top(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 18.0), egui::Sense::hover());
                kit::glyph(ui.painter(), r.center(), mark, 14.0, color);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    ui.add_sized(
                        [36.0, 18.0],
                        egui::Label::new(
                            RichText::new(n.at.strftime("%H:%M").to_string())
                                .small()
                                .color(theme::DUST),
                        ),
                    );
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                        let text = if n.err { theme::PAPER } else { theme::GRAPHITE };
                        ui.add(egui::Label::new(RichText::new(&n.msg).color(text)).wrap());
                    });
                });
            });
        })
        .response
        .interact(egui::Sense::click());
    if r.hovered() {
        ui.painter()
            .rect_filled(r.rect, theme::R_CONTROL, theme::RAISED.gamma_multiply(0.5));
    }
    if r.on_hover_text("click to copy").clicked() {
        ui.ctx().copy_text(n.msg.clone());
    }
}
