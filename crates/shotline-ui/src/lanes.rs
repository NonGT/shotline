//! A cut drawn the way editors draw one: a time ruler, video tracks stacked above the audio
//! tracks (the highest V on top, A1 right under V1), each clip a block as long as it lasts, and a
//! playhead. Pinching zooms in on it, scrolling or the bar under it moves along. Clicking or
//! dragging along the ruler moves the playhead; clicking a clip opens its
//! shot, or picks it when it can be changed.

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
    /// A join, by the clip after it.
    PickJoin(String),
}

const HEAD: f32 = 36.0;
const RULER: f32 = 22.0;
const VIDEO: f32 = 54.0;
const AUDIO: f32 = 30.0;
const GAP: f32 = 2.0;
const BAR: f32 = 8.0;
/// As far in as a pinch goes.
const MOST_PPS: f64 = 400.0;

/// How far a timeline is pinched in, and the second at its left edge.
#[derive(Clone, Copy, Default)]
struct View {
    zoom: f64,
    start: f64,
    /// Where the playhead was last frame, to keep it in sight while it plays.
    last: Option<f64>,
}

/// `video` V1 first, `audio` A1 first. `length` is the seconds the ruler spans.
pub(crate) fn show(
    ui: &mut egui::Ui,
    id: &str,
    video: &[Lane],
    audio: &[Lane],
    length: f64,
    playhead: Option<f64>,
    thumbs: &mut crate::Thumbs,
) -> Option<Hit> {
    let lanes: Vec<&Lane> = video.iter().rev().chain(audio).collect();
    let height = RULER
        + BAR
        + lanes
            .iter()
            .map(|l| if l.sound { AUDIO } else { VIDEO } + GAP)
            .sum::<f32>();
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let id = ui.scope_id().with(("lanes", id));
    let span = Rect::from_min_max(pos2(rect.left() + HEAD, rect.top()), rect.right_bottom());
    let length = length.max(1.0);
    let view_id = id.with("view");
    let mut view: View = ui.data(|d| d.get_temp(view_id)).unwrap_or_default();
    let width = span.width().max(1.0) as f64;
    let most = (length * MOST_PPS / width).max(1.0);
    view.zoom = view.zoom.clamp(1.0, most);
    // Pinch in on the second under the fingers; scroll along, sideways or not.
    if ui.rect_contains_pointer(rect) {
        let (pinch, scroll, at) = ui.input(|i| {
            (
                i.zoom_delta() as f64,
                i.smooth_scroll_delta,
                i.pointer.hover_pos(),
            )
        });
        let pps = width * view.zoom / length;
        if pinch != 1.0 {
            let x = at.map_or(span.center().x, |a| a.x.clamp(span.left(), span.right()));
            let off = (x - span.left()) as f64;
            let under = view.start + off / pps;
            view.zoom = (view.zoom * pinch).clamp(1.0, most);
            view.start = under - off / (width * view.zoom / length);
        } else if view.zoom > 1.0 {
            let d = if scroll.x != 0.0 { scroll.x } else { scroll.y };
            view.start -= d as f64 / pps;
        }
    }
    let shown = length / view.zoom;
    let pps = width / shown;
    // The bar under the tracks: what's in sight of the whole, dragged to move along.
    let bar = Rect::from_min_size(
        pos2(span.left(), rect.bottom() - BAR + 2.0),
        vec2(span.width(), BAR - 3.0),
    );
    // Only there when pinched in.
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
    view.start = view.start.clamp(0.0, length - shown);
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

    // Clicked or dragged, the ruler moves the playhead.
    let ruler = Rect::from_min_size(span.min, vec2(span.width(), RULER));
    let seek = ui.interact(ruler, id.with("seek"), Sense::click_and_drag());
    seek.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Slider, true, "Playhead"));
    if let Some(at) = seek
        .interact_pointer_pos()
        .or_else(|| ui.ctx().pointer_latest_pos())
        .filter(|_| seek.clicked() || seek.dragged() || seek.drag_stopped())
    {
        if seek.clicked() || seek.drag_stopped() {
            hit = Some(Hit::Seek(t_of(at.x)));
        } else if seek.dragged() {
            hit = Some(Hit::Scrub(t_of(at.x)));
        }
    }
    p.rect_filled(ruler, 0.0, theme::RACK);
    let step = tick_step(pps);
    let small = egui::FontId::proportional(11.0);
    let mut t = (start / step).floor() * step;
    while t <= (start + shown).min(length) + 1e-9 {
        let x = x_of(t);
        if x < span.left() - 0.5 {
            t += step;
            continue;
        }
        p.line_segment(
            [pos2(x, ruler.bottom() - 6.0), pos2(x, ruler.bottom())],
            Stroke::new(1.0, theme::DUST),
        );
        p.text(
            pos2(x + 3.0, ruler.top() + 3.0),
            egui::Align2::LEFT_TOP,
            clock(t),
            small.clone(),
            theme::DUST,
        );
        t += step;
    }

    let mut y = ruler.bottom() + GAP;
    for lane in &lanes {
        let h = if lane.sound { AUDIO } else { VIDEO };
        let row = Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), h));
        p.rect_filled(
            Rect::from_min_max(pos2(span.left(), row.top()), row.right_bottom()),
            0.0,
            theme::RACK,
        );
        p.text(
            pos2(rect.left() + 6.0, row.center().y),
            egui::Align2::LEFT_CENTER,
            &lane.name,
            egui::FontId::proportional(12.0),
            if lane.enabled {
                theme::GRAPHITE
            } else {
                theme::DUST
            },
        );
        for (i, b) in lane.blocks.iter().enumerate() {
            let r = Rect::from_min_max(
                pos2(x_of(b.start), row.top()),
                pos2(
                    x_of(b.start + b.length).max(x_of(b.start) + 2.0),
                    row.bottom(),
                ),
            )
            .intersect(span);
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
            p.rect_filled(r, 2.0, fill);
            let (width, colour) = if b.picked {
                (2.0, theme::TUNGSTEN)
            } else {
                (1.0, theme::LINE)
            };
            p.rect_stroke(r, 2.0, Stroke::new(width, colour), egui::StrokeKind::Inside);
            let inner = r.shrink(3.0);
            let p = p.with_clip_rect(inner);
            let mut text_left = inner.left();
            // Asked for before it's made, a still would be remembered as missing.
            if let Some(still) = b.still.as_ref().filter(|f| f.is_file())
                && inner.width() > 40.0
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
                let band = Rect::from_min_max(
                    pos2(x_of(start), row.top() + 4.0),
                    pos2(x_of(start + length), row.bottom() - 4.0),
                )
                .intersect(span);
                if band.is_positive() {
                    p.rect_filled(band, 2.0, theme::TUNGSTEN.gamma_multiply(0.35));
                }
            }
            let x = x_of(j.at);
            if x < span.left() || x > span.right() {
                continue;
            }
            let mark = Rect::from_center_size(pos2(x, row.bottom() - 8.0), vec2(14.0, 16.0));
            let resp = ui.interact(mark, id.with((&lane.name, "join", i)), Sense::click());
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Join {}", j.name))
            });
            let colour = if j.picked || resp.hovered() {
                theme::TUNGSTEN
            } else if j.bridged {
                theme::TUNGSTEN_DEEP
            } else {
                theme::PAPER
            };
            let c = mark.center();
            p.add(egui::Shape::convex_polygon(
                vec![
                    pos2(c.x, c.y - 6.0),
                    pos2(c.x + 5.0, c.y),
                    pos2(c.x, c.y + 6.0),
                    pos2(c.x - 5.0, c.y),
                ],
                colour,
                Stroke::new(1.0, theme::LINE),
            ));
            let bridged = if j.bridged { "; has a bridge" } else { "" };
            let resp = resp.on_hover_text(match j.fade {
                Some((_, l)) => format!("{}: dissolve, {l:.2} s{bridged}", j.name),
                None => format!("{}: a straight cut{bridged}", j.name),
            });
            if resp.clicked() {
                hit = Some(Hit::PickJoin(j.id.clone()));
            }
        }
        y += h + GAP;
    }

    if let Some(t) = playhead.filter(|t| *t >= start && *t <= start + shown) {
        let x = x_of(t.clamp(0.0, length));
        // A handle to grab in the ruler.
        p.rect_filled(
            Rect::from_center_size(pos2(x, ruler.center().y), vec2(9.0, RULER - 4.0)),
            2.0,
            theme::TUNGSTEN,
        );
        p.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(1.5, theme::TUNGSTEN),
        );
        p.add(egui::Shape::convex_polygon(
            vec![
                pos2(x - 5.0, rect.top()),
                pos2(x + 5.0, rect.top()),
                pos2(x, rect.top() + 7.0),
            ],
            theme::TUNGSTEN,
            Stroke::NONE,
        ));
    }
    hit
}

/// Seconds between ruler marks, at least ~70 px apart.
fn tick_step(pps: f64) -> f64 {
    [1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0]
        .into_iter()
        .find(|s| s * pps >= 70.0)
        .unwrap_or(1200.0)
}

fn clock(t: f64) -> String {
    let t = t.round() as u64;
    format!("{}:{:02}", t / 60, t % 60)
}
