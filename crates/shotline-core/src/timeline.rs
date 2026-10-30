use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{Error, Project, Result, ShotRef, Take, Transition, fsx};

pub const TIMELINE_FILE: &str = "timeline.toml";

/// `timeline.toml`: how the film is put together. Everything is optional; without the file it's
/// every shot in order, colour matched where it continues.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Timeline {
    /// Shot ids in the order they play; empty means every shot in chapter order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shots: Vec<String>,
    /// Crossfade at each join, in seconds; 0 cuts.
    pub xfade: f64,
    pub colormatch: ColourMatch,
    #[serde(rename = "track", skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColourMatch {
    /// Which joins get matched: `continue` (default), `all` or `none`.
    pub at: MatchAt,
    /// Frames each side of a join used for the colour stats.
    pub frames: usize,
    /// 0..1, how much of the correction to apply.
    pub strength: f64,
}

impl Default for ColourMatch {
    fn default() -> Self {
        Self {
            at: MatchAt::Continue,
            frames: 5,
            strength: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchAt {
    /// Only shots that continue from the one before; a cut is usually a new look on purpose.
    #[default]
    Continue,
    All,
    None,
}

/// Music or effects over the film.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    /// Relative to the project root.
    pub file: String,
    /// Seconds into the film where it starts.
    #[serde(default)]
    pub at: f64,
    #[serde(default = "one")]
    pub volume: f64,
}

fn one() -> f64 {
    1.0
}

/// One shot of the cut, with the take that plays.
#[derive(Debug, Clone)]
pub struct CutShot {
    pub shot: ShotRef,
    pub transition: Transition,
    /// `None` when nothing is selected yet.
    pub take: Option<Take>,
    /// The file that plays.
    pub video: Option<PathBuf>,
}

impl CutShot {
    pub fn colormatch(&self, at: MatchAt, first: bool) -> bool {
        !first
            && match at {
                MatchAt::All => true,
                MatchAt::Continue => self.transition == Transition::Continue,
                MatchAt::None => false,
            }
    }
}

impl Project {
    pub fn timeline(&self) -> Result<Timeline> {
        let path = self.root.join(TIMELINE_FILE);
        if !path.exists() {
            return Ok(Timeline::default());
        }
        let t: Timeline = fsx::read_toml(&path)?;
        if t.xfade < 0.0 || !(0.0..=1.0).contains(&t.colormatch.strength) {
            return Err(Error::Invalid(format!(
                "{TIMELINE_FILE}: xfade can't be negative and colormatch.strength goes from 0 to 1"
            )));
        }
        Ok(t)
    }

    /// The shots of the film in play order with their selected takes.
    pub fn cut(&self, timeline: &Timeline) -> Result<Vec<CutShot>> {
        let refs: Vec<ShotRef> = if timeline.shots.is_empty() {
            self.shots(None)?.into_iter().map(|(r, _)| r).collect()
        } else {
            timeline
                .shots
                .iter()
                .map(|s| s.parse())
                .collect::<Result<_>>()?
        };
        let mut out = vec![];
        for r in refs {
            let shot = self.shot(r)?;
            let take = self.selected_take(r)?;
            let video = take
                .as_ref()
                .and_then(|t| t.output())
                .and_then(|j| j.outputs.first().map(|f| self.job_dir(j.id).join(f)));
            out.push(CutShot {
                shot: r,
                transition: shot.transition,
                take,
                video,
            });
        }
        Ok(out)
    }
}
