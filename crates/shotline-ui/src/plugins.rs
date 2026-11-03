//! The Plugins window: what's installed, where each one runs from, copies on PATH that are passed
//! over, and each model's options for the open film.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, RichText};
use serde_json::Value;
use shotline_core::protocol::ModelInfo;

use crate::{Host, PluginInfo, Runs, hint, theme};

/// What the window asks the shell for.
pub(crate) enum Ask {
    Keys,
    /// A model option for the film; None goes back to the default.
    Param {
        plugin: String,
        model: String,
        key: String,
        value: Option<toml::Value>,
    },
}

pub(crate) struct Plugins {
    list: Vec<PluginInfo>,
    problems: Vec<String>,
    /// What a text option holds while it's typed, by (plugin, model, key).
    typed: std::collections::HashMap<(String, String, String), String>,
    note: Option<(String, bool)>,
    /// The plugin list changed and the app should hear about it.
    pub changed: bool,
}

impl Plugins {
    pub fn new(host: Option<&Arc<dyn Host>>) -> Plugins {
        let (list, problems) = host.map(|h| h.plugins()).unwrap_or_default();
        Plugins {
            list,
            problems,
            typed: Default::default(),
            note: None,
            changed: false,
        }
    }

    pub fn descriptions(&self) -> Vec<shotline_core::protocol::Description> {
        self.list
            .iter()
            .filter_map(|p| p.description.clone().ok())
            .collect()
    }

    fn reload(&mut self, host: &Arc<dyn Host>, done: anyhow::Result<String>) {
        self.note = Some(match done {
            Ok(m) => (m, false),
            Err(e) => (e.to_string(), true),
        });
        (self.list, self.problems) = host.plugins();
        self.changed = true;
    }

    /// The window. `film` is the open film's options, if one is open. Returns whether it's
    /// still open and what it asks for.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        host: Option<Arc<dyn Host>>,
        film: Option<&shotline_core::Config>,
    ) -> (bool, Vec<Ask>) {
        let mut close = false;
        let mut asks = vec![];
        let mut pin: Option<(String, PathBuf)> = None;
        let mut unpin: Option<String> = None;
        let modal = egui::Modal::new(egui::Id::unique("plugins")).show(ui.ctx(), |ui| {
            ui.set_width(720.0);
            ui.heading("Plugins");
            ui.add_space(6.0);
            ui.label(
                RichText::new(
                    "Found in your plugins folder or on PATH; the first one on PATH wins. Keep one \
                     to use it whatever PATH says. Options are saved in this film and a shot's own \
                     win over them.",
                )
                .color(theme::DUST),
            );
            for p in &self.problems {
                ui.label(RichText::new(p).color(theme::FLAG));
            }
            ui.add_space(8.0);
            egui::ScrollArea::vertical()
                .max_height(560.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (i, p) in self.list.iter().enumerate() {
                        if i > 0 {
                            ui.add_space(10.0);
                            ui.separator();
                        }
                        ui.add_space(6.0);
                        plugin(
                            ui,
                            p,
                            film,
                            &mut self.typed,
                            &mut asks,
                            &mut pin,
                            &mut unpin,
                        );
                    }
                    if self.list.is_empty() {
                        hint(ui, "No plugins found.");
                    }
                });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if let Some((n, bad)) = &self.note {
                    let c = if *bad { theme::FLAG } else { theme::DUST };
                    ui.label(RichText::new(n).color(c));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = ui.button("Close").clicked();
                });
            });
        });
        if let Some(h) = &host {
            if let Some((name, exec)) = pin {
                let done = h.pin(&name, &exec);
                self.reload(h, done);
            }
            if let Some(name) = unpin {
                let done = h.unpin(&name);
                self.reload(h, done);
            }
        }
        (!(close || modal.should_close()), asks)
    }
}

/// One plugin: where it runs from, what's passed over, its keys and models.
fn plugin(
    ui: &mut egui::Ui,
    p: &PluginInfo,
    film: Option<&shotline_core::Config>,
    typed: &mut std::collections::HashMap<(String, String, String), String>,
    asks: &mut Vec<Ask>,
    pin: &mut Option<(String, PathBuf)>,
    unpin: &mut Option<String>,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(&p.name)
                .family(egui::FontFamily::Name(theme::SEMIBOLD.into()))
                .color(theme::PAPER),
        );
        if let Ok(d) = &p.description {
            ui.label(RichText::new(&d.version).small().color(theme::DUST));
        }
        if !p.missing.is_empty() {
            ui.label(RichText::new(format!("needs {}", p.missing.join(", "))).color(theme::FLAG));
            if ui.small_button("Keys…").clicked() {
                asks.push(Ask::Keys);
            }
        }
    });
    let (place, kept) = match &p.runs {
        Runs::Builtin => ("built into shotline".to_string(), false),
        Runs::Path(e) => (e.display().to_string(), false),
        Runs::Pinned(e) => (e.display().to_string(), true),
        Runs::Folder(d) => (d.display().to_string(), false),
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(&place).small().color(theme::GRAPHITE));
        if kept {
            ui.label(RichText::new("kept").small().color(theme::TUNGSTEN));
            if ui
                .small_button("Let PATH decide")
                .on_hover_text("takes away the saved choice in your plugins folder")
                .clicked()
            {
                *unpin = Some(p.name.clone());
            }
        } else if let Runs::Path(e) = &p.runs
            && ui
                .small_button("Keep this one")
                .on_hover_text(
                    "saves this choice in your plugins folder, so it's used whatever PATH says",
                )
                .clicked()
        {
            *pin = Some((p.name.clone(), e.clone()));
        }
    });
    if !p.skipped.is_empty() {
        ui.label(
            RichText::new("Also found, not used:")
                .small()
                .color(theme::TUNGSTEN),
        );
        for s in &p.skipped {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.label(
                    RichText::new(s.display().to_string())
                        .small()
                        .color(theme::DUST),
                );
                if ui
                    .small_button("Use this one")
                    .on_hover_text("keeps it in your plugins folder, so it wins over PATH")
                    .clicked()
                {
                    *pin = Some((p.name.clone(), s.clone()));
                }
            });
        }
    }
    let d = match &p.description {
        Ok(d) => d,
        Err(e) => {
            ui.label(RichText::new(format!("doesn't answer: {e}")).color(theme::FLAG));
            return;
        }
    };
    ui.add_space(4.0);
    for m in &d.models {
        model(ui, &p.name, m, film, typed, asks);
    }
}

/// A model, and its options when there's a film to set them in.
fn model(
    ui: &mut egui::Ui,
    plugin: &str,
    m: &ModelInfo,
    film: Option<&shotline_core::Config>,
    typed: &mut std::collections::HashMap<(String, String, String), String>,
    asks: &mut Vec<Ask>,
) {
    let props = m
        .params_schema
        .as_ref()
        .and_then(|s| s.get("properties"))
        .and_then(Value::as_object);
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(RichText::new(&m.id).color(theme::GRAPHITE));
        ui.label(RichText::new(m.op.to_string()).small().color(theme::DUST));
    });
    let Some(props) = props.filter(|p| !p.is_empty()) else {
        return;
    };
    let Some(cfg) = film else {
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            hint(ui, "open a film to set its options");
        });
        return;
    };
    let set = cfg.params.get(plugin).and_then(|x| x.get(&m.id));
    egui::Grid::new(("options", plugin, &m.id))
        .num_columns(3)
        .spacing(egui::vec2(12.0, 6.0))
        .show(ui, |ui| {
            for (key, s) in props {
                let now = set.and_then(|t| t.get(key));
                let title = s.get("title").and_then(Value::as_str).unwrap_or(key);
                let about = s.get("description").and_then(Value::as_str).unwrap_or("");
                ui.horizontal(|ui| {
                    ui.add_space(24.0);
                    ui.label(RichText::new(title).color(theme::DUST))
                        .on_hover_text(about);
                });
                let id = (plugin.to_string(), m.id.clone(), key.clone());
                let mut put = |value| {
                    asks.push(Ask::Param {
                        plugin: plugin.into(),
                        model: m.id.clone(),
                        key: key.clone(),
                        value,
                    })
                };
                control(
                    ui,
                    &format!("{title} for {}", m.id),
                    s,
                    now,
                    typed,
                    id,
                    &mut put,
                );
                if now.is_some() {
                    if ui
                        .small_button("Use default")
                        .on_hover_text("go back to the provider's default")
                        .clicked()
                    {
                        put(None);
                    }
                } else {
                    ui.label(RichText::new("default").small().color(theme::DUST));
                }
                ui.end_row();
            }
        });
}

/// The input for one option, by its schema: a tick box, a list, or a box to type in.
fn control(
    ui: &mut egui::Ui,
    name: &str,
    s: &Value,
    now: Option<&toml::Value>,
    typed: &mut std::collections::HashMap<(String, String, String), String>,
    id: (String, String, String),
    put: &mut impl FnMut(Option<toml::Value>),
) {
    let default = s.get("default");
    let kind = s.get("type").and_then(Value::as_str).unwrap_or("string");
    if let Some(choices) = s.get("enum").and_then(Value::as_array) {
        let shown = |v: &Value| {
            v.as_str()
                .map(String::from)
                .unwrap_or_else(|| v.to_string())
        };
        let current = now
            .map(|v| {
                v.as_str()
                    .map(String::from)
                    .unwrap_or_else(|| v.to_string())
            })
            .or_else(|| default.map(shown))
            .unwrap_or_default();
        let r = egui::ComboBox::from_id_salt(&id)
            .selected_text(&current)
            .width(160.0)
            .show_ui(ui, |ui| {
                for c in choices {
                    let text = shown(c);
                    if ui.selectable_label(text == current, &text).clicked() && text != current {
                        put(toml::Value::try_from(c.clone()).ok());
                    }
                }
            });
        r.response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, name));
        return;
    }
    if kind == "boolean" {
        let mut on = now
            .and_then(toml::Value::as_bool)
            .or_else(|| default.and_then(Value::as_bool))
            .unwrap_or(false);
        let r = ui.checkbox(&mut on, "");
        r.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, on, name));
        if r.changed() {
            put(Some(on.into()));
        }
        return;
    }
    let saved = now
        .map(|v| {
            v.as_str()
                .map(String::from)
                .unwrap_or_else(|| v.to_string())
        })
        .unwrap_or_default();
    let text = typed.entry(id.clone()).or_insert_with(|| saved.clone());
    let hint_text = default.map(|d| d.to_string()).unwrap_or_default();
    let r = ui.add(
        egui::TextEdit::singleline(text)
            .hint_text(RichText::new(hint_text).color(theme::DUST))
            .desired_width(240.0),
    );
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, name));
    if r.lost_focus() && *text != saved {
        let t = text.trim();
        let v = if t.is_empty() {
            None
        } else if kind == "number" || kind == "integer" {
            match t.parse::<f64>() {
                Ok(n) if kind == "integer" => Some((n as i64).into()),
                Ok(n) => Some(n.into()),
                // Back to what's saved; nothing to send.
                Err(_) => {
                    *text = saved;
                    return;
                }
            }
        } else {
            Some(t.into())
        };
        put(v);
    }
    if !r.has_focus() && *text != saved && !r.lost_focus() {
        // Changed from elsewhere, e.g. undo.
        *text = saved;
    }
}
