//! The Bridge window on the Cut page: a short clip made between two clips of the editor's
//! sequence, from the last frame of one to the first of the next, put in at the join.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use shotline_core::protocol::{Change, ClipInfo, EditParams, ModelInfo, Op, SequenceInfo};
use shotline_core::{Bridge, Job, JobStatus, Point, Project, ShotRef, Take, Ulid, looks_english};
use shotline_media::Ffmpeg;

use crate::cut_tools::Undo;
use crate::player::Deck;
use crate::price::{Pressed, Price, PriceRow};
use crate::{App, short, theme};

type Slot<T> = Arc<Mutex<Option<T>>>;

/// The points either side of the join and their frames' files.
type Framed = (Point, Point, String, String);

pub(crate) struct BridgeWin {
    /// None until the price is first asked: only then is the bridge written to the film.
    id: Option<Ulid>,
    form: Bridge,
    /// As last saved.
    saved: Bridge,
    edited: f64,
    price: Option<(Bridge, Result<String, String>)>,
    pricing: Option<(Bridge, Slot<Result<String, String>>)>,
    /// Put the prompt into English first, for this bridge.
    translate: bool,
    framing: Option<Slot<Result<Framed, String>>>,
    takes: Vec<(usize, Job)>,
    /// A take open in its own popup: the job, its number, its player.
    deck: Option<(Ulid, usize, Deck)>,
    error: Option<String>,
    /// The shots of the ends' files, looked up once per pair of files.
    shots: Option<(String, String, [Option<ShotRef>; 2])>,
}

/// A take of a bridge being made. It goes on with the window closed.
pub(crate) struct BridgeJob {
    slot: Slot<Result<PathBuf, String>>,
    stop: Arc<AtomicBool>,
}

/// Where the bridge is in the sequence now.
enum Place {
    /// Not yet in: the two clips meet.
    Join(ClipInfo, ClipInfo),
    /// In between them, as a take of it: before, the take's clip, after.
    In(ClipInfo, ClipInfo, ClipInfo, Ulid),
    Gone,
}

fn same(project: &Project, p: &Point, file: Option<&PathBuf>) -> bool {
    let Some(f) = file else { return false };
    let mine = project.point_file(p);
    mine == *f
        || project.rel(f) == p.file
        || std::fs::canonicalize(&mine).ok() == std::fs::canonicalize(f).ok()
}

fn meet(a: &ClipInfo, b: &ClipInfo, fps: f64) -> bool {
    (a.start + a.length - b.start).abs() < 0.5 / fps.max(1.0)
}

/// Inside the last frame shown of `a`, and the first of `b`, seconds into their files.
pub(crate) fn points(project: &Project, a: &ClipInfo, b: &ClipInfo, fps: f64) -> (Point, Point) {
    let half = 0.5 / fps.max(1.0);
    let point = |c: &ClipInfo, at: f64| Point {
        file: c.file.as_ref().map(|f| project.rel(f)).unwrap_or_default(),
        at,
    };
    (
        point(a, (a.from + a.length * a.speed - half).max(0.0)),
        point(b, b.from + half),
    )
}

/// Bridge takes in the sequences, by file: the bridge, the take, its number.
pub(crate) type BridgeTakes = std::collections::HashMap<PathBuf, (Ulid, Ulid, usize)>;

/// Takes in the editor's sequences, by file.
type InCut = HashMap<PathBuf, Take>;

/// Where `b` is in `seq`: its ends' own files first, else other takes of the same shots, for
/// when a shot's take was changed in the cut after the bridge was made.
fn place(
    project: &Project,
    takes: &BridgeTakes,
    id: Ulid,
    b: &Bridge,
    seq: &SequenceInfo,
    shots: Option<(&InCut, [Option<ShotRef>; 2])>,
) -> Place {
    let exact = place_by(takes, id, seq, &|end, c| {
        same(
            project,
            if end == 0 { &b.from } else { &b.to },
            c.file.as_ref(),
        )
    });
    match (exact, shots) {
        (Place::Gone, Some((in_cut, shots))) => place_by(takes, id, seq, &|end, c| {
            same(
                project,
                if end == 0 { &b.from } else { &b.to },
                c.file.as_ref(),
            ) || shots[end].is_some()
                && c.file.as_ref().and_then(|f| in_cut.get(f)).map(|t| t.shot) == shots[end]
        }),
        (p, _) => p,
    }
}

fn place_by(
    takes: &BridgeTakes,
    id: Ulid,
    seq: &SequenceInfo,
    is: &dyn Fn(usize, &ClipInfo) -> bool,
) -> Place {
    for t in &seq.video {
        let mut clips: Vec<&ClipInfo> = t.clips.iter().filter(|c| !c.id.is_empty()).collect();
        clips.sort_by(|x, y| x.start.total_cmp(&y.start));
        for w in clips.windows(2) {
            if is(0, w[0]) && is(1, w[1]) && meet(w[0], w[1], seq.fps) {
                return Place::Join(w[0].clone(), w[1].clone());
            }
        }
        for w in clips.windows(3) {
            let take = w[1]
                .file
                .as_ref()
                .and_then(|f| takes.get(f))
                .filter(|(of, _, _)| *of == id);
            if let Some((_, take, _)) = take
                && is(0, w[0])
                && is(1, w[2])
            {
                return Place::In(w[0].clone(), w[1].clone(), w[2].clone(), *take);
            }
        }
    }
    Place::Gone
}

/// What made a take and what it cost.
fn about(job: &Job) -> String {
    let mut out = format!("{} {}", job.plugin, job.model);
    if let Some(c) = &job.cost {
        out += &format!("   {} {}", shotline_core::ledger::money(c.amount), c.unit);
    }
    out
}

/// Models that end on a given frame, by plugin.
fn models(plugins: &[shotline_core::protocol::Description]) -> Vec<(String, ModelInfo)> {
    plugins
        .iter()
        .flat_map(|d| {
            d.models
                .iter()
                .filter(|m| m.op == Op::VideoGenerate && m.end_frame)
                .map(|m| (d.name.clone(), m.clone()))
        })
        .collect()
}

/// The smallest of the model's resolutions that covers the sequence's short side.
fn resolution_for(m: &ModelInfo, seq: &SequenceInfo) -> Option<String> {
    let short_side = seq.width.min(seq.height);
    let height = |r: &str| r.trim_end_matches(['P', 'p']).parse::<u32>().ok();
    m.resolutions
        .iter()
        .filter(|r| height(r).is_some_and(|h| h >= short_side))
        .min_by_key(|r| height(r))
        .or(m.resolutions.last())
        .cloned()
}

/// Where a bridge's frames go: its own folder, or the cache while it isn't saved yet.
fn frames_dir(project: &Project, id: Option<Ulid>) -> PathBuf {
    match id {
        Some(id) => project.bridge_dir(id),
        None => project.root.join(".shotline/cache/bridge-frames"),
    }
}

fn take_frames(
    ff: &Ffmpeg,
    project: &Project,
    id: Option<Ulid>,
    (from, to): (Point, Point),
) -> Result<Framed, String> {
    let ms = jiff::Timestamp::now().as_millisecond();
    let dir = frames_dir(project, id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let start = dir.join(format!("start-{ms}.png"));
    let end = dir.join(format!("end-{ms}.png"));
    ff.frame_at(&project.point_file(&from), from.at, &start)
        .and_then(|()| ff.frame_at(&project.point_file(&to), to.at, &end))
        .map_err(|e| e.to_string())?;
    Ok((from, to, project.rel(&start), project.rel(&end)))
}

impl BridgeWin {
    fn new(project: &Project, id: Option<Ulid>, b: Bridge) -> BridgeWin {
        BridgeWin {
            id,
            takes: id
                .map(|id| project.bridge_takes(id).unwrap_or_default())
                .unwrap_or_default(),
            form: b.clone(),
            saved: b,
            edited: 0.0,
            price: None,
            pricing: None,
            translate: false,
            framing: None,
            deck: None,
            error: None,
            shots: None,
        }
    }
}

impl App {
    /// The bridge across the join from `a` to `b`, made now if there's none.
    pub(crate) fn open_bridge(
        &mut self,
        ctx: &egui::Context,
        seq: &SequenceInfo,
        a: &ClipInfo,
        b: &ClipInfo,
    ) {
        let (from, to) = points(&self.project, a, b, seq.fps);
        let found = self
            .project
            .bridges()
            .unwrap_or_default()
            .into_iter()
            .find(|(_, x)| {
                x.sequence == seq.name
                    && same(&self.project, &x.from, a.file.as_ref())
                    && same(&self.project, &x.to, b.file.as_ref())
            });
        if let Some((id, x)) = found {
            self.bridge_win = Some(BridgeWin::new(&self.project, Some(id), x));
            return;
        }
        let all = models(&self.plugins);
        let pick = all
            .iter()
            .find(|(p, m)| *p == self.project.config.plugin && m.id == self.project.config.model)
            .or_else(|| all.iter().find(|(p, _)| *p == self.project.config.plugin))
            .or(all.first());
        let Some((plugin, model)) = pick.cloned() else {
            self.error("no installed model takes an end frame; h3 on fal or Venice does".into());
            return;
        };
        let length = 2.0_f64.clamp(
            model.min_duration.unwrap_or(0.0),
            model.max_duration.unwrap_or(f64::MAX),
        );
        let bridge = Bridge {
            sequence: seq.name.clone(),
            from: from.clone(),
            to: to.clone(),
            length,
            plugin,
            resolution: resolution_for(&model, seq),
            model: model.id.clone(),
            ..Bridge::default()
        };
        let mut w = BridgeWin::new(&self.project, None, bridge);
        self.frames(ctx, &mut w, (from, to));
        self.bridge_win = Some(w);
    }

    /// The bridge a take in the sequence belongs to.
    pub(crate) fn open_bridge_of(&mut self, id: Ulid) {
        match self.project.bridge(id) {
            Ok(b) => self.bridge_win = Some(BridgeWin::new(&self.project, Some(id), b)),
            Err(e) => self.error(e.to_string()),
        }
    }

    fn frames(&self, ctx: &egui::Context, w: &mut BridgeWin, at: (Point, Point)) {
        let Some(ff) = self.ff.clone() else {
            w.error = Some("taking the frames needs ffmpeg".into());
            return;
        };
        let slot: Slot<_> = Arc::default();
        let (s, project, id, ctx) = (slot.clone(), self.project.clone(), w.id, ctx.clone());
        std::thread::spawn(move || {
            *s.lock().unwrap() = Some(take_frames(&ff, &project, id, at));
            ctx.request_repaint();
        });
        w.framing = Some(slot);
    }

    /// The window, while one is open.
    pub(crate) fn bridge_window(
        &mut self,
        ui: &mut egui::Ui,
        plugin: &str,
        file: &Path,
        seq: &SequenceInfo,
    ) {
        let Some(mut w) = self.bridge_win.take() else {
            return;
        };
        let ctx = ui.ctx().clone();
        let now = ctx.input(|i| i.time);
        self.bridge_news(&ctx, &mut w);
        let shots = match &w.shots {
            Some((f, t, s)) if *f == w.form.from.file && *t == w.form.to.file => *s,
            _ => {
                let shot = |p: &Point| self.project.take_of_file(&self.project.point_file(p));
                let s = [shot(&w.form.from), shot(&w.form.to)].map(|t| t.map(|t| t.shot));
                w.shots = Some((w.form.from.file.clone(), w.form.to.file.clone(), s));
                s
            }
        };
        let place = place(
            &self.project,
            &self.edit_bridges,
            w.id.unwrap_or_default(),
            &w.form,
            seq,
            Some((&self.edit_takes, shots)),
        );
        let all = models(&self.plugins);
        let model = all
            .iter()
            .find(|(p, m)| *p == w.form.plugin && m.id == w.form.model)
            .map(|(_, m)| m.clone());
        let job = w.id.and_then(|id| self.bridge_jobs.get(&id));
        let making = job.is_some();
        let stopping = job.is_some_and(|j| j.stop.load(Ordering::Relaxed));
        let busy = w.framing.is_some();
        let wide = crate::kit::modal_width(&ctx, 0.6, 480.0, 860.0);
        let mut close = false;
        let mut go = None;
        let mut ask = false;
        let mut new_frames = None;
        let mut play = None;
        let mut remove = None;
        let mut use_take = None;
        let name = |c: &ClipInfo| {
            c.file
                .as_ref()
                .and_then(|f| self.edit_takes.get(f))
                .map_or(c.name.clone(), |t| t.to_string())
        };
        let (title, ends, after) = match &place {
            Place::Join(a, b) | Place::In(a, _, b, _) => (
                format!("Bridge {} to {}", name(a), name(b)),
                Some(points(&self.project, a, b, seq.fps)),
                name(b),
            ),
            Place::Gone => ("Bridge".to_string(), None, String::new()),
        };
        let fps = seq.fps.max(1.0);
        let moved = ends.as_ref().is_some_and(|(f, t)| {
            f.file != w.form.from.file
                || t.file != w.form.to.file
                || (f.at - w.form.from.at).abs() > 0.25 / fps
                || (t.at - w.form.to.at).abs() > 0.25 / fps
        });
        let modal = crate::modal(&ctx, "bridge", |ui| {
            ui.set_width(wide);
            ui.horizontal(|ui| {
                ui.heading(&title);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = crate::icon_button(ui, "Close", crate::close_icon)
                        .on_hover_text(if making {
                            "Close; the take is made on, and shows here when it's done"
                        } else {
                            "Close"
                        })
                        .clicked();
                });
            });
            crate::hint(
                ui,
                "A short clip from the last frame before the join to the first one after it, \
                 put in at the join; what follows moves later. Silent: the sound carries on.",
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                for (png, label) in [
                    (&w.form.start, "last frame before"),
                    (&w.form.end, "first frame after"),
                ] {
                    ui.vertical(|ui| {
                        let path = self.project.root.join(png);
                        if !png.is_empty() && path.is_file() {
                            if self.thumbs.show(ui, &path, 120.0, label) {
                                self.zoom = Some(path);
                            }
                        } else {
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(160.0, 120.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().rect_filled(rect, 4.0, theme::RAISED);
                            if w.framing.is_some() {
                                ui.put(rect, egui::Spinner::new());
                            }
                        }
                        ui.weak(label);
                    });
                }
            });
            match &place {
                Place::Gone => {
                    ui.label(
                        RichText::new(format!(
                            "These two clips don't meet in {} any more, so no take can go in.",
                            w.form.sequence
                        ))
                        .color(theme::FLAG),
                    );
                }
                _ if moved => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new("The cut changed here since these frames were taken.")
                                .color(theme::FLAG),
                        );
                        if ui
                            .add_enabled(!busy, egui::Button::new("Use the new frames"))
                            .on_hover_text(
                                "takes the frames where the clips meet now, for the next takes",
                            )
                            .clicked()
                        {
                            new_frames = ends.clone();
                        }
                    });
                }
                _ => {}
            }
            ui.add_space(8.0);
            ui.label("What happens");
            let r = ui.add(
                egui::TextEdit::multiline(&mut w.form.prompt)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("e.g. she turns to the window as the camera pushes in"),
            );
            if r.changed() {
                w.edited = now;
            }
            if !w.form.prompt.trim().is_empty() && !looks_english(&w.form.prompt) {
                let label = match w.form.translated() {
                    Some(_) => "Put into English (done)",
                    None => "Put into English first",
                };
                ui.checkbox(&mut w.translate, label).on_hover_text(
                    "the text model translates it once and it's kept with the bridge",
                );
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = format!("{} · {}", w.form.plugin, w.form.model);
                egui::ComboBox::from_id_salt("bridge-model")
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        for (p, m) in &all {
                            let on = *p == w.form.plugin && m.id == w.form.model;
                            let r = ui.selectable_label(on, format!("{p} · {}", m.id));
                            let r = match &m.price {
                                Some(price) => r.on_hover_text(price),
                                None => r,
                            };
                            if r.clicked() && !on {
                                w.form.plugin = p.clone();
                                w.form.model = m.id.clone();
                                w.form.resolution = resolution_for(m, seq);
                                w.form.length = w.form.length.clamp(
                                    m.min_duration.unwrap_or(0.5),
                                    m.max_duration.unwrap_or(15.0),
                                );
                                if m.whole_seconds {
                                    w.form.length = w.form.length.round();
                                }
                                w.edited = now;
                            }
                        }
                    });
                if let Some(m) = &model {
                    let (lo, hi) = (
                        m.min_duration.unwrap_or(0.5),
                        m.max_duration.unwrap_or(15.0),
                    );
                    let mut drag = egui::DragValue::new(&mut w.form.length)
                        .range(lo..=hi)
                        .suffix(" s")
                        .speed(0.05);
                    drag = if m.whole_seconds {
                        drag.max_decimals(0)
                    } else {
                        drag.max_decimals(2)
                    };
                    if ui.add(drag).on_hover_text(format!("{lo}–{hi} s")).changed() {
                        if m.whole_seconds {
                            w.form.length = w.form.length.round();
                        }
                        w.edited = now;
                    }
                    if !m.resolutions.is_empty() {
                        let shown = w.form.resolution.clone().unwrap_or_default();
                        egui::ComboBox::from_id_salt("bridge-resolution")
                            .selected_text(shown)
                            .show_ui(ui, |ui| {
                                for r in &m.resolutions {
                                    let on = w.form.resolution.as_ref() == Some(r);
                                    if ui.selectable_label(on, r).clicked() && !on {
                                        w.form.resolution = Some(r.clone());
                                        w.edited = now;
                                    }
                                }
                            });
                    }
                }
            });
            ui.add_space(6.0);
            let priced = w
                .price
                .as_ref()
                .filter(|(f, _)| *f == w.form)
                .map(|(_, p)| p.clone());
            let asking = w.pricing.as_ref().is_some_and(|(f, _)| *f == w.form);
            let shown = match (making, &priced) {
                (true, _) if stopping => Price::Working("cancelling…"),
                (true, _) => Price::Working("making the take…"),
                (false, _) if asking => Price::Asking,
                (false, Some(Ok(price))) => Price::Priced(price),
                (false, Some(Err(e))) => Price::Failed(e),
                (false, None) => Price::Idle,
            };
            let blocked = if model.is_none() {
                Some("pick a model first".to_string())
            } else if busy {
                Some("busy with the frames".to_string())
            } else {
                None
            };
            let keys = self.needs_keys(&w.form.plugin);
            let row = PriceRow::new("Make a take");
            let row = if stopping { row } else { row.stoppable() };
            match row
                .keys(keys)
                .blocked(blocked)
                .go_hover("makes another take of the bridge")
                .show(ui, shown)
            {
                Some(Pressed::Ask) => ask = true,
                Some(Pressed::Go) => {
                    if let Some(Ok(price)) = priced {
                        go = Some(price);
                    }
                }
                Some(Pressed::Cancel) => w.price = None,
                Some(Pressed::Keys) => self.wants_keys = Some(w.form.plugin.clone()),
                Some(Pressed::Stop) => {
                    if let Some(j) = w.id.and_then(|id| self.bridge_jobs.get(&id)) {
                        j.stop.store(true, Ordering::Relaxed);
                    }
                }
                None => {}
            }
            if let Some(e) = &w.error {
                ui.label(RichText::new(short(e, 200)).color(theme::FLAG))
                    .on_hover_text(e);
            }
            ui.add_space(10.0);
            if !w.takes.is_empty() {
                crate::heading(ui, "Takes");
                ui.add_space(4.0);
            }
            let in_cut = match &place {
                Place::In(_, _, _, t) => Some(*t),
                _ => None,
            };
            let above = ui.cursor().top() - ui.min_rect().top();
            let list_room = ctx.content_rect().height() * 0.9 - above - 40.0;
            egui::ScrollArea::vertical()
                .id_salt("bridge takes")
                // All the height the window has left, so a good few takes show before it scrolls.
                // The modal's own room is only what it took last time, so it's asked for outright.
                .max_height(list_room.max(160.0))
                .min_scrolled_height(list_room.max(160.0))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (n, job) in &w.takes {
                        let n = *n;
                        let on = in_cut == Some(job.id);
                        let video = self.project.bridge_video(job);
                        let row = egui::Frame::new()
                            .inner_margin(egui::Margin::symmetric(10, 8))
                            .corner_radius(theme::R_ROW)
                            .fill(if on {
                                theme::TUNGSTEN.gamma_multiply(0.12)
                            } else {
                                egui::Color32::TRANSPARENT
                            })
                            .stroke(if on {
                                egui::Stroke::new(1.0, theme::TUNGSTEN)
                            } else {
                                egui::Stroke::NONE
                            })
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    match &video {
                                        Some(v) => {
                                            let png = self
                                                .project
                                                .root
                                                .join(".shotline/cache/posters")
                                                .join(format!("{}.png", job.id));
                                            let name = format!("bridge #{n} poster");
                                            let make = (self.ff.as_ref(), &self.posters);
                                            if crate::poster(ui, &mut self.thumbs, make, v, &png, &name)
                                                .clicked()
                                            {
                                                play = Some((job.id, n, v.clone()));
                                            }
                                        }
                                        None => {
                                            let (rect, _) = ui.allocate_exact_size(
                                                egui::vec2(96.0, 54.0),
                                                egui::Sense::hover(),
                                            );
                                            ui.painter().rect_stroke(
                                                rect,
                                                2.0,
                                                egui::Stroke::new(1.0, theme::LINE),
                                                egui::StrokeKind::Inside,
                                            );
                                        }
                                    }
                                    ui.add_space(6.0);
                                    ui.vertical(|ui| {
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                RichText::new(format!("#{n}"))
                                                    .family(egui::FontFamily::Name(
                                                        theme::SEMIBOLD.into(),
                                                    ))
                                                    .color(if on {
                                                        theme::TUNGSTEN
                                                    } else {
                                                        theme::PAPER
                                                    }),
                                            );
                                            if on {
                                                ui.label(
                                                    RichText::new("in the cut")
                                                        .color(theme::TUNGSTEN),
                                                );
                                            } else if job.status != JobStatus::Done {
                                                let color = match job.status {
                                                    JobStatus::Failed => theme::FLAG,
                                                    _ => theme::DUST,
                                                };
                                                ui.label(
                                                    RichText::new(job.status.to_string())
                                                        .color(color),
                                                );
                                            }
                                        });
                                        ui.label(
                                            RichText::new(about(job)).small().color(theme::DUST),
                                        );
                                        if video.is_none()
                                            && let Some(why) = &job.error
                                        {
                                            ui.label(
                                                RichText::new(short(why, 90))
                                                    .small()
                                                    .color(theme::FLAG),
                                            )
                                            .on_hover_text(why);
                                        }
                                    });
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if job.status.is_finished() {
                                                let rm = ui
                                                    .add_enabled_ui(!on, crate::remove_button)
                                                    .inner;
                                                rm.widget_info(|| {
                                                    egui::WidgetInfo::labeled(
                                                        egui::Role::Button,
                                                        !on,
                                                        format!("Remove #{n}"),
                                                    )
                                                });
                                                if rm
                                                    .on_hover_text(
                                                        "off the list; what it cost stays in the \
                                                         ledger",
                                                    )
                                                    .on_disabled_hover_text(
                                                        "it's in the cut; put another take in first",
                                                    )
                                                    .clicked()
                                                {
                                                    remove = Some(job.id);
                                                }
                                            }
                                            if let Some(v) = &video
                                                && !on
                                            {
                                                let r = ui.add_enabled(
                                                    !busy
                                                        && self.changing.is_none()
                                                        && !matches!(place, Place::Gone),
                                                    egui::Button::new("Use in the cut"),
                                                );
                                                r.widget_info(|| {
                                                    egui::WidgetInfo::labeled(
                                                        egui::Role::Button,
                                                        true,
                                                        format!("Use in the cut #{n}"),
                                                    )
                                                });
                                                if r.on_hover_text(match in_cut {
                                                    Some(_) => {
                                                        "in place of the bridge take there now"
                                                    }
                                                    None => {
                                                        "in at the join, moving what follows later"
                                                    }
                                                })
                                                .clicked()
                                                {
                                                    use_take = Some((n, job.id, v.clone()));
                                                }
                                            }
                                        },
                                    );
                                });
                            })
                            .response;
                        if !on && row.hovered() {
                            ui.painter().rect_filled(
                                row.rect,
                                4.0,
                                egui::Color32::from_white_alpha(6),
                            );
                        }
                        ui.add_space(4.0);
                    }
                });
        });

        // Saved first, since the price is of what's saved.
        if ask {
            self.price_bridge(&ctx, &mut w);
        }
        if let Some(at) = new_frames {
            self.frames(&ctx, &mut w, at);
        }
        if let Some(price) = go {
            self.make_bridge(&ctx, &mut w, price);
        }
        if let Some((id, n, video)) = play {
            if let Some((_, _, d)) = &mut w.deck {
                d.pause(self.audio.as_ref());
            }
            let label = format!("bridge #{n}");
            w.deck = self.player(&label, &[video]).map(|p| {
                let tall = ctx.content_rect().height() * 0.55;
                (id, n, Deck::new(&label, p, true).in_popup(tall))
            });
        }
        let popup = w.deck.is_some();
        self.bridge_take_popup(&ctx, &mut w);
        if let Some((take, id)) = remove.zip(w.id) {
            match self.project.remove_bridge_take(id, take) {
                Ok(()) => {
                    if w.deck.as_ref().is_some_and(|(id, _, _)| *id == take)
                        && let Some((_, _, d)) = &mut w.deck
                    {
                        d.pause(self.audio.as_ref());
                        w.deck = None;
                    }
                    w.takes = self.project.bridge_takes(id).unwrap_or_default();
                    if let Ok(b) = self.project.bridge(id) {
                        w.form.used = b.used;
                        w.saved.used = b.used;
                    }
                }
                Err(e) => w.error = Some(e.to_string()),
            }
        }
        if let Some((n, take, video)) = use_take {
            let video = std::path::absolute(&video).unwrap_or(video);
            let (clip, change, what) = match &place {
                Place::Join(_, b) => (
                    b.id.clone(),
                    Change::Insert {
                        file: video,
                        after: false,
                        sound: false,
                    },
                    format!("bridge #{n} in before {after}"),
                ),
                Place::In(_, mid, _, _) => (
                    mid.id.clone(),
                    Change::Replace {
                        length: self
                            .ff
                            .as_ref()
                            .and_then(|ff| ff.probe(&video).ok())
                            .map(|i| i.duration),
                        file: video,
                        from: 0.0,
                    },
                    format!("bridge #{n} in place of the one there"),
                ),
                Place::Gone => unreachable!("disabled"),
            };
            let ask = EditParams {
                film: self.project.root.clone(),
                file: file.to_path_buf(),
                sequence: seq.name.clone(),
                clip,
                change,
            };
            let undo = Undo {
                plugin: plugin.to_string(),
                file: file.to_path_buf(),
                sequence: seq.name.clone(),
                what: what.clone(),
                steps: serde_json::Value::Null,
            };
            self.start_change(&ctx, plugin, what, ask, Some(undo));
            // Marked as used once the editor has it.
            if let (Some(c), Some(id)) = (&mut self.changing, w.id) {
                c.then_use = Some((id, take));
            }
        }
        // Esc closes the take's popup first.
        if (close || modal.should_close()) && !popup {
            if let Some(id) = w.id {
                if w.form != w.saved {
                    let _ = self.project.save_bridge(id, &w.form);
                }
                if w.takes.is_empty() && !making {
                    let _ = self.project.remove_bridge(id);
                }
            }
            self.bridges_seen = None;
            return;
        }
        self.bridge_win = Some(w);
    }

    /// A take of the bridge, bigger, in a popup over the window.
    fn bridge_take_popup(&mut self, ctx: &egui::Context, w: &mut BridgeWin) {
        let Some((id, n, deck)) = &mut w.deck else {
            return;
        };
        let job = w.takes.iter().find(|(_, j)| j.id == *id).map(|(_, j)| j);
        let wide = (ctx.content_rect().width() * 0.6).clamp(360.0, 900.0);
        let mut close = false;
        let modal = crate::modal(ctx, "bridge take", |ui| {
            ui.set_width(wide);
            let bar = crate::title_bar(ui, &format!("bridge #{n}"));
            close |= !deck.show(ui, self.audio.as_ref());
            if let Some(j) = job {
                ui.add_space(6.0);
                ui.label(RichText::new(about(j)).small().color(theme::DUST));
            }
            close |= crate::close_corner(ui, bar);
        });
        if close || modal.should_close() {
            deck.pause(self.audio.as_ref());
            w.deck = None;
        }
    }

    /// Takes of bridges that finished, whether their window is open or not.
    pub(crate) fn bridge_jobs_done(&mut self, ctx: &egui::Context) {
        let done: Vec<(Ulid, Result<PathBuf, String>)> = self
            .bridge_jobs
            .iter()
            .filter_map(|(id, j)| Some((*id, j.slot.lock().unwrap().take()?)))
            .collect();
        for (id, got) in done {
            let stopped = self
                .bridge_jobs
                .remove(&id)
                .is_some_and(|j| j.stop.load(Ordering::Relaxed));
            let takes = self.project.bridge_takes(id).unwrap_or_default();
            if let Some(w) = self.bridge_win.as_mut().filter(|w| w.id == Some(id)) {
                w.takes = takes.clone();
                if let Ok(b) = self.project.bridge(id) {
                    w.form.english = b.english.clone();
                    w.saved = b;
                }
                w.price = None;
                if let Err(e) = &got {
                    w.error = Some(e.clone());
                }
            }
            match got {
                Ok(_) => {
                    if let Some((n, _)) = takes.last() {
                        self.info(format!("bridge #{n} made"));
                    }
                }
                Err(_) if stopped => self.info("bridge take cancelled".into()),
                Err(e) => self.error(format!("bridge take: {}", short(&e, 300))),
            }
            self.bridges_seen = None;
            ctx.request_repaint();
        }
    }

    pub(crate) fn bridge_making(&self, id: Ulid) -> bool {
        self.bridge_jobs.contains_key(&id)
    }

    /// The editor took the bridge's take in: it's the one in the cut now.
    pub(crate) fn bridge_used(&mut self, id: Ulid, take: Ulid) {
        let saved = self.project.bridge(id).and_then(|mut b| {
            b.used = Some(take);
            self.project.save_bridge(id, &b)
        });
        if let Err(e) = saved {
            return self.error(format!("can't mark the bridge take used: {e}"));
        }
        if let Some(w) = self.bridge_win.as_mut().filter(|w| w.id == Some(id)) {
            w.form.used = Some(take);
            w.saved.used = Some(take);
        }
    }

    /// Writes a bridge first opened just now, its frames moved in with it. Its id.
    fn bridge_saved(&self, w: &mut BridgeWin) -> Result<Ulid, String> {
        if let Some(id) = w.id {
            self.save_form(w)?;
            return Ok(id);
        }
        let id = self
            .project
            .new_bridge(&w.form)
            .map_err(|e| format!("can't make a bridge: {e}"))?;
        let dir = self.project.bridge_dir(id);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        for png in [&mut w.form.start, &mut w.form.end] {
            let from = self.project.root.join(&*png);
            if png.is_empty() || !from.is_file() {
                continue;
            }
            let to = dir.join(from.file_name().unwrap_or_default());
            std::fs::rename(&from, &to)
                .or_else(|_| std::fs::copy(&from, &to).map(|_| ()))
                .map_err(|e| e.to_string())?;
            *png = self.project.rel(&to);
        }
        w.id = Some(id);
        self.project
            .save_bridge(id, &w.form)
            .map_err(|e| e.to_string())?;
        w.saved = w.form.clone();
        Ok(id)
    }

    /// Picks up frames and prices that came back.
    fn bridge_news(&mut self, ctx: &egui::Context, w: &mut BridgeWin) {
        if let Some(got) = w.framing.as_ref().and_then(|s| s.lock().unwrap().take()) {
            w.framing = None;
            match got {
                Ok((from, to, start, end)) => {
                    w.form.from = from;
                    w.form.to = to;
                    w.form.start = start;
                    w.form.end = end;
                    w.edited = 0.0;
                    // Not written until the price is asked.
                    if w.id.is_some()
                        && let Err(e) = self.save_form(w)
                    {
                        w.error = Some(e);
                    }
                }
                Err(e) => w.error = Some(format!("can't take the frames: {e}")),
            }
        }
        let priced = w
            .pricing
            .as_ref()
            .and_then(|(form, slot)| Some((form.clone(), slot.lock().unwrap().take()?)));
        if let Some(got) = priced {
            w.price = Some(got);
            w.pricing = None;
        }
        let _ = ctx;
    }

    fn save_form(&self, w: &mut BridgeWin) -> Result<(), String> {
        let Some(id) = w.id else {
            return Ok(());
        };
        if w.form == w.saved {
            return Ok(());
        }
        self.project
            .save_bridge(id, &w.form)
            .map_err(|e| e.to_string())?;
        w.saved = w.form.clone();
        Ok(())
    }

    fn price_bridge(&self, ctx: &egui::Context, w: &mut BridgeWin) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let id = match self.bridge_saved(w) {
            Ok(id) => id,
            Err(e) => {
                w.price = Some((w.form.clone(), Err(e)));
                return;
            }
        };
        let slot: Slot<_> = Arc::default();
        let (s, film, translate, ctx) = (
            slot.clone(),
            self.project.root.clone(),
            w.translate,
            ctx.clone(),
        );
        std::thread::spawn(move || {
            *s.lock().unwrap() = Some(
                host.bridge_quote(&film, id, translate)
                    .map_err(|e| format!("{e:#}")),
            );
            ctx.request_repaint();
        });
        w.pricing = Some((w.form.clone(), slot));
    }

    fn make_bridge(&mut self, ctx: &egui::Context, w: &mut BridgeWin, price: String) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let id = match self.bridge_saved(w) {
            Ok(id) => id,
            Err(e) => return w.error = Some(e),
        };
        w.error = None;
        let slot: Slot<_> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (s, film, translate, ctx, halt) = (
            slot.clone(),
            self.project.root.clone(),
            w.translate,
            ctx.clone(),
            stop.clone(),
        );
        std::thread::spawn(move || {
            *s.lock().unwrap() = Some(
                host.bridge_take(&film, id, &price, translate, halt)
                    .map_err(|e| format!("{e:#}")),
            );
            ctx.request_repaint();
        });
        self.bridge_jobs.insert(id, BridgeJob { slot, stop });
    }

    /// The bridges whose clips meet at a join of `seq`, by the editor's id of the clip after it.
    pub(crate) fn bridged_joins(&mut self, seq: &SequenceInfo) -> Vec<(String, Ulid)> {
        let bridges = self
            .bridges_seen
            .get_or_insert_with(|| self.project.bridges().unwrap_or_default())
            .clone();
        let none = BridgeTakes::new();
        let mut out = vec![];
        for (id, b) in bridges.iter().filter(|(_, b)| b.sequence == seq.name) {
            if let Place::Join(_, after) = place(&self.project, &none, *id, b, seq, None) {
                out.push((after.id, *id));
            }
        }
        out
    }
}
