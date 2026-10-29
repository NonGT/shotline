use std::fmt;
use std::path::PathBuf;
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
            status: JobStatus::Pending,
            plugin_job: None,
            outputs: vec![],
            error: None,
            cost: None,
            created: jiff::Timestamp::UNIX_EPOCH,
            finished: None,
        }
    }
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

    pub fn create_job(&self, mut job: Job) -> Result<Job> {
        job.id = self.clock.ulid();
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

    fn load_job(&self, id: Ulid) -> Result<Job> {
        let path = self.job_dir(id).join("job.toml");
        if !path.exists() {
            return invalid(format!("no job {id}"));
        }
        let mut job: Job = fsx::read_toml(&path)?;
        job.id = id;
        Ok(job)
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
        Ok(takes)
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
    fn system_ids_only_go_up() {
        let ids: Vec<Ulid> = (0..2000).map(|_| SystemClock.ulid()).collect();
        assert!(ids.windows(2).all(|w| w[0] < w[1]));
    }
}
