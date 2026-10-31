//! Changing the project from the app. Widgets only edit `Form`; once nothing has focus the form is
//! compared with the shot as read from disk, and each difference becomes an `Action`. `apply` is
//! the one place that writes, through shotline-core like the CLI.

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
        for a in self.form.changes(r, &s, self.project.config.duration) {
            self.apply(a);
        }
    }

    pub(crate) fn apply(&mut self, a: Action) {
        let added = matches!(a, Action::AddLine(..));
        match self.write(a) {
            Ok(msg) => {
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
