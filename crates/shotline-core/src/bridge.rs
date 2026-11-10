//! A bridge: a short clip made to join two clips of an editor's sequence, from the frame where
//! the first one leaves off to the one where the next picks up. It belongs to the join, not to
//! a shot, so its takes are jobs tied to it rather than takes of a shot.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::invalid;
use crate::{Job, JobStatus, Project, Result, Translated, fsx};

/// A place in a video file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    /// Relative to the project root when it's inside it.
    pub file: String,
    /// Seconds into the file.
    pub at: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Bridge {
    /// The editor sequence it's in, by name.
    pub sequence: String,
    /// The last frame shown before the join.
    pub from: Point,
    /// The first frame shown after it.
    pub to: Point,
    /// The frames taken at those points, which takes start and end on.
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub english: Option<Translated>,
    /// Seconds.
    pub length: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub plugin: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// The take put in the cut last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<Ulid>,
}

impl Bridge {
    /// The prompt in English, when it's been put into English for this text.
    pub fn translated(&self) -> Option<&str> {
        self.english
            .as_ref()
            .filter(|t| t.from == self.prompt)
            .map(|t| t.text.as_str())
    }
}

impl Project {
    pub fn bridge_file(&self, id: Ulid) -> PathBuf {
        self.root.join("bridges").join(format!("{id}.toml"))
    }

    /// Where its frames are kept.
    pub fn bridge_dir(&self, id: Ulid) -> PathBuf {
        self.root.join("bridges").join(id.to_string())
    }

    pub fn bridges(&self) -> Result<Vec<(Ulid, Bridge)>> {
        let mut out = vec![];
        for path in fsx::list(&self.root.join("bridges"))? {
            if path.extension().is_none_or(|e| e != "toml") {
                continue;
            }
            let Some(id) = path
                .file_stem()
                .and_then(|n| Ulid::from_string(&n.to_string_lossy()).ok())
            else {
                continue;
            };
            out.push((id, fsx::read_toml(&path)?));
        }
        out.sort_by_key(|(id, _)| *id);
        Ok(out)
    }

    pub fn bridge(&self, id: Ulid) -> Result<Bridge> {
        let path = self.bridge_file(id);
        if !path.exists() {
            return invalid(format!("no bridge {id}"));
        }
        fsx::read_toml(&path)
    }

    pub fn save_bridge(&self, id: Ulid, b: &Bridge) -> Result<()> {
        fsx::write_toml(&self.bridge_file(id), b)
    }

    pub fn new_bridge(&self, b: &Bridge) -> Result<Ulid> {
        let id = self.clock.ulid();
        self.save_bridge(id, b)?;
        Ok(id)
    }

    /// Takes a bridge away with its frames, if it has no takes.
    pub fn remove_bridge(&self, id: Ulid) -> Result<()> {
        if self.jobs()?.iter().any(|j| j.bridge == Some(id)) {
            return invalid(format!("bridge {id} has takes, so it stays"));
        }
        fsx::remove(&self.bridge_file(id))?;
        let dir = self.bridge_dir(id);
        if dir.exists() {
            fsx::remove_dir(&dir)?;
        }
        Ok(())
    }

    /// Its takes, oldest first, removed ones left out. Numbered by position from 1, counting
    /// removed ones, so the numbers don't move.
    pub fn bridge_takes(&self, id: Ulid) -> Result<Vec<(usize, Job)>> {
        Ok(self
            .jobs()?
            .into_iter()
            .filter(|j| j.bridge == Some(id))
            .enumerate()
            .map(|(i, j)| (i + 1, j))
            .filter(|(_, j)| !j.discarded)
            .collect())
    }

    /// Takes a finished take off the bridge's list; what it cost stays in the ledger.
    pub fn remove_bridge_take(&self, id: Ulid, take: Ulid) -> Result<()> {
        let job = self.load_job(take)?;
        if job.bridge != Some(id) {
            return invalid(format!("{take} isn't a take of bridge {id}"));
        }
        if !job.status.is_finished() {
            return invalid(format!("it's still {}", job.status));
        }
        self.set_discarded(take, true)?;
        let mut b = self.bridge(id)?;
        if b.used == Some(take) {
            b.used = None;
            self.save_bridge(id, &b)?;
        }
        Ok(())
    }

    /// A finished bridge take's video.
    pub fn bridge_video(&self, job: &Job) -> Option<PathBuf> {
        let f = job
            .outputs
            .first()
            .filter(|_| job.status == JobStatus::Done)?;
        Some(self.job_dir(job.id).join(f))
    }

    /// The bridge take a file is the output of, by the `jobs/<id>/` folder it's in.
    pub fn bridge_of_file(&self, file: &Path) -> Option<Job> {
        let dir = file.parent()?;
        if dir.parent()?.file_name()? != "jobs" {
            return None;
        }
        let id = Ulid::from_string(dir.file_name()?.to_str()?).ok()?;
        self.load_job(id).ok().filter(|j| j.bridge.is_some())
    }

    /// A point's file as a path.
    pub fn point_file(&self, p: &Point) -> PathBuf {
        let f = Path::new(&p.file);
        if f.is_absolute() {
            f.to_path_buf()
        } else {
            self.root.join(f)
        }
    }
}
