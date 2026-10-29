use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::invalid;
use crate::{Error, Project, Result, fsx, handle, slug};

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
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelOverride>,
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
provider = "fal"
"#,
        )
        .unwrap();
        assert_eq!(shot.duration, Some(8.0));
        assert_eq!(shot.transition, Transition::Continue);
        assert_eq!(shot.model.unwrap().provider.as_deref(), Some("fal"));
    }
}
