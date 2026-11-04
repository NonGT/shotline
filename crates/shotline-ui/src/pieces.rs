//! Making a take from pieces of others: play a take, mark where a piece starts and ends, add it,
//! and join each piece to the one before with a cut, a fade or made-up frames in between.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use shotline_core::{Join, Piece, Project, ShotRef, Take};
use shotline_media::Ffmpeg;

use crate::player::{Audio, Deck, Player};
use crate::{hint, theme};

pub(crate) enum Making {
    Idle,
    Busy,
    Done(Result<Take, String>),
}

pub(crate) struct Splicer {
    pub shot: ShotRef,
    /// The shot's takes that have a video.
    sources: Vec<(Take, PathBuf)>,
    source: usize,
    deck: Option<Deck>,
    /// Where the player goes once it's open.
    go_to: Option<f64>,
    start: Option<f64>,
    end: Option<f64>,
    pieces: Vec<Piece>,
    /// The piece whose start and end the marks change, instead of a new one.
    editing: Option<usize>,
    colormatch: bool,
    pub making: Arc<Mutex<Making>>,
    error: Option<String>,
}

const JOINS: [(Join, &str, &str); 3] = [
    (Join::Cut, "Cut", "straight from one to the next"),
    (
        Join::Fade,
        "Fade",
        "the two overlap for 0.3s and fade into each other",
    ),
    (
        Join::Smooth,
        "Smooth",
        "0.25s of frames made up between them; for two pieces that almost line up",
    ),
];

impl Splicer {
    /// Starts on `first`'s take, or the first one, with `pieces` already in.
    pub fn new(shot: ShotRef, sources: Vec<(Take, PathBuf)>, pieces: Vec<Piece>) -> Splicer {
        let source = pieces
            .first()
            .and_then(|p| sources.iter().position(|(t, _)| t.id == p.take))
            .unwrap_or(0);
        Splicer {
            shot,
            sources,
            source,
            deck: None,
            go_to: None,
            start: None,
            end: None,
            pieces,
            editing: None,
            colormatch: true,
            making: Arc::new(Mutex::new(Making::Idle)),
            error: None,
        }
    }

    pub fn pause(&mut self, audio: Option<&Audio>) {
        if let Some(d) = &mut self.deck {
            d.pause(audio);
        }
    }

    fn open(&mut self, i: usize, ff: &Ffmpeg, audio: Option<&Audio>, height: f32) {
        self.pause(audio);
        self.source = i;
        let (t, file) = &self.sources[i];
        self.deck = match Player::new(ff, &t.to_string(), std::slice::from_ref(file)) {
            Ok(p) => {
                let mut d = Deck::new(&t.to_string(), p, true).in_popup(height);
                if let Some(at) = self.go_to.take() {
                    d.seek(at, audio);
                }
                Some(d)
            }
            Err(e) => {
                self.error = Some(format!("can't play {t}: {e}"));
                None
            }
        };
    }

    fn length(&self) -> f64 {
        self.deck.as_ref().map_or(0.0, |d| d.players[0].duration())
    }

    fn now(&self) -> f64 {
        self.deck.as_ref().map_or(0.0, |d| d.players[0].position())
    }

    /// The window. Returns false once it's closed.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        ff: Option<&Ffmpeg>,
        audio: Option<&Audio>,
        project: &Project,
    ) -> bool {
        let Some(ff) = ff else {
            return false;
        };
        let tall = ui.ctx().content_rect().height() * 0.45;
        if self.deck.is_none() && self.error.is_none() && !self.sources.is_empty() {
            self.open(self.source, ff, audio, tall);
        }
        let busy = !matches!(*self.making.lock().unwrap(), Making::Idle);
        let wide = (ui.ctx().content_rect().width() * 0.7).clamp(480.0, 960.0);
        let mut close = false;
        let mut pick = None;
        let modal = crate::modal(ui.ctx(), "pieces", |ui| {
            ui.set_width(wide);
            ui.horizontal(|ui| {
                ui.heading("Make a take from pieces");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = ui
                        .add_enabled_ui(!busy, |ui| {
                            crate::icon_button(ui, "Close", crate::close_icon)
                        })
                        .inner
                        .on_hover_text("Close")
                        .on_disabled_hover_text("still making the take")
                        .clicked();
                });
            });
            ui.label(
                RichText::new(
                    "Play a take, mark where a piece starts and ends, and add it. Made here with \
                     ffmpeg; nothing is spent.",
                )
                .color(theme::DUST),
            );
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                for (i, (t, _)) in self.sources.iter().enumerate() {
                    let r = ui.selectable_label(i == self.source, format!("#{}", t.number));
                    r.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::Role::RadioButton,
                            true,
                            i == self.source,
                            format!("cut from #{}", t.number),
                        )
                    });
                    if r.clicked() && i != self.source {
                        pick = Some(i);
                    }
                }
            });
            ui.add_space(6.0);
            if let Some(d) = &mut self.deck {
                d.show(ui, audio);
            }
            if let Some(e) = &self.error {
                ui.label(RichText::new(e).color(theme::FLAG));
            }
            self.range_bar(ui);
            ui.add_space(6.0);
            self.marks(ui, audio);
            ui.add_space(12.0);
            self.list(ui, audio);
            ui.add_space(10.0);
            self.make_row(ui, ff, project);
        });
        if let Some(i) = pick {
            self.editing = None;
            self.open(i, ff, audio, tall);
        }
        if (close || modal.should_close()) && !busy {
            self.pause(audio);
            return false;
        }
        true
    }

    /// The take's length, with its pieces and the marked stretch on it.
    fn range_bar(&self, ui: &mut egui::Ui) {
        let total = self.length();
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), egui::Sense::hover());
        let p = ui.painter();
        p.rect_filled(rect, 2.0, theme::LINE);
        if total <= 0.0 {
            return;
        }
        let x = |t: f64| rect.left() + rect.width() * (t / total).clamp(0.0, 1.0) as f32;
        let span = |a: f64, b: f64| egui::Rect::from_x_y_ranges(x(a)..=x(b), rect.y_range());
        let here = self.sources[self.source].0.id;
        for (i, piece) in self.pieces.iter().enumerate() {
            if piece.take == here && Some(i) != self.editing {
                p.rect_filled(
                    span(piece.from, piece.to),
                    2.0,
                    theme::TUNGSTEN.gamma_multiply(0.4),
                );
            }
        }
        let (a, b) = self.marked();
        if let (Some(a), Some(b)) = (a, b) {
            p.rect_filled(span(a, b), 2.0, theme::TUNGSTEN_LIT);
        } else if let Some(a) = a.or(b) {
            p.rect_filled(
                span(a, a).expand2(egui::vec2(1.0, 2.0)),
                0.0,
                theme::TUNGSTEN_LIT,
            );
        }
    }

    fn marked(&self) -> (Option<f64>, Option<f64>) {
        match self.editing.and_then(|i| self.pieces.get(i)) {
            Some(p) => (Some(p.from), Some(p.to)),
            None => (self.start, self.end),
        }
    }

    /// Start and end, typed or taken from where the player is, and Add.
    fn marks(&mut self, ui: &mut egui::Ui, audio: Option<&Audio>) {
        let total = self.length();
        let now = self.now();
        let keys = ui.memory(|m| m.focused().is_none());
        let (mut a, mut b) = self.marked();
        let mut changed = false;
        ui.horizontal(|ui| {
            let r = ui
                .button("Start here")
                .on_hover_text("where the player is now (I)");
            if r.clicked() || (keys && ui.input(|i| i.key_pressed(egui::Key::I))) {
                a = Some(now);
                changed = true;
            }
            changed |= seconds(ui, "Start", &mut a, 0.0, total);
            ui.add_space(12.0);
            let r = ui
                .button("End here")
                .on_hover_text("where the player is now (O)");
            if r.clicked() || (keys && ui.input(|i| i.key_pressed(egui::Key::O))) {
                b = Some(now);
                changed = true;
            }
            changed |= seconds(ui, "End", &mut b, total, total);
            ui.add_space(12.0);
            match self.editing {
                Some(i) => {
                    ui.label(
                        RichText::new(format!("changing piece {}", i + 1)).color(theme::TUNGSTEN),
                    );
                    if ui.button("Done").clicked() {
                        self.editing = None;
                    }
                }
                None => {
                    let from = a.unwrap_or(0.0);
                    let to = b.unwrap_or(total);
                    let ok = total > 0.0 && to > from;
                    let r = ui
                        .add_enabled(ok, egui::Button::new("Add piece"))
                        .on_hover_text("without marks, the whole take")
                        .on_disabled_hover_text("the end has to come after the start");
                    if r.clicked() {
                        self.pieces.push(Piece {
                            take: self.sources[self.source].0.id,
                            from,
                            to,
                            join: Join::Cut,
                        });
                        (a, b) = (None, None);
                        changed = true;
                        if let Some(d) = &mut self.deck {
                            d.pause(audio);
                        }
                    }
                }
            }
        });
        if changed {
            match self.editing.and_then(|i| self.pieces.get_mut(i)) {
                Some(p) => {
                    p.from = a.unwrap_or(p.from);
                    p.to = b.unwrap_or(p.to);
                }
                None => (self.start, self.end) = (a, b),
            }
        }
    }

    fn list(&mut self, ui: &mut egui::Ui, audio: Option<&Audio>) {
        ui.label(
            RichText::new("Pieces")
                .family(egui::FontFamily::Name(theme::SEMIBOLD.into()))
                .color(theme::PAPER),
        );
        ui.add_space(4.0);
        if self.pieces.is_empty() {
            hint(ui, "None yet.");
            return;
        }
        enum Do {
            Edit(usize),
            Up(usize),
            Remove(usize),
        }
        let mut todo = None;
        egui::ScrollArea::vertical()
            .max_height(220.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (i, p) in self.pieces.iter_mut().enumerate() {
                    let name = self
                        .sources
                        .iter()
                        .find(|(t, _)| t.id == p.take)
                        .map_or("a removed take".to_string(), |(t, _)| {
                            format!("#{}", t.number)
                        });
                    let on = self.editing == Some(i);
                    ui.horizontal(|ui| {
                        // A fixed column, so the pieces' names line up.
                        ui.allocate_ui_with_layout(
                            egui::vec2(190.0, ui.spacing().interact_size.y),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.set_width(190.0);
                                if i > 0 {
                                    for (j, label, about) in JOINS {
                                        let r = ui
                                            .selectable_label(p.join == j, label)
                                            .on_hover_text(about);
                                        r.widget_info(|| {
                                            egui::WidgetInfo::selected(
                                                egui::Role::RadioButton,
                                                true,
                                                p.join == j,
                                                format!("{label} into piece {}", i + 1),
                                            )
                                        });
                                        if r.clicked() {
                                            p.join = j;
                                        }
                                    }
                                } else {
                                    ui.label(RichText::new("first").color(theme::DUST));
                                }
                            },
                        );
                        ui.add_space(12.0);
                        let text = format!(
                            "{}. {name}  {:.2}–{:.2}s  ({:.1}s)",
                            i + 1,
                            p.from,
                            p.to,
                            p.to - p.from
                        );
                        let color = if on { theme::TUNGSTEN } else { theme::PAPER };
                        if ui
                            .add(egui::Button::new(RichText::new(text).color(color)).frame(false))
                            .on_hover_text("play it, and change where it starts and ends")
                            .clicked()
                        {
                            todo = Some(Do::Edit(i));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let rm = crate::remove_button(ui);
                            rm.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::Role::Button,
                                    true,
                                    format!("Remove piece {}", i + 1),
                                )
                            });
                            if rm.clicked() {
                                todo = Some(Do::Remove(i));
                            }
                            if i > 0 && ui.small_button("Earlier").clicked() {
                                todo = Some(Do::Up(i));
                            }
                        });
                    });
                }
            });
        let total: f64 = self.pieces.iter().map(|p| p.to - p.from).sum();
        hint(ui, &format!("about {total:.1}s in all"));
        match todo {
            Some(Do::Edit(i)) => {
                self.editing = Some(i);
                let p = self.pieces[i].clone();
                if let Some(s) = self.sources.iter().position(|(t, _)| t.id == p.take) {
                    if s != self.source {
                        self.source = s;
                        self.pause(audio);
                        self.deck = None;
                        self.go_to = Some(p.from);
                    } else if let Some(d) = &mut self.deck {
                        d.seek(p.from, audio);
                    }
                }
            }
            Some(Do::Up(i)) => {
                self.pieces.swap(i - 1, i);
                self.editing = None;
            }
            Some(Do::Remove(i)) => {
                self.pieces.remove(i);
                self.editing = None;
            }
            None => {}
        }
    }

    fn make_row(&mut self, ui: &mut egui::Ui, ff: &Ffmpeg, project: &Project) {
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.colormatch, "Match colours at the joins")
                .on_hover_text("each piece's colours are brought to the end of the one before");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let busy = matches!(*self.making.lock().unwrap(), Making::Busy);
                if busy {
                    ui.spinner();
                    ui.weak("making it…");
                    return;
                }
                let r = ui
                    .add_enabled(!self.pieces.is_empty(), egui::Button::new("Make take"))
                    .on_hover_text("a new take of this shot; the takes it's cut from stay");
                if r.clicked() {
                    self.make(ui.ctx(), ff, project);
                }
            });
        });
    }

    fn make(&mut self, ctx: &egui::Context, ff: &Ffmpeg, project: &Project) {
        *self.making.lock().unwrap() = Making::Busy;
        let (p, ff, shot) = (project.clone(), ff.clone(), self.shot);
        let pieces = self.pieces.clone();
        let opts = shotline_media::splice::Options {
            colormatch: self.colormatch,
            ..Default::default()
        };
        let making = self.making.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let work = p.root.join(".shotline/cache/splice");
            let out = p.splice_take(shot, &pieces, |sources, out| {
                shotline_media::splice::splice(&ff, &media_pieces(sources), &opts, &work, out)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            });
            *making.lock().unwrap() = Making::Done(out.map_err(|e| e.to_string()));
            ctx.request_repaint();
        });
    }
}

/// Seconds to type in; unset shows `default`.
fn seconds(ui: &mut egui::Ui, name: &str, v: &mut Option<f64>, default: f64, total: f64) -> bool {
    let mut x = v.unwrap_or(default);
    let r = ui.add(
        egui::DragValue::new(&mut x)
            .range(0.0..=total.max(0.0))
            .speed(0.04)
            .max_decimals(2)
            .suffix("s"),
    );
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::SpinButton, true, name));
    if r.changed() {
        *v = Some(x);
        return true;
    }
    false
}

pub(crate) fn media_pieces(sources: &[(PathBuf, Piece)]) -> Vec<shotline_media::splice::Piece> {
    use shotline_media::splice::Join as J;
    sources
        .iter()
        .map(|(path, p)| shotline_media::splice::Piece {
            path: path.clone(),
            from: p.from,
            to: p.to,
            join: match p.join {
                Join::Cut => J::Cut,
                Join::Fade => J::Fade,
                Join::Smooth => J::Smooth,
            },
        })
        .collect()
}
