//! The Cost tab: what was spent, per day and model, and per shot.

use std::collections::{BTreeMap, HashMap, HashSet};

use eframe::egui::{self, Color32, RichText};
use jiff::civil::Date;
use shotline_core::ledger::report::{self, By, Row, Summary};
use shotline_core::ledger::{Entry, Kind, money};

use crate::{App, PAGE, hint, page, theme};

/// Model colours, biggest spender first. Red is left out: it means trouble elsewhere.
const PALETTE: [Color32; 8] = [
    Color32::from_rgb(0x6f, 0x8f, 0xd8),
    Color32::from_rgb(0xe0, 0xa8, 0x4a),
    Color32::from_rgb(0x56, 0xb3, 0xa6),
    Color32::from_rgb(0xb0, 0x7f, 0xd6),
    Color32::from_rgb(0xd9, 0x7c, 0x9b),
    Color32::from_rgb(0x8f, 0xb8, 0x5f),
    Color32::from_rgb(0xc9, 0xa2, 0x7a),
    Color32::from_rgb(0x5f, 0xb7, 0xd6),
];
const OTHERS: Color32 = Color32::from_rgb(0x6b, 0x6b, 0x70);
const SHOWN: usize = 8;

pub(crate) struct Cost {
    pub summary: Summary,
    pub by_shot: Vec<Row>,
    /// Most spent first.
    models: Vec<Model>,
    /// Spent per model, same order as `models`.
    days: BTreeMap<Date, Vec<f64>>,
    today: Date,
}

struct Model {
    name: String,
    plugin: Option<String>,
    op: Option<String>,
    spent: f64,
    units: f64,
    unit_name: Option<String>,
    jobs: usize,
}

impl Cost {
    pub fn new(
        entries: &[Entry],
        summary: Summary,
        by_shot: Vec<Row>,
        rates: &shotline_core::ledger::Rates,
        tz: &jiff::tz::TimeZone,
        now: jiff::Timestamp,
    ) -> anyhow::Result<Cost> {
        let mut models: Vec<Model> = vec![];
        let mut index: HashMap<(Option<String>, String), usize> = HashMap::new();
        let mut jobs: Vec<HashSet<&str>> = vec![];
        let mut spent: Vec<(Date, usize, f64)> = vec![];
        for e in entries.iter().filter(|e| e.spent() != 0.0) {
            let name = match (&e.model, e.kind) {
                (Some(m), _) => m.clone(),
                (None, Kind::Adjust) => "corrections".into(),
                (None, _) => e.plugin.clone().unwrap_or_else(|| "other".into()),
            };
            let key = (e.plugin.clone(), name.clone());
            let i = *index.entry(key).or_insert_with(|| {
                models.push(Model {
                    name,
                    plugin: e.plugin.clone(),
                    op: e.op.clone(),
                    spent: 0.0,
                    units: 0.0,
                    unit_name: None,
                    jobs: 0,
                });
                jobs.push(HashSet::new());
                models.len() - 1
            });
            let v = report::value(e, &summary.currency, rates)?;
            let m = &mut models[i];
            m.spent += v;
            if e.kind == Kind::Charge
                && let Some(u) = e.units
            {
                m.units += u;
                m.unit_name = m.unit_name.take().or(e.unit_name.clone());
            }
            if let Some(j) = &e.job {
                jobs[i].insert(j);
            }
            spent.push((e.time.to_zoned(tz.clone()).date(), i, v));
        }
        for (m, j) in models.iter_mut().zip(&jobs) {
            m.jobs = j.len();
        }
        let mut order: Vec<usize> = (0..models.len()).collect();
        order.sort_by(|&a, &b| models[b].spent.total_cmp(&models[a].spent));
        let mut rank = vec![0; models.len()];
        for (r, &i) in order.iter().enumerate() {
            rank[i] = r;
        }
        let mut days: BTreeMap<Date, Vec<f64>> = BTreeMap::new();
        for (d, i, v) in spent {
            days.entry(d).or_insert_with(|| vec![0.0; models.len()])[rank[i]] += v;
        }
        let mut sorted: Vec<Option<Model>> = models.into_iter().map(Some).collect();
        let models = order.iter().map(|&i| sorted[i].take().unwrap()).collect();
        Ok(Cost {
            summary,
            by_shot,
            models,
            days,
            today: now.to_zoned(tz.clone()).date(),
        })
    }

    /// The days to chart, oldest first, with nothing-spent days in between.
    fn span(&self, span: Span) -> Vec<Date> {
        let first = match span {
            Span::Week => self.today.saturating_sub(jiff::Span::new().days(6)),
            Span::Month => self.today.saturating_sub(jiff::Span::new().days(29)),
            Span::All => *self.days.keys().next().unwrap_or(&self.today),
        };
        let last = self
            .today
            .max(self.days.keys().next_back().copied().unwrap_or(self.today));
        first
            .series(jiff::Span::new().days(1))
            .take_while(|d| *d <= last)
            .collect()
    }

    fn colour(&self, rank: usize) -> Color32 {
        PALETTE.get(rank).copied().unwrap_or(OTHERS)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Span {
    #[default]
    Week,
    Month,
    All,
}

#[derive(Default)]
pub(crate) struct View {
    span: Span,
    /// When the bars started growing.
    since: Option<f64>,
    last_frame: u64,
    /// A model to pick out, from the table.
    pick: Option<usize>,
    all: bool,
}

impl App {
    pub(crate) fn costs(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BAY))
            .show(ui, |ui| {
                page(ui, PAGE, |ui| {
                    let Some(c) = &self.film.cost else {
                        hint(
                            ui,
                            "Nothing spent yet. Paid jobs show up here, by day and model.",
                        );
                        return;
                    };
                    let v = &mut self.cost_view;
                    // Grow the bars again each time the tab is opened.
                    let frame = ui.ctx().cumulative_frame_nr();
                    if frame > v.last_frame + 1 {
                        v.since = None;
                    }
                    v.last_frame = frame;

                    summary(ui, &c.summary);
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Spent per day")
                                .text_style(egui::TextStyle::Heading)
                                .color(theme::PAPER),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            for (s, name) in [
                                (Span::All, "All"),
                                (Span::Month, "30 days"),
                                (Span::Week, "7 days"),
                            ] {
                                if ui.selectable_label(v.span == s, name).clicked() && v.span != s {
                                    v.span = s;
                                    v.since = None;
                                }
                            }
                        });
                    });
                    ui.add_space(6.0);
                    let hovered = chart(ui, c, v);
                    ui.add_space(14.0);
                    models(ui, c, v, hovered);
                    ui.add_space(18.0);
                    by_shot(ui, c);
                });
            });
    }
}

fn summary(ui: &mut egui::Ui, s: &Summary) {
    let cur = &s.currency;
    ui.heading(format!("{} {cur}", money(s.spent)));
    ui.label(format!(
        "in the cut {}, thrown away {}, other {}",
        money(s.in_cut),
        money(s.discarded),
        money(s.other)
    ));
    if s.held > 0.0 {
        ui.label(
            RichText::new(format!(
                "{} {cur} held for jobs still running",
                money(s.held)
            ))
            .color(theme::PAPER),
        );
    }
}

/// Stacked bars, one per day, growing in from the left. Returns the model under the pointer.
fn chart(ui: &mut egui::Ui, c: &Cost, v: &mut View) -> Option<usize> {
    let days = c.span(v.span);
    let totals: Vec<f64> = days
        .iter()
        .map(|d| c.days.get(d).map_or(0.0, |m| m.iter().sum()))
        .collect();
    let (step, top) = ticks(totals.iter().copied().fold(0.0, f64::max));

    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 260.0), egui::Sense::hover());
    let label = match v.span {
        Span::Week => "spent per day, last 7 days",
        Span::Month => "spent per day, last 30 days",
        Span::All => "spent per day",
    };
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, label));
    let p = ui.painter_at(rect.expand(2.0));
    let font = egui::FontId::proportional(12.0);
    let axis = 64.0;
    let plot = egui::Rect::from_min_max(
        rect.min + egui::vec2(axis, 8.0),
        rect.max - egui::vec2(0.0, 26.0),
    );
    let y = |amount: f64| plot.bottom() - (amount / top) as f32 * plot.height();

    let mut t = 0.0;
    while t <= top + step / 2.0 {
        let yy = y(t);
        p.extend(egui::Shape::dashed_line(
            &[egui::pos2(plot.left(), yy), egui::pos2(plot.right(), yy)],
            egui::Stroke::new(1.0, theme::LINE),
            4.0,
            4.0,
        ));
        p.text(
            egui::pos2(rect.left(), yy),
            egui::Align2::LEFT_CENTER,
            format!("{} {}", money(t), c.summary.currency),
            font.clone(),
            theme::DUST,
        );
        t += step;
    }

    let now = ui.input(|i| i.time);
    let since = *v.since.get_or_insert(now);
    let n = days.len().max(1);
    let slot = plot.width() / n as f32;
    let bar = (slot * 0.45).clamp(2.0, 44.0);
    let stagger = (0.2 / n as f64).min(0.04);
    let every = ((56.0 / slot).ceil() as usize).max(1);
    let pointer = resp.hover_pos();
    let mut hovered = None;
    let mut moving = false;
    for (i, d) in days.iter().enumerate() {
        let cx = plot.left() + slot * (i as f32 + 0.5);
        if (n - 1 - i).is_multiple_of(every) {
            p.text(
                egui::pos2(cx, plot.bottom() + 8.0),
                egui::Align2::CENTER_TOP,
                d.strftime("%b %-d").to_string(),
                font.clone(),
                theme::DUST,
            );
        }
        let Some(spent) = c.days.get(d) else { continue };
        let grow = ((now - since - i as f64 * stagger) / 0.4).clamp(0.0, 1.0);
        moving |= grow < 1.0;
        let grow = 1.0 - (1.0 - grow).powi(3);
        let mut base = 0.0;
        for (rank, &amount) in spent.iter().enumerate() {
            if amount <= 0.0 {
                continue;
            }
            let seg = egui::Rect::from_min_max(
                egui::pos2(cx - bar / 2.0, y((base + amount) * grow)),
                egui::pos2(cx + bar / 2.0, y(base * grow)),
            );
            base += amount;
            let colour = c.colour(rank);
            let picked = v.pick.is_none_or(|m| m == rank.min(SHOWN));
            p.rect_filled(
                seg,
                0.0,
                if picked {
                    colour
                } else {
                    colour.gamma_multiply(0.3)
                },
            );
            if pointer.is_some_and(|pt| seg.expand2(egui::vec2(0.0, 1.0)).contains(pt)) {
                hovered = Some((rank, *d, amount));
            }
        }
    }
    if moving {
        ui.ctx().request_repaint();
    }
    let (rank, day, amount) = hovered?;
    let m = &c.models[rank];
    resp.on_hover_ui_at_pointer(|ui| {
        ui.label(RichText::new(&m.name).color(theme::PAPER));
        ui.weak(format!(
            "{} · {} {}",
            day.strftime("%b %-d"),
            money(amount),
            c.summary.currency
        ));
    });
    Some(rank)
}

/// A round step for the y axis and the top it reaches, with four or so lines.
fn ticks(max: f64) -> (f64, f64) {
    if max <= 0.0 {
        return (0.25, 1.0);
    }
    let raw = max / 4.0;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|s| s * mag)
        .find(|s| *s >= raw)
        .unwrap_or(10.0 * mag);
    (step, (max / step).ceil() * step)
}

fn models(ui: &mut egui::Ui, c: &Cost, v: &mut View, hovered: Option<usize>) {
    let cur = &c.summary.currency;
    let shown = if v.all {
        c.models.len()
    } else {
        SHOWN.min(c.models.len())
    };
    let width = ui.available_width();
    let cols = [width - 330.0, width - 190.0, width - 110.0, width];
    let header = |ui: &mut egui::Ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 26.0), egui::Sense::hover());
        let p = ui.painter();
        let f = egui::FontId::proportional(13.0);
        p.text(
            rect.left_center() + egui::vec2(26.0, 0.0),
            egui::Align2::LEFT_CENTER,
            "Model",
            f.clone(),
            theme::DUST,
        );
        for (x, t) in [
            (cols[1], "Usage"),
            (cols[2], "Jobs"),
            (cols[3] - 4.0, cur.as_str()),
        ] {
            p.text(
                egui::pos2(x, rect.center().y),
                egui::Align2::RIGHT_CENTER,
                t,
                f.clone(),
                theme::DUST,
            );
        }
    };
    header(ui);
    let mut pick = None;
    for (rank, m) in c.models.iter().take(shown).enumerate() {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 34.0), egui::Sense::hover());
        let usage = match &m.unit_name {
            Some(u) if m.units > 0.0 => {
                let s = if m.units == 1.0 || u.ends_with('s') {
                    ""
                } else {
                    "s"
                };
                format!("{} {u}{s}", money(m.units))
            }
            _ => "–".into(),
        };
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::Role::Label,
                true,
                format!("{}  {} {cur}", m.name, money(m.spent)),
            )
        });
        let lit = resp.hovered() || hovered == Some(rank);
        if resp.hovered() {
            pick = Some(rank.min(SHOWN));
        }
        let p = ui.painter();
        if lit {
            p.rect_filled(rect, 3.0, theme::RAISED);
        }
        p.hline(
            rect.x_range(),
            rect.bottom(),
            egui::Stroke::new(1.0, theme::LINE),
        );
        let mid = rect.center().y;
        p.circle_filled(egui::pos2(rect.left() + 11.0, mid), 5.0, c.colour(rank));
        let f = egui::FontId::proportional(14.0);
        let name = p.text(
            egui::pos2(rect.left() + 26.0, mid),
            egui::Align2::LEFT_CENTER,
            &m.name,
            f.clone(),
            theme::PAPER,
        );
        let mut x = name.right() + 10.0;
        for tag in [m.op.as_deref(), m.plugin.as_deref()].into_iter().flatten() {
            let g = p.layout_no_wrap(tag.into(), egui::FontId::proportional(12.0), theme::DUST);
            let chip = egui::Rect::from_min_size(
                egui::pos2(x, mid - g.size().y / 2.0 - 2.0),
                g.size() + egui::vec2(12.0, 4.0),
            );
            if chip.right() > cols[0] {
                break;
            }
            p.rect_filled(chip, 4.0, theme::RACK);
            p.galley(chip.min + egui::vec2(6.0, 2.0), g, theme::DUST);
            x = chip.right() + 6.0;
        }
        p.text(
            egui::pos2(cols[1], mid),
            egui::Align2::RIGHT_CENTER,
            usage,
            f.clone(),
            theme::GRAPHITE,
        );
        p.text(
            egui::pos2(cols[2], mid),
            egui::Align2::RIGHT_CENTER,
            m.jobs.to_string(),
            f.clone(),
            theme::GRAPHITE,
        );
        p.text(
            egui::pos2(cols[3] - 4.0, mid),
            egui::Align2::RIGHT_CENTER,
            money(m.spent),
            f,
            theme::PAPER,
        );
    }
    v.pick = pick.or(hovered.map(|r| r.min(SHOWN)));
    if c.models.len() > SHOWN {
        ui.add_space(6.0);
        let text = if v.all {
            "Show fewer".to_string()
        } else {
            format!("Show all ({})", c.models.len())
        };
        if ui.button(text).clicked() {
            v.all = !v.all;
        }
    }
}

fn by_shot(ui: &mut egui::Ui, c: &Cost) {
    ui.label(
        RichText::new("Per shot")
            .text_style(egui::TextStyle::Heading)
            .color(theme::PAPER),
    );
    ui.add_space(4.0);
    egui::Grid::new("by shot")
        .striped(true)
        .num_columns(3)
        .spacing(egui::vec2(24.0, 6.0))
        .show(ui, |ui| {
            ui.weak("Shot");
            ui.weak(c.summary.currency.as_str());
            ui.weak("Jobs");
            ui.end_row();
            for r in &c.by_shot {
                ui.label(if r.key == "-" {
                    "outside shots"
                } else {
                    &r.key
                });
                ui.label(money(r.spent));
                ui.label(r.jobs.to_string());
                ui.end_row();
            }
        });
}

/// Loads the film's spending, or None if there's nothing yet.
pub(crate) fn load(
    p: &shotline_core::Project,
    user_dir: &std::path::Path,
) -> anyhow::Result<Option<Cost>> {
    let user = shotline_core::UserConfig::load(user_dir)?;
    let b = p.books(user_dir, &user, &[])?;
    let entries = b.ledger.entries()?;
    if entries.is_empty() {
        return Ok(None);
    }
    let s = report::summary(&entries, &b.currency, &b.rates, &p.selected_take_ids()?)?;
    let rows = report::group(&entries, By::Shot, &b.currency, &b.rates, &b.tz)?;
    Ok(Some(Cost::new(
        &entries,
        s,
        rows,
        &b.rates,
        &b.tz,
        p.clock.now(),
    )?))
}
