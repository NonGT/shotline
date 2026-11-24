//! Work going on in the app's own threads: what the Jobs pill shows besides made images, and
//! what closing the film would stop.

use shotline_core::{ShotRef, Ulid};

use crate::{App, Tab, cut, shoot, voice};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Go {
    Shot(ShotRef),
    Bridge(Ulid),
    Cut,
}

#[derive(Clone, Debug)]
pub(crate) struct Busy {
    /// "A take of 01-020".
    pub what: String,
    /// How far it got, e.g. "running (40%)".
    pub how: String,
    /// The price agreed, for paid work; closing loses track of it while it's held.
    pub held: Option<String>,
    /// Goes on in a process of its own after the app closes.
    pub outlives: bool,
    pub go: Go,
}

impl App {
    pub(crate) fn busy(&self) -> Vec<Busy> {
        let mut out = vec![];
        if let shoot::Shoot::Running(r, step) = &*self.shoot.lock().unwrap() {
            out.push(Busy {
                what: format!("A take of {r}"),
                how: step.clone(),
                held: Some(self.shoot_held.clone().unwrap_or_default()),
                outlives: self.take_apart,
                go: Go::Shot(*r),
            });
        }
        if let voice::Say::Recording(r, i) = &*self.say.lock().unwrap() {
            out.push(Busy {
                what: format!("Line {} of {r}", i + 1),
                how: "recording".into(),
                held: Some(self.say_held.clone().unwrap_or_default()),
                outlives: false,
                go: Go::Shot(*r),
            });
        }
        let mut bridges: Vec<_> = self.bridge_jobs.iter().collect();
        bridges.sort_by_key(|(id, _)| **id);
        for (id, j) in bridges {
            out.push(Busy {
                what: "A bridge take".into(),
                how: "making".into(),
                held: Some(j.price.clone()),
                outlives: false,
                go: Go::Bridge(*id),
            });
        }
        if let Some(w) = &self.sounds_win {
            for (what, cost) in w.making() {
                out.push(Busy {
                    what: format!("{what} for {}", w.name),
                    how: "making".into(),
                    held: Some(cost),
                    outlives: false,
                    go: Go::Cut,
                });
            }
        }
        if let Some(c) = &self.changing {
            out.push(Busy {
                what: crate::edit::first_up(&c.what),
                how: "in the editor".into(),
                held: c.held.clone(),
                outlives: false,
                go: Go::Cut,
            });
        }
        if let cut::Render::Running(step) = &*self.render.lock().unwrap() {
            out.push(Busy {
                what: "Rendering the cut".into(),
                how: step.clone(),
                held: None,
                outlives: false,
                go: Go::Cut,
            });
        }
        if self.edit_rendering.is_some() {
            out.push(Busy {
                what: "The editor's preview".into(),
                how: "rendering".into(),
                held: None,
                outlives: false,
                go: Go::Cut,
            });
        }
        out
    }

    /// Only the paid ones: what closing the film would leave held in the ledger.
    pub(crate) fn held(&self) -> Vec<Busy> {
        self.busy()
            .into_iter()
            .filter(|b| b.held.is_some() && !b.outlives)
            .collect()
    }

    pub(crate) fn go_to(&mut self, go: &Go) {
        match go {
            Go::Shot(r) => {
                self.tab = Tab::Shots;
                if self.shot != Some(*r) {
                    self.open_shot(*r);
                }
            }
            Go::Bridge(id) => {
                self.tab = Tab::Cut;
                self.open_bridge_of(*id);
            }
            Go::Cut => self.tab = Tab::Cut,
        }
    }
}

/// "A take of 01-020 ($0.40 held)", one per line.
pub(crate) fn said(held: &[Busy]) -> String {
    held.iter()
        .map(|b| match b.held.as_deref() {
            Some(p) if !p.is_empty() => format!("{} ({p} held)", b.what),
            _ => b.what.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
