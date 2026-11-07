use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use shotline_protocol::Op;
use ulid::Ulid;

use crate::error::invalid;
use crate::{Error, Project, Result, ShotRef, fsx};

/// Where time and ids come from, so tests can pin them.
pub trait Clock: fmt::Debug + Send + Sync {
    fn now(&self) -> jiff::Timestamp;
    fn ulid(&self) -> Ulid;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> jiff::Timestamp {
        jiff::Timestamp::now()
    }

    fn ulid(&self) -> Ulid {
        // Plain Ulid::generate() is random within a millisecond, which would let take numbers
        // (ordered by id) swap when two are made in the same ms.
        static GEN: std::sync::Mutex<ulid::Generator> =
            std::sync::Mutex::new(ulid::Generator::new());
        GEN.lock()
            .unwrap()
            .generate()
            .unwrap_or_else(|_| Ulid::generate())
    }
}

/// Starts at a fixed time and moves one second per call.
#[derive(Debug)]
pub struct TestClock {
    start_ms: i64,
    calls: AtomicU64,
}

impl TestClock {
    pub fn new(start: jiff::Timestamp) -> Self {
        Self {
            start_ms: start.as_millisecond(),
            calls: AtomicU64::new(0),
        }
    }

    fn tick(&self) -> (i64, u64) {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        (self.start_ms + n as i64 * 1000, n)
    }
}

impl Clock for TestClock {
    fn now(&self) -> jiff::Timestamp {
        jiff::Timestamp::from_millisecond(self.tick().0).unwrap()
    }

    fn ulid(&self) -> Ulid {
        let (ms, n) = self.tick();
        Ulid::from_parts(ms as u64, n as u128)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn is_finished(self) -> bool {
        matches!(
            self,
            JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled
        )
    }
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.pad(match self {
            JobStatus::Pending => "pending",
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    pub amount: f64,
    pub unit: String,
}

/// One generation, stored as jobs/<id>/job.toml next to its outputs. Never changed once finished.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    #[serde(skip, default = "Ulid::nil")]
    pub id: Ulid,
    pub op: Op,
    pub plugin: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shot: Option<ShotRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take: Option<Ulid>,
    /// Position in the take's recipe, from 0.
    #[serde(default)]
    pub step: u32,
    /// Other job ids, or project files.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,
    #[serde(default, skip_serializing_if = "toml::Table::is_empty")]
    pub params: toml::Table,
    /// What the person wrote, when the prompt was made from it (an anchor's "side view").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    /// The brief in English, when it was translated before going into the prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub english: Option<String>,
    /// The prompt exactly as the model got it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub status: JobStatus,
    /// The plugin's own id for the job, for polling and cancelling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_job: Option<String>,
    /// File names inside the job folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Cost>,
    pub created: jiff::Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished: Option<jiff::Timestamp>,
    /// The element it's for: who speaks a line's recording, or whose anchor an image is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub who: Option<String>,
    /// A recording thrown out but kept, since a take still uses it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub discarded: bool,
}

impl Job {
    /// A pending job; `Project::create_job` gives it an id and creation time.
    pub fn new(op: Op, plugin: &str, model: &str) -> Job {
        Job {
            id: Ulid::nil(),
            op,
            plugin: plugin.into(),
            model: model.into(),
            shot: None,
            take: None,
            step: 0,
            inputs: vec![],
            params: toml::Table::new(),
            brief: None,
            english: None,
            prompt: None,
            status: JobStatus::Pending,
            plugin_job: None,
            outputs: vec![],
            error: None,
            cost: None,
            created: jiff::Timestamp::UNIX_EPOCH,
            finished: None,
            who: None,
            discarded: false,
        }
    }
}

/// An image a model made: an anchor, a shot's picture or its storyboard frame.
#[derive(Debug, Clone)]
pub struct MadeImage {
    pub path: PathBuf,
    pub shot: Option<ShotRef>,
    /// The element's handle, for an anchor.
    pub element: Option<String>,
    pub job: Job,
}

/// One run of a shot's recipe: a chain of jobs.
#[derive(Debug, Clone)]
pub struct Take {
    pub id: Ulid,
    pub shot: ShotRef,
    /// 1-based position among the shot's takes, by creation order. Not stored.
    pub number: usize,
    /// In step order.
    pub jobs: Vec<Job>,
}

impl Take {
    /// The finished last step, if the take got that far.
    pub fn output(&self) -> Option<&Job> {
        self.jobs.last().filter(|j| j.status == JobStatus::Done)
    }

    pub fn status(&self) -> JobStatus {
        self.jobs.last().map_or(JobStatus::Pending, |j| j.status)
    }

    /// Thrown out. Its recordings don't count: lines may still send them.
    pub fn removed(&self) -> bool {
        self.jobs
            .iter()
            .any(|j| j.discarded && j.op != Op::AudioTts)
    }

    /// Total per unit across all steps.
    pub fn costs(&self) -> Vec<Cost> {
        let mut out: Vec<Cost> = vec![];
        for c in self.jobs.iter().filter_map(|j| j.cost.as_ref()) {
            match out.iter_mut().find(|x| x.unit == c.unit) {
                Some(x) => x.amount += c.amount,
                None => out.push(c.clone()),
            }
        }
        out
    }
}

impl fmt::Display for Take {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}#{}", self.shot, self.number)
    }
}

impl Project {
    pub fn job_dir(&self, id: Ulid) -> PathBuf {
        self.root.join("jobs").join(id.to_string())
    }

    pub fn new_take_id(&self) -> Ulid {
        self.clock.ulid()
    }

    /// Writes a new pending job. It gets an id unless it already has one (callers that need the id
    /// before the job exists, e.g. for a ledger reservation, take one from `self.clock`).
    pub fn create_job(&self, mut job: Job) -> Result<Job> {
        if job.id.is_nil() {
            job.id = self.clock.ulid();
        }
        if self.job_dir(job.id).join("job.toml").exists() {
            return invalid(format!("job {} already exists", job.id));
        }
        job.created = self.clock.now();
        job.status = JobStatus::Pending;
        fsx::write_toml(&self.job_dir(job.id).join("job.toml"), &job)?;
        Ok(job)
    }

    /// Writes a job's new state. Finished jobs can't be changed.
    pub fn save_job(&self, job: &Job) -> Result<()> {
        let current = self.load_job(job.id)?;
        if current.status.is_finished() {
            return invalid(format!(
                "job {} is {} and can't change",
                job.id, current.status
            ));
        }
        let mut job = job.clone();
        if job.status.is_finished() && job.finished.is_none() {
            job.finished = Some(self.clock.now());
        }
        fsx::write_toml(&self.job_dir(job.id).join("job.toml"), &job)
    }

    /// Marks a finished job thrown out, or back. The one change a finished job takes.
    pub fn set_discarded(&self, id: Ulid, discarded: bool) -> Result<()> {
        let mut job = self.load_job(id)?;
        job.discarded = discarded;
        fsx::write_toml(&self.job_dir(id).join("job.toml"), &job)
    }

    fn load_job(&self, id: Ulid) -> Result<Job> {
        let path = self.job_dir(id).join("job.toml");
        if !path.exists() {
            return invalid(format!("no job {id}"));
        }
        let mut job: Job = fsx::read_toml(&path)?;
        job.id = id;
        Ok(job)
    }

    /// Every image made for the film and not thrown out, newest first, so one made again doesn't
    /// lose the one before.
    pub fn made_images(&self) -> Result<Vec<MadeImage>> {
        // Anchors made before jobs said whose they were are known by matching an anchor's bytes.
        let mut anchors: Vec<(String, PathBuf, u64)> = vec![];
        for el in self.elements()? {
            for a in self.anchors(&el)? {
                if let Ok(m) = std::fs::metadata(&a) {
                    anchors.push((el.handle.clone(), a, m.len()));
                }
            }
        }
        let whose = |path: &Path| -> Option<String> {
            let len = std::fs::metadata(path).ok()?.len();
            let mine = fsx::read_bytes(path).ok()?;
            anchors
                .iter()
                .filter(|(_, _, l)| *l == len)
                .find(|(_, a, _)| fsx::read_bytes(a).is_ok_and(|b| b == mine))
                .map(|(h, _, _)| h.clone())
        };
        let mut out = vec![];
        for j in self.jobs()? {
            if !matches!(j.op, Op::ImageGenerate | Op::ImageEdit)
                || j.status != JobStatus::Done
                || j.discarded
            {
                continue;
            }
            let Some(f) = j.outputs.first() else { continue };
            let path = self.job_dir(j.id).join(f);
            if path.is_file() {
                let element = match (&j.who, j.shot) {
                    (None, None) => whose(&path),
                    _ => j.who.clone(),
                };
                out.push(MadeImage {
                    path,
                    shot: j.shot,
                    element,
                    job: j,
                });
            }
        }
        out.reverse();
        Ok(out)
    }

    /// The job whose output `file` is a copy of, like an anchor made by "Make image". Newest
    /// first, so a picture made twice is put down to the last time.
    pub fn job_that_made(&self, file: &Path) -> Result<Option<Job>> {
        let Ok(len) = std::fs::metadata(file).map(|m| m.len()) else {
            return Ok(None);
        };
        let mut bytes = None;
        let mut jobs = self.jobs()?;
        jobs.sort_by_key(|j| std::cmp::Reverse(j.id));
        for j in jobs {
            for o in &j.outputs {
                let out = self.job_dir(j.id).join(o);
                if out == file || std::fs::metadata(&out).map(|m| m.len()).ok() != Some(len) {
                    continue;
                }
                let mine = match &bytes {
                    Some(b) => b,
                    None => bytes.insert(fsx::read_bytes(file)?),
                };
                if fsx::read_bytes(&out)? == *mine {
                    return Ok(Some(j));
                }
            }
        }
        Ok(None)
    }

    pub fn jobs(&self) -> Result<Vec<Job>> {
        let mut out = vec![];
        for dir in fsx::list(&self.root.join("jobs"))? {
            let Some(id) = dir
                .file_name()
                .and_then(|n| Ulid::from_string(&n.to_string_lossy()).ok())
            else {
                continue;
            };
            if dir.join("job.toml").exists() {
                out.push(self.load_job(id)?);
            }
        }
        out.sort_by_key(|j| j.id);
        Ok(out)
    }

    /// By full id or a unique prefix, any case.
    pub fn job(&self, id: &str) -> Result<Job> {
        let id = id.to_ascii_uppercase();
        let mut hits = self
            .jobs()?
            .into_iter()
            .filter(|j| j.id.to_string().starts_with(&id));
        match (hits.next(), hits.next()) {
            (Some(j), None) => Ok(j),
            (None, _) => invalid(format!("no job {id}")),
            (Some(_), Some(_)) => invalid(format!("{id} matches more than one job")),
        }
    }

    pub fn takes(&self, shot: ShotRef) -> Result<Vec<Take>> {
        let mut takes: Vec<Take> = vec![];
        for job in self.jobs()? {
            let (Some(s), Some(t)) = (job.shot, job.take) else {
                continue;
            };
            if s != shot {
                continue;
            }
            match takes.iter_mut().find(|x| x.id == t) {
                Some(take) => take.jobs.push(job),
                None => takes.push(Take {
                    id: t,
                    shot,
                    number: 0,
                    jobs: vec![job],
                }),
            }
        }
        takes.sort_by_key(|t| t.id);
        for (i, t) in takes.iter_mut().enumerate() {
            t.number = i + 1;
            t.jobs.sort_by_key(|j| (j.step, j.id));
        }
        // Numbered first, so the others keep theirs.
        takes.retain(|t| !t.removed());
        Ok(takes)
    }

    /// Where a take's last frame is kept; a shot continuing from the take starts on it.
    pub fn last_frame(&self, take: &Take) -> Option<PathBuf> {
        Some(self.job_dir(take.output()?.id).join("last.png"))
    }

    /// The takes, not thrown out, whose last frame has been taken, with it. Newest first.
    pub fn last_frames(&self) -> Result<Vec<(Take, PathBuf)>> {
        let mut shots = vec![];
        for j in self.jobs()? {
            if let Some(s) = j.shot
                && j.take.is_some()
                && !shots.contains(&s)
                && self.job_dir(j.id).join("last.png").is_file()
            {
                shots.push(s);
            }
        }
        let mut out = vec![];
        for s in shots {
            for t in self.takes(s)? {
                if let Some(f) = self.last_frame(&t).filter(|f| f.is_file()) {
                    out.push((t, f));
                }
            }
        }
        out.sort_by_key(|(t, _)| std::cmp::Reverse(t.id));
        Ok(out)
    }

    /// The take a file belongs to, by the `jobs/<id>/` folder it's in. Only those two folders
    /// count, so a path written by another app, absolute or through a link, still matches.
    pub fn take_of_file(&self, file: &Path) -> Option<Take> {
        let dir = file.parent()?;
        if dir.parent()?.file_name()? != "jobs" {
            return None;
        }
        let id = Ulid::from_string(dir.file_name()?.to_str()?).ok()?;
        let job = self.job(&id.to_string()).ok()?;
        self.takes(job.shot?)
            .ok()?
            .into_iter()
            .find(|t| Some(t.id) == job.take)
    }

    /// `3`, `#3` or a ULID prefix.
    pub fn take(&self, shot: ShotRef, spec: &str) -> Result<Take> {
        let takes = self.takes(shot)?;
        let spec = spec.trim_start_matches('#');
        if let Ok(n) = spec.parse::<usize>()
            && let Some(t) = takes.iter().find(|t| t.number == n)
        {
            return Ok(t.clone());
        }
        let upper = spec.to_ascii_uppercase();
        let mut hits = takes
            .into_iter()
            .filter(|t| !upper.is_empty() && t.id.to_string().starts_with(&upper));
        match (hits.next(), hits.next()) {
            (Some(t), None) => Ok(t),
            (Some(_), Some(_)) => invalid(format!("{spec} matches more than one take of {shot}")),
            (None, _) => invalid(format!("{shot} has no take {spec}")),
        }
    }

    /// Makes a finished take the one the shot uses.
    pub fn select_take(&self, shot: ShotRef, spec: &str) -> Result<Take> {
        let take = self.take(shot, spec)?;
        if take.output().is_none() {
            return invalid(format!(
                "{take} is {}, only done takes can be selected",
                take.status()
            ));
        }
        let mut s = self.shot(shot)?;
        s.selected_take = Some(take.id.to_string());
        self.save_shot(shot, &s)?;
        Ok(take)
    }

    /// Throws a finished take out. Its files stay (it's in the ledger) and the others keep their
    /// numbers; the shot stops using it if it did.
    pub fn remove_take(&self, shot: ShotRef, spec: &str) -> Result<Take> {
        let take = self.take(shot, spec)?;
        if !take.status().is_finished() {
            return invalid(format!("{take} is still {}", take.status()));
        }
        for j in take.jobs.iter().filter(|j| j.op != Op::AudioTts) {
            self.set_discarded(j.id, true)?;
        }
        let mut s = self.shot(shot)?;
        if s.selected_take.as_deref() == Some(take.id.to_string().as_str()) {
            s.selected_take = None;
            self.save_shot(shot, &s)?;
        }
        Ok(take)
    }

    /// The take a shot has selected, if it still exists.
    pub fn selected_take(&self, shot: ShotRef) -> Result<Option<Take>> {
        let Some(id) = self.shot(shot)?.selected_take else {
            return Ok(None);
        };
        let id = Ulid::from_string(&id)
            .map_err(|_| Error::Invalid(format!("{shot}: selected_take {id:?} isn't a ULID")))?;
        Ok(self.takes(shot)?.into_iter().find(|t| t.id == id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_take_by_its_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = Project::init(&tmp.path().join("film"), Some("Rain")).unwrap();
        p.add_chapter("One").unwrap();
        let r = p.add_shot("1", None, crate::Shot::default()).unwrap();
        let mut j = Job::new(Op::VideoGenerate, "fake", "fake/video");
        j.shot = Some(r);
        j.take = Some(p.new_take_id());
        let j = p.create_job(j).unwrap();
        // Named by another app through another path to the same folder.
        let elsewhere = Path::new("/Volumes/x/film/jobs")
            .join(j.id.to_string())
            .join("out.mp4");
        let t = p.take_of_file(&elsewhere).unwrap();
        assert_eq!((t.shot, t.number), (r, 1));
        assert!(p.take_of_file(Path::new("/x/jobs/nope/out.mp4")).is_none());
        assert!(
            p.take_of_file(&Path::new("/x/renders").join(j.id.to_string()).join("a.mp4"))
                .is_none()
        );
    }

    #[test]
    fn system_ids_only_go_up() {
        let ids: Vec<Ulid> = (0..2000).map(|_| SystemClock.ulid()).collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]));
    }
}
