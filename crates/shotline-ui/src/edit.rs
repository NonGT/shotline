//! Changing the project from the app. Widgets only edit `Form`; once nothing has focus the form is
//! compared with the shot as read from disk, and each difference becomes an `Action`. `apply` is
//! the one place that writes, through shotline-core like the CLI, and it keeps each shot as it was
//! before and after so the change can be undone.

use shotline_core::ledger::money;
use shotline_core::{Shot, ShotRef, Take, Transition};

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
}

pub(crate) enum Action {
    Set(ShotRef, Field),
    AddLine(ShotRef, LineForm),
    SetLine(ShotRef, usize, LineForm),
    RemoveLine(ShotRef, usize),
    Select(Take),
}

impl Action {
    fn shot(&self) -> ShotRef {
        match self {
            Action::Set(r, _)
            | Action::AddLine(r, _)
            | Action::SetLine(r, ..)
            | Action::RemoveLine(r, _) => *r,
            Action::Select(t) => t.shot,
        }
    }
}

struct Change {
    shot: ShotRef,
    before: Shot,
    after: Shot,
}

impl Change {
    /// The shot as it is now and as it will be once this change is undone, or redone.
    fn ends(&self, back: bool) -> (&Shot, &Shot) {
        if back {
            (&self.after, &self.before)
        } else {
            (&self.before, &self.after)
        }
    }
}

/// What one save did, as a whole: undo takes back every shot it touched.
struct Step {
    changes: Vec<Change>,
}

impl Step {
    fn what(&self) -> String {
        let shots: Vec<String> = self.changes.iter().map(|c| c.shot.to_string()).collect();
        format!("the change to {}", shots.join(" and "))
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
    }

    /// Writes whatever was typed into the open shot and not saved yet.
    pub(crate) fn flush(&mut self) {
        let Some((r, s)) = self.open() else { return };
        if self.form.shot != Some(r) {
            return;
        }
        let acts = self.form.changes(r, &s, self.project.config.duration);
        self.apply(acts);
    }

    /// Writes the actions as one step of the undo history.
    pub(crate) fn apply(&mut self, acts: Vec<Action>) {
        let mut step = Step { changes: vec![] };
        for a in acts {
            let r = a.shot();
            let before = self.project.shot(r);
            let added = matches!(a, Action::AddLine(..));
            match self.write(a) {
                Ok(msg) => {
                    if let (Ok(before), Ok(after)) = (before, self.project.shot(r)) {
                        match step.changes.iter_mut().find(|c| c.shot == r) {
                            Some(c) => c.after = after,
                            None => step.changes.push(Change {
                                shot: r,
                                before,
                                after,
                            }),
                        }
                    }
                    self.info(msg);
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
        match self.restore(&step, back) {
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
        self.reload();
    }

    fn restore(&self, step: &Step, back: bool) -> anyhow::Result<()> {
        // Something else, like the co-director over MCP, may have changed the shot since; don't
        // write over that.
        for c in &step.changes {
            if self.project.shot(c.shot)? != *c.ends(back).0 {
                anyhow::bail!("{} was changed outside the app since", c.shot);
            }
        }
        for c in &step.changes {
            self.project.save_shot(c.shot, c.ends(back).1)?;
        }
        Ok(())
    }

    fn write(&mut self, a: Action) -> anyhow::Result<String> {
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
                }
                p.save_shot(r, &s)?;
                format!("{r} saved")
            }
            Action::AddLine(r, l) => {
                let s = p.add_line(r, &l.who, &l.text, l.at()?)?;
                format!("{r} has {} lines", s.lines.len())
            }
            Action::SetLine(r, i, l) => {
                p.set_line(r, i, &l.who, &l.text, l.at()?)?;
                format!("{r} saved")
            }
            Action::RemoveLine(r, i) => {
                p.remove_line(r, i)?;
                format!("line {} of {r} removed", i + 1)
            }
            Action::Select(t) => {
                let t = p.select_take(t.shot, &t.number.to_string())?;
                format!("{t} selected")
            }
        })
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
