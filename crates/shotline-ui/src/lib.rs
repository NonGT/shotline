//! The desktop app: browse a film's story, elements, shots, takes and costs, and play takes and
//! the cut. It reads and writes the project folder through shotline-core like the CLI does, so
//! both always agree.

mod player;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use eframe::egui::{self, Color32, RichText};
use shotline_core::ledger::money;
use shotline_core::ledger::report::{self, By, Row, Summary};
use shotline_core::{
    Chapter, CutShot, Element, JobStatus, Project, Shot, ShotRef, Take, Transition, UserConfig,
};
use shotline_media::Ffmpeg;

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
        };
        app.reload();
        app
    }

    /// What "Open" runs with the file as its last argument, to watch a take full size.
    pub fn with_viewer(mut self, viewer: Option<PathBuf>) -> App {
        self.viewer = viewer;
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
        if let Err(e) = self.load() {
            self.error(e.to_string());
        }
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
        self.shot = Some(r);
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

    fn select(&mut self, take: &Take) {
        match self
            .project
            .select_take(take.shot, &take.number.to_string())
        {
            Ok(t) => {
                self.info(format!("{t} selected"));
                self.reload();
            }
            Err(e) => self.error(e.to_string()),
        }
    }

    /// The whole window. Kept apart from eframe so tests can drive it.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let tab = self.tab;
        egui::Panel::top("bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(&self.project.config.name);
                ui.separator();
                for (tab, name) in Tab::ALL {
                    ui.selectable_value(&mut self.tab, tab, name);
                }
                ui.separator();
                if ui.button("Reload").clicked() || ui.input(|i| i.key_pressed(egui::Key::F5)) {
                    self.status = None;
                    self.reload();
                }
            });
        });
        if let Some((msg, err)) = &self.status {
            let (msg, err) = (msg.clone(), *err);
            egui::Panel::bottom("status").show(ui, |ui| {
                let color = if err {
                    ui.visuals().error_fg_color
                } else {
                    ui.visuals().weak_text_color()
                };
                ui.label(RichText::new(msg).color(color));
            });
        }
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
        self.keep_place();
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
            .default_size(240.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.film.chapters.is_empty() {
                        ui.weak("no chapters yet");
                    }
                    for (c, shots) in &self.film.chapters {
                        ui.strong(format!("{} {}", c.id(), c.title));
                        for (r, s) in shots {
                            let title = if s.title.is_empty() {
                                &s.prompt
                            } else {
                                &s.title
                            };
                            // Shots still waiting for a take are dimmed.
                            let mut text = RichText::new(format!("{r}  {}", short(title, 32)));
                            if s.selected_take.is_none() {
                                text = text.color(ui.visuals().weak_text_color());
                            }
                            if ui.selectable_label(self.shot == Some(*r), text).clicked() {
                                open = Some(*r);
                            }
                        }
                    }
                });
            });
        if let Some(r) = open {
            self.open_shot(r);
        }
        egui::CentralPanel::default().show(ui, |ui| {
            let found = self.shot.and_then(|r| {
                self.film
                    .chapters
                    .iter()
                    .flat_map(|(_, s)| s)
                    .find(|(x, _)| *x == r)
                    .cloned()
            });
            match found {
                Some((r, s)) => {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        self.deck(ui);
                        self.shot_detail(ui, r, &s)
                    });
                }
                None => {
                    ui.weak("pick a shot");
                }
            }
        });
    }

    fn shot_detail(&mut self, ui: &mut egui::Ui, r: ShotRef, s: &Shot) {
        ui.heading(if s.title.is_empty() {
            r.to_string()
        } else {
            format!("{r}  {}", s.title)
        });
        let duration = s.duration.unwrap_or(self.project.config.duration);
        let transition = match s.transition {
            Transition::Cut => "cut",
            Transition::Continue => "continue",
        };
        let mut facts = vec![format!("{}s", money(duration)), transition.to_string()];
        if let Some(rc) = &s.recipe {
            facts.push(format!("recipe {rc}"));
        }
        if let Some(m) = &s.model {
            facts.push(format!(
                "{} {}",
                m.plugin.as_deref().unwrap_or("-"),
                m.id.as_deref().unwrap_or("-")
            ));
        }
        ui.weak(facts.join(" · "));
        if !s.plot.is_empty() {
            ui.label(&s.plot);
        }
        ui.add_space(6.0);
        ui.label(RichText::new(&s.prompt).monospace());
        for l in &s.lines {
            ui.label(format!("@{}: {}", l.who, l.text));
        }
        if let Some(f) = &s.frame {
            ui.add_space(6.0);
            ui.weak("storyboard frame");
            self.thumbs.show(ui, &self.project.root.join(f), 200.0, f);
        }
        ui.add_space(12.0);
        ui.heading("Takes");
        if self.takes.is_empty() {
            ui.weak("no takes yet");
            return;
        }
        let mut play = None;
        let mut compare = None;
        let mut open = None;
        let mut select = None;
        // Comparing makes sense while one take of this shot is up.
        let single = self
            .deck
            .as_ref()
            .filter(|d| d.players.len() == 1)
            .map(|d| d.players[0].label.clone())
            .filter(|l| l.starts_with(&format!("{r}#")));
        egui::Grid::new("takes").striped(true).show(ui, |ui| {
            for t in &self.takes {
                let selected = s.selected_take.as_deref() == Some(t.id.to_string().as_str());
                let label = format!("#{}", t.number);
                if selected {
                    ui.strong(format!("{label} (selected)"));
                } else {
                    ui.label(label);
                }
                let status = t.status();
                let color = match status {
                    JobStatus::Done => ui.visuals().text_color(),
                    JobStatus::Failed => ui.visuals().error_fg_color,
                    _ => ui.visuals().weak_text_color(),
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
                            if ui.button(format!("Play #{}", t.number)).clicked() {
                                play = Some((label.clone(), file.clone()));
                            }
                            if single.as_ref().is_some_and(|l| *l != label)
                                && ui.button(format!("Compare #{}", t.number)).clicked()
                            {
                                compare = Some((label.clone(), file.clone()));
                            }
                            if self.viewer.is_some()
                                && ui
                                    .button(format!("Open #{}", t.number))
                                    .on_hover_text("full size in mpv or ffplay")
                                    .clicked()
                            {
                                open = Some(file.clone());
                            }
                        });
                        if !selected && ui.button(format!("Select #{}", t.number)).clicked() {
                            select = Some(t.clone());
                        }
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
        if let Some(t) = select {
            self.select(&t);
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
                    .color(Color32::from_rgb(200, 150, 40)),
                );
            }
            ui.add_space(12.0);
            egui::Grid::new("by shot").striped(true).show(ui, |ui| {
                ui.strong("shot");
                ui.strong(cur.as_str());
                ui.strong("jobs");
                ui.end_row();
                for r in rows {
                    ui.label(if r.key == "-" { "not a take" } else { &r.key });
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

/// Thai (and other scripts egui doesn't ship) from the system's own fonts, as a fallback.
fn fonts(ctx: &egui::Context) {
    const THAI: &[(&str, u32)] = &[
        ("/System/Library/Fonts/Supplemental/Ayuthaya.ttf", 0),
        ("/System/Library/Fonts/Thonburi.ttc", 0),
        ("/usr/share/fonts/truetype/noto/NotoSansThai-Regular.ttf", 0),
        ("/usr/share/fonts/opentype/noto/NotoSansThai-Regular.ttf", 0),
        ("/usr/share/fonts/noto/NotoSansThai-Regular.ttf", 0),
        ("C:\\Windows\\Fonts\\LeelawUI.ttf", 0),
        ("C:\\Windows\\Fonts\\tahoma.ttf", 0),
    ];
    let Some((bytes, index)) = THAI
        .iter()
        .find_map(|(p, i)| std::fs::read(p).ok().map(|b| (b, *i)))
    else {
        return;
    };
    let mut data = egui::FontData::from_owned(bytes);
    data.index = index;
    ctx.add_font(egui::epaint::text::FontInsert::new(
        "thai",
        data,
        vec![
            egui::epaint::text::InsertFontFamily {
                family: egui::FontFamily::Proportional,
                priority: egui::epaint::text::FontPriority::Lowest,
            },
            egui::epaint::text::InsertFontFamily {
                family: egui::FontFamily::Monospace,
                priority: egui::epaint::text::FontPriority::Lowest,
            },
        ],
    ));
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
        Box::new(move |cc| {
            fonts(&cc.egui_ctx);
            Ok(Box::new(App::new(project, user_dir).remember_place()))
        }),
    )
    .map_err(|e| anyhow::anyhow!("can't open the window: {e}"))
}
