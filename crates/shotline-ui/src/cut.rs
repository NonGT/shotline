//! The Cut tab: the shots in play order, how they join, music over them, and renders.

use std::path::PathBuf;

use eframe::egui::{self, RichText};
use shotline_core::{MatchAt, Transition};

use crate::edit::Action;
use crate::{App, PAGE, hint, page, remove_button, short, theme};

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
                    if self.film.cut.is_empty() {
                        hint(ui, "No shots yet. Add some in Shots.");
                        return;
                    }
                    let mut acts = vec![];
                    self.cut_actions(ui);
                    ui.add_space(10.0);
                    self.cut_shots(ui);
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
                self.play("the cut", &files, false);
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

    fn cut_shots(&mut self, ui: &mut egui::Ui) {
        let at = self.timeline_form.colormatch.at;
        let mut open = None;
        egui::Grid::new("cut")
            .num_columns(4)
            .spacing(egui::vec2(18.0, 6.0))
            .show(ui, |ui| {
                for (i, c) in self.film.cut.iter().enumerate() {
                    if ui
                        .link(RichText::new(c.shot.to_string()).color(theme::PAPER))
                        .on_hover_text("open the shot")
                        .clicked()
                    {
                        open = Some(c.shot);
                    }
                    ui.weak(match c.transition {
                        Transition::Cut => "on a cut",
                        Transition::Continue => "from the shot before",
                    });
                    match &c.take {
                        Some(t) if c.video.is_some() => ui.label(format!("take #{}", t.number)),
                        _ => ui.label(RichText::new("no take").color(theme::FLAG)),
                    };
                    if c.colormatch(at, i == 0) {
                        ui.weak("colour matched");
                    } else {
                        ui.label("");
                    }
                    ui.end_row();
                }
            });
        if let Some(r) = open {
            self.tab = crate::Tab::Shots;
            self.open_shot(r);
        }
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
