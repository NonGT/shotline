//! The desktop app: browse a film's story, elements, shots, takes and costs, and play takes and
//! the cut. It reads and writes the project folder through shotline-core like the CLI does, so
//! both always agree.

mod anchor;
mod cost;
mod cut;
mod edit;
mod git;
mod keys;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod menu;
mod player;
mod start;
mod theme;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use shotline_core::ledger::money;
use shotline_core::protocol::{Description, ModelInfo, Op, VoiceInfo};
use shotline_core::{
    Chapter, CutShot, Element, Git, JobStatus, Kind, Project, Shot, ShotRef, Take, Timeline,
    Transition,
};
use shotline_media::Ffmpeg;

use edit::{Action, ElementForm, Form, History, LineForm, NewElement, Texts};
use git::{Dialog, Repo};
use player::{Audio, Deck, Player};
pub use start::{MenuCommand, Shell};

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
    timeline: Timeline,
    /// renders/*.mp4, newest first.
    renders: Vec<PathBuf>,
    cost: Option<cost::Cost>,
    /// How many takes each shot has.
    takes: HashMap<ShotRef, usize>,
    /// Shots with a selected take.
    made: usize,
}

/// What the app asks of installed plugins. The CLI provides it; the app calls it off the UI thread.
pub trait Host: Send + Sync {
    fn voices(&self, plugin: &str) -> anyhow::Result<Vec<VoiceInfo>>;
    /// What's left on the account, said in a line. It costs nothing, so it checks a key.
    fn balance(&self, plugin: &str) -> anyhow::Result<String>;
    /// What a new anchor image would cost, e.g. "$0.04". Free.
    fn anchor_quote(&self, film: &Path, ask: &AnchorAsk) -> anyhow::Result<String>;
    /// Downloads a reference image into the film's cache. Free.
    fn fetch_image(&self, film: &Path, url: &str) -> anyhow::Result<PathBuf>;
    /// Renders the cut as timeline.toml says, telling `on` each step. Returns the file and a
    /// line about it. Free: ffmpeg runs here.
    fn render(&self, film: &Path, on: &mut dyn FnMut(String)) -> anyhow::Result<(PathBuf, String)>;
    /// Every rate for each unit and where it comes from. Free, but asks each plugin.
    fn rates(&self, film: &Path) -> anyhow::Result<Vec<shotline_core::ledger::RateLine>>;
    /// The ledger against what providers say they charged. Free reads.
    fn reconcile(
        &self,
        film: &Path,
    ) -> anyhow::Result<Vec<shotline_core::ledger::reconcile::Check>>;
    /// Spends, if the price is still `at`, and returns the image, not yet an anchor.
    fn make_anchor(&self, film: &Path, ask: &AnchorAsk, at: &str) -> anyhow::Result<PathBuf>;
}

/// A new anchor image for an element, made by an image model.
#[derive(Clone, Debug, PartialEq)]
pub struct AnchorAsk {
    pub handle: String,
    /// Added to the element's description.
    pub prompt: String,
    pub plugin: String,
    pub model: String,
    /// The images that go along, named Image 1, Image 2… in this order.
    pub refs: Vec<PathBuf>,
    /// None for [image] in shotline.toml.
    pub aspect: Option<String>,
    pub resolution: Option<String>,
}

#[derive(Clone)]
enum VoiceList {
    Loading,
    Ready(Vec<VoiceInfo>),
    Failed(String),
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
    /// Jobs found holding money with nothing running them when the film was opened.
    stuck: Vec<String>,
    thumbs: Thumbs,
    /// The tab and shot last written to the place file, when the app keeps one.
    place: Option<(Tab, Option<ShotRef>, Option<String>)>,
    themed: bool,
    form: Form,
    texts: Texts,
    /// The title typed for a new chapter.
    new_chapter: String,
    /// The open element, by handle.
    element: Option<String>,
    element_form: ElementForm,
    new_element: NewElement,
    /// The picture shown big over everything.
    zoom: Option<PathBuf>,
    plugins: Vec<Description>,
    host: Option<Arc<dyn Host>>,
    /// Keys asked for, e.g. from a plugin that said it has none; the shell opens Settings.
    wants_keys: bool,
    maker: Option<anchor::Maker>,
    /// timeline.toml as edited, saved once nothing has focus.
    timeline_form: Timeline,
    render: Arc<Mutex<cut::Render>>,
    cost_view: cost::View,
    voice_lists: Arc<Mutex<HashMap<String, VoiceList>>>,
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
/// once when first shown. Big ones are decoded off the UI thread; that takes a moment.
#[derive(Default)]
struct Thumbs {
    done: HashMap<(PathBuf, u32), Option<egui::TextureHandle>>,
    decoding: Arc<Mutex<HashMap<(PathBuf, u32), Decoded>>>,
}

/// None while the thread is still at it, then what it made of the file.
type Decoded = Option<Option<egui::ColorImage>>;

enum Big {
    Ready(egui::TextureHandle),
    Decoding,
    Failed,
}

fn decode(path: &Path, px: u32) -> Option<egui::ColorImage> {
    // By its bytes, not its name: a .png that's really a JPEG still shows.
    let img = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?
        .thumbnail(px, px)
        .to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()))
}

const THUMB: u32 = 512;
const BIG: u32 = 2048;

impl Thumbs {
    fn texture(
        &mut self,
        ui: &egui::Ui,
        path: &Path,
        px: u32,
        name: &str,
    ) -> Option<egui::TextureHandle> {
        self.done
            .entry((path.to_path_buf(), px))
            .or_insert_with(|| {
                let pixels = decode(path, px)?;
                Some(ui.ctx().load_texture(name, pixels, Default::default()))
            })
            .clone()
    }

    /// Like `texture`, but decoded on another thread, asking for a repaint when it's there.
    fn texture_later(&mut self, ui: &egui::Ui, path: &Path, px: u32, name: &str) -> Big {
        let key = (path.to_path_buf(), px);
        if let Some(t) = self.done.get(&key) {
            return t.clone().map_or(Big::Failed, Big::Ready);
        }
        let mut decoding = self.decoding.lock().unwrap();
        match decoding.get_mut(&key) {
            None => {
                decoding.insert(key.clone(), None);
                let (all, ctx) = (self.decoding.clone(), ui.ctx().clone());
                std::thread::spawn(move || {
                    let img = decode(&key.0, key.1);
                    all.lock().unwrap().insert(key, Some(img));
                    ctx.request_repaint();
                });
                Big::Decoding
            }
            Some(None) => Big::Decoding,
            Some(Some(img)) => {
                let t = img
                    .take()
                    .map(|img| ui.ctx().load_texture(name, img, Default::default()));
                decoding.remove(&key);
                drop(decoding);
                self.done.insert(key, t.clone());
                t.map_or(Big::Failed, Big::Ready)
            }
        }
    }

    /// A thumbnail; clicking it asks for the picture full size.
    fn show(&mut self, ui: &mut egui::Ui, path: &Path, height: f32, name: &str) -> bool {
        match self.texture(ui, path, THUMB, name) {
            Some(t) => ui
                .add(
                    egui::Image::new(&t)
                        .max_height(height)
                        .alt_text(name)
                        .sense(egui::Sense::click()),
                )
                .on_hover_cursor(egui::CursorIcon::ZoomIn)
                .on_hover_text("click to see it bigger")
                .clicked(),
            None => {
                ui.weak(format!("can't show {name}"));
                false
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
            stuck: vec![],
            thumbs: Thumbs::default(),
            place: None,
            themed: false,
            form: Form::default(),
            texts: Texts::default(),
            new_chapter: String::new(),
            element: None,
            element_form: ElementForm::default(),
            new_element: NewElement::default(),
            zoom: None,
            plugins: vec![],
            host: None,
            wants_keys: false,
            maker: None,
            timeline_form: Timeline::default(),
            render: Arc::default(),
            cost_view: cost::View::default(),
            voice_lists: Arc::default(),
            history: History::default(),
            git: Git::find(),
            repo: Repo::None,
            repo_at: 0.0,
            dialog: None,
        };
        app.reload();
        let now = app.project.clock.now();
        app.stuck = app.film.cost.as_ref().map_or(vec![], |c| c.stuck(now));
        app
    }

    /// What "Open" runs with the file as its last argument, to watch a take full size.
    pub fn with_viewer(mut self, viewer: Option<PathBuf>) -> App {
        self.viewer = viewer;
        self
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn set_plugins(&mut self, plugins: Vec<Description>, host: Option<Arc<dyn Host>>) {
        self.plugins = plugins;
        self.host = host;
    }

    /// A paid-for image is kept whichever element is showing now.
    fn add_made_anchor(&mut self) {
        let Some(m) = &mut self.maker else { return };
        let Some(out) = m.take_made() else { return };
        match self.project.element(&m.handle) {
            Ok(el) => {
                self.flush();
                self.apply(vec![Action::AddAnchor(el, out)]);
            }
            Err(e) => self.status = Some((format!("made {}, but {e}", out.display()), true)),
        }
    }

    fn image_models(&self) -> Vec<anchor::ImageModel> {
        self.plugins
            .iter()
            .flat_map(|d| {
                d.models
                    .iter()
                    .filter(|m| m.op == Op::ImageGenerate)
                    .map(|m| anchor::ImageModel {
                        plugin: d.name.clone(),
                        model: m.id.clone(),
                        ref_label: m.ref_image.clone(),
                        max_refs: m.max_images,
                        aspects: m.aspects.clone(),
                        resolutions: m.resolutions.clone(),
                    })
            })
            .collect()
    }

    /// Every installed video model, as plugin, model and what it can do.
    fn video_models(&self) -> Vec<(String, ModelInfo)> {
        self.plugins
            .iter()
            .flat_map(|d| {
                d.models
                    .iter()
                    .filter(|m| m.op == Op::VideoGenerate)
                    .map(|m| (d.name.clone(), m.clone()))
            })
            .collect()
    }

    /// Every element's anchors, named like "@paka 2", to pick references from.
    fn anchor_pool(&self) -> Vec<(String, PathBuf)> {
        let mut pool = vec![];
        for (el, anchors) in &self.film.elements {
            for (i, a) in anchors.iter().enumerate() {
                pool.push((format!("@{} {}", el.handle, i + 1), a.clone()));
            }
        }
        pool
    }

    /// Voice lists are asked for again, e.g. once a key is set.
    pub(crate) fn forget_voices(&self) {
        self.voice_lists.lock().unwrap().clear();
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
                Some(("element", h)) => self.open_element_named(h.to_string()),
                _ => {}
            }
        }
        self.place = Some((self.tab, self.shot, self.element.clone()));
        self
    }

    fn keep_place(&mut self) {
        let now = (self.tab, self.shot, self.element.clone());
        if self.place.as_ref().is_none_or(|p| *p == now) {
            return;
        }
        self.place = Some(now);
        let mut text = format!("tab={}\n", self.tab.name());
        if let Some(r) = self.shot {
            text += &format!("shot={r}\n");
        }
        if let Some(h) = &self.element {
            text += &format!("element={h}\n");
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

    /// The open element as last read from disk.
    fn open_element(&self) -> Option<Element> {
        let h = self.element.as_ref()?;
        self.film
            .elements
            .iter()
            .find(|(e, _)| e.handle == *h)
            .map(|(e, _)| e.clone())
    }

    fn open_element_named(&mut self, handle: String) {
        self.flush();
        self.element = Some(handle);
        self.fill_form();
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
        let timeline = p.timeline()?;
        self.film = Film {
            story: read(&p.root.join("story.md")),
            style: read(&p.root.join("style.md")),
            elements,
            chapters,
            cut: p.cut(&timeline)?,
            timeline,
            renders: renders(&p.root.join("renders")),
            cost: cost::load(p, &self.user_dir)?,
            takes: takes.into_iter().map(|(s, t)| (s, t.len())).collect(),
            made: p.selected_take_ids()?.len(),
        };
        self.load_takes()
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
        self.add_made_anchor();
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
        self.stuck_jobs(ui);
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
        self.zoomed(ui);
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
            // Ctrl+Y as well on Windows and Linux.
            let y = !cfg!(target_os = "macos")
                && ui.input_mut(|i| {
                    i.consume_shortcut(&egui::KeyboardShortcut::new(
                        egui::Modifiers::COMMAND,
                        egui::Key::Y,
                    ))
                });
            if y || key(true) {
                self.redo();
            } else if key(false) {
                self.undo();
            }
        }
    }

    fn zoomed(&mut self, ui: &mut egui::Ui) {
        let Some(path) = self.zoom.clone() else {
            return;
        };
        let name = self.project.rel(&path);
        let room = ui.ctx().content_rect().size() * 0.88;
        let mut close = false;
        let max = room - egui::vec2(0.0, 40.0);
        let modal = egui::Modal::new(egui::Id::unique("zoom")).show(ui.ctx(), |ui| {
            match self.thumbs.texture_later(ui, &path, BIG, &name) {
                Big::Ready(t) => {
                    let img = ui.add(
                        egui::Image::new(&t)
                            .max_size(max)
                            .alt_text(format!("{name} big"))
                            .sense(egui::Sense::click()),
                    );
                    close = img.clicked();
                }
                Big::Decoding => {
                    // The size it will show at, from the header, so nothing jumps once it's in.
                    let size = image::ImageReader::open(&path)
                        .ok()
                        .and_then(|r| r.with_guessed_format().ok()?.into_dimensions().ok())
                        .map_or(egui::vec2(max.y, max.y), |(w, h)| {
                            let s = egui::vec2(w as f32, h as f32);
                            s * (BIG as f32 / s.max_elem()).min(1.0)
                        });
                    let size = size * (max / size).min_elem().min(1.0);
                    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                    ui.painter().rect_filled(rect, 2.0, theme::RACK);
                    egui::Spinner::new().size(32.0).paint_at(
                        ui,
                        egui::Rect::from_center_size(rect.center(), egui::vec2(32.0, 32.0)),
                    );
                }
                Big::Failed => {
                    ui.weak(format!("can't show {name}"));
                }
            }
            // As wide as the picture, or a right-to-left row widens the modal every frame.
            let width = ui.min_rect().width().max(320.0);
            ui.allocate_ui_with_layout(
                egui::vec2(width, ui.spacing().interact_size.y),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.label(RichText::new(&name).color(theme::DUST));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        close |= ui.button("Close").clicked();
                    });
                },
            );
        });
        if close || modal.should_close() {
            self.zoom = None;
        }
    }

    fn story(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BAY))
            .show(ui, |ui| {
                page(ui, PAGE, |ui| {
                    ui.set_max_width(720.0);
                    let models = self.video_models();
                    format_fields(ui, &mut self.texts.format, &models, self.film.made);
                    ui.add_space(20.0);
                    let t = &mut self.texts;
                    text_field(
                        ui,
                        "Story",
                        &mut t.story,
                        "What the film is about; shot drafts read it",
                    );
                    ui.add_space(14.0);
                    text_field(
                        ui,
                        "Style",
                        &mut t.style,
                        "How every shot should look; added to every prompt",
                    );
                    hint(ui, "Text between <!-- and --> is left out of prompts.");
                });
            });
    }

    fn elements(&mut self, ui: &mut egui::Ui) {
        let mut open = None;
        let mut acts = vec![];
        egui::Panel::left("elements")
            .resizable(true)
            .default_size(260.0)
            .frame(egui::Frame::new().fill(theme::RACK))
            .show(ui, |ui| {
                page(ui, SIDE, |ui| {
                    for (kind, heading) in [
                        (Kind::Cast, "Cast"),
                        (Kind::Set, "Sets"),
                        (Kind::Prop, "Props"),
                    ] {
                        let els: Vec<_> = self
                            .film
                            .elements
                            .iter()
                            .filter(|(e, _)| e.kind == kind)
                            .collect();
                        if els.is_empty() {
                            continue;
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(heading)
                                    .family(egui::FontFamily::Name(theme::SEMIBOLD.into()))
                                    .color(theme::PAPER),
                            );
                        });
                        for (e, anchors) in els {
                            let here = self.element.as_ref() == Some(&e.handle);
                            let key = format!("@{}", e.handle);
                            if list_row(ui, &key, &e.name, anchors.len(), false, here).clicked() {
                                open = Some(e.handle.clone());
                            }
                        }
                        ui.add_space(8.0);
                    }
                    ui.add_space(4.0);
                    let n = &mut self.new_element;
                    let label = caption(ui, "New element");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("kind")
                            .selected_text(n.kind.to_string())
                            .width(64.0)
                            .show_ui(ui, |ui| {
                                for k in Kind::ALL {
                                    ui.selectable_value(&mut n.kind, k, k.to_string());
                                }
                            })
                            .response
                            .widget_info(|| {
                                egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Kind")
                            });
                        ui.add(
                            egui::TextEdit::singleline(&mut n.name)
                                .hint_text(RichText::new("Name").color(theme::DUST))
                                .desired_width(f32::INFINITY),
                        )
                        .labelled_by(label.id);
                    });
                    let slug = shotline_core::slug::slugify(n.name.trim());
                    let handle = ui
                        .horizontal(|ui| {
                            ui.label(RichText::new("@").color(theme::DUST));
                            ui.add(
                                egui::TextEdit::singleline(&mut n.handle)
                                    .hint_text(
                                        RichText::new(if slug.is_empty() {
                                            "handle"
                                        } else {
                                            slug.as_str()
                                        })
                                        .color(theme::DUST),
                                    )
                                    .desired_width(f32::INFINITY),
                            )
                            .on_hover_text("how prompts name it; made from the name if left empty")
                        })
                        .inner;
                    let enter =
                        handle.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let ready = !n.name.trim().is_empty();
                    let add = ui.add_enabled(ready, egui::Button::new("Add element"));
                    if (add.clicked() || enter) && ready {
                        acts.push(Action::AddElement(std::mem::take(n)));
                    }
                });
            });
        if !acts.is_empty() {
            self.flush();
            self.apply(acts);
        }
        if let Some(h) = open {
            self.open_element_named(h);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BAY))
            .show(ui, |ui| {
                let found = self.element.as_ref().and_then(|h| {
                    self.film.elements.iter().find(|(e, _)| e.handle == *h).cloned()
                });
                match found {
                    Some((el, anchors)) => {
                        page(ui, PAGE, |ui| {
                            self.element_detail(ui, &el, &anchors);
                        });
                    }
                    None if self.film.elements.is_empty() => {
                        page(ui, PAGE, |ui| hint(ui, "Cast, sets and props that shots name as @handles. Add the first one on the left."));
                    }
                    None => page(ui, PAGE, |ui| hint(ui, "Pick an element on the left.")),
                }
            });
    }

    fn element_detail(&mut self, ui: &mut egui::Ui, el: &Element, anchors: &[PathBuf]) {
        let mut acts = vec![];
        ui.horizontal(|ui| {
            let key = format!("@{}", el.handle);
            title_row(ui, &key, &mut self.element_form.name, "", 60.0)
                .on_hover_text("its name; the @handle stays as prompts use it");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(el.kind.to_string());
            });
        });
        ui.add_space(14.0);
        ui.vertical(|ui| {
            ui.set_max_width(620.0);
            text_field(
                ui,
                "Description",
                &mut self.element_form.description,
                "What it looks like. Goes into the prompt of every shot that names it; models follow English best.",
            );
            ui.add_space(14.0);
            caption(ui, "Anchor images");
            if anchors.is_empty() {
                hint(ui, "Pictures of it that video models are shown, to keep it the same in every shot.");
            }
            let mut remove = None;
            ui.horizontal_wrapped(|ui| {
                for a in anchors {
                    ui.vertical(|ui| {
                        let name = self.project.rel(a);
                        if self.thumbs.show(ui, a, 160.0, &name) {
                            self.zoom = Some(a.clone());
                        }
                        if remove_button(ui)
                            .on_hover_text(format!("take {name} off @{}", el.handle))
                            .clicked()
                        {
                            remove = Some(a.clone());
                        }
                    });
                }
            });
            if let Some(a) = remove {
                acts.push(Action::RemoveAnchor(el.clone(), a));
            }
            let models = self.image_models();
            let making = self.maker.as_ref().is_some_and(|m| m.handle == el.handle);
            ui.horizontal(|ui| {
                if ui.button("Add image…").clicked()
                    && let Some(file) = rfd::FileDialog::new()
                        .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
                        .pick_file()
                {
                    acts.push(Action::AddAnchor(el.clone(), file));
                }
                let busy = self.maker.as_ref().filter(|m| m.busy()).map(|m| m.handle.clone());
                if !models.is_empty()
                    && !making
                    && ui
                        .add_enabled(busy.is_none(), egui::Button::new("Make image…"))
                        .on_disabled_hover_text(format!(
                            "still making one for @{}",
                            busy.unwrap_or_default()
                        ))
                        .clicked()
                {
                    let film = &self.project.config;
                    let cfg = &film.image;
                    self.maker = Some(anchor::Maker::new(
                        &el.handle,
                        anchors.to_vec(),
                        &models,
                        (&cfg.plugin, &cfg.model),
                        (
                            cfg.aspect.clone().unwrap_or_else(|| film.aspect.clone()),
                            cfg.resolution.clone(),
                        ),
                    ));
                }
            });
            if making {
                ui.add_space(6.0);
                let pool = self.anchor_pool();
                let wants = self.maker.as_mut().unwrap().show(
                    ui,
                    &models,
                    &pool,
                    &mut self.thumbs,
                    self.host.clone(),
                    &self.project.root,
                );
                let host = self.host.clone();
                match (wants, host) {
                    (anchor::Wants::Price(ask), Some(host)) => {
                        self.flush();
                        let m = self.maker.as_ref().unwrap();
                        m.price(ui.ctx(), host, &self.project.root, ask);
                    }
                    (anchor::Wants::Make(ask, cost), Some(host)) => {
                        self.flush();
                        let m = self.maker.as_ref().unwrap();
                        m.make(ui.ctx(), host, &self.project.root, ask, cost);
                    }
                    (anchor::Wants::Zoom(p), _) => self.zoom = Some(p),
                    _ => {}
                }
            }
            if el.kind == Kind::Cast {
                ui.add_space(20.0);
                self.voice(ui, el);
            }
        });
        if !acts.is_empty() {
            self.flush();
            self.apply(acts);
        }
    }

    /// Which TTS plugin and voice speak a cast member's lines.
    fn voice(&mut self, ui: &mut egui::Ui, el: &Element) {
        ui.label(
            RichText::new("Voice")
                .text_style(egui::TextStyle::Heading)
                .color(theme::PAPER),
        );
        ui.add_space(4.0);
        let plugin = self.element_form.voice.plugin.clone();
        let voices = (!plugin.is_empty()).then(|| self.voice_list(ui.ctx(), &plugin));
        // Plugins that can speak, with their speaking models.
        let tts: Vec<(String, Vec<&ModelInfo>)> = self
            .plugins
            .iter()
            .filter_map(|d| {
                let models: Vec<&ModelInfo> =
                    d.models.iter().filter(|m| m.op == Op::AudioTts).collect();
                (!models.is_empty()).then(|| (d.name.clone(), models))
            })
            .collect();
        let root = self.project.root.clone();
        let v = &mut self.element_form.voice;
        let installed = tts.iter().find(|(p, _)| *p == v.plugin);
        let shown = match (v.plugin.as_str(), installed) {
            ("", _) => "no voice".to_string(),
            (p, Some(_)) => p.to_string(),
            (p, None) => format!("{p} (not installed)"),
        };
        // The model's settings, or the first model's when it's left to the plugin.
        let models = installed.map(|(_, m)| m.as_slice()).unwrap_or_default();
        let schema = models
            .iter()
            .find(|m| m.id == v.model)
            .or(models.first())
            .and_then(|m| m.voice_schema.clone());
        let mut sample = None;
        let mut keys = false;
        egui::Grid::new("voice")
            .num_columns(2)
            .spacing(egui::vec2(12.0, 8.0))
            .show(ui, |ui| {
                ui.weak("Plugin");
                egui::ComboBox::from_id_salt("voice plugin")
                    .selected_text(shown)
                    .width(320.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(v.plugin.is_empty(), "no voice")
                            .clicked()
                        {
                            *v = Default::default();
                        }
                        for (p, _) in &tts {
                            if ui.selectable_label(v.plugin == *p, p).clicked() && v.plugin != *p {
                                v.plugin = p.clone();
                                v.model.clear();
                                v.voice_id.clear();
                            }
                        }
                    })
                    .response
                    .widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Plugin")
                    });
                ui.end_row();
                if v.plugin.is_empty() {
                    return;
                }
                ui.weak("Model");
                egui::ComboBox::from_id_salt("voice model")
                    .selected_text(if v.model.is_empty() {
                        "the plugin's default"
                    } else {
                        v.model.as_str()
                    })
                    .width(320.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut v.model, String::new(), "the plugin's default");
                        for m in models {
                            ui.selectable_value(&mut v.model, m.id.clone(), &m.id);
                        }
                    })
                    .response
                    .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Model"));
                ui.end_row();
                ui.weak("Voice");
                keys |= voice_picker(ui, &mut v.voice_id, voices.as_ref());
                ui.end_row();
                let l = ui.weak("Sample");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut v.sample)
                            .hint_text(RichText::new("a recording to clone").color(theme::DUST))
                            .desired_width(240.0),
                    )
                    .labelled_by(l.id);
                    if ui.button("Choose").clicked() {
                        sample = rfd::FileDialog::new().set_directory(&root).pick_file();
                    }
                });
                ui.end_row();
                settings_rows(ui, schema.as_ref(), &mut v.settings);
            });
        let none = v.plugin.is_empty();
        self.wants_keys |= keys;
        if let Some(f) = sample {
            self.element_form.voice.sample = self.project.rel(&f);
        }
        if tts.is_empty() {
            hint(
                ui,
                "No installed plugin can speak yet. One that does audio.tts, like elevenlabs, \
                 shows up here once it's installed.",
            );
        } else if el.voice.is_none() && none {
            hint(
                ui,
                &format!("Without a voice, @{}'s lines aren't spoken.", el.handle),
            );
        }
    }

    /// A plugin's voices, asked for once in the background.
    fn voice_list(&mut self, ctx: &egui::Context, plugin: &str) -> VoiceList {
        let mut lists = self.voice_lists.lock().unwrap();
        if let Some(l) = lists.get(plugin) {
            return l.clone();
        }
        let Some(host) = self.host.clone() else {
            return VoiceList::Failed("can't ask plugins for voices here".into());
        };
        lists.insert(plugin.to_string(), VoiceList::Loading);
        let (lists, ctx, plugin) = (self.voice_lists.clone(), ctx.clone(), plugin.to_string());
        std::thread::spawn(move || {
            let got = match host.voices(&plugin) {
                Ok(v) => VoiceList::Ready(v),
                Err(e) => VoiceList::Failed(e.to_string()),
            };
            lists.lock().unwrap().insert(plugin, got);
            ctx.request_repaint();
        });
        VoiceList::Loading
    }

    fn shots(&mut self, ui: &mut egui::Ui) {
        let mut open = None;
        let mut acts = vec![];
        egui::Panel::left("shots")
            .resizable(true)
            .default_size(260.0)
            .frame(egui::Frame::new().fill(theme::RACK))
            .show(ui, |ui| {
                page(ui, SIDE, |ui| {
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
                        ui.horizontal(|ui| {
                            ui.add_space(4.0);
                            if quiet_button(ui, "Add shot")
                                .on_hover_text(format!("at the end of chapter {}", c.id()))
                                .clicked()
                            {
                                acts.push(Action::AddShot(c.number, None));
                            }
                        });
                        ui.add_space(8.0);
                    }
                    ui.add_space(4.0);
                    let label = caption(ui, "New chapter");
                    let field = ui
                        .add(
                            egui::TextEdit::singleline(&mut self.new_chapter)
                                .hint_text(RichText::new("Its title").color(theme::DUST))
                                .desired_width(f32::INFINITY),
                        )
                        .labelled_by(label.id);
                    let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let title = self.new_chapter.trim().to_string();
                    let add = ui.add_enabled(!title.is_empty(), egui::Button::new("Add chapter"));
                    if (add.clicked() || enter) && !title.is_empty() {
                        acts.push(Action::AddChapter(title));
                        self.new_chapter.clear();
                    }
                });
            });
        if !acts.is_empty() {
            // Typing in the open shot is saved first, as its own step.
            self.flush();
            self.apply(acts);
        }
        if let Some(r) = open {
            self.open_shot(r);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BAY))
            .show(ui, |ui| match self.open() {
                Some((r, s)) => {
                    page(ui, PAGE, |ui| {
                        self.deck(ui);
                        self.shot_detail(ui, r, &s)
                    });
                }
                None if self.film.chapters.is_empty() => {
                    page(ui, PAGE, |ui| {
                        hint(ui, "Shots go in chapters. Add the first one on the left.")
                    });
                }
                None => {
                    page(ui, PAGE, |ui| hint(ui, "Pick a shot on the left."));
                }
            });
    }

    fn shot_detail(&mut self, ui: &mut egui::Ui, r: ShotRef, s: &Shot) {
        let mut acts = vec![];
        ui.horizontal(|ui| {
            title_row(ui, &r.to_string(), &mut self.form.title, "Add a title", 0.0);
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
                ui.add_space(12.0);
                if quiet_button(ui, "Add a shot after this one").clicked() {
                    acts.push(Action::AddShot(r.chapter, Some(r.shot)));
                }
            });
        });
        ui.add_space(20.0);
        ui.separator();
        ui.add_space(8.0);
        self.take_list(ui, r, s, &mut acts);
        if acts.iter().any(|a| matches!(a, Action::AddShot(..))) {
            self.flush();
        }
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
                "Only cast speak, and there's no cast yet. Add some in Elements.",
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
            Some(f) => {
                let path = self.project.root.join(f);
                if self.thumbs.show(ui, &path, 150.0, f) {
                    self.zoom = Some(path);
                }
            }
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
                top_label(ui, "Starts");
                ui.vertical(|ui| {
                    ui.radio_value(&mut f.transition, Transition::Cut, "on a cut");
                    ui.radio_value(
                        &mut f.transition,
                        Transition::Continue,
                        "from the shot before",
                    );
                });
                ui.end_row();
                let recipes: Vec<String> = self.project.config.recipes.keys().cloned().collect();
                if !recipes.is_empty() || s.recipe.is_some() {
                    ui.weak("Recipe");
                    egui::ComboBox::from_id_salt("recipe")
                        .selected_text(f.recipe.as_deref().unwrap_or("video only"))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut f.recipe, None, "video only");
                            for rc in recipes {
                                ui.selectable_value(&mut f.recipe, Some(rc.clone()), rc);
                            }
                        })
                        .response
                        .widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Recipe")
                        });
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
    list_row(
        ui,
        &r.to_string(),
        title,
        takes,
        s.selected_take.is_some(),
        open,
    )
}

/// A row in a side list: its key in grey, a title, a count and maybe an amber dot.
fn list_row(
    ui: &mut egui::Ui,
    key: &str,
    title: &str,
    count: usize,
    dot: bool,
    open: bool,
) -> egui::Response {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 26.0), egui::Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::Button, true, open, format!("{key}  {title}"))
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
    let num = p.layout_no_wrap(key.to_string(), font.clone(), theme::DUST);
    let x = rect.left() + 12.0;
    p.galley(
        egui::pos2(x, mid - num.size().y / 2.0),
        num.clone(),
        theme::DUST,
    );

    let mut right = rect.right() - 8.0;
    if dot {
        p.circle_filled(egui::pos2(right - 3.0, mid), 3.5, theme::TUNGSTEN);
        right -= 14.0;
    }
    if count > 0 {
        let n = p.layout_no_wrap(
            count.to_string(),
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

/// A shot's number or an element's handle, big, with its title editable beside it on the same
/// baseline. Centring them, as a row does, lifts the smaller title.
fn title_row(
    ui: &mut egui::Ui,
    key: &str,
    text: &mut String,
    hint: &str,
    right: f32,
) -> egui::Response {
    let big = theme::slate();
    let key = ui.label(RichText::new(key).font(big.clone()).color(theme::PAPER));
    let size = ui.style().text_styles[&egui::TextStyle::Heading].size;
    let baseline = key.rect.top() + big.size * theme::ASCENT;
    let x = key.rect.right() + 10.0;
    let width = (ui.max_rect().right() - right - x).max(80.0);
    let rect = egui::Rect::from_min_size(
        egui::pos2(x, baseline - size * theme::ASCENT),
        egui::vec2(width, size * theme::LINE_HEIGHT),
    );
    ui.put(
        rect,
        egui::TextEdit::singleline(text)
            .font(egui::TextStyle::Heading)
            .text_color(theme::GRAPHITE)
            .hint_text(hint)
            .frame(egui::Frame::NONE)
            .margin(egui::Margin::ZERO)
            .desired_width(width),
    )
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

/// The voice by name from the plugin's list, or its ID typed in when there's no list. Returns
/// whether the keys were asked for.
fn voice_picker(ui: &mut egui::Ui, id: &mut String, list: Option<&VoiceList>) -> bool {
    let mut keys = false;
    let field = |ui: &mut egui::Ui, id: &mut String| {
        ui.add(
            egui::TextEdit::singleline(id)
                .hint_text(RichText::new("the voice's ID from the provider").color(theme::DUST))
                .desired_width(320.0),
        )
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Voice ID"));
    };
    match list {
        Some(VoiceList::Ready(voices)) if !voices.is_empty() => {
            let name = voices.iter().find(|v| v.id == *id).map(|v| v.name.clone());
            let shown = match (name, id.is_empty()) {
                (Some(n), _) => n,
                (None, true) => "pick a voice".into(),
                (None, false) => format!("{id} (not on the account)"),
            };
            egui::ComboBox::from_id_salt("voice id")
                .selected_text(shown)
                .width(320.0)
                .show_ui(ui, |ui| {
                    for v in voices {
                        let r = ui.selectable_value(id, v.id.clone(), &v.name);
                        if let Some(d) = &v.description {
                            r.on_hover_text(d);
                        }
                    }
                })
                .response
                .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Voice"));
        }
        Some(VoiceList::Loading) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("asking the plugin for its voices");
            });
        }
        Some(VoiceList::Failed(e)) => {
            ui.vertical(|ui| {
                field(ui, id);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(short(e, 56)).small().color(theme::DUST))
                        .on_hover_text(e);
                    // Most often the key isn't set.
                    keys = ui.small_button("Keys…").clicked();
                });
            });
        }
        _ => field(ui, id),
    }
    keys
}

/// Voice settings as the model's schema describes them, or as `key = value` lines without one.
fn settings_rows(ui: &mut egui::Ui, schema: Option<&serde_json::Value>, text: &mut String) {
    let props = schema
        .and_then(|s| s.get("properties"))
        .and_then(|p| p.as_object())
        .filter(|p| !p.is_empty());
    let Some(props) = props else {
        let l = top_label(ui, "Settings");
        ui.add(
            egui::TextEdit::multiline(text)
                .hint_text(RichText::new("key = value, one a line").color(theme::DUST))
                .desired_rows(2)
                .desired_width(320.0),
        )
        .labelled_by(l.id);
        ui.end_row();
        return;
    };
    let mut table = edit::settings_table(text);
    let before = table.clone();
    for (key, p) in props {
        let l = ui.weak(key.replace('_', " "));
        if let Some(d) = p.get("description").and_then(|d| d.as_str()) {
            l.on_hover_text(d);
        }
        ui.horizontal(|ui| {
            setting(ui, key, p, &mut table);
            // Greyed out while the provider's default is what's used.
            let set = table.contains_key(key);
            if ui
                .add_enabled(set, egui::Button::new("Default"))
                .on_hover_text("go back to the provider's default")
                .on_disabled_hover_text("the provider's default is used")
                .clicked()
            {
                table.remove(key);
            }
        });
        ui.end_row();
    }
    // Settings the schema doesn't know stay, and can be changed as text.
    for key in before.keys().filter(|k| !props.contains_key(*k)) {
        ui.weak(key);
        let mut raw = before[key].to_string();
        if ui.text_edit_singleline(&mut raw).changed() {
            table.insert(key.clone(), edit::setting_value(&raw));
        }
        ui.end_row();
    }
    if table != before {
        *text = edit::settings_text(&table);
    }
}

/// One setting's widget, by its JSON Schema type. Until it's set, the default shows.
fn setting(ui: &mut egui::Ui, key: &str, p: &serde_json::Value, table: &mut toml::Table) {
    use serde_json::Value as J;
    let default = p.get("default");
    let now = table.get(key).cloned();
    let label = key.replace('_', " ");
    let plain = |v: &J| v.to_string().trim_matches('"').to_string();
    if let Some(options) = p.get("enum").and_then(J::as_array) {
        let shown = match (&now, default) {
            (Some(v), _) => v.to_string().trim_matches('"').to_string(),
            (None, Some(d)) => format!("{} (default)", plain(d)),
            (None, None) => String::new(),
        };
        egui::ComboBox::from_id_salt(("setting", key))
            .selected_text(shown)
            .width(160.0)
            .show_ui(ui, |ui| {
                for o in options {
                    let v = match o {
                        J::String(s) => toml::Value::String(s.clone()),
                        J::Bool(b) => toml::Value::Boolean(*b),
                        J::Number(n) => match n.as_i64() {
                            Some(i) => toml::Value::Integer(i),
                            None => toml::Value::Float(n.as_f64().unwrap_or_default()),
                        },
                        _ => continue,
                    };
                    if ui
                        .selectable_label(now.as_ref() == Some(&v), plain(o))
                        .clicked()
                    {
                        table.insert(key.to_string(), v);
                    }
                }
            })
            .response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &label));
        return;
    }
    let kind = p.get("type").and_then(J::as_str);
    match kind {
        Some("number") | Some("integer") => {
            let whole = kind == Some("integer");
            let lo = p.get("minimum").and_then(J::as_f64);
            let hi = p.get("maximum").and_then(J::as_f64);
            let mut x = now
                .as_ref()
                .and_then(|v| v.as_float().or(v.as_integer().map(|i| i as f64)))
                .or(default.and_then(J::as_f64))
                .unwrap_or(lo.unwrap_or(0.0));
            let step = if whole { 1.0 } else { 0.01 };
            let r = match (lo, hi) {
                (Some(lo), Some(hi)) => {
                    // The handle grows with the row height, so it gets a row of its own size
                    // and egui's usual handle.
                    let size = egui::vec2(ui.spacing().slider_width, ui.spacing().interact_size.y);
                    let center = egui::Layout::left_to_right(egui::Align::Center);
                    let rail = ui
                        .allocate_ui_with_layout(size, center, |ui| {
                            ui.spacing_mut().interact_size.y = 18.0;
                            ui.add(
                                egui::Slider::new(&mut x, lo..=hi)
                                    .step_by(step)
                                    .show_value(false),
                            )
                        })
                        .inner;
                    let digits = if whole { 0 } else { 2 };
                    let value = ui.add(
                        egui::DragValue::new(&mut x)
                            .range(lo..=hi)
                            .speed(step)
                            .fixed_decimals(digits),
                    );
                    value.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::SpinButton, true, &label)
                    });
                    rail | value
                }
                _ => ui.add(egui::DragValue::new(&mut x).speed(step)),
            };
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Slider, true, &label));
            if r.changed() {
                let v = if whole {
                    toml::Value::Integer(x.round() as i64)
                } else {
                    toml::Value::Float((x * 100.0).round() / 100.0)
                };
                table.insert(key.to_string(), v);
            }
        }
        Some("boolean") => {
            let mut b = now
                .as_ref()
                .and_then(toml::Value::as_bool)
                .or(default.and_then(J::as_bool))
                .unwrap_or(false);
            let r = ui.checkbox(&mut b, "");
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::CheckBox, true, &label));
            if r.changed() {
                table.insert(key.to_string(), toml::Value::Boolean(b));
            }
        }
        _ => {
            let mut s = match &now {
                Some(toml::Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            };
            let hint = default.map(plain).unwrap_or_default();
            let r = ui.add(
                egui::TextEdit::singleline(&mut s)
                    .hint_text(RichText::new(hint).color(theme::DUST))
                    .desired_width(200.0),
            );
            if r.changed() {
                table.insert(key.to_string(), toml::Value::String(s));
            }
        }
    }
}

/// Framed so it reads as a button under a picture, in red as it takes something away.
/// A grid label for a cell several rows tall, beside the first row rather than the middle.
fn top_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.vertical(|ui| {
        let row = egui::vec2(0.0, ui.spacing().interact_size.y);
        let center = egui::Layout::left_to_right(egui::Align::Center);
        ui.allocate_ui_with_layout(row, center, |ui| ui.weak(text))
            .inner
    })
    .inner
}

// No fill of its own, so it lights up on hover like every other button.
fn remove_button(ui: &mut egui::Ui) -> egui::Response {
    ui.button(RichText::new("Remove").small().color(theme::FLAG))
}

/// The one button that spends money: tungsten, and still lit on hover and press.
pub(crate) fn spend_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        for (state, fill) in [
            (&mut w.inactive, theme::TUNGSTEN),
            (&mut w.hovered, theme::TUNGSTEN_LIT),
            (&mut w.active, theme::TUNGSTEN_DEEP),
        ] {
            state.weak_bg_fill = fill;
            state.bg_fill = fill;
            state.fg_stroke.color = theme::RACK;
        }
        ui.button(text)
    })
    .inner
}

/// A take's button: the number is in the row already, but screen readers and tests get it too.
fn act(ui: &mut egui::Ui, text: &str, take: usize) -> egui::Response {
    let r = ui.button(text);
    r.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{text} #{take}"))
    });
    r
}

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}

/// Opens the film, or without one a page to make or pick one.
pub fn run(
    project: Option<Project>,
    user_dir: PathBuf,
    plugins: Vec<Description>,
    host: Arc<dyn Host>,
) -> anyhow::Result<()> {
    let title = start::title(project.as_ref());
    let here = std::env::current_dir()?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 720.0]),
        ..Default::default()
    };
    eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            let shell = Shell::new(project, user_dir, &here).with_plugins(plugins, Some(host));
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            let shell = shell.with_native_menu(cc);
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            let _ = cc;
            Ok(Box::new(shell))
        }),
    )
    .map_err(|e| anyhow::anyhow!("can't open the window: {e}"))
}

/// Videos in `dir`, newest first.
fn renders(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut files: Vec<_> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "mp4"))
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    files.into_iter().map(|(_, p)| p).collect()
}

/// Room round a tab's content, and round the lists down the side.
const PAGE: egui::Vec2 = egui::vec2(28.0, 20.0);
const SIDE: egui::Vec2 = egui::vec2(8.0, 12.0);

/// Scrolls `add` with the bar at the panel's edge: the panel has no margin, the content does.
fn page<R>(ui: &mut egui::Ui, margin: egui::Vec2, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new().inner_margin(margin).show(ui, add).inner
        })
        .inner
}

/// "9:16 tall", so the shape reads without working out the ratio.
pub(crate) fn shape(aspect: &str) -> String {
    let what = match aspect {
        "1:1" => "square",
        "9:16" => "tall",
        "16:9" => "wide",
        "21:9" => "cinema",
        "adaptive" => "",
        _ => match aspect
            .split_once(':')
            .map(|(w, h)| (w.parse::<f32>(), h.parse::<f32>()))
        {
            Some((Ok(w), Ok(h))) if w > h => "landscape",
            Some((Ok(w), Ok(h))) if w < h => "portrait",
            _ => "",
        },
    };
    if what.is_empty() {
        aspect.to_string()
    } else {
        format!("{aspect} {what}")
    }
}

/// Shapes offered when a model doesn't list its own.
pub(crate) const SHAPES: [&str; 6] = ["9:16", "16:9", "1:1", "4:3", "3:4", "21:9"];

/// The Format section of the Story tab: what every shot's video is made with and at.
fn format_fields(
    ui: &mut egui::Ui,
    f: &mut shotline_core::Format,
    models: &[(String, ModelInfo)],
    made: usize,
) {
    ui.label(RichText::new("Format").text_style(egui::TextStyle::Heading));
    ui.add_space(8.0);
    let spec = models
        .iter()
        .find(|(p, m)| *p == f.plugin && m.id == f.model)
        .map(|(_, m)| m);
    let with = |list: &[String], now: &str| -> Vec<String> {
        let mut out: Vec<String> = if list.is_empty() {
            SHAPES.map(String::from).to_vec()
        } else {
            list.to_vec()
        };
        if !out.iter().any(|x| x == now) {
            out.insert(0, now.to_string());
        }
        out
    };
    egui::Grid::new("format")
        .num_columns(2)
        .spacing(egui::vec2(12.0, 8.0))
        .show(ui, |ui| {
            let l = ui.weak("Video model");
            egui::ComboBox::from_id_salt("format model")
                .selected_text(format!("{} · {}", f.plugin, f.model))
                .width(360.0)
                .show_ui(ui, |ui| {
                    for (p, m) in models {
                        let on = *p == f.plugin && m.id == f.model;
                        if ui.selectable_label(on, format!("{p} · {}", m.id)).clicked() {
                            f.plugin = p.clone();
                            f.model = m.id.clone();
                        }
                    }
                })
                .response
                .labelled_by(l.id);
            ui.end_row();

            let l = ui.weak("Shape");
            ui.horizontal(|ui| {
                let aspects = with(spec.map_or(&[][..], |m| &m.aspects), &f.aspect);
                egui::ComboBox::from_id_salt("format aspect")
                    .selected_text(shape(&f.aspect))
                    .width(180.0)
                    .show_ui(ui, |ui| {
                        for a in &aspects {
                            ui.selectable_value(&mut f.aspect, a.clone(), shape(a));
                        }
                    })
                    .response
                    .labelled_by(l.id);
                ui.add_space(8.0);
                let l = ui.weak("Resolution");
                match spec.filter(|m| !m.resolutions.is_empty()) {
                    Some(m) => {
                        let list = with(&m.resolutions, &f.resolution);
                        egui::ComboBox::from_id_salt("format resolution")
                            .selected_text(&f.resolution)
                            .width(110.0)
                            .show_ui(ui, |ui| {
                                for r in &list {
                                    ui.selectable_value(&mut f.resolution, r.clone(), r);
                                }
                            })
                            .response
                            .labelled_by(l.id);
                    }
                    None => {
                        ui.add(egui::TextEdit::singleline(&mut f.resolution).desired_width(96.0))
                            .labelled_by(l.id);
                    }
                }
            });
            ui.end_row();

            let l = ui.weak("Shot length");
            ui.add(
                egui::DragValue::new(&mut f.duration)
                    .range(1.0..=60.0)
                    .speed(0.1)
                    .max_decimals(1)
                    .suffix(" s"),
            )
            .on_hover_text("for shots that don't set their own")
            .labelled_by(l.id);
            ui.end_row();
        });
    if let Some(m) = spec {
        for (v, list) in [(&f.aspect, &m.aspects), (&f.resolution, &m.resolutions)] {
            if !list.is_empty() && !list.contains(v) {
                ui.label(RichText::new(format!("{} doesn't do {v}", m.id)).color(theme::FLAG));
            }
        }
    } else if !models.is_empty() {
        ui.label(RichText::new(format!("{} isn't installed", f.plugin)).color(theme::FLAG));
    }
    if made > 0 {
        hint(
            ui,
            &format!(
                "{made} shot{} already {} a take. Takes keep the shape and size they were made at.",
                if made == 1 { "" } else { "s" },
                if made == 1 { "has" } else { "have" }
            ),
        );
    }
}
