//! Browse existing: a dropdown of pictures the film already has, anchors and every image made
//! before, grouped by what they were for, and the last frames of takes. A made one can be thrown
//! out from it.

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
    /// Fits what it's picked for; the rest wait behind Show all.
    pub near: bool,
}

pub(crate) enum Browsed {
    Pick(PathBuf),
    /// By job id.
    Forget(String),
}

/// What the dropdown is open for: its own made images come first.
#[derive(Clone, PartialEq)]
pub(crate) enum Near {
    /// A picture for the shot to send along.
    Shot(ShotRef),
    /// The shot's storyboard frame.
    Frame(ShotRef),
    Element(String),
}

pub(crate) const LABEL: &str = "Browse existing";
pub(crate) const MINE: &str = "Made for this one";
pub(crate) const TAKES: &str = "Takes";

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
    let all_id = egui::Id::unique(("browse all", &salt));
    let all = ui.data(|d| d.get_temp::<bool>(all_id).unwrap_or(false));
    let far = rows.iter().filter(|(_, _, e)| !e.near).count();
    let shown: Vec<&(String, String, Entry)> =
        rows.iter().filter(|(_, _, e)| e.near || all).collect();
    let mut more = false;
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
                    &shown
                        .iter()
                        .map(|(a, b, e)| (a.clone(), b.clone(), e))
                        .collect::<Vec<_>>(),
                    |ui, e: &&Entry| {
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
                    |ui| {
                        if !all && far > 0 {
                            more = ui
                                .add(
                                    grouped::item(false, format!("Show all ({far} more)"))
                                        .min_size(egui::vec2(ui.available_width(), 0.0)),
                                )
                                .clicked();
                        }
                    },
                );
                grouped::remeasure_later(ui, grown);
                if matches!(out, Some(Browsed::Pick(_))) {
                    ui.close();
                }
            });
        grouped::remeasure(ui.ctx());
        if open.inner.is_none() && all {
            ui.data_mut(|d| d.insert_temp(all_id, false));
        }
        open.response
            .on_hover_text(hover)
            .on_disabled_hover_text("nothing to pick from yet")
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, name));
    });
    if more {
        ui.data_mut(|d| d.insert_temp(all_id, true));
    }
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
        // Frames made before jobs said so are known by being some shot's frame.
        let is_frame = |m: &MadeImage| {
            m.job.frame
                || self
                    .film
                    .in_use
                    .get(&m.path)
                    .is_some_and(|u| u.starts_with("the storyboard frame"))
        };
        let prev = match near {
            Near::Frame(r) => self.project.previous_shot(*r).ok().flatten(),
            _ => None,
        };
        let group = |m: &MadeImage| -> (u8, String) {
            let mine = match near {
                Near::Shot(r) => m.shot == Some(*r) && !is_frame(m),
                Near::Frame(r) => m.shot == Some(*r) && is_frame(m),
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
                        near: matches!(near, Near::Frame(_)) == is_frame(m),
                    },
                )
            })
            .chain(self.film.last_frames.iter().map(|(t, f)| {
                let shot = names
                    .get(&t.shot)
                    .map_or_else(|| t.shot.to_string(), |(_, s)| s.clone());
                (
                    TAKES.to_string(),
                    shot,
                    Entry {
                        name: format!("Last frame of {t}"),
                        path: f.clone(),
                        hover: format!("the last frame of {t}'s video"),
                        made: None,
                        used: self.film.in_use.get(f).cloned(),
                        near: prev.is_some_and(|p| p == t.shot),
                    },
                )
            }))
            .collect()
    }
}
