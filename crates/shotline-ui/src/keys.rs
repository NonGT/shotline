//! Settings: the API keys plugins need, typed in and kept in the user's `.env`, where the CLI
//! and plugins read them too.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use shotline_core::dotenv;
use shotline_core::protocol::Description;

use crate::{Host, hint, short, theme};

/// Where a key's value comes from.
#[derive(Clone, PartialEq)]
enum Source {
    File,
    /// The environment wins over the file.
    Environment,
    Missing,
}

struct Key {
    var: String,
    source: Source,
    /// What's in the file.
    saved: String,
    typed: String,
    shown: bool,
}

#[derive(Clone)]
enum Check {
    Running,
    Ok(String),
    Failed(String),
}

pub(crate) struct Keys {
    file: PathBuf,
    plugins: Vec<(String, Vec<Key>)>,
    checks: Arc<Mutex<HashMap<String, Check>>>,
    note: Option<(String, bool)>,
}

impl Keys {
    pub fn new(user_dir: &Path, plugins: &[Description]) -> Keys {
        let mut k = Keys {
            file: dotenv::path(user_dir),
            plugins: plugins
                .iter()
                .filter(|d| !d.env.is_empty())
                .map(|d| (d.name.clone(), d.env.iter().map(|v| key(v)).collect()))
                .collect(),
            checks: Arc::default(),
            note: None,
        };
        k.read();
        k
    }

    fn read(&mut self) {
        let file = dotenv::read(&self.file);
        for (_, keys) in &mut self.plugins {
            for k in keys {
                let saved = file
                    .iter()
                    .find(|(n, _)| *n == k.var)
                    .map(|(_, v)| v.clone());
                k.source = if std::env::var_os(&k.var).is_some() {
                    Source::Environment
                } else if saved.is_some() {
                    Source::File
                } else {
                    Source::Missing
                };
                k.saved = saved.unwrap_or_default();
                k.typed = k.saved.clone();
            }
        }
    }

    fn set(&mut self, var: &str, value: Option<&str>) -> bool {
        let done = dotenv::set(&self.file, var, value);
        self.note = Some(match &done {
            Ok(()) if value.is_some() => (format!("{var} saved"), false),
            Ok(()) => (format!("{var} taken out"), false),
            Err(e) => (e.to_string(), true),
        });
        self.read();
        self.checks.lock().unwrap().clear();
        done.is_ok()
    }

    fn check(&self, ctx: &egui::Context, host: Option<Arc<dyn Host>>, plugin: &str) {
        let Some(host) = host else { return };
        self.checks
            .lock()
            .unwrap()
            .insert(plugin.to_string(), Check::Running);
        let (checks, ctx, plugin) = (self.checks.clone(), ctx.clone(), plugin.to_string());
        std::thread::spawn(move || {
            let c = match host.balance(&plugin) {
                Ok(b) => Check::Ok(b),
                Err(e) => Check::Failed(e.to_string()),
            };
            checks.lock().unwrap().insert(plugin, c);
            ctx.request_repaint();
        });
    }

    /// The window. Returns whether it's still open and whether a key changed.
    pub fn show(&mut self, ui: &mut egui::Ui, host: Option<Arc<dyn Host>>) -> (bool, bool) {
        let mut changed = false;
        let mut close = false;
        let mut save = None;
        let mut check = None;
        let modal = egui::Modal::new(egui::Id::unique("keys")).show(ui.ctx(), |ui| {
            ui.set_width(560.0);
            ui.heading("Keys");
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "API keys for the installed plugins, kept in {}. The CLI reads them from there \
                     too. A key set in the environment wins over the file.",
                    self.file.display()
                ))
                .color(theme::DUST),
            );
            ui.add_space(10.0);
            if self.plugins.is_empty() {
                hint(ui, "No installed plugin needs a key.");
            }
            let checks = self.checks.lock().unwrap().clone();
            egui::ScrollArea::vertical()
                .max_height(480.0)
                // Full width, so the scroll bar sits at the window's edge.
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (plugin, keys) in &mut self.plugins {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(plugin.as_str())
                                .family(egui::FontFamily::Name(theme::SEMIBOLD.into()))
                                .color(theme::PAPER),
                        );
                        for k in keys.iter_mut() {
                            key_row(ui, k, &mut save);
                        }
                        ui.horizontal(|ui| {
                            let ready = keys.iter().all(|k| k.source != Source::Missing);
                            let running =
                                matches!(checks.get(plugin.as_str()), Some(Check::Running));
                            if ui
                                .add_enabled(ready && !running, egui::Button::new("Check"))
                                .on_hover_text("asks the provider for the account's balance; free")
                                .on_disabled_hover_text("set the key first")
                                .clicked()
                            {
                                check = Some(plugin.clone());
                            }
                            match checks.get(plugin.as_str()) {
                                Some(Check::Running) => {
                                    ui.spinner();
                                }
                                Some(Check::Ok(b)) => {
                                    ui.label(
                                        RichText::new(format!("works: {b}")).color(theme::GRAPHITE),
                                    );
                                }
                                Some(Check::Failed(e)) => {
                                    ui.label(RichText::new(short(e, 60)).color(theme::FLAG))
                                        .on_hover_text(e);
                                }
                                None => {}
                            }
                        });
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
        if let Some((var, value)) = save {
            changed = self.set(&var, value.as_deref());
        }
        if let Some(p) = check {
            self.check(ui.ctx(), host, &p);
        }
        (!(close || modal.should_close()), changed)
    }
}

fn key(var: &str) -> Key {
    Key {
        var: var.to_string(),
        source: Source::Missing,
        saved: String::new(),
        typed: String::new(),
        shown: false,
    }
}

/// One key: its name and where it's set, a hidden box to type it, and what to do with it.
fn key_row(ui: &mut egui::Ui, k: &mut Key, save: &mut Option<(String, Option<String>)>) {
    ui.horizontal(|ui| {
        let (state, color) = match k.source {
            Source::File => ("in .env", theme::GRAPHITE),
            Source::Environment => ("from the environment", theme::GRAPHITE),
            Source::Missing => ("not set", theme::FLAG),
        };
        ui.label(RichText::new(&k.var).color(theme::PAPER));
        ui.label(RichText::new(state).small().color(color));
    });
    ui.horizontal(|ui| {
        let field = ui.add(
            egui::TextEdit::singleline(&mut k.typed)
                .password(!k.shown)
                .hint_text(RichText::new("paste the key").color(theme::DUST))
                .desired_width(330.0),
        );
        field.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, &k.var));
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.button(if k.shown { "Hide" } else { "Show" }).clicked() {
            k.shown = !k.shown;
        }
        let typed = k.typed.trim().to_string();
        let new = !typed.is_empty() && typed != k.saved;
        if (ui.add_enabled(new, egui::Button::new("Save")).clicked() || (enter && new))
            && save.is_none()
        {
            *save = Some((k.var.clone(), Some(typed)));
        }
        if !k.saved.is_empty()
            && ui
                .button(RichText::new("Remove").color(theme::FLAG))
                .on_hover_text("take it out of .env")
                .clicked()
        {
            *save = Some((k.var.clone(), None));
        }
    });
    ui.add_space(4.0);
}
