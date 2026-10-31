//! A film's history, through the user's own git so their name, email and signing settings apply.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};

#[derive(Clone)]
pub struct Git {
    exe: PathBuf,
    identity: Option<(String, String)>,
}

impl Git {
    /// The git on PATH, if there is one.
    pub fn find() -> Option<Git> {
        let git = Git {
            exe: "git".into(),
            identity: None,
        };
        let ok = Command::new(&git.exe)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        ok.then_some(git)
    }

    /// Commits as this name and email instead of the user's config, for tests.
    pub fn with_identity(mut self, name: &str, email: &str) -> Git {
        self.identity = Some((name.into(), email.into()));
        self
    }

    fn run(&self, dir: &Path, args: &[&str]) -> Result<String> {
        let mut c = Command::new(&self.exe);
        c.arg("-C").arg(dir);
        if let Some((name, email)) = &self.identity {
            c.arg("-c").arg(format!("user.name={name}"));
            c.arg("-c").arg(format!("user.email={email}"));
        }
        let out = c.args(args).output().map_err(|e| Error::Io {
            path: self.exe.clone(),
            source: e,
        })?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(Error::Invalid(format!("git {}: {}", args[0], err.trim())));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Whether `dir` is in a repository, its own or a parent folder's.
    pub fn is_repo(&self, dir: &Path) -> bool {
        self.run(dir, &["rev-parse", "--git-dir"]).is_ok()
    }

    /// Files under `dir` that differ from the last commit, relative to `dir`.
    pub fn changes(&self, dir: &Path) -> Result<Vec<String>> {
        let prefix = self.run(dir, &["rev-parse", "--show-prefix"])?;
        let prefix = prefix.trim();
        let out = self.run(
            dir,
            &[
                "status",
                "--porcelain",
                "-z",
                "--untracked-files=all",
                "--",
                ".",
            ],
        )?;
        let mut paths = vec![];
        let mut fields = out.split('\0').filter(|f| !f.is_empty());
        while let Some(f) = fields.next() {
            let (xy, path) = f.split_at(3.min(f.len()));
            // A rename is followed by the name it had before.
            if xy.starts_with('R') || xy.starts_with('C') {
                fields.next();
            }
            paths.push(path.strip_prefix(prefix).unwrap_or(path).to_string());
        }
        Ok(paths)
    }

    /// Commits everything under `dir` and nothing outside it. Returns the short hash.
    pub fn commit(&self, dir: &Path, message: &str) -> Result<String> {
        self.run(dir, &["add", "--all", "--", "."])?;
        self.run(dir, &["commit", "--quiet", "--message", message, "--", "."])?;
        Ok(self
            .run(dir, &["rev-parse", "--short", "HEAD"])?
            .trim()
            .into())
    }

    /// Starts a repository in `dir` with everything in it as the first commit.
    pub fn init(&self, dir: &Path, message: &str) -> Result<String> {
        self.run(dir, &["init", "--quiet"])?;
        self.commit(dir, message)
    }
}

/// A commit message naming what changed: shots by number, elements by handle.
pub fn summary(paths: &[String]) -> String {
    let mut names: Vec<String> = vec![];
    for p in paths {
        let parts: Vec<&str> = p.split('/').collect();
        let stem = |s: &str| s.split('.').next().unwrap_or(s).to_string();
        let chapter = |s: &str| s.split('-').next().unwrap_or(s).to_string();
        let name = match parts.as_slice() {
            ["chapters", c, "shots", f, ..] => format!("{}-{}", chapter(c), stem(f)),
            ["chapters", c, ..] => format!("chapter {}", chapter(c)),
            ["elements", _, e, ..] => format!("@{}", stem(e)),
            ["jobs", ..] => "takes".into(),
            [f] => stem(f),
            _ => p.clone(),
        };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    let list = match names.as_slice() {
        [] => return "update".into(),
        [one] => one.clone(),
        [first @ .., last] if first.len() < 4 => format!("{} and {last}", first.join(", ")),
        _ => format!("{} and {} more", names[..3].join(", "), names.len() - 3),
    };
    format!("edit {list}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_what_changed() {
        let s = |paths: &[&str]| summary(&paths.iter().map(|p| p.to_string()).collect::<Vec<_>>());
        assert_eq!(s(&["chapters/01-rain/shots/010.toml"]), "edit 01-010");
        assert_eq!(
            s(&[
                "chapters/01-rain/shots/010.toml",
                "chapters/01-rain/shots/010.png",
                "elements/cast/mali/01.png",
                "story.md",
            ]),
            "edit 01-010, @mali and story"
        );
        assert_eq!(
            s(&["jobs/01J/job.toml", "chapters/02/chapter.toml"]),
            "edit takes and chapter 02"
        );
        assert_eq!(
            s(&["a.md", "b.md", "c.md", "d.md", "e.md"]),
            "edit a, b, c and 2 more"
        );
    }
}
