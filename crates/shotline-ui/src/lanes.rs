//! A cut drawn the way editors draw one: a time ruler, video tracks stacked above the audio
//! tracks (the highest V on top, A1 right under V1), each clip a block as long as it lasts, and a
//! playhead. Clicking or dragging along the ruler moves the playhead; clicking a clip opens its
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
        + lanes
            .iter()
            .map(|l| if l.sound { AUDIO } else { VIDEO } + GAP)
            .sum::<f32>();
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let id = ui.scope_id().with(("lanes", id));
    let span = Rect::from_min_max(pos2(rect.left() + HEAD, rect.top()), rect.right_bottom());
    let length = length.max(1.0);
    let pps = span.width() as f64 / length;
    let x_of = |t: f64| span.left() + (t * pps) as f32;
    let t_of = |x: f32| (((x - span.left()) as f64) / pps).clamp(0.0, length);
    let p = ui.painter_at(rect);
    let mut hit = None;

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
    let mut t = 0.0;
    while t <= length + 1e-9 {
        let x = x_of(t);
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
                p.rect_filled(band, 2.0, theme::TUNGSTEN.gamma_multiply(0.35));
            }
            let x = x_of(j.at);
            let mark = Rect::from_center_size(pos2(x, row.bottom() - 8.0), vec2(14.0, 16.0));
            let resp = ui.interact(mark, id.with((&lane.name, "join", i)), Sense::click());
            resp.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Join {}", j.name))
            });
            let colour = if j.picked || resp.hovered() {
                theme::TUNGSTEN
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
            let resp = resp.on_hover_text(match j.fade {
                Some((_, l)) => format!("{}: dissolve, {l:.2} s", j.name),
                None => format!("{}: a straight cut", j.name),
            });
            if resp.clicked() {
                hit = Some(Hit::PickJoin(j.id.clone()));
            }
        }
        y += h + GAP;
    }

    if let Some(t) = playhead {
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
