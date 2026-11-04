//! Preferences: a page per plugin with its keys, the program it runs from, and its models' options
//! for the open film. Everything is a draft until OK.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use serde_json::Value;
use shotline_core::dotenv;
use shotline_core::protocol::ModelInfo;
use shotline_core::{ModelRef, Task};

use crate::edit::{Action, task_name};
use crate::{Host, PluginInfo, Runs, hint, short, theme};

type Params = BTreeMap<String, BTreeMap<String, toml::Table>>;
type Models = BTreeMap<Task, Option<ModelRef>>;

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

impl Key {
    /// What OK would do with it: Some(None) takes it out of the file.
    fn change(&self) -> Option<Option<String>> {
        let t = self.typed.trim();
        (t != self.saved).then(|| Some(t.to_string()).filter(|t| !t.is_empty()))
    }
}

#[derive(Clone)]
enum Check {
    Running,
    Ok(String),
    Failed(String),
}

/// What closing the window left for the app.
#[derive(Default)]
pub(crate) struct Done {
    pub open: bool,
    /// The film's default models and model options that changed, as one step to undo.
    pub film: Vec<Action>,
    pub keys_changed: bool,
    /// Which program a plugin runs from changed, so the plugin list is read again.
    pub plugins_changed: bool,
}

pub(crate) struct Prefs {
    file: PathBuf,
    list: Vec<PluginInfo>,
    problems: Vec<String>,
    page: usize,
    /// Default Models is open rather than a plugin's page.
    models_page: bool,
    /// The film's default models as opened, and as changed; None with no film open.
    models: Option<(Models, Models)>,
    /// By plugin, in the order it lists them.
    keys: BTreeMap<String, Vec<Key>>,
    /// The film's options as opened, and as changed; None with no film open.
    params: Option<(Params, Params)>,
    /// Some(program) keeps it, None lets PATH decide.
    pins: BTreeMap<String, Option<PathBuf>>,
    /// What a text option holds while it's typed, by (plugin, model, key).
    typed: HashMap<(String, String, String), String>,
    checks: Arc<Mutex<HashMap<String, Check>>>,
    note: Option<(String, bool)>,
}

/// The labels' column, so the controls line up on its right edge.
const LABEL: f32 = 170.0;

impl Prefs {
    /// Opens at `plugin`'s page, or the first.
    pub fn new(
        user_dir: &Path,
        host: Option<&Arc<dyn Host>>,
        film: Option<&shotline_core::Config>,
        plugin: Option<&str>,
    ) -> Prefs {
        let mut p = Prefs {
            file: dotenv::path(user_dir),
            list: vec![],
            problems: vec![],
            page: 0,
            models_page: false,
            models: film.map(|c| {
                let m: Models = Task::ALL.iter().map(|t| (*t, c.model_set(*t))).collect();
                (m.clone(), m)
            }),
            keys: BTreeMap::new(),
            params: film.map(|c| (c.params.clone(), c.params.clone())),
            pins: BTreeMap::new(),
            typed: HashMap::new(),
            checks: Arc::default(),
            note: None,
        };
        p.read(host);
        if let Some(i) = plugin.and_then(|n| p.list.iter().position(|x| x.name == n)) {
            p.page = i;
        }
        p
    }

    /// The plugins and the keys as they are now; anything typed goes.
    fn read(&mut self, host: Option<&Arc<dyn Host>>) {
        (self.list, self.problems) = host.map(|h| h.plugins()).unwrap_or_default();
        self.page = self.page.min(self.list.len().saturating_sub(1));
        let file = dotenv::read(&self.file);
        self.keys = self
            .list
            .iter()
            .map(|p| {
                let mut vars: Vec<String> = p
                    .description
                    .as_ref()
                    .map(|d| d.env.clone())
                    .unwrap_or_default();
                for m in &p.missing {
                    if !vars.contains(m) {
                        vars.push(m.clone());
                    }
                }
                let keys = vars
                    .into_iter()
                    .map(|var| {
                        let saved = file.iter().find(|(n, _)| *n == var).map(|(_, v)| v.clone());
                        let source = if std::env::var_os(&var).is_some() {
                            Source::Environment
                        } else if saved.is_some() {
                            Source::File
                        } else {
                            Source::Missing
                        };
                        let saved = saved.unwrap_or_default();
                        Key {
                            var,
                            source,
                            typed: saved.clone(),
                            saved,
                            shown: false,
                        }
                    })
                    .collect();
                (p.name.clone(), keys)
            })
            .collect();
    }

    fn dirty(&self) -> bool {
        self.keys.values().flatten().any(|k| k.change().is_some())
            || !self.pins.is_empty()
            || self.params.as_ref().is_some_and(|(was, now)| was != now)
            || self.models.as_ref().is_some_and(|(was, now)| was != now)
    }

    /// Writes the keys and the kept programs; the film's options go back to the app.
    fn save(&mut self, host: Option<&Arc<dyn Host>>, done: &mut Done) -> bool {
        let mut said = vec![];
        let mut failed = None;
        for k in self.keys.values().flatten() {
            if let Some(value) = k.change() {
                match dotenv::set(&self.file, &k.var, value.as_deref()) {
                    Ok(()) => done.keys_changed = true,
                    Err(e) => failed = Some(e.to_string()),
                }
            }
        }
        if let Some(h) = host {
            for (name, pin) in std::mem::take(&mut self.pins) {
                let r = match &pin {
                    Some(exec) => h.pin(&name, exec),
                    None => h.unpin(&name),
                };
                match r {
                    Ok(m) => said.push(m),
                    Err(e) => failed = Some(e.to_string()),
                }
                done.plugins_changed = true;
            }
        }
        if let Some((was, now)) = &self.models {
            for (t, m) in now {
                if was.get(t) != Some(m) {
                    done.film.push(Action::Model(*t, m.clone()));
                }
            }
        }
        if let Some((was, now)) = &self.params {
            done.film.extend(param_changes(was, now));
        }
        if done.keys_changed || done.plugins_changed {
            self.read(host);
            self.checks.lock().unwrap().clear();
        }
        match failed {
            Some(e) => {
                self.note = Some((e, true));
                false
            }
            None => true,
        }
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

    pub fn show(&mut self, ui: &mut egui::Ui, host: Option<Arc<dyn Host>>) -> Done {
        let mut done = Done::default();
        let (mut ok, mut cancel) = (false, false);
        let mut check = None;
        let modal = crate::modal(ui.ctx(), "preferences", |ui| {
            ui.set_width(900.0);
            ui.heading("Preferences");
            ui.add_space(8.0);
            let row = ui.horizontal_top(|ui| {
                self.side(ui);
                // Drawn after: a separator in a row grows to whatever height it's given.
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical()
                        .id_salt("page")
                        .max_height(560.0)
                        .min_scrolled_height(560.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for p in &self.problems {
                                ui.label(RichText::new(p).color(theme::FLAG));
                            }
                            if self.models_page {
                                self.models_page(ui);
                            } else if self.list.is_empty() {
                                hint(ui, "No plugins found.");
                            } else {
                                check = self.page(ui);
                            }
                        });
                });
            });
            let x = row.response.rect.left() + 170.0 + 6.0;
            ui.painter().vline(
                x,
                row.response.rect.y_range(),
                egui::Stroke::new(1.0, theme::LINE),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if let Some((n, bad)) = &self.note {
                    let c = if *bad { theme::FLAG } else { theme::DUST };
                    ui.label(RichText::new(n).color(c));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ok = ui
                        .add(egui::Button::new("OK").min_size(egui::vec2(64.0, 0.0)))
                        .clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        });
        if let Some(p) = check {
            self.check(ui.ctx(), host.clone(), &p);
        }
        done.open = true;
        if ok {
            done.open = !self.save(host.as_ref(), &mut done);
        } else if cancel {
            done.open = false;
        } else if modal.should_close() {
            // Escape or a click outside shouldn't lose what was changed.
            if self.dirty() {
                self.note = Some((
                    "OK saves the changes; Cancel throws them away".into(),
                    false,
                ));
            } else {
                done.open = false;
            }
        }
        done
    }

    /// The list of pages, a plugin that wants attention marked.
    fn side(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.set_width(170.0);
            ui.label(RichText::new("Plugins").small().color(theme::DUST));
            ui.add_space(2.0);
            for (i, p) in self.list.iter().enumerate() {
                let trouble = match &p.description {
                    Err(_) => Some("doesn't answer".to_string()),
                    Ok(_) if !p.missing.is_empty() => {
                        Some(format!("needs {}", p.missing.join(", ")))
                    }
                    Ok(_) => None,
                };
                let on = i == self.page && !self.models_page;
                let r = side_item(ui, on, &p.name);
                let r = match &trouble {
                    Some(t) => {
                        let at = egui::pos2(r.rect.right() - 10.0, r.rect.center().y);
                        ui.painter().circle_filled(at, 3.5, theme::FLAG);
                        r.on_hover_text(t)
                    }
                    None => r,
                };
                if r.clicked() {
                    self.page = i;
                    self.models_page = false;
                }
            }
            ui.add_space(12.0);
            ui.label(RichText::new("This film").small().color(theme::DUST));
            ui.add_space(2.0);
            if side_item(ui, self.models_page, "Default Models").clicked() {
                self.models_page = true;
            }
        });
    }

    /// Which model the film uses for each job, with how each is charged.
    fn models_page(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("Default Models")
                .text_style(egui::TextStyle::Heading)
                .color(theme::PAPER),
        );
        ui.add_space(8.0);
        let Some((_, now)) = &mut self.models else {
            hint(ui, "Each film picks its own; open one to choose them.");
            return;
        };
        let about = "This film's, saved in its shotline.toml. Where a shot, a voice or a panel \
                     picks its own, that wins; the price shown is a guide, the quote before \
                     anything is spent is what counts.";
        let list = &self.list;
        group(ui, "For each job", about, |ui| {
            for (i, t) in Task::ALL.iter().enumerate() {
                if i > 0 {
                    ui.add_space(10.0);
                }
                let pick = now.get_mut(t).expect("every task");
                task_row(ui, list, *t, pick);
            }
        });
    }

    /// The open plugin's page. Returns a plugin to check the key of.
    fn page(&mut self, ui: &mut egui::Ui) -> Option<String> {
        let p = &self.list[self.page];
        let mut check = None;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&p.name)
                    .text_style(egui::TextStyle::Heading)
                    .color(theme::PAPER),
            );
            if let Ok(d) = &p.description {
                ui.label(RichText::new(&d.version).small().color(theme::DUST));
            }
            if !p.missing.is_empty() {
                ui.label(
                    RichText::new(format!("needs {}", p.missing.join(", "))).color(theme::FLAG),
                );
            }
        });
        ui.add_space(8.0);
        let keys = self.keys.get_mut(&p.name).filter(|k| !k.is_empty());
        if let Some(keys) = keys {
            let checks = self.checks.lock().unwrap().clone();
            group(
                ui,
                "Keys",
                &format!(
                    "Yours, for every film. Kept in {}, where the CLI reads them too; one set in \
                     the environment wins.",
                    self.file.display()
                ),
                |ui| {
                    for k in keys.iter_mut() {
                        key_row(ui, k);
                    }
                    ui.horizontal(|ui| {
                        ui.add_space(LABEL + ui.spacing().item_spacing.x);
                        let changed = keys.iter().any(|k| k.change().is_some());
                        let ready = keys.iter().all(|k| k.source != Source::Missing);
                        let running = matches!(checks.get(&p.name), Some(Check::Running));
                        if ui
                            .add_enabled(ready && !changed && !running, egui::Button::new("Check"))
                            .on_hover_text("asks the provider for the account's balance; free")
                            .on_disabled_hover_text(if changed {
                                "OK first, to save the key"
                            } else {
                                "set the key first"
                            })
                            .clicked()
                        {
                            check = Some(p.name.clone());
                        }
                        match checks.get(&p.name) {
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
                },
            );
        }
        group(
            ui,
            "Program",
            "Found in your plugins folder or on PATH; the first one on PATH wins. Keep one to use \
             it whatever PATH says.",
            |ui| program(ui, p, &mut self.pins),
        );
        match &p.description {
            Err(e) => {
                ui.label(RichText::new(format!("doesn't answer: {e}")).color(theme::FLAG));
            }
            Ok(d) if !d.models.is_empty() => {
                let about = if self.params.is_some() {
                    "This film's options, saved in its shotline.toml; a shot's own win over them."
                } else {
                    "Each film keeps its own options; open one to set them."
                };
                group(ui, "Models", about, |ui| {
                    for (i, m) in d.models.iter().enumerate() {
                        if i > 0 {
                            ui.add_space(8.0);
                        }
                        let params = self.params.as_mut().map(|(_, now)| now);
                        model(ui, &p.name, m, params, &mut self.typed);
                    }
                });
            }
            Ok(_) => {}
        }
        check
    }
}

/// Each option that differs, as an action taking it to what it is now.
fn param_changes(was: &Params, now: &Params) -> Vec<Action> {
    let get = |p: &Params, plugin: &str, model: &str, key: &str| {
        p.get(plugin)
            .and_then(|x| x.get(model))
            .and_then(|t| t.get(key))
            .cloned()
    };
    let mut seen = std::collections::BTreeSet::new();
    for p in [was, now] {
        for (plugin, models) in p {
            for (model, t) in models {
                for key in t.keys() {
                    seen.insert((plugin.clone(), model.clone(), key.clone()));
                }
            }
        }
    }
    seen.into_iter()
        .filter_map(|(plugin, model, key)| {
            let value = get(now, &plugin, &model, &key);
            (get(was, &plugin, &model, &key) != value).then_some(Action::Param {
                plugin,
                model,
                key,
                value,
            })
        })
        .collect()
}

/// A titled box of settings.
fn group(ui: &mut egui::Ui, title: &str, about: &str, body: impl FnOnce(&mut egui::Ui)) {
    ui.label(
        RichText::new(title)
            .family(egui::FontFamily::Name(theme::SEMIBOLD.into()))
            .color(theme::PAPER),
    );
    ui.add_space(2.0);
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, theme::LINE))
        .corner_radius(4.0)
        .inner_margin(10)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(about).small().color(theme::DUST));
            ui.add_space(6.0);
            body(ui);
        });
    ui.add_space(14.0);
}

/// A label in the left column, against its control's edge.
fn label(ui: &mut egui::Ui, text: &str, about: &str) -> egui::Response {
    let h = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(
        egui::vec2(LABEL, h),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            ui.set_min_size(egui::vec2(LABEL, h));
            let r = ui.add(egui::Label::new(RichText::new(text).color(theme::GRAPHITE)).truncate());
            if about.is_empty() {
                r
            } else {
                r.on_hover_text(about)
            }
        },
    )
    .inner
}

/// One key: its name, a hidden box to type it, and where it's set now.
fn key_row(ui: &mut egui::Ui, k: &mut Key) {
    ui.horizontal(|ui| {
        label(ui, &k.var, "");
        let field = ui.add(
            egui::TextEdit::singleline(&mut k.typed)
                .password(!k.shown)
                .hint_text(RichText::new("paste the key").color(theme::DUST))
                .desired_width(300.0),
        );
        field.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, &k.var));
        if ui.button(if k.shown { "Hide" } else { "Show" }).clicked() {
            k.shown = !k.shown;
        }
        if ui
            .add_enabled(
                !k.typed.is_empty(),
                egui::Button::new(RichText::new("Remove").color(theme::FLAG)),
            )
            .on_hover_text("take it out of .env")
            .clicked()
        {
            k.typed.clear();
        }
        let (state, color) = match (k.change(), &k.source) {
            (Some(Some(_)), _) => ("saved on OK", theme::TUNGSTEN),
            (Some(None), _) => ("taken out on OK", theme::TUNGSTEN),
            (None, Source::File) => ("in .env", theme::GRAPHITE),
            (None, Source::Environment) => ("from the environment", theme::GRAPHITE),
            (None, Source::Missing) => ("not set", theme::FLAG),
        };
        ui.label(RichText::new(state).small().color(color));
    });
}

/// Where the plugin runs from, and the other copies found.
fn program(ui: &mut egui::Ui, p: &PluginInfo, pins: &mut BTreeMap<String, Option<PathBuf>>) {
    if let Some(pin) = pins.get(&p.name).cloned() {
        ui.horizontal(|ui| {
            label(ui, "On OK", "");
            let text = match &pin {
                Some(e) => format!("keeps {}", e.display()),
                None => "PATH decides".into(),
            };
            ui.label(RichText::new(text).color(theme::TUNGSTEN));
            if ui.small_button("Leave as is").clicked() {
                pins.remove(&p.name);
            }
        });
        return;
    }
    let (place, kept) = match &p.runs {
        Runs::Builtin => ("built into shotline".to_string(), false),
        Runs::Path(e) => (e.display().to_string(), false),
        Runs::Pinned(e) => (e.display().to_string(), true),
        Runs::Folder(d) => (d.display().to_string(), false),
    };
    ui.horizontal(|ui| {
        label(ui, "Runs from", "");
        ui.label(RichText::new(&place).color(theme::PAPER));
        if kept {
            ui.label(RichText::new("kept").small().color(theme::TUNGSTEN));
            if ui
                .small_button("Let PATH decide")
                .on_hover_text("takes away the saved choice in your plugins folder")
                .clicked()
            {
                pins.insert(p.name.clone(), None);
            }
        } else if let Runs::Path(e) = &p.runs
            && ui
                .small_button("Keep this one")
                .on_hover_text(
                    "saves this choice in your plugins folder, so it's used whatever PATH says",
                )
                .clicked()
        {
            pins.insert(p.name.clone(), Some(e.clone()));
        }
    });
    for (i, s) in p.skipped.iter().enumerate() {
        ui.horizontal(|ui| {
            label(ui, if i == 0 { "Also found, not used" } else { "" }, "");
            ui.label(RichText::new(s.display().to_string()).color(theme::DUST));
            if ui
                .small_button("Use this one")
                .on_hover_text("keeps it in your plugins folder, so it wins over PATH")
                .clicked()
            {
                pins.insert(p.name.clone(), Some(s.clone()));
            }
        });
    }
}

/// A model, and its options when there's a film to set them in.
fn model(
    ui: &mut egui::Ui,
    plugin: &str,
    m: &ModelInfo,
    params: Option<&mut Params>,
    typed: &mut HashMap<(String, String, String), String>,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(&m.id).color(theme::PAPER));
        ui.label(RichText::new(m.op.to_string()).small().color(theme::DUST));
    });
    let props = m
        .params_schema
        .as_ref()
        .and_then(|s| s.get("properties"))
        .and_then(Value::as_object)
        .filter(|p| !p.is_empty());
    let (Some(props), Some(params)) = (props, params) else {
        return;
    };
    for (key, s) in props {
        let set = params.get(plugin).and_then(|x| x.get(&m.id));
        let now = set.and_then(|t| t.get(key)).cloned();
        let title = s.get("title").and_then(Value::as_str).unwrap_or(key);
        let about = s.get("description").and_then(Value::as_str).unwrap_or("");
        let mut put = None;
        ui.horizontal(|ui| {
            label(ui, title, about);
            let id = (plugin.to_string(), m.id.clone(), key.clone());
            control(
                ui,
                &format!("{title} for {}", m.id),
                s,
                now.as_ref(),
                typed,
                id,
                &mut |v| put = Some(v),
            );
            if now.is_some() {
                if ui
                    .small_button("Use default")
                    .on_hover_text("go back to the provider's default")
                    .clicked()
                {
                    put = Some(None);
                }
            } else {
                ui.label(RichText::new("default").small().color(theme::DUST));
            }
        });
        if let Some(v) = put {
            let models = params.entry(plugin.to_string()).or_default();
            let t = models.entry(m.id.clone()).or_default();
            match v {
                Some(v) => {
                    t.insert(key.clone(), v);
                }
                None => {
                    t.remove(key);
                }
            }
            if t.is_empty() {
                models.remove(&m.id);
            }
            if models.is_empty() {
                params.remove(plugin);
            }
        }
    }
}

/// The input for one option, by its schema: a tick box, a list, or a box to type in.
fn control(
    ui: &mut egui::Ui,
    name: &str,
    s: &Value,
    now: Option<&toml::Value>,
    typed: &mut HashMap<(String, String, String), String>,
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
                // Back to what it was; nothing to change.
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
        // Changed from elsewhere, e.g. Use default.
        *text = saved;
    }
}

/// An entry in the list of pages.
fn side_item(ui: &mut egui::Ui, on: bool, name: &str) -> egui::Response {
    let mut text = RichText::new(name);
    if on {
        text = text.color(theme::PAPER);
    }
    ui.add(egui::Button::selectable(on, text).min_size(egui::vec2(170.0, 24.0)))
}

/// What a job is for, shown under its name.
fn task_about(t: Task) -> &'static str {
    match t {
        Task::Translate => "puts what's typed into English, e.g. the tick box under a prompt",
        Task::Draft => "writes a shot's prompt from its plot",
        Task::Voice => {
            "speaks the lines of a voice on this plugin that names no model, and is what a new \
             voice starts on"
        }
        Task::Image => "anchors, a shot's pictures and storyboard frames",
        Task::Video => "a shot's takes, unless the shot picks its own model",
        Task::LipSync => "the lip-synced recipe, after the video is made",
        Task::Assistant => "for an assistant in the app; not built yet, so nothing uses it",
    }
}

/// What leaving it unset means.
fn task_none(t: Task) -> String {
    match t {
        Task::Translate => "same as drafting".into(),
        Task::Voice => "none, each voice names its own".into(),
        Task::LipSync => format!(
            "built in: {} · {}",
            shotline_core::LIPSYNC.0,
            shotline_core::LIPSYNC.1
        ),
        _ => "none".into(),
    }
}

/// One job: the label on the left, the model list, and how the picked one is charged.
fn task_row(ui: &mut egui::Ui, list: &[PluginInfo], t: Task, pick: &mut Option<ModelRef>) {
    let name = {
        let n = task_name(t);
        let mut c = n.chars();
        c.next()
            .map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
    };
    let models: Vec<(&PluginInfo, &ModelInfo)> = list
        .iter()
        .filter_map(|p| p.description.as_ref().ok().map(|d| (p, d)))
        .flat_map(|(p, d)| {
            d.models
                .iter()
                .filter(|m| m.op == t.op())
                .map(move |m| (p, m))
        })
        .collect();
    let found = pick.as_ref().and_then(|m| {
        models
            .iter()
            .find(|(p, x)| p.name == m.plugin && x.id == m.model)
    });
    let none = t.optional() || t == Task::LipSync;
    let shown = match (pick.as_ref(), found) {
        (None, _) => task_none(t),
        (Some(m), Some(_)) => format!("{} · {}", m.plugin, m.model),
        (Some(m), None) => format!("{} · {} (not installed)", m.plugin, m.model),
    };
    ui.horizontal(|ui| {
        label(ui, &name, task_about(t));
        let r = egui::ComboBox::from_id_salt(("default model", t as u8))
            .selected_text(&shown)
            .width(380.0)
            .height(420.0)
            .show_ui(ui, |ui| {
                ui.set_min_width(420.0);
                if none && ui.selectable_label(pick.is_none(), task_none(t)).clicked() {
                    *pick = None;
                }
                for (p, m) in &models {
                    let on = pick
                        .as_ref()
                        .is_some_and(|x| x.plugin == p.name && x.model == m.id);
                    let mut job = egui::text::LayoutJob::default();
                    let style = ui.style();
                    RichText::new(format!("{} · {}", p.name, m.id))
                        .color(theme::PAPER)
                        .append_to(
                            &mut job,
                            style,
                            egui::FontSelection::Default,
                            egui::Align::Min,
                        );
                    RichText::new(format!("\n{}", price_of(m)))
                        .small()
                        .color(theme::DUST)
                        .append_to(
                            &mut job,
                            style,
                            egui::FontSelection::Default,
                            egui::Align::Min,
                        );
                    if ui.selectable_label(on, job).clicked() {
                        *pick = Some(ModelRef::new(&p.name, &m.id));
                    }
                }
                if models.is_empty() {
                    ui.weak(format!("no installed plugin does {}", t.op()));
                }
            });
        r.response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &name));
    });
    // How it's charged, and anything in the way of using it.
    let mut notes = vec![];
    match found {
        Some((p, m)) => {
            notes.push((price_of(m).to_string(), theme::GRAPHITE));
            if !p.missing.is_empty() {
                notes.push((format!("needs {}", p.missing.join(", ")), theme::FLAG));
            }
        }
        None if pick.is_some() => notes.push((
            "its plugin isn't installed, so this won't work".into(),
            theme::FLAG,
        )),
        None => {}
    }
    if notes.is_empty() {
        return;
    }
    ui.horizontal(|ui| {
        ui.add_space(LABEL + ui.spacing().item_spacing.x);
        ui.vertical(|ui| {
            for (n, c) in notes {
                ui.label(RichText::new(n).small().color(c));
            }
        });
    });
}

fn price_of(m: &ModelInfo) -> &str {
    m.price
        .as_deref()
        .unwrap_or("the plugin doesn't say; see the quote")
}
