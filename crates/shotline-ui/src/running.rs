//! Images being made out of the app's way, so closing it doesn't stop them. Each has a record
//! in the film's cache, written when Make is clicked and again once it's made or failed, and a
//! beat file touched while something is still making it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::anchor::For;
use crate::{AnchorAsk, Host};

const DIR: &str = ".shotline/cache/running";
const BEAT: Duration = Duration::from_secs(5);
/// Without a beat for this long, nothing is making it any more.
const GONE: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Run {
    #[serde(skip)]
    pub id: String,
    /// As the popup had it, e.g. "Make a frame for 01-010".
    pub title: String,
    pub started: jiff::Timestamp,
    pub target: For,
    pub ask: AnchorAsk,
    /// The price agreed to.
    pub cost: String,
    /// What Default means in the popup's Size row.
    pub aspect: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Running,
    /// Its maker went away before it was done, e.g. the computer was turned off.
    Stopped,
    Made(PathBuf),
    Failed(String),
}

fn dir(film: &Path) -> PathBuf {
    film.join(DIR)
}

fn record(film: &Path, id: &str) -> PathBuf {
    dir(film).join(format!("{id}.json"))
}

fn beat_file(film: &Path, id: &str) -> PathBuf {
    dir(film).join(format!("{id}.beat"))
}

/// Writes a new run, giving it its id.
pub fn create(film: &Path, run: &mut Run) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir(film))?;
    let mut n = jiff::Timestamp::now().as_nanosecond();
    while record(film, &n.to_string()).exists() {
        n += 1;
    }
    run.id = n.to_string();
    std::fs::write(beat_file(film, &run.id), "")?;
    save(film, run)
}

fn save(film: &Path, run: &Run) -> anyhow::Result<()> {
    let tmp = dir(film).join(format!("{}.tmp", run.id));
    std::fs::write(&tmp, serde_json::to_string_pretty(run)?)?;
    std::fs::rename(tmp, record(film, &run.id))?;
    Ok(())
}

pub fn load(film: &Path, id: &str) -> anyhow::Result<Run> {
    let mut run: Run = serde_json::from_str(&std::fs::read_to_string(record(film, id))?)?;
    run.id = id.to_string();
    Ok(run)
}

pub fn remove(film: &Path, id: &str) {
    let _ = std::fs::remove_file(record(film, id));
    let _ = std::fs::remove_file(beat_file(film, id));
    let _ = std::fs::remove_file(dir(film).join(format!("{id}.log")));
}

fn state(film: &Path, run: &Run) -> State {
    if let Some(out) = &run.out {
        return State::Made(out.clone());
    }
    if let Some(e) = &run.error {
        return State::Failed(e.clone());
    }
    let beat = std::fs::metadata(beat_file(film, &run.id)).and_then(|m| m.modified());
    match beat.map(|t| SystemTime::now().duration_since(t).unwrap_or_default()) {
        Ok(age) if age < GONE => State::Running,
        _ => State::Stopped,
    }
}

/// Every run the film has, oldest first.
pub fn list(film: &Path) -> Vec<(Run, State)> {
    let Ok(entries) = std::fs::read_dir(dir(film)) else {
        return vec![];
    };
    let mut runs: Vec<(Run, State)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let run = load(film, name.strip_suffix(".json")?).ok()?;
            let s = state(film, &run);
            Some((run, s))
        })
        .collect();
    runs.sort_by(|a, b| (a.0.started, &a.0.id).cmp(&(b.0.started, &b.0.id)));
    runs
}

/// Records how it went.
pub fn finish(film: &Path, id: &str, made: anyhow::Result<PathBuf>) -> anyhow::Result<()> {
    let mut run = load(film, id)?;
    match made {
        Ok(out) => run.out = Some(out),
        Err(e) => run.error = Some(format!("{e:#}")),
    }
    save(film, &run)?;
    let _ = std::fs::remove_file(beat_file(film, id));
    Ok(())
}

/// Makes the image of run `id`, beating until it's done. This is what `shotline __run` does.
pub fn work(host: &dyn Host, film: &Path, id: &str) -> anyhow::Result<()> {
    let run = load(film, id)?;
    let done = Arc::new(AtomicBool::new(false));
    let beat = {
        let (done, file) = (done.clone(), beat_file(film, id));
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                if let Ok(f) = std::fs::File::options().write(true).open(&file) {
                    let _ = f.set_modified(SystemTime::now());
                }
                // Short naps, so it stops soon after it's done.
                for _ in 0..BEAT.as_millis() / 100 {
                    if done.load(Ordering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        })
    };
    let made = host.make_anchor(film, &run.ask, &run.cost);
    done.store(true, Ordering::Relaxed);
    let _ = beat.join();
    finish(film, id, made)
}

/// Starts making it: in a process of its own when the host can, else on a thread here.
pub fn start(host: Arc<dyn Host>, film: &Path, id: &str) {
    match host.start_run(film, id) {
        Ok(true) => {}
        Ok(false) => {
            let (film, id) = (film.to_path_buf(), id.to_string());
            std::thread::spawn(move || {
                if let Err(e) = work(host.as_ref(), &film, &id) {
                    let _ = finish(&film, &id, Err(e));
                }
            });
        }
        Err(e) => {
            let _ = finish(film, id, Err(e.context("couldn't start making it")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shotline_core::ShotRef;

    #[test]
    fn a_run_reads_back_as_written() {
        let film = tempfile::tempdir().unwrap();
        let r: ShotRef = "01-010".parse().unwrap();
        let mut run = Run {
            id: String::new(),
            title: "Remake picture 2 of 01-010".into(),
            started: jiff::Timestamp::UNIX_EPOCH,
            target: For::Frame(r, None),
            ask: AnchorAsk {
                handle: String::new(),
                shot: Some(r),
                frame: false,
                prompt: "at night".into(),
                plugin: "fal".into(),
                model: "nano".into(),
                refs: vec![film.path().join("a.png")],
                aspect: None,
                resolution: Some("2K".into()),
                translate: false,
                outfit: true,
            },
            cost: "$0.04".into(),
            aspect: "9:16".into(),
            resolution: None,
            out: None,
            error: None,
        };
        create(film.path(), &mut run).unwrap();
        assert_eq!(list(film.path()), [(run.clone(), State::Running)]);
        finish(film.path(), &run.id, Ok("made.png".into())).unwrap();
        assert_eq!(list(film.path())[0].1, State::Made("made.png".into()));
        remove(film.path(), &run.id);
        assert!(list(film.path()).is_empty());
    }
}
