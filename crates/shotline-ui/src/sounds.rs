//! Sounds laid over a take in the cut, from a file or said by one of the cast, and put in the
//! editor's sequence too when the take is in it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use shotline_core::protocol::{Change, ClipInfo, EditParams, SequenceInfo};
use shotline_core::{Job, Kind, TakeSound, Under};

use crate::cut_tools::Undo;
use crate::edit::Action;
use crate::{App, hint, short, theme};

type Slot = Arc<Mutex<Option<Result<String, String>>>>;

/// Speech being priced or made: who, what they say, and how far it got.
enum Speak {
    Idle,
    Asking(Slot),
    Priced(String),
    Making(Slot),
    Failed(String),
}

pub(crate) struct SoundsWin {
    /// The take, a shot's or a bridge's, and what it's called.
    pub under: Under,
    pub name: String,
    /// Seconds into the take a new sound starts.
    pub at: f64,
    who: String,
    text: String,
    speak: Speak,
    /// What the sound effect sounds like, its length (0 lets the model pick) and model.
    effect_text: String,
    length: f64,
    model: usize,
    effect: Speak,
    /// The sound the next speech or effect made replaces: its job id and name.
    remake_speech: Option<(String, String)>,
    remake_effect: Option<(String, String)>,
    /// The editor's clip of a remade sound, taken out once the editor is free.
    lift: Option<(String, String)>,
}

impl SoundsWin {
    pub fn new(under: Under, name: String, at: f64) -> Self {
        Self {
            under,
            name,
            at: (at * 100.0).round() / 100.0,
            who: String::new(),
            text: String::new(),
            speak: Speak::Idle,
            effect_text: String::new(),
            length: 0.0,
            model: 0,
            effect: Speak::Idle,
            remake_speech: None,
            remake_effect: None,
            lift: None,
        }
    }

    fn busy(&self) -> bool {
        self.lift.is_some()
            || [&self.speak, &self.effect]
                .iter()
                .any(|s| matches!(s, Speak::Asking(_) | Speak::Making(_)))
    }
}

/// The editor's main sequence, its project and plugin.
type Main = (String, PathBuf, SequenceInfo);

impl App {
    pub(crate) fn open_sounds(&mut self, under: Under, name: String, at: f64) {
        self.sounds_win = Some(SoundsWin::new(under, name, at));
    }

    /// Whether the editor's `file` is `u`'s take.
    fn plays(&self, file: &Path, u: Under) -> bool {
        self.edit_takes
            .get(file)
            .is_some_and(|t| Under::from(t) == u)
            || self
                .edit_bridges
                .get(file)
                .is_some_and(|(b, t, _)| Under::Bridge(*b, *t) == u)
    }

    fn main_edit(&self) -> Option<Main> {
        let (file, name) = crate::cut::main_sequence(&self.project.root, &self.edits.0)?;
        let plugin = crate::cut::plugin_of(&self.edits.0, &file);
        let seq = self
            .edits
            .0
            .iter()
            .find(|e| e.file == file)?
            .sequences
            .iter()
            .find(|s| s.name == name)?
            .clone();
        (!plugin.is_empty()).then_some((plugin, file, seq))
    }

    /// The take's clip in the sequence that plays `at`, else its first one.
    fn clip_of(&self, seq: &SequenceInfo, u: Under, at: f64) -> Option<ClipInfo> {
        let clips: Vec<&ClipInfo> = seq
            .video
            .iter()
            .flat_map(|t| &t.clips)
            .filter(|c| c.file.as_ref().is_some_and(|f| self.plays(f, u)))
            .collect();
        clips
            .iter()
            .find(|c| c.from <= at && at < c.from + c.length * c.speed.max(0.01))
            .or(clips.first())
            .map(|c| (*c).clone())
    }

    fn sound_change(
        &mut self,
        ctx: &egui::Context,
        main: Main,
        clip: String,
        what: String,
        change: Change,
    ) {
        let (plugin, file, seq) = main;
        let ask = EditParams {
            film: self.project.root.clone(),
            file: file.clone(),
            sequence: seq.name.clone(),
            clip,
            change,
        };
        let undo = Undo {
            plugin: plugin.clone(),
            file,
            sequence: seq.name,
            what: what.clone(),
            steps: serde_json::Value::Null,
        };
        self.start_change(ctx, &plugin, what, ask, Some(undo));
    }

    /// The cast who have a voice to speak with.
    fn voiced(&self) -> Vec<String> {
        self.film
            .elements
            .iter()
            .filter(|(e, _)| e.kind == Kind::Cast && e.voice.is_some())
            .map(|(e, _)| e.handle.clone())
            .collect()
    }

    pub(crate) fn sounds_window(&mut self, ctx: &egui::Context) {
        let Some(mut w) = self.sounds_win.take() else {
            return;
        };
        // Kept up to date, to know where the take is in the editor's sequence.
        self.refresh_edits(ctx);
        self.speech_news(ctx, &mut w);
        if self.changing.is_none()
            && let Some((clip, what)) = w.lift.take()
            && let Some(m) = self.main_edit()
        {
            self.sound_change(ctx, m, clip, what, Change::Lift);
        }
        let sounds = self.project.sounds(w.under).unwrap_or_default();
        let main = self.main_edit();
        let busy = self.changing.is_some() || w.busy();
        let voiced = self.voiced();
        let effects = self.effect_models();
        if w.who.is_empty() || !voiced.contains(&w.who) {
            w.who = voiced.first().cloned().unwrap_or_default();
        }
        let mut add = false;
        let mut remove: Option<(TakeSound, Job, PathBuf)> = None;
        let mut remake: Option<(TakeSound, Job, PathBuf)> = None;
        let mut close = false;
        let modal = crate::modal(ctx, "take sounds", |ui| {
            ui.set_width(520.0);
            ui.horizontal(|ui| {
                ui.heading(format!("Sounds over {}", w.name));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close |= crate::icon_button(ui, "Close", crate::close_icon).clicked();
                });
            });
            ui.add_space(6.0);
            if sounds.is_empty() {
                hint(
                    ui,
                    "Effects or a voice off, laid over this take in the cut.",
                );
            }
            for s in &sounds {
                let (ts, job, path) = s;
                ui.horizontal(|ui| {
                    let what = job.id.to_string();
                    let name = sound_name(job);
                    let playing = self.hearing(ui.ctx(), &what);
                    let (icon, label): (fn(&egui::Painter, egui::Rect, egui::Stroke), _) =
                        if playing {
                            (crate::stop_icon, format!("Stop {name}"))
                        } else {
                            (crate::play_icon, format!("Play {name}"))
                        };
                    if crate::icon_button(ui, &label, icon).clicked() {
                        if playing {
                            self.hush();
                        } else {
                            self.hear(&what, path);
                        }
                    }
                    ui.label(RichText::new(short(&name, 48)).color(theme::PAPER))
                        .on_hover_text(&name);
                    ui.label(
                        RichText::new(format!("from {:.2} s", ts.at))
                            .small()
                            .color(theme::DUST),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let r = ui.add_enabled_ui(!busy, crate::remove_button).inner;
                        r.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::Role::Button,
                                !busy,
                                format!("Remove {name}"),
                            )
                        });
                        if r.clicked() {
                            remove = Some(s.clone());
                        }
                        if remakeable(job) {
                            let r = ui
                                .add_enabled(!busy, egui::Button::new("Remake").small())
                                .on_hover_text("make it again with the same words, in its place");
                            r.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::Role::Button,
                                    !busy,
                                    format!("Remake {name}"),
                                )
                            });
                            if r.clicked() {
                                remake = Some(s.clone());
                            }
                        }
                    });
                });
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label("Starts at");
                ui.add(
                    egui::DragValue::new(&mut w.at)
                        .range(0.0..=600.0)
                        .speed(0.05)
                        .suffix(" s"),
                )
                .on_hover_text("seconds into the take, for the next sound added");
                add = ui
                    .add_enabled(!busy, egui::Button::new("From a file…"))
                    .on_hover_text(match &main {
                        Some(_) => "a sound from somewhere else, put in the editor's sequence too",
                        None => "a sound from somewhere else, mixed in when the cut is rendered",
                    })
                    .clicked();
            });
            ui.add_space(10.0);
            ui.label(RichText::new("Speech").strong().color(theme::PAPER));
            ui.add_space(4.0);
            if voiced.is_empty() {
                hint(
                    ui,
                    "Give one of the cast a voice in Elements to have them say something here.",
                );
            } else {
                self.speech_row(ui, &mut w, &voiced);
            }
            if !effects.is_empty() {
                ui.add_space(10.0);
                ui.label(RichText::new("Sound effect").strong().color(theme::PAPER));
                ui.add_space(4.0);
                self.effect_row(ui, &mut w, &effects);
            }
        });
        if add
            && let Some(file) = self.pick_file(
                rfd::FileDialog::new()
                    .add_filter("Sound", &["wav", "mp3", "m4a", "aac", "flac", "ogg"]),
            )
        {
            let before = self.sound_jobs(w.under);
            self.flush();
            self.apply(vec![Action::AddSound(w.under, file, w.at)]);
            let new = self
                .sound_jobs(w.under)
                .into_iter()
                .find(|j| !before.contains(j));
            if let Some(job) = new {
                self.lay(ctx, (w.under, &w.name), &job, main.clone());
            }
        }
        if let Some(s) = remove
            && let Some(lift) = self.unlay(ctx, (w.under, &w.name), s, main.as_ref())
            && let Some(m) = main
        {
            self.sound_change(ctx, m, lift.0, lift.1, Change::Lift);
        }
        if let Some(s) = remake {
            self.remake_sound(ctx, &mut w, s, &effects);
        }
        if !(close || modal.should_close()) || w.busy() {
            self.sounds_win = Some(w);
        }
    }

    fn sound_jobs(&self, u: Under) -> Vec<String> {
        self.project
            .sounds(u)
            .unwrap_or_default()
            .into_iter()
            .map(|(s, ..)| s.job)
            .collect()
    }

    /// Installed models that make sound effects: (plugin, model).
    fn effect_models(&self) -> Vec<(String, String)> {
        self.plugins
            .iter()
            .flat_map(|d| {
                d.models
                    .iter()
                    .filter(|m| m.op == shotline_core::protocol::Op::AudioSfx)
                    .map(|m| (d.name.clone(), m.id.clone()))
            })
            .collect()
    }

    /// What it sounds like, how long, which model, and the price, then Go.
    fn effect_row(&mut self, ui: &mut egui::Ui, w: &mut SoundsWin, models: &[(String, String)]) {
        let before = (w.effect_text.clone(), w.length, w.model);
        w.model = w.model.min(models.len() - 1);
        ui.add(
            egui::TextEdit::singleline(&mut w.effect_text)
                .hint_text("what it sounds like, e.g. a wooden door slams shut")
                .desired_width(f32::INFINITY),
        )
        .widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::TextInput, true, "What it sounds like")
        });
        ui.horizontal(|ui| {
            ui.label("Length");
            ui.add(
                egui::DragValue::new(&mut w.length)
                    .range(0.0..=30.0)
                    .speed(0.1)
                    .custom_formatter(|v, _| {
                        if v < 0.5 {
                            "model picks".into()
                        } else {
                            format!("{v:.1} s")
                        }
                    }),
            )
            .on_hover_text("0.5 to 30 seconds; at 0 the model picks, which can cost more");
            if models.len() > 1 {
                egui::ComboBox::from_id_salt("effect model")
                    .selected_text(&models[w.model].1)
                    .show_ui(ui, |ui| {
                        for (i, (p, m)) in models.iter().enumerate() {
                            ui.selectable_value(&mut w.model, i, format!("{p} · {m}"));
                        }
                    });
            }
        });
        if (w.effect_text.clone(), w.length, w.model) != before
            && matches!(w.effect, Speak::Priced(_) | Speak::Failed(_))
        {
            w.effect = Speak::Idle;
        }
        if let Some((_, name)) = &w.remake_effect {
            hint(ui, &format!("in place of {}", short(name, 60)));
        }
        let ready = !w.effect_text.trim().is_empty();
        let free = self.changing.is_none();
        let at = w.at;
        match go_buttons(
            ui,
            &w.effect,
            ready,
            free,
            at,
            ("the sound effect", "say what it sounds like first"),
        ) {
            Some(Ask::Price) => {
                w.effect = Speak::Asking(self.ask_effect(ui.ctx(), w, &models[w.model], None));
            }
            Some(Ask::Go(cost)) => {
                let slot = self.ask_effect(ui.ctx(), w, &models[w.model], Some(cost));
                w.effect = Speak::Making(slot);
            }
            Some(Ask::Cancel) => {
                w.effect = Speak::Idle;
                w.remake_effect = None;
            }
            None => {}
        }
    }

    fn ask_effect(
        &self,
        ctx: &egui::Context,
        w: &SoundsWin,
        (plugin, model): &(String, String),
        at: Option<String>,
    ) -> Slot {
        let slot: Slot = Arc::default();
        let Some(host) = self.host.clone() else {
            *slot.lock().unwrap() = Some(Err("can't make sound effects here".into()));
            return slot;
        };
        let (s, ctx, film) = (slot.clone(), ctx.clone(), self.project.root.clone());
        let (r, text) = (w.under.shot(), w.effect_text.clone());
        let length = (w.length >= 0.5).then_some(w.length);
        let (plugin, model) = (plugin.clone(), model.clone());
        std::thread::spawn(move || {
            let got = match at {
                None => host.effect_quote(&film, &plugin, &model, &text, length),
                Some(at) => host.effect(&film, r, &plugin, &model, &text, length, &at),
            };
            *s.lock().unwrap() = Some(got.map_err(|e| format!("{e:#}")));
            ctx.request_repaint();
        });
        slot
    }

    /// Who, what they say, and the price, then Go.
    fn speech_row(&mut self, ui: &mut egui::Ui, w: &mut SoundsWin, voiced: &[String]) {
        let before = (w.who.clone(), w.text.clone());
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("speech who")
                .selected_text(format!("@{}", w.who))
                .width(96.0)
                .show_ui(ui, |ui| {
                    for c in voiced {
                        ui.selectable_value(&mut w.who, c.clone(), format!("@{c}"));
                    }
                })
                .response
                .widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Who says it")
                });
            ui.add(
                egui::TextEdit::singleline(&mut w.text)
                    .hint_text("what they say")
                    .desired_width(f32::INFINITY),
            )
            .widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::TextInput, true, "What they say")
            });
        });
        // Another voice or other words need their own price.
        if (w.who.clone(), w.text.clone()) != before
            && matches!(w.speak, Speak::Priced(_) | Speak::Failed(_))
        {
            w.speak = Speak::Idle;
        }
        if let Some((_, name)) = &w.remake_speech {
            hint(ui, &format!("in place of {}", short(name, 60)));
        }
        let ready = !w.text.trim().is_empty();
        let free = self.changing.is_none();
        match go_buttons(
            ui,
            &w.speak,
            ready,
            free,
            w.at,
            ("the speech", "type what they say first"),
        ) {
            Some(Ask::Price) => w.speak = Speak::Asking(self.ask_speech(ui.ctx(), w, None)),
            Some(Ask::Go(cost)) => {
                w.speak = Speak::Making(self.ask_speech(ui.ctx(), w, Some(cost)));
            }
            Some(Ask::Cancel) => {
                w.speak = Speak::Idle;
                w.remake_speech = None;
            }
            None => {}
        }
    }

    /// Prices the speech, or with `at` makes it, off the UI thread.
    fn ask_speech(&self, ctx: &egui::Context, w: &SoundsWin, at: Option<String>) -> Slot {
        let slot: Slot = Arc::default();
        let Some(host) = self.host.clone() else {
            *slot.lock().unwrap() = Some(Err("can't make speech here".into()));
            return slot;
        };
        let (s, ctx, film) = (slot.clone(), ctx.clone(), self.project.root.clone());
        let (r, who, text) = (w.under.shot(), w.who.clone(), w.text.clone());
        std::thread::spawn(move || {
            let got = match at {
                None => host.speech_quote(&film, r, &who, &text),
                Some(at) => host.speech(&film, r, &who, &text, &at),
            };
            *s.lock().unwrap() = Some(got.map_err(|e| format!("{e:#}")));
            ctx.request_repaint();
        });
        slot
    }

    /// Picks up prices, and speech or effects that came back made.
    fn speech_news(&mut self, ctx: &egui::Context, w: &mut SoundsWin) {
        for effect in [false, true] {
            let state = if effect { &mut w.effect } else { &mut w.speak };
            let Some(job) = news(state) else { continue };
            self.reload();
            self.flush();
            let old = if effect {
                w.remake_effect.take()
            } else {
                w.remake_speech.take()
            };
            let old = old.and_then(|(id, _)| {
                self.project
                    .sounds(w.under)
                    .unwrap_or_default()
                    .into_iter()
                    .find(|(s, ..)| s.job == id)
            });
            self.apply(vec![Action::AddSoundJob(w.under, job.clone(), w.at)]);
            let main = self.main_edit();
            if let Some(old) = old {
                w.lift = self.unlay(ctx, (w.under, &w.name), old, main.as_ref());
            }
            if main.is_some() {
                self.lay(ctx, (w.under, &w.name), &job, main);
            }
            if effect {
                w.effect_text.clear();
            } else {
                w.text.clear();
            }
        }
    }

    /// Takes sound `s` off take `u`, and says which of the editor's clips to lift with it.
    fn unlay(
        &mut self,
        ctx: &egui::Context,
        (u, name): (Under, &str),
        (ts, job, path): (TakeSound, Job, PathBuf),
        main: Option<&Main>,
    ) -> Option<(String, String)> {
        if self.hearing(ctx, &job.id.to_string()) {
            self.hush();
        }
        self.flush();
        self.apply(vec![Action::RemoveSound(u, ts.job.clone())]);
        let clip = main?
            .2
            .audio
            .iter()
            .flat_map(|t| &t.clips)
            .find(|c| c.file.as_deref().is_some_and(|f| same(f, &path)))?;
        Some((clip.id.clone(), format!("{} off {name}", sound_name(&job))))
    }

    /// Fills in speech or an effect as `s` was made, to be priced and made again in its place.
    fn remake_sound(
        &mut self,
        ctx: &egui::Context,
        w: &mut SoundsWin,
        (ts, job, path): (TakeSound, Job, PathBuf),
        effects: &[(String, String)],
    ) {
        let text = job.prompt.clone().unwrap_or_default();
        w.at = ts.at;
        let old = Some((ts.job.clone(), sound_name(&job)));
        if let Some(who) = &job.who {
            w.who = who.clone();
            w.text = text;
            w.remake_speech = old;
            w.speak = Speak::Asking(self.ask_speech(ctx, w, None));
            return;
        }
        let Some(i) = effects
            .iter()
            .position(|(p, m)| *p == job.plugin && *m == job.model)
        else {
            w.effect = Speak::Failed(format!("{} · {} isn't installed", job.plugin, job.model));
            return;
        };
        w.model = i;
        w.effect_text = text;
        // As long as the one made, which also costs less than letting the model pick again.
        w.length = self
            .ff
            .as_ref()
            .and_then(|ff| ff.length(&path).ok())
            .map(|l| ((l * 10.0).round() / 10.0).clamp(0.5, 30.0))
            .unwrap_or(0.0);
        w.remake_effect = old;
        w.effect = Speak::Asking(self.ask_effect(ctx, w, &effects[i], None));
    }

    /// Puts sound `job`, already over `u`, in the editor's sequence.
    fn lay(
        &mut self,
        ctx: &egui::Context,
        (u, name): (Under, &str),
        job: &str,
        main: Option<Main>,
    ) {
        let Some(m) = main else { return };
        let Some((ts, j, file)) = self
            .project
            .sounds(u)
            .unwrap_or_default()
            .into_iter()
            .find(|(s, ..)| s.job == job)
        else {
            return;
        };
        let Some(clip) = self.clip_of(&m.2, u, ts.at) else {
            return;
        };
        let file = std::path::absolute(&file).unwrap_or(file);
        let what = format!("{} over {name}", sound_name(&j));
        self.sound_change(ctx, m, clip.id, what, Change::Sound { file, at: ts.at });
    }
}

/// A file's name, what was said, or what an effect was asked to sound like.
pub(crate) fn sound_name(j: &Job) -> String {
    match (&j.prompt, &j.who, &j.brief) {
        (Some(text), Some(who), _) => format!("@{who}: {text}"),
        (_, _, Some(b)) => b.clone(),
        (Some(text), None, None) => text.clone(),
        _ => j.id.to_string(),
    }
}

/// Speech, or an effect a plugin made; not a file brought in.
fn remakeable(j: &Job) -> bool {
    j.plugin != "shotline"
        && matches!(
            j.op,
            shotline_core::protocol::Op::AudioTts | shotline_core::protocol::Op::AudioSfx
        )
}

enum Ask {
    Price,
    Go(String),
    Cancel,
}

/// Price…, then Go or Cancel, or why not.
fn go_buttons(
    ui: &mut egui::Ui,
    state: &Speak,
    ready: bool,
    free: bool,
    at: f64,
    (name, not_ready): (&str, &str),
) -> Option<Ask> {
    let mut out = None;
    ui.horizontal(|ui| match state {
        Speak::Asking(_) => {
            ui.weak("asking the price…");
            ui.spinner();
        }
        Speak::Making(_) => {
            ui.weak("making it…");
            ui.spinner();
        }
        Speak::Priced(cost) => {
            if crate::spend_button(ui, &format!("Go · {cost}"))
                .on_hover_text(format!("makes it, from {at:.2} s into the take"))
                .clicked()
            {
                out = Some(Ask::Go(cost.clone()));
            } else if ui.button("Cancel").clicked() {
                out = Some(Ask::Cancel);
            }
        }
        Speak::Idle | Speak::Failed(_) => {
            if let Speak::Failed(e) = state {
                ui.label(RichText::new(short(e, 60)).color(theme::FLAG))
                    .on_hover_text(e);
            }
            let price = ui.add_enabled(ready && free, egui::Button::new("Price…"));
            price.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::Role::Button,
                    ready && free,
                    format!("Price {name}"),
                )
            });
            if price
                .on_hover_text("asking the price is free")
                .on_disabled_hover_text(not_ready)
                .clicked()
            {
                out = Some(Ask::Price);
            }
        }
    });
    out
}

/// A price that came back is kept; a job that came back made is handed on.
fn news(state: &mut Speak) -> Option<String> {
    let slot = match state {
        Speak::Asking(s) | Speak::Making(s) => s.clone(),
        _ => return None,
    };
    let got = slot.lock().unwrap().take()?;
    let making = matches!(state, Speak::Making(_));
    match got {
        Err(e) => {
            *state = Speak::Failed(e);
            None
        }
        Ok(cost) if !making => {
            *state = Speak::Priced(cost);
            None
        }
        Ok(job) => {
            *state = Speak::Idle;
            Some(job)
        }
    }
}

pub(crate) fn same(a: &Path, b: &Path) -> bool {
    let full = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    full(a) == full(b)
}
