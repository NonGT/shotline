//! Changing a clip of an editor's sequence from here: swap its take, match its colour to the clip
//! before, give it a LUT. The editor plugin makes the change in the editor and says how to take
//! it back; those undo steps are kept in the film's cache, newest last.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};
use shotline_core::protocol::{
    Change, ClipInfo, EditParams, Edited, ExportParams, Op, Request, SequenceInfo, TrackInfo,
    TransitionInfo,
};
use shotline_core::{ShotRef, Take};

use crate::price::{Pressed, Price, PriceRow};
use crate::{App, short, theme};

const KEPT: usize = 30;

/// `a` ends where `b` starts, give or take half a frame.
fn meet(a: &ClipInfo, b: &ClipInfo, fps: f64) -> bool {
    (a.start + a.length - b.start).abs() < 0.5 / fps.max(1.0)
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Undo {
    pub plugin: String,
    pub file: PathBuf,
    pub sequence: String,
    /// "swap 01-020#1 for #3".
    pub what: String,
    pub steps: serde_json::Value,
}

/// A change on its way: what it is, and how it went.
pub(crate) struct Changing {
    what: String,
    undo: Option<Undo>,
    ask: EditParams,
    plugin: String,
    slot: Arc<Mutex<Option<Result<Edited, String>>>>,
}

fn undo_file(film: &Path) -> PathBuf {
    film.join(".shotline/cache/editor-undo.json")
}

pub(crate) fn load_undo(film: &Path) -> Vec<Undo> {
    std::fs::read_to_string(undo_file(film))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_undo(film: &Path, list: &[Undo]) {
    let path = undo_file(film);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let from = list.len().saturating_sub(KEPT);
    if let Ok(text) = serde_json::to_string_pretty(&list[from..]) {
        let _ = std::fs::write(path, text);
    }
}

impl App {
    /// Under an editor's sequence: the picked clip's tools, and undo for what was changed here.
    pub(crate) fn clip_tools(&mut self, ui: &mut egui::Ui, file: &Path, seq: &SequenceInfo) {
        self.change_done();
        self.bridged = self.bridged_joins(seq);
        let Some(plugin) = self
            .edits
            .0
            .iter()
            .find(|e| e.file == file)
            .map(|e| e.plugin.clone())
        else {
            return;
        };
        let busy = self.changing.as_ref().map(|c| c.what.clone());
        let find = |tracks: &[shotline_core::protocol::TrackInfo]| {
            let id = self.clip_pick.as_ref()?;
            tracks
                .iter()
                .flat_map(|t| &t.clips)
                .find(|c| c.id == *id)
                .cloned()
        };
        let picked = find(&seq.video);
        let sound = find(&seq.audio);
        if picked.is_none() && sound.is_none() {
            self.clip_pick = None;
        }
        let join = self.join_pick.as_ref().and_then(|id| {
            seq.video.iter().find_map(|t| {
                let i = t.clips.iter().position(|c| c.id == *id)?;
                let after = &t.clips[i];
                let before = t
                    .clips
                    .iter()
                    .find(|c| meet(c, after, seq.fps) && c.id != after.id)?;
                let fade = t.transitions.iter().find(|x| x.to == *id).cloned();
                Some((before.clone(), after.clone(), fade))
            })
        });
        if join.is_none() {
            self.join_pick = None;
        }
        ui.horizontal_wrapped(|ui| match (&picked, &join) {
            (Some(c), _) => self.picked_tools(ui, &plugin, file, seq, c, busy.is_none()),
            (None, _) if let Some(c) = &sound => self.sound_tools(ui, file, seq, c, busy.is_none()),
            (None, Some((a, b, fade))) => self.join_tools(
                ui,
                &plugin,
                file,
                seq,
                (a, b),
                fade.as_ref(),
                busy.is_none(),
            ),
            (None, None) => {
                ui.weak("Click a clip, or the mark where two meet, to change it from here.");
            }
        });
        self.process_row(ui, file, seq);
        self.bridge_window(ui, &plugin, file, seq);
        if let Some(what) = &busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak(what);
            });
        }
        let undo = self
            .undo
            .get_or_insert_with(|| load_undo(&self.project.root))
            .iter()
            .rev()
            .find(|u| u.file == file && u.sequence == seq.name)
            .cloned();
        if let Some(u) = undo {
            let label = format!("Undo {}", u.what);
            if ui
                .add_enabled(busy.is_none(), egui::Button::new(label))
                .on_hover_text(
                    "takes back the last change made from here; what was done in the editor \
                     since stays",
                )
                .clicked()
            {
                let ask = EditParams {
                    film: self.project.root.clone(),
                    file: file.to_path_buf(),
                    sequence: seq.name.clone(),
                    clip: String::new(),
                    change: Change::Undo {
                        steps: u.steps.clone(),
                    },
                };
                self.start_change(ui.ctx(), &plugin, format!("undoing {}", u.what), ask, None);
            }
        }
    }

    /// The places on a video track where two clips the editor can change meet.
    pub(crate) fn joins_of(&self, t: &TrackInfo, fps: f64) -> Vec<crate::lanes::Join> {
        let name = |c: &ClipInfo| {
            c.file
                .as_ref()
                .and_then(|f| self.edit_takes.get(f))
                .map_or(c.name.clone(), |t| t.to_string())
        };
        let mut clips: Vec<&ClipInfo> = t.clips.iter().filter(|c| !c.id.is_empty()).collect();
        clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        let bridged = &self.bridged;
        clips
            .windows(2)
            .filter(|w| meet(w[0], w[1], fps))
            .map(|w| crate::lanes::Join {
                at: w[1].start,
                id: w[1].id.clone(),
                name: format!("{} to {}", name(w[0]), name(w[1])),
                picked: self.join_pick.as_ref() == Some(&w[1].id),
                bridged: bridged.contains(&w[1].id),
                fade: t
                    .transitions
                    .iter()
                    .find(|x| x.to == w[1].id)
                    .map(|x| (x.start, x.length)),
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn join_tools(
        &mut self,
        ui: &mut egui::Ui,
        plugin: &str,
        file: &Path,
        seq: &SequenceInfo,
        (a, b): (&ClipInfo, &ClipInfo),
        fade: Option<&TransitionInfo>,
        free: bool,
    ) {
        let take_of = |c: &ClipInfo| {
            c.file
                .as_ref()
                .and_then(|f| self.edit_takes.get(f))
                .cloned()
        };
        let name = |c: &ClipInfo| take_of(c).map_or(c.name.clone(), |t| t.to_string());
        let (na, nb) = (name(a), name(b));
        ui.label(
            RichText::new(format!("{na} to {nb}"))
                .strong()
                .color(theme::PAPER),
        );
        let continues = take_of(b).is_some_and(|t| {
            self.film
                .cut
                .iter()
                .any(|c| c.shot == t.shot && c.transition == shotline_core::Transition::Continue)
        });
        if continues {
            ui.weak("continues from the shot before").on_hover_text(
                "its first frame was the last one before it; a colour match hides the seam",
            );
        }
        let film = self.project.root.clone();
        let ask = |change| EditParams {
            film: film.clone(),
            file: file.to_path_buf(),
            sequence: seq.name.clone(),
            clip: b.id.clone(),
            change,
        };
        let mut start = None;
        let frame = 1.0 / seq.fps.max(1.0);
        if ui
            .add_enabled(free, egui::Button::new(format!("Match colour to {na}")))
            .on_hover_text(format!(
                "the editor's colour match on {nb}, to the last frame of {na}"
            ))
            .clicked()
        {
            let change = Change::Match {
                to: b.start - frame,
            };
            start = Some((format!("colour match of {nb}"), ask(change)));
        }
        let files = a
            .file
            .clone()
            .zip(b.file.clone())
            .filter(|(x, y)| x.is_file() && y.is_file());
        let lut = ui
            .add_enabled(
                free && files.is_some() && self.ff.is_some(),
                egui::Button::new("Match with shotline's LUT"),
            )
            .on_hover_text(
                "shotline's own colour match (as in its renders, with the Joins settings) as an \
                 input LUT on the clip",
            )
            .on_disabled_hover_text("needs both clips' files, and ffmpeg");
        if lut.clicked()
            && let Some((fa, fb)) = files.clone()
        {
            let m = &self.timeline_form.colormatch;
            let (frames, strength) = (m.frames, m.strength);
            let cube = self.project.root.join(".shotline/cache/luts").join(format!(
                "{}-{}.cube",
                b.id,
                jiff::Timestamp::now().as_millisecond()
            ));
            let prev = (fa, a.from + a.length * a.speed);
            let next = (fb, b.from);
            let ff = self.ff.clone().unwrap();
            let make = move || {
                ff.match_lut(
                    (&prev.0, prev.1),
                    (&next.0, next.1),
                    frames,
                    strength,
                    &cube,
                )
                .map(|()| Change::Lut { file: Some(cube) })
                .map_err(|e| e.to_string())
            };
            let what = format!("shotline's colour match on {nb}");
            let undo = Undo {
                plugin: plugin.to_string(),
                file: file.to_path_buf(),
                sequence: seq.name.clone(),
                what: what.clone(),
                steps: serde_json::Value::Null,
            };
            let a = ask(Change::Lut { file: None });
            self.start_made_change(ui.ctx(), plugin, what, a, undo, make);
        }
        let bridge = ui
            .add_enabled(
                free && files.is_some() && self.ff.is_some(),
                egui::Button::new("Bridge…"),
            )
            .on_hover_text(format!(
                "a short clip made from the last frame of {na} to the first of {nb}, put in \
                 between them"
            ))
            .on_disabled_hover_text("needs both clips' files, and ffmpeg");
        if bridge.clicked() {
            self.open_bridge(ui.ctx(), seq, a, b);
        }
        ui.separator();
        ui.add(
            egui::DragValue::new(&mut self.fade_frames)
                .range(1..=240)
                .suffix(" frames"),
        )
        .on_hover_text(format!("{:.2} s", self.fade_frames as f64 * frame));
        let label = if fade.is_some() {
            "Change dissolve"
        } else {
            "Dissolve"
        };
        if ui
            .add_enabled(free, egui::Button::new(label))
            .on_hover_text("a cross dissolve centred on the cut")
            .clicked()
        {
            let frames = self.fade_frames;
            start = Some((
                format!("{frames}-frame dissolve into {nb}"),
                ask(Change::Crossfade { frames }),
            ));
        }
        if let Some(f) = fade
            && ui
                .add_enabled(free, egui::Button::new("Straight cut"))
                .on_hover_text(format!("takes the {:.2} s {} off", f.length, f.name))
                .clicked()
        {
            start = Some((
                format!("straight cut into {nb}"),
                ask(Change::Crossfade { frames: 0 }),
            ));
        }
        if let Some((what, ask)) = start {
            let undo = Undo {
                plugin: plugin.to_string(),
                file: file.to_path_buf(),
                sequence: seq.name.clone(),
                what: what.clone(),
                steps: serde_json::Value::Null,
            };
            self.start_change(ui.ctx(), plugin, what, ask, Some(undo));
        }
    }

    /// Sounds… for a clip's take, a shot's or a bridge's.
    fn sounds_button(
        &mut self,
        ui: &mut egui::Ui,
        (under, name): (shotline_core::Under, String),
        (file, seq, c): (&Path, &SequenceInfo, &ClipInfo),
        free: bool,
    ) {
        let n = self.project.sounds(under).map_or(0, |s| s.len());
        let label = match n {
            0 => "Sounds…".to_string(),
            n => format!("Sounds ({n})…"),
        };
        if ui
            .add_enabled(free, egui::Button::new(label))
            .on_hover_text("effects or a voice-over over this take")
            .clicked()
        {
            // From the playhead when it's over the clip, else from where the clip starts.
            let title = crate::cut::seq_title(file, seq);
            let at = self
                .shown_deck(&title)
                .map(|d| d.position())
                .filter(|p| *p >= c.start && *p < c.start + c.length)
                .map_or(c.from, |p| c.from + (p - c.start) * c.speed);
            self.open_sounds(under, name, at);
        }
    }

    /// What a picked sound clip can do: what it's over, and that's sounds.
    fn sound_tools(
        &mut self,
        ui: &mut egui::Ui,
        file: &Path,
        seq: &SequenceInfo,
        c: &ClipInfo,
        free: bool,
    ) {
        // Looked up once per pick: a laid sound means reading each take's sounds.
        if self
            .sound_pick
            .as_ref()
            .is_none_or(|(id, f, _)| *id != c.id || *f != c.file)
        {
            self.sound_pick = Some((c.id.clone(), c.file.clone(), self.sound_under(c)));
        }
        let m = self.sound_pick.as_ref().expect("set").2.clone();
        ui.label(RichText::new(&m.clip).strong().color(theme::PAPER));
        let Some((under, name, at)) = m.under else {
            ui.weak("not over a take or bridge");
            return;
        };
        if m.laid {
            ui.weak(format!("over {name}"));
        }
        if let Some(s) = m.shot
            && ui.button("Open the shot").clicked()
        {
            self.tab = crate::Tab::Shots;
            self.open_shot(s);
        }
        if m.laid {
            if ui
                .add_enabled(free, egui::Button::new(format!("Sounds over {name}…")))
                .on_hover_text("remake it, take it off, or lay others")
                .clicked()
            {
                self.open_sounds(under, name, at);
            }
        } else {
            self.sounds_button(ui, (under, name), (file, seq, c), free);
        }
    }

    fn picked_tools(
        &mut self,
        ui: &mut egui::Ui,
        plugin: &str,
        file: &Path,
        seq: &SequenceInfo,
        c: &ClipInfo,
        free: bool,
    ) {
        let take = c
            .file
            .as_ref()
            .and_then(|f| self.edit_takes.get(f))
            .cloned();
        let bridge = c
            .file
            .as_ref()
            .and_then(|f| self.edit_bridges.get(f))
            .copied();
        let name = match (&take, bridge) {
            (Some(t), _) => t.to_string(),
            (None, Some((_, _, n))) => format!("bridge #{n}"),
            (None, None) => c.name.clone(),
        };
        ui.label(RichText::new(&name).strong().color(theme::PAPER));
        if let Some((id, _, _)) = bridge
            && ui
                .add_enabled(free, egui::Button::new("Bridge…"))
                .on_hover_text("its prompt and takes")
                .clicked()
        {
            self.open_bridge_of(id);
        }
        if let Some((id, take, _)) = bridge {
            let under = shotline_core::Under::Bridge(id, take);
            self.sounds_button(ui, (under, name.clone()), (file, seq, c), free);
        }
        let film = self.project.root.clone();
        let ask = |change| EditParams {
            film: film.clone(),
            file: file.to_path_buf(),
            sequence: seq.name.clone(),
            clip: c.id.clone(),
            change,
        };
        let mut start: Option<(String, EditParams)> = None;
        if let Some(t) = &take {
            if ui.button("Open the shot").clicked() {
                self.tab = crate::Tab::Shots;
                self.open_shot(t.shot);
            }
            self.sounds_button(ui, (t.into(), t.to_string()), (file, seq, c), free);
            let others: Vec<(String, PathBuf)> = self
                .project
                .takes(t.shot)
                .unwrap_or_default()
                .into_iter()
                .filter(|o| o.id != t.id)
                .filter_map(|o| {
                    let video = self.project.take_video(o.id).ok()?;
                    Some((o.to_string(), std::path::absolute(&video).unwrap_or(video)))
                })
                .collect();
            ui.add_enabled_ui(free && !others.is_empty(), |ui| {
                ui.menu_button("Swap take", |ui| {
                    for (o, video) in &others {
                        if ui.button(o).clicked() {
                            // All of the new take from the same point, however long it is.
                            let length = self
                                .ff
                                .as_ref()
                                .and_then(|ff| ff.probe(video).ok())
                                .map(|i| i.duration - c.from)
                                .filter(|l| *l > 0.0);
                            let change = Change::Replace {
                                file: video.clone(),
                                from: c.from,
                                length,
                            };
                            start = Some((format!("swap {name} for {o}"), ask(change)));
                            ui.close();
                        }
                    }
                })
                .response
                .on_hover_text("puts another take of the shot in its place; what follows moves when it's longer or shorter")
                .on_disabled_hover_text("the shot has no other take with a video");
            });
        }
        let frame = 1.0 / seq.fps.max(1.0);
        let before = c.start >= frame;
        if ui
            .add_enabled(
                free && before,
                egui::Button::new("Match colour to the clip before"),
            )
            .on_hover_text("the editor's colour match, to the last frame before this clip")
            .on_disabled_hover_text("nothing comes before it")
            .clicked()
        {
            let change = Change::Match {
                to: c.start - frame,
            };
            start = Some((format!("colour match of {name}"), ask(change)));
        }
        if ui
            .add_enabled(free, egui::Button::new("Input LUT…"))
            .on_hover_text("a .cube LUT on the clip, before the rest of its colour")
            .clicked()
            && let Some(lut) = rfd::FileDialog::new()
                .add_filter("LUT", &["cube"])
                .set_directory(&self.project.root)
                .pick_file()
        {
            let label = lut.file_name().unwrap_or_default().to_string_lossy();
            let what = format!("{label} on {name}");
            start = Some((what, ask(Change::Lut { file: Some(lut) })));
        }
        let replace = ask(Change::Undo {
            steps: serde_json::Value::Null,
        });
        self.process_menu(
            ui,
            plugin,
            replace,
            c,
            &name,
            free && self.process.is_none(),
        );
        if let Some((what, ask)) = start {
            let undo = Undo {
                plugin: plugin.to_string(),
                file: file.to_path_buf(),
                sequence: seq.name.clone(),
                what: what.clone(),
                steps: serde_json::Value::Null,
            };
            self.start_change(ui.ctx(), plugin, what, ask, Some(undo));
        }
    }

    /// Asks the plugin off the UI thread. `undo` is kept once the plugin says how; without one
    /// it's an undo itself, taken off the list once it worked.
    pub(crate) fn start_change(
        &mut self,
        ctx: &egui::Context,
        plugin: &str,
        what: String,
        ask: EditParams,
        undo: Option<Undo>,
    ) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let slot: Arc<Mutex<Option<_>>> = Arc::default();
        let (s, p, a, ctx) = (slot.clone(), plugin.to_string(), ask.clone(), ctx.clone());
        std::thread::spawn(move || {
            *s.lock().unwrap() = Some(host.edit_clip(&p, &a).map_err(|e| format!("{e:#}")));
            ctx.request_repaint();
        });
        self.changing = Some(Changing {
            what,
            undo,
            ask,
            plugin: plugin.to_string(),
            slot,
        });
    }

    /// Like `start_change`, with the change worked out first, off the UI thread.
    fn start_made_change(
        &mut self,
        ctx: &egui::Context,
        plugin: &str,
        what: String,
        mut ask: EditParams,
        undo: Undo,
        make: impl FnOnce() -> Result<Change, String> + Send + 'static,
    ) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let slot: Arc<Mutex<Option<_>>> = Arc::default();
        let (s, p, mut a, ctx) = (slot.clone(), plugin.to_string(), ask.clone(), ctx.clone());
        std::thread::spawn(move || {
            let got = make().and_then(|change| {
                a.change = change;
                host.edit_clip(&p, &a).map_err(|e| format!("{e:#}"))
            });
            *s.lock().unwrap() = Some(got);
            ctx.request_repaint();
        });
        ask.change = Change::Undo {
            steps: serde_json::Value::Null,
        };
        self.changing = Some(Changing {
            what,
            undo: Some(undo),
            ask,
            plugin: plugin.to_string(),
            slot,
        });
    }

    fn change_done(&mut self) {
        let Some(c) = &self.changing else {
            return;
        };
        let Some(got) = c.slot.lock().unwrap().take() else {
            return;
        };
        let c = self.changing.take().unwrap();
        let root = self.project.root.clone();
        let list = self.undo.get_or_insert_with(|| load_undo(&root));
        match got {
            Err(e) => {
                self.error(format!("{} didn't work: {}", c.what, short(&e, 300)));
                return;
            }
            Ok(done) => {
                match (c.undo, done.undo) {
                    (Some(mut u), Some(steps)) => {
                        u.steps = steps;
                        list.push(u);
                    }
                    // An undo: the newest entry for this sequence is the one it took back.
                    (None, _) => {
                        if let Some(i) = list.iter().rposition(|u| {
                            u.file == c.ask.file
                                && u.sequence == c.ask.sequence
                                && u.plugin == c.plugin
                        }) {
                            list.remove(i);
                        }
                    }
                    (Some(_), None) => {}
                }
                let list = list.clone();
                save_undo(&root, &list);
                if !done.saved {
                    self.status = Some((
                        "Changed in the editor, not saved there yet: it shows here once it is."
                            .into(),
                        false,
                    ));
                }
            }
        }
        self.edits_stale = true;
    }
}

/// The request and what it would cost.
type Priced = (Request, String);

/// A tool run on the used part of a clip, waiting for its price to be agreed.
pub(crate) struct Process {
    what: String,
    plugin: String,
    /// The swap once it's made: Replace with `from` the head kept before the used part.
    ask: EditParams,
    editor: String,
    head: f64,
    slot: Arc<Mutex<Option<Result<Priced, String>>>>,
    priced: Option<Priced>,
}

/// A sequence being rendered by its editor: into which file, and how it went.
pub(crate) type Rendering = (PathBuf, Arc<Mutex<Option<Result<PathBuf, String>>>>);

/// Seconds kept either side of a clip's used part, for the editor to trim into.
const HANDLE: f64 = 0.5;

impl App {
    /// Shots whose take in the sequence isn't their selected one: the take it uses, and the
    /// selected one's name.
    fn other_takes(&self, seq: &SequenceInfo) -> Vec<(Take, String)> {
        let mut by_shot: Vec<(ShotRef, Vec<&Take>)> = vec![];
        for c in seq.video.iter().flat_map(|t| &t.clips) {
            let Some(t) = c.file.as_ref().and_then(|f| self.edit_takes.get(f)) else {
                continue;
            };
            match by_shot.iter_mut().find(|(s, _)| *s == t.shot) {
                Some((_, ts)) if ts.iter().any(|x| x.id == t.id) => {}
                Some((_, ts)) => ts.push(t),
                None => by_shot.push((t.shot, vec![t])),
            }
        }
        by_shot
            .into_iter()
            .filter_map(|(shot, ts)| {
                // A shot cut from two takes has no one take to select.
                let [t] = ts[..] else { return None };
                let selected = self.film.cut.iter().find(|c| c.shot == shot)?;
                match &selected.take {
                    Some(s) if s.id == t.id => None,
                    Some(s) => Some((t.clone(), s.to_string())),
                    None => Some((t.clone(), "none".into())),
                }
            })
            .collect()
    }

    /// Shots of the film with a take but none of them in the sequence, e.g. one added after the
    /// editor's project was made, and where each would go: (take, video, clip, after it).
    fn missing(&mut self, seq: &SequenceInfo) -> Vec<(Take, PathBuf, String, bool)> {
        if self.cut_takes.is_none() {
            let mut all = vec![];
            for c in &self.film.cut {
                let got = match (&c.take, &c.video) {
                    (Some(t), Some(v)) => Some((t.clone(), v.clone())),
                    _ => self
                        .project
                        .takes(c.shot)
                        .unwrap_or_default()
                        .into_iter()
                        .rev()
                        .filter(|t| !t.removed())
                        .find_map(|t| {
                            let j = t.output()?;
                            let v = self.project.job_dir(j.id).join(j.outputs.first()?);
                            Some((t, v))
                        }),
                };
                all.extend(got);
            }
            self.cut_takes = Some(all);
        }
        let mut out = vec![];
        for (t, video) in self.cut_takes.iter().flatten() {
            if let Some((clip, after)) = self.place_of(t.shot, seq) {
                out.push((t.clone(), video.clone(), clip, after));
            }
        }
        out
    }

    /// Where a shot the sequence hasn't got would go: next to which clip, and after it or not.
    /// None when the sequence has it already.
    fn place_of(&self, shot: ShotRef, seq: &SequenceInfo) -> Option<(String, bool)> {
        let clips: Vec<(ShotRef, &ClipInfo)> = seq
            .video
            .iter()
            .flat_map(|t| &t.clips)
            .filter_map(|c| {
                Some((
                    c.file.as_ref().and_then(|f| self.edit_takes.get(f))?.shot,
                    c,
                ))
            })
            .collect();
        if clips.iter().any(|(s, _)| *s == shot) {
            return None;
        }
        let shots: Vec<ShotRef> = self.film.cut.iter().map(|c| c.shot).collect();
        let i = shots.iter().position(|r| *r == shot)?;
        // After the last clip of the nearest shot before it, or else before the first of
        // the nearest one after it.
        let before = shots[..i].iter().rev().find_map(|r| {
            clips
                .iter()
                .filter(|(s, _)| s == r)
                .max_by(|a, b| a.1.start.total_cmp(&b.1.start))
                .map(|(_, c)| (c.id.clone(), true))
        });
        before.or_else(|| {
            shots[i + 1..].iter().find_map(|r| {
                clips
                    .iter()
                    .filter(|(s, _)| s == r)
                    .min_by(|a, b| a.1.start.total_cmp(&b.1.start))
                    .map(|(_, c)| (c.id.clone(), false))
            })
        })
    }

    /// A take picked on the shot page goes in the editor's sequence too: in place of the
    /// shot's clip, or next to the shots around it when the sequence hasn't got the shot.
    pub(crate) fn follow_cut(&mut self, ctx: &egui::Context) {
        self.change_done();
        if self.cut_follow.is_none() || self.changing.is_some() {
            return;
        }
        if self.host.is_none() {
            self.cut_follow = None;
            return;
        }
        // The sequence as it is after the select, not before.
        self.refresh_edits(ctx);
        if self.edits_stale || self.edits_asked.is_some() {
            return;
        }
        let Some(t) = self.cut_follow.take() else {
            return;
        };
        let selected = self
            .film
            .cut
            .iter()
            .any(|c| c.take.as_ref().is_some_and(|s| s.id == t.id));
        let Some((file, name)) = crate::cut::main_sequence(&self.project.root, &self.edits.0)
        else {
            return;
        };
        let plugin = crate::cut::plugin_of(&self.edits.0, &file);
        let seq = self
            .edits
            .0
            .iter()
            .find(|e| e.file == file)
            .and_then(|e| e.sequences.iter().find(|s| s.name == name))
            .cloned();
        let (Some(seq), Ok(video)) = (seq, self.project.take_video(t.id)) else {
            return;
        };
        if !selected || plugin.is_empty() {
            return;
        }
        let video = std::path::absolute(&video).unwrap_or(video);
        let clips: Vec<ClipInfo> = seq
            .video
            .iter()
            .flat_map(|tr| &tr.clips)
            .filter(|c| {
                c.file
                    .as_ref()
                    .and_then(|f| self.edit_takes.get(f))
                    .is_some_and(|x| x.shot == t.shot)
            })
            .cloned()
            .collect();
        let (what, clip, change) = match &clips[..] {
            [] => {
                let Some((clip, after)) = self.place_of(t.shot, &seq) else {
                    return;
                };
                let change = Change::Insert {
                    file: video,
                    after,
                    sound: true,
                };
                (format!("{t} in"), clip, change)
            }
            [c] => {
                let was = c.file.as_ref().and_then(|f| self.edit_takes.get(f));
                if was.is_some_and(|w| w.id == t.id) {
                    return;
                }
                let was = was.map_or(c.name.clone(), |w| w.to_string());
                // All of the new take from the same point, as Swap take does.
                let length = self
                    .ff
                    .as_ref()
                    .and_then(|ff| ff.probe(&video).ok())
                    .map(|i| i.duration - c.from)
                    .filter(|l| *l > 0.0);
                let change = Change::Replace {
                    file: video,
                    from: c.from,
                    length,
                };
                (format!("swap {was} for {t}"), c.id.clone(), change)
            }
            _ => {
                self.info(format!(
                    "{t} selected; {} is cut from several clips in the sequence, so swap the \
                     one you want on the Cut page",
                    t.shot
                ));
                return;
            }
        };
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
            sequence: seq.name.clone(),
            what: what.clone(),
            steps: serde_json::Value::Null,
        };
        self.start_change(ctx, &plugin, what, ask, Some(undo));
    }

    /// Offers to put shots the sequence hasn't got into it, sound and all.
    pub(crate) fn missing_shots(
        &mut self,
        ui: &mut egui::Ui,
        plugin: &str,
        app: &str,
        file: &Path,
        seq: &SequenceInfo,
    ) {
        let missing = self.missing(seq);
        let mut put = None;
        for (t, video, clip, after) in missing {
            ui.horizontal_wrapped(|ui| {
                ui.weak(format!("{} isn't in {app}'s sequence.", t.shot));
                let next_to = seq
                    .video
                    .iter()
                    .flat_map(|t| &t.clips)
                    .find(|c| c.id == clip)
                    .map(|c| c.name.clone())
                    .unwrap_or_default();
                let tip = format!(
                    "{t} {} {next_to}, moving what follows later",
                    if after { "after" } else { "before" }
                );
                if ui
                    .add_enabled(
                        self.changing.is_none(),
                        egui::Button::new(format!("Put {t} in")),
                    )
                    .on_hover_text(tip)
                    .clicked()
                {
                    put = Some((t, video, clip, after));
                }
            });
        }
        let Some((t, video, clip, after)) = put else {
            return;
        };
        let video = std::path::absolute(&video).unwrap_or(video);
        let what = format!("{t} in");
        let ask = EditParams {
            film: self.project.root.clone(),
            file: file.to_path_buf(),
            sequence: seq.name.clone(),
            clip,
            change: Change::Insert {
                file: video,
                after,
                sound: true,
            },
        };
        let undo = Undo {
            plugin: plugin.to_string(),
            file: file.to_path_buf(),
            sequence: seq.name.clone(),
            what: what.clone(),
            steps: serde_json::Value::Null,
        };
        let selected = self
            .film
            .cut
            .iter()
            .any(|c| c.take.as_ref().is_some_and(|s| s.id == t.id));
        if !selected {
            self.flush();
            self.apply(vec![crate::edit::Action::Select(t)]);
        }
        self.start_change(ui.ctx(), plugin, what, ask, Some(undo));
    }

    /// Offers to select the takes the editor's sequence uses.
    pub(crate) fn take_sync(&mut self, ui: &mut egui::Ui, app: &str, seq: &SequenceInfo) {
        let other = self.other_takes(seq);
        if other.is_empty() {
            return;
        }
        let said: Vec<String> = other
            .iter()
            .map(|(t, s)| format!("{t} (selected: {s})"))
            .collect();
        ui.horizontal_wrapped(|ui| {
            ui.weak(format!("{app} uses {}.", said.join(", ")));
            if ui
                .button("Select these takes")
                .on_hover_text("so the film's own cut and the Shots page use them too")
                .clicked()
            {
                let acts = other
                    .iter()
                    .map(|(t, _)| crate::edit::Action::Select(t.clone()))
                    .collect();
                self.flush();
                self.apply(acts);
            }
        });
    }

    /// Render, through the editor, into renders/.
    pub(crate) fn edit_render(
        &mut self,
        ui: &mut egui::Ui,
        plugin: &str,
        file: &Path,
        seq: &SequenceInfo,
    ) {
        let got = self
            .edit_rendering
            .as_ref()
            .and_then(|(out, slot)| Some((out.clone(), slot.lock().unwrap().take()?)));
        if let Some((out, got)) = got {
            self.edit_rendering = None;
            match got {
                Ok(f) => {
                    self.status = Some((format!("Rendered {}", self.project.rel(&f)), false));
                    self.reload();
                }
                Err(e) => self.error(format!("can't render {}: {e}", self.project.rel(&out))),
            }
        }
        let busy = self.edit_rendering.is_some();
        if ui
            .add_enabled(!busy && self.host.is_some(), egui::Button::new("Render"))
            .on_hover_text("the sequence as the editor renders it, into renders/; free")
            .clicked()
        {
            let safe: String = seq
                .name
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let at = jiff::Zoned::now().strftime("%Y%m%d-%H%M%S");
            let out = self
                .project
                .root
                .join("renders")
                .join(format!("{safe}-{at}.mp4"));
            let ask = ExportParams {
                film: self.project.root.clone(),
                file: file.to_path_buf(),
                sequence: seq.name.clone(),
                out: out.clone(),
                saved: false,
                scale: None,
            };
            let slot: Arc<Mutex<Option<_>>> = Arc::default();
            let (s, host, p, ctx) = (
                slot.clone(),
                self.host.clone().unwrap(),
                plugin.to_string(),
                ui.ctx().clone(),
            );
            std::thread::spawn(move || {
                *s.lock().unwrap() = Some(host.export_edit(&p, &ask).map_err(|e| format!("{e:#}")));
                ctx.request_repaint();
            });
            self.edit_rendering = Some((out, slot));
        }
        if busy {
            ui.spinner();
            ui.weak("rendering…");
        }
    }

    /// The video tools of installed plugins, by plugin: upscale and interpolation.
    fn clip_ops(&self) -> Vec<(String, String, Op)> {
        let mut ops = vec![];
        for d in &self.plugins {
            for m in &d.models {
                if matches!(m.op, Op::VideoInterpolate | Op::VideoUpscale) {
                    ops.push((d.name.clone(), m.id.clone(), m.op.clone()));
                }
            }
        }
        ops
    }

    /// The menu of tools for a picked clip.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process_menu(
        &mut self,
        ui: &mut egui::Ui,
        editor: &str,
        ask: EditParams,
        c: &ClipInfo,
        name: &str,
        free: bool,
    ) {
        let ops = self.clip_ops();
        let video = c.file.clone().filter(|f| f.is_file());
        let mut pick = None;
        ui.add_enabled_ui(
            free && !ops.is_empty() && video.is_some() && self.ff.is_some(),
            |ui| {
                ui.menu_button("Process", |ui| {
                    for (plugin, model, op) in &ops {
                        if ui.button(format!("{op} · {plugin} {model}")).clicked() {
                            pick = Some((plugin.clone(), model.clone(), op.clone()));
                            ui.close();
                        }
                    }
                })
                .response
                .on_hover_text(
                    "runs it on the part of the clip in use, plus half a second either side, and \
                 swaps the result in",
                )
                .on_disabled_hover_text(if ops.is_empty() {
                    "no plugin has video tools; the local plugin has interpolation and upscaling"
                } else {
                    "the clip's file isn't there"
                });
            },
        );
        let (Some((plugin, model, op)), Some(video)) = (pick, video) else {
            return;
        };
        let head = c.from.min(HANDLE);
        let length = c.length * c.speed + head + HANDLE;
        let from = c.from - head;
        let part = self
            .project
            .root
            .join(".shotline/cache/clip-parts")
            .join(format!(
                "{}-{}.mp4",
                ask.clip,
                jiff::Timestamp::now().as_millisecond()
            ));
        let slot: Arc<Mutex<Option<_>>> = Arc::default();
        let (s, host, ff, film, p, ctx) = (
            slot.clone(),
            self.host.clone().unwrap(),
            self.ff.clone().unwrap(),
            self.project.root.clone(),
            plugin.clone(),
            ui.ctx().clone(),
        );
        let req = Request {
            op: op.clone(),
            model: model.clone(),
            inputs: [("video".to_string(), part.clone())].into(),
            ..Request::default()
        };
        std::thread::spawn(move || {
            let got = ff
                .part(&video, from, length, &part)
                .map_err(|e| e.to_string())
                .and_then(|()| {
                    host.op_quote(&film, &p, &req)
                        .map(|price| (req, price))
                        .map_err(|e| format!("{e:#}"))
                });
            *s.lock().unwrap() = Some(got);
            ctx.request_repaint();
        });
        self.process = Some(Process {
            what: format!("{model} on {name}"),
            plugin,
            ask,
            editor: editor.to_string(),
            head,
            slot,
            priced: None,
        });
    }

    /// The tool's price, to agree to or not, then the run and the swap.
    pub(crate) fn process_row(&mut self, ui: &mut egui::Ui, file: &Path, seq: &SequenceInfo) {
        let Some(p) = &mut self.process else {
            return;
        };
        let got = if p.priced.is_none() {
            p.slot.lock().unwrap().take()
        } else {
            None
        };
        match got {
            Some(Ok(priced)) => p.priced = Some(priced),
            Some(Err(e)) => {
                let what = p.what.clone();
                self.process = None;
                self.error(format!("{what} didn't work: {}", short(&e, 300)));
                return;
            }
            None => {}
        }
        let p = self.process.as_ref().unwrap();
        let mut go = false;
        let mut cancel = false;
        let free = self.changing.is_none();
        ui.horizontal(|ui| {
            ui.label(&p.what);
            let shown = match &p.priced {
                None => Price::Working("cutting the part and asking the price…"),
                Some((_, price)) => Price::Priced(price),
            };
            let pressed = PriceRow::new("Run")
                .go_hover("runs it on the part in use and swaps the result in")
                .show(ui, shown);
            match pressed {
                Some(Pressed::Go) if free => go = true,
                Some(Pressed::Cancel) => cancel = true,
                _ => {}
            }
        });
        if cancel {
            self.process = None;
        }
        if !go {
            return;
        }
        let p = self.process.take().unwrap();
        let (req, price) = p.priced.unwrap();
        let slot: Arc<Mutex<Option<_>>> = Arc::default();
        let (s, host, film, ctx) = (
            slot.clone(),
            self.host.clone().unwrap(),
            self.project.root.clone(),
            ui.ctx().clone(),
        );
        let (plugin, editor, head, mut ask) = (p.plugin, p.editor.clone(), p.head, p.ask.clone());
        std::thread::spawn(move || {
            let got = host
                .run_op(&film, &plugin, &req, &price)
                .and_then(|out| {
                    ask.change = Change::Replace {
                        file: std::path::absolute(&out).unwrap_or(out),
                        from: head,
                        length: None,
                    };
                    host.edit_clip(&editor, &ask)
                })
                .map_err(|e| format!("{e:#}"));
            *s.lock().unwrap() = Some(got);
            ctx.request_repaint();
        });
        self.changing = Some(Changing {
            what: p.what.clone(),
            undo: Some(Undo {
                plugin: p.editor.clone(),
                file: file.to_path_buf(),
                sequence: seq.name.clone(),
                what: p.what,
                steps: serde_json::Value::Null,
            }),
            ask: p.ask,
            plugin: p.editor,
            slot,
        });
    }
}
