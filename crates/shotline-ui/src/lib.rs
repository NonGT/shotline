//! The desktop app: browse a film's story, elements, shots, takes and costs. It reads and writes
//! the project folder through shotline-core like the CLI does, so both always agree.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use eframe::egui::{self, Color32, RichText};
use shotline_core::ledger::money;
use shotline_core::ledger::report::{self, By, Row, Summary};
use shotline_core::{
    Chapter, Element, JobStatus, Project, Shot, ShotRef, Take, Transition, UserConfig,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Story,
    Elements,
    Shots,
    Cost,
}

/// Everything shown, read from disk at once so a frame never touches the filesystem.
#[derive(Default)]
struct Film {
    story: String,
    style: String,
    elements: Vec<(Element, Vec<PathBuf>)>,
    chapters: Vec<(Chapter, Vec<(ShotRef, Shot)>)>,
    cost: Option<(Summary, Vec<Row>)>,
}

pub struct App {
    project: Project,
    user_dir: PathBuf,
    player: Option<PathBuf>,
    film: Film,
    tab: Tab,
    shot: Option<ShotRef>,
    takes: Vec<Take>,
    status: Option<(String, bool)>,
    thumbs: Thumbs,
}

const PLAYERS: [&str; 2] = ["mpv", "ffplay"];

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
            player: PLAYERS.iter().find_map(|p| on_path(p)),
            film: Film::default(),
            tab: Tab::Shots,
            shot: None,
            takes: vec![],
            status: None,
            thumbs: Thumbs::default(),
        };
        app.reload();
        app
    }

    /// What "Play" runs with the file as its last argument. None shows a hint instead.
    pub fn with_player(mut self, player: Option<PathBuf>) -> App {
        self.player = player;
        self
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

    fn play(&mut self, file: &Path) {
        let Some(player) = &self.player else {
            self.error(format!(
                "no player found; install mpv or ffplay, or open {} yourself",
                file.display()
            ));
            return;
        };
        let mut cmd = Command::new(player);
        if player.file_stem().is_some_and(|s| s == "ffplay") {
            cmd.args(["-autoexit", "-loglevel", "error"]);
        }
        match cmd.arg(file).spawn() {
            Ok(_) => self.info(format!("playing {}", self.project.rel(file))),
            Err(e) => self.error(format!("can't run {}: {e}", player.display())),
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
        egui::Panel::top("bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(&self.project.config.name);
                ui.separator();
                for (tab, name) in [
                    (Tab::Story, "Story"),
                    (Tab::Elements, "Elements"),
                    (Tab::Shots, "Shots"),
                    (Tab::Cost, "Cost"),
                ] {
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
        match self.tab {
            Tab::Story => self.story(ui),
            Tab::Elements => self.elements(ui),
            Tab::Shots => self.shots(ui),
            Tab::Cost => self.costs(ui),
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
                    egui::ScrollArea::vertical().show(ui, |ui| self.shot_detail(ui, r, &s));
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
        let mut select = None;
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
                        if ui.button(format!("Play #{}", t.number)).clicked() {
                            play = Some(file);
                        }
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
        if let Some(f) = play {
            self.play(&f);
        }
        if let Some(t) = select {
            self.select(&t);
        }
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
            Ok(Box::new(App::new(project, user_dir)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("can't open the window: {e}"))
}
