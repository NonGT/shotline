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
    /// What puts typed prompts into English; the drafting model when not set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translate: Option<ModelRef>,
    /// Speaks the lines of a voice that doesn't name its own model, with the same plugin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts: Option<ModelRef>,
    /// For the built-in lip-synced recipe.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lipsync: Option<ModelRef>,
    /// Kept for the assistant; nothing uses it yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant: Option<ModelRef>,
    /// A model's options for every request of this film, by plugin then model, e.g.
    /// `[params.fal."minimax/h3-max/reference-to-video"] enable_safety_checker = false`.
    /// A shot's or a recipe step's own params win.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, BTreeMap<String, toml::Table>>,
}

/// A plugin and one of its models.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub plugin: String,
    pub model: String,
}

impl ModelRef {
    pub fn new(plugin: &str, model: &str) -> ModelRef {
        ModelRef {
            plugin: plugin.into(),
            model: model.into(),
        }
    }
}

/// The jobs a film picks a default model for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Task {
    Translate,
    Draft,
    Voice,
    Image,
    Video,
    LipSync,
    Assistant,
}

impl Task {
    pub const ALL: [Task; 7] = [
        Task::Translate,
        Task::Draft,
        Task::Voice,
        Task::Image,
        Task::Video,
        Task::LipSync,
        Task::Assistant,
    ];

    pub fn op(self) -> shotline_protocol::Op {
        use shotline_protocol::Op;
        match self {
            Task::Translate | Task::Draft | Task::Assistant => Op::TextGenerate,
            Task::Voice => Op::AudioTts,
            Task::Image => Op::ImageGenerate,
            Task::Video => Op::VideoGenerate,
            Task::LipSync => Op::VideoLipsync,
        }
    }

    /// Where it goes in shotline.toml; None for video, which is the film's own plugin and model.
    fn table(self) -> Option<&'static str> {
        match self {
            Task::Translate => Some("translate"),
            Task::Draft => Some("text"),
            Task::Voice => Some("tts"),
            Task::Image => Some("image"),
            Task::Video => None,
            Task::LipSync => Some("lipsync"),
            Task::Assistant => Some("assistant"),
        }
    }

    /// Whether it can be left unset.
    pub fn optional(self) -> bool {
        matches!(self, Task::Translate | Task::Voice | Task::Assistant)
    }
}

/// Lip sync when the film doesn't pick one.
pub const LIPSYNC: (&str, &str) = ("wavespeed", "sync/lipsync-3");

impl Config {
    /// The model set for `task`, or what it falls back to.
    pub fn model_for(&self, task: Task) -> Option<ModelRef> {
        match task {
            Task::Translate => self
                .translate
                .clone()
                .or_else(|| self.model_for(Task::Draft)),
            Task::Draft => Some(ModelRef::new(&self.text.plugin, &self.text.model)),
            Task::Voice => self.tts.clone(),
            Task::Image => Some(ModelRef::new(&self.image.plugin, &self.image.model)),
            Task::Video => Some(ModelRef::new(&self.plugin, &self.model)),
            Task::LipSync => Some(
                self.lipsync
                    .clone()
                    .unwrap_or_else(|| ModelRef::new(LIPSYNC.0, LIPSYNC.1)),
            ),
            Task::Assistant => self.assistant.clone(),
        }
    }

    /// Only what's set for `task` itself, not what it falls back to.
    pub fn model_set(&self, task: Task) -> Option<ModelRef> {
        match task {
            Task::Translate => self.translate.clone(),
            Task::LipSync => self.lipsync.clone(),
            t => self.model_for(t),
        }
    }

    /// The film's options for `model`, under `over`, which wins.
    pub fn params_for(&self, plugin: &str, model: &str, over: &toml::Table) -> toml::Table {
        let mut t = self
            .params
            .get(plugin)
            .and_then(|m| m.get(model))
            .cloned()
            .unwrap_or_default();
        for (k, v) in over {
            t.insert(k.clone(), v.clone());
        }
        t
    }
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
            translate: None,
            tts: None,
            lipsync: None,
            assistant: None,
            params: BTreeMap::new(),
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

/// Every film has these unless shotline.toml has a recipe of the same name: the lines spoken
/// with the speakers' voices, then the video model lip syncs them itself...
pub const SPOKEN: &str = "spoken";
/// ...or the video is made as usual and lip synced after.
pub const LIP_SYNCED: &str = "lip-synced";

fn builtin_recipe(name: &str, cfg: &Config) -> Option<Vec<Step>> {
    use shotline_protocol::Op;
    let step = |op, audio| Step {
        op,
        plugin: None,
        model: None,
        audio,
        params: toml::Table::new(),
    };
    match name {
        SPOKEN => Some(vec![
            step(Op::AudioTts, false),
            step(Op::VideoGenerate, true),
        ]),
        LIP_SYNCED => {
            let mut sync = step(Op::VideoLipsync, false);
            let m = cfg.model_for(crate::Task::LipSync)?;
            sync.plugin = Some(m.plugin);
            sync.model = Some(m.model);
            Some(vec![
                step(Op::AudioTts, false),
                step(Op::VideoGenerate, false),
                sync,
            ])
        }
        _ => None,
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

/// What every shot's video is made with and at, unless the shot says otherwise.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Format {
    pub plugin: String,
    pub model: String,
    /// e.g. 9:16
    pub aspect: String,
    /// As the model names it, e.g. 768P
    pub resolution: String,
    /// Seconds.
    pub duration: f64,
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

/// Characters of story.md a draft gets.
const STORY_BRIEF: usize = 1500;

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

    /// shotline.toml again, for when something else changed it.
    pub fn reread(&mut self) -> Result<()> {
        self.config = fsx::read_toml(&self.root.join(CONFIG_FILE))?;
        Ok(())
    }

    pub fn format(&self) -> Format {
        let c = &self.config;
        Format {
            plugin: c.plugin.clone(),
            model: c.model.clone(),
            aspect: c.aspect.clone(),
            resolution: c.resolution.clone(),
            duration: c.duration,
        }
    }

    /// Writes the format into shotline.toml, leaving everything else in it as it was.
    pub fn set_format(&mut self, f: &Format) -> Result<()> {
        if f.duration.is_nan() || f.duration <= 0.0 {
            return invalid(format!("a shot can't last {} seconds", f.duration));
        }
        for (what, v) in [
            ("plugin", &f.plugin),
            ("model", &f.model),
            ("shape", &f.aspect),
            ("resolution", &f.resolution),
        ] {
            if v.trim().is_empty() {
                return invalid(format!("the {what} can't be empty"));
            }
        }
        let path = self.root.join(CONFIG_FILE);
        let mut t: toml::Table = fsx::read_toml(&path)?;
        for (k, v) in [
            ("plugin", &f.plugin),
            ("model", &f.model),
            ("aspect", &f.aspect),
            ("resolution", &f.resolution),
        ] {
            t.insert(k.into(), v.trim().into());
        }
        t.insert("duration".into(), f.duration.into());
        fsx::write_toml(&path, &t)?;
        self.reread()
    }

    /// Sets the film's model for `task`, or with None takes it out (only where that's allowed),
    /// leaving the rest of shotline.toml as it was.
    pub fn set_model(&mut self, task: Task, to: Option<&ModelRef>) -> Result<()> {
        if let Some(m) = to
            && (m.plugin.trim().is_empty() || m.model.trim().is_empty())
        {
            return invalid("a model needs both its plugin and its name");
        }
        let path = self.root.join(CONFIG_FILE);
        let mut t: toml::Table = fsx::read_toml(&path)?;
        match (task.table(), to) {
            (None, Some(m)) => {
                t.insert("plugin".into(), m.plugin.trim().into());
                t.insert("model".into(), m.model.trim().into());
            }
            (Some(k), Some(m)) => {
                let x = table(&mut t, k);
                x.insert("plugin".into(), m.plugin.trim().into());
                x.insert("model".into(), m.model.trim().into());
            }
            (Some(k), None) if task.optional() || task == Task::LipSync => {
                t.remove(k);
            }
            _ => return invalid(format!("{task:?} needs a model")),
        }
        fsx::write_toml(&path, &t)?;
        self.reread()
    }

    /// Sets one of a model's options for the whole film, or with None takes it out, leaving the
    /// rest of shotline.toml as it was.
    pub fn set_param(
        &mut self,
        plugin: &str,
        model: &str,
        key: &str,
        value: Option<toml::Value>,
    ) -> Result<()> {
        let path = self.root.join(CONFIG_FILE);
        let mut t: toml::Table = fsx::read_toml(&path)?;
        let all = table(&mut t, "params");
        let mine = table(all, plugin);
        let opts = table(mine, model);
        match value {
            Some(v) => {
                opts.insert(key.into(), v);
            }
            None => {
                opts.remove(key);
            }
        }
        // No empty tables left behind.
        if opts.is_empty() {
            mine.remove(model);
        }
        if mine.is_empty() {
            all.remove(plugin);
        }
        if all.is_empty() {
            t.remove("params");
        }
        fsx::write_toml(&path, &t)?;
        self.reread()
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
            None => builtin_recipe(name, &self.config).map_or_else(
                || invalid(format!("no recipe called {name} in {CONFIG_FILE}")),
                Ok,
            ),
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

    /// story.md cut down for a text model: no comments, no title, no empty sections, and
    /// "Logline: ..." for each `## Logline` section. Empty if nothing's been written.
    pub fn story_brief(&self) -> Result<String> {
        let path = self.root.join("story.md");
        if !path.exists() {
            return Ok(String::new());
        }
        let text = strip_comments(&fsx::read(&path)?);
        let mut out: Vec<String> = vec![];
        let mut head: Option<&str> = None;
        let mut body: Vec<&str> = vec![];
        let mut flush = |head: Option<&str>, body: &mut Vec<&str>| {
            let b = body.join("\n").trim().to_string();
            body.clear();
            if !b.is_empty() {
                out.push(match head {
                    Some(h) => format!("{h}: {b}"),
                    None => b,
                });
            }
        };
        for line in text.lines() {
            if let Some(h) = line.strip_prefix("## ") {
                flush(head, &mut body);
                head = Some(h.trim());
            } else if !line.starts_with("# ") {
                body.push(line);
            }
        }
        flush(head, &mut body);
        let mut brief = out.join("\n\n");
        // A long story would crowd out the shot itself.
        if let Some((i, _)) = brief.char_indices().nth(STORY_BRIEF) {
            brief.truncate(i);
            brief = format!("{}…", brief.trim_end());
        }
        Ok(brief)
    }

    /// Path relative to the project root with `/` separators on every OS.
    pub fn rel(&self, path: &Path) -> String {
        // Outside the film, e.g. a reference image from Downloads, it stays as it is.
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return path.to_string_lossy().into_owned();
        };
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

/// The table under `key`, made if it isn't there or isn't a table.
fn table<'a>(t: &'a mut toml::Table, key: &str) -> &'a mut toml::Table {
    if !t.get(key).is_some_and(toml::Value::is_table) {
        t.insert(key.into(), toml::Value::Table(toml::Table::new()));
    }
    t.get_mut(key)
        .and_then(toml::Value::as_table_mut)
        .expect("just made")
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
