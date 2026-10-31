//! The desktop app: browse a film's story, elements, shots, takes and costs, and play takes and
//! the cut. It reads and writes the project folder through shotline-core like the CLI does, so
//! both always agree.

mod edit;
mod git;
mod player;
mod theme;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use eframe::egui::{self, RichText};
use shotline_core::ledger::money;
use shotline_core::ledger::report::{self, By, Row, Summary};
use shotline_core::{
    Chapter, CutShot, Element, Git, JobStatus, Kind, Project, Shot, ShotRef, Take, Transition,
    UserConfig,
};
use shotline_media::Ffmpeg;

use edit::{Action, Form, History, LineForm};
use git::{Dialog, Repo};
use player::{Audio, Deck, Player};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Story,
    Elements,
    Shots,
    Cut,
    Cost,
}

impl Tab {
    const ALL: [(Tab, &str); 5] = [
        (Tab::Story, "Story"),
        (Tab::Elements, "Elements"),
        (Tab::Shots, "Shots"),
        (Tab::Cut, "Cut"),
        (Tab::Cost, "Cost"),
    ];

    fn name(self) -> &'static str {
        Tab::ALL.iter().find(|(t, _)| *t == self).unwrap().1
    }

    fn named(name: &str) -> Option<Tab> {
        Tab::ALL.iter().find(|(_, n)| *n == name).map(|(t, _)| *t)
    }
}

/// Everything shown, read from disk at once so a frame never touches the filesystem.
#[derive(Default)]
struct Film {
    story: String,
    style: String,
    elements: Vec<(Element, Vec<PathBuf>)>,
    chapters: Vec<(Chapter, Vec<(ShotRef, Shot)>)>,
    cut: Vec<CutShot>,
    cost: Option<(Summary, Vec<Row>)>,
    /// How many takes each shot has.
    takes: HashMap<ShotRef, usize>,
}

pub struct App {
    project: Project,
    user_dir: PathBuf,
    viewer: Option<PathBuf>,
    ff: Option<Ffmpeg>,
    audio: Option<Audio>,
    /// Opening the sound device is tried once, on first play.
    audio_tried: bool,
    deck: Option<Deck>,
    film: Film,
    tab: Tab,
    shot: Option<ShotRef>,
    takes: Vec<Take>,
    status: Option<(String, bool)>,
    thumbs: Thumbs,
    /// The tab and shot last written to the place file, when the app keeps one.
    place: Option<(Tab, Option<ShotRef>)>,
    themed: bool,
    form: Form,
    history: History,
    git: Option<Git>,
    repo: Repo,
    /// When `repo` was last read, in egui's seconds.
    repo_at: f64,
    dialog: Option<Dialog>,
}

const VIEWERS: [&str; 2] = ["mpv", "ffplay"];

fn on_path(exe: &str) -> Option<PathBuf> {
    let name = format!("{exe}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&name))
        .find(|p| p.is_file())
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Anchors and frames are often 2-4K, past what some GPUs take as one texture, so they're shrunk
/// once when first shown.
#[derive(Default)]
struct Thumbs(HashMap<PathBuf, Option<egui::TextureHandle>>);

const THUMB: u32 = 512;

impl Thumbs {
    fn show(&mut self, ui: &mut egui::Ui, path: &Path, height: f32, name: &str) {
        let tex = self.0.entry(path.to_path_buf()).or_insert_with(|| {
            let img = image::open(path).ok()?.thumbnail(THUMB, THUMB).to_rgba8();
            let size = [img.width() as usize, img.height() as usize];
            let pixels = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
            Some(ui.ctx().load_texture(name, pixels, Default::default()))
        });
        match tex {
            Some(t) => {
                ui.add(egui::Image::new(&*t).max_height(height).alt_text(name));
            }
            None => {
                ui.weak(format!("can't show {name}"));
            }
        }
    }
}

impl App {
    /// `user_dir` is where the user's config and cross-film ledger live, normally `user_dir()`.
    pub fn new(project: Project, user_dir: PathBuf) -> App {
        let mut app = App {
            project,
            user_dir,
            viewer: VIEWERS.iter().find_map(|p| on_path(p)),
            ff: Ffmpeg::find().ok(),
            audio: None,
            audio_tried: false,
            deck: None,
            film: Film::default(),
            tab: Tab::Shots,
            shot: None,
            takes: vec![],
            status: None,
            thumbs: Thumbs::default(),
            place: None,
            themed: false,
            form: Form::default(),
            history: History::default(),
            git: Git::find(),
            repo: Repo::None,
            repo_at: 0.0,
            dialog: None,
        };
        app.reload();
        app
    }

    /// What "Open" runs with the file as its last argument, to watch a take full size.
    pub fn with_viewer(mut self, viewer: Option<PathBuf>) -> App {
        self.viewer = viewer;
        self
    }

    pub fn with_git(mut self, git: Option<Git>) -> App {
        self.git = git;
        self.refresh_repo();
        self
    }

    fn place_file(&self) -> PathBuf {
        self.project.root.join(".shotline/cache/ui-place")
    }

    /// Opens on the tab and shot it was last on, and keeps track from here on, so a restart
    /// lands in the same place.
    pub fn remember_place(mut self) -> App {
        let text = read(&self.place_file());
        for line in text.lines() {
            match line.split_once('=') {
                Some(("tab", t)) => self.tab = Tab::named(t).unwrap_or(self.tab),
                Some(("shot", r)) => {
                    if let Ok(r) = r.parse() {
                        self.open_shot(r);
                    }
                }
                _ => {}
            }
        }
        self.place = Some((self.tab, self.shot));
        self
    }

    fn keep_place(&mut self) {
        let now = (self.tab, self.shot);
        if self.place.is_none_or(|p| p == now) {
            return;
        }
        self.place = Some(now);
        let mut text = format!("tab={}\n", self.tab.name());
        if let Some(r) = self.shot {
            text += &format!("shot={r}\n");
        }
        let file = self.place_file();
        let _ = std::fs::create_dir_all(file.parent().unwrap());
        let _ = std::fs::write(file, text);
    }

    /// Plays without sound and never touches the sound device, for tests.
    pub fn silent(mut self) -> App {
        self.audio_tried = true;
        self
    }

    pub fn is_playing(&self) -> bool {
        self.deck.as_ref().is_some_and(Deck::is_playing)
    }

    fn reload(&mut self) {
        self.thumbs = Thumbs::default();
        self.refresh_repo();
        if let Err(e) = self.load() {
            self.error(e.to_string());
        }
        self.fill_form();
    }

    /// The open shot as last read from disk.
    fn open(&self) -> Option<(ShotRef, Shot)> {
        let r = self.shot?;
        self.film
            .chapters
            .iter()
            .flat_map(|(_, s)| s)
            .find(|(x, _)| *x == r)
            .cloned()
    }

    fn load(&mut self) -> anyhow::Result<()> {
        if let Ok(p) = Project::open(&self.project.root) {
            self.project = p.with_clock(self.project.clock.clone());
        }
        let p = &self.project;
        let mut elements = vec![];
        for el in p.elements()? {
            let anchors = p.anchors(&el)?;
            elements.push((el, anchors));
        }
        let mut takes: HashMap<ShotRef, HashSet<_>> = HashMap::new();
        for j in p.jobs()? {
            if let (Some(s), Some(t)) = (j.shot, j.take) {
                takes.entry(s).or_default().insert(t);
            }
        }
        let mut chapters = vec![];
        for c in p.chapters()? {
            let shots = p.shots(Some(&c.id()))?;
            chapters.push((c, shots));
        }
        self.film = Film {
            story: read(&p.root.join("story.md")),
            style: p.style()?,
            elements,
            chapters,
            cut: p.cut(&p.timeline()?)?,
            cost: self.cost()?,
            takes: takes.into_iter().map(|(s, t)| (s, t.len())).collect(),
        };
        self.load_takes()
    }

    fn cost(&self) -> anyhow::Result<Option<(Summary, Vec<Row>)>> {
        let p = &self.project;
        let user = UserConfig::load(&self.user_dir)?;
        let b = p.books(&self.user_dir, &user, &[])?;
        let entries = b.ledger.entries()?;
        if entries.is_empty() {
            return Ok(None);
        }
        let s = report::summary(&entries, &b.currency, &b.rates, &p.selected_take_ids()?)?;
        let rows = report::group(&entries, By::Shot, &b.currency, &b.rates, &b.tz)?;
        Ok(Some((s, rows)))
    }

    fn load_takes(&mut self) -> anyhow::Result<()> {
        self.takes = match self.shot {
            Some(r) => self.project.takes(r)?,
            None => vec![],
        };
        Ok(())
    }

    fn error(&mut self, msg: String) {
        self.status = Some((msg, true));
    }

    fn info(&mut self, msg: String) {
        self.status = Some((msg, false));
    }

    fn open_shot(&mut self, r: ShotRef) {
        self.flush();
        self.shot = Some(r);
        self.fill_form();
        if let Err(e) = self.load_takes() {
            self.error(e.to_string());
        }
    }

    fn open_file(&mut self, file: &Path) {
        let Some(viewer) = &self.viewer else {
            return;
        };
        let mut cmd = Command::new(viewer);
        if viewer.file_stem().is_some_and(|s| s == "ffplay") {
            cmd.args(["-autoexit", "-loglevel", "error"]);
        }
        match cmd.arg(file).spawn() {
            Ok(_) => self.info(format!("opened {}", self.project.rel(file))),
            Err(e) => self.error(format!("can't run {}: {e}", viewer.display())),
        }
    }

    fn player(&mut self, label: &str, files: &[PathBuf]) -> Option<Player> {
        let Some(ff) = &self.ff else {
            self.error("can't play without ffmpeg; install it or set SHOTLINE_FFMPEG".into());
            return None;
        };
        if !self.audio_tried {
            self.audio_tried = true;
            self.audio = Audio::open();
        }
        match Player::new(ff, label, files) {
            Ok(p) => Some(p),
            Err(e) => {
                self.error(format!("can't play {label}: {e}"));
                None
            }
        }
    }

    fn play(&mut self, label: &str, files: &[PathBuf], looping: bool) {
        if let Some(d) = &mut self.deck {
            d.pause(self.audio.as_ref());
        }
        self.deck = None;
        if let Some(p) = self.player(label, files) {
            self.deck = Some(Deck::new(label, p, looping));
        }
    }

    fn compare(&mut self, label: &str, file: PathBuf) {
        if let Some(p) = self.player(label, &[file])
            && let Some(d) = &mut self.deck
        {
            d.compare(p, self.audio.as_ref());
        }
    }

    fn deck(&mut self, ui: &mut egui::Ui) {
        if let Some(d) = &mut self.deck {
            if !d.show(ui, self.audio.as_ref()) {
                self.deck = None;
            }
            ui.separator();
        }
    }

    /// The whole window. Kept apart from eframe so tests can drive it.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        if !self.themed {
            self.themed = true;
            theme::apply(ui.ctx());
        }
        let tab = self.tab;
        egui::Panel::top("bar")
            .frame(
                egui::Frame::new()
                    .fill(theme::RACK)
                    .inner_margin(egui::vec2(16.0, 0.0)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_height(40.0);
                    ui.label(
                        RichText::new(&self.project.config.name)
                            .text_style(egui::TextStyle::Heading)
                            .color(theme::PAPER),
                    );
                    ui.add_space(20.0);
                    ui.spacing_mut().item_spacing.x = 20.0;
                    for (t, name) in Tab::ALL {
                        if tab_button(ui, self.tab == t, name).clicked() {
                            self.tab = t;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button("Reload")
                            .on_hover_text("read the project folder again (F5)")
                            .clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::F5))
                        {
                            self.status = None;
                            self.reload();
                        }
                    });
                });
            });
        let (msg, err) = self.status.clone().unwrap_or_default();
        egui::Panel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(theme::RACK)
                    .inner_margin(egui::vec2(16.0, 0.0)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_height(36.0);
                    let color = if err { theme::FLAG } else { theme::GRAPHITE };
                    ui.label(RichText::new(msg).color(color));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let cmd = if ui.ctx().os() == egui::os::OperatingSystem::Mac {
                            "Cmd"
                        } else {
                            "Ctrl"
                        };
                        // Right to left, so the last added sits leftmost.
                        self.git_button(ui);
                        let redo = self.history.redo_what();
                        if history_button(ui, "Redo", redo, &format!("{cmd}+Shift+Z")) {
                            self.redo();
                        }
                        let undo = self.history.undo_what();
                        if history_button(ui, "Undo", undo, &format!("{cmd}+Z")) {
                            self.undo();
                        }
                    });
                });
            });
        if self.tab != tab
            && !matches!(self.tab, Tab::Shots | Tab::Cut)
            && let Some(d) = &mut self.deck
        {
            d.pause(self.audio.as_ref());
        }
        match self.tab {
            Tab::Story => self.story(ui),
            Tab::Elements => self.elements(ui),
            Tab::Shots => self.shots(ui),
            Tab::Cut => self.cut(ui),
            Tab::Cost => self.costs(ui),
        }
        self.git_dialog(ui);
        // Files change outside the app too, from the CLI or the co-director.
        let now = ui.input(|i| i.time);
        if now - self.repo_at > 5.0 {
            self.repo_at = now;
            self.refresh_repo();
        }
        self.keep_place();
        if ui.memory(|m| m.focused().is_none()) && ui.ctx().dragged_id().is_none() {
            self.flush();
            // In a text box these keys undo typing instead.
            let key = |shift| {
                let m = egui::Modifiers::COMMAND;
                let m = if shift { m | egui::Modifiers::SHIFT } else { m };
                ui.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(m, egui::Key::Z)))
            };
            if key(true) {
                self.redo();
            } else if key(false) {
                self.undo();
            }
        }
    }

    fn story(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.label(&self.film.story);
                ui.add_space(12.0);
                ui.heading("Style");
                if self.film.style.is_empty() {
                    ui.weak("style.md is empty");
                } else {
                    ui.label(&self.film.style);
                }
            });
        });
    }

    fn elements(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                if self.film.elements.is_empty() {
                    ui.weak("no elements yet");
                }
                for (el, anchors) in &self.film.elements {
                    ui.horizontal(|ui| {
                        ui.strong(format!("@{}", el.handle));
                        ui.label(&el.name);
                        ui.weak(format!("{:?}", el.kind).to_lowercase());
                        if let Some(v) = &el.voice {
                            ui.weak(format!("voice: {}", v.plugin));
                        }
                    });
                    if !el.description.is_empty() {
                        ui.label(&el.description);
                    }
                    if anchors.is_empty() {
                        ui.weak("no anchor images");
                    } else {
                        ui.horizontal_wrapped(|ui| {
                            for a in anchors {
                                self.thumbs.show(ui, a, 160.0, &self.project.rel(a));
                            }
                        });
                    }
                    ui.separator();
                }
            });
        });
    }

    fn shots(&mut self, ui: &mut egui::Ui) {
        let mut open = None;
        egui::Panel::left("shots")
            .resizable(true)
            .default_size(260.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::RACK)
                    .inner_margin(egui::vec2(8.0, 12.0)),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.film.chapters.is_empty() {
                        ui.weak("No chapters yet. Add one with shotline chapter add");
                    }
                    for (c, shots) in &self.film.chapters {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(format!("{} {}", c.id(), c.title))
                                    .family(egui::FontFamily::Name(theme::SEMIBOLD.into()))
                                    .color(theme::PAPER),
                            );
                        });
                        for (r, s) in shots {
                            let takes = self.film.takes.get(r).copied().unwrap_or(0);
                            if shot_row(ui, *r, s, takes, self.shot == Some(*r)).clicked() {
                                open = Some(*r);
                            }
                        }
                        ui.add_space(8.0);
                    }
                });
            });
        if let Some(r) = open {
            self.open_shot(r);
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::BAY)
                    .inner_margin(egui::vec2(28.0, 20.0)),
            )
            .show(ui, |ui| match self.open() {
                Some((r, s)) => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        self.deck(ui);
                        self.shot_detail(ui, r, &s)
                    });
                }
                None => {
                    ui.weak("Pick a shot on the left.");
                }
            });
    }

    fn shot_detail(&mut self, ui: &mut egui::Ui, r: ShotRef, s: &Shot) {
        let mut acts = vec![];
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(r.to_string())
                    .font(theme::slate())
                    .color(theme::PAPER),
            );
            ui.add_space(8.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.form.title)
                    .font(egui::TextStyle::Heading)
                    .text_color(theme::GRAPHITE)
                    .hint_text("Add a title")
                    .frame(egui::Frame::NONE)
                    .desired_width(ui.available_width()),
            );
        });
        ui.add_space(14.0);
        let side = 240.0;
        ui.horizontal_top(|ui| {
            let w = (ui.available_width() - side - 32.0).clamp(280.0, 620.0);
            ui.vertical(|ui| {
                ui.set_width(w);
                self.script(ui, r, s, &mut acts);
            });
            ui.add_space(32.0);
            ui.vertical(|ui| {
                ui.set_width(side);
                self.facts(ui, r, s);
            });
        });
        ui.add_space(20.0);
        ui.separator();
        ui.add_space(8.0);
        self.take_list(ui, r, s, &mut acts);
        self.apply(acts);
    }

    /// Plot, prompt and lines: what the shot says.
    fn script(&mut self, ui: &mut egui::Ui, r: ShotRef, s: &Shot, acts: &mut Vec<Action>) {
        let f = &mut self.form;
        text_field(
            ui,
            "Plot",
            &mut f.plot,
            "What happens in the shot, in any language",
        );
        ui.add_space(14.0);
        text_field(
            ui,
            "Prompt",
            &mut f.prompt,
            "What the video model is sent, in English, naming elements as @handles",
        );
        if f.prompt.is_empty() && !f.plot.is_empty() {
            hint(
                ui,
                &format!("or draft one from the plot with shotline shot draft {r}"),
            );
        }
        ui.add_space(14.0);
        text_field(
            ui,
            "Frame prompt",
            &mut f.frame_prompt,
            "Same as the prompt",
        );
        ui.add_space(14.0);
        let lines = caption(ui, "Lines").id;
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
                "Only cast speak, and there's no cast yet. Add some with shotline element add cast",
            );
            return;
        }
        let f = &mut self.form;
        if f.new.who.is_empty() {
            f.new.who = s.lines.last().map_or(cast[0].clone(), |l| l.who.clone());
        }
        for (i, l) in f.lines.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                line_fields(ui, i, l, &cast, 80.0);
                if quiet_button(ui, "Remove").clicked() {
                    acts.push(Action::RemoveLine(r, i));
                }
            });
        }
        ui.horizontal(|ui| {
            let text = line_fields(ui, usize::MAX, &mut f.new, &cast, 90.0).labelled_by(lines);
            let enter = text.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.button("Add line").clicked() || enter) && !f.new.text.trim().is_empty() {
                acts.push(Action::AddLine(r, f.new.clone()));
            }
        });
    }

    /// The storyboard frame and the shot's settings.
    fn facts(&mut self, ui: &mut egui::Ui, r: ShotRef, s: &Shot) {
        match &s.frame {
            Some(f) => self.thumbs.show(ui, &self.project.root.join(f), 150.0, f),
            None => hint(
                ui,
                &format!("No storyboard frame. Make one with shotline frame {r}"),
            ),
        }
        ui.add_space(12.0);
        let f = &mut self.form;
        egui::Grid::new("facts")
            .num_columns(2)
            .spacing(egui::vec2(12.0, 8.0))
            .show(ui, |ui| {
                ui.weak("Length");
                ui.add(
                    egui::DragValue::new(&mut f.duration)
                        .range(0.5..=60.0)
                        .speed(0.1)
                        .max_decimals(2)
                        .suffix(" s"),
                );
                ui.end_row();
                ui.weak("Starts");
                ui.vertical(|ui| {
                    ui.radio_value(&mut f.transition, Transition::Cut, "on a cut");
                    ui.radio_value(
                        &mut f.transition,
                        Transition::Continue,
                        "from the shot before",
                    );
                });
                ui.end_row();
                if let Some(rc) = &s.recipe {
                    ui.weak("Recipe");
                    ui.label(rc);
                    ui.end_row();
                }
                if let Some(m) = &s.model {
                    ui.weak("Model");
                    ui.label(format!(
                        "{} {}",
                        m.plugin.as_deref().unwrap_or("-"),
                        m.id.as_deref().unwrap_or("-")
                    ));
                    ui.end_row();
                }
            });
    }

    fn take_list(&mut self, ui: &mut egui::Ui, r: ShotRef, s: &Shot, acts: &mut Vec<Action>) {
        ui.label(
            RichText::new("Takes")
                .text_style(egui::TextStyle::Heading)
                .color(theme::PAPER),
        );
        ui.add_space(4.0);
        if self.takes.is_empty() {
            hint(
                ui,
                &format!("No takes yet. Shoot one with shotline take {r}"),
            );
            return;
        }
        let mut play = None;
        let mut compare = None;
        let mut open = None;
        // Comparing makes sense while one take of this shot is up.
        let single = self
            .deck
            .as_ref()
            .filter(|d| d.players.len() == 1)
            .map(|d| d.players[0].label.clone())
            .filter(|l| l.starts_with(&format!("{r}#")));
        egui::Grid::new("takes")
            .num_columns(5)
            .spacing(egui::vec2(20.0, 8.0))
            .show(ui, |ui| {
                for t in &self.takes {
                    let selected = s.selected_take.as_deref() == Some(t.id.to_string().as_str());
                    let n = t.number;
                    if selected {
                        ui.label(RichText::new(format!("• #{n}")).color(theme::TUNGSTEN))
                            .on_hover_text("in the cut");
                    } else {
                        ui.label(RichText::new(format!("   #{n}")).color(theme::GRAPHITE));
                    }
                    let status = t.status();
                    let color = match status {
                        JobStatus::Done => theme::GRAPHITE,
                        JobStatus::Failed => theme::FLAG,
                        _ => theme::DUST,
                    };
                    ui.label(RichText::new(status.to_string()).color(color));
                    let cost: Vec<String> = t
                        .costs()
                        .iter()
                        .map(|c| format!("{} {}", money(c.amount), c.unit))
                        .collect();
                    ui.label(cost.join(" + "));
                    let last = t.jobs.last();
                    ui.weak(last.map_or(String::new(), |j| format!("{} {}", j.plugin, j.model)));
                    match t.output().and_then(|j| {
                        j.outputs
                            .first()
                            .map(|f| self.project.job_dir(j.id).join(f))
                    }) {
                        Some(file) => {
                            let label = t.to_string();
                            ui.horizontal(|ui| {
                                if act(ui, "Play", n).clicked() {
                                    play = Some((label.clone(), file.clone()));
                                }
                                if single.as_ref().is_some_and(|l| *l != label)
                                    && act(ui, "Compare", n).clicked()
                                {
                                    compare = Some((label.clone(), file.clone()));
                                }
                                if self.viewer.is_some()
                                    && act(ui, "Open", n)
                                        .on_hover_text("full size in mpv or ffplay")
                                        .clicked()
                                {
                                    open = Some(file.clone());
                                }
                                if !selected
                                    && act(ui, "Select", n)
                                        .on_hover_text("use this take in the cut")
                                        .clicked()
                                {
                                    acts.push(Action::Select(t.clone()));
                                }
                            });
                        }
                        None => {
                            let why = last.and_then(|j| j.error.clone()).unwrap_or_default();
                            ui.weak(short(&why, 60));
                        }
                    }
                    ui.end_row();
                }
            });
        if let Some((label, f)) = play {
            self.play(&label, &[f], true);
        }
        if let Some((label, f)) = compare {
            self.compare(&label, f);
        }
        if let Some(f) = open {
            self.open_file(&f);
        }
    }

    fn cut(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.deck(ui);
                if self.film.cut.is_empty() {
                    ui.weak("no shots yet");
                    return;
                }
                let files: Vec<PathBuf> = self
                    .film
                    .cut
                    .iter()
                    .filter_map(|c| c.video.clone())
                    .collect();
                let missing = self.film.cut.len() - files.len();
                ui.horizontal(|ui| {
                    if !files.is_empty() && ui.button("Play the cut").clicked() {
                        self.play("the cut", &files, false);
                    }
                    if missing > 0 {
                        ui.weak(format!("{missing} shots have no take yet and are left out"));
                    }
                });
                ui.weak("Takes as they are: colour matching and music only happen in a render.");
                ui.add_space(6.0);
                egui::Grid::new("cut").striped(true).show(ui, |ui| {
                    for c in &self.film.cut {
                        ui.label(c.shot.to_string());
                        ui.weak(match c.transition {
                            Transition::Cut => "cut",
                            Transition::Continue => "continue",
                        });
                        match &c.take {
                            Some(t) if c.video.is_some() => ui.label(format!("#{}", t.number)),
                            _ => ui.weak("no take"),
                        };
                        ui.end_row();
                    }
                });
            });
        });
    }

    fn costs(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            let Some((s, rows)) = &self.film.cost else {
                ui.weak("nothing spent yet");
                return;
            };
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
            ui.add_space(12.0);
            egui::Grid::new("by shot").striped(true).show(ui, |ui| {
                ui.strong("shot");
                ui.strong(cur.as_str());
                ui.strong("jobs");
                ui.end_row();
                for r in rows {
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
        });
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }
}

fn short(s: &str, n: usize) -> String {
    let line = s.lines().next().unwrap_or("");
    if line.chars().count() <= n {
        line.to_string()
    } else {
        format!("{}…", line.chars().take(n).collect::<String>())
    }
}

/// A tab in the top bar: text only, with an amber rule under the open one.
fn tab_button(ui: &mut egui::Ui, open: bool, name: &str) -> egui::Response {
    let color = if open { theme::PAPER } else { theme::DUST };
    let r = ui.add(egui::Button::new(RichText::new(name).color(color)).frame(false));
    if open {
        let rect = r.rect;
        ui.painter().hline(
            rect.x_range(),
            ui.max_rect().bottom() - 1.0,
            egui::Stroke::new(2.0, theme::TUNGSTEN),
        );
    } else if r.hovered() {
        ui.painter().hline(
            r.rect.x_range(),
            ui.max_rect().bottom() - 1.0,
            egui::Stroke::new(1.0, theme::LINE),
        );
    }
    r
}

/// One shot in the list: number, title, how many takes, and an amber dot once one is in the cut.
fn shot_row(ui: &mut egui::Ui, r: ShotRef, s: &Shot, takes: usize, open: bool) -> egui::Response {
    let title = if s.title.is_empty() {
        &s.prompt
    } else {
        &s.title
    };
    let title = title.lines().next().unwrap_or("");
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 26.0), egui::Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::Button, true, open, format!("{r}  {title}"))
    });
    let p = ui.painter();
    if open {
        p.rect_filled(rect, 3.0, theme::RAISED);
        p.rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            0.0,
            theme::TUNGSTEN,
        );
    } else if resp.hovered() {
        p.rect_filled(rect, 3.0, theme::BAY);
    }
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mid = rect.center().y;
    let num = p.layout_no_wrap(r.to_string(), font.clone(), theme::DUST);
    let x = rect.left() + 12.0;
    p.galley(
        egui::pos2(x, mid - num.size().y / 2.0),
        num.clone(),
        theme::DUST,
    );

    let mut right = rect.right() - 8.0;
    if s.selected_take.is_some() {
        p.circle_filled(egui::pos2(right - 3.0, mid), 3.5, theme::TUNGSTEN);
        right -= 14.0;
    }
    if takes > 0 {
        let n = p.layout_no_wrap(
            takes.to_string(),
            egui::TextStyle::Small.resolve(ui.style()),
            theme::DUST,
        );
        right -= n.size().x;
        p.galley(egui::pos2(right, mid - n.size().y / 2.0), n, theme::DUST);
        right -= 8.0;
    }
    let left = x + num.size().x + 10.0;
    let mut job = egui::text::LayoutJob::simple_singleline(
        title.to_string(),
        font,
        if open { theme::PAPER } else { theme::GRAPHITE },
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width((right - left).max(0.0));
    let t = p.layout_job(job);
    p.galley(egui::pos2(left, mid - t.size().y / 2.0), t, theme::GRAPHITE);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The small label over a part of a shot.
fn caption(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.label(RichText::new(text).small().color(theme::DUST))
}

/// A labelled text box that grows with its text.
fn text_field(ui: &mut egui::Ui, label: &str, text: &mut String, hint: &str) -> egui::Response {
    let cap = caption(ui, label).id;
    ui.add(
        egui::TextEdit::multiline(text)
            .hint_text(RichText::new(hint).color(theme::DUST))
            .desired_rows(1)
            .desired_width(f32::INFINITY)
            .margin(egui::vec2(8.0, 6.0)),
    )
    .labelled_by(cap)
}

/// When a line starts, who says it and what. Returns the text box.
fn line_fields(
    ui: &mut egui::Ui,
    i: usize,
    l: &mut LineForm,
    cast: &[String],
    after: f32,
) -> egui::Response {
    ui.add(
        egui::TextEdit::singleline(&mut l.at)
            .hint_text(RichText::new("next").color(theme::DUST))
            .desired_width(64.0),
    )
    .on_hover_text("seconds into the shot; empty follows the line before");
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
    ui.add(
        egui::TextEdit::singleline(&mut l.text)
            .hint_text(RichText::new("What they say").color(theme::DUST))
            .desired_width(ui.available_width() - after),
    )
}

/// What to do about something missing.
fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).color(theme::DUST));
}

fn history_button(ui: &mut egui::Ui, text: &str, what: Option<String>, keys: &str) -> bool {
    what.is_some_and(|w| {
        ui.button(text)
            .on_hover_text(format!("{text} {w} ({keys})"))
            .clicked()
    })
}

fn quiet_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).color(theme::DUST)).frame(false))
}

/// A take's button: the number is in the row already, but screen readers and tests get it too.
fn act(ui: &mut egui::Ui, text: &str, take: usize) -> egui::Response {
    let r = ui.button(text);
    r.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{text} #{take}"))
    });
    r
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

pub fn run(project: Project, user_dir: PathBuf) -> anyhow::Result<()> {
    let title = format!("{} - shotline", project.config.name);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        &title,
        options,
        Box::new(move |_| Ok(Box::new(App::new(project, user_dir).remember_place()))),
    )
    .map_err(|e| anyhow::anyhow!("can't open the window: {e}"))
}
