//! Making an anchor image with an image model: price first, then spend only on a second click.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};

use crate::{AnchorAsk, Host, Thumbs, caption, hint, remove_button, short, spend_button, theme};

#[derive(Clone)]
enum Stage {
    Idle,
    Pricing,
    /// What this ask costs. Changing the ask throws it away.
    Priced(AnchorAsk, String),
    Making,
    Made(PathBuf),
    Failed(String),
}

/// An image model as the panel needs it.
pub(crate) struct ImageModel {
    pub plugin: String,
    pub model: String,
    /// How the prompt names a reference, e.g. "Image {n}"; None if it takes none.
    pub ref_label: Option<String>,
    pub max_refs: Option<usize>,
}

#[derive(Clone)]
enum Fetch {
    Idle,
    Running,
    Got(PathBuf),
    Failed(String),
}

pub(crate) struct Maker {
    pub handle: String,
    pick: usize,
    prompt: String,
    /// Images to go along, in the order the prompt numbers them.
    refs: Vec<PathBuf>,
    url: String,
    fetch: Arc<Mutex<Fetch>>,
    stage: Arc<Mutex<Stage>>,
}

/// What the panel wants of the app.
pub(crate) enum Wants {
    Nothing,
    /// Unsaved edits written first, so the price is for what's on screen.
    Price(AnchorAsk),
    Make(AnchorAsk, String),
    Zoom(PathBuf),
}

impl Maker {
    /// The element's own anchors go along to start with, so a new one keeps the look.
    pub fn new(
        handle: &str,
        anchors: Vec<PathBuf>,
        models: &[ImageModel],
        preferred: (&str, &str),
    ) -> Maker {
        Maker {
            handle: handle.to_string(),
            pick: models
                .iter()
                .position(|m| (m.plugin.as_str(), m.model.as_str()) == preferred)
                .unwrap_or(0),
            prompt: String::new(),
            refs: anchors,
            url: String::new(),
            fetch: Arc::new(Mutex::new(Fetch::Idle)),
            stage: Arc::new(Mutex::new(Stage::Idle)),
        }
    }

    /// The image made, once. The panel may not be showing by then.
    pub fn take_made(&mut self) -> Option<PathBuf> {
        let mut s = self.stage.lock().unwrap();
        let Stage::Made(out) = s.clone() else {
            return None;
        };
        *s = Stage::Idle;
        drop(s);
        self.prompt.clear();
        Some(out)
    }

    pub fn busy(&self) -> bool {
        matches!(*self.stage.lock().unwrap(), Stage::Pricing | Stage::Making)
    }

    fn run(
        &self,
        ctx: &egui::Context,
        host: Arc<dyn Host>,
        work: impl FnOnce(&dyn Host) -> Stage + Send + 'static,
        now: Stage,
    ) {
        *self.stage.lock().unwrap() = now;
        let (stage, ctx) = (self.stage.clone(), ctx.clone());
        std::thread::spawn(move || {
            let s = work(host.as_ref());
            *stage.lock().unwrap() = s;
            ctx.request_repaint();
        });
    }

    pub fn price(&self, ctx: &egui::Context, host: Arc<dyn Host>, film: &Path, ask: AnchorAsk) {
        let film = film.to_path_buf();
        self.run(
            ctx,
            host,
            move |h| match h.anchor_quote(&film, &ask) {
                Ok(cost) => Stage::Priced(ask, cost),
                Err(e) => Stage::Failed(format!("{e:#}")),
            },
            Stage::Pricing,
        );
    }

    pub fn make(
        &self,
        ctx: &egui::Context,
        host: Arc<dyn Host>,
        film: &Path,
        ask: AnchorAsk,
        cost: String,
    ) {
        let film = film.to_path_buf();
        self.run(
            ctx,
            host,
            move |h| match h.make_anchor(&film, &ask, &cost) {
                Ok(out) => Stage::Made(out),
                Err(e) => Stage::Failed(format!("{e:#}")),
            },
            Stage::Making,
        );
    }

    fn fetch(&self, ctx: &egui::Context, host: Arc<dyn Host>, film: &Path, url: String) {
        *self.fetch.lock().unwrap() = Fetch::Running;
        let (fetch, ctx, film) = (self.fetch.clone(), ctx.clone(), film.to_path_buf());
        std::thread::spawn(move || {
            let f = match host.fetch_image(&film, &url) {
                Ok(p) => Fetch::Got(p),
                Err(e) => Fetch::Failed(format!("{e:#}")),
            };
            *fetch.lock().unwrap() = f;
            ctx.request_repaint();
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        models: &[ImageModel],
        pool: &[(String, PathBuf)],
        thumbs: &mut Thumbs,
        host: Option<Arc<dyn Host>>,
        film: &Path,
    ) -> Wants {
        let mut wants = Wants::Nothing;
        let fetched = self.fetch.lock().unwrap().clone();
        if let Fetch::Got(p) = &fetched {
            self.refs.push(p.clone());
            self.url.clear();
            *self.fetch.lock().unwrap() = Fetch::Idle;
        }
        let stage = self.stage.lock().unwrap().clone();
        let busy = matches!(stage, Stage::Pricing | Stage::Making);
        // The page's own colour: on RAISED, buttons (RAISED too) would lose their edges.
        egui::Frame::new()
            .fill(theme::BAY)
            .stroke(egui::Stroke::new(1.0, theme::LINE))
            .corner_radius(4.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.set_max_width(560.0);
                ui.add_enabled_ui(!busy, |ui| {
                    ui.horizontal(|ui| {
                        let l = ui.weak("Image model");
                        let name = |i: usize| {
                            models
                                .get(i)
                                .map(|m| format!("{} · {}", m.plugin, m.model))
                                .unwrap_or_default()
                        };
                        egui::ComboBox::from_id_salt("anchor model")
                            .selected_text(name(self.pick))
                            .width(320.0)
                            .show_ui(ui, |ui| {
                                for i in 0..models.len() {
                                    ui.selectable_value(&mut self.pick, i, name(i));
                                }
                            })
                            .response
                            .labelled_by(l.id);
                    });
                    ui.add(
                        egui::TextEdit::singleline(&mut self.prompt)
                            .hint_text(
                                RichText::new("anything to add, e.g. side view, full body")
                                    .color(theme::DUST),
                            )
                            .desired_width(f32::INFINITY),
                    )
                    .widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Image prompt")
                    });
                    if let Some(m) = models.get(self.pick) {
                        ui.add_space(6.0);
                        if let Some(z) =
                            self.references(ui, m, pool, thumbs, &fetched, host.clone(), film)
                        {
                            wants = Wants::Zoom(z);
                        }
                    }
                });
                let Some(m) = models.get(self.pick) else {
                    return;
                };
                let ask = AnchorAsk {
                    handle: self.handle.clone(),
                    prompt: self.prompt.trim().to_string(),
                    plugin: m.plugin.clone(),
                    model: m.model.clone(),
                    // A model that takes none gets none, whatever is listed.
                    refs: if m.ref_label.is_some() {
                        self.refs.clone()
                    } else {
                        vec![]
                    },
                };
                ui.add_space(4.0);
                ui.horizontal(|ui| match &stage {
                    Stage::Pricing => {
                        ui.spinner();
                        ui.weak("asking the price…");
                    }
                    Stage::Making => {
                        ui.spinner();
                        ui.weak("making the image; this can take a minute");
                    }
                    Stage::Priced(priced, cost) if *priced == ask => {
                        if spend_button(ui, &format!("Make it for {cost}"))
                            .on_hover_text("spends this and writes it to the ledger")
                            .clicked()
                        {
                            wants = Wants::Make(ask.clone(), cost.clone());
                        }
                        if ui.button("Cancel").clicked() {
                            *self.stage.lock().unwrap() = Stage::Idle;
                        }
                    }
                    _ => {
                        if ui
                            .button("Get price")
                            .on_hover_text("free; nothing is spent yet")
                            .clicked()
                        {
                            wants = Wants::Price(ask.clone());
                        }
                        if let Stage::Failed(e) = &stage {
                            ui.label(RichText::new(short(e, 70)).color(theme::FLAG))
                                .on_hover_text(e);
                        }
                    }
                });
            });
        wants
    }

    /// The images going along, and ways to add more. Returns a picture to see bigger.
    #[allow(clippy::too_many_arguments)]
    fn references(
        &mut self,
        ui: &mut egui::Ui,
        m: &ImageModel,
        pool: &[(String, PathBuf)],
        thumbs: &mut Thumbs,
        fetched: &Fetch,
        host: Option<Arc<dyn Host>>,
        film: &Path,
    ) -> Option<PathBuf> {
        let Some(label) = &m.ref_label else {
            hint(ui, &format!("{} makes images from text only.", m.model));
            return None;
        };
        let mut zoom = None;
        ui.horizontal(|ui| {
            caption(ui, "References");
            if let Some(max) = m.max_refs {
                ui.label(
                    RichText::new(format!("up to {max}"))
                        .small()
                        .color(theme::DUST),
                );
            }
        });
        let mut remove = None;
        if self.refs.is_empty() {
            hint(ui, "None: the image comes from the words alone.");
        }
        ui.horizontal_wrapped(|ui| {
            for (i, r) in self.refs.iter().enumerate() {
                let name = label.replace("{n}", &(i + 1).to_string());
                ui.vertical(|ui| {
                    if thumbs.show(ui, r, 96.0, &name) {
                        zoom = Some(r.clone());
                    }
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&name).small().color(theme::GRAPHITE));
                        if remove_button(ui)
                            .on_hover_text(format!("leave {name} out"))
                            .clicked()
                        {
                            remove = Some(i);
                        }
                    });
                });
            }
        });
        if let Some(i) = remove {
            self.refs.remove(i);
        }
        let full = m.max_refs.is_some_and(|max| self.refs.len() >= max);
        let why = format!("{} takes at most {}", m.model, m.max_refs.unwrap_or(0));
        ui.add_enabled_ui(!full, |ui| {
            ui.horizontal(|ui| {
                let left: Vec<_> = pool
                    .iter()
                    .filter(|(_, p)| !self.refs.contains(p))
                    .collect();
                ui.add_enabled_ui(!left.is_empty(), |ui| {
                    egui::ComboBox::from_id_salt("add anchor ref")
                        .selected_text("Add an anchor")
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            for (name, p) in left {
                                if ui.selectable_label(false, name).clicked() {
                                    self.refs.push(p.clone());
                                }
                            }
                        })
                        .response
                        .on_disabled_hover_text(&why)
                        .widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Add an anchor")
                        });
                });
                if ui
                    .button("Add file…")
                    .on_disabled_hover_text(&why)
                    .clicked()
                    && let Some(f) = rfd::FileDialog::new()
                        .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
                        .pick_file()
                {
                    self.refs.push(f);
                }
            });
            ui.horizontal(|ui| {
                let running = matches!(fetched, Fetch::Running);
                let field = ui.add_enabled(
                    !running,
                    egui::TextEdit::singleline(&mut self.url)
                        .hint_text(RichText::new("or an image's web address").color(theme::DUST))
                        .desired_width(380.0),
                );
                field.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Image address")
                });
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let url = self.url.trim().to_string();
                let ok = url.starts_with("https://") || url.starts_with("http://");
                if running {
                    ui.spinner();
                } else if (ui
                    .add_enabled(ok, egui::Button::new("Fetch"))
                    .on_hover_text("downloads it into the film's cache")
                    .on_disabled_hover_text("an http or https address")
                    .clicked()
                    || (enter && ok))
                    && let Some(h) = host
                {
                    self.fetch(ui.ctx(), h, film, url);
                }
            });
        });
        if let Fetch::Failed(e) = fetched {
            ui.label(RichText::new(short(e, 80)).color(theme::FLAG))
                .on_hover_text(e);
        }
        if !self.refs.is_empty() {
            hint(
                ui,
                &format!(
                    "Name them in the prompt by the label under each; @{}'s own are named for you.",
                    self.handle
                ),
            );
        }
        zoom
    }
}
