//! The Cut tab: the shots in play order, how they join, music over them, and renders.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use shotline_core::protocol::{
    ClipInfo, ExportParams, OpenParams, SequenceInfo, TakeFile, TrackInfo,
};
use shotline_core::{MatchAt, ShotRef, Transition};

use crate::edit::Action;
use crate::lanes::{self, Block, Hit, Lane};
use crate::player::{Deck, Player};
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
        // The Shots page's player keeps quiet here; this page has its own.
        if let Some(d) = &mut self.deck
            && d.is_playing()
        {
            d.pause(self.audio.as_ref());
        }
        let edge = egui::Margin::symmetric(PAGE.x as i8, 12);
        let mut editor = None;
        egui::Panel::top("cut-head")
            .frame(egui::Frame::new().fill(theme::BAY).inner_margin(edge))
            .show(ui, |ui| {
                self.cut_switch(ui);
                editor = self.edit_sequence();
                if let Some((file, seq)) = &editor {
                    self.edit_head(ui, file, seq);
                } else if !self.film.cut.is_empty() && !self.reading_edits() {
                    self.cut_actions(ui);
                }
            });
        if let Some((file, seq)) = editor {
            self.edit_lanes(ui, &file, &seq);
            return;
        }
        if self.reading_edits() || self.film.cut.is_empty() {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(theme::BAY))
                .show(ui, |ui| {
                    page(ui, PAGE, |ui| {
                        if self.film.cut.is_empty() && !self.reading_edits() {
                            hint(ui, "No shots yet. Add some in Shots.");
                        } else {
                            // Not shotline's cut for a moment while the editor's is read.
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.weak("reading the editor's project…");
                            });
                        }
                    });
                });
            return;
        }
        let files: Vec<PathBuf> = self
            .film
            .cut
            .iter()
            .filter_map(|c| c.video.clone())
            .collect();
        self.cut_deck_on(CUT, &files);
        let mut acts = vec![];
        egui::Panel::right("cut-side")
            .resizable(true)
            .default_size(340.0)
            .frame(egui::Frame::new().fill(theme::RACK))
            .show(ui, |ui| {
                page(ui, egui::vec2(16.0, 12.0), |ui| {
                    self.joins(ui);
                    ui.add_space(18.0);
                    self.music(ui, &mut acts);
                    ui.add_space(18.0);
                    self.renders(ui);
                });
            });
        egui::Panel::bottom("cut-timeline")
            .frame(egui::Frame::new().fill(theme::RACK).inner_margin(edge))
            .show(ui, |ui| self.cut_lanes(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BAY).inner_margin(edge))
            .show(ui, |ui| self.show_cut_deck(ui, CUT, None));
        if !acts.is_empty() {
            self.flush();
            self.apply(acts);
        }
    }

    fn reading_edits(&self) -> bool {
        self.edits.0.is_empty()
            && self.edits_asked.is_some()
            && self.plugins.iter().any(|d| d.editor)
    }

    /// Play and Render, and how the render is going.
    fn cut_actions(&mut self, ui: &mut egui::Ui) {
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
                id: None,
                picked: false,
            });
            end += length;
        }
        let video = vec![Lane {
            name: "V1".into(),
            blocks,
            sound: false,
            enabled: true,
            joins: vec![],
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
                    id: None,
                    picked: false,
                }],
                sound: true,
                enabled: true,
                joins: vec![],
            });
        }
        let playhead = self.shown_deck(CUT).map(|d| d.position());
        match lanes::show(ui, "cut", &video, &audio, end, playhead, &mut self.thumbs) {
            Some(Hit::Seek(t)) => self.cut_seek(ui.ctx(), CUT, t, false),
            Some(Hit::Scrub(t)) => self.cut_seek(ui.ctx(), CUT, t, true),
            Some(Hit::Open(r)) => {
                self.tab = crate::Tab::Shots;
                self.open_shot(r);
            }
            Some(Hit::Pick(_) | Hit::PickJoin(_)) | None => {}
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
        self.watch_edits(ctx);
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

    /// Asks the editors again once something in shared/, or one of their files, was saved.
    fn watch_edits(&mut self, ctx: &egui::Context) {
        const EVERY: std::time::Duration = std::time::Duration::from_secs(1);
        ctx.request_repaint_after(EVERY);
        let now = std::time::Instant::now();
        if self
            .edits_seen
            .is_some_and(|(_, at)| now.duration_since(at) < EVERY)
        {
            return;
        }
        let mut files: Vec<PathBuf> = self.edits.0.iter().map(|e| e.file.clone()).collect();
        let shared = self.project.root.join("shared");
        let mut dirs = vec![shared];
        while let Some(dir) = dirs.pop() {
            for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = e.path();
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    dirs.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files.sort();
        files.dedup();
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for f in &files {
            f.hash(&mut h);
            if let Ok(m) = f.metadata() {
                m.len().hash(&mut h);
                m.modified().ok().hash(&mut h);
            }
        }
        let seen = h.finish();
        if self.edits_seen.is_some_and(|(was, _)| was != seen) {
            self.edits_stale = true;
        }
        self.edits_seen = Some((seen, now));
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
        self.editor_opened();
        // Whole, so a fix's link and steps can be read and copied.
        for p in &self.edits.1 {
            ui.label(RichText::new(p).color(theme::FLAG));
        }
        let editors: Vec<(String, String)> = self
            .plugins
            .iter()
            .filter(|d| d.editor)
            .map(|d| {
                let app = if d.app.is_empty() { &d.name } else { &d.app };
                (d.name.clone(), app.clone())
            })
            .collect();
        let edits = &self.edits.0;
        if edits.is_empty() && editors.is_empty() {
            hint(
                ui,
                "With an editor plugin like filmcraft, this cut opens in a video editor and \
                 its sequences show here.",
            );
            ui.add_space(6.0);
            return;
        }
        // With an editor project, its cut is the film's cut; shotline's own only starts it.
        let main = main_sequence(&self.project.root, edits);
        if self.edit_sequence().is_none() {
            self.cut_view = main.clone();
        }
        let edits = &self.edits.0;
        let several = edits.len() > 1;
        let sequences = edits.iter().map(|e| e.sequences.len()).sum::<usize>();
        let now = self.edit_sequence().map(|(f, s)| (f, s.name));
        let mut pick = None;
        // Only when there's something to pick or to say about a project.
        let row = sequences > 1
            || edits
                .iter()
                .any(|e| e.error.is_some() || e.sequences.is_empty());
        if row {
            ui.horizontal_wrapped(|ui| {
                for e in edits {
                    let file = e.file.file_name().unwrap_or_default().to_string_lossy();
                    if let Some(why) = &e.error {
                        ui.label(RichText::new(format!("{file} can't be read")).color(theme::FLAG))
                            .on_hover_text(why);
                        continue;
                    }
                    if e.sequences.is_empty() {
                        ui.weak(format!("{file} has no sequences"));
                    }
                    if sequences < 2 {
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
                            pick = Some((e.file.clone(), seq.name.clone()));
                        }
                    }
                    if several {
                        ui.separator();
                    }
                }
            });
        }
        if let Some(p) = pick {
            self.cut_view = Some(p);
        }
        let none_yet = main.is_none();
        // Shown with the editor's cut instead, by its title.
        if none_yet {
            ui.horizontal(|ui| self.open_buttons(ui));
        }
        if none_yet {
            ui.add_space(4.0);
            hint(
                ui,
                "Once it's saved in the film's shared/ folder, the editor's cut is the film's \
                 cut: it shows here in place of this one, and both change it.",
            );
        }
        if row || none_yet || !self.edits.1.is_empty() {
            ui.add_space(10.0);
        }
    }

    /// "Open in FilmCraft" for each editor plugin: the shown sequence, or a new project.
    fn open_buttons(&mut self, ui: &mut egui::Ui) {
        let editors: Vec<(String, String)> = self
            .plugins
            .iter()
            .filter(|d| d.editor)
            .map(|d| {
                let app = if d.app.is_empty() { &d.name } else { &d.app };
                (d.name.clone(), app.clone())
            })
            .collect();
        let now = self.edit_sequence().map(|(f, s)| (f, s.name));
        let edits = &self.edits.0;
        let mut open = None;
        let opening = self.edit_opening.is_some();
        for (plugin, app) in &editors {
            let shown = now
                .as_ref()
                .filter(|(f, _)| edits.iter().any(|e| e.file == *f && e.plugin == *plugin));
            let has_own = edits.iter().any(|e| e.plugin == *plugin);
            let hover = match (shown, has_own) {
                (Some(_), _) => format!(
                    "change this sequence in {app}; it shows here again each time it's saved"
                ),
                (None, true) => format!("open the film's {app} project"),
                (None, false) => format!(
                    "start a {app} project from this cut, in shared/{plugin}/; its \
                     sequences then show here, and again each time it's saved"
                ),
            };
            if ui
                .add_enabled(!opening, egui::Button::new(format!("Open in {app}")))
                .on_hover_text(hover)
                .on_disabled_hover_text("opening it…")
                .clicked()
            {
                open = Some((plugin.clone(), app.clone(), shown.cloned()));
            }
        }
        if opening {
            ui.spinner();
        }

        if let Some((plugin, app, shown)) = open {
            self.open_in_editor(ui.ctx(), plugin, app, shown);
        }
    }

    /// An editor's sequence on its tracks, read only. Clips that are takes go by the take's name.
    /// The sequence's name and what it is, what to do with it, and what the preview leaves out.
    fn edit_head(&mut self, ui: &mut egui::Ui, file: &Path, seq: &SequenceInfo) {
        let fps = if seq.fps.fract() == 0.0 {
            format!("{}", seq.fps)
        } else {
            format!("{:.3}", seq.fps)
        };
        let (plugin, app) = self
            .edits
            .0
            .iter()
            .find(|e| e.file == file)
            .map(|e| (e.plugin.clone(), e.app.clone()))
            .unwrap_or_default();
        // Its name and what it is on the left, what to do with it on the right.
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&seq.name)
                    .size(18.0)
                    .color(theme::PAPER)
                    .strong(),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!(
                    "{} × {} · {fps} fps · {}",
                    seq.width,
                    seq.height,
                    crate::player::clock(seq.length()),
                ))
                .color(theme::DUST),
            )
            .on_hover_text(self.project.rel(file));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if !plugin.is_empty() {
                    self.edit_render(ui, &plugin, file, seq);
                }
                self.open_buttons(ui);
            });
        });
        ui.label(
            RichText::new(self.project.rel(file))
                .small()
                .color(theme::DUST),
        );
        self.take_sync(ui, &app, seq);
        let out = self.preview_of(seq);
        let full = full_of(&out);
        let making = self.full_making.as_ref().is_some_and(|(f, _)| *f == full);
        let failed = self.full_failed.as_ref().filter(|(f, _)| *f == full);
        ui.horizontal(|ui| {
            // Always there, so the page doesn't move when it says something.
            // A spinner is as tall as a button.
            ui.set_min_height(ui.spacing().interact_size.y);
            if full.is_file() {
                ui.weak(format!("Preview rendered by {app}"));
            } else if out.is_file() {
                ui.weak("Quick preview: LUTs and dissolves, no other effects")
                    .on_hover_text(format!(
                        "{app} renders the whole thing once the sequence stays the same for a \
                         few seconds while shotline is in front"
                    ));
            }
            if making {
                ui.spinner();
                ui.weak(format!("{app} is rendering the full preview…"));
            } else if let Some((_, e)) = failed {
                ui.label(
                    RichText::new(format!(
                        "{app} couldn't render a preview: {}",
                        short(e, 160)
                    ))
                    .color(theme::FLAG),
                )
                .on_hover_text(e.as_str());
            }
        });
        if let Some((_, skipped)) = self.seq_playing.as_ref().filter(|(p, _)| *p == out)
            && !skipped.is_empty()
        {
            ui.label(
                RichText::new(format!(
                    "Left out of the preview: {}",
                    short(&skipped.join("; "), 200)
                ))
                .color(theme::DUST),
            )
            .on_hover_text(skipped.join("\n"));
        }
    }

    /// The sequence's tracks along the bottom, and its preview above them as big as fits.
    fn edit_lanes(&mut self, ui: &mut egui::Ui, file: &Path, seq: &SequenceInfo) {
        let ctx = ui.ctx().clone();
        let lanes_of = |tracks: &[TrackInfo], sound: bool| -> Vec<Lane> {
            tracks
                .iter()
                .enumerate()
                .map(|(i, t)| Lane {
                    name: format!("{}{}", if sound { "A" } else { "V" }, i + 1),
                    blocks: t
                        .clips
                        .iter()
                        .map(|c| {
                            let mut b = self.edit_block(&ctx, c, sound);
                            if !sound && !c.id.is_empty() {
                                b.picked = self.clip_pick.as_ref() == Some(&c.id);
                                b.id = Some(c.id.clone());
                            }
                            b
                        })
                        .collect(),
                    sound,
                    enabled: t.enabled,
                    joins: if sound {
                        vec![]
                    } else {
                        self.joins_of(t, seq.fps)
                    },
                })
                .collect()
        };
        let video = lanes_of(&seq.video, false);
        let audio = lanes_of(&seq.audio, true);
        let out = self.preview_of(seq);
        self.sequence_made();
        let title = seq_title(file, seq);
        // The player shows a preview: made once for what the sequence is now, and in the
        // meantime the one before, if any.
        let full = full_of(&out);
        self.full_preview(&ctx, &plugin_of(&self.edits.0, file), file, seq, &out);
        let mut note = None;
        if full.is_file() {
            self.cut_deck_on(&title, std::slice::from_ref(&full));
        } else if out.is_file() {
            self.cut_deck_on(&title, std::slice::from_ref(&out));
        } else {
            let failed = self
                .seq_failed
                .as_ref()
                .filter(|(f, _)| *f == out)
                .map(|(_, e)| format!("can't make a preview to play: {}", short(e, 200)));
            if failed.is_none() && self.seq_making.is_none() && seq.length() > 0.0 {
                self.make_preview(&ctx, seq, &out);
            }
            note = Some(failed.unwrap_or_else(|| "making a preview to play…".into()));
        }
        let edge = egui::Margin::symmetric(PAGE.x as i8, 12);
        egui::Panel::bottom("edit-timeline")
            .frame(egui::Frame::new().fill(theme::RACK).inner_margin(edge))
            .show(ui, |ui| {
                // Above the tracks, so picking a clip doesn't move them.
                ui.vertical(|ui| {
                    ui.set_min_height(28.0);
                    self.clip_tools(ui, file, seq);
                });
                ui.add_space(6.0);
                let playhead = self.shown_deck(&title).map(|d| d.position());
                match lanes::show(
                    ui,
                    "edit",
                    &video,
                    &audio,
                    seq.length(),
                    playhead,
                    &mut self.thumbs,
                ) {
                    Some(Hit::Open(r)) => {
                        self.tab = crate::Tab::Shots;
                        self.open_shot(r);
                    }
                    Some(Hit::Seek(t)) => self.cut_seek(&ctx, &title, t, false),
                    Some(Hit::Scrub(t)) => self.cut_seek(&ctx, &title, t, true),
                    Some(Hit::Pick(id)) => {
                        self.join_pick = None;
                        self.clip_pick = (self.clip_pick.as_ref() != Some(&id)).then_some(id);
                    }
                    Some(Hit::PickJoin(id)) => {
                        self.clip_pick = None;
                        self.join_pick = (self.join_pick.as_ref() != Some(&id)).then_some(id);
                    }
                    _ => {}
                }
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BAY).inner_margin(edge))
            .show(ui, |ui| self.show_cut_deck(ui, &title, note.as_deref()));
    }

    /// Asks the editor plugin to open the film's project there, at the sequence shown, or to
    /// make one from this cut when there's none yet.
    fn open_in_editor(
        &mut self,
        ctx: &egui::Context,
        plugin: String,
        app: String,
        shown: Option<(PathBuf, String)>,
    ) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let takes: Vec<(String, PathBuf)> = self
            .film
            .cut
            .iter()
            .filter_map(|c| Some((c.take.as_ref()?.to_string(), c.video.clone()?)))
            .collect();
        let music: Vec<(String, PathBuf, f64)> = self
            .timeline_form
            .tracks
            .iter()
            .filter(|t| t.volume > 0.0)
            .map(|t| (t.file.clone(), self.project.root.join(&t.file), t.at))
            .collect();
        let has_own = self.edits.0.iter().any(|e| e.plugin == plugin);
        let (ff, project) = (self.ff.clone(), self.project.clone());
        // The editor runs elsewhere, so no paths relative to here.
        let film = std::path::absolute(&project.root).unwrap_or_else(|_| project.root.clone());
        let slot: crate::Opened = Arc::default();
        self.edit_opening = Some((app, slot.clone()));
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = (|| {
                let mut ask = OpenParams {
                    film: film.clone(),
                    ..OpenParams::default()
                };
                match shown {
                    Some((file, seq)) => {
                        ask.file = Some(file);
                        ask.sequence = Some(seq);
                    }
                    None if !has_own && !takes.is_empty() => {
                        let ff = ff.ok_or("can't measure the takes without ffmpeg")?;
                        ask.cut = Some(own_cut(&ff, &film, &takes, &music)?);
                        ask.takes = all_takes(&project);
                    }
                    None => {}
                }
                host.open_edit(&plugin, &ask).map_err(|e| e.to_string())
            })();
            *slot.lock().unwrap() = Some(r);
            ctx.request_repaint();
        });
    }

    fn editor_opened(&mut self) {
        let Some((app, slot)) = &self.edit_opening else {
            return;
        };
        let Some(r) = slot.lock().unwrap().take() else {
            return;
        };
        let app = app.clone();
        self.edit_opening = None;
        if let Err(e) = r {
            self.error(format!("can't open it in {app}: {e}"));
        }
    }

    /// Where a sequence's preview goes: named by what's in it, so a changed sequence or a
    /// changed file gets a new one.
    fn preview_of(&self, seq: &SequenceInfo) -> PathBuf {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        // Up when previews are made differently, so old ones aren't shown.
        2u32.hash(&mut h);
        serde_json::to_string(seq).unwrap_or_default().hash(&mut h);
        for c in seq.video.iter().chain(&seq.audio).flat_map(|t| &t.clips) {
            for f in c.file.iter().chain(&c.luts) {
                if let Ok(m) = f.metadata() {
                    m.len().hash(&mut h);
                    m.modified().ok().hash(&mut h);
                }
            }
        }
        self.project
            .root
            .join(".shotline/cache/sequences")
            .join(format!("{:016x}.mp4", h.finish()))
    }

    /// Plays the preview from `at`, once it's made.
    /// Has ffmpeg make the sequence's preview, off the UI thread.
    fn make_preview(&mut self, ctx: &egui::Context, seq: &SequenceInfo, out: &Path) {
        let Some(ff) = self.ff.clone() else {
            return;
        };
        let media = to_media(seq);
        let slot: Made = Arc::default();
        self.seq_making = Some(Making {
            out: out.to_path_buf(),
            left_out: left_out(seq),
            slot: slot.clone(),
        });
        let (out, ctx) = (out.to_path_buf(), ctx.clone());
        std::thread::spawn(move || {
            let r = ff.sequence_preview(&media, &out).map_err(|e| e.to_string());
            *slot.lock().unwrap() = Some(r);
            ctx.request_repaint();
        });
    }

    /// Has the editor render the sequence as the player's preview, once it has stayed the same
    /// for a few seconds while shotline is in front (so not while it's being worked on in the
    /// editor), and nothing else is asking the editor for something.
    fn full_preview(
        &mut self,
        ctx: &egui::Context,
        plugin: &str,
        file: &Path,
        seq: &SequenceInfo,
        out: &Path,
    ) {
        let full = full_of(out);
        let done = self
            .full_making
            .as_ref()
            .and_then(|(f, slot)| Some((f.clone(), slot.lock().unwrap().take()?)));
        if let Some((f, r)) = done {
            self.full_making = None;
            match r {
                Ok(()) => prune(f.parent().unwrap_or(Path::new(".")), &[&f, out]),
                Err(e) => self.full_failed = Some((f, e)),
            }
        }
        let now = std::time::Instant::now();
        if self.seq_seen.as_ref().is_none_or(|(f, _)| *f != full) {
            self.seq_seen = Some((full.clone(), now));
        }
        let quiet = self
            .seq_seen
            .as_ref()
            .map(|(_, t)| now - *t)
            .unwrap_or_default();
        let wanted = !full.is_file()
            && !plugin.is_empty()
            && seq.length() > 0.0
            && self.full_failed.as_ref().is_none_or(|(f, _)| *f != full);
        let free = self.host.is_some()
            && self.full_making.is_none()
            && self.edit_rendering.is_none()
            && self.changing.is_none();
        if !wanted || !free || !ctx.input(|i| i.focused) {
            return;
        }
        if quiet < FULL_AFTER {
            ctx.request_repaint_after(FULL_AFTER - quiet);
            return;
        }
        let part = full.with_extension("part.mp4");
        let ask = ExportParams {
            film: self.project.root.clone(),
            file: file.to_path_buf(),
            sequence: seq.name.clone(),
            out: part.clone(),
        };
        let slot: FullSlot = Arc::default();
        let (s, host, p, ctx, to) = (
            slot.clone(),
            self.host.clone().expect("checked"),
            plugin.to_string(),
            ctx.clone(),
            full.clone(),
        );
        std::thread::spawn(move || {
            let r = host
                .export_edit(&p, &ask)
                .map_err(|e| format!("{e:#}"))
                .and_then(|_| std::fs::rename(&part, &to).map_err(|e| e.to_string()));
            *s.lock().unwrap() = Some(r);
            ctx.request_repaint();
        });
        self.full_making = Some((full, slot));
    }

    /// Notes what a preview that's just been made left out; the player picks it up.
    fn sequence_made(&mut self) {
        let Some(m) = &self.seq_making else {
            return;
        };
        let Some(r) = m.slot.lock().unwrap().take() else {
            return;
        };
        let m = self.seq_making.take().expect("checked");
        match r {
            Ok(skipped) => {
                let mut all = m.left_out.clone();
                all.extend(skipped);
                self.seq_playing = Some((m.out.clone(), all));
            }
            // Said on the page, and not tried again until the sequence changes.
            Err(e) => self.seq_failed = Some((m.out, e)),
        }
    }

    /// Puts the Cut page's player on `files` as `title`, staying where it was when they
    /// change.
    fn cut_deck_on(&mut self, title: &str, files: &[PathBuf]) {
        let key = files
            .iter()
            .map(|f| f.display().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        if self.cut_deck.as_ref().is_none_or(|(k, _)| *k != key) {
            let was = self
                .shown_deck(title)
                .map(|d| (d.position(), d.is_playing()));
            let deck = match self.ff.clone() {
                None => Err("ffmpeg isn't installed, so the cut can't be shown here".to_string()),
                Some(_) if files.is_empty() => Err(String::new()),
                Some(ff) => {
                    self.audio();
                    Player::new(&ff, title, files)
                        .map(|p| Deck::new(title, p, false).fixed())
                        .map_err(|e| format!("can't show {title}: {e}"))
                }
            };
            if let Some(d) = self.cut_deck.as_mut().and_then(|(_, d)| d.as_mut().ok()) {
                d.pause(self.audio.as_ref());
            }
            let mut deck = deck;
            if let (Ok(d), Some((t, _))) = (&mut deck, was)
                && t > 0.0
            {
                d.seek(t, self.audio.as_ref());
            }
            self.cut_deck = Some((key, deck));
        }
    }

    fn show_cut_deck(&mut self, ui: &mut egui::Ui, title: &str, waiting: Option<&str>) {
        // All the room there is, less Play and the timecode under it.
        let height = (ui.available_height() - 40.0).max(120.0);
        match &mut self.cut_deck {
            Some((_, Ok(d))) if d.title() == title => d.monitor(ui, self.audio.as_ref(), height),
            Some((_, Err(e))) if !e.is_empty() => blank_monitor(ui, height, e),
            _ => blank_monitor(ui, height, waiting.unwrap_or("")),
        }
    }

    /// The Cut page's player, while it shows `title`.
    pub(crate) fn shown_deck(&self, title: &str) -> Option<&Deck> {
        self.cut_deck
            .as_ref()
            .and_then(|(_, d)| d.as_ref().ok())
            .filter(|d| d.title() == title)
    }

    /// The playhead put at `t` on the timeline: just the frame while it's dragged.
    fn cut_seek(&mut self, ctx: &egui::Context, title: &str, t: f64, scrub: bool) {
        let audio = self.audio.as_ref();
        if let Some(d) = self
            .cut_deck
            .as_mut()
            .and_then(|(_, d)| d.as_mut().ok())
            .filter(|d| d.title() == title)
        {
            if scrub {
                d.scrub(ctx, t);
            } else {
                d.seek(t, audio);
            }
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
            id: None,
            picked: false,
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

/// Filled once the preview is made: the clips left out of it, or why it couldn't be.
type Made = Arc<Mutex<Option<Result<Vec<String>, String>>>>;

/// A sequence's preview being made in the background.
pub(crate) struct Making {
    out: PathBuf,
    left_out: Vec<String>,
    slot: Made,
}

/// The sequence to show first: one in a project shotline made (under `shared/<plugin>/`), else
/// the first one there is.
fn main_sequence(
    film: &Path,
    edits: &[shotline_core::protocol::EditInfo],
) -> Option<(PathBuf, String)> {
    let readable = || {
        edits
            .iter()
            .filter(|e| e.error.is_none() && !e.sequences.is_empty())
    };
    let own = readable()
        .find(|e| !e.plugin.is_empty() && e.file.starts_with(film.join("shared").join(&e.plugin)));
    let e = own.or_else(|| readable().next())?;
    Some((e.file.clone(), e.sequences[0].name.clone()))
}

pub(crate) type FullSlot = Arc<Mutex<Option<Result<(), String>>>>;

/// How long a sequence stays the same before the editor renders it.
const FULL_AFTER: std::time::Duration = std::time::Duration::from_secs(4);

/// The editor's own render of what `quick` previews.
fn full_of(quick: &Path) -> PathBuf {
    let stem = quick.file_stem().unwrap_or_default().to_string_lossy();
    quick.with_file_name(format!("{stem}-full.mp4"))
}

fn plugin_of(edits: &[shotline_core::protocol::EditInfo], file: &Path) -> String {
    edits
        .iter()
        .find(|e| e.file == file)
        .map(|e| e.plugin.clone())
        .unwrap_or_default()
}

/// Keeps the newest previews and the ones in use; a sequence changed back and forth still finds
/// its recent ones. The editor's are full size, so fewer of those.
fn prune(dir: &Path, keep: &[&Path]) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "mp4"))
        .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
        .collect();
    files.sort_by_key(|a| std::cmp::Reverse(a.0));
    let full = |p: &PathBuf| p.to_string_lossy().ends_with("-full.mp4");
    let (big, small): (Vec<_>, Vec<_>) = files.into_iter().partition(|(_, p)| full(p));
    for (_, p) in big.into_iter().skip(3).chain(small.into_iter().skip(16)) {
        if !keep.contains(&p.as_path()) {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// A screen with nothing to show yet, the same size as the monitor, saying why.
fn blank_monitor(ui: &mut egui::Ui, height: f32, note: &str) {
    let (screen, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(screen, 4.0, egui::Color32::BLACK);
    if !note.is_empty() {
        ui.put(
            screen.shrink(16.0),
            egui::Label::new(RichText::new(note).color(theme::DUST)),
        );
    }
    // Where the monitor's Play and timecode go.
    ui.add_space(36.0);
}

fn seq_title(file: &Path, seq: &SequenceInfo) -> String {
    let file = file.file_name().unwrap_or_default().to_string_lossy();
    format!("{file} · {}", seq.name)
}

/// Every finished take of the film, for an editor's bins.
fn all_takes(p: &shotline_core::Project) -> Vec<TakeFile> {
    let mut all = vec![];
    for (r, _) in p.shots(None).unwrap_or_default() {
        for t in p.takes(r).unwrap_or_default() {
            let Some(file) = t
                .output()
                .and_then(|j| Some(p.job_dir(j.id).join(j.outputs.first()?)))
            else {
                continue;
            };
            all.push(TakeFile {
                name: t.to_string(),
                shot: r.to_string(),
                file: std::path::absolute(&file).unwrap_or(file),
            });
        }
    }
    all
}

/// The film's cut as an editor sequence: the takes back to back on V1 and their sound on A1,
/// each music track on an A track of its own, at the first take's size and frame rate.
fn own_cut(
    ff: &shotline_media::Ffmpeg,
    film: &Path,
    takes: &[(String, PathBuf)],
    music: &[(String, PathBuf, f64)],
) -> Result<SequenceInfo, String> {
    let mut cut = SequenceInfo {
        name: film
            .file_name()
            .map_or("film".into(), |n| n.to_string_lossy().into()),
        ..SequenceInfo::default()
    };
    let (mut v1, mut a1) = (vec![], vec![]);
    let mut at = 0.0;
    for (name, video) in takes {
        let info = ff.probe(video).map_err(|e| e.to_string())?;
        if cut.width == 0 {
            (cut.width, cut.height, cut.fps) = (info.width, info.height, info.fps_value());
        }
        let clip = ClipInfo {
            name: name.clone(),
            file: Some(video.clone()),
            start: at,
            length: info.duration,
            speed: 1.0,
            enabled: true,
            ..ClipInfo::default()
        };
        if info.audio {
            a1.push(clip.clone());
        }
        v1.push(clip);
        at += info.duration;
    }
    let track = |name: &str, clips| TrackInfo {
        name: name.into(),
        enabled: true,
        clips,
        transitions: vec![],
    };
    cut.video.push(track("V1", v1));
    cut.audio.push(track("A1", a1));
    for (i, (name, path, start)) in music.iter().enumerate() {
        let length = ff.audio_duration(path).map_err(|e| e.to_string())?;
        cut.audio.push(track(
            &format!("A{}", i + 2),
            vec![ClipInfo {
                name: name.clone(),
                file: Some(path.clone()),
                start: *start,
                length,
                speed: 1.0,
                enabled: true,
                ..ClipInfo::default()
            }],
        ));
    }
    Ok(cut)
}

/// What the editor shows but a preview can't: clips made in the editor itself.
fn left_out(seq: &SequenceInfo) -> Vec<String> {
    seq.video
        .iter()
        .chain(&seq.audio)
        .filter(|t| t.enabled)
        .flat_map(|t| &t.clips)
        .filter(|c| c.enabled && c.file.is_none())
        .map(|c| format!("{}: made in the editor", c.name))
        .collect()
}

/// Lowest video track first, so higher ones cover it; tracks and clips turned off left out.
fn to_media(seq: &SequenceInfo) -> shotline_media::sequence::Sequence {
    use shotline_media::sequence::Sequence;
    let pieces = |tracks: &[TrackInfo], video: bool| {
        tracks
            .iter()
            .filter(|t| t.enabled)
            .flat_map(|t| track_pieces(t, video))
            .collect()
    };
    Sequence {
        width: seq.width,
        height: seq.height,
        fps: seq.fps,
        length: seq.length(),
        video: pieces(&seq.video, true),
        audio: pieces(&seq.audio, false),
    }
}

/// A track's clips to preview. On video, each dissolve runs the clip it leads into back to
/// where it starts and fades it in over the one before, which runs on to where it ends; with
/// nothing before or after, it fades from or to what's under it.
fn track_pieces(t: &TrackInfo, video: bool) -> Vec<shotline_media::sequence::Piece> {
    use shotline_media::sequence::Piece;
    let mut ps: Vec<(&str, Piece)> = t
        .clips
        .iter()
        .filter(|c| c.enabled)
        .filter_map(|c| {
            Some((
                c.id.as_str(),
                Piece {
                    path: c.file.clone()?,
                    label: c.name.clone(),
                    start: c.start,
                    length: c.length,
                    from: c.from,
                    speed: c.speed,
                    luts: if video { c.luts.clone() } else { vec![] },
                    ..Piece::default()
                },
            ))
        })
        .collect();
    if !video {
        return ps.into_iter().map(|(_, p)| p).collect();
    }
    let at = |ps: &[(&str, Piece)], id: &str| {
        (!id.is_empty())
            .then(|| ps.iter().position(|(i, _)| *i == id))
            .flatten()
    };
    for tr in t.transitions.iter().filter(|tr| tr.length > 0.0) {
        let (a, b) = (at(&ps, &tr.from), at(&ps, &tr.to));
        let end = tr.start + tr.length;
        if let Some(a) = a {
            let p = &mut ps[a].1;
            p.length = p.length.max(end - p.start);
            if b.is_none() {
                p.fade_out = Some((tr.start, tr.length));
            }
        }
        if let Some(b) = b {
            let p = &mut ps[b].1;
            // As far back as the file goes.
            let early = (p.start - tr.start)
                .max(0.0)
                .min(p.from / p.speed.max(1e-9));
            p.start -= early;
            p.from -= early * p.speed;
            p.length += early;
            p.fade_in = Some((tr.start, tr.length));
        }
    }
    ps.into_iter().map(|(_, p)| p).collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use shotline_core::protocol::TransitionInfo;

    fn clip(id: &str, start: f64, from: f64) -> ClipInfo {
        ClipInfo {
            id: id.into(),
            name: id.into(),
            file: Some(format!("/{id}.mp4").into()),
            start,
            length: 2.0,
            from,
            speed: 1.0,
            enabled: true,
            ..ClipInfo::default()
        }
    }

    fn dissolve(from: &str, to: &str, start: f64, length: f64) -> TransitionInfo {
        TransitionInfo {
            id: "t".into(),
            name: "cross_dissolve".into(),
            from: from.into(),
            to: to.into(),
            start,
            length,
        }
    }

    #[test]
    fn a_dissolve_overlaps_the_clips_around_it() {
        let t = TrackInfo {
            clips: vec![
                clip("1", 0.0, 0.0),
                clip("2", 2.0, 1.0),
                clip("3", 4.0, 0.2),
            ],
            transitions: vec![
                dissolve("1", "2", 1.5, 1.0),
                // Only 0.2 s of the file before where it's cut in.
                dissolve("2", "3", 3.5, 1.0),
                dissolve("3", "", 5.5, 0.5),
            ],
            enabled: true,
            ..TrackInfo::default()
        };
        let ps = track_pieces(&t, true);
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        let (one, two, three) = (&ps[0], &ps[1], &ps[2]);
        assert!(close(one.length, 2.5) && one.fade_in.is_none() && one.fade_out.is_none());
        assert!(close(two.start, 1.5) && close(two.from, 0.5) && close(two.length, 3.0));
        assert_eq!(two.fade_in, Some((1.5, 1.0)));
        assert!(close(three.start, 3.8) && close(three.from, 0.0) && close(three.length, 2.2));
        assert_eq!(three.fade_in, Some((3.5, 1.0)));
        assert_eq!(three.fade_out, Some((5.5, 0.5)));
        // Sound is left as it is.
        assert!(close(track_pieces(&t, false)[1].start, 2.0));
    }
}
