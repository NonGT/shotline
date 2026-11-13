//! Making an image with an image model, an element's anchor or a shot's picture: price first,
//! then spend only on a second click.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use shotline_core::ShotRef;

use crate::price::{Pressed, Price, PriceRow};
use crate::{AnchorAsk, Host, Thumbs, caption, hint, remove_button, running, shape, short, theme};

#[derive(Clone)]
enum Stage {
    Idle,
    Pricing,
    /// What this ask costs. Changing the ask throws it away.
    Priced(AnchorAsk, String),
    Making,
    Made(PathBuf),
    /// Asking the price didn't work.
    Failed(String),
    /// Making it didn't work; nothing was kept.
    Unmade(String),
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
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum For {
    Element(String),
    /// A new picture for the shot.
    Shot(ShotRef),
    /// One of the shot's pictures, made again in its place. By file, since the shot's pictures
    /// can be moved or removed while it's made.
    Picture(ShotRef, PathBuf),
    /// The shot's storyboard frame; what's typed becomes its frame prompt once kept. With the
    /// frame it had when Make was clicked, so one changed meanwhile isn't overwritten.
    Frame(ShotRef, Option<PathBuf>),
}

impl For {
    pub fn shot(&self) -> Option<ShotRef> {
        match self {
            For::Element(_) => None,
            For::Shot(r) | For::Picture(r, _) | For::Frame(r, _) => Some(*r),
        }
    }
}

pub(crate) struct Maker {
    pub target: For,
    /// The popup's title, e.g. "Make frame for 01-010".
    pub title: String,
    /// The run making it, once Make is clicked, as `running` names it.
    pub run: Option<String>,
    /// Said at the top, e.g. that the model it was made with isn't installed.
    note: Option<String>,
    /// Anchors of the cast, for whom "Outfit exactly as…" means something.
    pub cast: Vec<PathBuf>,
    pick: usize,
    prompt: String,
    /// Images to go along, in the order the prompt numbers them.
    refs: Vec<PathBuf>,
    /// None for the film's [image] settings.
    aspect: Option<String>,
    resolution: Option<String>,
    /// The film's: what None means.
    pub defaults: (String, Option<String>),
    /// Ticked: the prompt goes into English first.
    translate: bool,
    /// Ticked: the outfit as in the references, not only faces and hair.
    outfit: bool,
    url: String,
    /// The web address field, open once asked for.
    url_open: bool,
    fetch: Arc<Mutex<Fetch>>,
    stage: Arc<Mutex<Stage>>,
    /// Browse existing shows other shots' pictures too.
    show_all: bool,
    /// Keys each plugin needs that aren't set, as App::needs_keys has them.
    pub missing: std::collections::HashMap<String, Vec<String>>,
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
    /// Out of sight while it's made; it waits on the jobs list.
    Hide,
    /// The image made, to use.
    Keep,
    /// Not the image made: the settings again to change.
    Again,
    /// Not the image made, and done with it.
    Discard,
    /// Preferences, at this plugin's keys.
    Keys(String),
}

impl Maker {
    /// `refs` go along to start with, e.g. a shot's references.
    pub fn new(
        target: For,
        title: String,
        refs: Vec<PathBuf>,
        models: &[ImageModel],
        preferred: (&str, &str),
        defaults: (String, Option<String>),
    ) -> Maker {
        let found = models
            .iter()
            .position(|m| (m.plugin.as_str(), m.model.as_str()) == preferred);
        let note = match (found, models.first()) {
            (None, Some(m)) if !preferred.1.is_empty() => Some(format!(
                "{} · {} isn't installed; using {} · {}.",
                preferred.0, preferred.1, m.plugin, m.model
            )),
            _ => None,
        };
        Maker {
            target,
            title,
            run: None,
            note,
            cast: vec![],
            pick: found.unwrap_or(0),
            prompt: String::new(),
            refs,
            aspect: None,
            resolution: None,
            defaults,
            translate: false,
            outfit: true,
            url: String::new(),
            url_open: false,
            fetch: Arc::new(Mutex::new(Fetch::Idle)),
            stage: Arc::new(Mutex::new(Stage::Idle)),
            show_all: false,
            missing: Default::default(),
        }
    }

    /// The image made, once, and what was typed for it. The panel may not be showing by then.
    pub fn take_made(&mut self) -> Option<(PathBuf, String)> {
        let mut s = self.stage.lock().unwrap();
        let Stage::Made(out) = s.clone() else {
            return None;
        };
        *s = Stage::Idle;
        Some((out, self.prompt.trim().to_string()))
    }

    /// Catches up with how its run is going, as the run's record has it.
    pub fn follow(&mut self, s: Option<&running::State>) {
        let stage = self.stage.clone();
        let mut stage = stage.lock().unwrap();
        match s {
            Some(running::State::Running) => *stage = Stage::Making,
            Some(running::State::Made(out)) => {
                if !matches!(*stage, Stage::Made(_)) {
                    *stage = Stage::Made(out.clone());
                }
            }
            Some(running::State::Failed(e)) => {
                *stage = Stage::Unmade(e.clone());
                self.run = None;
            }
            Some(running::State::Stopped) => {
                *stage = Stage::Unmade(
                    "it stopped before it was done, as if the computer was turned off".into(),
                );
                self.run = None;
            }
            None => {
                if matches!(*stage, Stage::Making) {
                    *stage = Stage::Idle;
                }
                self.run = None;
            }
        }
    }

    /// Back to the settings an earlier run was made with.
    pub fn restore(&mut self, ask: &AnchorAsk) {
        self.prompt = ask.prompt.clone();
        self.aspect = ask.aspect.clone();
        self.resolution = ask.resolution.clone();
        self.translate = ask.translate;
        self.outfit = ask.outfit;
    }

    /// Bound to run `id`, which is being made now.
    pub fn making(&mut self, id: String) {
        self.run = Some(id);
        *self.stage.lock().unwrap() = Stage::Making;
    }

    /// Starts with this in the prompt box, e.g. a shot's frame prompt.
    pub fn set_prompt(&mut self, prompt: &str) {
        self.prompt = prompt.to_string();
    }

    /// Starts with the prompt put into English first, as it was last time.
    pub fn set_translate(&mut self, on: bool) {
        self.translate = on;
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

    /// The popup's insides: title, settings, references and the price row, or once made, the
    /// image to keep or try again. `esc` is the popup being asked to close.
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
        esc: bool,
    ) -> Wants {
        let mut wants = Wants::Nothing;
        let fetched = self.fetch.lock().unwrap().clone();
        if let Fetch::Got(p) = &fetched {
            self.refs.push(p.clone());
            self.url.clear();
            self.url_open = false;
            *self.fetch.lock().unwrap() = Fetch::Idle;
        }
        let stage = self.stage.lock().unwrap().clone();
        let making = matches!(stage, Stage::Making);
        let busy = matches!(stage, Stage::Pricing | Stage::Making);
        let frame = matches!(self.target, For::Frame(..));
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&self.title)
                    .font(egui::FontId::new(
                        17.0,
                        egui::FontFamily::Name(theme::SEMIBOLD.into()),
                    ))
                    .color(theme::PAPER),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let close =
                    crate::icon_button(ui, "Close", crate::close_icon).on_hover_text(if making {
                        "Close; it carries on, even with the app closed, and waits under the \
                         jobs at the top right"
                    } else {
                        "Close"
                    });
                if close.clicked() || esc {
                    wants = if making { Wants::Hide } else { Wants::Close };
                }
            });
        });
        ui.add_space(8.0);
        if let Stage::Made(out) = &stage {
            return match self.review(ui, out, thumbs, models) {
                Wants::Nothing => wants,
                w => w,
            };
        }
        if let Some(n) = &self.note {
            ui.label(RichText::new(n).color(theme::DUST));
            ui.add_space(6.0);
        }
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
            let (label, example) = match self.target {
                For::Frame(..) => (
                    "Frame prompt",
                    "what the first frame shows, e.g. @paka at the edge of the forest, looking \
                     back; left empty, the shot's prompt",
                ),
                For::Element(_) => ("Image prompt", "anything to add, e.g. side view, full body"),
                For::Shot(_) | For::Picture(..) => (
                    "Image prompt",
                    "what it shows, e.g. the café at night, from the door",
                ),
            };
            let l = ui.weak(label).id;
            ui.add(
                egui::TextEdit::multiline(&mut self.prompt)
                    .hint_text(RichText::new(example).color(theme::DUST))
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            )
            .labelled_by(l);
            translate_box(ui, &mut self.translate, &self.prompt, translator.as_deref());
            if let Some(m) = models.get(self.pick) {
                ui.add_space(6.0);
                if let Some(z) = self.references(ui, m, pool, thumbs, &fetched, host.clone(), film)
                {
                    wants = Wants::Zoom(z);
                }
                // Only means something for someone's clothes.
                if m.ref_label.is_some() && self.refs.iter().any(|r| self.cast.contains(r)) {
                    ui.checkbox(
                        &mut self.outfit,
                        "Outfit exactly as in their reference images",
                    )
                    .on_hover_text(
                        "off: faces and hair stay, the clothes can change, so you can try \
                             other looks",
                    );
                }
            }
        });
        let Some(m) = models.get(self.pick) else {
            return wants;
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
            t => (String::new(), t.shot()),
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
        ui.add_space(12.0);
        let shown = match &stage {
            Stage::Pricing => Price::Asking,
            Stage::Making => Price::Working("making the image; this can take a minute"),
            Stage::Priced(priced, cost) if *priced == ask => Price::Priced(cost),
            Stage::Failed(e) => Price::Failed(e),
            _ => Price::Idle,
        };
        let keys = self
            .missing
            .get(&m.plugin)
            .map(|k| format!("{} needs {}", m.plugin, k.join(", ")));
        match PriceRow::new("Make").keys(keys).show(ui, shown) {
            Some(Pressed::Keys) => wants = Wants::Keys(m.plugin.clone()),
            Some(Pressed::Ask) => wants = Wants::Price(ask.clone()),
            Some(Pressed::Go) => {
                if let Stage::Priced(_, cost) = &stage {
                    wants = Wants::Make(ask.clone(), cost.clone());
                }
            }
            Some(Pressed::Cancel) => *self.stage.lock().unwrap() = Stage::Idle,
            None => {}
        }
        if let Stage::Unmade(e) = &stage {
            ui.label(
                RichText::new(format!("Couldn't make it: {}", short(e, 200))).color(theme::FLAG),
            )
            .on_hover_text(e);
        }
        wants
    }

    /// Showing the image made, which wants the popup wider.
    pub fn reviewing(&self) -> bool {
        matches!(*self.stage.lock().unwrap(), Stage::Made(_))
    }

    /// The image just made, big on the left; on the right what it was made with, and Keep, Try
    /// again (back to the settings) and Discard.
    fn review(
        &mut self,
        ui: &mut egui::Ui,
        out: &Path,
        thumbs: &mut Thumbs,
        models: &[ImageModel],
    ) -> Wants {
        let mut wants = Wants::Nothing;
        let side = 280.0;
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(ui.available_width() - side - 20.0);
                if thumbs.stage(ui, out, 480.0, "Made image") {
                    wants = Wants::Zoom(out.to_path_buf());
                }
            });
            ui.add_space(20.0);
            ui.vertical(|ui| {
                ui.set_width(side);
                ui.set_min_height(480.0);
                ui.label(crate::kit::with_icon(
                    crate::icon::CHECK,
                    "Made",
                    egui::FontFamily::Proportional,
                    13.5,
                    theme::GO,
                ));
                ui.add_space(12.0);
                let model = models
                    .get(self.pick)
                    .map(|m| format!("{} · {}", m.model, m.plugin))
                    .unwrap_or_default();
                egui::Grid::new("made with")
                    .num_columns(2)
                    .spacing(egui::vec2(12.0, 10.0))
                    .show(ui, |ui| {
                        ui.label(RichText::new("Model").color(theme::DUST));
                        ui.label(model);
                        ui.end_row();
                        if !self.prompt.trim().is_empty() {
                            ui.label(RichText::new("Prompt").color(theme::DUST));
                            ui.add(egui::Label::new(
                                RichText::new(self.prompt.trim()).color(theme::GRAPHITE),
                            ));
                            ui.end_row();
                        }
                        if !self.refs.is_empty() {
                            ui.label(RichText::new("References").color(theme::DUST));
                            ui.horizontal_wrapped(|ui| {
                                for (i, r) in self.refs.iter().enumerate() {
                                    thumbs.show(ui, r, 36.0, &format!("Reference {}", i + 1));
                                }
                            });
                            ui.end_row();
                        }
                    });
                ui.add_space(ui.available_height() - 150.0);
                let what = match &self.target {
                    For::Element(h) => format!("Keep adds it to @{h}'s anchors."),
                    For::Shot(r) => format!("Keep adds it to {r}'s pictures."),
                    For::Picture(r, f) => format!(
                        "Keep puts it in place of {} in {r}; undo brings the old one back.",
                        f.file_name().unwrap_or_default().to_string_lossy()
                    ),
                    For::Frame(r, _) => format!(
                        "Keep makes it {r}'s storyboard frame, and what was typed its frame \
                         prompt; undo brings the old ones back."
                    ),
                };
                hint(ui, &what);
                ui.add_space(10.0);
                let keep = ui.scope(|ui| {
                    ui.spacing_mut().button_padding.y = 9.0;
                    ui.add_sized(
                        egui::vec2(side, 36.0),
                        egui::Button::new(
                            RichText::new("Keep")
                                .family(egui::FontFamily::Name(theme::SEMIBOLD.into()))
                                .color(theme::RACK),
                        )
                        .fill(theme::PAPER),
                    )
                });
                if keep.inner.on_hover_text("use it").clicked() {
                    wants = Wants::Keep;
                }
                ui.add_space(4.0);
                ui.columns(2, |c| {
                    let w = c[0].available_width();
                    if c[0]
                        .add_sized(egui::vec2(w, 32.0), egui::Button::new("Try again"))
                        .on_hover_text("back to what it was made with, to change and make again")
                        .clicked()
                    {
                        *self.stage.lock().unwrap() = Stage::Idle;
                        wants = Wants::Again;
                    }
                    c[1].with_layout(
                        egui::Layout::top_down_justified(egui::Align::Center),
                        |ui| {
                            if crate::kit::danger_button(ui, "Discard")
                            .on_hover_text(
                                "don't use it; it stays under Browse existing, and what it cost \
                                 stays in the ledger",
                            )
                            .clicked()
                        {
                            wants = Wants::Discard;
                        }
                        },
                    );
                });
            });
        });
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
                let mut file = false;
                ui.menu_button("Add a reference…", |ui| {
                    if ui.button("From a file…").clicked() {
                        file = true;
                        ui.close();
                    }
                    if ui.button("From a web address…").clicked() {
                        self.url_open = true;
                        ui.close();
                    }
                })
                .response
                .on_disabled_hover_text(&why);
                if file
                    && let Some(f) = rfd::FileDialog::new()
                        .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
                        .pick_file()
                {
                    self.refs.push(f);
                }
            });
            let running = matches!(fetched, Fetch::Running);
            if !self.url_open && !running {
                return;
            }
            ui.horizontal(|ui| {
                let field = ui.add_enabled(
                    !running,
                    egui::TextEdit::singleline(&mut self.url)
                        .hint_text(RichText::new("https://…").color(theme::DUST))
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
                    For::Shot(_) | For::Picture(..) => {
                        "Name them in the prompt by the label under each.".into()
                    }
                    For::Frame(..) => "Elements the prompt names are pointed at their pictures \
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
