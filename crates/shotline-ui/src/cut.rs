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
use crate::{App, PAGE, hint, kit, page, remove_button, short, theme};

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
        self.edit_render_finished();
        // The Shots page's player keeps quiet here; this page has its own.
        if let Some(d) = &mut self.deck
            && d.is_playing()
        {
            d.pause(self.audio.as_ref());
        }
        self.refresh_edits(ui.ctx());
        self.editor_opened();
        let editor = self.pick_sequence();
        // Esc lets go of what's picked, once nothing else wants it.
        if ui.input(|i| i.key_pressed(egui::Key::Escape))
            && ui.memory(|m| m.focused().is_none())
            && !egui::Popup::is_any_open(ui.ctx())
            && self.sounds_win.is_none()
        {
            self.deselect();
        }
        let edge = egui::Margin::symmetric(PAGE.x as i8, 12);
        egui::Panel::top("cut-head")
            .frame(egui::Frame::new().fill(theme::BAY).inner_margin(edge))
            .show(ui, |ui| self.cut_head(ui, editor.as_ref()));
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
        self.sequence_made();
        let (files, badge) = self.own_preview(ui.ctx());
        let title = self.monitor_on(CUT, Some(&files));
        let mut acts = vec![];
        egui::Panel::right("cut-side")
            .resizable(true)
            .default_size(340.0)
            .frame(egui::Frame::new().fill(theme::BAY))
            .show(ui, |ui| {
                page(ui, egui::vec2(theme::GAP, theme::GAP), |ui| {
                    kit::card(ui, |ui| self.joins(ui));
                    ui.add_space(theme::GAP);
                    kit::card(ui, |ui| self.music(ui, &mut acts));
                });
            });
        egui::Panel::bottom("cut-timeline")
            .frame(egui::Frame::new().fill(theme::RACK).inner_margin(edge))
            .show(ui, |ui| {
                inspector(ui, |ui| self.own_tools(ui));
                status_row(ui, |ui| {
                    let what = if badge.is_some() {
                        "Playing the takes back to back while the preview is made"
                    } else {
                        "Quick preview: crossfades, music and sounds; colour match is in Render"
                    };
                    ui.weak(what);
                });
                self.cut_lanes(ui);
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BAY).inner_margin(edge))
            .show(ui, |ui| {
                self.show_cut_deck(ui, &title, None, badge.as_deref())
            });
        if !acts.is_empty() {
            self.flush();
            self.apply(acts);
        }
    }

    /// Puts the monitor on a render picked to play, or else on `files` as `title`. Returns
    /// what it shows.
    /// With no files it keeps what it shows, if that's `title`.
    fn monitor_on(&mut self, title: &str, files: Option<&[PathBuf]>) -> String {
        if let Some(r) = self.cut_ui.showing.clone() {
            if r.is_file() {
                let name = self.project.rel(&r);
                self.cut_deck_on(&name, std::slice::from_ref(&r));
                return name;
            }
            self.cut_ui.showing = None;
        }
        match files {
            Some(f) => self.cut_deck_on(title, f),
            None if self.shown_deck(title).is_none() => self.cut_deck_on(title, &[]),
            None => {}
        }
        title.to_string()
    }

    /// The editor sequence to show: the one picked while it's there, else the main one.
    fn pick_sequence(&mut self) -> Option<(PathBuf, SequenceInfo)> {
        if self.edit_sequence().is_none() {
            self.cut_view = main_sequence(&self.project.root, &self.edits.0);
        }
        self.edit_sequence()
    }

    fn reading_edits(&self) -> bool {
        self.edits.0.is_empty()
            && self.edits_asked.is_some()
            && self.plugins.iter().any(|d| d.editor)
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
        let mut sounds = vec![];
        let fps = self.cut_fps();
        let mut end = 0.0;
        let mut joins = vec![];
        let mut before: Option<String> = None;
        let xfade = self.timeline_form.xfade;
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
            hover.push("click for tools".into());
            if let Some(b) = before.take() {
                joins.push(lanes::Join {
                    at: end,
                    id: format!("own:{i}"),
                    name: format!("{b} to {take}"),
                    picked: self.cut_ui.own_join == Some(i),
                    bridged: false,
                    fade: (xfade > 0.0).then_some((end - xfade / 2.0, xfade)),
                    continues: c.transition == Transition::Continue,
                    matched: c.colormatch(at, false),
                });
            }
            before = Some(take.to_string());
            blocks.push(Block {
                name: take.to_string(),
                start: end,
                length,
                still,
                hover: hover.join("\n"),
                enabled: true,
                shot: Some(c.shot),
                id: Some(format!("own:{i}")),
                picked: self.cut_ui.own_pick == Some(i),
            });
            for (ts, job, path) in self.project.take_sounds(take).unwrap_or_default() {
                let whole = self.clip_length(&ctx, &path).unwrap_or(1.0);
                let long = ts.length.map_or(whole, |l| l.min(whole));
                sounds.push(Block {
                    name: job.brief.unwrap_or_default(),
                    start: end + ts.at,
                    length: long,
                    still: None,
                    hover: format!(
                        "over {take}, {} in\nclick for tools",
                        crate::player::timecode(ts.at, fps)
                    ),
                    enabled: true,
                    shot: Some(c.shot),
                    id: Some(format!("own:{i}")),
                    picked: false,
                });
            }
            end += length;
        }
        let video = vec![Lane {
            name: "V1".into(),
            blocks,
            sound: false,
            enabled: true,
            joins,
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
        if !sounds.is_empty() {
            end = sounds
                .iter()
                .map(|b| b.start + b.length)
                .fold(end, f64::max);
            audio.push(Lane {
                name: "Sounds".into(),
                blocks: sounds,
                sound: true,
                enabled: true,
                joins: vec![],
            });
        }
        let deck = self.shown_deck(CUT);
        let playhead = deck.map(|d| d.position());
        let fps = deck.map_or(24.0, |d| d.fps());
        self.cut_ui.edits = lanes::edit_points(video.iter());
        let own = |id: &str| {
            id.strip_prefix("own:")
                .and_then(|i| i.parse::<usize>().ok())
        };
        match lanes::show(
            ui,
            "cut",
            &video,
            &audio,
            end,
            fps,
            playhead,
            &mut self.thumbs,
        ) {
            Some(Hit::Seek(t)) => self.cut_seek(ui.ctx(), CUT, t, false),
            Some(Hit::Scrub(t)) => self.cut_seek(ui.ctx(), CUT, t, true),
            Some(Hit::Open(r)) => {
                self.tab = crate::Tab::Shots;
                self.open_shot(r);
            }
            Some(Hit::Pick(id)) => {
                let i = own(&id);
                self.cut_ui.own_join = None;
                self.cut_ui.own_pick = (self.cut_ui.own_pick != i).then_some(i).flatten();
            }
            Some(Hit::PickJoin(id, t)) => {
                let i = own(&id);
                self.cut_ui.own_pick = None;
                self.cut_ui.own_join = (self.cut_ui.own_join != i).then_some(i).flatten();
                self.cut_seek(ui.ctx(), CUT, t, false);
            }
            Some(Hit::Deselect) => self.deselect(),
            None => {}
        }
    }

    /// Lets go of whatever's picked on the timeline.
    fn deselect(&mut self) {
        self.cut_ui.own_pick = None;
        self.cut_ui.own_join = None;
        self.clip_pick = None;
        self.join_pick = None;
    }

    /// shotline's own cut as one preview with its crossfades, music and laid sounds, made in the
    /// background; the takes back to back in the meantime. The files to play, and a badge for
    /// the monitor while the preview is behind.
    fn own_preview(&mut self, ctx: &egui::Context) -> (Vec<PathBuf>, Option<String>) {
        let takes: Vec<(String, PathBuf)> = self
            .film
            .cut
            .iter()
            .filter_map(|c| Some((c.take.as_ref()?.to_string(), c.video.clone()?)))
            .collect();
        let raw: Vec<PathBuf> = takes.iter().map(|(_, v)| v.clone()).collect();
        let Some(ff) = self.ff.clone() else {
            return (raw, None);
        };
        if takes.is_empty() {
            return (raw, None);
        }
        let music: Vec<(String, PathBuf, f64)> = self
            .timeline_form
            .tracks
            .iter()
            .filter(|t| t.volume > 0.0)
            .map(|t| (t.file.clone(), self.project.root.join(&t.file), t.at))
            .collect();
        let mut sounds = vec![];
        let mut i = 0;
        for c in &self.film.cut {
            let Some(t) = c.take.as_ref().filter(|_| c.video.is_some()) else {
                continue;
            };
            for (ts, _, path) in self.project.take_sounds(t).unwrap_or_default() {
                sounds.push((i, path, ts.at, ts.length));
            }
            i += 1;
        }
        let xfade = self.timeline_form.xfade;
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        1u32.hash(&mut h);
        xfade.to_bits().hash(&mut h);
        let files = takes
            .iter()
            .map(|(_, v)| v)
            .chain(music.iter().map(|(_, p, _)| p))
            .chain(sounds.iter().map(|(_, p, ..)| p));
        for f in files {
            f.hash(&mut h);
            if let Ok(m) = f.metadata() {
                m.len().hash(&mut h);
                m.modified().ok().hash(&mut h);
            }
        }
        for (_, _, at) in &music {
            at.to_bits().hash(&mut h);
        }
        for (i, _, at, length) in &sounds {
            (i, at.to_bits(), length.map(f64::to_bits)).hash(&mut h);
        }
        let out = self
            .project
            .root
            .join(".shotline/cache/sequences")
            .join(format!("own-{:016x}.mp4", h.finish()));
        if out.is_file() {
            return (vec![out], None);
        }
        if let Some((_, e)) = self.seq_failed.as_ref().filter(|(f, _)| *f == out) {
            return (raw, Some(format!("No preview: {}", short(e, 80))));
        }
        if self.seq_making.is_none() {
            let slot: Made = Arc::default();
            self.seq_making = Some(Making {
                out: out.clone(),
                left_out: vec![],
                slot: slot.clone(),
            });
            let (film, ctx, to) = (self.project.root.clone(), ctx.clone(), out.clone());
            std::thread::spawn(move || {
                let r = own_cut(&ff, &film, &takes, &music, &sounds, xfade).and_then(|seq| {
                    ff.sequence_preview(&to_media(&seq), &to)
                        .map_err(|e| e.to_string())
                });
                *slot.lock().unwrap() = Some(r);
                ctx.request_repaint();
            });
        }
        (
            raw,
            Some(format!("{} Updating preview…", crate::icon::CLOCK)),
        )
    }

    /// What can be done with the take picked on shotline's own cut.
    fn own_tools(&mut self, ui: &mut egui::Ui) {
        if let Some(i) = self.cut_ui.own_join {
            self.own_join_tools(ui, i);
            return;
        }
        let picked = self
            .cut_ui
            .own_pick
            .and_then(|i| self.film.cut.get(i))
            .and_then(|c| c.take.clone());
        let Some(t) = picked else {
            self.cut_ui.own_pick = None;
            ui.weak("Click a take or a join to change it from here.");
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(t.to_string()).strong().color(theme::PAPER));
            if ui.button("Open the shot").clicked() {
                self.tab = crate::Tab::Shots;
                self.open_shot(t.shot);
            }
            let n = self.project.take_sounds(&t).map_or(0, |s| s.len());
            let label = match n {
                0 => "Sounds…".to_string(),
                n => format!("Sounds ({n})…"),
            };
            if ui
                .button(label)
                .on_hover_text("effects or a voice-over over this take")
                .clicked()
            {
                self.open_sounds((&t).into(), t.to_string(), 0.0);
            }
            if let Some(other) = self.take_menu(ui, &t, false).map(|(o, _)| o) {
                self.flush();
                self.apply(vec![Action::Select(other)]);
            }
        });
    }

    /// What a join on shotline's own cut is: a cut or carried on, matched or not. How it's made
    /// is the shot's, so that's where it's changed.
    fn own_join_tools(&mut self, ui: &mut egui::Ui, i: usize) {
        let at = self.timeline_form.colormatch.at;
        let Some((c, before)) = self
            .film
            .cut
            .get(i)
            .zip(self.film.cut[..i].iter().rev().find_map(|c| c.take.clone()))
        else {
            self.cut_ui.own_join = None;
            return;
        };
        let (Some(t), shot) = (c.take.clone(), c.shot) else {
            self.cut_ui.own_join = None;
            return;
        };
        let (continues, matched) = (
            c.transition == Transition::Continue,
            c.colormatch(at, false),
        );
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("{before} to {t}"))
                    .strong()
                    .color(theme::PAPER),
            );
            let (what, colour) = if continues {
                ("continues from the shot before", theme::TUNGSTEN)
            } else {
                ("a cut", theme::DUST)
            };
            ui.label(RichText::new(what).color(colour));
            if matched {
                ui.label(RichText::new("colour matched").color(theme::GO));
            }
            if self.timeline_form.xfade > 0.0 {
                ui.weak(format!(
                    "crossfade {}",
                    crate::player::timecode(self.timeline_form.xfade, self.cut_fps())
                ));
            }
            if ui
                .button(format!("Open {shot}"))
                .on_hover_text("how a shot starts is set on the shot")
                .clicked()
            {
                self.tab = crate::Tab::Shots;
                self.open_shot(shot);
            }
        });
    }

    /// Picks up what the editor plugins said, and asks them again once the film was read again.
    pub(crate) fn refresh_edits(&mut self, ctx: &egui::Context) {
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
            let mut bridges = crate::bridge::BridgeTakes::new();
            for f in got
                .0
                .iter()
                .flat_map(|e| &e.sequences)
                .flat_map(|s| &s.video)
                .flat_map(|t| &t.clips)
                .filter_map(|c| c.file.as_ref())
            {
                if bridges.contains_key(f) || takes.contains_key(f) {
                    continue;
                }
                if let Some(job) = self.project.bridge_of_file(f)
                    && let Some(id) = job.bridge
                {
                    let n = self
                        .project
                        .bridge_takes(id)
                        .unwrap_or_default()
                        .iter()
                        .find(|(_, j)| j.id == job.id)
                        .map_or(0, |(n, _)| *n);
                    bridges.insert(f.clone(), (id, job.id, n));
                }
            }
            let mut laid = std::collections::HashSet::new();
            let unders = takes.values().map(shotline_core::Under::from).chain(
                bridges
                    .values()
                    .map(|(b, t, _)| shotline_core::Under::Bridge(*b, *t)),
            );
            for u in unders {
                for (_, _, p) in self.project.sounds(u).unwrap_or_default() {
                    laid.insert(std::path::absolute(&p).unwrap_or(p));
                }
            }
            self.cut_ui.laid = laid;
            self.edits = got;
            self.edit_takes = takes;
            self.edit_bridges = bridges;
            self.bridges_seen = None;
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

    /// Picks up how a plugin's fix went; asked again, so a warning that's put right goes.
    pub(crate) fn fix_finished(&mut self) {
        let done = self
            .edit_fixing
            .as_ref()
            .and_then(|(_, slot)| slot.lock().unwrap().take());
        if let Some(r) = done {
            self.edit_fixing = None;
            self.edits_stale = r.is_ok();
            self.edit_fixed = Some(r);
        }
    }

    /// Has the plugin fix what it warned about, off the UI thread.
    pub(crate) fn start_fix(&mut self, ctx: &egui::Context, fix: crate::EditFix) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let slot: crate::Said = Arc::default();
        let (s, film, ctx) = (slot.clone(), self.project.root.clone(), ctx.clone());
        self.edit_fixed = None;
        self.edit_fixing = Some((fix.label.clone(), slot));
        std::thread::spawn(move || {
            let r = host
                .fix_edit(&fix.plugin, &film, &fix.id)
                .map_err(|e| format!("{e:#}"));
            *s.lock().unwrap() = Some(r);
            ctx.request_repaint();
        });
    }

    /// Asks the editors again once something in shared/, or one of their files, was saved.
    fn watch_edits(&mut self, ctx: &egui::Context) {
        let every = self.edits_every;
        ctx.request_repaint_after(every);
        let now = std::time::Instant::now();
        if self
            .edits_seen
            .is_some_and(|(_, at)| now.duration_since(at) < every)
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

    /// "Open in FilmCraft" for each editor plugin: the shown sequence, or a new project.
    pub(crate) fn open_buttons(&mut self, ui: &mut egui::Ui) {
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
                            // A sound is picked only when it's over a take or a bridge.
                            let over = c.file.as_ref().is_some_and(|f| {
                                self.edit_takes.contains_key(f)
                                    || self.edit_bridges.contains_key(f)
                                    || self.cut_ui.laid.contains(
                                        &std::path::absolute(f).unwrap_or_else(|_| f.clone()),
                                    )
                            });
                            if !c.id.is_empty() && (!sound || over) {
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
        self.full_done(&out);
        let mut note = None;
        let mut files = None;
        if full.is_file() {
            files = Some(vec![full.clone()]);
        } else if out.is_file() {
            files = Some(vec![out.clone()]);
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
        // The one before plays on meanwhile, marked as behind.
        let behind = files.is_none() && self.shown_deck(&title).is_some();
        let failed = self.seq_failed.as_ref().is_some_and(|(f, _)| *f == out);
        let badge =
            (behind && !failed).then(|| format!("{} Updating preview…", crate::icon::CLOCK));
        let title = self.monitor_on(&title, files.as_deref());
        let edge = egui::Margin::symmetric(PAGE.x as i8, 12);
        egui::Panel::bottom("edit-timeline")
            .frame(egui::Frame::new().fill(theme::RACK).inner_margin(edge))
            .show(ui, |ui| {
                // Fixed heights above the tracks, so nothing here moves them.
                inspector(ui, |ui| self.clip_tools(ui, file, seq));
                status_row(ui, |ui| self.edit_status(ui, file, seq));
                let playhead = self.shown_deck(&title).map(|d| d.position());
                self.cut_ui.edits = lanes::edit_points(video.iter());
                match lanes::show(
                    ui,
                    "edit",
                    &video,
                    &audio,
                    seq.length(),
                    seq.fps,
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
                    Some(Hit::PickJoin(id, t)) => {
                        self.clip_pick = None;
                        self.join_pick = (self.join_pick.as_ref() != Some(&id)).then_some(id);
                        self.cut_seek(&ctx, &title, t, false);
                    }
                    Some(Hit::Deselect) => self.deselect(),
                    _ => {}
                }
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BAY).inner_margin(edge))
            .show(ui, |ui| {
                self.show_cut_deck(ui, &title, note.as_deref(), badge.as_deref())
            });
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
                        ask.cut = Some(own_cut(&ff, &film, &takes, &music, &[], 0.0)?);
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
        3u32.hash(&mut h);
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

    /// What a sound clip in the sequence is over: a take's own sound, a bridge's, or a sound
    /// laid over one of them.
    pub(crate) fn sound_under(&self, c: &ClipInfo) -> SoundPick {
        let name = |f: &Path| {
            f.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let mut m = SoundPick {
            clip: c
                .file
                .as_deref()
                .map(name)
                .unwrap_or_else(|| c.name.clone()),
            ..SoundPick::default()
        };
        let Some(f) = &c.file else { return m };
        if let Some(t) = self.edit_takes.get(f) {
            m.under = Some((t.into(), t.to_string(), c.from));
            m.shot = Some(t.shot);
            m.clip = format!("{t}'s own sound");
            return m;
        }
        if let Some((b, t, n)) = self.edit_bridges.get(f) {
            m.under = Some((
                shotline_core::Under::Bridge(*b, *t),
                format!("bridge #{n}"),
                c.from,
            ));
            m.clip = format!("bridge #{n}'s own sound");
            return m;
        }
        let takes = self
            .edit_takes
            .values()
            .map(|t| (shotline_core::Under::from(t), t.to_string(), Some(t.shot)));
        let bridges = self.edit_bridges.values().map(|(b, t, n)| {
            (
                shotline_core::Under::Bridge(*b, *t),
                format!("bridge #{n}"),
                None,
            )
        });
        for (u, n, shot) in takes.chain(bridges) {
            let laid = self
                .project
                .sounds(u)
                .unwrap_or_default()
                .into_iter()
                .find(|(_, _, p)| crate::sounds::same(p, f));
            if let Some((ts, j, _)) = laid {
                m.clip = crate::sounds::sound_name(&j);
                m.under = Some((u, n, ts.at));
                m.laid = true;
                m.shot = shot;
                return m;
            }
        }
        m
    }

    /// Picks up the editor's render of the preview once it's done.
    fn full_done(&mut self, out: &Path) {
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
    }

    /// Whether the editor can be asked to render a preview now.
    fn full_free(&self) -> bool {
        self.host.is_some()
            && self.full_making.is_none()
            && self.edit_rendering.is_none()
            && self.changing.is_none()
    }

    /// Has the editor render the sequence as the player's preview, off the UI thread.
    fn start_full(&mut self, ctx: &egui::Context, plugin: &str, file: &Path, seq: &SequenceInfo) {
        let full = full_of(&self.preview_of(seq));
        if !self.full_free() {
            return;
        }
        self.full_failed = None;
        let part = full.with_extension("part.mp4");
        let ask = ExportParams {
            film: self.project.root.clone(),
            file: file.to_path_buf(),
            sequence: seq.name.clone(),
            out: part.clone(),
            saved: true,
            // Half size is plenty for the player and renders much sooner.
            scale: Some(0.5),
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

    /// Which preview plays: the editor's render of it, or the quick one with Update preview.
    /// Amber only when something went wrong.
    pub(crate) fn preview_state(&mut self, ui: &mut egui::Ui, file: &Path, seq: &SequenceInfo) {
        let plugin = plugin_of(&self.edits.0, file);
        let app = self
            .edits
            .0
            .iter()
            .find(|e| e.file == file)
            .map(|e| e.app.clone())
            .unwrap_or_default();
        let out = self.preview_of(seq);
        let full = full_of(&out);
        let making = self.full_making.as_ref().is_some_and(|(f, _)| *f == full);
        let failed = self
            .full_failed
            .as_ref()
            .filter(|(f, _)| *f == full)
            .map(|(_, e)| e.clone());
        if let Some((_, skipped)) = self.seq_playing.as_ref().filter(|(p, _)| *p == out)
            && !skipped.is_empty()
        {
            ui.weak(format!("{} left out of the preview", skipped.len()))
                .on_hover_text(skipped.join("\n"));
            ui.weak("·");
        }
        if full.is_file() {
            ui.weak(format!("Preview rendered by {app}"));
            return;
        }
        if making {
            ui.spinner();
            ui.weak(format!("{app} is rendering the preview…"));
            return;
        }
        if out.is_file() {
            ui.weak("Quick preview (no effects)").on_hover_text(
                "LUTs and dissolves only; Update preview has the editor render it with everything",
            );
        }
        if plugin.is_empty() || seq.length() <= 0.0 {
            return;
        }
        if ui
            .add_enabled(self.full_free(), egui::Button::new("Update preview"))
            .on_hover_text(format!("{app} renders the sequence at half size"))
            .on_disabled_hover_text(format!("{app} is busy"))
            .clicked()
        {
            self.start_full(ui.ctx(), &plugin, file, seq);
        }
        if let Some(e) = failed {
            ui.label(
                RichText::new(format!("{app} couldn't render it: {}", short(&e, 80)))
                    .color(theme::WARN),
            )
            .on_hover_text(e);
        }
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

    fn show_cut_deck(
        &mut self,
        ui: &mut egui::Ui,
        title: &str,
        waiting: Option<&str>,
        badge: Option<&str>,
    ) {
        let mut back = false;
        if self.cut_ui.showing.is_some() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Playing {title}")).color(theme::PAPER));
                back = ui.button("Back to the cut").clicked();
            });
        }
        if back {
            self.cut_ui.showing = None;
        }
        // All the room there is, less Play and the timecode under it.
        let height = (ui.available_height() - 40.0).max(120.0);
        let top = ui.cursor().min;
        match &mut self.cut_deck {
            Some((_, Ok(d))) if d.title() == title => {
                let edits: &[f64] = if self.cut_ui.showing.is_some() {
                    &[]
                } else {
                    &self.cut_ui.edits
                };
                if d.monitor(ui, self.audio.as_ref(), height, edits) {
                    let name = format!(
                        "{}-{}.png",
                        title.replace(['#', ' ', '/', '·'], "-"),
                        crate::player::timecode(d.position(), d.fps()).replace(':', "")
                    );
                    let png = self.project.root.join("stills").join(name);
                    // A failure shows when the frame can't be written there.
                    let _ = std::fs::create_dir_all(self.project.root.join("stills"));
                    d.save_frame(&png, self.audio.as_ref());
                }
            }
            Some((_, Err(e))) if !e.is_empty() => blank_monitor(ui, height, e),
            _ => blank_monitor(ui, height, waiting.unwrap_or("")),
        }
        if let Some(b) = badge {
            let at =
                egui::Rect::from_min_size(top + egui::vec2(12.0, 12.0), egui::vec2(320.0, 28.0));
            ui.put(at, |ui: &mut egui::Ui| {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                    egui::Frame::new()
                        .fill(egui::Color32::from_black_alpha(190))
                        .corner_radius(12.0)
                        .inner_margin(egui::Margin::symmetric(10, 4))
                        .show(ui, |ui| {
                            // Still, not spinning: it's a note, and it can stay up a while.
                            ui.label(RichText::new(b).color(theme::PAPER).small());
                        })
                        .response
                })
                .inner
            });
        }
    }

    /// The frame rate the Cut page counts in: the monitor's, else 24.
    pub(crate) fn cut_fps(&self) -> f64 {
        self.cut_deck
            .as_ref()
            .and_then(|(_, d)| d.as_ref().ok())
            .map(|d| d.fps())
            .filter(|f| *f > 0.0)
            .unwrap_or(24.0)
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
            hover.push(format!(
                "from {} into it",
                crate::player::timecode(c.from, self.cut_fps())
            ));
        }
        if c.speed != 1.0 {
            hover.push(format!("at {:.0}% speed", c.speed * 100.0));
        }
        if !c.enabled {
            hover.push("turned off".into());
        }
        if !c.id.is_empty() && (!sound || take.is_some()) {
            hover.push("click for tools".into());
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
        let bridge = c.file.as_ref().and_then(|f| self.edit_bridges.get(f));
        Block {
            name: match (take, bridge) {
                (Some(t), _) => t.to_string(),
                (None, Some((_, _, n))) => format!("bridge #{n}"),
                (None, None) => c.name.clone(),
            },
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
    pub(crate) fn clip_length(&self, ctx: &egui::Context, path: &Path) -> Option<f64> {
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
        let fps = self.cut_fps();
        let t = &mut self.timeline_form;
        egui::Grid::new("joins")
            .num_columns(2)
            .spacing(egui::vec2(12.0, 8.0))
            .show(ui, |ui| {
                let l = ui.weak("Crossfade");
                ui.horizontal(|ui| {
                    ui.add(crate::player::timecode_field(&mut t.xfade, fps, 3.0))
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
        let fps = self.cut_fps();
        let t = &mut self.timeline_form;
        if t.tracks.is_empty() {
            hint(
                ui,
                "Nothing plays over the cut. Music and effects mix in when it's rendered.",
            );
        }
        let mut remove = None;
        // Two rows each, so the card stays narrow next to the monitor.
        for (i, tr) in t.tracks.iter_mut().enumerate() {
            ui.push_id(i, |ui| {
                let mut name = None;
                ui.horizontal(|ui| {
                    name = Some(ui.label(RichText::new(&tr.file).color(theme::PAPER)).id);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if remove_button(ui)
                            .on_hover_text(format!("{} stops playing over the cut", tr.file))
                            .clicked()
                        {
                            remove = Some(i);
                        }
                    });
                });
                let name = name.unwrap();
                ui.horizontal(|ui| {
                    ui.weak("starts at");
                    ui.add(crate::player::timecode_field(&mut tr.at, fps, 36000.0))
                        .labelled_by(name);
                    ui.add_space(8.0);
                    ui.weak("volume");
                    ui.add(
                        egui::DragValue::new(&mut tr.volume)
                            .range(0.0..=2.0)
                            .speed(0.01)
                            .fixed_decimals(2),
                    )
                    .labelled_by(name);
                });
                ui.add_space(8.0);
            });
        }
        if let Some(i) = remove {
            t.tracks.remove(i);
        }
        if ui.button("Add music…").clicked()
            && let Some(f) = rfd::FileDialog::new()
                .add_filter("Audio", &crate::SOUND_FILES)
                .pick_file()
        {
            acts.push(Action::AddTrack(f));
        }
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
pub(crate) fn main_sequence(
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

/// What a picked sound clip is.
#[derive(Clone, Default)]
pub(crate) struct SoundPick {
    /// A file's name, what was said, or whose own sound it is.
    pub clip: String,
    /// What it's over, by name, and where in it the sound starts.
    pub under: Option<(shotline_core::Under, String, f64)>,
    pub shot: Option<ShotRef>,
    /// A sound laid over it, rather than its own.
    pub laid: bool,
}

pub(crate) type FullSlot = Arc<Mutex<Option<Result<(), String>>>>;

/// The editor's own render of what `quick` previews.
fn full_of(quick: &Path) -> PathBuf {
    let stem = quick.file_stem().unwrap_or_default().to_string_lossy();
    quick.with_file_name(format!("{stem}-full.mp4"))
}

pub(crate) fn plugin_of(edits: &[shotline_core::protocol::EditInfo], file: &Path) -> String {
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
    ui.painter()
        .rect_filled(screen, theme::R_CARD, theme::STAGE);
    if !note.is_empty() {
        ui.put(
            screen.shrink(16.0),
            egui::Label::new(RichText::new(note).color(theme::DUST)),
        );
    }
    // Where the monitor's Play and timecode go.
    ui.add_space(36.0);
}

pub(crate) fn seq_title(file: &Path, seq: &SequenceInfo) -> String {
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
/// Sounds laid over a take of the cut: which take (by place), the file, where in the take it
/// starts and how much of it plays.
type Laid = (usize, PathBuf, f64, Option<f64>);

/// The takes back to back, each `xfade` seconds over the one before, with the music and sounds
/// over them, as an editor's sequence.
fn own_cut(
    ff: &shotline_media::Ffmpeg,
    film: &Path,
    takes: &[(String, PathBuf)],
    music: &[(String, PathBuf, f64)],
    sounds: &[Laid],
    xfade: f64,
) -> Result<SequenceInfo, String> {
    let mut cut = SequenceInfo {
        name: film
            .file_name()
            .map_or("film".into(), |n| n.to_string_lossy().into()),
        ..SequenceInfo::default()
    };
    let (mut v1, mut a1) = (vec![], vec![]);
    let mut fades = vec![];
    let mut starts = vec![];
    let mut at = 0.0;
    for (i, (name, video)) in takes.iter().enumerate() {
        let info = ff.probe(video).map_err(|e| e.to_string())?;
        if cut.width == 0 {
            (cut.width, cut.height, cut.fps) = (info.width, info.height, info.fps_value());
        }
        if i > 0 && xfade > 0.0 {
            at -= xfade.min(info.duration / 2.0);
            fades.push(shotline_core::protocol::TransitionInfo {
                id: format!("x{i}"),
                name: "cross_dissolve".into(),
                from: format!("c{}", i - 1),
                to: format!("c{i}"),
                start: at,
                length: xfade.min(info.duration / 2.0),
            });
        }
        starts.push(at);
        let clip = ClipInfo {
            id: if xfade > 0.0 {
                format!("c{i}")
            } else {
                String::new()
            },
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
    cut.video.push(TrackInfo {
        transitions: fades,
        ..track("V1", v1)
    });
    cut.audio.push(track("A1", a1));
    let mut laid = vec![];
    for (i, path, from, only) in sounds {
        let whole = ff.audio_duration(path).map_err(|e| e.to_string())?;
        laid.push(ClipInfo {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into(),
            file: Some(path.clone()),
            start: starts.get(*i).copied().unwrap_or(0.0) + from,
            length: only.map_or(whole, |l| l.min(whole)),
            speed: 1.0,
            enabled: true,
            ..ClipInfo::default()
        });
    }
    if !laid.is_empty() {
        cut.audio.push(track("Sounds", laid));
    }
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

/// How tall the picked clip's tools are, whatever they show, so the tracks under them stay put.
const INSPECTOR: f32 = 60.0;

pub(crate) fn inspector(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let w = ui.available_width();
    ui.allocate_ui_with_layout(
        egui::vec2(w, INSPECTOR),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_min_size(egui::vec2(w, INSPECTOR));
            ui.set_max_height(INSPECTOR);
            ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
            add(ui);
        },
    );
}

/// One line under the tools: what's going on, and Undo on the right.
pub(crate) fn status_row(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let h = ui.spacing().interact_size.y + 4.0;
    let w = ui.available_width();
    ui.allocate_ui_with_layout(
        egui::vec2(w, h),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_size(egui::vec2(w, h));
            add(ui);
        },
    );
    ui.add_space(4.0);
}

fn heading(ui: &mut egui::Ui, text: &str) {
    kit::card_title(ui, text, None);
    ui.add_space(4.0);
}

pub(crate) fn plural(n: usize, what: &str) -> String {
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
