//! Changing the project from the app. Widgets only edit `Form`; once nothing has focus the form is
//! compared with the shot as read from disk, and each difference becomes an `Action`. `apply` is
//! the one place that writes, through shotline-core like the CLI, and it keeps each shot as it was
//! before and after so the change can be undone.

use shotline_core::ledger::money;
use std::path::{Path, PathBuf};

use shotline_core::{Project, Shot, ShotRef, Take, Transition};

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
    pub recipe: Option<String>,
    pub lines: Vec<LineForm>,
    /// The line being added.
    pub new: LineForm,
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

pub(crate) enum Field {
    Title(String),
    Plot(String),
    Prompt(String),
    FramePrompt(String),
    Duration(f64),
    Transition(Transition),
    Recipe(Option<String>),
}

pub(crate) enum Action {
    Set(ShotRef, Field),
    AddLine(ShotRef, LineForm),
    SetLine(ShotRef, usize, LineForm),
    RemoveLine(ShotRef, usize),
    Select(Take),
    Story(String),
    Style(String),
    AddChapter(String),
    /// A new shot at the end of the chapter, or after the given number.
    AddShot(u32, Option<u32>),
}

impl Action {
    /// What it's about, as the undo button names it.
    fn name(&self) -> String {
        match self {
            Action::Set(r, _)
            | Action::AddLine(r, _)
            | Action::SetLine(r, ..)
            | Action::RemoveLine(r, _) => r.to_string(),
            Action::Select(t) => t.shot.to_string(),
            Action::Story(_) => "the story".into(),
            Action::Style(_) => "the style".into(),
            Action::AddChapter(t) => format!("chapter {t}"),
            Action::AddShot(..) => "a new shot".into(),
        }
    }

    /// Files it rewrites. Files it makes are told by `write` afterwards.
    fn files(&self, p: &Project) -> Vec<PathBuf> {
        match self {
            Action::Set(r, _)
            | Action::AddLine(r, _)
            | Action::SetLine(r, ..)
            | Action::RemoveLine(r, _) => p.shot_path(*r).into_iter().collect(),
            Action::Select(t) => p.shot_path(t.shot).into_iter().collect(),
            Action::Story(_) => vec![p.root.join("story.md")],
            Action::Style(_) => vec![p.root.join("style.md")],
            Action::AddChapter(_) | Action::AddShot(..) => vec![],
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
            recipe: s.recipe.clone(),
            lines: s.lines.iter().map(LineForm::of).collect(),
            new: LineForm::default(),
        }
    }

    /// What has to be written for the shot on disk to match the form.
    fn changes(&self, r: ShotRef, s: &Shot, default_duration: f64) -> Vec<Action> {
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
        set(self.recipe != s.recipe, Field::Recipe(self.recipe.clone()));
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
        self.form = match self.open() {
            Some((r, s)) => {
                let mut f = Form::of(r, &s, self.project.config.duration);
                if self.form.shot == Some(r) {
                    f.new = new;
                }
                f
            }
            None => Form::default(),
        };
        self.texts = Texts {
            story: self.film.story.clone(),
            style: self.film.style.clone(),
        };
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
            let added = matches!(a, Action::AddLine(..));
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
                    Field::Recipe(rc) => s.recipe = rc,
                }
                p.save_shot(r, &s)?;
                format!("{r} saved").into()
            }
            Action::AddLine(r, l) => {
                let s = p.add_line(r, &l.who, &l.text, l.at()?)?;
                format!("{r} has {} lines", s.lines.len()).into()
            }
            Action::SetLine(r, i, l) => {
                p.set_line(r, i, &l.who, &l.text, l.at()?)?;
                format!("{r} saved").into()
            }
            Action::RemoveLine(r, i) => {
                p.remove_line(r, i)?;
                format!("line {} of {r} removed", i + 1).into()
            }
            Action::Select(t) => {
                let t = p.select_take(t.shot, &t.number.to_string())?;
                format!("{t} selected").into()
            }
            Action::Story(t) => {
                std::fs::write(p.root.join("story.md"), t)?;
                "story saved".to_string().into()
            }
            Action::Style(t) => {
                std::fs::write(p.root.join("style.md"), t)?;
                "style saved".to_string().into()
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
