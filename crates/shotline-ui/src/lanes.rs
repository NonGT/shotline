//! A cut drawn the way editors draw one: a time ruler, video tracks stacked above the audio
//! tracks (the highest V on top, A1 right under V1), each clip a block as long as it lasts, and a
//! playhead. Pinching, the zoom buttons or Shift+Z zoom; scrolling or the bar under it moves
//! along. Past a few tracks the lanes scroll up and down, and an audio lane folds to a strip
//! when its name is clicked. Clicking or dragging along the ruler moves the playhead, snapping to
//! edits; clicking a clip opens its shot, or picks it when it can be changed.

use std::path::PathBuf;

use eframe::egui::{self, Rect, Sense, Stroke, pos2, vec2};
use shotline_core::ShotRef;

use crate::theme;

pub(crate) struct Lane {
    /// V1, A2…
    pub name: String,
    pub blocks: Vec<Block>,
    pub sound: bool,
    /// Shown (video) or heard (audio).
    pub enabled: bool,
    /// Where two of its clips meet, when they can be picked.
    pub joins: Vec<Join>,
}

pub(crate) struct Join {
    pub at: f64,
    /// The editor's id for the clip after it.
    pub id: String,
    /// "01-010#1 to 01-020#1".
    pub name: String,
    pub picked: bool,
    /// Has a bridge made for it, not put in yet.
    pub bridged: bool,
    /// A transition across it: where it starts and how long it lasts.
    pub fade: Option<(f64, f64)>,
    /// The shot after it was made from the last frame of the one before.
    pub continues: bool,
    /// Colour matched to the clip before when rendered.
    pub matched: bool,
}

pub(crate) struct Block {
    pub name: String,
    pub start: f64,
    pub length: f64,
    /// A still to show at its head.
    pub still: Option<PathBuf>,
    pub hover: String,
    pub enabled: bool,
    /// A take's shot, to open on click.
    pub shot: Option<ShotRef>,
    /// The editor's id for it, when it can be picked to change it; a click picks it instead.
    pub id: Option<String>,
    pub picked: bool,
}

pub(crate) enum Hit {
    Seek(f64),
    /// The playhead being dragged: show the frame there, seek once it's let go.
    Scrub(f64),
    Open(ShotRef),
    Pick(String),
    /// A join, by the clip after it, and where it is.
    PickJoin(String, f64),
    /// A click on empty space.
    Deselect,
}

const HEAD: f32 = 36.0;
const RULER: f32 = 22.0;
const VIDEO: f32 = 54.0;
const AUDIO: f32 = 30.0;
/// An audio lane folded away.
const FOLDED: f32 = 12.0;
const GAP: f32 = 2.0;
const BAR: f32 = 8.0;
/// The lanes past this scroll.
const MOST_LANES: f32 = 3.0 * VIDEO + 2.0 * AUDIO + 5.0 * GAP;
/// As far in as zooming goes.
const MOST_PPS: f64 = 1200.0;
/// How near, in points, the playhead jumps to an edit.
const SNAP: f32 = 6.0;
/// Room in the ruler's right end for the zoom buttons.
const ZOOMS: f32 = 92.0;

/// How far a timeline is zoomed in, the second at its left edge and how far down it's scrolled.
#[derive(Clone, Copy, Default)]
struct View {
    zoom: f64,
    start: f64,
    /// Points scrolled down past the first lane.
    top: f32,
    /// Where the playhead was last frame, to keep it in sight while it plays.
    last: Option<f64>,
}

/// Where clips start and end on `lanes`, and their joins, in order: what the playhead snaps and
/// jumps to.
pub(crate) fn edit_points<'a>(lanes: impl IntoIterator<Item = &'a Lane>) -> Vec<f64> {
    let mut at: Vec<f64> = vec![0.0];
    for l in lanes {
        for b in &l.blocks {
            at.extend([b.start, b.start + b.length]);
        }
        at.extend(l.joins.iter().map(|j| j.at));
    }
    at.sort_by(f64::total_cmp);
    at.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    at
}

/// `video` V1 first, `audio` A1 first. `length` is the seconds the ruler spans; `fps` sets the
/// ruler's timecode and its finest ticks.
#[allow(clippy::too_many_arguments)]
pub(crate) fn show(
    ui: &mut egui::Ui,
    id: &str,
    video: &[Lane],
    audio: &[Lane],
    length: f64,
    fps: f64,
    playhead: Option<f64>,
    thumbs: &mut crate::Thumbs,
) -> Option<Hit> {
    let fps = if fps > 0.0 { fps } else { 24.0 };
    let lanes: Vec<&Lane> = video.iter().rev().chain(audio).collect();
    let id = ui.scope_id().with(("lanes", id));
    let folded_id = id.with("folded");
    let mut folded: Vec<String> = ui.data(|d| d.get_temp(folded_id)).unwrap_or_default();
    let lane_h = |l: &Lane| match (l.sound, folded.contains(&l.name)) {
        (true, true) => FOLDED,
        (true, false) => AUDIO,
        (false, _) => VIDEO,
    };
    let full: f32 = lanes.iter().map(|l| lane_h(l) + GAP).sum();
    let shown_h = full.min(MOST_LANES);
    let height = RULER + GAP + shown_h + BAR;
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let span = Rect::from_min_max(pos2(rect.left() + HEAD, rect.top()), rect.right_bottom());
    let area = Rect::from_min_size(
        pos2(rect.left(), rect.top() + RULER + GAP),
        vec2(rect.width(), shown_h),
    );
    let length = length.max(1.0 / fps);
    let view_id = id.with("view");
    let mut view: View = ui.data(|d| d.get_temp(view_id)).unwrap_or_default();
    let width = span.width().max(1.0) as f64;
    let most = (length * MOST_PPS / width).max(1.0);
    view.zoom = view.zoom.clamp(1.0, most);
    let overflow = full > shown_h + 0.5;
    // Zoom in on the second under the fingers; scroll along, or down when the lanes don't fit.
    if ui.rect_contains_pointer(rect) {
        let (pinch, scroll, at) = ui.input(|i| {
            (
                i.zoom_delta() as f64,
                i.smooth_scroll_delta,
                i.pointer.hover_pos(),
            )
        });
        if pinch != 1.0 {
            let x = at.map_or(span.center().x, |a| a.x.clamp(span.left(), span.right()));
            zoom_at(&mut view, pinch, x, span, width, length, most);
        } else {
            let pps = width * view.zoom / length;
            let down = overflow && at.is_some_and(|a| area.contains(a));
            let along = if scroll.x != 0.0 {
                scroll.x
            } else if down {
                view.top -= scroll.y;
                0.0
            } else {
                scroll.y
            };
            if view.zoom > 1.0 {
                view.start -= along as f64 / pps;
            }
        }
    }
    // Zoom buttons at the right of the ruler, and Shift+Z to see it all.
    let zooms = Rect::from_min_max(
        pos2(rect.right() - ZOOMS, rect.top()),
        pos2(rect.right(), rect.top() + RULER),
    );
    ui.painter().rect_filled(
        Rect::from_min_max(span.min, pos2(span.right(), span.top() + RULER)),
        0.0,
        theme::RACK,
    );
    let mut zoom_by = None;
    let mut fit = ui.memory(|m| m.focused().is_none())
        && ui.input(|i| i.modifiers.shift && i.key_pressed(egui::Key::Z));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(zooms)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
        |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.spacing_mut().button_padding = vec2(5.0, 0.0);
            let small = |ui: &mut egui::Ui, text: &str, name: &str, hover: &str| {
                let r = ui
                    .add(egui::Button::new(egui::RichText::new(text).size(12.0)).small())
                    .on_hover_text(hover);
                r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, name));
                r.clicked()
            };
            fit |= small(ui, "Fit", "Fit", "all of it in sight (Shift+Z)");
            if small(ui, "+", "Zoom in", "zoom in") {
                zoom_by = Some(1.6);
            }
            if small(ui, "-", "Zoom out", "zoom out") {
                zoom_by = Some(1.0 / 1.6);
            }
        },
    );
    if fit {
        view.zoom = 1.0;
        view.start = 0.0;
    }
    if let Some(by) = zoom_by {
        // About the playhead when it's in sight, else the middle.
        let shown = length / view.zoom;
        let pps = width / shown;
        let x = playhead
            .filter(|t| *t >= view.start && *t <= view.start + shown)
            .map_or(span.center().x, |t| {
                span.left() + ((t - view.start) * pps) as f32
            });
        zoom_at(&mut view, by, x, span, width, length, most);
    }
    let shown = length / view.zoom;
    let pps = width / shown;
    // The bar under the tracks: what's in sight of the whole, dragged to move along.
    let bar = Rect::from_min_size(
        pos2(span.left(), rect.bottom() - BAR + 2.0),
        vec2(span.width(), BAR - 3.0),
    );
    // Only there when zoomed in.
    let drag = (view.zoom > 1.0).then(|| {
        let drag = ui.interact(bar, id.with("bar"), Sense::click_and_drag());
        drag.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::ScrollBar, true, "Scroll the timeline")
        });
        if drag.dragged() {
            view.start += drag.drag_delta().x as f64 * length / width;
        } else if drag.clicked()
            && let Some(at) = drag.interact_pointer_pos()
        {
            view.start = (at.x - span.left()) as f64 * length / width - shown / 2.0;
        }
        drag
    });
    // The bar up the side when the lanes don't all fit.
    let vbar = Rect::from_min_max(
        pos2(rect.right() - 6.0, area.top()),
        pos2(rect.right() - 1.0, area.bottom()),
    );
    let vdrag = overflow.then(|| {
        let r = ui.interact(vbar, id.with("vbar"), Sense::drag());
        r.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::ScrollBar, true, "Scroll the tracks")
        });
        if r.dragged() {
            view.top += r.drag_delta().y * full / shown_h;
        }
        r
    });
    view.top = view.top.clamp(0.0, (full - shown_h).max(0.0));
    if let Some(t) = playhead {
        let was_in = view
            .last
            .is_some_and(|l| l >= view.start && l <= view.start + shown);
        // Played past the right edge: a page on, as editors do.
        if was_in && t > view.start + shown && view.last.is_some_and(|l| t > l) {
            view.start = t;
        }
        view.last = Some(t);
    }
    view.start = view.start.clamp(0.0, (length - shown).max(0.0));
    ui.data_mut(|d| d.insert_temp(view_id, view));
    let start = view.start;
    let x_of = |t: f64| span.left() + ((t - start) * pps) as f32;
    let t_of = |x: f32| (start + ((x - span.left()) as f64) / pps).clamp(0.0, length);
    let p = ui.painter_at(rect);
    let mut hit = None;
    if let Some(drag) = &drag {
        let whole = |t: f64| bar.left() + (t / length * width) as f32;
        p.rect_filled(bar, 3.0, theme::RACK);
        let thumb = Rect::from_min_max(
            pos2(whole(start), bar.top()),
            pos2(whole(start + shown).max(whole(start) + 6.0), bar.bottom()),
        );
        let lit = drag.hovered() || drag.dragged();
        p.rect_filled(thumb, 3.0, if lit { theme::DUST } else { theme::GRAPHITE });
    }

    // A click on nothing in particular lets go of what's picked; clips are put on top of it.
    let empty = ui.interact(area, id.with("empty"), Sense::click());
    if empty.clicked() {
        hit = Some(Hit::Deselect);
    }

    // Clicked or dragged, the ruler moves the playhead, snapping to an edit near it.
    let ruler = Rect::from_min_max(span.min, pos2(zooms.left() - 4.0, span.top() + RULER));
    let edits = edit_points(lanes.iter().copied());
    let snap = |x: f32| {
        let t = t_of(x);
        if ui.input(|i| i.modifiers.alt) {
            return t;
        }
        edits
            .iter()
            .copied()
            .filter(|e| (x_of(*e) - x).abs() <= SNAP)
            .min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
            .unwrap_or(t)
    };
    let seek = ui.interact(ruler, id.with("seek"), Sense::click_and_drag());
    seek.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Slider, true, "Playhead"));
    if let Some(at) = seek
        .interact_pointer_pos()
        .or_else(|| ui.ctx().pointer_latest_pos())
        .filter(|_| seek.clicked() || seek.dragged() || seek.drag_stopped())
    {
        if seek.clicked() || seek.drag_stopped() {
            hit = Some(Hit::Seek(snap(at.x)));
        } else if seek.dragged() {
            hit = Some(Hit::Scrub(snap(at.x)));
        }
    }
    let (step, every) = tick_step(pps, fps);
    let small = egui::FontId::proportional(11.0);
    let tick_p = p.with_clip_rect(ruler);
    let first = (start / step).floor() as i64;
    let mut n = first;
    loop {
        let t = n as f64 * step;
        if t > (start + shown).min(length) + 1e-9 {
            break;
        }
        let x = x_of(t);
        let major = n % every == 0;
        if x >= span.left() - 0.5 {
            let tall = if major { 6.0 } else { 3.0 };
            tick_p.line_segment(
                [pos2(x, ruler.bottom() - tall), pos2(x, ruler.bottom())],
                Stroke::new(1.0, theme::DUST),
            );
            if major {
                tick_p.text(
                    pos2(x + 3.0, ruler.top() + 3.0),
                    egui::Align2::LEFT_TOP,
                    crate::player::timecode(t, fps),
                    small.clone(),
                    theme::DUST,
                );
            }
        }
        n += 1;
    }

    let mut y = area.top() - view.top;
    let lp = p.with_clip_rect(area);
    let mut fold = None;
    for lane in &lanes {
        let h = lane_h(lane);
        let row = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), h));
        y += h + GAP;
        if !row.intersects(area) {
            continue;
        }
        let in_area = |r: Rect| r.intersect(area).intersect(span);
        lp.rect_filled(
            Rect::from_min_max(pos2(span.left(), row.top()), row.right_bottom()),
            0.0,
            theme::RACK,
        );
        let is_folded = lane.sound && folded.contains(&lane.name);
        let name_rect = Rect::from_min_size(row.min, vec2(HEAD - 2.0, h)).intersect(area);
        let mut name_colour = if lane.enabled {
            theme::GRAPHITE
        } else {
            theme::DUST
        };
        if lane.sound && name_rect.is_positive() {
            let r = ui.interact(name_rect, id.with((&lane.name, "fold")), Sense::click());
            let what = if is_folded { "Unfold" } else { "Fold" };
            r.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{what} {}", lane.name))
            });
            if r.hovered() {
                name_colour = theme::PAPER;
            }
            if r.on_hover_text(format!("{} this lane", what.to_lowercase()))
                .clicked()
            {
                fold = Some(lane.name.clone());
            }
        }
        lp.text(
            pos2(rect.left() + 6.0, row.center().y),
            egui::Align2::LEFT_CENTER,
            &lane.name,
            egui::FontId::proportional(if is_folded { 9.0 } else { 12.0 }),
            name_colour,
        );
        for (i, b) in lane.blocks.iter().enumerate() {
            let r = in_area(Rect::from_min_max(
                pos2(x_of(b.start), row.top()),
                pos2(
                    x_of(b.start + b.length).max(x_of(b.start) + 2.0),
                    row.bottom(),
                ),
            ));
            if !r.is_positive() {
                continue;
            }
            let resp = ui.interact(
                r,
                id.with((&lane.name, i)),
                if b.shot.is_some() || b.id.is_some() {
                    Sense::click()
                } else {
                    Sense::hover()
                },
            );
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &b.name));
            let on = b.enabled && lane.enabled;
            let fill = match (lane.sound, on) {
                (_, false) => theme::RAISED,
                (false, true) => theme::VIDEO_CLIP,
                (true, true) => theme::AUDIO_CLIP,
            };
            let fill = if resp.hovered() && (b.shot.is_some() || b.id.is_some()) {
                fill.gamma_multiply(1.25)
            } else {
                fill
            };
            lp.rect_filled(r, 2.0, fill);
            let (width, colour) = if b.picked {
                (2.0, theme::PAPER)
            } else {
                (1.0, theme::LINE)
            };
            lp.rect_stroke(r, 2.0, Stroke::new(width, colour), egui::StrokeKind::Inside);
            if !is_folded {
                let inner = r.shrink(3.0);
                let p = lp.with_clip_rect(inner);
                let mut text_left = inner.left();
                // Asked for before it's made, a still would be remembered as missing.
                if let Some(still) = b.still.as_ref().filter(|f| f.is_file())
                    && inner.width() > 40.0
                    && inner.height() > 20.0
                {
                    let w = (inner.height() * 16.0 / 9.0).min(inner.width() / 2.0);
                    let s = Rect::from_min_size(inner.min, vec2(w, inner.height()));
                    crate::grouped::letterbox(ui, thumbs, still, &b.name, s);
                    text_left = s.right() + 4.0;
                }
                p.text(
                    pos2(text_left, inner.top()),
                    egui::Align2::LEFT_TOP,
                    &b.name,
                    small.clone(),
                    if on { theme::PAPER } else { theme::DUST },
                );
            }
            if !b.hover.is_empty() {
                resp.clone().on_hover_text(&b.hover);
            }
            if resp.clicked() {
                hit = match (&b.id, b.shot) {
                    (Some(id), _) => Some(Hit::Pick(id.clone())),
                    (None, Some(s)) => Some(Hit::Open(s)),
                    (None, None) => hit,
                };
            }
        }
        for (i, j) in lane.joins.iter().enumerate() {
            if let Some((start, length)) = j.fade {
                let band = in_area(Rect::from_min_max(
                    pos2(x_of(start), row.top() + 4.0),
                    pos2(x_of(start + length), row.bottom() - 4.0),
                ));
                if band.is_positive() {
                    lp.rect_filled(band, 2.0, theme::PAPER.gamma_multiply(0.18));
                }
            }
            let x = x_of(j.at);
            if x < span.left() || x > span.right() {
                continue;
            }
            let mark = Rect::from_center_size(pos2(x, row.bottom() - 8.0), vec2(14.0, 16.0));
            if !area.contains_rect(mark) {
                continue;
            }
            let resp = ui.interact(mark, id.with((&lane.name, "join", i)), Sense::click());
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Join {}", j.name))
            });
            let lit = j.picked || resp.hovered();
            let colour = if lit {
                theme::PAPER
            } else if j.continues {
                theme::TUNGSTEN
            } else if j.bridged {
                theme::STEEL
            } else {
                theme::DUST
            };
            let c = mark.center();
            if j.continues {
                // A shot carried on from the one before: a round mark, as a link.
                lp.circle(c, 5.5, colour, Stroke::new(1.0, theme::LINE));
                lp.line_segment(
                    [pos2(c.x, row.top() + 2.0), pos2(c.x, c.y - 5.5)],
                    Stroke::new(1.5, colour),
                );
            } else {
                lp.add(egui::Shape::convex_polygon(
                    vec![
                        pos2(c.x, c.y - 6.0),
                        pos2(c.x + 5.0, c.y),
                        pos2(c.x, c.y + 6.0),
                        pos2(c.x - 5.0, c.y),
                    ],
                    colour,
                    Stroke::new(1.0, theme::LINE),
                ));
            }
            if j.matched {
                // A small dot above it: colour matched.
                lp.circle_filled(pos2(c.x + 7.0, c.y - 7.0), 2.5, theme::GO);
            }
            let mut says = vec![match j.fade {
                Some((_, l)) => {
                    format!("{}: dissolve, {} frames", j.name, (l * fps).round() as u32)
                }
                None if j.continues => format!("{}: continues from the shot before", j.name),
                None => format!("{}: a straight cut", j.name),
            }];
            if j.continues && j.fade.is_some() {
                says.push("continues from the shot before".into());
            }
            if j.matched {
                says.push("colour matched".into());
            }
            if j.bridged {
                says.push("has a bridge".into());
            }
            let resp = resp.on_hover_text(says.join("; "));
            if resp.clicked() {
                hit = Some(Hit::PickJoin(j.id.clone(), j.at));
            }
        }
    }
    if let Some(r) = &vdrag {
        let at = |y: f32| area.top() + y / full * shown_h;
        let thumb = Rect::from_min_max(
            pos2(vbar.left(), at(view.top)),
            pos2(vbar.right(), at(view.top + shown_h)),
        );
        let lit = r.hovered() || r.dragged();
        p.rect_filled(thumb, 3.0, if lit { theme::DUST } else { theme::GRAPHITE });
    }
    if let Some(name) = fold {
        match folded.iter().position(|n| *n == name) {
            Some(i) => {
                folded.remove(i);
            }
            None => folded.push(name),
        }
        ui.data_mut(|d| d.insert_temp(folded_id, folded));
    }

    if let Some(t) = playhead.filter(|t| *t >= start && *t <= start + shown) {
        let x = x_of(t.clamp(0.0, length));
        // A handle to grab in the ruler.
        p.rect_filled(
            Rect::from_center_size(pos2(x, ruler.center().y), vec2(9.0, RULER - 4.0)),
            2.0,
            theme::STEEL,
        );
        p.line_segment(
            [pos2(x, rect.top()), pos2(x, area.bottom())],
            Stroke::new(1.5, theme::STEEL),
        );
        p.add(egui::Shape::convex_polygon(
            vec![
                pos2(x - 5.0, rect.top()),
                pos2(x + 5.0, rect.top()),
                pos2(x, rect.top() + 7.0),
            ],
            theme::STEEL,
            Stroke::NONE,
        ));
    }
    hit
}

/// Zooms `by` keeping the second under `x` where it is.
fn zoom_at(view: &mut View, by: f64, x: f32, span: Rect, width: f64, length: f64, most: f64) {
    let off = (x - span.left()) as f64;
    let under = view.start + off / (width * view.zoom / length);
    view.zoom = (view.zoom * by).clamp(1.0, most);
    view.start = under - off / (width * view.zoom / length);
}

/// Seconds between ruler ticks, and how many ticks to a labelled one: labels at least ~90 pt
/// apart, ticks down to single frames.
fn tick_step(pps: f64, fps: f64) -> (f64, i64) {
    let frame = 1.0 / fps;
    // Frames that go evenly into a second, so labels land on whole seconds too.
    let n = fps.round().max(1.0) as u32;
    let labels: Vec<f64> = (1..n)
        .filter(|d| n.is_multiple_of(*d))
        .map(|d| d as f64 * frame)
        .chain([1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0])
        .collect();
    let label = labels
        .iter()
        .copied()
        .find(|s| s * pps >= 90.0)
        .unwrap_or(1200.0);
    // Ticks between labels, at least 8 pt apart.
    let tick = labels
        .iter()
        .copied()
        .chain([frame])
        .filter(|s| *s <= label + 1e-9 && s * pps >= 8.0)
        .filter(|s| ((label / s).round() * s - label).abs() < 1e-6)
        .fold(label, f64::min);
    (tick, (label / tick).round().max(1.0) as i64)
}
