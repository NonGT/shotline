use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::invalid;
use crate::protocol::Op;
use crate::{Error, Job, JobStatus, Project, Result, fsx, handle, slug};

const STEP: u32 = 10;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transition {
    #[default]
    Cut,
    Continue,
}

impl fmt::Display for Transition {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.pad(match self {
            Transition::Cut => "cut",
            Transition::Continue => "continue",
        })
    }
}

impl FromStr for Transition {
    type Err = Error;

    fn from_str(s: &str) -> Result<Transition> {
        match s {
            "cut" => Ok(Transition::Cut),
            "continue" => Ok(Transition::Continue),
            _ => invalid(format!("transition must be cut or continue, not {s:?}")),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect: Option<String>,
    /// Passed to the plugin untouched.
    #[serde(default, skip_serializing_if = "toml::Table::is_empty")]
    pub params: toml::Table,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Shot {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default)]
    pub plot: String,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
    #[serde(default)]
    pub transition: Transition,
    #[serde(default)]
    pub elements: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_take: Option<String>,
    /// Name of a recipe in shotline.toml; without one a take is a single video.generate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<Line>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelOverride>,
    /// Storyboard frame the video starts on, relative to the project root. Not used by
    /// continue shots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<String>,
    /// What the storyboard frame is made from, if not `prompt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_prompt: Option<String>,
    /// Anchors of the elements it names that it doesn't send, relative to the project root.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub leave_out: Vec<String>,
    /// Pictures of its own that go along after the elements' anchors.
    #[serde(default, rename = "image", skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ShotImage>,
    /// What an element's anchor shows in this shot, e.g. "from behind", by its path.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub anchor_notes: BTreeMap<String, String>,
    /// The prompt put into English, sent instead while the prompt is still what it was made from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub english: Option<Translated>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Translated {
    pub from: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ShotImage {
    /// Relative to the project root.
    pub file: String,
    /// What it shows, put in the prompt next to its label.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl Shot {
    /// What the video model is sent: the English, if it's of the prompt as it is now.
    pub fn sent_prompt(&self) -> &str {
        self.translated().unwrap_or(&self.prompt)
    }

    /// The prompt in English, if it was translated as it is now.
    pub fn translated(&self) -> Option<&str> {
        self.english
            .as_ref()
            .filter(|t| t.from == self.prompt)
            .map(|t| t.text.as_str())
    }

    /// The video model (plugin, id) to use instead of the film's; None for the film's.
    pub fn set_video_model(&mut self, m: Option<(String, String)>) {
        let o = self.model.get_or_insert_default();
        (o.plugin, o.id) = m.unzip();
        self.prune_model();
    }

    /// The resolution to use instead of the film's; None for the film's.
    pub fn set_resolution(&mut self, r: Option<String>) {
        self.model.get_or_insert_default().resolution = r;
        self.prune_model();
    }

    fn prune_model(&mut self) {
        if self
            .model
            .as_ref()
            .is_some_and(|m| *m == ModelOverride::default())
        {
            self.model = None;
        }
    }
}

/// Something a cast member says in the shot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub who: String,
    pub text: String,
    /// Seconds into the shot it starts; without it, it follows the line before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<f64>,
    /// The recording a take sends, by job id, while it's of this text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChapterFile {
    title: String,
}

#[derive(Debug, Clone)]
pub struct Chapter {
    pub number: u32,
    pub title: String,
    pub dir: PathBuf,
}

impl Chapter {
    pub fn id(&self) -> String {
        format!("{:02}", self.number)
    }
}

/// A shot's id across the project, shown as `01-010` (chapter 01, shot 010).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShotRef {
    pub chapter: u32,
    pub shot: u32,
}

impl fmt::Display for ShotRef {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:02}-{:03}", self.chapter, self.shot)
    }
}

impl FromStr for ShotRef {
    type Err = Error;

    fn from_str(s: &str) -> Result<ShotRef> {
        let bad = || Error::Invalid(format!("bad shot id {s:?}, expected something like 01-010"));
        let (c, n) = s.split_once(['-', '/']).ok_or_else(bad)?;
        Ok(ShotRef {
            chapter: c.parse().map_err(|_| bad())?,
            shot: n.parse().map_err(|_| bad())?,
        })
    }
}

impl Serialize for ShotRef {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ShotRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<ShotRef, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// Leading number of a folder or file name: "01-into-the-forest" -> 1.
fn leading_number(name: &str) -> Option<u32> {
    let digits: String = name.chars().take_while(|c| c.is_ascii_digit()).collect();
    let rest = &name[digits.len()..];
    if digits.is_empty() || !(rest.is_empty() || rest.starts_with('-')) {
        return None;
    }
    digits.parse().ok()
}

/// Number for a new shot. Appends after the last one, or slots in after `after`
/// halfway to the next one, which is why ids step by 10.
fn next_number(existing: &[u32], after: Option<u32>) -> Result<u32> {
    let last = existing.iter().copied().max().unwrap_or(0);
    let Some(after) = after else {
        return Ok(last + STEP);
    };
    if !existing.contains(&after) {
        return invalid(format!("there's no shot {after:03} in this chapter"));
    }
    match existing.iter().copied().filter(|&n| n > after).min() {
        None => Ok(after + STEP),
        Some(next) if next - after < 2 => invalid(format!(
            "no room between {after:03} and {next:03}, renumber the shots first"
        )),
        Some(next) => Ok(after + (next - after) / 2),
    }
}

impl Project {
    pub fn chapters(&self) -> Result<Vec<Chapter>> {
        let mut out = vec![];
        for dir in fsx::list(&self.root.join("chapters"))? {
            let Some(number) = dir
                .file_name()
                .and_then(|n| leading_number(&n.to_string_lossy()))
            else {
                continue;
            };
            let file = dir.join("chapter.toml");
            if !file.exists() {
                continue;
            }
            let ChapterFile { title } = fsx::read_toml(&file)?;
            out.push(Chapter { number, title, dir });
        }
        out.sort_by_key(|c| c.number);
        Ok(out)
    }

    /// Accepts "1", "01" or the full folder name.
    pub fn chapter(&self, id: &str) -> Result<Chapter> {
        let Some(number) = leading_number(id) else {
            return invalid(format!("bad chapter id {id:?}, expected something like 01"));
        };
        self.chapters()?
            .into_iter()
            .find(|c| c.number == number)
            .ok_or_else(|| Error::Invalid(format!("no chapter {number:02}")))
    }

    pub fn add_chapter(&self, title: &str) -> Result<Chapter> {
        let number = self.chapters()?.last().map_or(1, |c| c.number + 1);
        let slug = slug::slugify(title);
        let name = if slug.is_empty() {
            format!("{number:02}")
        } else {
            format!("{number:02}-{slug}")
        };
        let dir = self.root.join("chapters").join(name);
        fsx::write_toml(
            &dir.join("chapter.toml"),
            &ChapterFile {
                title: title.to_string(),
            },
        )?;
        fsx::mkdir(&dir.join("shots"))?;
        Ok(Chapter {
            number,
            title: title.to_string(),
            dir,
        })
    }

    fn shot_numbers(&self, chapter: &Chapter) -> Result<Vec<u32>> {
        let mut out: Vec<u32> = fsx::list(&chapter.dir.join("shots"))?
            .iter()
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .filter_map(|p| leading_number(&p.file_stem()?.to_string_lossy()))
            .collect();
        out.sort();
        Ok(out)
    }

    pub fn shot_path(&self, r: ShotRef) -> Result<PathBuf> {
        let chapter = self.chapter(&r.chapter.to_string())?;
        Ok(chapter
            .dir
            .join("shots")
            .join(format!("{:03}.toml", r.shot)))
    }

    pub fn shot(&self, r: ShotRef) -> Result<Shot> {
        let path = self.shot_path(r)?;
        if !path.exists() {
            return invalid(format!("no shot {r}"));
        }
        fsx::read_toml(&path)
    }

    pub fn save_shot(&self, r: ShotRef, shot: &Shot) -> Result<()> {
        fsx::write_toml(&self.shot_path(r)?, shot)
    }

    /// Shots of one chapter, or of all chapters, in order.
    pub fn shots(&self, chapter: Option<&str>) -> Result<Vec<(ShotRef, Shot)>> {
        let chapters = match chapter {
            Some(id) => vec![self.chapter(id)?],
            None => self.chapters()?,
        };
        let mut out = vec![];
        for c in &chapters {
            for n in self.shot_numbers(c)? {
                let r = ShotRef {
                    chapter: c.number,
                    shot: n,
                };
                out.push((r, self.shot(r)?));
            }
        }
        Ok(out)
    }

    pub fn add_line(&self, r: ShotRef, who: &str, text: &str, at: Option<f64>) -> Result<Shot> {
        let line = self.line(who, text, at)?;
        let mut shot = self.shot(r)?;
        shot.lines.push(line);
        self.save_shot(r, &shot)?;
        Ok(shot)
    }

    /// Replaces line `i` (from 0).
    pub fn set_line(
        &self,
        r: ShotRef,
        i: usize,
        who: &str,
        text: &str,
        at: Option<f64>,
    ) -> Result<Shot> {
        let line = self.line(who, text, at)?;
        let mut shot = self.shot(r)?;
        match shot.lines.get_mut(i) {
            // The same words by the same speaker: the recording still fits.
            Some(l) if l.text == line.text && l.who == line.who => {
                *l = Line {
                    audio: l.audio.take(),
                    ..line
                }
            }
            Some(l) => *l = line,
            None => return invalid(format!("{r} has no line {}", i + 1)),
        }
        self.save_shot(r, &shot)?;
        Ok(shot)
    }

    /// Gives the shot a storyboard frame copied from `from`, next to its file
    /// (chapters/01-x/shots/010.png), or with None takes it away. Returns the new one.
    pub fn set_frame(&self, r: ShotRef, from: Option<&Path>) -> Result<Option<PathBuf>> {
        let mut shot = self.shot(r)?;
        if let Some(f) = from
            && !crate::element::is_image(f)
        {
            return invalid(format!("{} isn't an image", f.display()));
        }
        if let Some(old) = shot.frame.take() {
            let _ = std::fs::remove_file(self.root.join(old));
        }
        let dest = match from {
            Some(f) => {
                let ext = f
                    .extension()
                    .unwrap()
                    .to_string_lossy()
                    .to_ascii_lowercase();
                let dest = self.shot_path(r)?.with_extension(ext);
                fsx::copy(f, &dest)?;
                shot.frame = Some(self.rel(&dest));
                Some(dest)
            }
            None => None,
        };
        self.save_shot(r, &shot)?;
        Ok(dest)
    }

    /// Where a shot's own images are kept: chapters/01-x/shots/010/.
    pub fn shot_image_dir(&self, r: ShotRef) -> Result<PathBuf> {
        Ok(self.shot_path(r)?.with_extension(""))
    }

    /// Copies an image in as the shot's next one.
    pub fn add_shot_image(&self, r: ShotRef, file: &Path, note: &str) -> Result<PathBuf> {
        let mut shot = self.shot(r)?;
        if !crate::element::is_image(file) {
            return invalid(format!(
                "{} isn't an image ({})",
                file.display(),
                crate::element::IMAGE_EXTS.join(", ")
            ));
        }
        let ext = file
            .extension()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        let dir = self.shot_image_dir(r)?;
        // max + 1 so removing 01 doesn't make the next one overwrite 02
        let n = fsx::list(&dir)?
            .iter()
            .filter_map(|p| p.file_stem()?.to_str()?.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let dest = dir.join(format!("{n:02}.{ext}"));
        fsx::copy(file, &dest)?;
        shot.images.push(ShotImage {
            file: self.rel(&dest),
            note: note.trim().to_string(),
        });
        self.save_shot(r, &shot)?;
        Ok(dest)
    }

    /// Takes image `i` (from 0) off the shot, and deletes it if it's in the shot's folder.
    pub fn remove_shot_image(&self, r: ShotRef, i: usize) -> Result<ShotImage> {
        let mut shot = self.shot(r)?;
        if i >= shot.images.len() {
            return invalid(format!("{r} has no image {}", i + 1));
        }
        let img = shot.images.remove(i);
        self.save_shot(r, &shot)?;
        let path = self.root.join(&img.file);
        if path.starts_with(self.shot_image_dir(r)?) {
            let _ = std::fs::remove_file(&path);
        }
        Ok(img)
    }

    pub fn set_shot_image_note(&self, r: ShotRef, i: usize, note: &str) -> Result<()> {
        let mut shot = self.shot(r)?;
        let Some(img) = shot.images.get_mut(i) else {
            return invalid(format!("{r} has no image {}", i + 1));
        };
        img.note = note.trim().to_string();
        self.save_shot(r, &shot)
    }

    pub fn remove_line(&self, r: ShotRef, i: usize) -> Result<Shot> {
        let mut shot = self.shot(r)?;
        if i >= shot.lines.len() {
            return invalid(format!("{r} has no line {}", i + 1));
        }
        shot.lines.remove(i);
        self.save_shot(r, &shot)?;
        Ok(shot)
    }

    fn line(&self, who: &str, text: &str, at: Option<f64>) -> Result<Line> {
        let who = who.trim_start_matches('@');
        let el = self.element(who)?;
        if el.kind != crate::Kind::Cast {
            return invalid(format!("@{who} is a {}, only cast speak", el.kind));
        }
        if text.trim().is_empty() {
            return invalid("the line is empty");
        }
        if at.is_some_and(|a| a.is_nan() || a < 0.0) {
            return invalid("a line starts at 0 seconds or later");
        }
        Ok(Line {
            who: who.to_string(),
            text: text.to_string(),
            at,
            audio: None,
        })
    }

    /// Every recording @`who` has, oldest first, with the ones of shot `first` ahead of the rest.
    /// Recordings from before jobs said who speaks are matched by their words.
    pub fn recordings_by(&self, who: &str, first: Option<ShotRef>) -> Result<Vec<Job>> {
        let who = who.trim_start_matches('@');
        let mut shots: HashMap<ShotRef, Option<Shot>> = HashMap::new();
        let mut out = vec![];
        for j in self.jobs()? {
            if j.op != Op::AudioTts
                || j.status != JobStatus::Done
                || j.discarded
                || j.outputs.is_empty()
            {
                continue;
            }
            let by = match (&j.who, j.shot) {
                (Some(w), _) => Some(w.clone()),
                (None, Some(r)) => shots
                    .entry(r)
                    .or_insert_with(|| self.shot(r).ok())
                    .as_ref()
                    .and_then(|s| {
                        s.lines
                            .iter()
                            .find(|l| Some(&l.text) == j.prompt.as_ref())
                            .map(|l| l.who.clone())
                    }),
                (None, None) => None,
            };
            if by.as_deref() == Some(who) {
                out.push(j);
            }
        }
        out.sort_by_key(|j| (j.shot != first || first.is_none(), j.shot, j.id));
        Ok(out)
    }

    /// The recording line `i` sends with a take, if one's picked and still kept.
    pub fn line_audio(&self, r: ShotRef, i: usize) -> Result<Option<Job>> {
        let shot = self.shot(r)?;
        let Some(id) = shot.lines.get(i).and_then(|l| l.audio.clone()) else {
            return Ok(None);
        };
        let Ok(j) = self.job(&id) else {
            return Ok(None);
        };
        let kept = j.op == Op::AudioTts
            && j.status == JobStatus::Done
            && !j.discarded
            && !j.outputs.is_empty();
        Ok(kept.then_some(j))
    }

    /// Picks the recording line `i` sends, or with None leaves it to be made with the take.
    pub fn set_line_audio(&self, r: ShotRef, i: usize, job: Option<&Job>) -> Result<Shot> {
        let mut shot = self.shot(r)?;
        let Some(line) = shot.lines.get_mut(i) else {
            return invalid(format!("{r} has no line {}", i + 1));
        };
        line.audio = job.map(|j| j.id.to_string());
        // The line says what its recording says.
        if let Some(text) = job.and_then(|j| j.prompt.clone()) {
            line.text = text;
        }
        self.save_shot(r, &shot)?;
        Ok(shot)
    }

    /// Moves line `from` to `to` (both from 0). Start times stay as they were.
    pub fn move_line(&self, r: ShotRef, from: usize, to: usize) -> Result<Shot> {
        let mut shot = self.shot(r)?;
        let n = shot.lines.len();
        if from >= n || to >= n {
            return invalid(format!("{r} has {n} lines"));
        }
        let line = shot.lines.remove(from);
        shot.lines.insert(to, line);
        self.save_shot(r, &shot)?;
        Ok(shot)
    }

    /// Throws a recording out: its folder goes, unless a take was made with it, which keeps it
    /// marked as thrown out.
    pub fn discard_recording(&self, id: &str) -> Result<()> {
        let j = self.job(id)?;
        if j.op != Op::AudioTts {
            return invalid(format!("{} isn't a recording", j.id));
        }
        if j.take.is_none() {
            fsx::remove_dir(&self.job_dir(j.id))?;
        } else {
            self.set_discarded(j.id, true)?;
        }
        Ok(())
    }

    /// The shot before `r` in film order, crossing chapter boundaries.
    pub fn previous_shot(&self, r: ShotRef) -> Result<Option<ShotRef>> {
        Ok(self
            .shots(None)?
            .into_iter()
            .map(|(r, _)| r)
            .take_while(|&x| x != r)
            .last())
    }

    /// Adds a shot to a chapter. Handles mentioned in the prompt are added to `elements`
    /// and every element has to exist.
    pub fn add_shot(&self, chapter: &str, after: Option<u32>, mut shot: Shot) -> Result<ShotRef> {
        let chapter = self.chapter(chapter)?;
        for h in handle::unique(&shot.prompt) {
            if !shot.elements.contains(&h) {
                shot.elements.push(h);
            }
        }
        for h in &shot.elements {
            self.element(h)?;
        }
        let r = ShotRef {
            chapter: chapter.number,
            shot: next_number(&self.shot_numbers(&chapter)?, after)?,
        };
        self.save_shot(r, &shot)?;
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_by_ten() {
        assert_eq!(next_number(&[], None).unwrap(), 10);
        assert_eq!(next_number(&[10, 20], None).unwrap(), 30);
    }

    #[test]
    fn inserts_halfway() {
        assert_eq!(next_number(&[10, 20], Some(10)).unwrap(), 15);
        assert_eq!(next_number(&[10, 15, 20], Some(10)).unwrap(), 12);
        assert_eq!(next_number(&[10, 20], Some(20)).unwrap(), 30);
    }

    #[test]
    fn insert_errors() {
        assert!(next_number(&[10, 11], Some(10)).is_err());
        assert!(next_number(&[10], Some(5)).is_err());
    }

    #[test]
    fn parses_shot_refs() {
        let r: ShotRef = "01-010".parse().unwrap();
        assert_eq!(
            r,
            ShotRef {
                chapter: 1,
                shot: 10
            }
        );
        assert_eq!("2/5".parse::<ShotRef>().unwrap().to_string(), "02-005");
        assert!("010".parse::<ShotRef>().is_err());
        assert!("a-b".parse::<ShotRef>().is_err());
    }

    #[test]
    fn leading_numbers() {
        assert_eq!(leading_number("01-into-the-forest"), Some(1));
        assert_eq!(leading_number("010"), Some(10));
        assert_eq!(leading_number("01x"), None);
        assert_eq!(leading_number("notes"), None);
    }

    #[test]
    fn shot_toml_matches_the_design() {
        let shot: Shot = toml::from_str(
            r#"
title = "Rest stop"
plot = "พักเท้ากลางทาง"
prompt = "@mali films"
duration = 8
transition = "continue"
elements = ["mali"]

[model]
plugin = "fal"

[model.params]
prompt_expansion_mode = "disabled"
"#,
        )
        .unwrap();
        assert_eq!(shot.duration, Some(8.0));
        assert_eq!(shot.transition, Transition::Continue);
        let m = shot.model.unwrap();
        assert_eq!(m.plugin.as_deref(), Some("fal"));
        assert_eq!(m.params["prompt_expansion_mode"].as_str(), Some("disabled"));
    }
}
