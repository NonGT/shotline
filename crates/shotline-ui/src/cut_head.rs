//! The top of the Cut page, one row that never grows: where the cut comes from, what's out of
//! sync with the editor, renders, and Render.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui::{self, RichText};
use shotline_core::Take;
use shotline_core::protocol::{Change, EditParams, ExportParams, SequenceInfo};

use crate::cut::Render;
use crate::cut_tools::Undo;
use crate::{App, hint, icon, kit, short, theme};

/// Cut page state that isn't the sequence itself.
#[derive(Default)]
pub(crate) struct CutUi {
    /// A render playing in the monitor in place of the cut.
    pub showing: Option<PathBuf>,
    /// The last render made, to play or reveal.
    pub rendered: Option<PathBuf>,
    pub stop: Arc<AtomicBool>,
    /// When the editor started rendering.
    pub since: Option<Instant>,
    /// The join, and its dissolve's length, the field was last set from.
    pub fade_for: Option<String>,
    /// The take picked on shotline's own cut, by its place in the cut.
    pub own_pick: Option<usize>,
    /// The join picked on shotline's own cut, by the place in the cut of the shot after it.
    pub own_join: Option<usize>,
    /// Shots still to put in the editor's sequence, one change at a time.
    pub put_queue: Vec<Take>,
    /// Sounds laid over the editor's takes and bridges, as absolute paths.
    pub laid: std::collections::HashSet<PathBuf>,
    /// Where clips start and end on the video, for the transport to jump between.
    pub edits: Vec<f64>,
}

impl App {
    pub(crate) fn cut_head(&mut self, ui: &mut egui::Ui, editor: Option<&(PathBuf, SequenceInfo)>) {
        self.put_next(ui.ctx());
        ui.horizontal(|ui| {
            ui.set_min_height(32.0);
            self.breadcrumb(ui, editor);
            ui.add_space(8.0);
            self.sync_chip(ui, editor);
            if editor.is_none() {
                self.missing_chip(ui);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                match editor {
                    Some((file, seq)) => self.edit_render(ui, file, seq),
                    None => self.own_render(ui),
                }
                self.rendered_row(ui);
                self.renders_menu(ui);
                self.open_buttons(ui);
            });
        });
    }

    /// "edit.fcproj > Main edit · 720 × 1280 · 24 fps · 0:08.0", or shotline's own cut.
    fn breadcrumb(&mut self, ui: &mut egui::Ui, editor: Option<&(PathBuf, SequenceInfo)>) {
        let Some((file, seq)) = editor else {
            ui.label(
                RichText::new("shotline's cut")
                    .size(18.0)
                    .color(theme::PAPER)
                    .strong(),
            )
            .on_hover_text("the selected take of each shot, in order");
            let n = self.film.cut.len();
            ui.label(RichText::new(crate::cut::plural(n, "shot")).color(theme::DUST));
            if !self.plugins.iter().any(|d| d.editor) {
                ui.label(
                    RichText::new(
                        "With an editor plugin like filmcraft, this cut opens in a video editor.",
                    )
                    .color(theme::DUST),
                );
            }
            return;
        };
        let name = file.file_name().unwrap_or_default().to_string_lossy();
        ui.label(RichText::new(name.as_ref()).color(theme::DUST))
            .on_hover_text(self.project.rel(file));
        ui.label(RichText::new("/").color(theme::DUST));
        let all: Vec<(PathBuf, String, String)> = self
            .edits
            .0
            .iter()
            .flat_map(|e| {
                let f = e.file.file_name().unwrap_or_default().to_string_lossy();
                e.sequences
                    .iter()
                    .map(move |s| (e.file.clone(), s.name.clone(), format!("{f} · {}", s.name)))
            })
            .collect();
        let several_seqs = all.len() > 1;
        let title = RichText::new(&seq.name)
            .size(18.0)
            .color(theme::PAPER)
            .strong();
        if several_seqs {
            let several = self.edits.0.len() > 1;
            let r = ui.add(egui::Button::new(title).frame(false).right_text("   "));
            let c = egui::pos2(r.rect.right() - 12.0, r.rect.center().y);
            ui.painter().add(egui::Shape::convex_polygon(
                vec![
                    c + egui::vec2(-5.0, -2.5),
                    c + egui::vec2(5.0, -2.5),
                    c + egui::vec2(0.0, 3.5),
                ],
                theme::DUST,
                egui::Stroke::NONE,
            ));
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Sequence"));
            let r = r.on_hover_text("pick another sequence");
            let mut pick = None;
            egui::Popup::menu(&r).show(|ui| {
                for (f, s, label) in &all {
                    let on = f == file && *s == seq.name;
                    let shown = if several { label } else { s };
                    if ui.selectable_label(on, shown).clicked() {
                        pick = Some((f.clone(), s.clone()));
                    }
                }
            });
            if let Some(p) = pick {
                self.cut_view = Some(p);
            }
        } else {
            ui.label(title);
        }
        let fps = if seq.fps.fract() == 0.0 {
            format!("{}", seq.fps)
        } else {
            format!("{:.3}", seq.fps)
        };
        ui.label(
            RichText::new(format!(
                "{} × {} · {fps} fps · {}",
                seq.width,
                seq.height,
                crate::player::timecode(seq.length(), seq.fps),
            ))
            .color(theme::DUST),
        );
    }

    /// One chip for everything the editor's sequence and the film disagree on, opening a list
    /// with what to do about each.
    fn sync_chip(&mut self, ui: &mut egui::Ui, editor: Option<&(PathBuf, SequenceInfo)>) {
        self.fix_finished();
        let (plugin, app) = editor
            .and_then(|(file, _)| self.edits.0.iter().find(|e| e.file == *file))
            .map(|e| (e.plugin.clone(), e.app.clone()))
            .unwrap_or_default();
        let (others, missing) = match editor {
            Some((_, seq)) if !plugin.is_empty() => (self.other_takes(seq), self.missing(seq)),
            Some((_, seq)) => (self.other_takes(seq), vec![]),
            None => (vec![], vec![]),
        };
        let empty = SequenceInfo::default();
        let seq = editor.map_or(&empty, |(_, s)| s);
        // Projects that can't be read, or have nothing to show.
        let broken: Vec<(String, Option<String>)> = self
            .edits
            .0
            .iter()
            .filter_map(|e| {
                let f = e.file.file_name().unwrap_or_default().to_string_lossy();
                match &e.error {
                    Some(why) => Some((format!("{f} can't be read"), Some(why.clone()))),
                    None if e.sequences.is_empty() => Some((format!("{f} has no sequences"), None)),
                    None => None,
                }
            })
            .collect();
        let warnings = self.edits.1.clone();
        let n = usize::from(!others.is_empty()) + missing.len() + warnings.len() + broken.len();
        if n == 0 && self.edit_fixed.is_none() {
            return;
        }
        let label = match n {
            0 => "In sync".to_string(),
            1 => "1 thing to sync".to_string(),
            n => format!("{n} things to sync"),
        };
        let color = if warnings.is_empty() && broken.is_empty() {
            theme::WARN
        } else {
            theme::FLAG
        };
        let r = ui.add(
            egui::Button::new(RichText::new(label).color(color))
                .corner_radius(12.0)
                .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.5))),
        );
        let free = self.changing.is_none() && self.edit_rendering.is_none();
        let mut ask_fix = None;
        let mut select = false;
        let mut put: Vec<Take> = vec![];
        egui::Popup::menu(&r)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_max_width(440.0);
                for (said, why) in &broken {
                    let r = ui.label(RichText::new(said).color(theme::FLAG));
                    if let Some(why) = why {
                        r.on_hover_text(why);
                    }
                }
                for w in &warnings {
                    ui.label(RichText::new(&w.text).color(theme::FLAG));
                    if let Some(fix) = &w.fix {
                        ui.horizontal(|ui| {
                            let busy = self.edit_fixing.is_some() || self.host.is_none();
                            if ui
                                .add_enabled(!busy, egui::Button::new(&fix.label))
                                .clicked()
                            {
                                ask_fix = Some(fix.clone());
                            }
                            if self
                                .edit_fixing
                                .as_ref()
                                .is_some_and(|(l, _)| *l == fix.label)
                            {
                                ui.spinner();
                                ui.weak("working on it…");
                            }
                        });
                    }
                    ui.add_space(6.0);
                }
                match &self.edit_fixed {
                    Some(Ok(said)) => {
                        ui.weak(said);
                    }
                    Some(Err(e)) => {
                        ui.label(RichText::new(e).color(theme::FLAG));
                    }
                    None => {}
                }
                if !others.is_empty() {
                    let said: Vec<String> = others
                        .iter()
                        .map(|(t, s)| format!("{t} (selected: {s})"))
                        .collect();
                    ui.label(format!("{app} uses {}.", said.join(", ")));
                    select = ui
                        .button("Select these takes")
                        .on_hover_text("so the film's own cut and the Shots page use them too")
                        .clicked();
                    ui.add_space(6.0);
                }
                if !missing.is_empty() {
                    ui.label(format!("Not in {app}'s sequence yet:"));
                    for (t, _, clip, after) in &missing {
                        ui.horizontal(|ui| {
                            let next_to = seq
                                .video
                                .iter()
                                .flat_map(|t| &t.clips)
                                .find(|c| c.id == *clip)
                                .map(|c| c.name.clone())
                                .unwrap_or_default();
                            let selected = self.is_selected(t);
                            let tip = format!(
                                "puts {t} {} {next_to}, moving what follows later{}",
                                if *after { "after" } else { "before" },
                                if selected { "" } else { ", and selects it" }
                            );
                            if ui
                                .add_enabled(free, egui::Button::new(format!("Put {t} in")))
                                .on_hover_text(tip)
                                .clicked()
                            {
                                put = vec![t.clone()];
                            }
                            if !selected {
                                ui.weak("and selects it");
                            }
                        });
                    }
                    if missing.len() > 1
                        && ui
                            .add_enabled(free, egui::Button::new("Put all in"))
                            .on_hover_text("one after another, each next to the shots around it")
                            .clicked()
                    {
                        put = missing.iter().map(|(t, ..)| t.clone()).collect();
                    }
                }
            });
        if let Some(fix) = ask_fix {
            self.start_fix(ui.ctx(), fix);
        }
        if select {
            let acts = others
                .iter()
                .map(|(t, _)| crate::edit::Action::Select(t.clone()))
                .collect();
            self.flush();
            self.apply(acts);
        }
        if !put.is_empty() {
            self.cut_ui.put_queue = put;
        }
    }

    fn is_selected(&self, t: &Take) -> bool {
        self.film
            .cut
            .iter()
            .any(|c| c.take.as_ref().is_some_and(|s| s.id == t.id))
    }

    /// Puts the next shot waiting into the editor's sequence once it's free, selecting its take
    /// when it isn't.
    fn put_next(&mut self, ctx: &egui::Context) {
        if self.cut_ui.put_queue.is_empty()
            || self.changing.is_some()
            || self.edits_stale
            || self.edits_asked.is_some()
        {
            return;
        }
        let t = self.cut_ui.put_queue.remove(0);
        let Some((file, name)) = crate::cut::main_sequence(&self.project.root, &self.edits.0)
        else {
            self.cut_ui.put_queue.clear();
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
        let Some(seq) = seq else { return };
        let Some((_, video, clip, after)) =
            self.missing(&seq).into_iter().find(|(m, ..)| m.id == t.id)
        else {
            return;
        };
        let video = std::path::absolute(&video).unwrap_or(video);
        let what = format!("{t} in");
        let ask = EditParams {
            film: self.project.root.clone(),
            file: file.clone(),
            sequence: seq.name.clone(),
            clip,
            change: Change::Insert {
                file: video,
                after,
                sound: true,
            },
        };
        let undo = Undo {
            plugin: plugin.clone(),
            file,
            sequence: seq.name.clone(),
            what: what.clone(),
            steps: serde_json::Value::Null,
        };
        if !self.is_selected(&t) {
            self.flush();
            self.apply(vec![crate::edit::Action::Select(t.clone())]);
            self.info(format!("{t} put in and selected"));
        }
        self.start_change(ctx, &plugin, what, ask, Some(undo));
    }

    /// Shots of shotline's own cut that have no take, which Play skips and Render refuses.
    fn missing_chip(&mut self, ui: &mut egui::Ui) {
        let missing: Vec<shotline_core::ShotRef> = self
            .film
            .cut
            .iter()
            .filter(|c| c.video.is_none())
            .map(|c| c.shot)
            .collect();
        if missing.is_empty() {
            return;
        }
        let label = format!(
            "{} without a take",
            crate::cut::plural(missing.len(), "shot")
        );
        let r = ui.add(
            egui::Button::new(RichText::new(label).color(theme::WARN))
                .corner_radius(12.0)
                .stroke(egui::Stroke::new(1.0, theme::WARN.gamma_multiply(0.5))),
        );
        let mut open = None;
        egui::Popup::menu(&r).show(|ui| {
            ui.set_max_width(360.0);
            hint(ui, "They're left out of Play and stop a render.");
            for s in &missing {
                if ui.link(format!("Open {s}")).clicked() {
                    open = Some(*s);
                }
            }
        });
        if let Some(s) = open {
            self.tab = crate::Tab::Shots;
            self.open_shot(s);
        }
    }

    /// The film's renders, to play here or find on disk.
    fn renders_menu(&mut self, ui: &mut egui::Ui) {
        let n = self.film.renders.len();
        if n == 0 {
            ui.add_enabled(false, egui::Button::new("Renders"))
                .on_disabled_hover_text(
                    "none yet; Render puts them in renders/, which git leaves out",
                );
            return;
        }
        let r = kit::drop_button(ui, format!("Renders ({n})"), true);
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "Renders"));
        let mut play = None;
        let mut reveal = None;
        egui::Popup::menu(&r).show(|ui| {
            ui.set_min_width(300.0);
            for f in self.film.renders.iter().rev() {
                let name = self.project.rel(f);
                ui.horizontal(|ui| {
                    if crate::icon_button(ui, &format!("Play {name}"), crate::play_icon).clicked() {
                        play = Some(f.clone());
                    }
                    ui.label(RichText::new(&name).color(theme::PAPER));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("Reveal")
                            .on_hover_text("show it in its folder")
                            .clicked()
                        {
                            reveal = Some(f.clone());
                        }
                    });
                });
            }
        });
        if let Some(f) = play {
            self.cut_ui.showing = Some(f);
        }
        if let Some(f) = reveal {
            self.reveal(&f);
        }
    }

    /// The render just made: play it here, or find it.
    fn rendered_row(&mut self, ui: &mut egui::Ui) {
        let Some(f) = self.cut_ui.rendered.clone() else {
            return;
        };
        if ui
            .small_button("Reveal")
            .on_hover_text("show it in its folder")
            .clicked()
        {
            self.reveal(&f);
        }
        let name = self.project.rel(&f);
        if ui
            .small_button(format!("{} {}", icon::PLAY, short(&name, 32)))
            .on_hover_text("play it here")
            .clicked()
        {
            self.cut_ui.showing = Some(f);
        }
    }

    fn reveal(&mut self, f: &Path) {
        let f = std::path::absolute(f).unwrap_or_else(|_| f.to_path_buf());
        let mut cmd = if cfg!(target_os = "macos") {
            let mut c = std::process::Command::new("open");
            c.arg("-R").arg(&f);
            c
        } else if cfg!(windows) {
            let mut c = std::process::Command::new("explorer");
            c.arg(format!("/select,{}", f.display()));
            c
        } else {
            let mut c = std::process::Command::new("xdg-open");
            c.arg(f.parent().unwrap_or(Path::new(".")));
            c
        };
        if let Err(e) = cmd.spawn() {
            self.error(format!("can't show {}: {e}", f.display()));
        }
    }

    /// Render for shotline's own cut: how far it is and Stop while it runs.
    fn own_render(&mut self, ui: &mut egui::Ui) {
        let missing = self.film.cut.iter().any(|c| c.video.is_none());
        let state = self.render.lock().unwrap().clone();
        let running = matches!(state, Render::Running(_));
        let can = !missing && !running && self.host.is_some() && !self.film.cut.is_empty();
        if ui
            .add_enabled_ui(can, |ui| {
                kit::primary_button(ui, Some(icon::DOWNLOAD), "Render")
            })
            .inner
            .on_hover_text("one video with the joins, music and sounds, into renders/; free")
            .on_disabled_hover_text(if running {
                "rendering already"
            } else {
                "every shot needs a take first"
            })
            .clicked()
        {
            self.start_render(ui.ctx());
        }
        match &state {
            Render::Running(step) => {
                if ui
                    .button("Stop")
                    .on_hover_text("stops the render; nothing is kept")
                    .clicked()
                {
                    self.cut_ui.stop.store(true, Ordering::Relaxed);
                }
                ui.weak(short(step, 40));
                ui.spinner();
            }
            Render::Failed(_) if self.cut_ui.stop.load(Ordering::Relaxed) => {
                ui.weak("render stopped");
            }
            Render::Failed(e) => {
                ui.label(RichText::new(short(e, 60)).color(theme::FLAG))
                    .on_hover_text(e);
            }
            _ => {}
        }
    }

    fn start_render(&mut self, ctx: &egui::Context) {
        // What's on screen is what renders.
        self.flush();
        let Some(host) = self.host.clone() else {
            return;
        };
        self.cut_ui.rendered = None;
        self.cut_ui.stop = Arc::default();
        *self.render.lock().unwrap() = Render::Running("starting".into());
        let (state, ctx, film) = (self.render.clone(), ctx.clone(), self.project.root.clone());
        let stop = self.cut_ui.stop.clone();
        std::thread::spawn(move || {
            let step = state.clone();
            let c = ctx.clone();
            let mut on = move |s: String| {
                *step.lock().unwrap() = Render::Running(s);
                c.request_repaint();
            };
            let done = match host.render(&film, stop, &mut on) {
                Ok((f, about)) => Render::Done(f, about),
                Err(e) => Render::Failed(format!("{e:#}")),
            };
            *state.lock().unwrap() = done;
            ctx.request_repaint();
        });
    }

    /// Once a render is done: say so, list it, and offer it to play.
    pub(crate) fn render_finished(&mut self) {
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
        self.cut_ui.rendered = Some(f);
    }

    /// Render, through the editor, into renders/. The editor's render can't be stopped from
    /// here, so it says how long it's been going.
    fn edit_render(&mut self, ui: &mut egui::Ui, file: &Path, seq: &SequenceInfo) {
        let (plugin, app) = self
            .edits
            .0
            .iter()
            .find(|e| e.file == file)
            .map(|e| (e.plugin.clone(), e.app.clone()))
            .unwrap_or_default();
        if plugin.is_empty() {
            return;
        }
        let busy = self.edit_rendering.is_some();
        let can = !busy && self.host.is_some() && self.changing.is_none();
        if ui
            .add_enabled_ui(can, |ui| {
                kit::primary_button(ui, Some(icon::DOWNLOAD), "Render")
            })
            .inner
            .on_hover_text(format!(
                "the sequence as {app} renders it, into renders/; free"
            ))
            .on_disabled_hover_text(if busy {
                "rendering already"
            } else {
                "wait for the change being made"
            })
            .clicked()
        {
            self.start_edit_render(ui.ctx(), &plugin, file, seq);
        }
        if busy {
            let secs = self.cut_ui.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
            ui.weak(format!(
                "rendering in {app}… {}",
                crate::player::clock(secs)
            ))
            .on_hover_text(format!("{app}'s render can't be stopped from here"));
            ui.spinner();
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(500));
        }
    }

    fn start_edit_render(
        &mut self,
        ctx: &egui::Context,
        plugin: &str,
        file: &Path,
        seq: &SequenceInfo,
    ) {
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
        let Some(host) = self.host.clone() else {
            return;
        };
        let slot: Arc<Mutex<Option<_>>> = Arc::default();
        let (s, p, ctx) = (slot.clone(), plugin.to_string(), ctx.clone());
        std::thread::spawn(move || {
            *s.lock().unwrap() = Some(host.export_edit(&p, &ask).map_err(|e| format!("{e:#}")));
            ctx.request_repaint();
        });
        self.cut_ui.rendered = None;
        self.cut_ui.since = Some(Instant::now());
        self.edit_rendering = Some((out, slot));
    }

    pub(crate) fn edit_render_finished(&mut self) {
        let got = self
            .edit_rendering
            .as_ref()
            .and_then(|(out, slot)| Some((out.clone(), slot.lock().unwrap().take()?)));
        let Some((out, got)) = got else { return };
        self.edit_rendering = None;
        self.cut_ui.since = None;
        match got {
            Ok(f) => {
                self.status = Some((format!("Rendered {}", self.project.rel(&f)), false));
                self.reload();
                self.cut_ui.rendered = Some(f);
            }
            Err(e) => self.error(format!("can't render {}: {e}", self.project.rel(&out))),
        }
    }
}
