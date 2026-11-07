//! The desktop app: browse a film's story, elements, shots, takes and costs, and play takes and
//! the cut. It reads and writes the project folder through shotline-core like the CLI does, so
//! both always agree.

mod anchor;
mod browse;
mod cost;
mod cut;
mod edit;
mod git;
mod grouped;
mod lanes;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod menu;
mod pieces;
mod player;
mod prefs;
mod shoot;
mod start;
mod theme;
mod voice;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use shotline_core::ledger::money;
use shotline_core::protocol::{Description, EditInfo, ModelInfo, Op, VoiceInfo};
use shotline_core::{
    Chapter, CutShot, Element, Git, JobStatus, Kind, LIP_SYNCED, Project, SPOKEN, Shot, ShotRef,
    Take, Timeline, Transition,
};
use shotline_media::Ffmpeg;

use edit::{Action, ElementForm, Form, History, NewElement, Texts};
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
    /// Every image made and not thrown out, newest first.
    images: Vec<shotline_core::MadeImage>,
    /// Takes whose last frame was taken, with it, newest first.
    last_frames: Vec<(Take, PathBuf)>,
    /// Made images in use somewhere, by their path: where.
    in_use: HashMap<PathBuf, String>,
}

/// Editor projects, and problems asking the plugins for them.
type Edits = (Vec<EditInfo>, Vec<String>);

/// What the app asks of installed plugins. The CLI provides it; the app calls it off the UI thread.
pub trait Host: Send + Sync {
    /// The installed plugins, and problems with ones that couldn't be used.
    fn plugins(&self) -> (Vec<PluginInfo>, Vec<String>);
    /// Saves `exec` as the plugin's program in the user's plugins folder. Returns a line about it.
    fn pin(&self, name: &str, exec: &Path) -> anyhow::Result<String>;
    /// Takes that away again; PATH decides.
    fn unpin(&self, name: &str) -> anyhow::Result<String>;
    fn voices(&self, plugin: &str) -> anyhow::Result<Vec<VoiceInfo>>;
    /// What's left on the account, said in a line. It costs nothing, so it checks a key.
    fn balance(&self, plugin: &str) -> anyhow::Result<String>;
    /// The film's projects in video editors, from the editor plugins, and problems asking them.
    /// Free.
    fn sequences(&self, _film: &Path) -> (Vec<EditInfo>, Vec<String>) {
        (vec![], vec![])
    }
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
    /// What recording line `line` (from 0) on its own would cost. Free.
    fn say_quote(&self, film: &Path, shot: ShotRef, line: usize) -> anyhow::Result<String>;
    /// Records the line, if the price is still `at`, and picks it for the shot's takes.
    fn say(&self, film: &Path, shot: ShotRef, line: usize, at: &str) -> anyhow::Result<PathBuf>;
    /// What a take of the shot would cost, e.g. "0.40 USD". Free.
    /// `translate` puts the prompt into English first, if it isn't and that isn't done yet.
    fn take_quote(&self, film: &Path, shot: ShotRef, translate: bool) -> anyhow::Result<String>;
    /// Shoots a take, if the price is still `at`, telling `on` how it goes. Returns a line
    /// about it.
    fn take(
        &self,
        film: &Path,
        shot: ShotRef,
        at: &str,
        translate: bool,
        on: &mut dyn FnMut(String),
    ) -> anyhow::Result<String>;
}

/// One installed plugin as the Plugins window shows it.
#[derive(Clone, Debug)]
pub struct PluginInfo {
    pub name: String,
    pub runs: Runs,
    /// Programs of the same name on PATH that aren't used.
    pub skipped: Vec<PathBuf>,
    pub description: Result<Description, String>,
    /// Keys it needs that aren't set.
    pub missing: Vec<String>,
}

/// Where a plugin's program comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum Runs {
    Builtin,
    /// Found on PATH.
    Path(PathBuf),
    /// Kept in the user's plugins folder, pointing at this program.
    Pinned(PathBuf),
    /// A plugin folder of its own.
    Folder(PathBuf),
}

/// A new image made by an image model: an element's anchor, or a picture for a shot.
#[derive(Clone, Debug, PartialEq)]
pub struct AnchorAsk {
    pub handle: String,
    /// Set for a shot's picture; `handle` is empty then.
    pub shot: Option<ShotRef>,
    /// The shot's storyboard frame, made from its frame prompt rather than `prompt`.
    pub frame: bool,
    /// Added to the element's description.
    pub prompt: String,
    pub plugin: String,
    pub model: String,
    /// The images that go along, named Image 1, Image 2… in this order.
    pub refs: Vec<PathBuf>,
    /// None for [image] in shotline.toml.
    pub aspect: Option<String>,
    pub resolution: Option<String>,
    /// Put the prompt into English with the film's text model first.
    pub translate: bool,
    /// Ask for the references' clothes too; off lets them change.
    pub outfit: bool,
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
    /// A take open in its own popup, with a player when it could be opened.
    watching: Option<(Take, Option<Deck>)>,
    /// Making a take from pieces of others.
    splicer: Option<pieces::Splicer>,
    /// The open shot's selected take under its title, by take id; None in it when it can't play.
    in_cut: Option<(String, Option<Deck>)>,
    /// Posters of takes still being taken from their videos.
    posters: Arc<Mutex<HashSet<PathBuf>>>,
    film: Film,
    tab: Tab,
    /// The Cut page shows this editor sequence, by file and name, instead of the film's cut.
    cut_view: Option<(PathBuf, String)>,
    /// The film's projects in video editors as the editor plugins last read them, and problems
    /// asking them.
    edits: Edits,
    /// The takes the clips in them play, by file.
    edit_takes: HashMap<PathBuf, Take>,
    /// Being asked again in the background; the answer lands here.
    edits_asked: Option<Arc<Mutex<Option<Edits>>>>,
    /// The film was read again, so the editors are asked again.
    edits_stale: bool,
    shot: Option<ShotRef>,
    takes: Vec<Take>,
    status: Option<(String, bool)>,
    /// The job that made the zoomed picture, once Details is asked for.
    zoom_details: Option<(PathBuf, Option<shotline_core::Job>)>,
    /// How far the zoomed picture is pinched in, and the middle of what shows, 0-1 across it.
    zoom_view: (f32, egui::Pos2),
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
    /// The chapter whose title is being typed, and what's typed.
    renaming: Option<(u32, String)>,
    /// The open element, by handle.
    element: Option<String>,
    element_form: ElementForm,
    new_element: NewElement,
    /// The picture shown big over everything.
    zoom: Option<PathBuf>,
    plugins: Vec<Description>,
    host: Option<Arc<dyn Host>>,
    /// A plugin's keys asked for, e.g. one that said it has none; the shell opens Preferences.
    pub(crate) wants_keys: Option<String>,
    maker: Option<anchor::Maker>,
    /// The open shot as it would be sent; None once anything is read again.
    built: Option<(ShotRef, Result<shotline_core::Built, String>)>,
    /// Ticked: a shot's prompt goes into English before its next take.
    translate_prompt: bool,
    shoot: Arc<Mutex<shoot::Shoot>>,
    say: Arc<Mutex<voice::Say>>,
    /// What each speaker in the open shot has recorded.
    voices: HashMap<String, Vec<shotline_core::Job>>,
    /// The line whose recordings dropdown is open.
    voice_open: Option<(ShotRef, usize)>,
    /// That dropdown shows other shots' recordings too.
    show_all: bool,
    line_drag: Option<voice::LineDrag>,
    drags: u64,
    /// Where each line's row was last laid out, to tell where a dragged one lands.
    line_rects: Vec<egui::Rect>,
    /// How tall each line's own row is, without the player under it.
    line_heads: Vec<f32>,
    hearing: Option<voice::Hearing>,
    /// A line's recording stopped part way, and where.
    paused: Option<(String, f64)>,
    /// A line being recorded again, and the recording the new one replaces.
    remaking: Option<(ShotRef, usize, String)>,
    /// Recordings' lengths, as ffprobe gave them.
    lengths: HashMap<PathBuf, f64>,
    /// Clips' lengths for the Cut page, probed in the background; None while it's probing or
    /// when it can't tell.
    clip_lengths: Arc<Mutex<HashMap<PathBuf, Option<f64>>>>,
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
pub(crate) struct Thumbs {
    done: HashMap<(PathBuf, u32), Option<egui::TextureHandle>>,
    decoding: Arc<Mutex<HashMap<(PathBuf, u32), Decoded>>>,
    /// When each file was last changed as it was decoded, to tell which to decode again.
    stamps: HashMap<PathBuf, Option<std::time::SystemTime>>,
}

fn stamp(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// None while the thread is still at it, then what it made of the file.
type Decoded = Option<Option<egui::ColorImage>>;

pub(crate) enum Big {
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

pub(crate) const THUMB: u32 = 512;
const BIG: u32 = 2048;
/// The zoomed picture seen whole.
const FIT: (f32, egui::Pos2) = (1.0, egui::pos2(0.5, 0.5));
/// An anchor's card on an element's page, two to a row; the picture's box is the same for all.
const ANCHOR_CARD: f32 = 290.0;
const ANCHOR_IMAGE: egui::Vec2 = egui::vec2(128.0, 96.0);
/// Enough for "side view, red dress" without filling the list it's picked from.
const LABEL_MAX: usize = 40;

impl Thumbs {
    /// Drops the pictures whose files changed (or went) since they were decoded. Decoding is
    /// slow, so the rest are kept across reloads.
    fn forget_changed(&mut self) {
        let changed: Vec<PathBuf> = self
            .stamps
            .iter()
            .filter(|(p, s)| stamp(p) != **s)
            .map(|(p, _)| p.clone())
            .collect();
        for p in changed {
            self.stamps.remove(&p);
            self.done.retain(|(q, _), _| *q != p);
        }
    }

    fn noted(&mut self, path: &Path) {
        self.stamps.insert(path.to_path_buf(), stamp(path));
    }

    fn texture(
        &mut self,
        ui: &egui::Ui,
        path: &Path,
        px: u32,
        name: &str,
    ) -> Option<egui::TextureHandle> {
        if !self.done.contains_key(&(path.to_path_buf(), px)) {
            self.noted(path);
        }
        self.done
            .entry((path.to_path_buf(), px))
            .or_insert_with(|| {
                let pixels = decode(path, px)?;
                Some(ui.ctx().load_texture(name, pixels, Default::default()))
            })
            .clone()
    }

    /// Like `texture`, but decoded on another thread, asking for a repaint when it's there.
    pub(crate) fn texture_later(&mut self, ui: &egui::Ui, path: &Path, px: u32, name: &str) -> Big {
        let key = (path.to_path_buf(), px);
        if let Some(t) = self.done.get(&key) {
            return t.clone().map_or(Big::Failed, Big::Ready);
        }
        let mut decoding = self.decoding.lock().unwrap();
        match decoding.get_mut(&key) {
            None => {
                self.stamps.insert(key.0.clone(), stamp(&key.0));
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

    /// A thumbnail box, the picture whole and centred on black, so tall and wide ones line up.
    /// Clicking it asks for the picture full size.
    pub(crate) fn tile(
        &mut self,
        ui: &mut egui::Ui,
        path: &Path,
        side: egui::Vec2,
        name: &str,
    ) -> bool {
        let (rect, r) = ui.allocate_exact_size(side, egui::Sense::click());
        ui.painter().rect_filled(rect, 2.0, egui::Color32::BLACK);
        match self.texture_later(ui, path, THUMB, name) {
            Big::Decoding => {
                egui::Spinner::new().size(16.0).paint_at(
                    ui,
                    egui::Rect::from_center_size(rect.center(), egui::vec2(16.0, 16.0)),
                );
            }
            Big::Ready(t) => {
                let size = t.size_vec2();
                let k = (side.x / size.x).min(side.y / size.y);
                ui.painter().image(
                    t.id(),
                    egui::Rect::from_center_size(rect.center(), size * k),
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
            Big::Failed => {
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "can't show it",
                    egui::TextStyle::Small.resolve(ui.style()),
                    theme::DUST,
                );
            }
        }
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, name));
        r.on_hover_cursor(egui::CursorIcon::ZoomIn)
            .on_hover_text("click to see it bigger")
            .clicked()
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
    /// The sound output, opened the first time it's wanted.
    fn audio(&mut self) -> Option<&Audio> {
        if !self.audio_tried {
            self.audio_tried = true;
            self.audio = Audio::open();
        }
        self.audio.as_ref()
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
            watching: None,
            splicer: None,
            in_cut: None,
            posters: Arc::default(),
            film: Film::default(),
            tab: Tab::Shots,
            cut_view: None,
            edits: (vec![], vec![]),
            edit_takes: HashMap::new(),
            edits_asked: None,
            edits_stale: true,
            shot: None,
            takes: vec![],
            status: None,
            zoom_details: None,
            zoom_view: FIT,
            stuck: vec![],
            thumbs: Thumbs::default(),
            place: None,
            themed: false,
            form: Form::default(),
            texts: Texts::default(),
            new_chapter: String::new(),
            renaming: None,
            element: None,
            element_form: ElementForm::default(),
            new_element: NewElement::default(),
            zoom: None,
            plugins: vec![],
            host: None,
            wants_keys: None,
            maker: None,
            built: None,
            translate_prompt: false,
            shoot: Arc::default(),
            say: Arc::default(),
            voices: HashMap::new(),
            voice_open: None,
            show_all: false,
            line_drag: None,
            drags: 0,
            line_rects: vec![],
            line_heads: vec![],
            hearing: None,
            paused: None,
            remaking: None,
            lengths: HashMap::new(),
            clip_lengths: Arc::default(),
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
        let act = match &m.target {
            anchor::For::Element(h) => self
                .project
                .element(h)
                .map(|el| Action::AddAnchor(el, out.clone())),
            anchor::For::Shot(r) => self
                .project
                .shot(*r)
                .map(|_| Action::AddShotImage(*r, out.clone())),
            anchor::For::Frame(r) => self
                .project
                .shot(*r)
                .map(|_| Action::SetFrame(*r, Some(out.clone()))),
        };
        match act {
            Ok(a) => {
                self.maker = None;
                self.flush();
                self.apply(vec![a]);
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

    /// The Make image panel, open for whatever it's making.
    fn show_maker(&mut self, ui: &mut egui::Ui, models: &[anchor::ImageModel]) {
        let pool = self.anchor_pool();
        let translator = self.translator();
        let wants = self.maker.as_mut().unwrap().show(
            ui,
            models,
            &pool,
            &mut self.thumbs,
            self.host.clone(),
            &self.project.root,
            translator,
        );
        let host = self.host.clone();
        // A frame is made from the shot's frame prompt, so what's typed is saved first.
        if let anchor::Wants::Price(ask) | anchor::Wants::Make(ask, _) = &wants
            && ask.frame
            && ask.shot == self.form.shot
        {
            self.form.frame_prompt = ask.prompt.clone();
        }
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
            (anchor::Wants::Close, _) => self.maker = None,
            _ => {}
        }
    }

    /// The Make panel again for a picture made here: what was typed for it, the pictures that
    /// went along and the model, ready to change and make once more.
    pub(crate) fn remake(
        &mut self,
        target: anchor::For,
        file: &Path,
        defaults: (String, Option<String>),
    ) {
        let Some(job) = self.project.job_that_made(file).ok().flatten() else {
            let name = self.project.rel(file);
            self.error(format!(
                "{name} wasn't made here, so there's nothing to make again"
            ));
            return;
        };
        let models = self.image_models();
        let refs = job
            .inputs
            .iter()
            .map(|i| self.project.root.join(i))
            .filter(|p| p.is_file())
            .collect();
        self.maker = Some(
            anchor::Maker::new(target, refs, &models, (&job.plugin, &job.model), defaults)
                .with_prompt(job.brief.as_deref().unwrap_or_default())
                .translating(job.english.is_some()),
        );
    }

    /// A shot's picture's shape and resolution, unless picked: the shot's model's, else the film's.
    pub(crate) fn image_defaults(&self, s: &Shot) -> (String, Option<String>) {
        let film = &self.project.config;
        let aspect = s
            .model
            .as_ref()
            .and_then(|m| m.aspect.clone())
            .unwrap_or_else(|| film.aspect.clone());
        (aspect, film.image.resolution.clone())
    }

    /// What an image is being made or priced for, e.g. "@paka" or "01-010".
    fn making_for(&self) -> Option<String> {
        let m = self.maker.as_ref().filter(|m| m.busy())?;
        Some(match &m.target {
            anchor::For::Element(h) => format!("@{h}"),
            anchor::For::Shot(r) | anchor::For::Frame(r) => r.to_string(),
        })
    }

    /// The film's translating model as "plugin · model", when it's installed and its keys are
    /// set.
    fn translator(&self) -> Option<String> {
        let t = &self
            .project
            .config
            .model_for(shotline_core::Task::Translate)?;
        let d = self.plugins.iter().find(|d| d.name == t.plugin)?;
        d.models
            .iter()
            .find(|m| m.op == Op::TextGenerate && m.id == t.model)?;
        let saved = shotline_core::dotenv::read(&shotline_core::dotenv::path(&self.user_dir));
        let set = |v: &String| std::env::var_os(v).is_some() || saved.iter().any(|(n, _)| n == v);
        d.env
            .iter()
            .all(set)
            .then(|| format!("{} · {}", t.plugin, t.model))
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
    /// What the image panel can send along: the elements' anchors, and the pictures made for
    /// shots, the panel's own shot's first.
    fn anchor_pool(&self) -> Vec<anchor::Pooled> {
        let here = self.maker.as_ref().and_then(|m| match m.target {
            anchor::For::Shot(r) | anchor::For::Frame(r) => Some(r),
            anchor::For::Element(_) => None,
        });
        let names = self.shot_names();
        let mut shots = vec![];
        for (_, list) in &self.film.chapters {
            for (r, s) in list {
                let group = names.get(r).cloned().unwrap_or_default();
                let near = Some(*r) == here;
                let mut add = |name: String, file: &str| {
                    let path = self.project.root.join(file);
                    if path.is_file() {
                        shots.push(anchor::Pooled {
                            name,
                            path,
                            group: group.clone(),
                            near,
                        });
                    }
                };
                if let Some(f) = &s.frame {
                    add("Storyboard frame".into(), f);
                }
                for (i, img) in s.images.iter().enumerate() {
                    let mut name = format!("Picture {}", i + 1);
                    if !img.note.trim().is_empty() {
                        name.push_str(&format!("  {}", img.note.trim()));
                    }
                    add(name, &img.file);
                }
            }
        }
        // Stable, so shots stay in film order within each part.
        shots.sort_by_key(|p| !p.near);
        let split = shots.iter().take_while(|p| p.near).count();
        let rest = shots.split_off(split);
        let mut pool = shots;
        for (el, anchors) in &self.film.elements {
            for (i, a) in anchors.iter().enumerate() {
                pool.push(anchor::Pooled {
                    name: anchor_name(el, i, a),
                    path: a.clone(),
                    group: ("Elements".into(), format!("@{}  {}", el.handle, el.name)),
                    near: true,
                });
            }
        }
        pool.extend(rest);
        let near = match self.maker.as_ref().map(|m| &m.target) {
            Some(anchor::For::Element(h)) => browse::Near::Element(h.clone()),
            _ => browse::Near::Shot(here.unwrap_or(ShotRef {
                chapter: 0,
                shot: 0,
            })),
        };
        let used: HashSet<PathBuf> = pool.iter().map(|p| p.path.clone()).collect();
        for (top, group, e) in self.made_rows(&near) {
            if !used.contains(&e.path) {
                pool.push(anchor::Pooled {
                    near: top == browse::MINE,
                    name: e.name,
                    path: e.path,
                    group: (top, group),
                });
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
            || self
                .in_cut
                .as_ref()
                .and_then(|(_, d)| d.as_ref())
                .is_some_and(Deck::is_playing)
            || self
                .watching
                .as_ref()
                .and_then(|(_, d)| d.as_ref())
                .is_some_and(Deck::is_playing)
    }

    fn reload(&mut self) {
        self.thumbs.forget_changed();
        self.built = None;
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
            in_use: HashMap::new(),
            images: p.made_images()?,
            last_frames: p.last_frames()?,
        };
        self.film.in_use = in_use(&self.film, &self.project.root);
        self.edits_stale = true;
        self.load_takes()
    }

    fn load_takes(&mut self) -> anyhow::Result<()> {
        self.takes = match self.shot {
            Some(r) => self.project.takes(r)?,
            None => vec![],
        };
        self.load_recordings()
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
        if self.ff.is_none() {
            self.error("can't play without ffmpeg; install it or set SHOTLINE_FFMPEG".into());
            return None;
        }
        self.audio();
        let ff = self.ff.as_ref()?;
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

    fn deck(&mut self, ui: &mut egui::Ui) {
        if let Some(d) = &mut self.deck {
            if !d.show(ui, self.audio.as_ref()) {
                self.deck = None;
            } else if d.is_playing() {
                // It took the sound over.
                self.hearing = None;
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
        self.shoot_finished();
        self.said();
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
                        ui.add_space(12.0);
                        // What's left, for the message: cut short with … when it doesn't fit.
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            if msg.is_empty() {
                                return;
                            }
                            let color = if err { theme::FLAG } else { theme::GRAPHITE };
                            ui.scope(|ui| {
                                ui.set_max_width(ui.available_width() - 30.0);
                                ui.add(
                                    egui::Label::new(RichText::new(&msg).color(color)).truncate(),
                                )
                                .on_hover_text(&msg);
                            });
                            if icon_button(ui, "Copy the message", copy_icon)
                                .on_hover_text("Copy")
                                .clicked()
                            {
                                ui.ctx().copy_text(msg.clone());
                            }
                        });
                    });
                });
            });
        if self.tab != tab
            && !matches!(self.tab, Tab::Shots | Tab::Cut)
            && let Some(d) = &mut self.deck
        {
            d.pause(self.audio.as_ref());
        }
        // Only on the Shots tab; it doesn't go on playing out of sight.
        if self.tab != Tab::Shots
            && let Some((_, Some(d))) = &mut self.in_cut
            && d.is_playing()
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
        self.watched(ui);
        self.splicing(ui);
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
        // Room for the bottom row, and for the details under it when they're open.
        let details = self.zoom_details.as_ref().is_some_and(|(p, _)| *p == path);
        let max = room - egui::vec2(0.0, if details { 330.0 } else { 40.0 });
        let max = max - egui::vec2(0.0, 28.0);
        let modal = crate::modal(ui.ctx(), "zoom", |ui| {
            let bar = title_bar(ui, &name);
            match self.thumbs.texture_later(ui, &path, BIG, &name) {
                Big::Ready(t) => {
                    let size = t.size_vec2();
                    let size = size * (max / size).min_elem().min(1.0);
                    let (rect, img) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
                    let (scale, mid) = &mut self.zoom_view;
                    if img.hovered() {
                        let (pinch, scroll, at) = ui.input(|i| {
                            (i.zoom_delta(), i.smooth_scroll_delta, i.pointer.hover_pos())
                        });
                        if pinch != 1.0 {
                            // The spot under the fingers stays put.
                            let off = (at.unwrap_or(rect.center()) - rect.center()) / rect.size();
                            let under = *mid + off / *scale;
                            *scale = (*scale * pinch).clamp(1.0, 8.0);
                            *mid = under - off / *scale;
                        } else if *scale > 1.0 {
                            *mid -= scroll / rect.size() / *scale;
                        }
                    }
                    *mid -= img.drag_delta() / rect.size() / *scale;
                    let half = 0.5 / *scale;
                    *mid = mid.clamp(egui::pos2(half, half), egui::pos2(1.0 - half, 1.0 - half));
                    ui.painter().image(
                        t.id(),
                        rect,
                        egui::Rect::from_center_size(*mid, egui::Vec2::splat(2.0 * half)),
                        egui::Color32::WHITE,
                    );
                    let zoomed = *scale > 1.0;
                    if img.double_clicked() {
                        self.zoom_view = FIT;
                    }
                    // Clicking closes it, unless it's pinched in.
                    close = img.clicked() && !zoomed;
                    img.widget_info(|| {
                        egui::WidgetInfo::labeled(egui::Role::Image, true, format!("{name} big"))
                    });
                    let img = if zoomed {
                        img.on_hover_cursor(egui::CursorIcon::Grab)
                    } else {
                        img
                    };
                    img.on_hover_text(if zoomed {
                        "drag to look around; double-click to see it whole"
                    } else {
                        "pinch to zoom in; click to close"
                    });
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
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let open = self.zoom_details.as_ref().is_some_and(|(p, _)| *p == path);
                        let label = if open { "Hide details" } else { "Details" };
                        if ui
                            .button(label)
                            .on_hover_text("what made it, and the prompt")
                            .clicked()
                        {
                            self.zoom_details = if open {
                                None
                            } else {
                                Some((
                                    path.clone(),
                                    self.project.job_that_made(&path).ok().flatten(),
                                ))
                            };
                        }
                    });
                },
            );
            if let Some((p, job)) = &self.zoom_details
                && *p == path
            {
                ui.add_space(6.0);
                ui.allocate_ui(egui::vec2(width, 0.0), |ui| {
                    ui.set_width(width);
                    picture_details(ui, job.as_ref(), &self.project);
                });
            }
            close |= close_corner(ui, bar);
        });
        if close || modal.should_close() {
            self.zoom = None;
            self.zoom_details = None;
            self.zoom_view = FIT;
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
            // Rows worked out here: horizontal_wrapped can't tell a card's width before it's drawn.
            let gap = 8.0;
            let outer = ANCHOR_CARD + 14.0;
            let per_row = (((ui.available_width() + gap) / (outer + gap)) as usize).max(1);
            for row in anchors.chunks(per_row) {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for a in row {
                        if self.anchor_card(ui, el, a) {
                            remove = Some(a.clone());
                        }
                    }
                });
                ui.add_space(gap - ui.spacing().item_spacing.y);
            }
            if let Some(a) = remove {
                acts.push(Action::RemoveAnchor(el.clone(), a));
            }
            let models = self.image_models();
            let me = anchor::For::Element(el.handle.clone());
            let making = self.maker.as_ref().is_some_and(|m| m.target == me);
            let rows = self.made_rows(&browse::Near::Element(el.handle.clone()));
            ui.horizontal(|ui| {
                match browse::dropdown(
                    ui,
                    "Browse existing anchors",
                    ("element", &el.handle),
                    &rows,
                    &mut self.thumbs,
                    "an image made before, for this element or anything else",
                ) {
                    Some(browse::Browsed::Pick(f)) => acts.push(Action::AddAnchor(el.clone(), f)),
                    Some(browse::Browsed::Forget(id)) => acts.push(Action::ForgetImage(id)),
                    None => {}
                }
                if ui.button("Add image…").clicked()
                    && let Some(file) = rfd::FileDialog::new()
                        .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
                        .pick_file()
                {
                    acts.push(Action::AddAnchor(el.clone(), file));
                }
                let busy = self.making_for();
                if !models.is_empty()
                    && !making
                    && ui
                        .add_enabled(busy.is_none(), egui::Button::new("Make image…"))
                        .on_disabled_hover_text(format!(
                            "still making one for {}",
                            busy.unwrap_or_default()
                        ))
                        .clicked()
                {
                    let film = &self.project.config;
                    let cfg = &film.image;
                    self.maker = Some(anchor::Maker::new(
                        me.clone(),
                        vec![],
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
                self.show_maker(ui, &models);
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

    /// An anchor with its label beside it, all cards one size. True when Remove is clicked.
    fn anchor_card(&mut self, ui: &mut egui::Ui, el: &Element, a: &Path) -> bool {
        let name = self.project.rel(a);
        let file = a
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut remove = false;
        // The page's colour, so the label box shows as a box.
        egui::Frame::new()
            .fill(theme::BAY)
            .stroke(egui::Stroke::new(1.0, theme::LINE))
            .corner_radius(4.0)
            .inner_margin(6)
            .show(ui, |ui| {
                ui.set_width(ANCHOR_CARD);
                ui.horizontal_top(|ui| {
                    if self.thumbs.tile(ui, a, ANCHOR_IMAGE, &name) {
                        self.zoom = Some(a.to_path_buf());
                    }
                    let size = egui::vec2(ui.available_width(), ANCHOR_IMAGE.y);
                    ui.allocate_ui_with_layout(
                        size,
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_min_size(size);
                            let labels = &mut self.element_form.labels;
                            let mut text = labels.get(&file).cloned().unwrap_or_default();
                            let typed = ui
                                .add(
                                    egui::TextEdit::multiline(&mut text)
                                        .hint_text(
                                            RichText::new("what it shows, e.g. side view")
                                                .color(theme::DUST),
                                        )
                                        .char_limit(LABEL_MAX)
                                        .return_key(None)
                                        .desired_rows(3)
                                        .desired_width(f32::INFINITY),
                                )
                                .on_hover_text(format!(
                                    "shown with it when picking it from a list, up to {LABEL_MAX} \
                                 letters"
                                ));
                            typed.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::Role::MultilineTextInput,
                                    true,
                                    format!("Label of {name}"),
                                )
                            });
                            if typed.changed() {
                                let text = text.replace('\n', " ");
                                if text.is_empty() {
                                    labels.remove(&file);
                                } else {
                                    labels.insert(file.clone(), text);
                                }
                            }
                            // On the bottom edge, so it lines up across cards.
                            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                                remove = remove_button(ui)
                                    .on_hover_text(format!("take {name} off @{}", el.handle))
                                    .clicked();
                            });
                        },
                    );
                });
            });
        remove
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
        let film_tts = self.project.config.tts.clone();
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
                                v.model = film_tts
                                    .as_ref()
                                    .filter(|m| m.plugin == *p)
                                    .map(|m| m.model.clone())
                                    .unwrap_or_default();
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
                // Left empty, the film's voice model is used when it's on this plugin.
                let unset = match film_tts.as_ref().filter(|m| m.plugin == v.plugin) {
                    Some(m) => format!("the film's, {}", m.model),
                    None => "the plugin's default".into(),
                };
                egui::ComboBox::from_id_salt("voice model")
                    .selected_text(if v.model.is_empty() {
                        unset.as_str()
                    } else {
                        v.model.as_str()
                    })
                    .width(320.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut v.model, String::new(), &unset);
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
                settings_rows(ui, schema.as_ref(), &mut v.settings);
            });
        let none = v.plugin.is_empty();
        if keys {
            self.wants_keys = Some(v.plugin.clone());
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
                            if let Some(a) = chapter_title(ui, c, &mut self.renaming) {
                                acts.push(a);
                            }
                        });
                        for (r, s) in shots {
                            let takes = self.film.takes.get(r).copied().unwrap_or(0);
                            if shot_row(ui, *r, s, takes, self.shot == Some(*r)).clicked() {
                                open = Some(*r);
                            }
                        }
                        ui.horizontal(|ui| {
                            ui.add_space(4.0);
                            if ui
                                .button("Add shot")
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
        if self.in_cut_player(ui, s) {
            ui.add_space(14.0);
        }
        // Wide enough for References and its image panel.
        let side = 360.0;
        ui.horizontal_top(|ui| {
            let w = (ui.available_width() - side - 32.0).clamp(280.0, 620.0);
            ui.vertical(|ui| {
                ui.set_width(w);
                self.script(ui, r, s, &mut acts);
            });
            ui.add_space(32.0);
            ui.vertical(|ui| {
                ui.set_width(side);
                self.facts(ui, s);
                ui.add_space(20.0);
                self.references(ui, r, s, &mut acts);
            });
        });
        ui.add_space(20.0);
        ui.separator();
        ui.add_space(8.0);
        section(ui, "Shoot");
        ui.add_space(4.0);
        self.sent(ui, r);
        ui.add_space(20.0);
        self.shoot_row(ui, r);
        ui.add_space(20.0);
        ui.separator();
        ui.add_space(8.0);
        self.take_list(ui, s, &mut acts);
        ui.add_space(20.0);
        ui.separator();
        ui.add_space(8.0);
        if ui.button("Add a shot after this one").clicked() {
            acts.push(Action::AddShot(r.chapter, Some(r.shot)));
        }
        // These read the shot from disk, so what's typed goes first.
        if acts.iter().any(|a| {
            matches!(
                a,
                Action::AddShot(..)
                    | Action::LeaveOut(..)
                    | Action::SendAnchor(..)
                    | Action::MoveLine(..)
                    | Action::PickRecording(..)
                    | Action::DeleteRecording(..)
            )
        }) {
            self.flush();
        }
        self.apply(acts);
    }

    /// Plot, prompt and lines: what the shot says.
    fn script(&mut self, ui: &mut egui::Ui, r: ShotRef, s: &Shot, acts: &mut Vec<Action>) {
        let f = &mut self.form;
        explained_field(
            ui,
            "Plot",
            Some(
                "Your note of what happens, in any language. Not sent to the video model; drafts \
                 write the prompt from it.",
            ),
            &mut f.plot,
            "What happens in the shot, in any language",
        );
        ui.add_space(14.0);
        self.start(ui, r, s);
        ui.add_space(14.0);
        let f = &mut self.form;
        explained_field(
            ui,
            "Prompt",
            Some(
                "What the video model is sent. The references, style and other notes are added \
                 to it; see What the model gets under Shoot.",
            ),
            &mut f.prompt,
            "What the video model is sent, naming elements as @handles",
        );
        let f = &self.form;
        match &s.english {
            // Translated before and not changed since.
            Some(t) if t.from == f.prompt && !t.from.is_empty() => {
                hint(ui, &format!("Sent in English: {}", t.text));
            }
            _ => {
                let translator = self.translator();
                let prompt = self.form.prompt.clone();
                if anchor::translate_box(
                    ui,
                    &mut self.translate_prompt,
                    &prompt,
                    translator.as_deref(),
                ) {
                    // The price said doesn't hold any more.
                    *self.shoot.lock().unwrap() = shoot::Shoot::Idle;
                }
            }
        }
        let f = &mut self.form;
        if f.prompt.is_empty() && !f.plot.is_empty() {
            hint(
                ui,
                &format!("or draft one from the plot with shotline shot draft {r}"),
            );
        }
        ui.add_space(14.0);
        self.spoken(ui);
        if self.form.recipe.is_none() {
            match s.lines.len() {
                0 => {}
                1 => hint(ui, "It has a line, kept but not spoken."),
                n => hint(ui, &format!("It has {n} lines, kept but not spoken.")),
            }
            return;
        }
        ui.add_space(8.0);
        egui::Frame::new()
            .stroke(egui::Stroke::new(1.0, theme::LINE))
            .corner_radius(4.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.lines(ui, r, s, acts);
            });
    }

    /// Which recipe speaks the shot's lines.
    fn spoken(&mut self, ui: &mut egui::Ui) {
        let l = caption(ui, "Lines spoken");
        let f = &mut self.form;
        ui.horizontal(|ui| {
            // The film's own recipes too, unless they stand in for these.
            let mut picks: Vec<(Option<String>, String)> = vec![
                (None, "no".into()),
                (Some(SPOKEN.into()), "by the video model".into()),
                (Some(LIP_SYNCED.into()), "lip synced after".into()),
            ];
            for rc in self.project.config.recipes.keys() {
                if rc != SPOKEN && rc != LIP_SYNCED {
                    picks.push((Some(rc.clone()), format!("recipe {rc}")));
                }
            }
            if let Some(rc) = &f.recipe
                && !picks.iter().any(|(p, _)| p.as_ref() == Some(rc))
            {
                picks.push((Some(rc.clone()), format!("recipe {rc}")));
            }
            let now = picks
                .iter()
                .find(|(p, _)| *p == f.recipe)
                .map(|(_, n)| n.clone())
                .unwrap_or_default();
            egui::ComboBox::from_id_salt("recipe")
                .selected_text(now)
                .width(150.0)
                .show_ui(ui, |ui| {
                    for (p, name) in picks {
                        ui.selectable_value(&mut f.recipe, p, name);
                    }
                })
                .response
                .on_hover_text(
                    "by the video model: each line is made with the speaker's voice and sent \
                     along for the video model to lip sync. Lip synced after: the video is \
                     made as usual, then lip synced with WaveSpeed.",
                )
                .labelled_by(l.id);
        });
    }

    /// The shot's settings.
    fn facts(&mut self, ui: &mut egui::Ui, s: &Shot) {
        let models = self.video_models();
        let film = self.project.format();
        let mut flags = vec![];
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
                flags = shot_model(ui, f, &models, &film, s);
            });
        for flag in flags {
            ui.label(RichText::new(flag).color(theme::FLAG));
        }
    }

    /// How the shot starts: on a cut, with its storyboard frame if it has one, or carried on
    /// from the shot before, which needs that one's selected take.
    fn start(&mut self, ui: &mut egui::Ui, r: ShotRef, s: &Shot) {
        let prev = self.project.previous_shot(r).ok().flatten();
        let ready = prev.is_some_and(|p| {
            self.film
                .chapters
                .iter()
                .flat_map(|(_, s)| s)
                .any(|(x, s)| *x == p && s.selected_take.is_some())
        });
        let f = &mut self.form;
        let mut on = f.transition == Transition::Continue;
        // Left on, it can still be turned off.
        ui.add_enabled(
            on || ready,
            egui::Checkbox::new(&mut on, "Continue from previous shot"),
        )
        .on_disabled_hover_text(match prev {
            Some(p) => format!("{p} has no selected take yet"),
            None => "it's the first shot".into(),
        });
        f.transition = if on {
            Transition::Continue
        } else {
            Transition::Cut
        };
        if on {
            let why = match (prev, ready) {
                (Some(p), true) => {
                    format!("On the last frame of {p}'s selected take, colour matched in the cut.")
                }
                (Some(p), false) => {
                    format!("{p} has no selected take yet, so there's nothing to carry on from.")
                }
                (None, _) => {
                    "It's the first shot, so there's nothing before it to carry on from.".into()
                }
            };
            hint(ui, &why);
            return;
        }
        ui.add_space(8.0);
        egui::Frame::new()
            .stroke(egui::Stroke::new(1.0, theme::LINE))
            .corner_radius(4.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.frame(ui, r, s);
            });
    }

    /// The storyboard frame: a still the video starts on, and what it's made from.
    fn frame(&mut self, ui: &mut egui::Ui, r: ShotRef, s: &Shot) {
        heading(ui, "Storyboard frame").on_hover_text(
            "The first frame of the video, to settle the framing on a cheap still before paying \
             for video. Optional.",
        );
        ui.add_space(8.0);
        let mut acts = vec![];
        if let Some(f) = &s.frame {
            let path = self.project.root.join(f);
            if self.thumbs.show(ui, &path, 150.0, f) {
                self.zoom = Some(path);
            }
        }
        let models = self.image_models();
        let me = anchor::For::Frame(r);
        let making = self.maker.as_ref().is_some_and(|m| m.target == me);
        let rows = self.made_rows(&browse::Near::Shot(r));
        let mut remake = None;
        ui.horizontal_wrapped(|ui| {
            match browse::dropdown(
                ui,
                "Browse existing frames",
                ("frame", r.to_string()),
                &rows,
                &mut self.thumbs,
                "an image made before, for this shot or another",
            ) {
                Some(browse::Browsed::Pick(f)) => acts.push(Action::SetFrame(r, Some(f))),
                Some(browse::Browsed::Forget(id)) => acts.push(Action::ForgetImage(id)),
                None => {}
            }
            if ui.button("Add image…").clicked()
                && let Some(file) = rfd::FileDialog::new()
                    .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
                    .pick_file()
            {
                acts.push(Action::SetFrame(r, Some(file)));
            }
            let busy = self.making_for();
            if !models.is_empty()
                && !making
                && ui
                    .add_enabled(busy.is_none(), egui::Button::new("Make frame…"))
                    .on_disabled_hover_text(format!(
                        "still making one for {}",
                        busy.clone().unwrap_or_default()
                    ))
                    .clicked()
            {
                // The frame prompt and no pictures; Remake starts from the frame there.
                let prompt = self.form.frame_prompt.clone();
                let cfg = &self.project.config.image;
                let aspect = s
                    .model
                    .as_ref()
                    .and_then(|m| m.aspect.clone())
                    .unwrap_or_else(|| self.project.config.aspect.clone());
                self.maker = Some(
                    anchor::Maker::new(
                        me.clone(),
                        vec![],
                        &models,
                        (&cfg.plugin, &cfg.model),
                        (aspect, cfg.resolution.clone()),
                    )
                    .with_prompt(&prompt),
                );
            }
            if let Some(f) = &s.frame {
                if !models.is_empty()
                    && !making
                    && ui
                        .add_enabled(busy.is_none(), egui::Button::new("Remake…"))
                        .on_hover_text("make it again from what it was made with, to change first")
                        .clicked()
                {
                    remake = Some(self.project.root.join(f));
                }
                let rm = remove_button(ui);
                rm.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::Button, true, "Remove storyboard frame")
                });
                if rm.clicked() {
                    acts.push(Action::SetFrame(r, None));
                }
            }
        });
        if let Some(f) = remake {
            let defaults = self.image_defaults(s);
            self.remake(me, &f, defaults);
        }
        if making {
            ui.add_space(6.0);
            self.show_maker(ui, &models);
        }
        if !acts.is_empty() {
            self.flush();
            self.apply(acts);
        }
    }

    fn take_list(&mut self, ui: &mut egui::Ui, s: &Shot, acts: &mut Vec<Action>) {
        section(ui, "Takes");
        ui.add_space(6.0);
        let mut splice: Option<Vec<shotline_core::Piece>> = None;
        ui.horizontal(|ui| {
            let any = self.takes.iter().any(|t| t.output().is_some());
            if ui
                .add_enabled(any, egui::Button::new("Make a take from pieces…"))
                .on_hover_text(
                    "trim a take, or put parts of several together; made here for nothing",
                )
                .on_disabled_hover_text("needs a take with a video")
                .clicked()
            {
                splice = Some(vec![]);
            }
            if ui
                .button("Bring in a video…")
                .on_hover_text("a new take from a file, e.g. one cut in an editor")
                .clicked()
                && let Some(file) = rfd::FileDialog::new()
                    .add_filter("Video", &["mp4", "mov", "mkv", "webm", "m4v"])
                    .pick_file()
            {
                self.bring_in(&file);
            }
        });
        ui.add_space(8.0);
        if self.takes.is_empty() {
            hint(ui, "No takes yet.");
            return;
        }
        let mut watch = None;
        for t in &self.takes {
            let selected = s.selected_take.as_deref() == Some(t.id.to_string().as_str());
            let video = t.output().and_then(|j| {
                let f = j.outputs.first()?;
                Some((j.id, self.project.job_dir(j.id).join(f)))
            });
            let n = t.number;
            let row = egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(10, 8))
                .corner_radius(4.0)
                .fill(if selected {
                    theme::TUNGSTEN.gamma_multiply(0.12)
                } else {
                    egui::Color32::TRANSPARENT
                })
                .stroke(if selected {
                    egui::Stroke::new(1.0, theme::TUNGSTEN)
                } else {
                    egui::Stroke::NONE
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        match &video {
                            Some((id, file)) => {
                                let png = self
                                    .project
                                    .root
                                    .join(".shotline/cache/posters")
                                    .join(format!("{id}.png"));
                                let name = format!("{t} poster");
                                let make = (self.ff.as_ref(), &self.posters);
                                if poster(ui, &mut self.thumbs, make, file, &png, &name).clicked() {
                                    watch = Some(t.clone());
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
                                        .family(egui::FontFamily::Name(theme::SEMIBOLD.into()))
                                        .color(if selected {
                                            theme::TUNGSTEN
                                        } else {
                                            theme::PAPER
                                        }),
                                );
                                let status = t.status();
                                if selected {
                                    ui.label(RichText::new("in the cut").color(theme::TUNGSTEN));
                                } else if status != JobStatus::Done {
                                    let color = match status {
                                        JobStatus::Failed => theme::FLAG,
                                        _ => theme::DUST,
                                    };
                                    ui.label(RichText::new(status.to_string()).color(color));
                                }
                            });
                            let last = t.jobs.last();
                            let made = shotline_core::made_from(t, &self.takes);
                            let cost: Vec<String> = t
                                .costs()
                                .iter()
                                .map(|c| format!("{} {}", money(c.amount), c.unit))
                                .collect();
                            let about = made
                                .clone()
                                .or_else(|| last.map(|j| format!("{} {}", j.plugin, j.model)))
                                .into_iter()
                                .chain((!cost.is_empty()).then(|| cost.join(" + ")))
                                .collect::<Vec<_>>()
                                .join("   ");
                            ui.label(RichText::new(about).small().color(theme::DUST));
                            if video.is_none()
                                && let Some(why) = last.and_then(|j| j.error.clone())
                            {
                                ui.label(RichText::new(short(&why, 90)).small().color(theme::FLAG))
                                    .on_hover_text(why);
                            }
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if t.status().is_finished() {
                                let rm = remove_button(ui);
                                rm.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::Role::Button,
                                        true,
                                        format!("Remove #{n}"),
                                    )
                                });
                                if rm
                                    .on_hover_text("off the list; what it cost stays in the ledger")
                                    .clicked()
                                {
                                    acts.push(Action::RemoveTake(t.clone()));
                                }
                            }
                            if let Some(pieces) = shotline_core::pieces_of(t)
                                && act(ui, "Change pieces", n)
                                    .on_hover_text("makes a new take from these pieces, changed")
                                    .clicked()
                            {
                                splice = Some(pieces);
                            }
                            if video.is_some()
                                && !selected
                                && act(ui, "Use in the cut", n)
                                    .on_hover_text("the take this shot plays in the cut")
                                    .clicked()
                            {
                                acts.push(Action::Select(t.clone()));
                            }
                        });
                    });
                })
                .response;
            if !selected && row.hovered() {
                ui.painter()
                    .rect_filled(row.rect, 4.0, egui::Color32::from_white_alpha(6));
            }
            ui.add_space(4.0);
        }
        if let Some(t) = watch {
            self.watch(ui, t);
        }
        if let Some(pieces) = splice
            && let Some(r) = self.shot
        {
            self.open_splicer(r, pieces);
        }
    }

    fn open_splicer(&mut self, r: ShotRef, pieces: Vec<shotline_core::Piece>) {
        if self.ff.is_none() {
            self.error("can't cut without ffmpeg; install it or set SHOTLINE_FFMPEG".into());
            return;
        }
        // One sound device: the page's players give it up.
        if let Some(d) = &mut self.deck {
            d.pause(self.audio.as_ref());
        }
        if let Some((_, Some(d))) = &mut self.in_cut {
            d.pause(self.audio.as_ref());
        }
        self.audio();
        let sources = self
            .takes
            .iter()
            .filter_map(|t| {
                let j = t.output()?;
                Some((
                    t.clone(),
                    self.project.job_dir(j.id).join(j.outputs.first()?),
                ))
            })
            .collect();
        self.splicer = Some(pieces::Splicer::new(r, sources, pieces));
    }

    fn splicing(&mut self, ui: &mut egui::Ui) {
        let Some(s) = &mut self.splicer else {
            return;
        };
        let done = {
            let mut m = s.making.lock().unwrap();
            match std::mem::replace(&mut *m, pieces::Making::Idle) {
                pieces::Making::Done(out) => Some(out),
                other => {
                    *m = other;
                    None
                }
            }
        };
        match done {
            Some(Ok(t)) => {
                s.pause(self.audio.as_ref());
                self.splicer = None;
                if let Err(e) = self.load_takes() {
                    self.error(e.to_string());
                }
                self.reload();
                self.info(format!("{t} made; use it in the cut if it's the one"));
                return;
            }
            Some(Err(e)) => {
                if let Err(e) = self.load_takes() {
                    self.error(e.to_string());
                }
                self.error(format!("couldn't make it: {e}"));
            }
            None => {}
        }
        let Some(s) = &mut self.splicer else {
            return;
        };
        if !s.show(ui, self.ff.as_ref(), self.audio.as_ref(), &self.project) {
            self.splicer = None;
        }
    }

    fn bring_in(&mut self, file: &Path) {
        let Some(r) = self.shot else {
            return;
        };
        match self.project.import_take(r, file) {
            Ok(t) => {
                self.reload();
                self.info(format!(
                    "{t} brought in from {}",
                    file.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
            Err(e) => self.error(e.to_string()),
        }
    }

    pub fn tab(&self) -> Tab {
        self.tab
    }

    /// The selected take's video, stopped on its first frame until it's played. False when the
    /// shot has none.
    fn in_cut_player(&mut self, ui: &mut egui::Ui, s: &Shot) -> bool {
        let take = s
            .selected_take
            .as_ref()
            .and_then(|id| self.takes.iter().find(|t| t.id.to_string() == *id))
            .cloned();
        let Some(take) = take else {
            if let Some((_, Some(d))) = &mut self.in_cut {
                d.pause(self.audio.as_ref());
            }
            self.in_cut = None;
            return false;
        };
        if self.in_cut.as_ref().map(|(id, _)| id.as_str()) != Some(take.id.to_string().as_str()) {
            if let Some((_, Some(d))) = &mut self.in_cut {
                d.pause(self.audio.as_ref());
            }
            let file = take
                .output()
                .and_then(|j| Some(self.project.job_dir(j.id).join(j.outputs.first()?)));
            // Tried once, quietly: one that can't play isn't shown, and its popup says why.
            let label = take.to_string();
            let deck = match (file, &self.ff) {
                (Some(f), Some(ff)) => Player::new(ff, &label, &[f]).ok(),
                _ => None,
            }
            .map(|p| Deck::new(&label, p, true).in_popup(320.0));
            if deck.is_some() {
                self.audio();
            }
            self.in_cut = Some((take.id.to_string(), deck));
        }
        // The popup, or another player that's on, has the sound.
        let audio = if self.watching.is_some() || self.deck.as_ref().is_some_and(Deck::is_playing) {
            None
        } else {
            self.audio.as_ref()
        };
        let popup = self.watching.is_some() || self.deck.is_some();
        let Some((_, Some(d))) = &mut self.in_cut else {
            return false;
        };
        d.keys = !popup;
        // As wide as the picture, so the slider lines up under it.
        let wide = (d.players[0].aspect() * 320.0).max(320.0);
        let (size, length) = (d.players[0].size(), d.players[0].duration());
        ui.horizontal_top(|ui| {
            let left = ui
                .vertical(|ui| {
                    ui.set_width(ui.available_width().min(wide));
                    d.show(ui, audio);
                })
                .response
                .rect;
            ui.add_space(24.0);
            ui.vertical(|ui| in_cut_details(ui, &take, size, length, left.bottom()));
        });
        true
    }

    /// Opens a take in its popup, stopped on its first frame.
    fn watch(&mut self, ui: &egui::Ui, t: Take) {
        // One sound device: the page's players give it up.
        if let Some(d) = &mut self.deck {
            d.pause(self.audio.as_ref());
        }
        if let Some((_, Some(d))) = &mut self.in_cut {
            d.pause(self.audio.as_ref());
        }
        self.deck = None;
        let file = t
            .output()
            .and_then(|j| Some(self.project.job_dir(j.id).join(j.outputs.first()?)));
        let tall = ui.ctx().content_rect().height() * 0.55;
        let deck = file
            .and_then(|f| self.player(&t.to_string(), &[f]))
            .map(|p| Deck::new(&t.to_string(), p, true).in_popup(tall));
        self.watching = Some((t, deck));
    }

    fn watched(&mut self, ui: &mut egui::Ui) {
        let Some((take, deck)) = &mut self.watching else {
            return;
        };
        let wide = (ui.ctx().content_rect().width() * 0.6).clamp(360.0, 900.0);
        let mut close = false;
        let others: Vec<Take> = self
            .takes
            .iter()
            .filter(|o| o.shot == take.shot && o.id != take.id && o.output().is_some())
            .cloned()
            .collect();
        // Which take plays beside this one, if any.
        let beside = deck
            .as_ref()
            .and_then(|d| d.players.get(1))
            .map(|p| p.label.clone());
        let mut pick: Option<Option<Take>> = None;
        let modal = crate::modal(ui.ctx(), "take", |ui| {
            ui.set_width(wide);
            let bar = title_bar(ui, &take.to_string());
            match deck {
                Some(d) => close |= !d.show(ui, self.audio.as_ref()),
                None => hint(ui, "can't play it"),
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if deck.is_some() && !others.is_empty() {
                    let shown = beside
                        .as_deref()
                        .map_or("Compare with…".to_string(), |l| format!("beside {l}"));
                    egui::ComboBox::from_id_salt("compare")
                        .selected_text(shown)
                        .show_ui(ui, |ui| {
                            if beside.is_some() && ui.selectable_label(false, "Nothing").clicked() {
                                pick = Some(None);
                            }
                            for o in &others {
                                let l = o.to_string();
                                if ui
                                    .selectable_label(beside.as_deref() == Some(l.as_str()), &l)
                                    .clicked()
                                {
                                    pick = Some(Some(o.clone()));
                                }
                            }
                        })
                        .response
                        .on_hover_text("plays another take beside this one, in time with it")
                        .widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Compare with")
                        });
                }
            });
            ui.add_space(6.0);
            take_details(ui, take, &self.project);
            close |= close_corner(ui, bar);
        });
        match pick {
            Some(None) => {
                if let Some((_, Some(d))) = &mut self.watching {
                    d.alone();
                }
            }
            Some(Some(o)) => {
                let file = o
                    .output()
                    .and_then(|j| Some(self.project.job_dir(j.id).join(j.outputs.first()?)));
                if let Some(f) = file
                    && let Some(p) = self.player(&o.to_string(), &[f])
                    && let Some((_, Some(d))) = &mut self.watching
                {
                    d.compare(p, self.audio.as_ref());
                }
            }
            None => {}
        }
        if close || modal.should_close() {
            if let Some((_, Some(d))) = &mut self.watching {
                d.pause(self.audio.as_ref());
            }
            self.watching = None;
        }
    }
}

/// Which made images are a copy of an anchor, a frame or a shot's picture, matched by bytes.
fn in_use(film: &Film, root: &Path) -> HashMap<PathBuf, String> {
    let mut used: Vec<(PathBuf, String)> = vec![];
    for (el, anchors) in &film.elements {
        for a in anchors {
            used.push((a.clone(), format!("an anchor of @{}", el.handle)));
        }
    }
    for (_, shots) in &film.chapters {
        for (r, s) in shots {
            if let Some(f) = &s.frame {
                used.push((root.join(f), format!("the storyboard frame of {r}")));
            }
            for (i, img) in s.images.iter().enumerate() {
                used.push((root.join(&img.file), format!("picture {} of {r}", i + 1)));
            }
        }
    }
    let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).ok();
    let mut out = HashMap::new();
    let made = film.images.iter().map(|m| &m.path);
    for path in made.chain(film.last_frames.iter().map(|(_, f)| f)) {
        let Some(len) = size(path) else { continue };
        let mut bytes = None;
        for (f, what) in &used {
            if size(f) != Some(len) {
                continue;
            }
            let mine = bytes.get_or_insert_with(|| std::fs::read(path).unwrap_or_default());
            if std::fs::read(f).is_ok_and(|b| b == *mine) {
                out.insert(path.clone(), what.clone());
                break;
            }
        }
    }
    out
}

/// Takes `video`'s still into `png` in the background, unless it's being taken already.
pub(crate) fn make_poster(
    ctx: &egui::Context,
    (ff, making): (Option<&Ffmpeg>, &Arc<Mutex<HashSet<PathBuf>>>),
    video: &Path,
    png: &Path,
) {
    if let Some(ff) = ff
        && making.lock().unwrap().insert(png.to_path_buf())
    {
        let (ff, video, png) = (ff.clone(), video.to_path_buf(), png.to_path_buf());
        let (making, ctx) = (making.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = ff.poster(&video, &png);
            // Left in, a failed one isn't tried again every frame.
            if png.is_file() {
                making.lock().unwrap().remove(&png);
            }
            ctx.request_repaint();
        });
    }
}

/// A take's still, taken from its video the first time it's shown. Clicking it opens the take.
fn poster(
    ui: &mut egui::Ui,
    thumbs: &mut Thumbs,
    (ff, making): (Option<&Ffmpeg>, &Arc<Mutex<HashSet<PathBuf>>>),
    video: &Path,
    png: &Path,
    name: &str,
) -> egui::Response {
    let size = egui::vec2(96.0, 54.0);
    let texture = if png.is_file() {
        thumbs.texture(ui, png, THUMB, name)
    } else {
        make_poster(ui.ctx(), (ff, making), video, png);
        None
    };
    let r = match texture {
        Some(t) => ui.add(
            egui::Image::new(&t)
                .max_size(size)
                .corner_radius(2.0)
                .alt_text(name)
                .sense(egui::Sense::click()),
        ),
        None => {
            let (rect, r) = ui.allocate_exact_size(size, egui::Sense::click());
            ui.painter().rect_filled(rect, 2.0, theme::RACK);
            r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, name));
            r
        }
    };
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("click to watch it, with what went into it")
}

/// Beside the take in the cut: its size, length and model, and what it was asked for, as written
/// and in English when that's what went.
/// Ends level with `bottom`, the foot of the player; a long prompt scrolls.
fn in_cut_details(ui: &mut egui::Ui, t: &Take, (w, h): (u32, u32), length: f64, bottom: f32) {
    let video = t
        .jobs
        .iter()
        .rev()
        .find(|j| j.op == Op::VideoGenerate)
        .or(t.jobs.last());
    let row = |ui: &mut egui::Ui, k: &str, v: String| {
        let l = ui.label(RichText::new(k).color(theme::DUST));
        ui.label(RichText::new(v).color(theme::PAPER))
            .labelled_by(l.id);
        ui.end_row();
    };
    ui.label(
        RichText::new(format!("#{} in the cut", t.number))
            .family(egui::FontFamily::Name(theme::SEMIBOLD.into()))
            .color(theme::TUNGSTEN),
    );
    ui.add_space(6.0);
    egui::Grid::new("in cut")
        .num_columns(2)
        .spacing(egui::vec2(16.0, 6.0))
        .show(ui, |ui| {
            row(ui, "Size", format!("{w} × {h}"));
            row(ui, "Length", format!("{length:.1} s"));
            if let Some(j) = video {
                row(ui, "Model", format!("{} · {}", j.plugin, j.model));
            }
        });
    let Some(j) = video else {
        return;
    };
    let block = |ui: &mut egui::Ui, title: &str, text: &str| {
        ui.add_space(10.0);
        caption(ui, title);
        ui.add(egui::Label::new(text).selectable(true).wrap());
    };
    let room = (bottom - ui.cursor().top()).max(60.0);
    egui::ScrollArea::vertical()
        .id_salt("in cut prompt")
        .max_height(room)
        .auto_shrink([false, true])
        .show(ui, |ui| match (&j.brief, &j.english) {
            (Some(b), Some(e)) => {
                block(ui, "Prompt", b);
                block(ui, "In English", e);
            }
            (Some(b), None) => block(ui, "Prompt", b),
            // Takes from before the prompt was kept as written.
            _ => {
                if let Some(p) = &j.prompt {
                    block(ui, "Prompt the model got", p);
                }
            }
        });
}

/// Every step of a take: what ran, what it cost and what it was sent.
fn take_details(ui: &mut egui::Ui, t: &Take, p: &Project) {
    egui::ScrollArea::vertical()
        .max_height(240.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            let takes = p.takes(t.shot).unwrap_or_default();
            if let Some(m) = shotline_core::made_from(t, &takes) {
                ui.label(RichText::new(m).color(theme::GRAPHITE));
            }
            for (i, piece) in shotline_core::pieces_of(t)
                .into_iter()
                .flatten()
                .enumerate()
            {
                let name = takes
                    .iter()
                    .find(|o| o.id == piece.take)
                    .map_or("a removed take".into(), |o| format!("#{}", o.number));
                let join = if i == 0 { "" } else { piece.join.name() };
                hint(
                    ui,
                    &format!(
                        "{join} {}. {name} {:.2}–{:.2}s",
                        i + 1,
                        piece.from,
                        piece.to
                    ),
                );
            }
            for (i, j) in t.jobs.iter().enumerate() {
                if i > 0 {
                    ui.add_space(10.0);
                }
                let when = j
                    .created
                    .to_zoned(jiff::tz::TimeZone::system())
                    .strftime("%b %-d %H:%M");
                let cost = j
                    .cost
                    .as_ref()
                    .map(|c| format!(" · {} {}", money(c.amount), c.unit))
                    .unwrap_or_default();
                ui.label(
                    RichText::new(format!(
                        "{} · {} · {} · {when}{cost}",
                        j.op, j.plugin, j.model
                    ))
                    .color(theme::GRAPHITE),
                );
                if let Some(e) = &j.error {
                    ui.add(egui::Label::new(RichText::new(e).color(theme::FLAG)).wrap());
                }
                if j.op == Op::AudioTts {
                    if let Some(text) = &j.prompt {
                        caption(ui, "Line");
                        ui.add(egui::Label::new(text.as_str()).selectable(true).wrap());
                    }
                    continue;
                }
                if !j.inputs.is_empty() {
                    let with: Vec<String> =
                        j.inputs.iter().map(|i| p.rel(&p.root.join(i))).collect();
                    hint(ui, &format!("With {}", with.join(", ")));
                }
                if let Some(e) = &j.english {
                    caption(ui, "In English");
                    ui.add(egui::Label::new(e.as_str()).selectable(true).wrap());
                }
                if let Some(text) = &j.prompt {
                    caption(ui, "Prompt the model got");
                    ui.add(egui::Label::new(text.as_str()).selectable(true).wrap());
                }
            }
        });
}

pub(crate) fn short(s: &str, n: usize) -> String {
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

/// A chapter's heading in the shot list. Double-click or right-click to change its title; Enter
/// or leaving the box keeps it, Escape doesn't.
fn chapter_title(
    ui: &mut egui::Ui,
    c: &Chapter,
    renaming: &mut Option<(u32, String)>,
) -> Option<Action> {
    if let Some((n, text)) = renaming.as_mut().filter(|(n, _)| *n == c.number) {
        let n = *n;
        let field = ui.add(
            egui::TextEdit::singleline(text)
                .desired_width(f32::INFINITY)
                .id_salt(("chapter title", n)),
        );
        field.widget_info(|| {
            egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Chapter title")
        });
        if !field.has_focus() && !field.lost_focus() {
            field.request_focus();
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            *renaming = None;
            return None;
        }
        if field.lost_focus() {
            let title = text.trim().to_string();
            *renaming = None;
            if !title.is_empty() && title != c.title {
                return Some(Action::RenameChapter(n, title));
            }
        }
        return None;
    }
    let head = ui
        .add(
            egui::Label::new(
                RichText::new(format!("{} {}", c.id(), c.title))
                    .family(egui::FontFamily::Name(theme::SEMIBOLD.into()))
                    .color(theme::PAPER),
            )
            .sense(egui::Sense::click()),
        )
        .on_hover_text("double-click to rename");
    let mut start = head.double_clicked();
    head.context_menu(|ui| {
        if ui.button("Rename chapter").clicked() {
            start = true;
            ui.close();
        }
    });
    if start {
        *renaming = Some((c.number, c.title.clone()));
    }
    None
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
/// A small button drawn as an icon, lit on hover like any other button.
pub(crate) fn icon_button(
    ui: &mut egui::Ui,
    label: &str,
    draw: impl FnOnce(&egui::Painter, egui::Rect, egui::Stroke),
) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::click());
    let enabled = ui.is_enabled();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    if ui.is_rect_visible(rect) {
        let v = ui.style().interact(&r);
        if r.hovered() || r.is_pointer_button_down_on() {
            ui.painter()
                .rect_filled(rect, v.corner_radius, v.weak_bg_fill);
        }
        draw(
            ui.painter(),
            rect.shrink(7.0),
            egui::Stroke::new(1.5, v.fg_stroke.color),
        );
    }
    r
}

pub(crate) fn play_icon(p: &egui::Painter, s: egui::Rect, stroke: egui::Stroke) {
    let pts = vec![
        s.left_top(),
        egui::pos2(s.right(), s.center().y),
        s.left_bottom(),
    ];
    p.add(egui::Shape::convex_polygon(
        pts,
        stroke.color,
        egui::Stroke::NONE,
    ));
}

pub(crate) fn stop_icon(p: &egui::Painter, s: egui::Rect, stroke: egui::Stroke) {
    p.rect_filled(s.shrink(1.0), 1.0, stroke.color);
}

pub(crate) fn pause_icon(p: &egui::Painter, s: egui::Rect, stroke: egui::Stroke) {
    let w = s.width() / 3.0;
    p.rect_filled(
        egui::Rect::from_min_size(s.min, egui::vec2(w, s.height())),
        1.0,
        stroke.color,
    );
    p.rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(s.right() - w, s.top()),
            egui::vec2(w, s.height()),
        ),
        1.0,
        stroke.color,
    );
}

/// An `egui::Modal` whose dropdowns always draw above it. Both sit on the foreground layer, and
/// a click on the modal can put it back over a list that's open.
pub(crate) fn modal<R>(
    ctx: &egui::Context,
    name: &str,
    content: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::ModalResponse<R> {
    let modals = egui::Id::unique("modal layers");
    let r = egui::Modal::new(egui::Id::unique(name)).show(ctx, content);
    let me = r.response.layer_id;
    let others = ctx.data_mut(|d| {
        let all = d.get_temp_mut_or_default::<Vec<egui::LayerId>>(modals);
        if !all.contains(&me) {
            all.push(me);
        }
        all.clone()
    });
    if r.is_top_modal {
        let popups = ctx.memory(|m| m.areas().visible_layer_ids());
        for l in popups {
            if l.order == egui::Order::Foreground && !others.contains(&l) {
                ctx.set_sublayer(me, l);
            }
        }
    }
    r
}

/// A popup's top bar: its name, and once the popup's width is known, the ✕ at the right. Call
/// `title_bar` first and `close_corner` last, with what it returned.
fn title_bar(ui: &mut egui::Ui, name: &str) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().min(1.0), 24.0),
        egui::Sense::hover(),
    );
    ui.painter().text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        name,
        egui::TextStyle::Body.resolve(ui.style()),
        theme::GRAPHITE,
    );
    ui.add_space(4.0);
    rect
}

fn close_corner(ui: &mut egui::Ui, bar: egui::Rect) -> bool {
    let at = egui::Rect::from_min_size(
        egui::pos2(ui.min_rect().right() - 24.0, bar.top()),
        egui::vec2(24.0, 24.0),
    );
    let mut over = ui.new_child(egui::UiBuilder::new().max_rect(at));
    icon_button(&mut over, "Close", close_icon)
        .on_hover_text("Close")
        .clicked()
}

/// A red ✕ that takes something off a dropdown's list; grey while it can't.
pub(crate) fn delete_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let enabled = ui.is_enabled();
    icon_button(ui, label, |p, s, stroke| {
        let color = if enabled { theme::FLAG } else { stroke.color };
        close_icon(p, s, egui::Stroke::new(stroke.width, color));
    })
}

pub(crate) fn close_icon(p: &egui::Painter, s: egui::Rect, stroke: egui::Stroke) {
    p.line_segment([s.left_top(), s.right_bottom()], stroke);
    p.line_segment([s.right_top(), s.left_bottom()], stroke);
}

/// Two sheets, one over the other.
fn copy_icon(p: &egui::Painter, s: egui::Rect, stroke: egui::Stroke) {
    let s = s.expand(1.0);
    let back = egui::Rect::from_min_size(s.min, s.size() * 0.72);
    let front = egui::Rect::from_min_max(s.max - s.size() * 0.72, s.max);
    p.rect_stroke(back, 1.5, stroke, egui::StrokeKind::Middle);
    p.rect_filled(front, 1.5, theme::RACK);
    p.rect_stroke(front, 1.5, stroke, egui::StrokeKind::Middle);
}

/// The title of a part of a page, like Takes.
fn section(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .text_style(egui::TextStyle::Heading)
            .color(theme::PAPER),
    );
}

/// The title of a framed section.
pub(crate) fn heading(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.label(
        RichText::new(text)
            .family(egui::FontFamily::Name(theme::SEMIBOLD.into()))
            .color(theme::PAPER),
    )
}

fn caption(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.label(RichText::new(text).small().color(theme::DUST))
}

/// A labelled text box that grows with its text.
fn text_field(ui: &mut egui::Ui, label: &str, text: &mut String, hint: &str) -> egui::Response {
    explained_field(ui, label, None, text, hint)
}

/// A text field whose label says more on hover.
fn explained_field(
    ui: &mut egui::Ui,
    label: &str,
    about: Option<&str>,
    text: &mut String,
    hint: &str,
) -> egui::Response {
    let mut cap = caption(ui, label);
    if let Some(a) = about {
        cap = cap.on_hover_text(a);
    }
    let cap = cap.id;
    ui.add(
        egui::TextEdit::multiline(text)
            .hint_text(RichText::new(hint).color(theme::DUST))
            .desired_rows(1)
            .desired_width(f32::INFINITY)
            .margin(egui::vec2(8.0, 6.0)),
    )
    .labelled_by(cap)
}

/// What to do about something missing.
pub(crate) fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).color(theme::DUST));
}

fn history_button(ui: &mut egui::Ui, text: &str, what: Option<String>, keys: &str) -> bool {
    what.is_some_and(|w| {
        ui.button(text)
            .on_hover_text(format!("{text} {w} ({keys})"))
            .clicked()
    })
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
        top_label(ui, &key.replace('_', " "));
        ui.vertical(|ui| {
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
            if let Some(d) = p.get("description").and_then(|d| d.as_str()) {
                ui.set_max_width(360.0);
                ui.add(egui::Label::new(RichText::new(d).small().color(theme::DUST)).wrap());
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
/// "@paka 2", and its label if it has one.
pub(crate) fn anchor_name(el: &Element, i: usize, anchor: &Path) -> String {
    match el.label(anchor) {
        Some(l) => format!("@{} {}  {l}", el.handle, i + 1),
        None => format!("@{} {}", el.handle, i + 1),
    }
}

pub(crate) fn remove_button(ui: &mut egui::Ui) -> egui::Response {
    ui.button(RichText::new("Remove").small().color(theme::FLAG))
}

/// The one button that spends money: tungsten, and still lit on hover and press.
pub(crate) fn spend_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    spend(ui, egui::Button::new(text))
}

/// A button in tungsten: this one spends.
pub(crate) fn spend(ui: &mut egui::Ui, b: egui::Button) -> egui::Response {
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
        ui.add(b)
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

/// The Model and Resolution rows of a shot: the film's unless picked. Returns what's wrong
/// with them, for under the grid where it has room to wrap.
fn shot_model(
    ui: &mut egui::Ui,
    f: &mut Form,
    models: &[(String, ModelInfo)],
    film: &shotline_core::Format,
    s: &Shot,
) -> Vec<String> {
    // Ids are too long for the column: "minimax/h3-max/reference-to-video" shows as h3-max,
    // "fal-ai/flux" as flux, a Venice id as it is.
    let short = |m: &str| m.split('/').nth(1).unwrap_or(m).to_string();
    let l = ui.weak("Model");
    let film_says = format!("Film's ({})", short(&film.model));
    egui::ComboBox::from_id_salt("shot model")
        .selected_text(
            f.model
                .as_ref()
                .map_or(film_says.clone(), |(_, m)| short(m)),
        )
        .width(150.0)
        .truncate()
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut f.model, None, &film_says);
            for (p, m) in models {
                let pick = Some((p.clone(), m.id.clone()));
                ui.selectable_value(&mut f.model, pick, format!("{p} · {}", m.id));
            }
        })
        .response
        .on_hover_text(match &f.model {
            Some((p, m)) => format!("{p} · {m}"),
            None => format!("{} · {}", film.plugin, film.model),
        })
        .labelled_by(l.id);
    ui.end_row();

    let (plugin, model) = f
        .model
        .clone()
        .unwrap_or((film.plugin.clone(), film.model.clone()));
    let spec = models
        .iter()
        .find(|(p, m)| *p == plugin && m.id == model)
        .map(|(_, m)| m);
    let l = ui.weak("Resolution");
    let film_says = format!("Film's ({})", film.resolution);
    egui::ComboBox::from_id_salt("shot resolution")
        .selected_text(f.resolution.clone().unwrap_or(film_says.clone()))
        .width(150.0)
        .truncate()
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut f.resolution, None, &film_says);
            for r in spec.map_or(&[][..], |m| &m.resolutions) {
                ui.selectable_value(&mut f.resolution, Some(r.clone()), r);
            }
        })
        .response
        .labelled_by(l.id);
    ui.end_row();

    let resolution = f.resolution.as_ref().unwrap_or(&film.resolution);
    let aspect = s.model.as_ref().and_then(|m| m.aspect.as_ref());
    let mut flags = vec![];
    if let Some(m) = spec
        && !m.resolutions.is_empty()
        && !m.resolutions.contains(resolution)
    {
        flags.push(format!("{} doesn't do {resolution}", short(&m.id)));
    }
    if let Some(a) = aspect.filter(|a| **a != film.aspect) {
        flags.push(format!(
            "This shot is {a} but the film is {}; the render will stretch it.",
            film.aspect
        ));
    }
    flags
}

/// Under a zoomed picture: the job that made it and what it was asked for.
fn picture_details(ui: &mut egui::Ui, job: Option<&shotline_core::Job>, p: &Project) {
    let Some(j) = job else {
        hint(ui, "Added from a file, so there's no prompt for it.");
        return;
    };
    let when = j
        .created
        .to_zoned(jiff::tz::TimeZone::system())
        .strftime("%b %-d %H:%M");
    let cost = j
        .cost
        .as_ref()
        .map(|c| format!(" · {} {}", money(c.amount), c.unit))
        .unwrap_or_default();
    ui.label(
        RichText::new(format!("{} · {} · {when}{cost}", j.plugin, j.model)).color(theme::GRAPHITE),
    );
    if !j.inputs.is_empty() {
        let refs: Vec<String> = j.inputs.iter().map(|i| p.rel(&p.root.join(i))).collect();
        hint(ui, &format!("With {}", refs.join(", ")));
    }
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            if let Some(b) = &j.brief {
                ui.add_space(6.0);
                caption(ui, "You wrote");
                ui.add(egui::Label::new(b.as_str()).selectable(true).wrap());
            }
            if let Some(e) = &j.english {
                ui.add_space(6.0);
                caption(ui, "In English");
                ui.add(egui::Label::new(e.as_str()).selectable(true).wrap());
            }
            ui.add_space(6.0);
            match &j.prompt {
                Some(t) => {
                    caption(ui, "Prompt the model got");
                    ui.add(egui::Label::new(t.as_str()).selectable(true).wrap());
                }
                None => hint(ui, "Made before shotline kept prompts."),
            }
        });
}
