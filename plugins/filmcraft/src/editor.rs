//! The way an edit reaches a FilmCraft project. FilmCraft rewrites the whole file on save, so the
//! plugin never writes it: edits are FilmCraft's own commands, sent to the app shotline started
//! when it has the project open, else run by `filmcraft-cli` on the file. A FilmCraft shotline
//! didn't start may have the file open and would save over the CLI's edits, so then nothing is
//! done and the edits wait.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::control::{self, Session};

pub enum Editor {
    App(Session),
    Cli(Cli),
}

/// filmcraft-cli on one project file, which it reads, changes and saves for each edit.
pub struct Cli {
    exe: PathBuf,
    project: PathBuf,
    backups: PathBuf,
    backed_up: Cell<bool>,
}

/// Copies of the project from before the CLI saved over it, newest last.
const BACKUPS: usize = 20;

pub fn backups_dir(film: &Path) -> PathBuf {
    film.join(".shotline/cache/filmcraft-backups")
}

impl Editor {
    pub fn cli(exe: PathBuf, project: PathBuf, film: &Path) -> Editor {
        Editor::Cli(Cli {
            exe,
            project,
            backups: backups_dir(film),
            backed_up: Cell::new(false),
        })
    }

    /// A command that changes the project; the CLI saves it after.
    pub fn exec(&self, command: &str, params: Value) -> Result<Value, String> {
        match self {
            Editor::App(s) => s.exec(command, params),
            Editor::Cli(c) => {
                if !c.backed_up.get() {
                    back_up(&c.project, &c.backups)
                        .map_err(|e| format!("can't back the project up first: {e}"))?;
                    c.backed_up.set(true);
                }
                cli(&c.exe, &c.project, true, command, params)
            }
        }
    }

    /// A command that only looks.
    pub fn read(&self, command: &str, params: Value) -> Result<Value, String> {
        match self {
            Editor::App(s) => s.exec(command, params),
            Editor::Cli(c) => cli(&c.exe, &c.project, false, command, params),
        }
    }

    /// Commands (`{"id", "params"}`) one after another, stopping at the first that fails. The
    /// CLI runs them in one go, so what one sets up (an open sequence) holds for the next.
    pub fn batch(&self, steps: &[Value]) -> Result<Vec<Value>, String> {
        match self {
            Editor::App(_) => steps
                .iter()
                .map(|s| {
                    let id = s["id"].as_str().ok_or("a step without a command")?;
                    self.exec(id, s["params"].clone())
                })
                .collect(),
            Editor::Cli(c) => {
                if !c.backed_up.get() {
                    back_up(&c.project, &c.backups)
                        .map_err(|e| format!("can't back the project up first: {e}"))?;
                    c.backed_up.set(true);
                }
                cli_run(&c.exe, &c.project, steps)
            }
        }
    }
}

fn cli_run(exe: &Path, project: &Path, steps: &[Value]) -> Result<Vec<Value>, String> {
    use std::io::Write;
    let mut child = Command::new(exe)
        .arg("--project")
        .arg(project)
        .args(["--compact", "--save", "run", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("can't run {}: {e}", exe.display()))?;
    let script: String = steps.iter().map(|s| format!("{s}\n")).collect();
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(script.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let mut results = vec![];
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["ok"] != true {
            return Err(format!(
                "{}: {}",
                v["id"].as_str().unwrap_or("a step"),
                v["error"].as_str().unwrap_or("failed")
            ));
        }
        results.push(v["result"].clone());
    }
    if !out.status.success() || results.len() != steps.len() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        return Err(format!(
            "filmcraft-cli ran {} of {} steps ({}) {}",
            results.len(),
            steps.len(),
            out.status,
            last.trim()
        ));
    }
    Ok(results)
}

/// Keeps a copy of `project` as `<name>-<unix ms>.fcproj`, and only the last few.
fn back_up(project: &Path, dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let stem = project.file_stem().unwrap_or_default().to_string_lossy();
    std::fs::copy(project, dir.join(format!("{stem}-{ms}.fcproj")))?;
    let mut kept: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(&format!("{stem}-")))
        })
        .collect();
    kept.sort();
    let extra = kept.len().saturating_sub(BACKUPS);
    for old in &kept[..extra] {
        std::fs::remove_file(old)?;
    }
    Ok(())
}

fn cli(
    exe: &Path,
    project: &Path,
    save: bool,
    command: &str,
    params: Value,
) -> Result<Value, String> {
    let mut c = Command::new(exe);
    c.arg("--project").arg(project).arg("--compact");
    if save {
        c.arg("--save");
    }
    let out = c
        .args(["exec", command, &params.to_string()])
        .output()
        .map_err(|e| format!("can't run {}: {e}", exe.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        let line = line.trim().trim_start_matches("filmcraft-cli: ");
        return Err(if line.is_empty() {
            format!("{command}: filmcraft-cli failed ({})", out.status)
        } else {
            line.to_string()
        });
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(text.trim())
        .map_err(|_| format!("{command}: filmcraft-cli answered {}", text.trim()))
}

fn cli_name() -> &'static str {
    if cfg!(windows) {
        "filmcraft-cli.exe"
    } else {
        "filmcraft-cli"
    }
}

/// filmcraft-cli: FILMCRAFT_CLI if set, else on PATH, in the usual bin folders, or next to the
/// app. An app started from the Dock gets a short PATH, hence the folders.
pub fn cli_program() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FILMCRAFT_CLI") {
        return Some(p.into());
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    if let Some(h) = home.map(PathBuf::from) {
        dirs.push(h.join(".local/bin"));
        dirs.push(h.join(r"AppData\Local\Programs\ArtCraft\FilmCraft"));
    }
    dirs.extend(["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin"].map(PathBuf::from));
    if let Some(d) = control::program().parent() {
        dirs.push(d.to_path_buf());
    }
    dirs.into_iter()
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.join(cli_name()))
        .find(|p| p.is_file())
}

/// Process ids of running FilmCraft apps (not the CLI).
pub(crate) fn running_apps() -> Vec<u32> {
    let out = if cfg!(windows) {
        Command::new("tasklist")
            .args(["/FO", "CSV", "/NH", "/FI", "IMAGENAME eq filmcraft.exe"])
            .output()
    } else {
        Command::new("pgrep")
            .args(["-i", "-x", "filmcraft"])
            .output()
    };
    let Ok(out) = out else { return vec![] };
    let text = String::from_utf8_lossy(&out.stdout);
    if cfg!(windows) {
        text.lines()
            .filter_map(|l| l.split(',').nth(1))
            .filter_map(|p| p.trim_matches('"').parse().ok())
            .collect()
    } else {
        text.lines().filter_map(|l| l.trim().parse().ok()).collect()
    }
}

/// How to edit `project` now, or why it has to wait.
pub fn for_project(film: &Path, project: &Path) -> Result<Editor, String> {
    let ours = Session::load(film).filter(Session::alive);
    if let Some(s) = &ours {
        let open = s.exec("project.inspect", serde_json::json!({}))?;
        if open["path"]
            .as_str()
            .is_some_and(|p| crate::same_file(Path::new(p), project))
        {
            return Ok(Editor::App(s.clone()));
        }
    }
    let mine = ours.as_ref().and_then(|s| s.pid);
    if running_apps().into_iter().any(|p| Some(p) != mine) {
        return Err(
            "FilmCraft is open, but not from shotline; close it, or open the film there with \
             Open in FilmCraft"
                .into(),
        );
    }
    match cli_program() {
        Some(exe) => Ok(Editor::cli(exe, project.to_path_buf(), film)),
        None => Err("FilmCraft is closed and filmcraft-cli isn't installed".into()),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A stand-in CLI that prints its arguments, or fails like the real one.
    fn fake(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let exe = dir.join("filmcraft-cli");
        std::fs::write(&exe, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        exe
    }

    #[test]
    fn edits_through_the_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = fake(tmp.path(), r#"printf '{"n":%s}' "$#""#);
        let project = tmp.path().join("fan.fcproj");
        std::fs::write(&project, "{}").unwrap();
        let film = tmp.path().join("film");
        let e = Editor::cli(exe, project, &film);
        // --project p --compact [--save] exec cmd params
        assert_eq!(
            e.exec("file.newBin", serde_json::json!({})).unwrap()["n"],
            7
        );
        assert_eq!(
            e.read("project.inspect", serde_json::json!({})).unwrap()["n"],
            6
        );
        e.exec("file.newBin", serde_json::json!({})).unwrap();
        // Backed up once before its first save, not before each.
        let kept = std::fs::read_dir(backups_dir(&film)).unwrap().count();
        assert_eq!(kept, 1);
    }

    #[test]
    fn keeps_the_last_backups() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("fan.fcproj");
        std::fs::write(&project, "{}").unwrap();
        let dir = tmp.path().join("backups");
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..25 {
            std::fs::write(dir.join(format!("fan-{:013}.fcproj", 1000 + i)), "").unwrap();
        }
        std::fs::write(dir.join("other-1.fcproj"), "").unwrap();
        back_up(&project, &dir).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), BACKUPS + 1, "{names:?}");
        assert!(!names.contains(&"fan-0000000001005.fcproj".to_string()));
        assert!(names.contains(&"fan-0000000001006.fcproj".to_string()));
        assert!(names.contains(&"other-1.fcproj".to_string()));
    }

    #[test]
    fn says_what_the_cli_said() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = fake(
            tmp.path(),
            "echo 'filmcraft-cli: timeline.place: no such item' >&2; exit 1",
        );
        let project = tmp.path().join("f.fcproj");
        std::fs::write(&project, "{}").unwrap();
        let e = Editor::cli(exe, project, tmp.path());
        let err = e.exec("timeline.place", serde_json::json!({})).unwrap_err();
        assert_eq!(err, "timeline.place: no such item");
    }
}
