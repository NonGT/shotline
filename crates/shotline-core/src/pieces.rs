//! Takes made here rather than by a model: put together from pieces of other takes, or brought in
//! from a file cut somewhere else. Both are a single finished job that costs nothing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use shotline_protocol::Op;
use ulid::Ulid;

use crate::error::invalid;
use crate::{Error, Job, JobStatus, Project, Result, ShotRef, Take, fsx};

pub const SPLICE: &str = "x.splice";
pub const IMPORT: &str = "x.import";

/// How a piece meets the one before it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Join {
    #[default]
    Cut,
    Fade,
    Smooth,
}

impl Join {
    pub fn name(self) -> &'static str {
        match self {
            Join::Cut => "cut",
            Join::Fade => "fade",
            Join::Smooth => "smooth",
        }
    }
}

/// Part of a take, kept in the made take's job so it can be opened again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Piece {
    /// The take's id.
    pub take: Ulid,
    pub from: f64,
    pub to: f64,
    #[serde(default)]
    pub join: Join,
}

impl Piece {
    /// `2:1.5-4`, optionally after `fade:` or `smooth:`, with the take as in `shotline select`.
    pub fn parse(p: &Project, shot: ShotRef, spec: &str) -> Result<Piece> {
        let bad = || Error::Invalid(format!("{spec:?}: write a piece as [fade:|smooth:]2:1.5-4"));
        let (join, rest) = match spec.split_once(':') {
            Some(("fade", r)) => (Join::Fade, r),
            Some(("smooth", r)) => (Join::Smooth, r),
            Some(("cut", r)) => (Join::Cut, r),
            _ => (Join::Cut, spec),
        };
        let (take, range) = rest.rsplit_once(':').ok_or_else(bad)?;
        let (from, to) = range.split_once('-').ok_or_else(bad)?;
        let num = |s: &str| s.trim().parse::<f64>().map_err(|_| bad());
        Ok(Piece {
            take: p.take(shot, take)?.id,
            from: num(from)?,
            to: num(to)?,
            join,
        })
    }
}

/// What a take was made from, for one made here.
pub fn pieces_of(t: &Take) -> Option<Vec<Piece>> {
    let j = t.jobs.last()?;
    if j.op != Op::Custom(SPLICE.into()) {
        return None;
    }
    j.params
        .get("pieces")
        .and_then(|v| v.clone().try_into::<Pieces>().ok())
        .map(|p| p.0)
}

/// For a take made here: `from pieces of #1, #2` or `brought in from cut.mov`. `takes` names the
/// pieces' takes; one from another shot shows as `01-020#3`.
pub fn made_from(t: &Take, takes: &[Take]) -> Option<String> {
    let j = t.jobs.last()?;
    if j.op == Op::Custom(IMPORT.into()) {
        return Some(format!(
            "brought in from {}",
            j.brief.as_deref().unwrap_or("a file")
        ));
    }
    let mut names: Vec<String> = vec![];
    for p in pieces_of(t)? {
        let name = match takes.iter().find(|o| o.id == p.take) {
            Some(o) if o.shot == t.shot => format!("#{}", o.number),
            Some(o) => o.to_string(),
            None => "a removed take".into(),
        };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    Some(format!("from pieces of {}", names.join(", ")))
}

#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct Pieces(Vec<Piece>);

impl Project {
    /// A finished take's video, found by take id among all the film's shots.
    pub fn take_video(&self, take: Ulid) -> Result<PathBuf> {
        let jobs = self.jobs()?;
        let job = jobs
            .iter()
            .filter(|j| j.take == Some(take))
            .max_by_key(|j| (j.step, j.id))
            .ok_or_else(|| Error::Invalid(format!("no take {take}")))?;
        match (job.status, job.outputs.first()) {
            (JobStatus::Done, Some(f)) => Ok(self.job_dir(job.id).join(f)),
            _ => invalid(format!("take {take} has no video")),
        }
    }

    /// A new take of `shot` put together from `pieces`. `join` gets each piece's video and writes
    /// the result to the path it's given.
    pub fn splice_take(
        &self,
        shot: ShotRef,
        pieces: &[Piece],
        join: impl FnOnce(&[(PathBuf, Piece)], &Path) -> std::result::Result<(), String>,
    ) -> Result<Take> {
        if pieces.is_empty() {
            return invalid("no pieces");
        }
        let sources = pieces
            .iter()
            .map(|p| Ok((self.take_video(p.take)?, p.clone())))
            .collect::<Result<Vec<_>>>()?;
        let mut job = Job::new(Op::Custom(SPLICE.into()), "shotline", "pieces");
        job.params.insert(
            "pieces".into(),
            toml::Value::try_from(Pieces(pieces.to_vec()))
                .map_err(|e| Error::Invalid(e.to_string()))?,
        );
        self.made_take(shot, job, "take.mp4", |out| join(&sources, out))
    }

    /// A new take of `shot` from a video made somewhere else. The file is copied in.
    pub fn import_take(&self, shot: ShotRef, file: &Path) -> Result<Take> {
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .ok_or_else(|| Error::Invalid(format!("{} isn't a file", file.display())))?;
        if !file.is_file() {
            return invalid(format!("{} isn't a file", file.display()));
        }
        let ext = file
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_else(|| "mp4".into());
        let mut job = Job::new(Op::Custom(IMPORT.into()), "shotline", "import");
        job.brief = Some(name);
        self.made_take(shot, job, &format!("take.{ext}"), |out| {
            fsx::copy(file, out).map_err(|e| e.to_string())
        })
    }

    fn made_take(
        &self,
        shot: ShotRef,
        mut job: Job,
        name: &str,
        make: impl FnOnce(&Path) -> std::result::Result<(), String>,
    ) -> Result<Take> {
        self.shot(shot)?;
        job.shot = Some(shot);
        job.take = Some(self.new_take_id());
        let mut job = self.create_job(job)?;
        job.status = JobStatus::Running;
        self.save_job(&job)?;
        match make(&self.job_dir(job.id).join(name)) {
            Ok(()) => {
                job.outputs = vec![name.into()];
                job.status = JobStatus::Done;
                self.save_job(&job)?;
            }
            Err(e) => {
                job.status = JobStatus::Failed;
                job.error = Some(e.clone());
                self.save_job(&job)?;
                return invalid(e);
            }
        }
        let id = job.take.expect("set above");
        self.takes(shot)?
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| Error::Invalid(format!("take {id} went missing")))
    }
}
