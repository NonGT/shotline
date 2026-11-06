//! Making an image with an image model, an element's anchor or a shot's picture: price first,
//! then spend only on a second click.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use shotline_core::ShotRef;

use crate::{
    AnchorAsk, Host, Thumbs, caption, hint, remove_button, shape, short, spend_button, theme,
};

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

/// "Translate to English first", under anything typed for a model: only offered when the text
/// isn't English and a text model can do it. True when it was clicked.
pub(crate) fn translate_box(
    ui: &mut egui::Ui,
    on: &mut bool,
    text: &str,
    translator: Option<&str>,
) -> bool {
    let text = text.trim();
    let Some(t) = translator else {
        return false;
    };
    if text.is_empty() || shotline_core::looks_english(text) {
        return false;
    }
    ui.checkbox(on, "Translate to English first")
        .on_hover_text(format!(
            "with {t}; models follow English best. A small extra spend, shown in the price."
        ))
        .changed()
}

/// An image model as the panel needs it.
pub(crate) struct ImageModel {
    pub plugin: String,
    pub model: String,
    /// How the prompt names a reference, e.g. "Image {n}"; None if it takes none.
    pub ref_label: Option<String>,
    pub max_refs: Option<usize>,
    /// Empty when the model doesn't say.
    pub aspects: Vec<String>,
    pub resolutions: Vec<String>,
}

#[derive(Clone)]
enum Fetch {
    Idle,
    Running,
    Got(PathBuf),
    Failed(String),
}

/// What the image is for.
#[derive(Clone, PartialEq)]
pub(crate) enum For {
    Element(String),
    Shot(ShotRef),
    /// Made from the shot's frame prompt, which is what's typed.
    Frame(ShotRef),
}

pub(crate) struct Maker {
    pub target: For,
    pick: usize,
    prompt: String,
    /// Images to go along, in the order the prompt numbers them.
    refs: Vec<PathBuf>,
    /// None for the film's [image] settings.
    aspect: Option<String>,
    resolution: Option<String>,
    /// The film's: what None means.
    defaults: (String, Option<String>),
    /// Ticked: the prompt goes into English first.
    translate: bool,
    /// Ticked: the outfit as in the references, not only faces and hair.
    outfit: bool,
    url: String,
    fetch: Arc<Mutex<Fetch>>,
    stage: Arc<Mutex<Stage>>,
    /// Browse existing shows other shots' pictures too.
    show_all: bool,
}

/// A picture that can go along: an element's anchor, or a shot's frame or own picture.
pub(crate) struct Pooled {
    pub name: String,
    pub path: PathBuf,
    /// The two headers it's listed under.
    pub group: (String, String),
    /// Listed before Show all: anchors, and the pictures of the shot it's made for.
    pub near: bool,
}

/// What the panel wants of the app.
pub(crate) enum Wants {
    Nothing,
    /// Unsaved edits written first, so the price is for what's on screen.
    Price(AnchorAsk),
    Make(AnchorAsk, String),
    Zoom(PathBuf),
    Close,
}

impl Maker {
    /// `refs` go along to start with, e.g. a shot's references.
    pub fn new(
        target: For,
        refs: Vec<PathBuf>,
        models: &[ImageModel],
        preferred: (&str, &str),
        defaults: (String, Option<String>),
    ) -> Maker {
        Maker {
            target,
            pick: models
                .iter()
                .position(|m| (m.plugin.as_str(), m.model.as_str()) == preferred)
                .unwrap_or(0),
            prompt: String::new(),
            refs,
            aspect: None,
            resolution: None,
            defaults,
            translate: false,
            outfit: true,
            url: String::new(),
            fetch: Arc::new(Mutex::new(Fetch::Idle)),
            stage: Arc::new(Mutex::new(Stage::Idle)),
            show_all: false,
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

    /// Starts with this in the prompt box, e.g. a shot's frame prompt.
    pub fn with_prompt(mut self, prompt: &str) -> Maker {
        self.prompt = prompt.to_string();
        self
    }

    /// Starts with the prompt put into English first, as it was last time.
    pub fn translating(mut self, on: bool) -> Maker {
        self.translate = on;
        self
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
        pool: &[Pooled],
        thumbs: &mut Thumbs,
        host: Option<Arc<dyn Host>>,
        film: &Path,
        translator: Option<String>,
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
        let frame = matches!(self.target, For::Frame(_));
        // The page's own colour: on RAISED, buttons (RAISED too) would lose their edges.
        // Room on the right for the close button.
        let panel = egui::Frame::new()
            .fill(theme::BAY)
            .stroke(egui::Stroke::new(1.0, theme::LINE))
            .corner_radius(4.0)
            .inner_margin(egui::Margin {
                left: 10,
                right: 34,
                top: 10,
                bottom: 10,
            })
            .show(ui, |ui| {
                // No wider than the column it's in, e.g. a shot's References.
                ui.set_max_width(ui.available_width().min(560.0));
                ui.add_enabled_ui(!busy, |ui| {
                    egui::Grid::new("anchor settings")
                        .num_columns(2)
                        .spacing(egui::vec2(12.0, 8.0))
                        .show(ui, |ui| {
                            let l = ui.weak("Image model");
                            let name = |i: usize| {
                                models
                                    .get(i)
                                    .map(|m| format!("{} · {}", m.plugin, m.model))
                                    .unwrap_or_default()
                            };
                            egui::ComboBox::from_id_salt("anchor model")
                                .selected_text(name(self.pick))
                                .width(ui.available_width().min(320.0))
                                .show_ui(ui, |ui| {
                                    for i in 0..models.len() {
                                        ui.selectable_value(&mut self.pick, i, name(i));
                                    }
                                })
                                .response
                                .labelled_by(l.id);
                            ui.end_row();
                            if let Some(m) = models.get(self.pick) {
                                self.size(ui, m);
                            }
                        });
                    ui.add_space(6.0);
                    if frame {
                        // Saved as the shot's frame prompt before it's priced.
                        let l = ui.weak("Frame prompt").id;
                        ui.add(
                            egui::TextEdit::multiline(&mut self.prompt)
                                .hint_text(
                                    RichText::new(
                                        "what the first frame shows, e.g. @paka at the edge of the \
                                         forest, looking back; left empty, the shot's prompt",
                                    )
                                    .color(theme::DUST),
                                )
                                .desired_rows(2)
                                .desired_width(f32::INFINITY),
                        )
                        .labelled_by(l);
                    } else {
                        let example = match self.target {
                            For::Element(_) => "anything to add, e.g. side view, full body",
                            For::Shot(_) | For::Frame(_) => {
                                "what it shows, e.g. the café at night, from the door"
                            }
                        };
                        ui.add(
                            egui::TextEdit::multiline(&mut self.prompt)
                                .hint_text(RichText::new(example).color(theme::DUST))
                                .desired_rows(2)
                                .desired_width(f32::INFINITY),
                        )
                        .widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::Role::MultilineTextInput,
                                true,
                                "Image prompt",
                            )
                        });
                    }
                    translate_box(ui, &mut self.translate, &self.prompt, translator.as_deref());
                    if let Some(m) = models.get(self.pick) {
                        ui.add_space(6.0);
                        if let Some(z) =
                            self.references(ui, m, pool, thumbs, &fetched, host.clone(), film)
                        {
                            wants = Wants::Zoom(z);
                        }
                        if m.ref_label.is_some() && !self.refs.is_empty() {
                            ui.checkbox(
                                &mut self.outfit,
                                "Outfit exactly as in their reference images",
                            )
                            .on_hover_text(
                                "off: faces and hair stay, the clothes can change, so you can \
                                 try other looks",
                            );
                        }
                    }
                });
                let Some(m) = models.get(self.pick) else {
                    return;
                };
                // What another model picked may not be on offer here.
                if self.aspect.as_ref().is_some_and(|a| !m.aspects.contains(a)) {
                    self.aspect = None;
                }
                if self
                    .resolution
                    .as_ref()
                    .is_some_and(|r| !m.resolutions.contains(r))
                {
                    self.resolution = None;
                }
                let (handle, shot) = match &self.target {
                    For::Element(h) => (h.clone(), None),
                    For::Shot(r) | For::Frame(r) => (String::new(), Some(*r)),
                };
                let ask = AnchorAsk {
                    handle,
                    shot,
                    frame,
                    prompt: self.prompt.trim().to_string(),
                    plugin: m.plugin.clone(),
                    model: m.model.clone(),
                    // A model that takes none gets none, whatever is listed.
                    refs: if m.ref_label.is_some() {
                        self.refs.clone()
                    } else {
                        vec![]
                    },
                    aspect: self.aspect.clone(),
                    resolution: self.resolution.clone(),
                    translate: self.translate
                        && translator.is_some()
                        && !shotline_core::looks_english(self.prompt.trim()),
                    outfit: self.outfit,
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
        let r = panel.response.rect;
        let at = egui::Rect::from_min_size(
            egui::pos2(r.right() - 30.0, r.top() + 6.0),
            egui::vec2(24.0, 24.0),
        );
        // Laid over the panel without moving the cursor back up, or what follows overlaps it.
        let mut over = ui.new_child(egui::UiBuilder::new().max_rect(at));
        let close = over
            .add_enabled_ui(!busy, |ui| {
                crate::icon_button(ui, "Close", crate::close_icon)
            })
            .inner
            .on_hover_text("Close")
            .on_disabled_hover_text("still at it; it closes by itself once the image is made");
        if close.clicked() {
            wants = Wants::Close;
        }
        wants
    }

    /// The Size row: shape, and resolution for models that have a choice.
    fn size(&mut self, ui: &mut egui::Ui, m: &ImageModel) {
        let l = ui.weak("Size");
        ui.horizontal(|ui| {
            let default = format!("Default ({})", shape(&self.defaults.0));
            egui::ComboBox::from_id_salt("anchor aspect")
                .selected_text(self.aspect.as_deref().map_or(default.clone(), shape))
                .width(180.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.aspect, None, &default);
                    for a in &m.aspects {
                        ui.selectable_value(&mut self.aspect, Some(a.clone()), shape(a));
                    }
                })
                .response
                .labelled_by(l.id);
            if m.resolutions.is_empty() {
                return;
            }
            ui.add_space(8.0);
            let l = ui.weak("Resolution");
            let default = match &self.defaults.1 {
                Some(r) => format!("Default ({r})"),
                None => "Default".into(),
            };
            egui::ComboBox::from_id_salt("anchor resolution")
                .selected_text(self.resolution.clone().unwrap_or(default.clone()))
                .width(110.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.resolution, None, &default);
                    for r in &m.resolutions {
                        ui.selectable_value(&mut self.resolution, Some(r.clone()), r);
                    }
                })
                .response
                .labelled_by(l.id);
        });
        ui.end_row();
    }

    /// The images going along, and ways to add more. Returns a picture to see bigger.
    #[allow(clippy::too_many_arguments)]
    fn references(
        &mut self,
        ui: &mut egui::Ui,
        m: &ImageModel,
        pool: &[Pooled],
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
                let left: Vec<&Pooled> = pool
                    .iter()
                    .filter(|p| !self.refs.contains(&p.path))
                    .collect();
                let far = left.iter().filter(|p| !p.near).count();
                let rows: Vec<(String, String, &Pooled)> = left
                    .iter()
                    .filter(|p| p.near || self.show_all)
                    .map(|p| (p.group.0.clone(), p.group.1.clone(), *p))
                    .collect();
                let (mut picked, mut more) = (None, false);
                ui.add_enabled_ui(!left.is_empty(), |ui| {
                    let open = egui::ComboBox::from_id_salt("add anchor ref")
                        .selected_text(crate::browse::LABEL)
                        .width(150.0)
                        .height(crate::grouped::MAX)
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                        .show_ui(ui, |ui| {
                            ui.set_width(360.0);
                            let grown = crate::grouped::list(
                                ui,
                                &rows,
                                |ui, p| {
                                    let hover = film_rel(film, &p.path);
                                    if crate::grouped::picture(ui, thumbs, &p.path, &p.name, &hover)
                                    {
                                        picked = Some(p.path.clone());
                                    }
                                },
                                |ui| {
                                    if !self.show_all && far > 0 {
                                        more = ui
                                            .add(
                                                crate::grouped::item(
                                                    false,
                                                    format!("Show all ({far} from other shots)"),
                                                )
                                                .min_size(egui::vec2(ui.available_width(), 0.0)),
                                            )
                                            .clicked();
                                    }
                                },
                            );
                            crate::grouped::remeasure_later(ui, grown);
                            if picked.is_some() {
                                ui.close();
                            }
                        });
                    crate::grouped::remeasure(ui.ctx());
                    if open.inner.is_none() {
                        self.show_all = false;
                    }
                    open.response.on_disabled_hover_text(&why).widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::ComboBox, true, crate::browse::LABEL)
                    });
                });
                if more {
                    self.show_all = true;
                }
                if let Some(p) = picked {
                    self.refs.push(p);
                }
                if ui
                    .button("Add image…")
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
                        // Room for Fetch beside it.
                        .desired_width((ui.available_width() - 64.0).min(380.0)),
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
                &match &self.target {
                    For::Element(h) => format!(
                        "Name them in the prompt by the label under each; @{h}'s own are named \
                         for you."
                    ),
                    For::Shot(_) => "Name them in the prompt by the label under each.".into(),
                    For::Frame(_) => "Elements the prompt names are pointed at their pictures \
                                      here; name any others by the label under each."
                        .into(),
                },
            );
        }
        zoom
    }
}

/// A path as the film names it, or as it is when it's outside the film.
fn film_rel(film: &Path, p: &Path) -> String {
    p.strip_prefix(film).unwrap_or(p).display().to_string()
}
