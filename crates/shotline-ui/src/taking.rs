//! Takes shot out of the app's way, so closing it doesn't stop them. Like `running`, each has a
//! record in the film's cache, a beat file touched while it's being shot, and a stop file the
//! app leaves to ask for it to be cancelled.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use shotline_core::ShotRef;

use crate::Host;

const DIR: &str = ".shotline/cache/takes";
const BEAT: Duration = Duration::from_millis(500);
/// Without a beat for this long, nothing is shooting it any more.
const GONE: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Take {
    #[serde(skip)]
    pub id: String,
    pub shot: ShotRef,
    pub started: jiff::Timestamp,
    /// The price agreed to.
    pub cost: String,
    pub translate: bool,
    /// What it's doing now, e.g. "running (40%)".
    #[serde(default)]
    pub step: String,
    /// The line about the take once it's shot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Running,
    /// Whatever was shooting it went away before it was done.
    Stopped,
    Done(String),
    Failed(String),
}

fn dir(film: &Path) -> PathBuf {
    film.join(DIR)
}

fn file(film: &Path, id: &str, ext: &str) -> PathBuf {
    dir(film).join(format!("{id}.{ext}"))
}

/// Writes a new take, giving it its id.
pub fn create(film: &Path, t: &mut Take) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir(film))?;
    let mut n = jiff::Timestamp::now().as_nanosecond();
    while file(film, &n.to_string(), "json").exists() {
        n += 1;
    }
    t.id = n.to_string();
    std::fs::write(file(film, &t.id, "beat"), "")?;
    save(film, t)
}

fn save(film: &Path, t: &Take) -> anyhow::Result<()> {
    // Whole or not at all, since the app reads it while it's being shot.
    let tmp = file(film, &t.id, "tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(t)?)?;
    std::fs::rename(tmp, file(film, &t.id, "json"))?;
    Ok(())
}

pub fn load(film: &Path, id: &str) -> anyhow::Result<Take> {
    let mut t: Take = serde_json::from_str(&std::fs::read_to_string(file(film, id, "json"))?)?;
    t.id = id.to_string();
    Ok(t)
}

pub fn remove(film: &Path, id: &str) {
    for ext in ["json", "beat", "stop", "log"] {
        let _ = std::fs::remove_file(file(film, id, ext));
    }
}

/// Asks whatever is shooting it to cancel.
pub fn stop(film: &Path, id: &str) {
    let _ = std::fs::write(file(film, id, "stop"), "");
}

pub fn stopping(film: &Path, id: &str) -> bool {
    file(film, id, "stop").exists()
}

pub fn state(film: &Path, t: &Take) -> State {
    if let Some(d) = &t.done {
        return State::Done(d.clone());
    }
    if let Some(e) = &t.error {
        return State::Failed(e.clone());
    }
    let beat = std::fs::metadata(file(film, &t.id, "beat")).and_then(|m| m.modified());
    match beat.map(|b| SystemTime::now().duration_since(b).unwrap_or_default()) {
        Ok(age) if age < GONE => State::Running,
        _ => State::Stopped,
    }
}

/// Every take the film has going or done, oldest first.
pub fn list(film: &Path) -> Vec<Take> {
    let Ok(entries) = std::fs::read_dir(dir(film)) else {
        return vec![];
    };
    let mut out: Vec<Take> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            load(film, name.strip_suffix(".json")?).ok()
        })
        .collect();
    out.sort_by(|a, b| (a.started, &a.id).cmp(&(b.started, &b.id)));
    out
}

pub fn finish(film: &Path, id: &str, out: anyhow::Result<String>) -> anyhow::Result<()> {
    let mut t = load(film, id)?;
    match out {
        Ok(d) => t.done = Some(d),
        Err(e) => t.error = Some(format!("{e:#}")),
    }
    save(film, &t)?;
    let _ = std::fs::remove_file(file(film, id, "beat"));
    Ok(())
}

/// Shoots take `id`, beating and writing down each step until it's done. This is what
/// `shotline __take` does.
pub fn work(host: &dyn Host, film: &Path, id: &str) -> anyhow::Result<()> {
    let t = load(film, id)?;
    let (done, stop) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let beat = {
        let (done, stop, film, id) = (
            done.clone(),
            stop.clone(),
            film.to_path_buf(),
            id.to_string(),
        );
        std::thread::spawn(move || {
            let beat = file(&film, &id, "beat");
            while !done.load(Ordering::Relaxed) {
                if let Ok(f) = std::fs::File::options().write(true).open(&beat) {
                    let _ = f.set_modified(SystemTime::now());
                }
                if stopping(&film, &id) {
                    stop.store(true, Ordering::Relaxed);
                }
                std::thread::sleep(BEAT);
            }
        })
    };
    let mut seen = t.clone();
    let mut on = |s: String| {
        seen.step = s;
        let _ = save(film, &seen);
    };
    let out = host.take(film, t.shot, &t.cost, t.translate, &mut on, stop);
    done.store(true, Ordering::Relaxed);
    let _ = beat.join();
    finish(film, id, out)
}

/// Starts shooting it: in a process of its own when the host can, else on a thread here.
/// True when it's in a process of its own, so it goes on after the app closes.
pub fn start(host: Arc<dyn Host>, film: &Path, id: &str) -> bool {
    match host.start_take(film, id) {
        Ok(true) => true,
        Ok(false) => {
            let (film, id) = (film.to_path_buf(), id.to_string());
            std::thread::spawn(move || {
                if let Err(e) = work(host.as_ref(), &film, &id) {
                    let _ = finish(&film, &id, Err(e));
                }
            });
            false
        }
        Err(e) => {
            let _ = finish(film, id, Err(e.context("couldn't start the take")));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_take_reads_back_and_finishes() {
        let film = tempfile::tempdir().unwrap();
        let mut t = Take {
            id: String::new(),
            shot: "01-010".parse().unwrap(),
            started: jiff::Timestamp::UNIX_EPOCH,
            cost: "$0.40".into(),
            translate: false,
            step: String::new(),
            done: None,
            error: None,
        };
        create(film.path(), &mut t).unwrap();
        assert_eq!(list(film.path()), [t.clone()]);
        assert_eq!(state(film.path(), &t), State::Running);
        assert!(!stopping(film.path(), &t.id));
        stop(film.path(), &t.id);
        assert!(stopping(film.path(), &t.id));
        finish(film.path(), &t.id, Ok("take 1 of 01-010".into())).unwrap();
        let back = load(film.path(), &t.id).unwrap();
        assert_eq!(
            state(film.path(), &back),
            State::Done("take 1 of 01-010".into())
        );
        remove(film.path(), &t.id);
        assert!(list(film.path()).is_empty());
    }
}
