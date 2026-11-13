//! The desktop app: browse a film's story, elements, shots, takes and costs, and play takes and
//! the cut. It reads and writes the project folder through shotline-core like the CLI does, so
//! both always agree.

mod anchor;
mod bridge;
mod browse;
mod cost;
mod cut;
mod cut_tools;
mod edit;
mod git;
mod grouped;
mod icon;
mod kit;
mod lanes;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod menu;
mod pieces;
mod player;
mod prefs;
mod price;
pub mod running;
mod shoot;
mod sounds;
mod start;
mod theme;
mod voice;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use shotline_core::ledger::money;
use shotline_core::protocol::{Description, EditInfo, ModelInfo, Op, OpenParams, VoiceInfo};
use shotline_core::{
    Chapter, CutShot, Element, Git, JobStatus, Kind, Project, SPOKEN, Shot, ShotRef, Take,
    Timeline, Transition,
};
use shotline_media::Ffmpeg;

use edit::{Action, ElementForm, Field, Form, History, NewElement, Texts};
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

    fn icon(self) -> &'static str {
        match self {
            Tab::Story => icon::STORY,
            Tab::Elements => icon::ELEMENTS,
            Tab::Shots => icon::SHOTS,
            Tab::Cut => icon::CUT,
            Tab::Cost => icon::COST,
        }
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
type Edits = (Vec<EditInfo>, Vec<EditWarning>);

/// A problem on an editor's side, shown on the Cut page, and a fix its plugin offers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditWarning {
    pub text: String,
    pub fix: Option<EditFix>,
}

/// A fix an editor plugin can make itself: by `fix` with its id, on a button saying `label`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditFix {
    pub plugin: String,
    pub id: String,
    pub label: String,
}
/// Filled with what a plugin said once it's done.
type Said = Arc<Mutex<Option<Result<String, String>>>>;
/// Filled once an editor was asked to open a project.
type Opened = Arc<Mutex<Option<Result<(), String>>>>;

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
    fn sequences(&self, _film: &Path) -> Edits {
        (vec![], vec![])
    }
    /// Has an editor plugin make a fix it offered with a warning. Returns what it said.
    fn fix_edit(&self, plugin: &str, _film: &Path, _id: &str) -> anyhow::Result<String> {
        anyhow::bail!("{plugin} can't fix that")
    }
    /// What one operation outside any shot would cost, e.g. "0.00 USD". Free.
    fn op_quote(
        &self,
        _film: &Path,
        plugin: &str,
        _req: &shotline_core::protocol::Request,
    ) -> anyhow::Result<String> {
        anyhow::bail!("{plugin} can't be run from here")
    }
    /// Runs it, if the price is still `at`. Returns what it made.
    fn run_op(
        &self,
        _film: &Path,
        plugin: &str,
        _req: &shotline_core::protocol::Request,
        _at: &str,
    ) -> anyhow::Result<PathBuf> {
        anyhow::bail!("{plugin} can't be run from here")
    }
    /// What another take of a bridge would cost, with putting its prompt into English first
    /// when `translate` and that's needed. Free.
    fn bridge_quote(
        &self,
        _film: &Path,
        _bridge: shotline_core::Ulid,
        _translate: bool,
    ) -> anyhow::Result<String> {
        anyhow::bail!("bridges can't be made from here")
    }
    /// Makes a take of the bridge, if the price is still `at`. Returns its video.
    fn bridge_take(
        &self,
        _film: &Path,
        _bridge: shotline_core::Ulid,
        _at: &str,
        _translate: bool,
    ) -> anyhow::Result<PathBuf> {
        anyhow::bail!("bridges can't be made from here")
    }
    /// Renders a sequence of an editor's project through its plugin into `out`.
    fn export_edit(
        &self,
        plugin: &str,
        _ask: &shotline_core::protocol::ExportParams,
    ) -> anyhow::Result<PathBuf> {
        anyhow::bail!("{plugin} can't render")
    }
    /// Changes a clip of an editor's sequence through its plugin, and says how to undo it.
    fn edit_clip(
        &self,
        plugin: &str,
        _ask: &shotline_core::protocol::EditParams,
    ) -> anyhow::Result<shotline_core::protocol::Edited> {
        anyhow::bail!("{plugin} can't change clips")
    }
    /// Opens the film's project in an editor plugin's app, made from the cut if there's none.
    fn open_edit(&self, plugin: &str, _ask: &OpenParams) -> anyhow::Result<()> {
        anyhow::bail!("{plugin} can't open its projects")
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
    /// Starts making run `id` of `running` in a process that outlives the app; false to have
    /// it made on a thread of the app instead.
    fn start_run(&self, _film: &Path, _id: &str) -> anyhow::Result<bool> {
        Ok(false)
    }
    /// What recording line `line` (from 0) on its own would cost. Free.
    fn say_quote(&self, film: &Path, shot: ShotRef, line: usize) -> anyhow::Result<String>;
    /// Records the line, if the price is still `at`, and picks it for the shot's takes.
    fn say(&self, film: &Path, shot: ShotRef, line: usize, at: &str) -> anyhow::Result<PathBuf>;
    /// What `who` saying `text` would cost, with the voice a line of `shot` gets. Free.
    fn speech_quote(
        &self,
        _film: &Path,
        _shot: Option<ShotRef>,
        _who: &str,
        _text: &str,
    ) -> anyhow::Result<String> {
        anyhow::bail!("can't make speech here")
    }
    /// Records `who` saying `text`, if the price is still `at`. Returns the job's id.
    fn speech(
        &self,
        _film: &Path,
        _shot: Option<ShotRef>,
        _who: &str,
        _text: &str,
        _at: &str,
    ) -> anyhow::Result<String> {
        anyhow::bail!("can't make speech here")
    }
    /// What a sound effect made from `text` would cost; without `length` the model picks it.
    /// Free.
    fn effect_quote(
        &self,
        _film: &Path,
        plugin: &str,
        _model: &str,
        _text: &str,
        _length: Option<f64>,
    ) -> anyhow::Result<String> {
        anyhow::bail!("{plugin} can't make sound effects here")
    }
    /// Makes the sound effect for `shot`, if the price is still `at`. Returns the job's id.
    #[allow(clippy::too_many_arguments)]
    fn effect(
        &self,
        _film: &Path,
        _shot: Option<ShotRef>,
        plugin: &str,
        _model: &str,
        _text: &str,
        _length: Option<f64>,
        _at: &str,
    ) -> anyhow::Result<String> {
        anyhow::bail!("{plugin} can't make sound effects here")
    }
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
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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

/// The thread that asks which keys plugins are missing; tests tell it apart by name.
#[doc(hidden)]
pub const KEYS_THREAD: &str = "plugin keys";

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
    edit_bridges: bridge::BridgeTakes,
    bridge_win: Option<bridge::BridgeWin>,
    /// Joins of the sequence shown that have a bridge, by the clip after them.
    bridged: Vec<String>,
    /// The film's bridges, read again after the sequences are.
    bridges_seen: Option<Vec<(shotline_core::Ulid, shotline_core::Bridge)>>,
    /// Each shot of the cut with the take it would go in the editor's sequence with.
    cut_takes: Option<Vec<(Take, PathBuf)>>,
    /// Being asked again in the background; the answer lands here.
    edits_asked: Option<Arc<Mutex<Option<Edits>>>>,
    /// The film was read again, so the editors are asked again.
    edits_stale: bool,
    /// What shared/ and the editors' files looked like when last checked, and when that was.
    edits_seen: Option<(u64, std::time::Instant)>,
    /// An editor plugin making a fix, by its button's words, and then what came of it.
    edit_fixing: Option<(String, Said)>,
    edit_fixed: Option<Result<String, String>>,
    /// How often shared/ and the editors' files are looked at for a save.
    edits_every: std::time::Duration,
    /// A sequence's preview being made, and where to start playing it once it's there.
    seq_making: Option<crate::cut::Making>,
    /// The preview the deck plays, and the clips left out of it.
    seq_playing: Option<(PathBuf, Vec<String>)>,
    /// An editor being asked to open a project: what to call it, and how that went.
    edit_opening: Option<(String, Opened)>,
    /// The Cut page's player, by what it shows, or why it can't.
    cut_deck: Option<(String, Result<Deck, String>)>,
    /// A preview that couldn't be made, and why.
    seq_failed: Option<(PathBuf, String)>,
    /// The editor rendering a preview, into this file.
    full_making: Option<(PathBuf, cut::FullSlot)>,
    full_failed: Option<(PathBuf, String)>,
    /// The editor clip picked on the Cut page, by the editor's id.
    clip_pick: Option<String>,
    /// The sound clip picked on the Cut page, by the editor's id and file, and what it is.
    sound_pick: Option<(String, Option<PathBuf>, cut::SoundPick)>,
    /// The join picked instead, by the editor's id for the clip after it.
    join_pick: Option<String>,
    /// How long a dissolve put on a join is, in frames.
    fade_frames: u32,
    changing: Option<cut_tools::Changing>,
    /// Undo for clip changes made from here, read from the film's cache when first needed.
    undo: Option<Vec<cut_tools::Undo>>,
    /// A video tool on a clip, waiting for its price to be agreed.
    process: Option<cut_tools::Process>,
    edit_rendering: Option<cut_tools::Rendering>,
    shot: Option<ShotRef>,
    takes: Vec<Take>,
    /// The shot before's takes, for one continuing from it.
    prev_takes: Vec<Take>,
    /// The shot after, when it continues from one of this shot's takes, and that take's id.
    continued: Option<(ShotRef, String)>,
    /// A take to use in the cut once it's agreed the shot after keeps its start.
    confirm_select: Option<Take>,
    /// Picked on the shot page, to go in the editor's sequence too once it's been read again.
    cut_follow: Option<Take>,
    sounds_win: Option<sounds::SoundsWin>,
    picks: std::collections::VecDeque<PathBuf>,
    status: Option<(String, bool)>,
    /// The job that made the zoomed picture, once Details is asked for.
    zoom_details: Option<(PathBuf, Option<shotline_core::Job>)>,
    /// Where pictures came from, worked out once per load: it reads every image job.
    origins: HashMap<PathBuf, Origin>,
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
    /// Settings pressed on the rail; the shell opens Preferences.
    pub(crate) wants_prefs: bool,
    /// Keys each plugin needs that aren't set, by plugin, worked out off the UI thread.
    missing_keys: Arc<Mutex<HashMap<String, Vec<String>>>>,
    maker: Option<anchor::Maker>,
    /// Images being made, or made and waiting to be kept, as the film's cache has them.
    runs: Vec<(running::Run, running::State)>,
    runs_read: Option<std::time::Instant>,
    /// The open shot as it would be sent; None once anything is read again.
    built: Option<(ShotRef, Result<shotline_core::Built, String>)>,
    /// What the shot was when its take was priced.
    shoot_for: Option<Result<shotline_core::Built, String>>,
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
    /// Recordings' lengths, as ffprobe gave them; None when it couldn't, so it isn't asked
    /// again every frame.
    lengths: HashMap<PathBuf, Option<f64>>,
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
pub(crate) const FIT: (f32, egui::Pos2) = (1.0, egui::pos2(0.5, 0.5));

/// Pinch and scroll over `rect` to zoom and look around a picture, drag to move it, double-click
/// to see it whole. `view` is how far in and the middle of what shows, 0-1 across. Gives the part
/// of the picture to draw.
pub(crate) fn pan_zoom(
    ui: &egui::Ui,
    rect: egui::Rect,
    r: &egui::Response,
    view: &mut (f32, egui::Pos2),
) -> egui::Rect {
    let (scale, mid) = view;
    if r.hovered() {
        let (pinch, scroll, at) =
            ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.pointer.hover_pos()));
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
    *mid -= r.drag_delta() / rect.size() / *scale;
    if r.double_clicked() {
        *view = FIT;
    }
    let (scale, mid) = view;
    let half = 0.5 / *scale;
    *mid = mid.clamp(egui::pos2(half, half), egui::pos2(1.0 - half, 1.0 - half));
    egui::Rect::from_center_size(*mid, egui::Vec2::splat(2.0 * half))
}
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
    /// The picture whole and centred on a dark stage as wide as there's room for.
    pub(crate) fn stage(
        &mut self,
        ui: &mut egui::Ui,
        path: &Path,
        height: f32,
        name: &str,
    ) -> bool {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::hover(),
        );
        ui.painter().rect_filled(rect, 6.0, theme::STAGE);
        let Some(t) = self.texture(ui, path, BIG, name) else {
            ui.put(
                rect,
                egui::Label::new(RichText::new(format!("can't show {name}")).weak()),
            );
            return false;
        };
        let size = t.size_vec2();
        let fit = (rect.width() / size.x).min(rect.height() / size.y);
        let shown = egui::Rect::from_center_size(rect.center(), size * fit);
        ui.put(
            shown,
            egui::Image::new(&t)
                .fit_to_exact_size(shown.size())
                .alt_text(name)
                .sense(egui::Sense::click()),
        )
        .on_hover_cursor(egui::CursorIcon::ZoomIn)
        .on_hover_text("click to see it bigger")
        .clicked()
    }

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
            edit_bridges: HashMap::new(),
            bridge_win: None,
            bridged: vec![],
            bridges_seen: None,
            cut_takes: None,
            edits_asked: None,
            edits_stale: true,
            edits_seen: None,
            edit_fixing: None,
            edit_fixed: None,
            edits_every: std::time::Duration::from_secs(1),
            seq_making: None,
            seq_playing: None,
            edit_opening: None,
            cut_deck: None,
            seq_failed: None,
            full_making: None,
            full_failed: None,
            clip_pick: None,
            sound_pick: None,
            join_pick: None,
            fade_frames: 12,
            changing: None,
            undo: None,
            process: None,
            edit_rendering: None,
            shot: None,
            takes: vec![],
            prev_takes: vec![],
            continued: None,
            confirm_select: None,
            cut_follow: None,
            sounds_win: None,
            picks: Default::default(),
            status: None,
            zoom_details: None,
            origins: HashMap::new(),
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
            wants_prefs: false,
            missing_keys: Arc::default(),
            maker: None,
            runs: vec![],
            runs_read: None,
            built: None,
            shoot_for: None,
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
        self.check_keys();
    }

    /// Works out again which plugins are missing a key, after keys were changed.
    pub fn check_keys(&self) {
        let Some(host) = self.host.clone() else {
            return;
        };
        let into = self.missing_keys.clone();
        let spawned = std::thread::Builder::new()
            .name(KEYS_THREAD.into())
            .spawn(move || {
                let got = host
                    .plugins()
                    .0
                    .into_iter()
                    .filter(|p| !p.missing.is_empty())
                    .map(|p| (p.name, p.missing))
                    .collect();
                *into.lock().unwrap() = got;
            });
        if let Err(e) = spawned {
            eprintln!("can't check plugin keys: {e}");
        }
    }

    /// The plugin `who` speaks through, or why they can't speak.
    pub(crate) fn voice_plugin(&self, who: &str) -> Result<String, String> {
        let who = who.trim_start_matches('@');
        if who.is_empty() {
            return Err("pick who says it first".into());
        }
        let el = self
            .film
            .elements
            .iter()
            .find(|(e, _)| e.handle == who)
            .map(|(e, _)| e)
            .ok_or_else(|| format!("there's no @{who} in the cast"))?;
        el.voice
            .as_ref()
            .map(|v| v.plugin.clone())
            .ok_or_else(|| format!("@{who} has no voice yet; give them one on the Elements page"))
    }

    /// Why `plugin` can't be asked yet: a key it needs isn't set.
    pub(crate) fn needs_keys(&self, plugin: &str) -> Option<String> {
        let missing = self.missing_keys.lock().unwrap();
        let keys = missing.get(plugin)?;
        Some(format!("{plugin} needs {}", keys.join(", ")))
    }

    /// Reads the jobs list again now and then, and brings the popup up to date with its run.
    fn follow_runs(&mut self, ctx: &egui::Context) {
        let bound = self.maker.as_ref().is_some_and(|m| m.run.is_some());
        let every = std::time::Duration::from_secs(1);
        if bound || self.runs_read.is_none_or(|t| t.elapsed() >= every) {
            self.runs = running::list(&self.project.root);
            self.runs_read = Some(std::time::Instant::now());
        }
        if self.runs.iter().any(|(_, s)| *s == running::State::Running) {
            ctx.request_repaint_after(every);
        }
        // Made out of sight: used as soon as it's there. The popup watching one asks first.
        let watched = self.maker.as_ref().and_then(|m| m.run.clone());
        let made: Vec<(running::Run, PathBuf)> = self
            .runs
            .iter()
            .filter(|(r, _)| Some(&r.id) != watched.as_ref())
            .filter_map(|(r, s)| match s {
                running::State::Made(out) => Some((r.clone(), out.clone())),
                _ => None,
            })
            .collect();
        for (run, out) in made {
            self.flush();
            self.status = Some((format!("{}: made and put in place", run.title), false));
            self.keep(&run.target, out, &run.ask.prompt, true);
            self.drop_run(&run.id);
        }
        let Some(m) = &mut self.maker else { return };
        let Some(id) = m.run.clone() else { return };
        let s = self
            .runs
            .iter()
            .find(|(r, _)| r.id == id)
            .map(|(_, s)| s.clone());
        // Said in the popup now, so there's nothing to come back to it for.
        if matches!(s, Some(running::State::Failed(_) | running::State::Stopped)) {
            running::remove(&self.project.root, &id);
            self.runs.retain(|(r, _)| r.id != id);
        }
        m.follow(s.as_ref());
    }

    /// Takes a run off the jobs list.
    fn drop_run(&mut self, id: &str) {
        running::remove(&self.project.root, id);
        self.runs.retain(|(r, _)| r.id != id);
    }

    /// The popup a run was made from, as it was, following the run.
    pub(crate) fn open_run(&mut self, id: &str) {
        let Some((run, _)) = self.runs.iter().find(|(r, _)| r.id == id).cloned() else {
            return;
        };
        let from = (run.ask.plugin.as_str(), run.ask.model.as_str());
        let defaults = (run.aspect.clone(), run.resolution.clone());
        let refs = run.ask.refs.clone();
        let m = self.open_maker(
            run.target.clone(),
            run.title.clone(),
            refs,
            Some(from),
            defaults,
        );
        m.restore(&run.ask);
        m.run = Some(run.id.clone());
    }

    /// Makes the image in the background, from the popup's settings.
    fn start_run(&mut self, host: Arc<dyn Host>, ask: AnchorAsk, cost: String) {
        let root = self.project.root.clone();
        let Some(m) = self.maker.as_mut() else { return };
        if let Some(old) = m.run.take() {
            running::remove(&root, &old);
        }
        let mut run = running::Run {
            id: String::new(),
            title: m.title.clone(),
            started: jiff::Timestamp::now(),
            target: m.target.clone(),
            ask,
            cost,
            aspect: m.defaults.0.clone(),
            resolution: m.defaults.1.clone(),
            out: None,
            error: None,
        };
        match running::create(&root, &mut run) {
            Ok(()) => {
                m.making(run.id.clone());
                running::start(host, &root, &run.id);
                self.runs = running::list(&root);
                self.runs_read = Some(std::time::Instant::now());
            }
            Err(e) => self.error(format!("couldn't start making it: {e:#}")),
        }
    }

    /// Puts the image the popup is showing to use.
    fn keep_made(&mut self) {
        let Some(m) = &mut self.maker else { return };
        let Some((out, prompt)) = m.take_made() else {
            return;
        };
        let (target, run) = (m.target.clone(), m.run.clone());
        self.maker = None;
        self.flush();
        self.keep(&target, out, &prompt, false);
        if let Some(id) = run {
            self.drop_run(&id);
        }
    }

    /// Puts an image made to use, in place of what it was made again for, as one undo step.
    /// `prompt` is what was typed for it; `unseen` when nobody looked at it before it went in.
    fn keep(&mut self, target: &anchor::For, out: PathBuf, prompt: &str, unseen: bool) {
        let p = &self.project;
        let root = p.root.clone();
        let mut note = None;
        let acts = match target {
            anchor::For::Element(h) => p
                .element(h)
                .map(|el| vec![Action::AddAnchor(el, out.clone())]),
            anchor::For::Shot(r) => p
                .shot(*r)
                .map(|_| vec![Action::AddShotImage(*r, out.clone())]),
            anchor::For::Picture(r, f) => p.shot(*r).map(|s| {
                match s.images.iter().position(|i| root.join(&i.file) == *f) {
                    Some(i) => vec![Action::ReplaceShotImage(*r, i, out.clone())],
                    None => {
                        note = Some(format!(
                            "the picture of {r} it was made again for is gone, so it's added as \
                             a new one"
                        ));
                        vec![Action::AddShotImage(*r, out.clone())]
                    }
                }
            }),
            anchor::For::Frame(r, was) => p.shot(*r).map(|s| {
                let now = s.frame.as_ref().map(|f| root.join(f));
                if unseen && now != *was {
                    note = Some(format!(
                        "{r}'s frame was changed while the new one was made, so it was left \
                         as it is; the new one is under Browse existing frames"
                    ));
                    return vec![];
                }
                let mut acts = vec![Action::SetFrame(*r, Some(out.clone()))];
                if s.frame_prompt.as_deref().unwrap_or_default() != prompt {
                    acts.push(Action::Set(*r, Field::FramePrompt(prompt.to_string())));
                }
                acts
            }),
        };
        match acts {
            Ok(acts) if acts.is_empty() => {}
            Ok(acts) => self.apply(acts),
            Err(e) => self.error(format!("made {}, but {e}", self.project.rel(&out))),
        }
        if let Some(n) = note {
            self.error(n);
        }
    }

    /// Opens the Make image popup. `from` is the model to start on, else the film's.
    pub(crate) fn open_maker(
        &mut self,
        target: anchor::For,
        title: String,
        refs: Vec<PathBuf>,
        from: Option<(&str, &str)>,
        defaults: (String, Option<String>),
    ) -> &mut anchor::Maker {
        let models = self.image_models();
        let cfg = &self.project.config.image;
        let from = from.unwrap_or((&cfg.plugin, &cfg.model));
        let mut m = anchor::Maker::new(target, title, refs, &models, from, defaults);
        m.cast = self
            .film
            .elements
            .iter()
            .filter(|(e, _)| e.kind == Kind::Cast)
            .flat_map(|(_, a)| a.iter().cloned())
            .collect();
        self.maker.insert(m)
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

    /// The Make image popup, while it's open.
    fn maker_popup(&mut self, ctx: &egui::Context) {
        if self.maker.is_none() {
            return;
        }
        let models = self.image_models();
        let pool = self.anchor_pool();
        let translator = self.translator();
        let missing = self.missing_keys.lock().unwrap().clone();
        self.maker.as_mut().unwrap().missing = missing;
        let wide = if self.maker.as_ref().unwrap().reviewing() {
            kit::modal_width(ctx, 0.75, 640.0, 900.0)
        } else {
            kit::modal_width(ctx, 0.6, 480.0, 640.0)
        };
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        let modal = modal(ctx, "make image", |ui| {
            ui.set_width(wide);
            self.maker.as_mut().unwrap().show(
                ui,
                &models,
                &pool,
                &mut self.thumbs,
                self.host.clone(),
                &self.project.root,
                translator,
                esc,
            )
        });
        let host = self.host.clone();
        match (modal.inner, host) {
            (anchor::Wants::Price(ask), Some(host)) => {
                self.flush();
                let m = self.maker.as_ref().unwrap();
                m.price(ctx, host, &self.project.root, ask);
            }
            (anchor::Wants::Make(ask, cost), Some(host)) => {
                self.flush();
                self.start_run(host, ask, cost);
            }
            (anchor::Wants::Zoom(p), _) => self.zoom = Some(p),
            (anchor::Wants::Close | anchor::Wants::Hide, _) => self.maker = None,
            (anchor::Wants::Keep, _) => self.keep_made(),
            (anchor::Wants::Again, _) => {
                if let Some(id) = self.maker.as_mut().unwrap().run.take() {
                    self.drop_run(&id);
                }
            }
            (anchor::Wants::Discard, _) => {
                if let Some(id) = self.maker.take().and_then(|m| m.run) {
                    self.drop_run(&id);
                }
            }
            (anchor::Wants::Keys(plugin), _) => self.wants_keys = Some(plugin),
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
        let refs = job
            .inputs
            .iter()
            .map(|i| self.project.root.join(i))
            .filter(|p| p.is_file())
            .collect();
        let title = match &target {
            anchor::For::Element(h) => format!("Remake an image of @{h}"),
            anchor::For::Shot(r) => format!("New picture for {r}"),
            anchor::For::Picture(r, f) => {
                let n = self.project.shot(*r).ok().and_then(|s| {
                    s.images
                        .iter()
                        .position(|i| self.project.root.join(&i.file) == *f)
                });
                match n {
                    Some(i) => format!("Remake picture {} of {r}", i + 1),
                    None => format!("Remake a picture of {r}"),
                }
            }
            anchor::For::Frame(r, _) => format!("Remake the frame of {r}"),
        };
        let from = (job.plugin.as_str(), job.model.as_str());
        let m = self.open_maker(target, title, refs, Some(from), defaults);
        m.set_prompt(job.brief.as_deref().unwrap_or_default());
        m.set_translate(job.english.is_some());
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

    /// The top bar's left: the film, and the chapter and shot open on the Shots page.
    fn where_we_are(&self, ui: &mut egui::Ui) {
        let mut parts = vec![self.project.config.name.clone()];
        // Elsewhere the rail says which page it is.
        if let (Tab::Shots, Some(r)) = (self.tab, self.shot)
            && let Some((c, shots)) = self
                .film
                .chapters
                .iter()
                .find(|(c, _)| c.number == r.chapter)
        {
            parts.push(c.title.clone());
            let title = shots
                .iter()
                .find(|(s, _)| *s == r)
                .map(|(_, s)| s.title.clone())
                .unwrap_or_default();
            parts.push(format!("{r} {title}").trim().to_string());
        }
        ui.spacing_mut().item_spacing.x = 8.0;
        let last = parts.len() - 1;
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                ui.label(RichText::new("/").color(theme::FAINT));
            }
            let text = RichText::new(part);
            ui.label(if i == last {
                text.color(theme::PAPER)
                    .family(egui::FontFamily::Name(theme::SEMIBOLD.into()))
            } else {
                text.color(theme::DUST)
            });
        }
    }

    /// Beside Reload while anything is on the jobs list: a pill with a ring that turns while
    /// something is being made and a count, opening the list.
    fn jobs_button(&mut self, ui: &mut egui::Ui) {
        if self.runs.is_empty() {
            return;
        }
        let count = |f: fn(&running::State) -> bool| self.runs.iter().filter(|(_, s)| f(s)).count();
        let going = count(|s| *s == running::State::Running);
        let failed = count(|s| matches!(s, running::State::Failed(_) | running::State::Stopped));
        let made = self.runs.len() - going - failed;
        let said: Vec<String> = [(going, "running"), (made, "made"), (failed, "failed")]
            .iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, what)| format!("{n} {what}"))
            .collect();
        let label = format!("Jobs: {}", said.join(", "));
        let accent = if failed > 0 {
            theme::FLAG
        } else {
            theme::STEEL
        };
        let font = egui::TextStyle::Button.resolve(ui.style());
        let text = ui
            .painter()
            .layout_no_wrap("Jobs".into(), font, theme::PAPER);
        let size = egui::vec2(10.0 + 16.0 + 8.0 + text.size().x + 8.0 + 18.0 + 8.0, 32.0);
        let (rect, r) = ui.allocate_exact_size(size, egui::Sense::click());
        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
        let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&r));
        if ui.is_rect_visible(rect) {
            let p = ui.painter();
            let fill = if r.hovered() || open {
                theme::RAISED
            } else {
                theme::CARD
            };
            p.rect(
                rect,
                16.0,
                fill,
                egui::Stroke::new(1.0, theme::EDGE),
                egui::StrokeKind::Inside,
            );
            let c = egui::pos2(rect.left() + 18.0, rect.center().y);
            if going > 0 {
                kit::ring(p, c, 6.5, 2.0, theme::STEEL, ui.input(|i| i.time));
                ui.ctx().request_repaint();
            } else if failed > 0 {
                kit::glyph(p, c, icon::WARNING, 16.0, theme::FLAG);
            } else {
                kit::glyph(p, c, icon::CHECK, 16.0, theme::GO);
            }
            let x = c.x + 8.0 + 8.0;
            p.galley(
                egui::pos2(x, rect.center().y - text.size().y / 2.0),
                text.clone(),
                theme::PAPER,
            );
            kit::badge(
                p,
                egui::pos2(rect.right() - 17.0, rect.center().y),
                self.runs.len(),
                accent,
            );
        }
        let r = r.on_hover_text("images being made; they carry on with the app closed");
        let now = jiff::Timestamp::now();
        let mut pick = None;
        egui::Popup::menu(&r).show(|ui| {
            ui.set_width(340.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Jobs")
                        .font(theme::section())
                        .color(theme::PAPER),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("they carry on with the app closed")
                            .small()
                            .color(theme::DUST),
                    );
                });
            });
            for (run, s) in &self.runs {
                let how = match s {
                    running::State::Running => {
                        let secs = now.duration_since(run.started).as_secs().max(0);
                        format!("making · {}:{:02}", secs / 60, secs % 60)
                    }
                    running::State::Made(_) => "made".into(),
                    running::State::Failed(e) => format!("failed · {}", short(e, 60)),
                    running::State::Stopped => "stopped before it was done".into(),
                };
                if job_row(ui, &run.title, &how, s).clicked() {
                    pick = Some(run.id.clone());
                    ui.close();
                }
            }
        });
        if let Some(id) = pick {
            self.open_run(&id);
        }
    }

    /// A Make… button for images: off while another is being made, or with no image model.
    /// True when clicked.
    pub(crate) fn make_button(&self, ui: &mut egui::Ui, text: &str) -> bool {
        let none = self.image_models().is_empty();
        ui.add_enabled(!none, egui::Button::new(text))
            .on_disabled_hover_text(
                "needs an image model; install an image plugin, e.g. fal or venice, in \
                 Preferences",
            )
            .clicked()
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
        let here = self.maker.as_ref().and_then(|m| m.target.shot());
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

    /// Answers file dialogs with these files, one each, for tests.
    pub fn picking(mut self, files: Vec<PathBuf>) -> App {
        self.picks = files.into();
        self
    }

    /// A file from the dialog, or the next one `picking` was given.
    pub(crate) fn pick_file(&mut self, dialog: rfd::FileDialog) -> Option<PathBuf> {
        if !self.picks.is_empty() {
            return self.picks.pop_front();
        }
        dialog.pick_file()
    }

    /// Plays without sound and never touches the sound device, for tests.
    /// Looks for the editors' saves this often, instead of once a second.
    pub fn checking_edits_every(mut self, every: std::time::Duration) -> App {
        self.edits_every = every;
        self
    }

    pub fn silent(mut self) -> App {
        self.audio_tried = true;
        self
    }

    pub fn is_playing(&self) -> bool {
        self.deck.as_ref().is_some_and(Deck::is_playing)
            || self
                .cut_deck
                .as_ref()
                .and_then(|(_, d)| d.as_ref().ok())
                .is_some_and(Deck::is_playing)
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
        self.origins.clear();
        self.runs_read = None;
        self.refresh_repo();
        if let Err(e) = self.load() {
            self.error(e.to_string());
        }
        self.fill_form();
    }

    /// Where a picture of the film came from: Remake is only for those made here.
    pub(crate) fn origin(&mut self, path: &Path) -> Origin {
        if let Some(o) = self.origins.get(path) {
            return o.clone();
        }
        let same = |a: &Path, b: &Path| {
            let len = |p: &Path| std::fs::metadata(p).map(|m| m.len()).ok();
            len(a).is_some() && len(a) == len(b) && std::fs::read(a).ok() == std::fs::read(b).ok()
        };
        let o = if self.project.job_that_made(path).ok().flatten().is_some() {
            Origin::Made
        } else if let Some((t, _)) = self.film.last_frames.iter().find(|(_, f)| same(f, path)) {
            Origin::LastFrame(t.to_string())
        } else {
            Origin::File
        };
        self.origins.insert(path.to_path_buf(), o.clone());
        o
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
        self.cut_takes = None;
        self.load_takes()
    }

    fn load_takes(&mut self) -> anyhow::Result<()> {
        let p = &self.project;
        (self.takes, self.prev_takes, self.continued) = match self.shot {
            Some(r) => {
                let prev = match p.previous_shot(r)? {
                    Some(prev) => p.takes(prev)?,
                    None => vec![],
                };
                let mut continued = None;
                if let Some(next) = p.next_shot(r)? {
                    let n = p.shot(next)?;
                    if n.transition == Transition::Continue
                        && let Some(id) = n.continue_from.or(p.shot(r)?.selected_take)
                    {
                        continued = Some((next, id));
                    }
                }
                (p.takes(r)?, prev, continued)
            }
            None => (vec![], vec![], None),
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
        self.follow_runs(ui.ctx());
        self.shoot_finished();
        self.said();
        self.follow_cut(ui.ctx());
        self.sounds_window(ui.ctx());
        self.maker_popup(ui.ctx());
        let tab = self.tab;
        egui::Panel::left("rail")
            .resizable(false)
            .exact_size(72.0)
            .frame(
                egui::Frame::new()
                    .fill(theme::RACK)
                    .stroke(egui::Stroke::new(1.0, theme::LINE))
                    .inner_margin(egui::Margin::symmetric(8, 12)),
            )
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    let (mark, _) =
                        ui.allocate_exact_size(egui::vec2(32.0, 32.0), egui::Sense::hover());
                    ui.painter().rect_filled(mark, 9.0, theme::TUNGSTEN);
                    kit::glyph(ui.painter(), mark.center(), icon::PLAY, 16.0, theme::RACK);
                    ui.add_space(14.0);
                    for (t, name) in Tab::ALL {
                        if rail_button(ui, self.tab == t, t.icon(), name).clicked() {
                            self.tab = t;
                        }
                    }
                });
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                    if rail_button(ui, false, icon::SETTINGS, "Settings")
                        .on_hover_text("keys, plugins and this film's defaults")
                        .clicked()
                    {
                        self.wants_prefs = true;
                    }
                });
            });
        egui::Panel::top("bar")
            .frame(
                egui::Frame::new()
                    .fill(theme::BAY)
                    .inner_margin(egui::Margin::symmetric(20, 0)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_height(52.0);
                    self.where_we_are(ui);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if kit::icon_button(ui, icon::RELOAD, "Reload")
                            .on_hover_text("read the project folder again (F5)")
                            .clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::F5))
                        {
                            self.status = None;
                            self.reload();
                        }
                        self.jobs_button(ui);
                    });
                });
                let r = ui.max_rect();
                ui.painter()
                    .hline(r.x_range(), r.bottom(), egui::Stroke::new(1.0, theme::LINE));
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
                    ui.set_height(34.0);
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
        if self.tab != Tab::Cut
            && let Some((_, Ok(d))) = &mut self.cut_deck
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
                    let uv = pan_zoom(ui, rect, &img, &mut self.zoom_view);
                    ui.painter().image(t.id(), rect, uv, egui::Color32::WHITE);
                    let zoomed = self.zoom_view.0 > 1.0;
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
                    ui.set_max_width(ui.available_width().min(1080.0));
                    kit::page_title(ui, "Story", Some(&self.project.config.name));
                    ui.add_space(theme::GAP);
                    let models = self.video_models();
                    let format = |me: &mut Self, ui: &mut egui::Ui| {
                        kit::card(ui, |ui| {
                            format_fields(ui, &mut me.texts.format, &models, me.film.made)
                        });
                    };
                    let words = |me: &mut Self, ui: &mut egui::Ui| {
                        let t = &mut me.texts;
                        kit::card(ui, |ui| {
                            card_field(
                                ui,
                                "Story",
                                &mut t.story,
                                "What the film is about; shot drafts read it",
                                10,
                            );
                        });
                        ui.add_space(theme::GAP);
                        kit::card(ui, |ui| {
                            card_field(
                                ui,
                                "Style",
                                &mut t.style,
                                "How every shot should look; added to every prompt",
                                3,
                            );
                            ui.add_space(4.0);
                            hint(ui, "Text between <!-- and --> is left out of prompts.");
                        });
                    };
                    let side = 520.0;
                    if ui.available_width() >= 1000.0 {
                        ui.horizontal_top(|ui| {
                            ui.vertical(|ui| {
                                ui.set_width(ui.available_width() - side - theme::GAP);
                                words(self, ui);
                            });
                            ui.add_space(theme::GAP);
                            ui.vertical(|ui| {
                                ui.set_width(side);
                                format(self, ui);
                            });
                        });
                    } else {
                        format(self, ui);
                        ui.add_space(theme::GAP);
                        words(self, ui);
                    }
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
                            if list_row(ui, &key, &e.name, anchors.len(), None, here).clicked() {
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
                        kit::empty(
                            ui,
                            icon::ELEMENTS,
                            "Cast, sets and props that shots name as @handles. Add the first one on the left.",
                        );
                    }
                    None => kit::empty(ui, icon::ELEMENTS, "Pick an element on the left."),
                }
            });
    }

    fn element_detail(&mut self, ui: &mut egui::Ui, el: &Element, anchors: &[PathBuf]) {
        let mut acts = vec![];
        big_title(
            ui,
            &format!("@{} · {}", el.handle, el.kind),
            &mut self.element_form.name,
            "",
            "Element name",
        )
        .on_hover_text("its name; the @handle stays as prompts use it");
        ui.add_space(theme::GAP);
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width().min(900.0));
            kit::card(ui, |ui| {
                card_field(
                    ui,
                    "Description",
                    &mut self.element_form.description,
                    "What it looks like. Goes into the prompt of every shot that names it; models follow English best.",
                    2,
                );
            });
            ui.add_space(theme::GAP);
            kit::card(ui, |ui| {
            kit::card_title(ui, "Anchor images", (!anchors.is_empty()).then(|| anchors.len().to_string()).as_deref());
            ui.add_space(4.0);
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
                if self.make_button(ui, "Make image…") {
                    let film = &self.project.config;
                    let cfg = &film.image;
                    let defaults = (
                        cfg.aspect.clone().unwrap_or_else(|| film.aspect.clone()),
                        cfg.resolution.clone(),
                    );
                    let me = anchor::For::Element(el.handle.clone());
                    let title = format!("Make an image of @{}", el.handle);
                    self.open_maker(me, title, vec![], None, defaults);
                }
            });
            });
            if el.kind == Kind::Cast {
                ui.add_space(theme::GAP);
                kit::card(ui, |ui| self.voice(ui, el));
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
            .corner_radius(theme::R_ROW)
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
        kit::card_title(ui, "Voice", None);
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
                    kit::empty(
                        ui,
                        icon::SHOTS,
                        "Shots go in chapters. Add the first one on the left.",
                    );
                }
                None => {
                    kit::empty(ui, icon::SHOTS, "Pick a shot on the left.");
                }
            });
    }

    fn shot_detail(&mut self, ui: &mut egui::Ui, r: ShotRef, s: &Shot) {
        let mut acts = vec![];
        big_title(
            ui,
            &format!("Shot {r}"),
            &mut self.form.title,
            "Add a title",
            "Shot title",
        );
        ui.add_space(theme::GAP);
        if self.in_cut_player(ui, s) {
            ui.add_space(theme::GAP);
        }
        // Wide enough for References and its image panel; under the script when there's no
        // room for both side by side.
        let side = 360.0;
        let side_cards = |me: &mut Self, ui: &mut egui::Ui, acts: &mut Vec<Action>| {
            kit::card(ui, |ui| me.facts(ui, s));
            ui.add_space(theme::GAP);
            kit::card(ui, |ui| {
                kit::card_title(ui, "Start", None);
                ui.add_space(4.0);
                me.start(ui, r, s);
            });
            ui.add_space(theme::GAP);
            kit::card(ui, |ui| me.references(ui, r, s, acts));
        };
        if ui.available_width() >= 1000.0 {
            ui.horizontal_top(|ui| {
                let w = (ui.available_width() - side - theme::GAP).min(760.0);
                ui.vertical(|ui| {
                    ui.set_width(w);
                    kit::card(ui, |ui| self.script(ui, r, s, &mut acts));
                });
                ui.add_space(theme::GAP);
                ui.vertical(|ui| {
                    ui.set_width(side);
                    side_cards(self, ui, &mut acts);
                });
            });
        } else {
            kit::card(ui, |ui| self.script(ui, r, s, &mut acts));
            ui.add_space(theme::GAP);
            side_cards(self, ui, &mut acts);
        }
        ui.add_space(theme::GAP);
        kit::card(ui, |ui| {
            kit::card_title(ui, "Shoot", None);
            ui.add_space(4.0);
            self.sent(ui, r);
            ui.add_space(12.0);
            self.shoot_row(ui, r);
        });
        ui.add_space(theme::GAP * 1.5);
        self.take_list(ui, s, &mut acts);
        ui.add_space(theme::GAP * 1.5);
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
            .fill(theme::BAY)
            .stroke(egui::Stroke::new(1.0, theme::LINE))
            .corner_radius(theme::R_ROW)
            .inner_margin(12.0)
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
            ];
            for rc in self.project.config.recipes.keys() {
                if rc != SPOKEN {
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
                     along for the video model to lip sync",
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
        let selected = prev.and_then(|p| {
            self.film
                .chapters
                .iter()
                .flat_map(|(_, s)| s)
                .find(|(x, _)| *x == p)
                .and_then(|(_, s)| s.selected_take.clone())
        });
        let ready = selected.is_some();
        let f = &mut self.form;
        let was = f.transition == Transition::Continue;
        let mut on = was;
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
        if on != was {
            // Started from the take in the cut now, and kept on it.
            f.continue_from = if on { selected.clone() } else { None };
        }
        if on {
            match prev {
                Some(p) => {
                    ui.add_space(8.0);
                    self.continued_frame(ui, p, selected);
                }
                None => hint(
                    ui,
                    "It's the first shot, so there's nothing before it to carry on from.",
                ),
            }
            return;
        }
        ui.add_space(8.0);
        self.frame(ui, r, s);
    }

    /// The last frame of the shot before's take that this one starts on, and a way onto the take
    /// in the cut when that's another.
    fn continued_frame(&mut self, ui: &mut egui::Ui, prev: ShotRef, selected: Option<String>) {
        heading(ui, "Previous shot last frame").on_hover_text(
            "The first frame of the video, carried on from the shot before and colour matched in \
             the cut.",
        );
        ui.add_space(8.0);
        let from = self.form.continue_from.clone().or(selected.clone());
        let take = from
            .as_ref()
            .and_then(|id| self.prev_takes.iter().find(|t| t.id.to_string() == *id));
        match take {
            Some(t) => {
                let video = t
                    .output()
                    .and_then(|j| Some(self.project.job_dir(j.id).join(j.outputs.first()?)));
                if let (Some(png), Some(video)) = (self.project.last_frame(t), video) {
                    let name = format!("last frame of {t}");
                    if png.is_file() {
                        if self.thumbs.show(ui, &png, 150.0, &name) {
                            self.zoom = Some(png);
                        }
                    } else {
                        make_still(
                            ui.ctx(),
                            (self.ff.as_ref(), &self.posters),
                            &video,
                            &png,
                            true,
                        );
                        let (rect, r) = ui.allocate_exact_size(
                            egui::vec2(150.0 * 16.0 / 9.0, 150.0),
                            egui::Sense::hover(),
                        );
                        ui.painter().rect_filled(rect, 2.0, theme::RACK);
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, true, &name));
                    }
                }
                ui.label(
                    RichText::new(format!("{t}'s last frame"))
                        .small()
                        .color(theme::DUST),
                );
            }
            None => {
                let why = match from {
                    Some(_) => format!("The take of {prev} it started from is gone."),
                    None => format!(
                        "{prev} has no take in the cut yet, so there's nothing to carry on from."
                    ),
                };
                ui.label(RichText::new(why).color(theme::FLAG));
            }
        }
        if let Some(sel) = selected
            && from.as_ref() != Some(&sel)
        {
            let now = self
                .prev_takes
                .iter()
                .find(|t| t.id.to_string() == sel)
                .map_or_else(|| "another take".to_string(), |t| format!("#{}", t.number));
            ui.add_space(6.0);
            hint(ui, &format!("{prev} plays {now} in the cut now."));
            if kit::primary_button(ui, None, "Use new previous shot last frame")
                .on_hover_text(format!("start on the last frame of {prev}'s {now} instead"))
                .clicked()
            {
                self.form.continue_from = Some(sel);
            }
        }
    }

    /// The storyboard frame: a still the video starts on, and what it's made from.
    fn frame(&mut self, ui: &mut egui::Ui, r: ShotRef, s: &Shot) {
        heading(ui, "Storyboard frame").on_hover_text(
            "The first frame of the video, to settle the framing on a cheap still before paying \
             for video. Optional.",
        );
        ui.add_space(8.0);
        let mut acts = vec![];
        let mut made = false;
        if let Some(f) = &s.frame {
            let path = self.project.root.join(f);
            if self.thumbs.show(ui, &path, 150.0, f) {
                self.zoom = Some(path.clone());
            }
            let origin = self.origin(&path);
            made = origin == Origin::Made;
            if let Some(tag) = origin.tag() {
                ui.label(RichText::new(tag).small().color(theme::DUST));
            }
            ui.add_space(4.0);
        }
        let rows = self.made_rows(&browse::Near::Frame(r));
        let (mut make, mut remake) = (false, false);
        ui.horizontal_wrapped(|ui| {
            match browse::dropdown(
                ui,
                "Browse existing frames",
                ("frame", r.to_string()),
                &rows,
                &mut self.thumbs,
                "a frame made before, or the last frame of a take",
            ) {
                Some(browse::Browsed::Pick(f)) => acts.push(Action::SetFrame(r, Some(f))),
                Some(browse::Browsed::Forget(id)) => acts.push(Action::ForgetImage(id)),
                None => {}
            }
            let file = || {
                rfd::FileDialog::new()
                    .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
                    .pick_file()
            };
            if s.frame.is_none() {
                if ui.button("Add image…").clicked()
                    && let Some(f) = file()
                {
                    acts.push(Action::SetFrame(r, Some(f)));
                }
                make = self.make_button(ui, "Make frame…");
                return;
            }
            let mut from_file = false;
            ui.menu_button("Replace…", |ui| {
                if ui.button("With a file…").clicked() {
                    from_file = true;
                    ui.close();
                }
                if self.make_button(ui, "With a new one…") {
                    make = true;
                    ui.close();
                }
            })
            .response
            .on_hover_text("another frame in its place; undo brings this one back");
            if from_file && let Some(f) = file() {
                acts.push(Action::SetFrame(r, Some(f)));
            }
            if made {
                remake = self.make_button(ui, "Remake…");
            }
            let rm = remove_button(ui)
                .on_hover_text("no storyboard frame; the video starts as it likes");
            rm.widget_info(|| {
                egui::WidgetInfo::labeled(egui::Role::Button, true, "Remove storyboard frame")
            });
            if rm.clicked() {
                acts.push(Action::SetFrame(r, None));
            }
        });
        let defaults = self.image_defaults(s);
        if remake && let Some(f) = &s.frame {
            let f = self.project.root.join(f);
            self.remake(anchor::For::Frame(r, Some(f.clone())), &f, defaults);
        } else if make {
            // What the video gets, but not the frame itself.
            let frame = s.frame.as_ref().map(|f| self.project.root.join(f));
            let refs = self
                .built(r)
                .map(|b| b.references)
                .unwrap_or_default()
                .into_iter()
                .map(|x| self.project.root.join(&x.path))
                .filter(|p| p.is_file() && Some(p) != frame.as_ref())
                .collect();
            let title = format!("Make a frame for {r}");
            let m = self.open_maker(anchor::For::Frame(r, frame), title, refs, None, defaults);
            m.set_prompt(s.frame_prompt.as_deref().unwrap_or_default());
        }
        if !acts.is_empty() {
            self.flush();
            self.apply(acts);
        }
    }

    fn take_list(&mut self, ui: &mut egui::Ui, s: &Shot, acts: &mut Vec<Action>) {
        section(ui, "Takes");
        ui.add_space(6.0);
        let mut sounds_for = None;
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
                .corner_radius(theme::R_ROW)
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
                                let next = self
                                    .continued
                                    .as_ref()
                                    .filter(|(_, id)| *id == t.id.to_string())
                                    .map(|(next, _)| *next);
                                let rm = ui.add_enabled_ui(next.is_none(), remove_button).inner;
                                rm.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::Role::Button,
                                        next.is_none(),
                                        format!("Remove #{n}"),
                                    )
                                });
                                if rm
                                    .on_hover_text("off the list; what it cost stays in the ledger")
                                    .on_disabled_hover_text(format!(
                                        "{} starts on its last frame; have it start on another \
                                         take first",
                                        next.map(|n| n.to_string()).unwrap_or_default()
                                    ))
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
                            if video.is_some() && selected {
                                let k = self.project.take_sounds(t).map_or(0, |s| s.len());
                                let label = match k {
                                    0 => "Sounds…".to_string(),
                                    k => format!("Sounds ({k})…"),
                                };
                                if act(ui, &label, n)
                                    .on_hover_text("effects or a voice off over it in the cut")
                                    .clicked()
                                {
                                    sounds_for = Some(t.clone());
                                }
                            }
                            if video.is_some()
                                && !selected
                                && act(ui, "Use in the cut", n)
                                    .on_hover_text("the take this shot plays in the cut")
                                    .clicked()
                            {
                                if self
                                    .continued
                                    .as_ref()
                                    .is_some_and(|(_, id)| *id != t.id.to_string())
                                {
                                    self.confirm_select = Some(t.clone());
                                } else {
                                    acts.push(Action::Select(t.clone()));
                                    self.cut_follow = Some(t.clone());
                                }
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
        if let Some(t) = sounds_for {
            self.open_sounds((&t).into(), t.to_string(), 0.0);
        }
        self.confirm_select(ui, acts);
        if let Some(pieces) = splice
            && let Some(r) = self.shot
        {
            self.open_splicer(r, pieces);
        }
    }

    /// Asks before another take goes in the cut while the shot after starts on this one's frame.
    fn confirm_select(&mut self, ui: &mut egui::Ui, acts: &mut Vec<Action>) {
        let (Some(t), Some((next, id))) = (&self.confirm_select, &self.continued) else {
            self.confirm_select = None;
            return;
        };
        let from = self
            .takes
            .iter()
            .find(|x| x.id.to_string() == *id)
            .map_or("a take".to_string(), |x| format!("#{}", x.number));
        let (mut yes, mut no) = (false, false);
        let modal = crate::modal(ui.ctx(), "keep the next start", |ui| {
            ui.set_width(kit::modal_width(ui.ctx(), 1.0, 420.0, 420.0));
            ui.heading(format!("Use #{} in the cut?", t.number));
            ui.add_space(6.0);
            ui.label(format!(
                "{next} continues from the last frame of {from}. It keeps starting on that \
                 frame; pick the new one in {next} when you want it to follow."
            ));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                yes = ui.button(format!("Use #{}", t.number)).clicked();
                no = ui.button("Cancel").clicked();
            });
        });
        if yes {
            acts.push(Action::Select(t.clone()));
            self.cut_follow = Some(t.clone());
        }
        if yes || no || modal.should_close() {
            self.confirm_select = None;
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
            .map(|p| Deck::new(&label, p, true).in_popup(320.0).opens());
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
        if d.clicked() {
            self.watch(ui, take);
        }
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
        let wide = kit::modal_width(ui.ctx(), 0.6, 360.0, 900.0);
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
    make: (Option<&Ffmpeg>, &Arc<Mutex<HashSet<PathBuf>>>),
    video: &Path,
    png: &Path,
) {
    make_still(ctx, make, video, png, false);
}

/// A video's first frame, or its last, into `png` in the background.
pub(crate) fn make_still(
    ctx: &egui::Context,
    (ff, making): (Option<&Ffmpeg>, &Arc<Mutex<HashSet<PathBuf>>>),
    video: &Path,
    png: &Path,
    last: bool,
) {
    if let Some(ff) = ff
        && making.lock().unwrap().insert(png.to_path_buf())
    {
        let (ff, video, png) = (ff.clone(), video.to_path_buf(), png.to_path_buf());
        let (making, ctx) = (making.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = if last {
                ff.last_frame(&video, &png)
            } else {
                ff.poster(&video, &png)
            };
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

/// A page in the rail: its icon over its name, on a lit tile when open.
fn rail_button(ui: &mut egui::Ui, open: bool, icon: &str, name: &str) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(56.0, 54.0), egui::Sense::click());
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, open, name));
    let p = ui.painter();
    if open {
        p.rect_filled(rect, theme::R_ROW, theme::RAISED);
    } else if r.hovered() {
        p.rect_filled(rect, theme::R_ROW, theme::CARD);
    }
    let color = if open || r.hovered() {
        theme::PAPER
    } else {
        theme::DUST
    };
    kit::glyph(p, rect.center() - egui::vec2(0.0, 8.0), icon, 20.0, color);
    p.text(
        rect.center() + egui::vec2(0.0, 13.0),
        egui::Align2::CENTER_CENTER,
        name,
        egui::FontId::proportional(10.5),
        color,
    );
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
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
                    .color(theme::GRAPHITE),
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
        Some(s.selected_take.is_some()),
        open,
    )
}

/// A row in a side list: its key in grey, a title, a count and maybe an amber dot.
fn list_row(
    ui: &mut egui::Ui,
    key: &str,
    title: &str,
    count: usize,
    dot: Option<bool>,
    open: bool,
) -> egui::Response {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 34.0), egui::Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::Button, true, open, format!("{key}  {title}"))
    });
    let p = ui.painter();
    if open {
        p.rect_filled(rect, theme::R_ROW, theme::RAISED);
    } else if resp.hovered() {
        p.rect_filled(rect, theme::R_ROW, theme::CARD);
    }
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mid = rect.center().y;
    // Amber once something of it is in the cut, an empty ring until then.
    let at = egui::pos2(rect.left() + 14.0, mid);
    match dot {
        Some(true) => {
            p.circle_filled(at, 4.0, theme::TUNGSTEN);
        }
        Some(false) => {
            p.circle_stroke(at, 3.5, egui::Stroke::new(1.5, theme::EDGE));
        }
        None => {}
    }
    let num = p.layout_no_wrap(
        key.to_string(),
        font.clone(),
        if open { theme::GRAPHITE } else { theme::FAINT },
    );
    let x = rect.left() + if dot.is_some() { 28.0 } else { 12.0 };
    p.galley(
        egui::pos2(x, mid - num.size().y / 2.0),
        num.clone(),
        theme::DUST,
    );

    let mut right = rect.right() - 10.0;
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

pub(crate) fn camera_icon(p: &egui::Painter, s: egui::Rect, stroke: egui::Stroke) {
    let s = s.expand(1.0);
    let body = egui::Rect::from_min_max(egui::pos2(s.left(), s.top() + 2.5), s.max);
    p.rect_stroke(body, 1.5, stroke, egui::StrokeKind::Middle);
    p.line_segment(
        [
            egui::pos2(s.center().x - 2.0, s.top() + 1.0),
            egui::pos2(s.center().x + 2.0, s.top() + 1.0),
        ],
        stroke,
    );
    p.circle_stroke(body.center(), body.height() * 0.28, stroke);
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
    let frame = egui::Frame::new()
        .fill(theme::CARD)
        .stroke(egui::Stroke::new(1.0, theme::EDGE))
        .corner_radius(theme::R_POPUP)
        .inner_margin(egui::Margin::same(20))
        .shadow(egui::Shadow {
            offset: [0, 24],
            blur: 64,
            spread: 0,
            color: egui::Color32::from_black_alpha(140),
        });
    let r = egui::Modal::new(egui::Id::unique(name))
        .frame(frame)
        .backdrop_color(egui::Color32::from_rgba_unmultiplied(8, 8, 10, 170))
        .show(ctx, content);
    let me = r.response.layer_id;
    let shown = ctx.memory(|m| m.areas().visible_last_frame(&me));
    // In the order they were opened: one opened from another stays above it, even though a
    // click on the one below raises that.
    let others = ctx.data_mut(|d| {
        let all = d.get_temp_mut_or_default::<Vec<egui::LayerId>>(modals);
        if !shown || !all.contains(&me) {
            all.retain(|l| *l != me);
            all.push(me);
        }
        all.clone()
    });
    let newest = ctx.memory(|m| {
        others
            .iter()
            .rev()
            .find(|l| **l == me || m.areas().visible_last_frame(l))
            .copied()
    });
    if newest == Some(me) {
        ctx.move_to_top(me);
    }
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
    let (rect, r) = ui.allocate_exact_size(egui::vec2(28.0, 28.0), egui::Sense::click());
    let enabled = ui.is_enabled();
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    if ui.is_rect_visible(rect) {
        let lit = enabled && r.hovered();
        if lit {
            ui.painter().rect_filled(
                rect,
                theme::R_CONTROL,
                egui::Color32::from_rgb(0x3a, 0x2a, 0x28),
            );
        }
        let color = match (enabled, lit) {
            (false, _) => theme::FAINT,
            (true, true) => theme::FLAG,
            (true, false) => theme::DUST,
        };
        kit::glyph(ui.painter(), rect.center(), icon::TRASH, 16.0, color);
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
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

/// One job in the jobs list: what for on top, how it's going under it.
fn job_row(ui: &mut egui::Ui, title: &str, how: &str, s: &running::State) -> egui::Response {
    let failed = matches!(s, running::State::Failed(_) | running::State::Stopped);
    let (rect, r) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 52.0), egui::Sense::click());
    r.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Button, true, format!("{title} · {how}"))
    });
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        if r.hovered() {
            p.rect_filled(rect, theme::R_ROW, theme::RAISED);
        }
        let at = egui::pos2(rect.left() + 20.0, rect.center().y);
        match s {
            running::State::Running => {
                kit::ring(p, at, 7.0, 2.0, theme::STEEL, ui.input(|i| i.time));
                ui.ctx().request_repaint();
            }
            running::State::Made(_) => kit::glyph(p, at, icon::CHECK, 16.0, theme::GO),
            _ => kit::glyph(p, at, icon::WARNING, 16.0, theme::FLAG),
        }
        let x = rect.left() + 40.0;
        let w = rect.right() - x - 10.0;
        let top = p.layout(
            title.to_string(),
            egui::FontId::proportional(13.5),
            theme::PAPER,
            w,
        );
        let under = p.layout(
            how.to_string(),
            egui::FontId::proportional(12.0),
            if failed { theme::FLAG } else { theme::DUST },
            w,
        );
        p.galley(egui::pos2(x, rect.top() + 8.0), top, theme::PAPER);
        p.galley(egui::pos2(x, rect.top() + 28.0), under, theme::DUST);
    }
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The title of a part of a page, like Takes.
fn section(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(theme::section())
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

/// A page's big editable title, with what it is in small above it.
fn big_title(
    ui: &mut egui::Ui,
    over: &str,
    text: &mut String,
    hint: &str,
    label: &str,
) -> egui::Response {
    ui.label(RichText::new(over).small().color(theme::DUST));
    let r = ui.add(
        egui::TextEdit::singleline(text)
            .font(theme::title())
            .text_color(theme::PAPER)
            .hint_text(hint)
            .frame(egui::Frame::NONE)
            .margin(egui::Margin::ZERO)
            .desired_width(f32::INFINITY),
    );
    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, label));
    r
}

/// A text box in a card, under the card's heading as its label.
fn card_field(
    ui: &mut egui::Ui,
    label: &str,
    text: &mut String,
    hint: &str,
    rows: usize,
) -> egui::Response {
    let cap = kit::card_title(ui, label, None).id;
    ui.add_space(4.0);
    ui.add(
        egui::TextEdit::multiline(text)
            .hint_text(RichText::new(hint).color(theme::DUST))
            .desired_rows(rows)
            .desired_width(f32::INFINITY)
            .margin(egui::vec2(10.0, 8.0)),
    )
    .labelled_by(cap)
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
    let icon = if text == "Undo" {
        icon::UNDO
    } else {
        icon::REDO
    };
    what.is_some_and(|w| {
        kit::icon_button(ui, icon, text)
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
    kit::danger_button(ui, "Remove")
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
            state.bg_stroke = egui::Stroke::NONE;
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

/// Where a picture came from.
#[derive(Clone, PartialEq)]
pub(crate) enum Origin {
    /// An image job here made it, so it can be made again.
    Made,
    /// A take's last frame, e.g. "01-010#3".
    LastFrame(String),
    File,
}

impl Origin {
    /// Said under the picture when it can't be made again.
    pub(crate) fn tag(&self) -> Option<String> {
        match self {
            Origin::Made => None,
            Origin::LastFrame(t) => Some(format!("last frame of {t}")),
            Origin::File => Some("from a file".into()),
        }
    }
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
const PAGE: egui::Vec2 = egui::vec2(32.0, 28.0);
const SIDE: egui::Vec2 = egui::vec2(12.0, 16.0);

/// Scrolls `add` with the bar at the panel's edge: the panel has no margin, the content does.
fn page<R>(ui: &mut egui::Ui, margin: egui::Vec2, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    // No wider than the window: a card filling the width would otherwise keep the scroll area
    // as wide as it ever got.
    let width = ui.available_width() - 2.0 * margin.x;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(margin)
                .show(ui, |ui| {
                    ui.set_max_width(width);
                    add(ui)
                })
                .inner
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
    kit::card_title(ui, "Format", None);
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
                .width((ui.available_width() - 8.0).clamp(200.0, 360.0))
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
            {
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
                ui.end_row();
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
            }
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
