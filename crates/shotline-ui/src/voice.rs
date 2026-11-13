//! A shot's lines, each recorded on its own to hear how it comes out before paying for a take.
//! A line is typed and recorded, then picks among everything its speaker has recorded, in any
//! shot; the scene plays the picks through at their start times.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use shotline_core::protocol::Op;
use shotline_core::{Job, Kind, Shot, ShotRef};
use shotline_media::lines::{Clip, GAP};
use shotline_media::play::PcmPipe;

use crate::edit::{Action, LineForm};
use crate::player::{hush, listen};
use crate::price::{Pressed, Price, PriceRow};
use crate::{
    App, heading, hint, icon, icon_button, kit, pause_icon, play_icon, short, stop_icon, theme,
};

fn voice_file() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Voice", &crate::VOICE_FILES)
        .pick_file()
}

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
    started: Instant,
    /// Where in the sound it started.
    from: f64,
}

/// How wide a line's state chip and its Record button are, so the rows line up.
const CHIP: f32 = 86.0;
const RECORD: f32 = 128.0;

/// The id salt of the line being added.
const DRAFT: usize = usize::MAX;

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

    /// Picks up a recording once it's made. One that didn't work stays under its line, said in
    /// full, until it's dismissed.
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
        if let (r, i, Ok(f)) = done {
            // The one it had stays among the speaker's recordings, to go back to.
            self.info(format!("line {} of {r} recorded", i + 1));
            self.hear(&f.display().to_string(), &f);
        }
    }

    /// Asks the price of line `i`, or records it at `at`. The line being added (DRAFT) is
    /// priced from `draft`'s speaker and words, as it isn't saved until it's paid for.
    fn ask(
        &self,
        ctx: &egui::Context,
        r: ShotRef,
        i: usize,
        at: Option<String>,
        draft: Option<(String, String)>,
    ) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let (say, ctx, film) = (self.say.clone(), ctx.clone(), self.project.root.clone());
        *say.lock().unwrap() = match &at {
            Some(_) => Say::Recording(r, i),
            None => Say::Pricing(r, i),
        };
        std::thread::spawn(move || {
            let s = match (at, draft) {
                (None, Some((who, text))) => match host.speech_quote(&film, Some(r), &who, &text) {
                    Ok(cost) => Say::Priced(r, i, cost),
                    Err(e) => Say::Failed(r, i, format!("{e:#}")),
                },
                (None, None) => match host.say_quote(&film, r, i) {
                    Ok(cost) => Say::Priced(r, i, cost),
                    Err(e) => Say::Failed(r, i, format!("{e:#}")),
                },
                (Some(cost), _) => Say::Done(
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
        self.hear_from(what, path, 0.0);
    }

    fn hear_from(&mut self, what: &str, path: &Path, from: f64) {
        self.hush();
        self.paused = None;
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
        match listen(&ff, audio, path, from) {
            Ok(pipe) => {
                self.hearing = Some(Hearing {
                    what: what.into(),
                    _pipe: pipe,
                    until: Instant::now() + Duration::from_secs_f64((long - from).max(0.0) + 0.3),
                    started: Instant::now(),
                    from,
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

    /// What's being heard, if anything.
    pub(crate) fn hearing_what(&self) -> Option<&str> {
        self.hearing.as_ref().map(|h| h.what.as_str())
    }

    /// True while `what` is playing.
    pub(crate) fn hearing(&mut self, ctx: &egui::Context, what: &str) -> bool {
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
        let mut rects = vec![];
        let mut heads = vec![];
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
                    let head = ui.horizontal(|ui| {
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
                        self.line_row(ui, r, s, i, &cast, acts);
                    });
                    heads.push(head.response.rect.height());
                    // Kept while lines move, so they keep their height.
                    self.line_player(ui, r, s, i);
                    self.line_notes(ui, r, i, &s.lines[i].who.clone());
                    // The price on its own row, as wide as the line; out of the way while
                    // lines are moved.
                    if let Some((_, cost)) = priced.as_ref().filter(|(pi, _)| *pi == i)
                        && lifted.is_none()
                    {
                        ui.horizontal(|ui| {
                            ui.add_space(12.0 + ui.spacing().item_spacing.x);
                            match PriceRow::new("Record").show(ui, Price::Priced(cost)) {
                                Some(Pressed::Go) => {
                                    self.ask(ui.ctx(), r, i, Some(cost.clone()), None)
                                }
                                Some(Pressed::Cancel) => *self.say.lock().unwrap() = Say::Idle,
                                _ => {}
                            }
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
        self.line_heads = heads;

        let full = self
            .line_room(r, s)
            .filter(|(max, _)| s.lines.len() >= *max);
        if self.form.adding {
            self.draft_row(ui, r, &cast, title, acts);
            if let Some(cost) = priced
                .as_ref()
                .filter(|(pi, _)| *pi == DRAFT)
                .map(|(_, c)| c)
            {
                ui.horizontal(|ui| {
                    ui.add_space(12.0 + ui.spacing().item_spacing.x);
                    match PriceRow::new("Record").show(ui, Price::Priced(cost)) {
                        // Only now is it a line of the shot: what's paid for is what's kept.
                        Some(Pressed::Go) => {
                            self.flush();
                            self.apply(vec![Action::AddLine(r, self.form.new.clone())]);
                            self.ask(ui.ctx(), r, s.lines.len(), Some(cost.clone()), None);
                        }
                        Some(Pressed::Cancel) => *self.say.lock().unwrap() = Say::Idle,
                        _ => {}
                    }
                });
            }
            let who = self.form.new.who.clone();
            self.line_notes(ui, r, DRAFT, &who);
        } else if ui
            .add_enabled(full.is_none(), egui::Button::new("Add line"))
            .on_disabled_hover_text(full.map_or(String::new(), |(max, model)| {
                format!("{model} takes at most {max} lines, one clip each")
            }))
            .clicked()
        {
            let f = &mut self.form;
            f.adding = true;
            if f.new.who.is_empty() || !cast.contains(&f.new.who) {
                f.new.who = s.lines.last().map_or(cast[0].clone(), |l| l.who.clone());
            }
        }
        self.recordings_popup(ui, r, s, acts);

        if !s.lines.is_empty() {
            ui.add_space(8.0);
            self.play_scene(ui, r, s);
        }
    }

    /// The dragged line, lifted off the list under the pointer, and the gap it would drop into.
    fn ghost(
        &mut self,
        ui: &egui::Ui,
        r: ShotRef,
        s: &Shot,
        d: LineDrag,
        to: usize,
        p: egui::Pos2,
    ) {
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
        let head_h = self
            .line_heads
            .get(d.from)
            .copied()
            .unwrap_or(rect.height());
        dots(
            &painter,
            egui::Rect::from_min_size(rect.min, egui::vec2(12.0, head_h)),
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
        let head_y = rect.top() + head_h / 2.0;
        let mut x = rect.left() + 24.0;
        for (text, color) in [
            (at, theme::GRAPHITE),
            (format!("@{}", l.who), theme::GRAPHITE),
            (words, theme::PAPER),
        ] {
            let galley =
                painter.layout(text, font.clone(), color, (rect.right() - x - 8.0).max(0.0));
            let w = galley.size().x;
            painter.galley(egui::pos2(x, head_y - galley.size().y / 2.0), galley, color);
            x += w + 16.0;
        }
        if let Some((at, len, start)) = self.player_state(r, s, d.from) {
            // The player row as it sits under the line, without its controls working.
            let gap = ui.spacing().item_spacing;
            let y = rect.top() + head_h + gap.y + 12.0;
            let mut x = rect.left() + 12.0 + gap.x;
            let icon =
                egui::Rect::from_center_size(egui::pos2(x + 12.0, y), egui::vec2(10.0, 10.0));
            play_icon(&painter, icon, egui::Stroke::new(1.5, theme::PAPER));
            x += 24.0 + gap.x;
            let w = ui.visuals().widgets.inactive;
            let track = egui::Rect::from_min_size(egui::pos2(x, y - 3.0), egui::vec2(180.0, 6.0));
            painter.rect_filled(track, 3.0, ui.visuals().extreme_bg_color);
            let handle = egui::vec2(16.0, 20.0);
            let k = track.left()
                + handle.x / 2.0
                + (track.width() - handle.x) * (at / len.max(0.001)) as f32;
            painter.rect(
                egui::Rect::from_center_size(egui::pos2(k, y), handle),
                4.0,
                w.bg_fill,
                w.fg_stroke,
                egui::StrokeKind::Inside,
            );
            x += 180.0 + gap.x;
            let small = egui::TextStyle::Small.resolve(ui.style());
            let mut texts = vec![(format!("{at:.1} / {len:.1} s"), theme::GRAPHITE)];
            if let Some(st) = start {
                texts.push((format!("{:.1} s in the shot", st + at), theme::DUST));
            }
            for (text, color) in texts {
                let g = painter.layout_no_wrap(text, small.clone(), color);
                let w = g.size().x;
                painter.galley(egui::pos2(x, y - g.size().y / 2.0), g, color);
                x += w + gap.x;
            }
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }

    /// How long a recording is; None if ffprobe can't tell.
    fn length(&mut self, path: &Path) -> Option<f64> {
        if let Some(l) = self.lengths.get(path) {
            return *l;
        }
        let l = self.ff.as_ref()?.audio_duration(path).ok();
        self.lengths.insert(path.to_path_buf(), l);
        l
    }

    /// Where each line starts in the shot, as far as it's known: its own start, else after the
    /// line before once that one's recorded.
    fn line_starts(&mut self, s: &Shot) -> Vec<Option<f64>> {
        let mut out = vec![];
        let mut end: Option<f64> = Some(-GAP);
        for (i, line) in s.lines.iter().enumerate() {
            let start = line.at.or(end.map(|e| (e + GAP).max(0.0)));
            let len = self.picked(s, i).and_then(|j| self.length(&file(self, &j)));
            end = start.zip(len).map(|(s, l)| s + l);
            out.push(start);
        }
        out
    }

    /// Where the last line to finish ends, once every line has a length.
    fn scene_length(&mut self, s: &Shot) -> Option<f64> {
        let starts = self.line_starts(s);
        let mut end = 0.0f64;
        for (i, start) in starts.into_iter().enumerate() {
            let j = self.picked(s, i)?;
            end = end.max(start? + self.length(&file(self, &j))?);
        }
        Some(end)
    }

    /// Under a line with a recording picked: play and pause, where it's at, and where that is
    /// in the shot, to set the next line's start by.
    fn line_player(&mut self, ui: &mut egui::Ui, r: ShotRef, s: &Shot, i: usize) {
        let Some(j) = self.picked(s, i) else {
            return;
        };
        let path = file(self, &j);
        let what = format!("{r} line {}", i + 1);
        let playing = self.hearing(ui.ctx(), &what);
        let Some((at, len, start)) = self.player_state(r, s, i) else {
            return;
        };
        let mut click = false;
        let mut to = at;
        let mut moved = None;
        ui.horizontal(|ui| {
            ui.add_space(12.0 + ui.spacing().item_spacing.x);
            let (icon, label): (fn(&egui::Painter, egui::Rect, egui::Stroke), _) = if playing {
                (pause_icon, "Pause")
            } else {
                (play_icon, "Play")
            };
            click = icon_button(ui, &format!("{label} line {}", i + 1), icon).clicked();
            ui.spacing_mut().slider_width = 180.0;
            let bar = ui.add(egui::Slider::new(&mut to, 0.0..=len).show_value(false));
            bar.widget_info(|| {
                egui::WidgetInfo::slider(true, to, format!("Where in line {}", i + 1))
            });
            // Played on from where it's let go, if it was playing when taken.
            let was = bar.id.with("was playing");
            if bar.drag_started() {
                ui.data_mut(|d| d.insert_temp(was, playing));
            }
            if bar.changed() && !bar.dragged() {
                moved = Some(playing);
            } else if bar.drag_stopped() {
                moved = Some(ui.data(|d| d.get_temp(was)).unwrap_or(false));
            } else if bar.dragged() {
                moved = Some(false);
            }
            ui.label(
                RichText::new(format!("{at:.1} / {len:.1} s"))
                    .small()
                    .color(theme::GRAPHITE),
            );
            if let Some(st) = start {
                ui.label(
                    RichText::new(format!("{:.1} s in the shot", st + at))
                        .small()
                        .color(theme::DUST),
                )
                .on_hover_text(format!("this line starts at {st:.1} s"));
            }
        });
        if let Some(play_on) = moved {
            if play_on {
                self.hear_from(&what, &path, to.min(len - 0.05).max(0.0));
            } else {
                self.hush();
                self.paused = Some((what, to));
            }
            return;
        }
        if !click {
            return;
        }
        if playing {
            self.hush();
            self.paused = Some((what, at));
        } else {
            let from = if at >= len - 0.05 { 0.0 } else { at };
            self.hear_from(&what, &path, from);
        }
    }

    /// Where line `i`'s player is, how long its recording is, and where the line starts in the
    /// shot if that's known. None without a recording of known length.
    fn player_state(&mut self, r: ShotRef, s: &Shot, i: usize) -> Option<(f64, f64, Option<f64>)> {
        let j = self.picked(s, i)?;
        let len = self.length(&file(self, &j))?;
        let what = format!("{r} line {}", i + 1);
        let at = match (&self.hearing, &self.paused) {
            (Some(h), _) if h.what == what => h.from + h.started.elapsed().as_secs_f64(),
            (_, Some((w, t))) if *w == what => *t,
            _ => 0.0,
        }
        .min(len);
        let start = self.line_starts(s).get(i).copied().flatten();
        Some((at, len, start))
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

    /// One line, always the same shape: start, speaker, its words (editable, marked out of date
    /// when they're not what was recorded), whether it's recorded, Record…, and ⋯.
    fn line_row(
        &mut self,
        ui: &mut egui::Ui,
        r: ShotRef,
        s: &Shot,
        i: usize,
        cast: &[String],
        acts: &mut Vec<Action>,
    ) {
        starts(ui, &mut self.form.lines[i]);
        who(ui, i, &mut self.form.lines[i], cast);
        let picked = self.picked(s, i);
        let who = s.lines[i].who.clone();
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let more = kit::icon_button(ui, icon::MORE, &format!("More for line {}", i + 1))
                .on_hover_text("more");
            egui::Popup::menu(&more).show(|ui| {
                if ui.button(format!("Recordings by @{who}…")).clicked() {
                    self.recordings_for = Some((r, i));
                }
                if ui
                    .button("Recording from a file…")
                    .on_hover_text("one made somewhere else, of these words")
                    .clicked()
                    && let Some(f) = voice_file()
                {
                    acts.push(Action::ImportRecording(r, i, f));
                }
                if picked.is_some()
                    && ui
                        .button("None: record with each take")
                        .on_hover_text("each take records the line itself")
                        .clicked()
                {
                    acts.push(Action::PickRecording(r, i, None));
                }
                ui.separator();
                if kit::danger_button(ui, "Delete line").clicked() {
                    acts.push(Action::RemoveLine(r, i));
                }
            });
            let has = !self.form.lines[i].text.trim().is_empty();
            // A slot as wide as Record again…, so every row's words line up.
            let h = ui.spacing().interact_size.y;
            ui.allocate_ui_with_layout(
                egui::vec2(RECORD, h),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.set_min_width(RECORD);
                    self.record_button(ui, r, i, (&who, has), picked.is_some());
                },
            );
            match &picked {
                Some(j) => {
                    let then = j.prompt.clone().unwrap_or_default();
                    if self.form.lines[i].text.trim() != then.trim() {
                        kit::chip(ui, "out of date", theme::WARN, CHIP).on_hover_text(format!(
                            "recorded as \"{then}\"; record it again to match"
                        ));
                    } else {
                        kit::chip(ui, "recorded", theme::GO, CHIP)
                            .on_hover_text("takes send this recording");
                    }
                }
                None => {
                    kit::chip(ui, "not recorded", theme::DUST, CHIP)
                        .on_hover_text("each take records it; or record it now to hear it first");
                }
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.form.lines[i].text)
                    .hint_text(RichText::new("What they say").color(theme::DUST))
                    .desired_width(ui.available_width()),
            )
            .widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::TextInput, true, format!("Line {}", i + 1))
            });
        });
    }

    /// The line being added: saved once it's left with words in it, or once its recording is
    /// paid for, or with a recording its speaker made before.
    fn draft_row(
        &mut self,
        ui: &mut egui::Ui,
        r: ShotRef,
        cast: &[String],
        title: egui::Id,
        acts: &mut Vec<Action>,
    ) {
        ui.horizontal(|ui| {
            ui.add_space(12.0 + ui.spacing().item_spacing.x);
            starts(ui, &mut self.form.new);
            who(ui, DRAFT, &mut self.form.new, cast);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, "Don't add it", crate::close_icon).clicked() {
                    self.form.adding = false;
                    self.form.new.text.clear();
                    self.form.new.at.clear();
                    if self.say.lock().unwrap().line() == Some((r, DRAFT)) {
                        *self.say.lock().unwrap() = Say::Idle;
                    }
                    return;
                }
                let who = self.form.new.who.clone();
                let has = !self.form.new.text.trim().is_empty();
                self.record_button(ui, r, DRAFT, (&who, has), false);
                if ui
                    .button("Pick one…")
                    .on_hover_text(format!(
                        "one @{who} recorded before, in this shot or another"
                    ))
                    .clicked()
                {
                    self.recordings_for = Some((r, DRAFT));
                }
                if ui
                    .add_enabled(has, egui::Button::new("From a file…"))
                    .on_hover_text("a recording made somewhere else, of these words")
                    .on_disabled_hover_text("type what they say first")
                    .clicked()
                    && let Some(file) = voice_file()
                {
                    self.flush();
                    self.apply(vec![Action::ImportLine(r, self.form.new.clone(), file)]);
                    return;
                }
                let text = ui
                    .add(
                        egui::TextEdit::singleline(&mut self.form.new.text)
                            .hint_text(RichText::new("What they say").color(theme::DUST))
                            .desired_width(ui.available_width()),
                    )
                    .labelled_by(title);
                let waiting = self.say.lock().unwrap().line() == Some((r, DRAFT));
                if text.lost_focus() && has && !waiting {
                    acts.push(Action::AddLine(r, self.form.new.clone()));
                }
            });
        });
    }

    /// Record… (Record again… once it has one), then the wait. Its price goes on a row of its
    /// own under the line, and so does an error.
    fn record_button(
        &mut self,
        ui: &mut egui::Ui,
        r: ShotRef,
        i: usize,
        (who, has_words): (&str, bool),
        again: bool,
    ) {
        let say = self.say.lock().unwrap().clone();
        let mine = say.line() == Some((r, i));
        let shown = match &say {
            Say::Priced(..) if mine => return,
            Say::Pricing(..) if mine => Price::Asking,
            Say::Recording(..) if mine => Price::Working("recording…"),
            _ => Price::Idle,
        };
        let plugin = self.voice_plugin(who);
        // One line at a time, so a price waiting on another line isn't lost.
        let blocked = if let Err(why) = &plugin {
            Some(why.clone())
        } else if self.form.recipe.is_none() {
            Some("the shot's lines aren't spoken; pick how under Lines spoken".to_string())
        } else if !has_words {
            Some("type what they say first".to_string())
        } else if let Some((sr, si)) = say.line().filter(|_| {
            !mine && matches!(say, Say::Pricing(..) | Say::Priced(..) | Say::Recording(..))
        }) {
            Some(match (sr == r, si) {
                (true, DRAFT) => "the new line is waiting; record or cancel it first".into(),
                (true, si) => format!("line {} is waiting; record or cancel it first", si + 1),
                (false, _) => format!("a line of {sr} is waiting; record or cancel it first"),
            })
        } else {
            None
        };
        let keys = plugin.as_ref().ok().and_then(|p| self.needs_keys(p));
        let verb = if again { "Record again" } else { "Record" };
        match PriceRow::new(verb)
            .keys(keys)
            .blocked(blocked)
            .show(ui, shown)
        {
            Some(Pressed::Keys) => self.wants_keys = plugin.ok(),
            Some(Pressed::Ask) if i == DRAFT => {
                let draft = (self.form.new.who.clone(), self.form.new.text.clone());
                self.ask(ui.ctx(), r, i, None, Some(draft));
            }
            Some(Pressed::Ask) => {
                // What's typed is saved first, so that's what gets recorded.
                self.flush();
                self.ask(ui.ctx(), r, i, None, None);
            }
            Some(Pressed::Cancel) => *self.say.lock().unwrap() = Say::Idle,
            _ => {}
        }
    }

    /// Under a line: why it can't be recorded when that needs doing elsewhere, and what went
    /// wrong recording it, in full, until it's dismissed.
    fn line_notes(&mut self, ui: &mut egui::Ui, r: ShotRef, i: usize, who: &str) {
        let indent = 12.0 + ui.spacing().item_spacing.x;
        if let Err(why) = self.voice_plugin(who)
            && self.film.elements.iter().any(|(e, _)| e.handle == who)
        {
            let link = if why.contains("no model") {
                format!("Set @{who}'s voice…")
            } else {
                format!("Give @{who} a voice…")
            };
            ui.horizontal(|ui| {
                ui.add_space(indent);
                hint(ui, why.split(';').next().unwrap_or(&why));
                if ui.link(link).clicked() {
                    self.flush();
                    self.element = Some(who.to_string());
                    self.tab = crate::Tab::Elements;
                }
            });
        }
        let failed = match &*self.say.lock().unwrap() {
            Say::Failed(fr, fi, e) if (*fr, *fi) == (r, i) => Some(e.clone()),
            _ => None,
        };
        if let Some(e) = failed {
            ui.horizontal(|ui| {
                ui.add_space(indent);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if icon_button(ui, "Dismiss", crate::close_icon).clicked() {
                        *self.say.lock().unwrap() = Say::Idle;
                    }
                    ui.add(egui::Label::new(RichText::new(e).color(theme::FLAG)).wrap());
                });
            });
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

    /// The popup of what a line's speaker has recorded, from ⋯ (or Pick one… on the line being
    /// added): this shot's first, the rest behind Show all, grouped by chapter and shot.
    fn recordings_popup(
        &mut self,
        ui: &mut egui::Ui,
        r: ShotRef,
        s: &Shot,
        acts: &mut Vec<Action>,
    ) {
        let Some((pr, i)) = self.recordings_for else {
            return;
        };
        if pr != r || (i != DRAFT && i >= s.lines.len()) {
            self.recordings_for = None;
            return;
        }
        let who = match i {
            DRAFT => self.form.new.who.clone(),
            _ => s.lines[i].who.clone(),
        };
        let picked = if i == DRAFT { None } else { self.picked(s, i) };
        let all = self.voices_of(&who, r);
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
        let wide = kit::modal_width(&ctx, 0.5, 420.0, 640.0);
        let mut pick = None;
        let mut close = false;
        let modal = crate::modal(&ctx, "recordings", |ui| {
            ui.set_width(wide);
            ui.horizontal(|ui| {
                ui.heading(format!("Recordings by @{who}"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = crate::icon_button(ui, "Close", crate::close_icon).clicked();
                });
            });
            ui.label(
                RichText::new(match i {
                    DRAFT => "Pick one for the new line; it's added with its words.".to_string(),
                    _ => format!("Pick the one line {} sends.", i + 1),
                })
                .color(theme::DUST),
            );
            ui.add_space(8.0);
            if all.is_empty() {
                kit::empty(ui, icon::MIC, &format!("@{who} has nothing recorded yet."));
                return;
            }
            pick = recording_list(
                ui,
                (r, i),
                &all,
                picked.map(|j| j.id.to_string()),
                &playing,
                show_all,
                &names,
                &uses,
            );
        });
        if close || modal.should_close() || matches!(pick, Some(Pick::Use(_) | Pick::Open(_))) {
            self.recordings_for = None;
            self.show_all = false;
        }
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
        let total = match missing {
            Some(_) => None,
            None => self.scene_length(s),
        };
        let b = ui
            .horizontal(|ui| {
                let icon: fn(&egui::Painter, egui::Rect, egui::Stroke) =
                    if playing { stop_icon } else { play_icon };
                let b = ui
                    .add_enabled_ui(missing.is_none(), |ui| icon_button(ui, label, icon))
                    .inner
                    .on_hover_text(if playing {
                        "stop"
                    } else {
                        "play the scene: every line's recording at its start time"
                    })
                    .on_disabled_hover_text(match missing {
                        Some(i) => format!("line {} isn't recorded yet", i + 1),
                        None => String::new(),
                    });
                if let Some(t) = total {
                    let over = s.duration.filter(|d| t > *d + 0.05);
                    let color = if over.is_some() {
                        theme::FLAG
                    } else {
                        theme::GRAPHITE
                    };
                    let l = ui.label(RichText::new(format!("{t:.1} s")).color(color));
                    match over {
                        Some(d) => l.on_hover_text(format!("longer than the shot's {d:.1} s")),
                        None => l.on_hover_text("how long all the lines are together"),
                    };
                }
                b
            })
            .inner;
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

/// The dropdown's insides: the recordings grouped by chapter and shot.
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
) -> Option<Pick> {
    let mut pick = None;
    let width = ui.available_width().max(400.0);
    ui.set_width(width);
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
                    // The bin stays put; where it's used is said beside it.
                    let bin = ui
                        .add_enabled_ui(elsewhere.is_empty(), |ui| {
                            crate::delete_button(ui, &format!("Delete {words}"))
                        })
                        .inner;
                    if bin
                        .on_hover_text("delete this recording")
                        .on_disabled_hover_text("another line sends it, so it can't be deleted")
                        .clicked()
                    {
                        pick = Some(Pick::Delete(id.clone()));
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
                    if let Some((r, l)) = elsewhere.iter().find(|(r, _)| *r != here.0) {
                        let go = ui
                            .link(RichText::new(format!("line {} of {r} →", l + 1)).small())
                            .on_hover_text("it sends this too; go there");
                        go.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::Link, true, format!("Go to {r}"))
                        });
                        if go.clicked() {
                            pick = Some(Pick::Open(*r));
                        }
                    } else if let Some((_, l)) = elsewhere.first() {
                        ui.label(
                            RichText::new(format!("line {} too", l + 1))
                                .small()
                                .color(theme::DUST),
                        );
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
