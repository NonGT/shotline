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
        let picked = self.clip_pick.as_ref().and_then(|id| {
            seq.video
                .iter()
                .flat_map(|t| &t.clips)
                .find(|c| c.id == *id)
                .cloned()
        });
        if picked.is_none() {
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
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| match (&picked, &join) {
            (Some(c), _) => self.picked_tools(ui, &plugin, file, seq, c, busy.is_none()),
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
        clips
            .windows(2)
            .filter(|w| meet(w[0], w[1], fps))
            .map(|w| crate::lanes::Join {
                at: w[1].start,
                id: w[1].id.clone(),
                name: format!("{} to {}", name(w[0]), name(w[1])),
                picked: self.join_pick.as_ref() == Some(&w[1].id),
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
            && let Some((fa, fb)) = files
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
        let name = take.as_ref().map_or(c.name.clone(), |t| t.to_string());
        ui.label(RichText::new(&name).strong().color(theme::PAPER));
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
                            let change = Change::Replace {
                                file: video.clone(),
                                from: c.from,
                            };
                            start = Some((format!("swap {name} for {o}"), ask(change)));
                            ui.close();
                        }
                    }
                })
                .response
                .on_hover_text("puts another take of the shot in its place, same length")
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
    fn start_change(
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
        ui.horizontal(|ui| match &p.priced {
            None => {
                ui.spinner();
                ui.weak(format!(
                    "{}: cutting the part and asking the price…",
                    p.what
                ));
            }
            Some((_, price)) => {
                ui.label(format!("{} · {price}", p.what));
                go = ui
                    .add_enabled(self.changing.is_none(), egui::Button::new("Go"))
                    .clicked();
                cancel = ui.button("Cancel").clicked();
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
