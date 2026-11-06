//! A shot's lines, each recorded on its own to hear how it comes out before paying for a take.
//! A line is typed and recorded, then picks among everything its speaker has recorded, in any
//! shot; the scene plays the picks through at their start times.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use shotline_core::protocol::Op;
use shotline_core::{Job, Kind, Shot, ShotRef};
use shotline_media::lines::Clip;
use shotline_media::play::PcmPipe;

use crate::edit::{Action, LineForm};
use crate::player::{hush, listen};
use crate::{App, heading, hint, icon_button, remove_button, short, spend, theme};

#[derive(Clone, Default)]
pub(crate) enum Say {
    #[default]
    Idle,
    Pricing(ShotRef, usize),
    Priced(ShotRef, usize, String),
    Recording(ShotRef, usize),
    Failed(ShotRef, usize, String),
    Done(ShotRef, usize, Result<PathBuf, String>),
}

impl Say {
    fn line(&self) -> Option<(ShotRef, usize)> {
        match self {
            Say::Idle => None,
            Say::Pricing(r, i)
            | Say::Priced(r, i, _)
            | Say::Recording(r, i)
            | Say::Failed(r, i, _)
            | Say::Done(r, i, _) => Some((*r, *i)),
        }
    }

    fn busy(&self) -> bool {
        matches!(self, Say::Pricing(..) | Say::Recording(..))
    }

    /// Follows its line when line `from` of `r` is moved to `to`.
    fn moved(&mut self, r: ShotRef, from: usize, to: usize) {
        if let Say::Pricing(sr, k)
        | Say::Priced(sr, k, _)
        | Say::Recording(sr, k)
        | Say::Failed(sr, k, _)
        | Say::Done(sr, k, _) = self
            && *sr == r
        {
            *k = if *k == from {
                to
            } else if from < *k && *k <= to {
                *k - 1
            } else if to <= *k && *k < from {
                *k + 1
            } else {
                *k
            };
        }
    }
}

/// A line being dragged: which, where it was grabbed, and a fresh id for each drag so the rows
/// it pushed aside don't slide back once it's dropped.
#[derive(Clone, Copy)]
pub(crate) struct LineDrag {
    from: usize,
    grab: egui::Vec2,
    session: u64,
}

/// What's being heard: a recording or the scene, until it's played out.
pub(crate) struct Hearing {
    what: String,
    _pipe: PcmPipe,
    until: Instant,
}

/// The id salt of the line being added.
const DRAFT: usize = usize::MAX;

fn play_icon(p: &egui::Painter, s: egui::Rect, stroke: egui::Stroke) {
    let pts = vec![
        s.left_top(),
        egui::pos2(s.right(), s.center().y),
        s.left_bottom(),
    ];
    p.add(egui::Shape::convex_polygon(
        pts,
        stroke.color,
        egui::Stroke::NONE,
    ));
}

fn stop_icon(p: &egui::Painter, s: egui::Rect, stroke: egui::Stroke) {
    p.rect_filled(s.shrink(1.0), 1.0, stroke.color);
}

/// An arrow, to go where something is.
fn go_icon(p: &egui::Painter, s: egui::Rect, stroke: egui::Stroke) {
    let y = s.center().y;
    p.line_segment([egui::pos2(s.left(), y), egui::pos2(s.right(), y)], stroke);
    p.line_segment(
        [
            egui::pos2(s.right() - 4.0, y - 4.0),
            egui::pos2(s.right(), y),
        ],
        stroke,
    );
    p.line_segment(
        [
            egui::pos2(s.right() - 4.0, y + 4.0),
            egui::pos2(s.right(), y),
        ],
        stroke,
    );
}

/// Six dots, to drag a line by.
fn grip(ui: &mut egui::Ui, i: usize) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(12.0, 22.0), egui::Sense::drag());
    r.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Move line {}", i + 1))
    });
    let color = if r.hovered() || r.dragged() {
        theme::PAPER
    } else {
        theme::DUST
    };
    dots(ui.painter(), rect, color);
    r.on_hover_cursor(egui::CursorIcon::Grab)
}

fn dots(p: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    for row in 0..3 {
        for col in 0..2 {
            let c = egui::pos2(
                rect.left() + 3.0 + col as f32 * 5.0,
                rect.center().y - 5.0 + row as f32 * 5.0,
            );
            p.circle_filled(c, 1.3, color);
        }
    }
}

/// The file a finished recording made.
fn file(app: &App, j: &Job) -> PathBuf {
    app.project.job_dir(j.id).join(&j.outputs[0])
}

/// A line's start: dragged like the shot's length, or left empty to follow the line before.
fn starts(ui: &mut egui::Ui, l: &mut LineForm) {
    let mut v: f64 =
        l.at.trim()
            .trim_end_matches('s')
            .trim()
            .parse()
            .unwrap_or(-0.1);
    // One width whatever it says, so the speakers line up row to row.
    let size = egui::vec2(64.0, ui.spacing().interact_size.y);
    let r = ui
        .add_sized(
            size,
            egui::DragValue::new(&mut v)
                .range(-0.1..=600.0)
                .speed(0.05)
                .max_decimals(2)
                .custom_formatter(|v, _| {
                    if v < 0.0 {
                        "after".into()
                    } else {
                        format!("{} s", shotline_core::ledger::money(v))
                    }
                })
                .custom_parser(|t| {
                    let t = t.trim().trim_end_matches('s').trim();
                    if t.is_empty() || t == "after" {
                        Some(-0.1)
                    } else {
                        t.parse().ok()
                    }
                }),
        )
        .on_hover_text(
            "Seconds into the shot it starts. Drag or type; all the way down, or empty, it \
             follows the line before.",
        );
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::SpinButton, true, "Starts"));
    if r.changed() {
        l.at = if v < 0.0 {
            String::new()
        } else {
            shotline_core::ledger::money(v)
        };
    }
}

fn who(ui: &mut egui::Ui, i: usize, l: &mut LineForm, cast: &[String]) {
    egui::ComboBox::from_id_salt(("who", i))
        .selected_text(format!("@{}", l.who))
        .width(96.0)
        .show_ui(ui, |ui| {
            for c in cast {
                ui.selectable_value(&mut l.who, c.clone(), format!("@{c}"));
            }
        })
        .response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Who says it"));
}

/// What the recordings dropdown was asked to do, by recording id.
enum Pick {
    Use(String),
    Record,
    Play(String),
    Stop,
    Delete(String),
    Open(ShotRef),
    ShowAll,
}

impl App {
    pub(crate) fn load_recordings(&mut self) -> anyhow::Result<()> {
        self.voices = HashMap::new();
        if let Some((r, s)) = self.open() {
            for l in &s.lines {
                if !self.voices.contains_key(&l.who) {
                    let all = self.project.recordings_by(&l.who, Some(r))?;
                    self.voices.insert(l.who.clone(), all);
                }
            }
        }
        Ok(())
    }

    /// Picks up a recording once it's made.
    pub(crate) fn said(&mut self) {
        let done = {
            let mut s = self.say.lock().unwrap();
            let Say::Done(r, i, out) = s.clone() else {
                return;
            };
            *s = match &out {
                Ok(_) => Say::Idle,
                Err(e) => Say::Failed(r, i, e.clone()),
            };
            (r, i, out)
        };
        self.reload();
        if let Err(e) = self.load_takes() {
            self.error(e.to_string());
        }
        match done {
            (r, i, Ok(f)) => {
                self.info(format!("line {} of {r} recorded", i + 1));
                self.hear(&f.display().to_string(), &f);
            }
            (r, i, Err(e)) => self.error(format!("line {} of {r}: {e}", i + 1)),
        }
    }

    fn ask(&self, ctx: &egui::Context, r: ShotRef, i: usize, at: Option<String>) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let (say, ctx, film) = (self.say.clone(), ctx.clone(), self.project.root.clone());
        *say.lock().unwrap() = match &at {
            Some(_) => Say::Recording(r, i),
            None => Say::Pricing(r, i),
        };
        std::thread::spawn(move || {
            let s = match at {
                None => match host.say_quote(&film, r, i) {
                    Ok(cost) => Say::Priced(r, i, cost),
                    Err(e) => Say::Failed(r, i, format!("{e:#}")),
                },
                Some(cost) => Say::Done(
                    r,
                    i,
                    host.say(&film, r, i, &cost).map_err(|e| format!("{e:#}")),
                ),
            };
            *say.lock().unwrap() = s;
            ctx.request_repaint();
        });
    }

    /// Plays a sound through the app's output, stopping whatever was playing.
    pub(crate) fn hear(&mut self, what: &str, path: &Path) {
        self.hush();
        if let Some(d) = &mut self.deck {
            d.pause(self.audio.as_ref());
        }
        let Some(ff) = self.ff.clone() else {
            self.error("can't play without ffmpeg; install it or set SHOTLINE_FFMPEG".into());
            return;
        };
        let Some(audio) = self.audio() else {
            self.error("there's no sound output to play it on".into());
            return;
        };
        let long = ff.audio_duration(path).unwrap_or(30.0);
        match listen(&ff, audio, path) {
            Ok(pipe) => {
                self.hearing = Some(Hearing {
                    what: what.into(),
                    _pipe: pipe,
                    until: Instant::now() + Duration::from_secs_f64(long + 0.3),
                })
            }
            Err(e) => self.error(format!("can't play it: {e}")),
        }
    }

    pub(crate) fn hush(&mut self) {
        if self.hearing.take().is_some()
            && let Some(a) = &self.audio
        {
            hush(a);
        }
    }

    /// True while `what` is playing.
    fn hearing(&mut self, ctx: &egui::Context, what: &str) -> bool {
        if self
            .hearing
            .as_ref()
            .is_some_and(|h| Instant::now() > h.until)
        {
            self.hush();
        }
        match &self.hearing {
            Some(h) if h.what == what => {
                ctx.request_repaint_after(Duration::from_millis(200));
                true
            }
            _ => false,
        }
    }

    /// The framed Lines section: one row a line, a draft row while one's being added, and the
    /// scene played through.
    /// How many lines the shot's video model takes, when the recipe sends it each line as its
    /// own clip, and the model's name. None when it mixes them or isn't sent them.
    fn line_room(&mut self, r: ShotRef, s: &Shot) -> Option<(usize, String)> {
        let steps = self.project.steps(s).ok()?;
        if !steps.iter().any(|x| x.op == Op::VideoGenerate && x.audio) {
            return None;
        }
        let b = self.built(r).ok()?;
        let m = self
            .plugins
            .iter()
            .filter(|d| d.name == b.plugin)
            .flat_map(|d| &d.models)
            .find(|m| m.id == b.model && m.op == Op::VideoGenerate)?;
        if m.audio_mix {
            return None;
        }
        Some((m.max_audio?, b.model))
    }

    pub(crate) fn lines(
        &mut self,
        ui: &mut egui::Ui,
        r: ShotRef,
        s: &Shot,
        acts: &mut Vec<Action>,
    ) {
        let title = heading(ui, "Lines").id;
        ui.add_space(8.0);
        let cast: Vec<String> = self
            .film
            .elements
            .iter()
            .filter(|(e, _)| e.kind == Kind::Cast)
            .map(|(e, _)| e.handle.clone())
            .collect();
        if cast.is_empty() {
            hint(
                ui,
                "Only cast speak, and there's no cast yet. Add some in Elements.",
            );
            return;
        }
        let n = self.form.lines.len().min(s.lines.len());
        let lifted = self.line_drag.filter(|d| d.from < n && n > 1);
        let pointer = ui.ctx().pointer_interact_pos();
        // Where it'd land: past every other row whose middle is above the pointer.
        let to = lifted.zip(pointer).map(|(d, p)| {
            (0..n)
                .filter(|&k| k != d.from)
                .filter(|&k| self.line_rects.get(k).is_some_and(|rc| rc.center().y < p.y))
                .count()
        });
        let pitch = lifted
            .and_then(|d| self.line_rects.get(d.from))
            .map_or(0.0, |rc| rc.height() + ui.spacing().item_spacing.y);
        let priced = match &*self.say.lock().unwrap() {
            Say::Priced(pr, pi, cost) if *pr == r => Some((*pi, cost.clone())),
            _ => None,
        };
        let mut open = None;
        let mut any_open = false;
        let mut rects = vec![];
        for i in 0..n {
            // The others make room for it as it's dragged over them.
            let shift = match (lifted, to) {
                (Some(d), Some(t)) if i != d.from => {
                    let goal = if d.from < t && i > d.from && i <= t {
                        -pitch
                    } else if t < d.from && i >= t && i < d.from {
                        pitch
                    } else {
                        0.0
                    };
                    let id = ui.scope_id().with(("line shift", d.session, i));
                    ui.ctx().animate_value_with_time(id, goal, 0.12)
                }
                _ => 0.0,
            };
            let moving = lifted.is_some_and(|d| d.from == i);
            let shown = egui::emath::TSTransform::from_translation(egui::vec2(0.0, shift));
            let row = ui.with_visual_transform(shown, |ui| {
                if moving {
                    ui.multiply_opacity(0.0);
                }
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        let g = grip(ui, i)
                            .on_hover_text("drag to put it elsewhere; its start stays as it is");
                        if g.drag_started()
                            && let Some(p) = g.interact_pointer_pos()
                        {
                            let at = self.line_rects.get(i).map_or(g.rect.min, |rc| rc.min);
                            self.drags += 1;
                            self.line_drag = Some(LineDrag {
                                from: i,
                                grab: p - at,
                                session: self.drags,
                            });
                        } else if g.drag_stopped() {
                            if let Some(t) = to
                                && t != i
                            {
                                acts.push(Action::MoveLine(r, i, t));
                                self.say.lock().unwrap().moved(r, i, t);
                            }
                            self.line_drag = None;
                        } else if moving && !g.dragged() {
                            self.line_drag = None;
                        }
                        match self.line_row(ui, r, s, i, &cast, acts) {
                            Some(true) => open = Some((r, i)),
                            Some(false) => any_open = true,
                            None => {}
                        }
                    });
                    // The price on its own row, as wide as the line; out of the way while
                    // lines are moved.
                    if let Some((_, cost)) = priced.as_ref().filter(|(pi, _)| *pi == i)
                        && lifted.is_none()
                    {
                        ui.horizontal(|ui| {
                            ui.add_space(12.0 + ui.spacing().item_spacing.x);
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button("Cancel").clicked() {
                                        *self.say.lock().unwrap() = Say::Idle;
                                    }
                                    let wide = egui::Button::new(format!("Record it for {cost}"))
                                        .min_size(egui::vec2(ui.available_width(), 0.0));
                                    if spend(ui, wide)
                                        .on_hover_text("spends this and writes it to the ledger")
                                        .clicked()
                                    {
                                        self.ask(ui.ctx(), r, i, Some(cost.clone()));
                                    }
                                },
                            );
                        });
                    }
                });
            });
            rects.push(row.response.rect);
        }
        if let (Some(d), Some(t), Some(p)) = (lifted, to, pointer) {
            self.ghost(ui, r, s, d, t, p);
        }
        self.line_rects = rects;
        self.voice_open = open;

        let full = self
            .line_room(r, s)
            .filter(|(max, _)| s.lines.len() >= *max);
        if self.form.adding {
            any_open |= self.draft_row(ui, r, s, &cast, title, acts);
        } else if ui
            .add_enabled(full.is_none(), egui::Button::new("Add line"))
            .on_disabled_hover_text(full.map_or(String::new(), |(max, model)| {
                format!(
                    "{model} takes at most {max} lines, one clip each; \
                     have them lip synced after for more"
                )
            }))
            .clicked()
        {
            let f = &mut self.form;
            f.adding = true;
            if f.new.who.is_empty() || !cast.contains(&f.new.who) {
                f.new.who = s.lines.last().map_or(cast[0].clone(), |l| l.who.clone());
            }
        }
        if open.is_none() && !any_open {
            self.show_all = false;
        }

        if !s.lines.is_empty() {
            ui.add_space(8.0);
            self.play_scene(ui, r, s);
        }
    }

    /// The dragged line, lifted off the list under the pointer, and the gap it would drop into.
    fn ghost(&self, ui: &egui::Ui, r: ShotRef, s: &Shot, d: LineDrag, to: usize, p: egui::Pos2) {
        let (Some(from), Some(slot)) = (self.line_rects.get(d.from), self.line_rects.get(to))
        else {
            return;
        };
        let gap_y = ui.ctx().animate_value_with_time(
            ui.scope_id().with(("line gap", d.session)),
            slot.top(),
            0.12,
        );
        let gap = egui::Rect::from_min_size(egui::pos2(slot.left(), gap_y), from.size());
        ui.painter().rect_stroke(
            gap,
            4.0,
            egui::Stroke::new(1.0, theme::DUST),
            egui::StrokeKind::Inside,
        );

        let rect = egui::Rect::from_min_size(egui::pos2(from.left(), (p - d.grab).y), from.size());
        let layer = egui::LayerId::new(egui::Order::Tooltip, ui.scope_id().with(("line ghost", r)));
        let painter = ui.ctx().layer_painter(layer);
        painter.add(
            egui::epaint::Shadow {
                offset: [0, 6],
                blur: 16,
                spread: 0,
                color: egui::Color32::from_black_alpha(140),
            }
            .as_shape(rect, 4.0),
        );
        painter.rect(
            rect,
            4.0,
            theme::RAISED,
            egui::Stroke::new(1.0, theme::DUST),
            egui::StrokeKind::Inside,
        );
        dots(
            &painter,
            egui::Rect::from_min_size(rect.min, egui::vec2(12.0, rect.height())),
            theme::PAPER,
        );
        let l = &self.form.lines[d.from];
        let at = if l.at.trim().is_empty() {
            "after".to_string()
        } else {
            format!("{} s", l.at.trim())
        };
        let words = self
            .picked(s, d.from)
            .and_then(|j| j.prompt)
            .unwrap_or_else(|| l.text.clone());
        let font = egui::TextStyle::Body.resolve(ui.style());
        let mut x = rect.left() + 24.0;
        for (text, color) in [
            (at, theme::GRAPHITE),
            (format!("@{}", l.who), theme::GRAPHITE),
            (words, theme::PAPER),
        ] {
            let galley =
                painter.layout(text, font.clone(), color, (rect.right() - x - 8.0).max(0.0));
            let w = galley.size().x;
            painter.galley(
                egui::pos2(x, rect.center().y - galley.size().y / 2.0),
                galley,
                color,
            );
            x += w + 16.0;
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }

    /// The recording line `i` sends, among its speaker's.
    fn picked(&self, s: &Shot, i: usize) -> Option<Job> {
        let line = s.lines.get(i)?;
        let id = line.audio.as_deref()?;
        self.voices
            .get(&line.who)?
            .iter()
            .find(|j| j.id.to_string() == id)
            .cloned()
    }

    /// One line: its start and speaker, then either its words to record (with ▼ to pick a
    /// recording instead) or the dropdown of recordings once it has one. Some while a dropdown is
    /// open, true for the full one.
    fn line_row(
        &mut self,
        ui: &mut egui::Ui,
        r: ShotRef,
        s: &Shot,
        i: usize,
        cast: &[String],
        acts: &mut Vec<Action>,
    ) -> Option<bool> {
        starts(ui, &mut self.form.lines[i]);
        who(ui, i, &mut self.form.lines[i], cast);
        let picked = self.picked(s, i);
        // Kept as a dropdown while it's open, even once its pick is deleted.
        let recorded = picked.is_some() || self.voice_open == Some((r, i));
        let who = s.lines[i].who.clone();
        let mut open = None;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if remove_button(ui).clicked() {
                acts.push(Action::RemoveLine(r, i));
            }
            if recorded {
                open = self
                    .recordings(ui, r, i, &who, picked.as_ref(), false, acts)
                    .then_some(true);
            } else {
                open = self
                    .recordings(ui, r, i, &who, None, true, acts)
                    .then_some(false);
                let has = !self.form.lines[i].text.trim().is_empty();
                self.record_button(ui, r, i, has);
                ui.add(
                    egui::TextEdit::singleline(&mut self.form.lines[i].text)
                        .hint_text(RichText::new("What they say").color(theme::DUST))
                        .desired_width(ui.available_width()),
                )
                .widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::Role::TextInput,
                        true,
                        format!("Line {}", i + 1),
                    )
                });
            }
        });
        open
    }

    /// The line being added: saved once it's left with words in it, recorded right away, or
    /// given a recording its speaker made before. True while that dropdown is open.
    fn draft_row(
        &mut self,
        ui: &mut egui::Ui,
        r: ShotRef,
        s: &Shot,
        cast: &[String],
        title: egui::Id,
        acts: &mut Vec<Action>,
    ) -> bool {
        let mut open = false;
        ui.horizontal(|ui| {
            ui.add_space(12.0 + ui.spacing().item_spacing.x);
            starts(ui, &mut self.form.new);
            who(ui, DRAFT, &mut self.form.new, cast);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, "Don't add it", crate::close_icon).clicked() {
                    self.form.adding = false;
                    self.form.new.text.clear();
                    self.form.new.at.clear();
                    return;
                }
                let who = self.form.new.who.clone();
                open = self.recordings(ui, r, DRAFT, &who, None, true, acts);
                let has = !self.form.new.text.trim().is_empty();
                let busy = self.say.lock().unwrap().busy();
                if ui
                    .add_enabled(has && !busy, egui::Button::new("Record…"))
                    .on_hover_text("hear it before a take; asking the price is free")
                    .on_disabled_hover_text(if has {
                        "still recording another line"
                    } else {
                        "type what they say first"
                    })
                    .clicked()
                {
                    // Saved first: what's recorded is a line of the shot.
                    self.flush();
                    self.apply(vec![Action::AddLine(r, self.form.new.clone())]);
                    self.ask(ui.ctx(), r, s.lines.len(), None);
                    return;
                }
                let text = ui
                    .add(
                        egui::TextEdit::singleline(&mut self.form.new.text)
                            .hint_text(RichText::new("What they say").color(theme::DUST))
                            .desired_width(ui.available_width()),
                    )
                    .labelled_by(title);
                if text.lost_focus() && has {
                    acts.push(Action::AddLine(r, self.form.new.clone()));
                }
            });
        });
        open
    }

    /// Record… and what follows it, right to left: the price, the spend button, the wait.
    fn record_button(&mut self, ui: &mut egui::Ui, r: ShotRef, i: usize, has_words: bool) {
        let say = self.say.lock().unwrap().clone();
        let mine = say.line() == Some((r, i));
        match &say {
            Say::Pricing(..) if mine => {
                ui.weak("asking the price…");
                ui.spinner();
            }
            Say::Recording(..) if mine => {
                ui.weak("recording…");
                ui.spinner();
            }
            _ => {
                if ui
                    .add_enabled(has_words && !say.busy(), egui::Button::new("Record…"))
                    .on_hover_text("hear it before a take; asking the price is free")
                    .on_disabled_hover_text(if has_words {
                        "still recording another line"
                    } else {
                        "type what they say first"
                    })
                    .clicked()
                {
                    // What's typed is saved first, so that's what gets recorded.
                    self.flush();
                    self.ask(ui.ctx(), r, i, None);
                }
                if let Say::Failed(.., e) = &say
                    && mine
                {
                    ui.label(RichText::new(short(e, 40)).color(theme::FLAG))
                        .on_hover_text(e);
                }
            }
        }
    }

    /// What `who` has recorded, from any shot, loaded the first time it's asked for.
    fn voices_of(&mut self, who: &str, r: ShotRef) -> Vec<Job> {
        if !self.voices.contains_key(who) {
            let all = match self.project.recordings_by(who, Some(r)) {
                Ok(all) => all,
                Err(e) => {
                    self.error(e.to_string());
                    vec![]
                }
            };
            self.voices.insert(who.to_string(), all);
        }
        self.voices[who].clone()
    }

    /// The dropdown of what the speaker has recorded, for line `i` (or the one being added,
    /// DRAFT): this shot's first, the rest behind Show all, grouped by chapter and shot.
    /// `compact` is just the ▼ beside Record…. True while it's open.
    #[allow(clippy::too_many_arguments)]
    fn recordings(
        &mut self,
        ui: &mut egui::Ui,
        r: ShotRef,
        i: usize,
        who: &str,
        picked: Option<&Job>,
        compact: bool,
        acts: &mut Vec<Action>,
    ) -> bool {
        let all = self.voices_of(who, r);
        let shown = match picked.and_then(|j| j.prompt.as_deref()) {
            Some(t) => t.to_string(),
            None => "pick one, or record it again".into(),
        };
        let width = ui.available_width();
        let ctx = ui.ctx().clone();
        let playing: Vec<String> = all
            .iter()
            .map(|j| j.id.to_string())
            .filter(|id| self.hearing(&ctx, id))
            .collect();
        let show_all = self.show_all;
        let names = self.shot_names();
        // Which lines send each recording.
        let mut uses: HashMap<String, Vec<(ShotRef, usize)>> = HashMap::new();
        for (_, shots) in &self.film.chapters {
            for (sr, sh) in shots {
                for (k, l) in sh.lines.iter().enumerate() {
                    if let Some(a) = &l.audio {
                        uses.entry(a.clone()).or_default().push((*sr, k));
                    }
                }
            }
        }
        let mut pick = None;
        let label = match (compact, i) {
            (true, DRAFT) => "Pick a recording".to_string(),
            (true, _) => format!("Pick a recording for line {}", i + 1),
            (false, _) => format!("Line {}", i + 1),
        };
        let combo = egui::ComboBox::from_id_salt(("voice", r, i, compact))
            .height(crate::grouped::MAX)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        let combo = if compact {
            combo.selected_text("").width(ui.spacing().icon_width)
        } else {
            combo.selected_text(short(&shown, 200)).width(width)
        };
        let open = ui
            .add_enabled_ui(!all.is_empty(), |ui| {
                combo.show_ui(ui, |ui| {
                    let p = recording_list(
                        ui,
                        (r, i),
                        &all,
                        picked.map(|j| j.id.to_string()),
                        &playing,
                        show_all,
                        &names,
                        &uses,
                        !compact,
                    );
                    if matches!(p, Some(Pick::Use(_) | Pick::Record | Pick::Open(_))) {
                        ui.close();
                    }
                    pick = p;
                })
            })
            .inner;
        crate::grouped::remeasure(ui.ctx());
        let mut resp = open
            .response
            .on_disabled_hover_text(format!("@{who} has nothing recorded yet"));
        if compact {
            resp = resp.on_hover_text(format!(
                "one @{who} recorded before, in this shot or another"
            ));
        }
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &label));
        match pick {
            Some(Pick::Use(id)) if i == DRAFT => {
                // Added with the words of what it picks, and that recording.
                self.flush();
                let mut l = self.form.new.clone();
                if let Some(j) = all.iter().find(|j| j.id.to_string() == id) {
                    l.text = j.prompt.clone().unwrap_or_default();
                }
                self.apply(vec![
                    Action::AddLine(r, l),
                    Action::PickRecording(r, self.form.lines.len(), Some(id)),
                ]);
            }
            Some(Pick::Use(id)) => acts.push(Action::PickRecording(r, i, Some(id))),
            Some(Pick::Record) => acts.push(Action::PickRecording(r, i, None)),
            Some(Pick::Play(id)) => {
                if let Some(j) = all.iter().find(|j| j.id.to_string() == id) {
                    let f = file(self, j);
                    self.hear(&id, &f);
                }
            }
            Some(Pick::Stop) => self.hush(),
            Some(Pick::Delete(id)) => acts.push(Action::DeleteRecording(r, id)),
            Some(Pick::ShowAll) => self.show_all = true,
            Some(Pick::Open(to)) => {
                self.flush();
                self.open_shot(to);
            }
            None => {}
        }
        open.inner.is_some()
    }

    /// Each shot's "01-010  plot" and its chapter's "01 Title", for the dropdown's headers.
    pub(crate) fn shot_names(&self) -> HashMap<ShotRef, (String, String)> {
        let mut out = HashMap::new();
        for (c, shots) in &self.film.chapters {
            let chapter = format!("{:02} {}", c.number, c.title);
            for (r, s) in shots {
                let about = [&s.plot, &s.title, &s.prompt]
                    .into_iter()
                    .find(|t| !t.is_empty())
                    .map_or(String::new(), |t| short(t, 60));
                out.insert(*r, (chapter.clone(), format!("{r}  {about}")));
            }
        }
        out
    }

    /// Every line's picked recording at its start time.
    pub(crate) fn play_scene(&mut self, ui: &mut egui::Ui, r: ShotRef, s: &Shot) {
        let mut clips = vec![];
        let mut missing = None;
        for (i, line) in s.lines.iter().enumerate() {
            match self.picked(s, i) {
                Some(j) => clips.push(Clip {
                    path: file(self, &j),
                    at: line.at,
                }),
                None => {
                    missing.get_or_insert(i);
                }
            }
        }
        let what = format!("{r} scene");
        let playing = self.hearing(ui.ctx(), &what);
        let label = if playing { "Stop" } else { "Play the scene" };
        let b = ui
            .add_enabled(missing.is_none(), egui::Button::new(label))
            .on_hover_text("every line's recording at its start time")
            .on_disabled_hover_text(match missing {
                Some(i) => format!("line {} isn't recorded yet", i + 1),
                None => String::new(),
            });
        if !b.clicked() {
            return;
        }
        if playing {
            self.hush();
            return;
        }
        let Some(ff) = self.ff.clone() else {
            self.error("can't play without ffmpeg; install it or set SHOTLINE_FFMPEG".into());
            return;
        };
        let out = self
            .project
            .root
            .join(".shotline/cache/scenes")
            .join(format!("{r}.wav"));
        let joined = std::fs::create_dir_all(out.parent().unwrap())
            .map_err(anyhow::Error::from)
            .and_then(|_| Ok(ff.join_lines(&clips, &out)?));
        match joined {
            Ok(_) => self.hear(&what, &out),
            Err(e) => self.error(format!("can't put {r}'s lines together: {e}")),
        }
    }
}

/// The dropdown's insides: New recording, then the recordings grouped by chapter and shot.
/// One another line sends can't be deleted; it takes you to that line's shot instead.
#[allow(clippy::too_many_arguments)]
fn recording_list(
    ui: &mut egui::Ui,
    here: (ShotRef, usize),
    all: &[Job],
    picked: Option<String>,
    playing: &[String],
    show_all: bool,
    names: &HashMap<ShotRef, (String, String)>,
    uses: &HashMap<String, Vec<(ShotRef, usize)>>,
    new_item: bool,
) -> Option<Pick> {
    let mut pick = None;
    let width = ui.available_width().max(400.0);
    ui.set_width(width);
    if new_item {
        if ui
            .add(crate::grouped::item(false, "New recording").min_size(egui::vec2(width, 0.0)))
            .on_hover_text("type the words again, e.g. with other tags, and record them")
            .clicked()
        {
            pick = Some(Pick::Record);
        }
        ui.separator();
    }
    let others = all.iter().filter(|j| j.shot != Some(here.0)).count();
    let rows: Vec<(String, String, &Job)> = all
        .iter()
        .filter(|j| show_all || j.shot == Some(here.0))
        .map(|j| {
            let (c, s) = j
                .shot
                .and_then(|r| names.get(&r).cloned())
                .unwrap_or_default();
            (c, s, j)
        })
        .collect();
    let empty = rows.is_empty();
    let mut more_clicked = false;
    let grown = crate::grouped::list(
        ui,
        &rows,
        |ui, j| {
            let id = j.id.to_string();
            let words = j.prompt.as_deref().unwrap_or("");
            let elsewhere: Vec<(ShotRef, usize)> = uses
                .get(&id)
                .into_iter()
                .flatten()
                .filter(|u| **u != here)
                .copied()
                .collect();
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    match elsewhere.iter().find(|(r, _)| *r != here.0) {
                        Some((r, l)) => {
                            if icon_button(ui, &format!("Go to {r}"), go_icon)
                                .on_hover_text(format!(
                                    "line {} of {r} uses it, so it can't be deleted; go there",
                                    l + 1
                                ))
                                .clicked()
                            {
                                pick = Some(Pick::Open(*r));
                            }
                        }
                        None if !elsewhere.is_empty() => {
                            ui.add_enabled_ui(false, |ui| {
                                crate::delete_button(ui, &format!("Delete {words}"))
                            })
                            .inner
                            .on_disabled_hover_text(format!(
                                "line {} uses it too, so it can't be deleted",
                                elsewhere[0].1 + 1
                            ));
                        }
                        None => {
                            let bin = crate::delete_button(ui, &format!("Delete {words}"));
                            if bin.on_hover_text("delete this recording").clicked() {
                                pick = Some(Pick::Delete(id.clone()));
                            }
                        }
                    }
                    let on = playing.contains(&id);
                    let (icon, what): (fn(&egui::Painter, egui::Rect, egui::Stroke), _) = if on {
                        (stop_icon, "Stop")
                    } else {
                        (play_icon, "Play")
                    };
                    if icon_button(ui, &format!("{what} {words}"), icon).clicked() {
                        pick = Some(if on {
                            Pick::Stop
                        } else {
                            Pick::Play(id.clone())
                        });
                    }
                    if !elsewhere.is_empty() {
                        let all: Vec<String> = elsewhere
                            .iter()
                            .map(|(r, l)| format!("line {} of {r}", l + 1))
                            .collect();
                        let n = elsewhere.len();
                        ui.label(
                            RichText::new(if n == 1 {
                                "1 other line".to_string()
                            } else {
                                format!("{n} other lines")
                            })
                            .small()
                            .color(theme::DUST),
                        )
                        .on_hover_text(format!("also used by {}", all.join(", ")));
                    }
                    let chosen = picked.as_deref() == Some(id.as_str());
                    let b = crate::grouped::item(chosen, words)
                        .min_size(egui::vec2(ui.available_width(), 0.0));
                    if ui.add(b).on_hover_text(words).clicked() {
                        pick = Some(Pick::Use(id.clone()));
                    }
                });
            });
        },
        |ui| {
            if !show_all && others > 0 {
                let more = format!("Show all ({others} from other shots)");
                more_clicked = ui
                    .add(crate::grouped::item(false, more).min_size(egui::vec2(width, 0.0)))
                    .clicked();
            }
            if empty {
                ui.weak("nothing recorded by them in this shot");
            }
        },
    );
    crate::grouped::remeasure_later(ui, grown);
    if more_clicked {
        pick = Some(Pick::ShowAll);
    }
    pick
}
