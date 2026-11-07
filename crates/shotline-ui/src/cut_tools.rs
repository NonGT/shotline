//! Changing a clip of an editor's sequence from here: swap its take, match its colour to the clip
//! before, give it a LUT. The editor plugin makes the change in the editor and says how to take
//! it back; those undo steps are kept in the film's cache, newest last.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};
use shotline_core::protocol::{Change, ClipInfo, EditParams, Edited, SequenceInfo};

use crate::{App, short, theme};

const KEPT: usize = 30;

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
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            match &picked {
                Some(c) => self.picked_tools(ui, &plugin, file, seq, c, busy.is_none()),
                None => {
                    ui.weak("Click a clip to change it from here.");
                }
            }
            if let Some(what) = &busy {
                ui.spinner();
                ui.weak(what);
            }
        });
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
