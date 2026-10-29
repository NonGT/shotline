use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::invalid;
use crate::job::{Clock, SystemClock};
use crate::{Error, Result, fsx};

pub const CONFIG_FILE: &str = "shotline.toml";

/// shotline.toml
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub name: String,
    pub plugin: String,
    pub model: String,
    pub resolution: String,
    pub aspect: String,
    /// Seconds, used when a shot doesn't set its own.
    pub duration: f64,
    pub budget: Budget,
    pub prompt: PromptLayers,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: String::new(),
            plugin: "fal".into(),
            model: "minimax/h3-max/reference-to-video".into(),
            resolution: "768P".into(),
            aspect: "9:16".into(),
            duration: 8.0,
            budget: Budget::default(),
            prompt: PromptLayers::default(),
        }
    }
}

/// USD. Spending stops hard at these.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Budget {
    pub per_take: f64,
    pub per_day: f64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            per_take: 2.0,
            per_day: 10.0,
        }
    }
}

/// Which extra parts get added to a shot's prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PromptLayers {
    pub style: bool,
    pub identity: bool,
    pub continuity: bool,
    pub model_notes: bool,
}

impl Default for PromptLayers {
    fn default() -> Self {
        Self {
            style: true,
            identity: true,
            continuity: true,
            model_notes: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Project {
    pub root: PathBuf,
    pub config: Config,
    pub clock: Arc<dyn Clock>,
}

const STORY: &str = "\
# {name}

## Logline

## World

## Synopsis
";

const STYLE: &str = "\
<!-- Look of the film: palette, lighting, camera, lenses. Anything outside comments goes into every prompt. -->
";

const GITIGNORE: &str = "\
# Media made by shotline gets big fast. job.toml files stay, so what was made, from what and
# for how much is still on record.
jobs/**/*
!jobs/*/
!jobs/*/job.toml
.shotline/cache/
";

impl Project {
    pub fn init(dir: &Path, name: Option<&str>) -> Result<Project> {
        if dir.join(CONFIG_FILE).exists() {
            return invalid(format!("{} already has a {CONFIG_FILE}", dir.display()));
        }
        let name = match name {
            Some(n) => n.to_string(),
            None => dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "untitled".into()),
        };
        let config = Config {
            name: name.clone(),
            ..Config::default()
        };
        fsx::write_toml(&dir.join(CONFIG_FILE), &config)?;
        fsx::write(&dir.join("story.md"), &STORY.replace("{name}", &name))?;
        fsx::write(&dir.join("style.md"), STYLE)?;
        fsx::write(&dir.join(".gitignore"), GITIGNORE)?;
        for sub in [
            "elements/cast",
            "elements/sets",
            "elements/props",
            "chapters",
            ".shotline",
        ] {
            fsx::mkdir(&dir.join(sub))?;
        }
        Project::open(dir)
    }

    pub fn open(dir: &Path) -> Result<Project> {
        let path = dir.join(CONFIG_FILE);
        if !path.exists() {
            return Err(Error::NotAProject(dir.to_path_buf()));
        }
        Ok(Project {
            root: dir.to_path_buf(),
            config: fsx::read_toml(&path)?,
            clock: Arc::new(SystemClock),
        })
    }

    /// Opens the project containing `start`, looking upwards.
    pub fn discover(start: &Path) -> Result<Project> {
        let mut dir = Some(start);
        while let Some(d) = dir {
            if d.join(CONFIG_FILE).exists() {
                return Project::open(d);
            }
            dir = d.parent();
        }
        Err(Error::NotAProject(start.to_path_buf()))
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Project {
        self.clock = clock;
        self
    }

    /// style.md with HTML comments removed.
    pub fn style(&self) -> Result<String> {
        let path = self.root.join("style.md");
        if !path.exists() {
            return Ok(String::new());
        }
        Ok(strip_comments(&fsx::read(&path)?).trim().to_string())
    }

    /// Path relative to the project root with `/` separators on every OS.
    pub fn rel(&self, path: &Path) -> String {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        rel.components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// Per-user folder: plugins, .env, caches. `SHOTLINE_HOME` overrides it.
pub fn user_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("SHOTLINE_HOME") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".config").join("shotline")
}

fn strip_comments(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_comments() {
        assert_eq!(strip_comments("a<!-- x -->b<!--y-->c"), "abc");
        assert_eq!(strip_comments("a<!-- open"), "a");
    }

    #[test]
    fn config_fills_defaults() {
        let c: Config =
            toml::from_str("name = \"x\"\nduration = 5\n[budget]\nper_day = 3").unwrap();
        assert_eq!(c.duration, 5.0);
        assert_eq!(c.budget.per_day, 3.0);
        assert_eq!(c.budget.per_take, 2.0);
        assert_eq!(c.plugin, "fal");
    }
}
