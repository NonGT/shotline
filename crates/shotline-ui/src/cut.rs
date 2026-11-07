//! The Cut tab: the shots in play order, how they join, music over them, and renders.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use shotline_core::protocol::{ClipInfo, SequenceInfo, TrackInfo};
use shotline_core::{MatchAt, ShotRef, Transition};

use crate::edit::Action;
use crate::lanes::{self, Block, Hit, Lane};
use crate::{App, PAGE, hint, page, remove_button, short, theme};

/// The deck's title while it plays the cut.
const CUT: &str = "the cut";

#[derive(Clone, Default)]
pub(crate) enum Render {
    #[default]
    Idle,
    /// What it's doing now, e.g. "[2/5] 01-020#1".
    Running(String),
    Done(PathBuf, String),
    Failed(String),
}

impl App {
    pub(crate) fn cut(&mut self, ui: &mut egui::Ui) {
        self.render_finished();
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BAY))
            .show(ui, |ui| {
                page(ui, PAGE, |ui| {
                    self.deck(ui);
                    self.cut_switch(ui);
                    if let Some((file, seq)) = self.edit_sequence() {
                        self.edit_lanes(ui, &file, &seq);
                        return;
                    }
                    if self.film.cut.is_empty() {
                        hint(ui, "No shots yet. Add some in Shots.");
                        return;
                    }
                    let mut acts = vec![];
                    self.cut_actions(ui);
                    ui.add_space(10.0);
                    self.cut_lanes(ui);
                    ui.add_space(18.0);
                    self.joins(ui);
                    ui.add_space(18.0);
                    self.music(ui, &mut acts);
                    ui.add_space(18.0);
                    self.renders(ui);
                    if !acts.is_empty() {
                        self.flush();
                        self.apply(acts);
                    }
                });
            });
    }

    /// Play and Render, and how the render is going.
    fn cut_actions(&mut self, ui: &mut egui::Ui) {
        let files: Vec<PathBuf> = self
            .film
            .cut
            .iter()
            .filter_map(|c| c.video.clone())
            .collect();
        let missing: Vec<String> = self
            .film
            .cut
            .iter()
            .filter(|c| c.video.is_none())
            .map(|c| c.shot.to_string())
            .collect();
        let state = self.render.lock().unwrap().clone();
        let running = matches!(state, Render::Running(_));
        ui.horizontal(|ui| {
            if ui
                .add_enabled_ui(!files.is_empty(), |ui| {
                    crate::icon_button(ui, "Play the cut", crate::play_icon)
                })
                .inner
                .on_hover_text("play the cut: the takes as they are, back to back")
                .on_disabled_hover_text("no shot has a take selected yet")
                .clicked()
            {
                self.play(CUT, &files, false);
            }
            let why = if missing.is_empty() {
                String::new()
            } else {
                format!("no take selected for {}", missing.join(", "))
            };
            let can = missing.is_empty() && !running && self.host.is_some();
            if ui
                .add_enabled(can, egui::Button::new("Render"))
                .on_hover_text("one video with the joins and music below, into renders/; free")
                .on_disabled_hover_text(if running { "rendering already" } else { &why })
                .clicked()
            {
                self.start_render(ui.ctx());
            }
            match &state {
                Render::Running(step) => {
                    ui.spinner();
                    ui.weak(step);
                }
                Render::Failed(e) => {
                    ui.label(RichText::new(short(e, 80)).color(theme::FLAG))
                        .on_hover_text(e);
                }
                _ => {}
            }
        });
        if !missing.is_empty() {
            hint(
                ui,
                &format!(
                    "{} without a take: they're left out of Play and stop a render.",
                    plural(missing.len(), "shot")
                ),
            );
        }
    }

    /// The selected takes back to back on V1, as Play plays them, and the music under them.
    fn cut_lanes(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let at = self.timeline_form.colormatch.at;
        let shot_lengths: HashMap<ShotRef, f64> = self
            .film
            .chapters
            .iter()
            .flat_map(|(_, s)| s)
            .map(|(r, s)| (*r, s.duration.unwrap_or(5.0)))
            .collect();
        let mut blocks = vec![];
        let mut end = 0.0;
        let cut = self.film.cut.clone();
        for (i, c) in cut.iter().enumerate() {
            let (Some(video), Some(take)) = (&c.video, &c.take) else {
                continue;
            };
            let length = self
                .clip_length(&ctx, video)
                .unwrap_or(shot_lengths.get(&c.shot).copied().unwrap_or(5.0));
            let still = take.output().map(|j| {
                self.project
                    .root
                    .join(".shotline/cache/posters")
                    .join(format!("{}.png", j.id))
            });
            if let Some(png) = &still
                && !png.is_file()
            {
                crate::make_poster(&ctx, (self.ff.as_ref(), &self.posters), video, png);
            }
            let mut hover = vec![match c.transition {
                Transition::Cut => "on a cut".to_string(),
                Transition::Continue => "from the shot before".to_string(),
            }];
            if c.colormatch(at, i == 0) {
                hover.push("colour matched".into());
            }
            hover.push("click to open the shot".into());
            blocks.push(Block {
                name: take.to_string(),
                start: end,
                length,
                still,
                hover: hover.join("\n"),
                enabled: true,
                shot: Some(c.shot),
            });
            end += length;
        }
        let video = vec![Lane {
            name: "V1".into(),
            blocks,
            sound: false,
            enabled: true,
        }];
        let tracks = self.timeline_form.tracks.clone();
        let mut audio = vec![];
        for (i, tr) in tracks.iter().enumerate() {
            let path = self.project.root.join(&tr.file);
            let length = self
                .clip_length(&ctx, &path)
                .unwrap_or((end - tr.at).max(1.0));
            end = f64::max(end, tr.at + length);
            audio.push(Lane {
                name: format!("A{}", i + 1),
                blocks: vec![Block {
                    name: tr.file.clone(),
                    start: tr.at,
                    length,
                    still: None,
                    hover: format!("volume {:.2}", tr.volume),
                    enabled: tr.volume > 0.0,
                    shot: None,
                }],
                sound: true,
                enabled: true,
            });
        }
        let playhead = self
            .deck
            .as_ref()
            .filter(|d| d.title() == CUT)
            .map(|d| d.position());
        match lanes::show(ui, "cut", &video, &audio, end, playhead, &mut self.thumbs) {
            Some(Hit::Seek(t)) => {
                if self.deck.as_ref().is_none_or(|d| d.title() != CUT) {
                    let files: Vec<PathBuf> = cut.iter().filter_map(|c| c.video.clone()).collect();
                    self.play(CUT, &files, false);
                }
                if let Some(d) = &mut self.deck {
                    d.seek(t, self.audio.as_ref());
                }
            }
            Some(Hit::Open(r)) => {
                self.tab = crate::Tab::Shots;
                self.open_shot(r);
            }
            None => {}
        }
    }

    /// Picks up what the editor plugins said, and asks them again once the film was read again.
    fn refresh_edits(&mut self, ctx: &egui::Context) {
        if let Some(slot) = &self.edits_asked {
            let Some(got) = slot.lock().unwrap().take() else {
                return;
            };
            self.edits_asked = None;
            let mut takes = HashMap::new();
            for e in &got.0 {
                for c in e
                    .sequences
                    .iter()
                    .flat_map(|s| s.video.iter().chain(&s.audio))
                {
                    for f in c.clips.iter().filter_map(|c| c.file.as_ref()) {
                        if !takes.contains_key(f)
                            && let Some(t) = self.project.take_of_file(f)
                        {
                            takes.insert(f.clone(), t);
                        }
                    }
                }
            }
            self.edits = got;
            self.edit_takes = takes;
        }
        if !self.edits_stale {
            return;
        }
        let Some(host) = self.host.clone() else {
            return;
        };
        self.edits_stale = false;
        let slot: Arc<Mutex<Option<_>>> = Arc::default();
        self.edits_asked = Some(slot.clone());
        let (film, ctx) = (self.project.root.clone(), ctx.clone());
        std::thread::spawn(move || {
            *slot.lock().unwrap() = Some(host.sequences(&film));
            ctx.request_repaint();
        });
    }

    /// The editor sequence picked to show, while it's still there.
    fn edit_sequence(&self) -> Option<(PathBuf, SequenceInfo)> {
        let (file, name) = self.cut_view.as_ref()?;
        let edit = self.edits.0.iter().find(|e| e.file == *file)?;
        let seq = edit.sequences.iter().find(|s| s.name == *name)?;
        Some((file.clone(), seq.clone()))
    }

    /// The film's own cut, or a sequence of a video editor's project, like FilmCraft's in
    /// shared/.
    fn cut_switch(&mut self, ui: &mut egui::Ui) {
        self.refresh_edits(ui.ctx());
        let (edits, problems) = &self.edits;
        for p in problems {
            ui.label(RichText::new(short(p, 100)).color(theme::FLAG))
                .on_hover_text(p);
        }
        if edits.is_empty() {
            hint(
                ui,
                "A FilmCraft project saved in shared/ shows its sequences here, next to this cut.",
            );
            ui.add_space(6.0);
            return;
        }
        let several = edits.len() > 1;
        let now = self.edit_sequence().map(|(f, s)| (f, s.name));
        let mut pick = None;
        ui.horizontal_wrapped(|ui| {
            if ui
                .selectable_label(now.is_none(), "This film's cut")
                .on_hover_text("the selected takes in shot order, as shotline renders them")
                .clicked()
            {
                pick = Some(None);
            }
            ui.separator();
            for e in edits {
                let file = e.file.file_name().unwrap_or_default().to_string_lossy();
                if let Some(why) = &e.error {
                    ui.label(RichText::new(format!("{file} can't be read")).color(theme::FLAG))
                        .on_hover_text(why);
                    continue;
                }
                for seq in &e.sequences {
                    let label = if several {
                        format!("{file} · {}", seq.name)
                    } else {
                        seq.name.clone()
                    };
                    let on = now
                        .as_ref()
                        .is_some_and(|(f, s)| *f == e.file && *s == seq.name);
                    if ui
                        .selectable_label(on, label)
                        .on_hover_text(format!("{} in {}", seq.name, self.project.rel(&e.file)))
                        .clicked()
                    {
                        pick = Some(Some((e.file.clone(), seq.name.clone())));
                    }
                }
                if e.sequences.is_empty() {
                    ui.weak(format!("{file} has no sequences"));
                }
            }
        });
        if let Some(p) = pick {
            self.cut_view = p;
        }
        ui.add_space(10.0);
    }

    /// An editor's sequence on its tracks, read only. Clips that are takes go by the take's name.
    fn edit_lanes(&mut self, ui: &mut egui::Ui, file: &Path, seq: &SequenceInfo) {
        let ctx = ui.ctx().clone();
        let fps = if seq.fps.fract() == 0.0 {
            format!("{}", seq.fps)
        } else {
            format!("{:.3}", seq.fps)
        };
        hint(
            ui,
            &format!(
                "{} · {} × {} · {fps} fps · {}. Read only: it's edited in its own app.",
                self.project.rel(file),
                seq.width,
                seq.height,
                crate::player::clock(seq.length()),
            ),
        );
        ui.add_space(8.0);
        let lanes_of = |tracks: &[TrackInfo], sound: bool| -> Vec<Lane> {
            tracks
                .iter()
                .enumerate()
                .map(|(i, t)| Lane {
                    name: format!("{}{}", if sound { "A" } else { "V" }, i + 1),
                    blocks: t
                        .clips
                        .iter()
                        .map(|c| self.edit_block(&ctx, c, sound))
                        .collect(),
                    sound,
                    enabled: t.enabled,
                })
                .collect()
        };
        let video = lanes_of(&seq.video, false);
        let audio = lanes_of(&seq.audio, true);
        if let Some(Hit::Open(r)) = lanes::show(
            ui,
            "edit",
            &video,
            &audio,
            seq.length(),
            None,
            &mut self.thumbs,
        ) {
            self.tab = crate::Tab::Shots;
            self.open_shot(r);
        }
    }

    fn edit_block(&self, ctx: &egui::Context, c: &ClipInfo, sound: bool) -> Block {
        let take = c.file.as_ref().and_then(|f| self.edit_takes.get(f));
        let mut hover = vec![];
        if let Some(f) = &c.file {
            hover.push(self.project.rel(f));
        }
        if c.from > 0.0 {
            hover.push(format!("from {:.2} s into it", c.from));
        }
        if c.speed != 1.0 {
            hover.push(format!("at {:.0}% speed", c.speed * 100.0));
        }
        if !c.enabled {
            hover.push("turned off".into());
        }
        if take.is_some() {
            hover.push("click to open the shot".into());
        }
        let still = match (&c.file, take.and_then(|t| t.output())) {
            _ if sound => None,
            (Some(f), Some(job)) if f.is_file() => Some((
                f.clone(),
                self.project
                    .root
                    .join(".shotline/cache/posters")
                    .join(format!("{}.png", job.id)),
            )),
            (Some(f), None) if f.is_file() && is_video(f) => {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                f.hash(&mut h);
                Some((
                    f.clone(),
                    self.project
                        .root
                        .join(".shotline/cache/posters")
                        .join(format!("file-{:016x}.png", h.finish())),
                ))
            }
            _ => None,
        };
        if let Some((video, png)) = &still
            && !png.is_file()
        {
            crate::make_poster(ctx, (self.ff.as_ref(), &self.posters), video, png);
        }
        Block {
            name: take.map_or_else(|| c.name.clone(), |t| t.to_string()),
            start: c.start,
            length: c.length,
            still: still.map(|(_, png)| png),
            hover: hover.join("\n"),
            enabled: c.enabled,
            shot: take.map(|t| t.shot),
        }
    }

    /// How long a clip plays, once ffprobe has said.
    fn clip_length(&self, ctx: &egui::Context, path: &Path) -> Option<f64> {
        let mut known = self.clip_lengths.lock().unwrap();
        if let Some(l) = known.get(path) {
            return *l;
        }
        known.insert(path.to_path_buf(), None);
        if let Some(ff) = self.ff.clone() {
            let (lengths, path, ctx) = (self.clip_lengths.clone(), path.to_path_buf(), ctx.clone());
            std::thread::spawn(move || {
                let l = ff.audio_duration(&path).ok();
                lengths.lock().unwrap().insert(path, l);
                ctx.request_repaint();
            });
        }
        None
    }

    /// Crossfade and colour matching, saved in timeline.toml once left.
    fn joins(&mut self, ui: &mut egui::Ui) {
        heading(ui, "Joins");
        let t = &mut self.timeline_form;
        egui::Grid::new("joins")
            .num_columns(2)
            .spacing(egui::vec2(12.0, 8.0))
            .show(ui, |ui| {
                let l = ui.weak("Crossfade");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut t.xfade)
                            .range(0.0..=3.0)
                            .speed(0.05)
                            .fixed_decimals(2)
                            .suffix(" s"),
                    )
                    .labelled_by(l.id);
                    ui.weak(if t.xfade == 0.0 {
                        "straight cuts"
                    } else {
                        "each shot fades into the next"
                    });
                });
                ui.end_row();

                let l = ui.weak("Colour match");
                let name = |m: MatchAt| match m {
                    MatchAt::Continue => "shots that continue",
                    MatchAt::All => "every join",
                    MatchAt::None => "none",
                };
                egui::ComboBox::from_id_salt("colormatch")
                    .selected_text(name(t.colormatch.at))
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for m in [MatchAt::Continue, MatchAt::All, MatchAt::None] {
                            ui.selectable_value(&mut t.colormatch.at, m, name(m));
                        }
                    })
                    .response
                    .labelled_by(l.id);
                ui.end_row();

                if t.colormatch.at != MatchAt::None {
                    let l = ui.weak("Strength");
                    ui.add(
                        egui::DragValue::new(&mut t.colormatch.strength)
                            .range(0.0..=1.0)
                            .speed(0.01)
                            .fixed_decimals(2),
                    )
                    .labelled_by(l.id)
                    .on_hover_text("how much of the correction is applied, 0 to 1");
                    ui.end_row();
                    let l = ui.weak("Frames");
                    ui.add(
                        egui::DragValue::new(&mut t.colormatch.frames)
                            .range(1..=60)
                            .speed(0.2),
                    )
                    .labelled_by(l.id)
                    .on_hover_text("frames each side of a join that the colours are measured on");
                    ui.end_row();
                }
            });
    }

    fn music(&mut self, ui: &mut egui::Ui, acts: &mut Vec<Action>) {
        heading(ui, "Music");
        let t = &mut self.timeline_form;
        if t.tracks.is_empty() {
            hint(
                ui,
                "Nothing plays over the cut. Music and effects mix in when it's rendered.",
            );
        }
        let mut remove = None;
        egui::Grid::new("tracks")
            .num_columns(4)
            .spacing(egui::vec2(12.0, 8.0))
            .show(ui, |ui| {
                for (i, tr) in t.tracks.iter_mut().enumerate() {
                    let name = ui.label(RichText::new(&tr.file).color(theme::PAPER));
                    ui.horizontal(|ui| {
                        ui.weak("starts at");
                        ui.add(
                            egui::DragValue::new(&mut tr.at)
                                .range(0.0..=36000.0)
                                .speed(0.1)
                                .fixed_decimals(1)
                                .suffix(" s"),
                        )
                        .labelled_by(name.id);
                    });
                    ui.horizontal(|ui| {
                        ui.weak("volume");
                        ui.add(
                            egui::DragValue::new(&mut tr.volume)
                                .range(0.0..=2.0)
                                .speed(0.01)
                                .fixed_decimals(2),
                        )
                        .labelled_by(name.id);
                    });
                    if remove_button(ui)
                        .on_hover_text(format!("{} stops playing over the cut", tr.file))
                        .clicked()
                    {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
        if let Some(i) = remove {
            t.tracks.remove(i);
        }
        if ui.button("Add music…").clicked()
            && let Some(f) = rfd::FileDialog::new()
                .add_filter("Audio", &["mp3", "wav", "m4a", "aac", "flac", "ogg"])
                .pick_file()
        {
            acts.push(Action::AddTrack(f));
        }
    }

    fn renders(&mut self, ui: &mut egui::Ui) {
        heading(ui, "Renders");
        if self.film.renders.is_empty() {
            hint(
                ui,
                "None yet. Render puts them in renders/, which git leaves out.",
            );
            return;
        }
        let mut play = None;
        let mut open = None;
        egui::Grid::new("renders")
            .num_columns(3)
            .spacing(egui::vec2(12.0, 6.0))
            .show(ui, |ui| {
                for f in &self.film.renders {
                    let name = self.project.rel(f);
                    ui.label(RichText::new(&name).color(theme::GRAPHITE));
                    if crate::icon_button(ui, &format!("Play {name}"), crate::play_icon)
                        .on_hover_text(format!("play {name}"))
                        .clicked()
                    {
                        play = Some(f.clone());
                    }
                    if self.viewer.is_some()
                        && ui
                            .button("Open")
                            .on_hover_text("full size in mpv or ffplay")
                            .clicked()
                    {
                        open = Some(f.clone());
                    }
                    ui.end_row();
                }
            });
        if let Some(f) = play {
            let name = self.project.rel(&f);
            self.play(&name, &[f], false);
        }
        if let Some(f) = open {
            self.open_file(&f);
        }
    }

    fn start_render(&mut self, ctx: &egui::Context) {
        // What's on screen is what renders.
        self.flush();
        let Some(host) = self.host.clone() else {
            return;
        };
        *self.render.lock().unwrap() = Render::Running("starting".into());
        let (state, ctx, film) = (self.render.clone(), ctx.clone(), self.project.root.clone());
        std::thread::spawn(move || {
            let step = state.clone();
            let c = ctx.clone();
            let mut on = move |s: String| {
                *step.lock().unwrap() = Render::Running(s);
                c.request_repaint();
            };
            let done = match host.render(&film, &mut on) {
                Ok((f, about)) => Render::Done(f, about),
                Err(e) => Render::Failed(format!("{e:#}")),
            };
            *state.lock().unwrap() = done;
            ctx.request_repaint();
        });
    }

    /// Once a render is done: say so, list it, and play it.
    fn render_finished(&mut self) {
        let done = {
            let mut s = self.render.lock().unwrap();
            match &*s {
                Render::Done(..) => std::mem::take(&mut *s),
                _ => return,
            }
        };
        let Render::Done(f, about) = done else { return };
        self.reload();
        self.info(format!("{} · {about}", self.project.rel(&f)));
    }
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .text_style(egui::TextStyle::Heading)
            .color(theme::PAPER),
    );
    ui.add_space(4.0);
}

fn plural(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

fn is_video(f: &Path) -> bool {
    f.extension().and_then(|x| x.to_str()).is_some_and(|x| {
        ["mp4", "mov", "m4v", "webm", "mkv", "avi", "mxf"]
            .contains(&x.to_ascii_lowercase().as_str())
    })
}
