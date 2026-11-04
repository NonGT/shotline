//! Browse existing: a dropdown of pictures the film already has, anchors and every image made
//! before, grouped by what they were for. A made one can be thrown out from it.

use std::path::PathBuf;

use eframe::egui;
use shotline_core::{MadeImage, ShotRef};

use crate::{App, grouped, short};

pub(crate) struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub hover: String,
    /// The job that made it, for one that can be thrown out.
    pub made: Option<String>,
    /// Where it's used, which keeps it from being thrown out.
    pub used: Option<String>,
}

pub(crate) enum Browsed {
    Pick(PathBuf),
    /// By job id.
    Forget(String),
}

/// What the dropdown is open for: its own made images come first.
#[derive(Clone, PartialEq)]
pub(crate) enum Near {
    Shot(ShotRef),
    Element(String),
}

pub(crate) const LABEL: &str = "Browse existing";
pub(crate) const MINE: &str = "Made for this one";

/// The dropdown, `name`d for screen readers. `salt` keeps two on one page apart.
pub(crate) fn dropdown(
    ui: &mut egui::Ui,
    name: &str,
    salt: impl std::hash::Hash + std::fmt::Debug,
    rows: &[(String, String, Entry)],
    thumbs: &mut crate::Thumbs,
    hover: &str,
) -> Option<Browsed> {
    let mut out = None;
    ui.add_enabled_ui(!rows.is_empty(), |ui| {
        let open = egui::ComboBox::from_id_salt(("browse", salt))
            .selected_text(LABEL)
            .width(170.0)
            .height(grouped::MAX)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show_ui(ui, |ui| {
                ui.set_width(360.0);
                let grown = grouped::list(
                    ui,
                    rows,
                    |ui, e| {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(44.0, 28.0), egui::Sense::hover());
                            grouped::letterbox(ui, thumbs, &e.path, &e.name, rect);
                            // Throw out at the right edge; the name takes what's left.
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if let Some(id) = &e.made {
                                        let rm = ui
                                            .add_enabled_ui(e.used.is_none(), |ui| {
                                                crate::delete_button(
                                                    ui,
                                                    &format!("Throw out {}", e.name),
                                                )
                                            })
                                            .inner;
                                        if let Some(at) = &e.used {
                                            ui.label(
                                                egui::RichText::new("in use")
                                                    .small()
                                                    .color(crate::theme::DUST),
                                            )
                                            .on_hover_text(format!("it's {at}"));
                                        }
                                        if rm
                                            .on_hover_text(
                                                "off this list; what it cost stays in the ledger",
                                            )
                                            .on_disabled_hover_text(format!(
                                                "it's {}; take it off there first",
                                                e.used.as_deref().unwrap_or_default()
                                            ))
                                            .clicked()
                                        {
                                            out = Some(Browsed::Forget(id.clone()));
                                        }
                                    }
                                    let room = ui.available_width();
                                    ui.with_layout(
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            if ui
                                                .add(
                                                    grouped::item(false, e.name.as_str())
                                                        .min_size(egui::vec2(room, 28.0)),
                                                )
                                                .on_hover_text(&e.hover)
                                                .clicked()
                                            {
                                                out = Some(Browsed::Pick(e.path.clone()));
                                            }
                                        },
                                    );
                                },
                            );
                        });
                    },
                    |_| {},
                );
                grouped::remeasure_later(ui, grown);
                if matches!(out, Some(Browsed::Pick(_))) {
                    ui.close();
                }
            });
        grouped::remeasure(ui.ctx());
        open.response
            .on_hover_text(hover)
            .on_disabled_hover_text("nothing to pick from yet")
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, name));
    });
    out
}

impl App {
    /// Every made image, those for `near` first, each group newest first.
    pub(crate) fn made_rows(&self, near: &Near) -> Vec<(String, String, Entry)> {
        let names = self.shot_names();
        let elements: Vec<(String, String)> = self
            .film
            .elements
            .iter()
            .map(|(e, _)| (e.handle.clone(), e.name.clone()))
            .collect();
        let group = |m: &MadeImage| -> (u8, String) {
            let mine = match near {
                Near::Shot(r) => m.shot == Some(*r),
                Near::Element(h) => m.element.as_deref() == Some(h.as_str()),
            };
            let name = if let Some(h) = &m.element {
                let full = elements
                    .iter()
                    .find(|(x, _)| x == h)
                    .map_or(String::new(), |(_, n)| n.clone());
                format!("@{h}  {full}")
            } else if let Some(r) = m.shot {
                names
                    .get(&r)
                    .map_or_else(|| r.to_string(), |(_, s)| s.clone())
            } else {
                "Not made for anything in particular".into()
            };
            (if mine { 0 } else { 1 }, name)
        };
        let mut rows: Vec<((u8, String), &MadeImage)> =
            self.film.images.iter().map(|m| (group(m), m)).collect();
        // Stable: newest first within each group, groups in the order first seen.
        let mut order: Vec<(u8, String)> = vec![];
        for (g, _) in &rows {
            if !order.contains(g) {
                order.push(g.clone());
            }
        }
        rows.sort_by_key(|(g, _)| (g.0, order.iter().position(|o| o == g)));
        rows.into_iter()
            .map(|((mine, name), m)| {
                let top = if mine == 0 { MINE } else { "Made for others" };
                let when = m
                    .job
                    .created
                    .to_zoned(jiff::tz::TimeZone::system())
                    .strftime("%b %-d %H:%M")
                    .to_string();
                let about = m
                    .job
                    .brief
                    .as_deref()
                    .or(m.job.prompt.as_deref())
                    .unwrap_or("");
                let label = if about.is_empty() {
                    when.clone()
                } else {
                    format!("{when}  {}", short(about, 40))
                };
                (
                    top.to_string(),
                    name,
                    Entry {
                        name: label,
                        path: m.path.clone(),
                        hover: format!("{} · {}\n{about}", m.job.plugin, m.job.model),
                        made: Some(m.job.id.to_string()),
                        used: self.film.in_use.get(&m.path).cloned(),
                    },
                )
            })
            .collect()
    }
}
