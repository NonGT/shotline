use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use shotline_ledger::Budgets;

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
    pub money: Money,
    pub budget: Budgets,
    /// `unit = "amount CURRENCY"`, e.g. `venice-credit = "0.004 USD"`.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub rates: BTreeMap<String, String>,
    /// Named lists of steps a take runs through, e.g. tts, video, lip sync.
    #[serde(rename = "recipe", skip_serializing_if = "BTreeMap::is_empty")]
    pub recipes: BTreeMap<String, Vec<Step>>,
    pub prompt: PromptLayers,
    /// What anchors and storyboard frames are made with.
    pub image: ImageConfig,
    /// What drafts prompts from plot.
    pub text: TextConfig,
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
            money: Money::default(),
            budget: Budgets::default(),
            rates: BTreeMap::new(),
            recipes: BTreeMap::new(),
            prompt: PromptLayers::default(),
            image: ImageConfig::default(),
            text: TextConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageConfig {
    pub plugin: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// For anchors; storyboard frames use the shot's. Default: the project's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect: Option<String>,
}

impl Default for ImageConfig {
    fn default() -> Self {
        Self {
            plugin: "fal".into(),
            model: "fal-ai/nano-banana".into(),
            resolution: None,
            aspect: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextConfig {
    pub plugin: String,
    pub model: String,
}

impl Default for TextConfig {
    fn default() -> Self {
        Self {
            plugin: "venice".into(),
            model: "claude-haiku-5-5".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Money {
    /// Budgets and totals are in this.
    pub currency: String,
}

impl Default for Money {
    fn default() -> Self {
        Self {
            currency: "USD".into(),
        }
    }
}

/// One step of a recipe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub op: shotline_protocol::Op,
    /// Defaults: the speaker's voice for audio.tts, the shot's model for video.generate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// For video.generate: send the lines' audio along for the model to lip sync itself.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub audio: bool,
    #[serde(default, skip_serializing_if = "toml::Table::is_empty")]
    pub params: toml::Table,
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
renders/
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
            budget: Budgets {
                per_take: Some(2.0),
                per_day: Some(10.0),
                ..Budgets::default()
            },
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

    /// The steps a take of `shot` runs: its recipe, or just video.generate.
    pub fn steps(&self, shot: &crate::Shot) -> Result<Vec<Step>> {
        let Some(name) = &shot.recipe else {
            return Ok(vec![Step {
                op: shotline_protocol::Op::VideoGenerate,
                plugin: None,
                model: None,
                audio: false,
                params: toml::Table::new(),
            }]);
        };
        match self.config.recipes.get(name) {
            Some(steps) if !steps.is_empty() => Ok(steps.clone()),
            Some(_) => invalid(format!("recipe {name} has no steps")),
            None => invalid(format!("no recipe called {name} in {CONFIG_FILE}")),
        }
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
        assert_eq!(c.budget.per_day, Some(3.0));
        assert_eq!(c.budget.per_take, None);
        assert_eq!(c.money.currency, "USD");
        assert_eq!(c.plugin, "fal");
    }
}
