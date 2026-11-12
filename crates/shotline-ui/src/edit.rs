//! Changing the project from the app. Widgets only edit `Form`; once nothing has focus the form is
//! compared with the shot as read from disk, and each difference becomes an `Action`. `apply` is
//! the one place that writes, through shotline-core like the CLI, and it keeps each shot as it was
//! before and after so the change can be undone.

use shotline_core::ledger::money;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use shotline_core::{
    CONFIG_FILE, Element, Format, Kind, ModelRef, Project, Shot, ShotRef, TIMELINE_FILE, Take,
    Task, Timeline, Track, Transition, Under, Voice,
};

use crate::App;

/// What's typed into the open shot's fields.
#[derive(Default, Clone, PartialEq)]
pub(crate) struct Form {
    pub shot: Option<ShotRef>,
    pub title: String,
    pub plot: String,
    pub prompt: String,
    pub frame_prompt: String,
    pub duration: f64,
    pub transition: Transition,
    /// The take of the shot before it starts on, by id; None for that shot's selected take.
    pub continue_from: Option<String>,
    pub recipe: Option<String>,
    /// Plugin and model instead of the film's.
    pub model: Option<(String, String)>,
    /// What each of the shot's own images shows.
    pub image_notes: Vec<String>,
    /// What each element anchor it sends shows here, by path.
    pub anchor_notes: std::collections::BTreeMap<String, String>,
    /// Instead of the film's.
    pub resolution: Option<String>,
    pub lines: Vec<LineForm>,
    /// The line being added.
    pub new: LineForm,
    /// The row for a new line is showing.
    pub adding: bool,
}

#[derive(Default, Clone, PartialEq)]
pub(crate) struct LineForm {
    pub who: String,
    pub text: String,
    /// Seconds into the shot, or empty to follow the line before.
    pub at: String,
}

impl LineForm {
    fn of(l: &shotline_core::Line) -> LineForm {
        LineForm {
            who: l.who.clone(),
            text: l.text.clone(),
            at: l.at.map(money).unwrap_or_default(),
        }
    }

    fn at(&self) -> anyhow::Result<Option<f64>> {
        let t = self.at.trim().trim_end_matches('s').trim();
        if t.is_empty() || t == "next" {
            return Ok(None);
        }
        t.parse()
            .map(Some)
            .map_err(|_| anyhow::anyhow!("\"{}\" isn't a number of seconds", self.at))
    }
}

/// What's typed into the open element's fields.
#[derive(Default, Clone, PartialEq)]
pub(crate) struct ElementForm {
    pub handle: Option<String>,
    pub name: String,
    pub description: String,
    pub voice: VoiceForm,
    /// Anchor labels by file name; an empty one is dropped.
    pub labels: BTreeMap<String, String>,
}

#[derive(Default, Clone, PartialEq)]
pub(crate) struct VoiceForm {
    pub plugin: String,
    pub model: String,
    pub voice_id: String,
    /// One `key = value` a line.
    pub settings: String,
}

impl ElementForm {
    pub fn of(el: &Element) -> ElementForm {
        let v = el.voice.clone().unwrap_or_default();
        ElementForm {
            handle: Some(el.handle.clone()),
            name: el.name.clone(),
            description: el.description.clone(),
            voice: VoiceForm {
                plugin: v.plugin,
                model: v.model.unwrap_or_default(),
                voice_id: v.voice_id.unwrap_or_default(),
                settings: settings_text(&v.settings),
            },
            labels: el.labels.clone(),
        }
    }

    /// `el` with the form's fields.
    fn element(&self, el: &Element) -> anyhow::Result<Element> {
        let v = &self.voice;
        let some = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
        let voice = match some(&v.plugin) {
            None => None,
            Some(plugin) => {
                let mut settings = toml::Table::new();
                for line in v.settings.lines().filter(|l| !l.trim().is_empty()) {
                    let Some((k, val)) = line.split_once('=') else {
                        anyhow::bail!("voice settings go one a line as key = value, not {line:?}");
                    };
                    settings.insert(k.trim().to_string(), setting_value(val));
                }
                Some(Voice {
                    plugin,
                    model: some(&v.model),
                    voice_id: some(&v.voice_id),
                    settings,
                })
            }
        };
        let name = self.name.trim();
        if name.is_empty() {
            anyhow::bail!("@{} needs a name", el.handle);
        }
        Ok(Element {
            name: name.to_string(),
            description: self.description.clone(),
            voice,
            labels: self.labels.clone(),
            ..el.clone()
        })
    }
}

/// A typed setting: TOML when it parses, else the text as a string.
pub(crate) fn setting_value(text: &str) -> toml::Value {
    let t = text.trim();
    t.parse::<toml::Value>()
        .unwrap_or_else(|_| toml::Value::String(t.to_string()))
}

pub(crate) fn settings_text(t: &toml::Table) -> String {
    t.iter().map(|(k, v)| format!("{k} = {v}\n")).collect()
}

/// The `key = value` lines that parse; `element` complains about the rest.
pub(crate) fn settings_table(text: &str) -> toml::Table {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), setting_value(v)))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

/// The element being added.
#[derive(Clone)]
pub(crate) struct NewElement {
    pub kind: Kind,
    pub name: String,
    pub handle: String,
}

impl Default for NewElement {
    fn default() -> NewElement {
        NewElement {
            kind: Kind::Cast,
            name: String::new(),
            handle: String::new(),
        }
    }
}

pub(crate) enum Field {
    Title(String),
    Plot(String),
    Prompt(String),
    FramePrompt(String),
    Duration(f64),
    Transition(Transition),
    ContinueFrom(Option<String>),
    Recipe(Option<String>),
    Model(Option<(String, String)>),
    Resolution(Option<String>),
}

pub(crate) enum Action {
    Set(ShotRef, Field),
    AddLine(ShotRef, LineForm),
    /// A line added with a recording made somewhere else, copied in as what it sends.
    ImportLine(ShotRef, LineForm, PathBuf),
    /// A sound from a file over a take in the cut, starting that many seconds into it.
    AddSound(Under, PathBuf, f64),
    /// A sound already made, like a recording, over a take; by job id.
    AddSoundJob(Under, String, f64),
    /// A take's sound taken off, by its job id.
    RemoveSound(Under, String),
    SetLine(ShotRef, usize, LineForm),
    RemoveLine(ShotRef, usize),
    /// The recording a line sends, by job id, or None to make it with each take.
    PickRecording(ShotRef, usize, Option<String>),
    /// A recording thrown out, by job id; lines of the shot that sent it go back to their words.
    DeleteRecording(ShotRef, String),
    MoveLine(ShotRef, usize, usize),
    Select(Take),
    /// Off the list; its files stay, as the ledger points at them.
    RemoveTake(Take),
    Story(String),
    Style(String),
    Format(Format),
    /// One of a model's options for the whole film; None takes it out.
    Param {
        plugin: String,
        model: String,
        key: String,
        value: Option<toml::Value>,
    },
    /// The film's default model for a job; None takes it out where it can be.
    Model(Task, Option<ModelRef>),
    Budget(shotline_core::ledger::Budgets),
    AddChapter(String),
    RenameChapter(u32, String),
    /// A new shot at the end of the chapter, or after the given number.
    AddShot(u32, Option<u32>),
    SaveElement(Element),
    AddElement(NewElement),
    /// An image file copied in as the element's next anchor.
    AddAnchor(Element, PathBuf),
    RemoveAnchor(Element, PathBuf),
    /// An image file copied in as the shot's next picture.
    AddShotImage(ShotRef, PathBuf),
    RemoveShotImage(ShotRef, usize),
    /// A made image off the Browse existing list, by job id; its file stays.
    ForgetImage(String),
    ShotImageNote(ShotRef, usize, String),
    /// What an element's anchor shows in this shot, by its path.
    AnchorNote(ShotRef, String, String),
    /// A storyboard frame copied in, or None to take it away.
    SetFrame(ShotRef, Option<PathBuf>),
    /// An element's anchor left out of the shot (true) or put back.
    LeaveOut(ShotRef, String, bool),
    /// One anchor of an element sent with the shot: put back if it was left out, or the element
    /// attached with only this anchor.
    SendAnchor(ShotRef, String, String),
    Timeline(Timeline),
    /// Music for the cut; a file from outside the film is copied into music/ first.
    AddTrack(PathBuf),
}

impl Action {
    /// What it's about, as the undo button names it.
    fn name(&self) -> String {
        match self {
            Action::Set(r, _)
            | Action::AddLine(r, _)
            | Action::ImportLine(r, ..)
            | Action::SetLine(r, ..)
            | Action::RemoveLine(r, _)
            | Action::PickRecording(r, ..)
            | Action::DeleteRecording(r, _)
            | Action::MoveLine(r, ..)
            | Action::AddShotImage(r, _)
            | Action::RemoveShotImage(r, _)
            | Action::ShotImageNote(r, ..)
            | Action::AnchorNote(r, ..)
            | Action::SetFrame(r, _)
            | Action::LeaveOut(r, ..)
            | Action::SendAnchor(r, ..) => r.to_string(),
            Action::Select(t) | Action::RemoveTake(t) => t.shot.to_string(),
            Action::AddSound(u, ..) | Action::AddSoundJob(u, ..) | Action::RemoveSound(u, _) => {
                under_name(*u)
            }
            Action::Story(_) => "the story".into(),
            Action::Style(_) => "the style".into(),
            Action::Format(_) => "the format".into(),
            Action::Param { model, key, .. } => format!("{key} for {model}"),
            Action::Model(task, _) => format!("the {} model", task_name(*task)),
            Action::Budget(_) => "the budget".into(),
            Action::AddChapter(t) => format!("chapter {t}"),
            Action::RenameChapter(n, _) => format!("chapter {n:02}'s title"),
            Action::AddShot(..) => "a new shot".into(),
            Action::AddElement(n) => format!("new {}", n.kind),
            Action::SaveElement(el) | Action::AddAnchor(el, _) | Action::RemoveAnchor(el, _) => {
                format!("@{}", el.handle)
            }
            Action::ForgetImage(_) => "a made image".into(),
            Action::Timeline(_) => "the cut".into(),
            Action::AddTrack(_) => "music".into(),
        }
    }

    /// Files it rewrites. Files it makes are told by `write` afterwards.
    fn files(&self, p: &Project) -> Vec<PathBuf> {
        match self {
            Action::Set(r, _)
            | Action::AddLine(r, _)
            | Action::ImportLine(r, ..)
            | Action::SetLine(r, ..)
            | Action::RemoveLine(r, _)
            | Action::PickRecording(r, ..)
            | Action::MoveLine(r, ..)
            | Action::AddShotImage(r, _)
            | Action::ShotImageNote(r, ..)
            | Action::AnchorNote(r, ..)
            | Action::LeaveOut(r, ..)
            | Action::SendAnchor(r, ..) => p.shot_path(*r).into_iter().collect(),
            Action::SetFrame(r, _) => {
                let mut out: Vec<PathBuf> = p.shot_path(*r).into_iter().collect();
                if let Ok(s) = p.shot(*r)
                    && let Some(f) = s.frame
                {
                    out.push(p.root.join(f));
                }
                out
            }
            Action::RemoveShotImage(r, i) => {
                let mut out: Vec<PathBuf> = p.shot_path(*r).into_iter().collect();
                if let Ok(s) = p.shot(*r)
                    && let Some(img) = s.images.get(*i)
                {
                    out.push(p.root.join(&img.file));
                }
                out
            }
            Action::AddSound(u, ..) | Action::AddSoundJob(u, ..) => sounds_file(p, *u),
            Action::RemoveSound(u, id) => sounds_file(p, *u)
                .into_iter()
                .chain(p.job(id).map(|j| p.job_dir(j.id).join("job.toml")))
                .collect(),
            Action::DeleteRecording(r, id) => {
                let mut out: Vec<PathBuf> = p.shot_path(*r).into_iter().collect();
                if let Ok(j) = p.job(id) {
                    let dir = p.job_dir(j.id);
                    if j.take.is_none() {
                        tree(&dir, &mut out);
                    } else {
                        out.push(dir.join("job.toml"));
                    }
                }
                out
            }
            // The shot after may be kept on the take it started from.
            Action::Select(t) => p
                .shot_path(t.shot)
                .into_iter()
                .chain(
                    p.next_shot(t.shot)
                        .ok()
                        .flatten()
                        .and_then(|n| p.shot_path(n).ok()),
                )
                .collect(),
            Action::RemoveTake(t) => p
                .shot_path(t.shot)
                .into_iter()
                .chain(t.jobs.iter().map(|j| p.job_dir(j.id).join("job.toml")))
                .collect(),
            Action::RenameChapter(n, _) => p
                .chapter(&n.to_string())
                .map(|c| c.dir.join("chapter.toml"))
                .into_iter()
                .collect(),
            Action::Story(_) => vec![p.root.join("story.md")],
            Action::Style(_) => vec![p.root.join("style.md")],
            Action::Format(_) | Action::Param { .. } | Action::Model(..) | Action::Budget(_) => {
                vec![p.root.join(CONFIG_FILE)]
            }
            Action::SaveElement(el) => vec![p.element_file(el.kind, &el.handle)],
            Action::RemoveAnchor(el, f) => vec![f.clone(), p.element_file(el.kind, &el.handle)],
            Action::ForgetImage(id) => p
                .job(id)
                .map(|j| p.job_dir(j.id).join("job.toml"))
                .into_iter()
                .collect(),
            Action::Timeline(_) | Action::AddTrack(_) => vec![p.root.join(TIMELINE_FILE)],
            Action::AddChapter(_)
            | Action::AddShot(..)
            | Action::AddElement(_)
            | Action::AddAnchor(..) => vec![],
        }
    }
}

/// A path as it was or will be.
#[derive(Clone, PartialEq)]
enum State {
    Gone,
    Dir,
    File(Vec<u8>),
}

impl State {
    fn of(path: &Path) -> State {
        if path.is_dir() {
            State::Dir
        } else {
            std::fs::read(path).map_or(State::Gone, State::File)
        }
    }

    fn put(&self, path: &Path) -> std::io::Result<()> {
        match self {
            State::Gone if path.is_dir() => std::fs::remove_dir(path),
            State::Gone => match std::fs::remove_file(path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                r => r,
            },
            State::Dir => std::fs::create_dir_all(path),
            State::File(b) => {
                if let Some(d) = path.parent() {
                    std::fs::create_dir_all(d)?;
                }
                std::fs::write(path, b)
            }
        }
    }
}

/// A path and everything under it, folders before what's in them.
fn tree(path: &Path, out: &mut Vec<PathBuf>) {
    out.push(path.to_path_buf());
    if let Ok(rd) = std::fs::read_dir(path) {
        let mut kids: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        kids.sort();
        for k in kids {
            tree(&k, out);
        }
    }
}

struct Change {
    path: PathBuf,
    before: State,
    after: State,
}

impl Change {
    /// The path as it is now and as it will be once this change is undone, or redone.
    fn ends(&self, back: bool) -> (&State, &State) {
        if back {
            (&self.after, &self.before)
        } else {
            (&self.before, &self.after)
        }
    }
}

/// What one save did, as a whole: undo takes back every file it touched.
struct Step {
    names: Vec<String>,
    changes: Vec<Change>,
}

impl Step {
    fn what(&self) -> String {
        format!("the change to {}", self.names.join(" and "))
    }

    fn record(&mut self, path: PathBuf, before: State) {
        let after = State::of(&path);
        match self.changes.iter_mut().find(|c| c.path == path) {
            Some(c) => c.after = after,
            None => self.changes.push(Change {
                path,
                before,
                after,
            }),
        }
    }
}

/// What `write` did: the message for the status bar and any paths it made.
struct Done {
    msg: String,
    made: Vec<PathBuf>,
}

impl From<String> for Done {
    fn from(msg: String) -> Done {
        Done { msg, made: vec![] }
    }
}

#[derive(Default)]
pub(crate) struct History {
    undo: Vec<Step>,
    redo: Vec<Step>,
}

impl History {
    pub fn undo_what(&self) -> Option<String> {
        self.undo.last().map(Step::what)
    }

    pub fn redo_what(&self) -> Option<String> {
        self.redo.last().map(Step::what)
    }
}

/// What's typed into the story and style.
#[derive(Default, Clone, PartialEq)]
pub(crate) struct Texts {
    pub story: String,
    pub style: String,
    pub format: Format,
}

impl Form {
    pub fn of(r: ShotRef, s: &Shot, default_duration: f64) -> Form {
        Form {
            shot: Some(r),
            title: s.title.clone(),
            plot: s.plot.clone(),
            prompt: s.prompt.clone(),
            frame_prompt: s.frame_prompt.clone().unwrap_or_default(),
            duration: s.duration.unwrap_or(default_duration),
            transition: s.transition,
            continue_from: s.continue_from.clone(),
            recipe: s.recipe.clone(),
            model: video_model(s),
            resolution: s.model.as_ref().and_then(|m| m.resolution.clone()),
            image_notes: s.images.iter().map(|i| i.note.clone()).collect(),
            anchor_notes: s.anchor_notes.clone(),
            lines: s.lines.iter().map(LineForm::of).collect(),
            new: LineForm::default(),
            adding: false,
        }
    }

    /// What has to be written for the shot on disk to match the form.
    pub(crate) fn changes(&self, r: ShotRef, s: &Shot, default_duration: f64) -> Vec<Action> {
        let mut out = vec![];
        let mut set = |differs: bool, f: Field| {
            if differs {
                out.push(Action::Set(r, f));
            }
        };
        set(self.title != s.title, Field::Title(self.title.clone()));
        set(self.plot != s.plot, Field::Plot(self.plot.clone()));
        set(self.prompt != s.prompt, Field::Prompt(self.prompt.clone()));
        set(
            self.frame_prompt != s.frame_prompt.clone().unwrap_or_default(),
            Field::FramePrompt(self.frame_prompt.clone()),
        );
        set(
            self.duration != s.duration.unwrap_or(default_duration),
            Field::Duration(self.duration),
        );
        set(
            self.transition != s.transition,
            Field::Transition(self.transition),
        );
        set(
            self.continue_from != s.continue_from,
            Field::ContinueFrom(self.continue_from.clone()),
        );
        set(self.recipe != s.recipe, Field::Recipe(self.recipe.clone()));
        set(
            self.model != video_model(s),
            Field::Model(self.model.clone()),
        );
        set(
            self.resolution != s.model.as_ref().and_then(|m| m.resolution.clone()),
            Field::Resolution(self.resolution.clone()),
        );
        for (i, (n, img)) in self.image_notes.iter().zip(&s.images).enumerate() {
            if n.trim() != img.note {
                out.push(Action::ShotImageNote(r, i, n.clone()));
            }
        }
        for (path, n) in &self.anchor_notes {
            let was = s.anchor_notes.get(path).map_or("", |n| n.as_str());
            if n.trim() != was {
                out.push(Action::AnchorNote(r, path.clone(), n.clone()));
            }
        }
        for (i, (f, l)) in self.lines.iter().zip(&s.lines).enumerate() {
            if *f != LineForm::of(l) {
                out.push(Action::SetLine(r, i, f.clone()));
            }
        }
        out
    }
}

impl App {
    pub(crate) fn fill_form(&mut self) {
        let new = std::mem::take(&mut self.form.new);
        let adding = self.form.adding;
        self.form = match self.open() {
            Some((r, s)) => {
                let mut f = Form::of(r, &s, self.project.config.duration);
                if self.form.shot == Some(r) {
                    f.new = new;
                    f.adding = adding;
                }
                f
            }
            None => Form::default(),
        };
        self.texts = Texts {
            story: self.film.story.clone(),
            style: self.film.style.clone(),
            format: self.project.format(),
        };
        self.timeline_form = self.film.timeline.clone();
        self.element_form = self
            .open_element()
            .map(|el| ElementForm::of(&el))
            .unwrap_or_default();
    }

    /// Writes whatever was typed and not saved yet.
    pub(crate) fn flush(&mut self) {
        let mut acts = vec![];
        if let Some((r, s)) = self.open()
            && self.form.shot == Some(r)
        {
            acts = self.form.changes(r, &s, self.project.config.duration);
        }
        if self.texts.story != self.film.story {
            acts.push(Action::Story(self.texts.story.clone()));
        }
        if self.texts.style != self.film.style {
            acts.push(Action::Style(self.texts.style.clone()));
        }
        if self.texts.format != self.project.format() {
            acts.push(Action::Format(self.texts.format.clone()));
        }
        if self.timeline_form != self.film.timeline {
            acts.push(Action::Timeline(self.timeline_form.clone()));
        }
        if let Some(el) = self.open_element()
            && self.element_form.handle.as_ref() == Some(&el.handle)
            && self.element_form != ElementForm::of(&el)
        {
            match self.element_form.element(&el) {
                Ok(new) => acts.push(Action::SaveElement(new)),
                Err(e) => {
                    self.error(e.to_string());
                    self.element_form = ElementForm::of(&el);
                }
            }
        }
        self.apply(acts);
    }

    /// Writes the actions as one step of the undo history.
    pub(crate) fn apply(&mut self, acts: Vec<Action>) {
        let mut step = Step {
            names: vec![],
            changes: vec![],
        };
        for a in acts {
            let name = a.name();
            let files: Vec<(PathBuf, State)> = a
                .files(&self.project)
                .into_iter()
                .map(|f| {
                    let s = State::of(&f);
                    (f, s)
                })
                .collect();
            let added = matches!(a, Action::AddLine(..) | Action::ImportLine(..));
            match self.write(a) {
                Ok(done) => {
                    for (f, before) in files {
                        step.record(f, before);
                    }
                    let mut made = vec![];
                    for m in &done.made {
                        tree(m, &mut made);
                    }
                    for m in made {
                        step.record(m, State::Gone);
                    }
                    if !step.names.contains(&name) {
                        step.names.push(name);
                    }
                    self.info(done.msg);
                    if added {
                        self.form.new.text.clear();
                        self.form.new.at.clear();
                        self.form.adding = false;
                    }
                    self.reload();
                }
                Err(e) => {
                    self.error(e.to_string());
                    // Back to what's on disk, or a bad value would be retried every frame.
                    let new = self.form.new.clone();
                    self.fill_form();
                    self.form.new = new;
                }
            }
        }
        step.changes.retain(|c| c.before != c.after);
        if !step.changes.is_empty() {
            self.history.undo.push(step);
            self.history.redo.clear();
        }
    }

    pub(crate) fn undo(&mut self) {
        self.travel(true);
    }

    pub(crate) fn redo(&mut self) {
        self.travel(false);
    }

    fn travel(&mut self, back: bool) {
        self.flush();
        let from = if back {
            &mut self.history.undo
        } else {
            &mut self.history.redo
        };
        let Some(step) = from.pop() else { return };
        match restore(&step, back) {
            Ok(()) => {
                let done = if back { "undid" } else { "redid" };
                self.info(format!("{done} {}", step.what()));
                let to = if back {
                    &mut self.history.redo
                } else {
                    &mut self.history.undo
                };
                to.push(step);
            }
            // The step is dropped: the files moved on without it.
            Err(e) => self.error(e.to_string()),
        }
        if self.shot.is_some_and(|r| self.project.shot(r).is_err()) {
            self.shot = None;
        }
        self.reload();
    }

    fn write(&mut self, a: Action) -> anyhow::Result<Done> {
        let p = &self.project;
        Ok(match a {
            Action::Set(r, f) => {
                let mut s = p.shot(r)?;
                match f {
                    Field::Title(t) => s.title = t.trim().to_string(),
                    Field::Plot(t) => s.plot = t,
                    Field::Prompt(t) => s.prompt = t,
                    Field::FramePrompt(t) => s.frame_prompt = (!t.trim().is_empty()).then_some(t),
                    Field::Duration(d) => {
                        if d.is_nan() || d <= 0.0 {
                            anyhow::bail!("a shot is longer than 0 seconds");
                        }
                        s.duration = Some(d);
                    }
                    Field::Transition(t) => s.transition = t,
                    Field::ContinueFrom(t) => s.continue_from = t,
                    Field::Recipe(rc) => s.recipe = rc,
                    Field::Model(m) => s.set_video_model(m),
                    Field::Resolution(r) => s.set_resolution(r),
                }
                p.save_shot(r, &s)?;
                format!("{r} saved").into()
            }
            Action::AddLine(r, l) => {
                let s = p.add_line(r, &l.who, &l.text, l.at()?)?;
                format!("{r} has {} lines", s.lines.len()).into()
            }
            Action::ImportLine(r, l, file) => {
                let at = l.at()?;
                let job = p.import_recording(r, &l.who, &l.text, &file)?;
                let s = match p.add_line(r, &l.who, &l.text, at) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = std::fs::remove_dir_all(p.job_dir(job.id));
                        return Err(e.into());
                    }
                };
                p.set_line_audio(r, s.lines.len() - 1, Some(&job))?;
                Done {
                    msg: format!(
                        "{r} has {} lines, the last sending {}",
                        s.lines.len(),
                        file.file_name().unwrap_or_default().to_string_lossy()
                    ),
                    made: vec![p.job_dir(job.id)],
                }
            }
            Action::AddSound(u, file, at) => {
                let job = p.add_sound(u, &file, at)?;
                Done {
                    msg: format!(
                        "{} over {}",
                        file.file_name().unwrap_or_default().to_string_lossy(),
                        under_name(u)
                    ),
                    made: vec![p.job_dir(job.id)],
                }
            }
            Action::AddSoundJob(u, job, at) => {
                p.add_sound_job(u, &job, at)?;
                format!("a sound over {}", under_name(u)).into()
            }
            Action::RemoveSound(u, id) => {
                p.remove_sound(u, &id)?;
                format!("a sound off {}", under_name(u)).into()
            }
            Action::SetLine(r, i, l) => {
                p.set_line(r, i, &l.who, &l.text, l.at()?)?;
                format!("{r} saved").into()
            }
            Action::PickRecording(r, i, id) => {
                let job = match id {
                    Some(id) => Some(p.job(&id)?),
                    None => None,
                };
                p.set_line_audio(r, i, job.as_ref())?;
                match job {
                    Some(_) => format!("line {} of {r} sends the recording picked", i + 1),
                    None => format!("line {} of {r} is recorded with each take", i + 1),
                }
                .into()
            }
            Action::DeleteRecording(r, id) => {
                let j = p.job(&id)?;
                p.discard_recording(&id)?;
                let mut s = p.shot(r)?;
                for l in &mut s.lines {
                    if l.audio.as_deref() == Some(j.id.to_string().as_str()) {
                        l.audio = None;
                    }
                }
                p.save_shot(r, &s)?;
                format!("recording \"{}\" deleted", j.prompt.unwrap_or_default()).into()
            }
            Action::MoveLine(r, from, to) => {
                p.move_line(r, from, to)?;
                format!("line {} of {r} moved to {}", from + 1, to + 1).into()
            }
            Action::RemoveLine(r, i) => {
                p.remove_line(r, i)?;
                format!("line {} of {r} removed", i + 1).into()
            }
            Action::Select(t) => {
                let t = p.select_take(t.shot, &t.number.to_string())?;
                // Kept for Browse existing, and for the shot after if it continues from this one.
                let video = t
                    .output()
                    .and_then(|j| j.outputs.first().map(|f| p.job_dir(j.id).join(f)));
                let last = p.last_frame(&t).filter(|f| !f.is_file());
                let mut said = format!("{t} selected");
                if let Some(next) = p.next_shot(t.shot)?
                    && p.shot(next)?.transition == Transition::Continue
                    && let Some(from) = p.continued_take(next)?
                    && from.id != t.id
                {
                    said += &format!("; {next} still starts on {from}'s last frame");
                }
                if let (Some(ff), Some(v), Some(png)) = (&self.ff, video, last)
                    && let Err(e) = ff.last_frame(&v, &png)
                {
                    said += &format!("; can't take its last frame: {e}");
                }
                said.into()
            }
            Action::RemoveTake(t) => {
                let t = p.remove_take(t.shot, &t.number.to_string())?;
                format!("{t} removed").into()
            }
            Action::Story(t) => {
                std::fs::write(p.root.join("story.md"), t)?;
                "story saved".to_string().into()
            }
            Action::Style(t) => {
                std::fs::write(p.root.join("style.md"), t)?;
                "style saved".to_string().into()
            }
            Action::Format(f) => {
                Project::open(&p.root)?.set_format(&f)?;
                format!("shots are now made at {} {}", f.aspect, f.resolution).into()
            }
            Action::Param {
                plugin,
                model,
                key,
                value,
            } => {
                let said = value.as_ref().map(|v| v.to_string());
                Project::open(&p.root)?.set_param(&plugin, &model, &key, value)?;
                match said {
                    Some(v) => format!("{model}: {key} = {v}"),
                    None => format!("{model}: {key} back to the default"),
                }
                .into()
            }
            Action::Model(task, to) => {
                Project::open(&p.root)?.set_model(task, to.as_ref())?;
                match to {
                    Some(m) => format!("{} with {} · {}", task_name(task), m.plugin, m.model),
                    None => format!("no {} model of its own", task_name(task)),
                }
                .into()
            }
            Action::Budget(b) => {
                Project::open(&p.root)?.set_budget(&b)?;
                String::from("the budget is saved").into()
            }
            Action::RenameChapter(n, t) => {
                let c = p.rename_chapter(&n.to_string(), &t)?;
                format!("chapter {} is now {}", c.id(), c.title).into()
            }
            Action::AddChapter(t) => {
                let c = p.add_chapter(t.trim())?;
                Done {
                    msg: format!("added chapter {}", c.id()),
                    made: vec![c.dir],
                }
            }
            Action::AddShot(chapter, after) => {
                let r = p.add_shot(&chapter.to_string(), after, Shot::default())?;
                let made = vec![p.shot_path(r)?];
                self.shot = Some(r);
                Done {
                    msg: format!("added {r}"),
                    made,
                }
            }
            Action::SaveElement(el) => {
                p.save_element(&el)?;
                format!("@{} saved", el.handle).into()
            }
            Action::AddElement(n) => {
                let handle = Some(n.handle.trim()).filter(|h| !h.is_empty());
                let el = p.add_element(n.kind, n.name.trim(), handle, "")?;
                self.element = Some(el.handle.clone());
                Done {
                    msg: format!("added @{}", el.handle),
                    made: vec![p.element_file(el.kind, &el.handle)],
                }
            }
            Action::AddAnchor(el, file) => {
                let dir = p.anchor_dir(&el);
                let new_dir = !dir.exists();
                let dest = p.add_anchor(&el.handle, &file)?;
                Done {
                    msg: format!("@{} has {}", el.handle, p.rel(&dest)),
                    made: vec![if new_dir { dir } else { dest }],
                }
            }
            Action::RemoveAnchor(el, file) => {
                p.remove_anchor(&el.handle, &file)?;
                format!("{} taken off @{}", p.rel(&file), el.handle).into()
            }
            Action::AddShotImage(r, file) => {
                let dir = p.shot_image_dir(r)?;
                let new_dir = !dir.exists();
                let dest = p.add_shot_image(r, &file, "")?;
                Done {
                    msg: format!("{r} has {}", p.rel(&dest)),
                    made: vec![if new_dir { dir } else { dest }],
                }
            }
            Action::ForgetImage(id) => {
                let j = p.job(&id)?;
                p.set_discarded(j.id, true)?;
                "image thrown out; undo brings it back".to_string().into()
            }
            Action::RemoveShotImage(r, i) => {
                let img = p.remove_shot_image(r, i)?;
                format!("{} taken off {r}", img.file).into()
            }
            Action::AnchorNote(r, path, note) => {
                let mut s = p.shot(r)?;
                match note.trim() {
                    "" => s.anchor_notes.remove(&path),
                    n => s.anchor_notes.insert(path.clone(), n.to_string()),
                };
                p.save_shot(r, &s)?;
                format!("note on {path} saved").into()
            }
            Action::ShotImageNote(r, i, note) => {
                p.set_shot_image_note(r, i, &note)?;
                format!("{r} saved").into()
            }
            Action::SetFrame(r, file) => match p.set_frame(r, file.as_deref())? {
                Some(dest) => Done {
                    msg: format!("{r} starts on {}", p.rel(&dest)),
                    made: vec![dest],
                },
                None => format!("{r} has no storyboard frame now").into(),
            },
            Action::LeaveOut(r, anchor, out) => {
                let mut s = p.shot(r)?;
                s.leave_out.retain(|a| *a != anchor);
                if out {
                    s.leave_out.push(anchor.clone());
                }
                // An element only attached for its pictures goes once none are left.
                let named = shotline_core::handle::unique(&s.prompt);
                for h in s.elements.clone() {
                    if named.contains(&h) {
                        continue;
                    }
                    let all: Vec<_> = p
                        .anchors(&p.element(&h)?)?
                        .iter()
                        .map(|a| p.rel(a))
                        .collect();
                    if all.iter().all(|a| s.leave_out.contains(a)) {
                        s.elements.retain(|e| *e != h);
                        s.leave_out.retain(|a| !all.contains(a));
                    }
                }
                p.save_shot(r, &s)?;
                let what = if out { "left out of" } else { "put back in" };
                format!("{anchor} {what} {r}").into()
            }
            Action::SendAnchor(r, handle, anchor) => {
                let mut s = p.shot(r)?;
                let named = shotline_core::handle::unique(&s.prompt).contains(&handle)
                    || s.elements.contains(&handle);
                if !named {
                    s.elements.push(handle.clone());
                    for a in p.anchors(&p.element(&handle)?)? {
                        let a = p.rel(&a);
                        if a != anchor && !s.leave_out.contains(&a) {
                            s.leave_out.push(a);
                        }
                    }
                }
                s.leave_out.retain(|a| *a != anchor);
                p.save_shot(r, &s)?;
                format!("{anchor} sent with {r}").into()
            }
            Action::Timeline(t) => {
                p.save_timeline(&t)?;
                "cut saved".to_string().into()
            }
            Action::AddTrack(file) => {
                let mut made = vec![];
                let rel = if file.starts_with(&p.root) {
                    p.rel(&file)
                } else {
                    let dir = p.root.join("music");
                    if !dir.exists() {
                        std::fs::create_dir_all(&dir)?;
                        made.push(dir.clone());
                    }
                    let name = file.file_name().context("not a file")?;
                    let mut dest = dir.join(name);
                    let stem = Path::new(name)
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy();
                    let ext = Path::new(name)
                        .extension()
                        .unwrap_or_default()
                        .to_string_lossy();
                    let mut n = 2;
                    while dest.exists() {
                        dest = dir.join(format!("{stem}-{n}.{ext}"));
                        n += 1;
                    }
                    std::fs::copy(&file, &dest)?;
                    if made.is_empty() {
                        made.push(dest.clone());
                    }
                    p.rel(&dest)
                };
                let mut t = p.timeline()?;
                t.tracks.push(Track {
                    file: rel.clone(),
                    at: 0.0,
                    volume: 1.0,
                });
                p.save_timeline(&t)?;
                Done {
                    msg: format!("{rel} plays over the cut"),
                    made,
                }
            }
        })
    }
}

fn restore(step: &Step, back: bool) -> anyhow::Result<()> {
    // Something else, like the co-director over MCP, may have changed a file since; don't write
    // over that.
    for c in &step.changes {
        let (now, then) = c.ends(back);
        let moved = State::of(&c.path) != *now
            // A folder only goes if nothing else was put in it.
            || (*then == State::Gone
                && *now == State::Dir
                && std::fs::read_dir(&c.path)?
                    .flatten()
                    .any(|e| !step.changes.iter().any(|c| c.path == e.path())));
        if moved {
            anyhow::bail!(
                "{} was changed outside the app since",
                step.names.join(" and ")
            );
        }
    }
    let mut changes: Vec<&Change> = step.changes.iter().collect();
    if back {
        changes.reverse();
    }
    for c in changes {
        c.ends(back).1.put(&c.path)?;
    }
    Ok(())
}

/// The shot's own video model, when it has both halves of one.
fn video_model(s: &Shot) -> Option<(String, String)> {
    let m = s.model.as_ref()?;
    Some((m.plugin.clone()?, m.id.clone()?))
}

/// What a job is called in messages and on the Default Models page.
pub(crate) fn task_name(t: Task) -> &'static str {
    match t {
        Task::Translate => "translating",
        Task::Draft => "drafting",
        Task::Voice => "voice",
        Task::Image => "image",
        Task::Video => "video",
        Task::LipSync => "lip sync",
        Task::Assistant => "assistant",
    }
}

fn under_name(u: Under) -> String {
    u.shot().map_or("a bridge".into(), |r| r.to_string())
}

/// The shot or bridge file that lists a take's sounds.
fn sounds_file(p: &Project, u: Under) -> Vec<PathBuf> {
    match u {
        Under::Take(r, _) => p.shot_path(r).into_iter().collect(),
        Under::Bridge(b, _) => vec![p.bridge_file(b)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_starts() {
        let at = |s: &str| {
            LineForm {
                at: s.into(),
                ..LineForm::default()
            }
            .at()
        };
        assert_eq!(at("").unwrap(), None);
        assert_eq!(at(" next ").unwrap(), None);
        assert_eq!(at("2.5").unwrap(), Some(2.5));
        assert_eq!(at("3 s").unwrap(), Some(3.0));
        assert!(at("soon").is_err());
    }
}
