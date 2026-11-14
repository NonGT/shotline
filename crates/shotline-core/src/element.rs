use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::invalid;
use crate::{Error, Project, Result, ShotRef, fsx, handle, slug};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Cast,
    Set,
    Prop,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Cast, Kind::Set, Kind::Prop];

    pub fn dir(self) -> &'static str {
        match self {
            Kind::Cast => "cast",
            Kind::Set => "sets",
            Kind::Prop => "props",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.pad(match self {
            Kind::Cast => "cast",
            Kind::Set => "set",
            Kind::Prop => "prop",
        })
    }
}

impl FromStr for Kind {
    type Err = Error;

    fn from_str(s: &str) -> Result<Kind> {
        match s {
            "cast" | "character" => Ok(Kind::Cast),
            "set" | "sets" => Ok(Kind::Set),
            "prop" | "props" => Ok(Kind::Prop),
            _ => invalid(format!("unknown element kind {s:?}, use cast, set or prop")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Element {
    pub name: String,
    pub handle: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<Voice>,
    /// A few words on each anchor, by file name, to tell them apart when picking one.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(skip, default = "default_kind")]
    pub kind: Kind,
}

impl Element {
    pub fn label(&self, anchor: &Path) -> Option<&str> {
        let name = anchor.file_name()?.to_str()?;
        self.labels.get(name).map(String::as_str)
    }
}

/// How a cast member sounds: which TTS plugin and voice to use for their lines.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Voice {
    pub plugin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_id: Option<String>,
    /// Passed to the plugin as is, e.g. stability.
    #[serde(default, skip_serializing_if = "toml::Table::is_empty")]
    pub settings: toml::Table,
}

fn default_kind() -> Kind {
    Kind::Cast
}

pub(crate) const IMAGE_EXTS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

pub(crate) fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

impl Project {
    pub fn element_file(&self, kind: Kind, handle: &str) -> PathBuf {
        self.root
            .join("elements")
            .join(kind.dir())
            .join(format!("{handle}.toml"))
    }

    /// Folder holding an element's anchor images.
    pub fn anchor_dir(&self, el: &Element) -> PathBuf {
        self.root
            .join("elements")
            .join(el.kind.dir())
            .join(&el.handle)
    }

    pub fn add_element(
        &self,
        kind: Kind,
        name: &str,
        handle: Option<&str>,
        description: &str,
    ) -> Result<Element> {
        let handle = match handle {
            Some(h) => h.trim_start_matches('@').to_string(),
            None => slug::slugify(name),
        };
        if handle.is_empty() {
            return invalid(format!(
                "can't make a handle from {name:?}, give one with --handle"
            ));
        }
        if !handle::is_valid(&handle) {
            return invalid(format!(
                "bad handle {handle:?}: use lowercase a-z, 0-9, - and _"
            ));
        }
        if let Some(existing) = self.find_element(&handle)? {
            return invalid(format!("@{handle} is already used by {}", existing.name));
        }
        let el = Element {
            name: name.to_string(),
            handle,
            description: description.to_string(),
            voice: None,
            labels: BTreeMap::new(),
            kind,
        };
        fsx::write_toml(&self.element_file(kind, &el.handle), &el)?;
        Ok(el)
    }

    pub fn elements(&self) -> Result<Vec<Element>> {
        let mut out = vec![];
        for kind in Kind::ALL {
            for path in fsx::list(&self.root.join("elements").join(kind.dir()))? {
                if path.extension().is_some_and(|e| e == "toml") {
                    let mut el: Element = fsx::read_toml(&path)?;
                    el.kind = kind;
                    out.push(el);
                }
            }
        }
        Ok(out)
    }

    pub fn find_element(&self, handle: &str) -> Result<Option<Element>> {
        let handle = handle.trim_start_matches('@');
        Ok(self.elements()?.into_iter().find(|e| e.handle == handle))
    }

    pub fn element(&self, handle: &str) -> Result<Element> {
        match self.find_element(handle)? {
            Some(el) => Ok(el),
            None => invalid(format!(
                "unknown element @{}",
                handle.trim_start_matches('@')
            )),
        }
    }

    pub fn set_voice(&self, handle: &str, voice: Voice) -> Result<Element> {
        let mut el = self.element(handle)?;
        if el.kind != Kind::Cast {
            return invalid(format!(
                "@{} is a {}, only cast have voices",
                el.handle, el.kind
            ));
        }
        el.voice = Some(voice);
        fsx::write_toml(&self.element_file(el.kind, &el.handle), &el)?;
        Ok(el)
    }

    /// Writes an element as given, under its kind and handle.
    pub fn save_element(&self, el: &Element) -> Result<()> {
        if el.voice.is_some() && el.kind != Kind::Cast {
            return invalid(format!(
                "@{} is a {}, only cast have voices",
                el.handle, el.kind
            ));
        }
        fsx::write_toml(&self.element_file(el.kind, &el.handle), el)
    }

    pub fn set_description(&self, handle: &str, description: &str) -> Result<Element> {
        let mut el = self.element(handle)?;
        el.description = description.to_string();
        fsx::write_toml(&self.element_file(el.kind, &el.handle), &el)?;
        Ok(el)
    }

    pub fn anchors(&self, el: &Element) -> Result<Vec<PathBuf>> {
        Ok(fsx::list(&self.anchor_dir(el))?
            .into_iter()
            .filter(|p| is_image(p))
            .collect())
    }

    /// Copies an image in as the element's next anchor (01.png, 02.jpg, ...).
    pub fn add_anchor(&self, handle: &str, file: &Path) -> Result<PathBuf> {
        let el = self.element(handle)?;
        if !is_image(file) {
            return invalid(format!(
                "{} isn't an image ({})",
                file.display(),
                IMAGE_EXTS.join(", ")
            ));
        }
        let ext = file
            .extension()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        // max + 1 rather than count + 1 so deleting 01 doesn't make the next one overwrite 02
        let n = self
            .anchors(&el)?
            .iter()
            .filter_map(|p| p.file_stem()?.to_str()?.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let dest = self.anchor_dir(&el).join(format!("{n:02}.{ext}"));
        fsx::copy(file, &dest)?;
        Ok(dest)
    }

    /// Deletes an anchor and its label.
    pub fn remove_anchor(&self, handle: &str, anchor: &Path) -> Result<()> {
        let mut el = self.element(handle)?;
        fsx::remove(anchor)?;
        if let Some(name) = anchor.file_name().and_then(|n| n.to_str())
            && el.labels.remove(name).is_some()
        {
            self.save_element(&el)?;
        }
        Ok(())
    }

    /// Shots that send the element or give it lines.
    pub fn shots_naming(&self, handle: &str) -> Result<Vec<ShotRef>> {
        let handle = handle.trim_start_matches('@');
        Ok(self
            .shots(None)?
            .into_iter()
            .filter(|(_, s)| {
                s.elements.iter().any(|h| h == handle) || s.lines.iter().any(|l| l.who == handle)
            })
            .map(|(r, _)| r)
            .collect())
    }

    /// Deletes an element with its anchors and takes it off the shots that send it. Lines it
    /// speaks stay, without a voice.
    pub fn remove_element(&self, handle: &str) -> Result<Element> {
        let el = self.element(handle)?;
        let dir = self.anchor_dir(&el);
        let under = format!("{}/", self.rel(&dir));
        for (r, mut s) in self.shots(None)? {
            let was = s.clone();
            s.elements.retain(|h| *h != el.handle);
            s.leave_out.retain(|p| !p.starts_with(&under));
            s.anchor_notes.retain(|p, _| !p.starts_with(&under));
            if s != was {
                self.save_shot(r, &s)?;
            }
        }
        if dir.exists() {
            fsx::remove_dir(&dir)?;
        }
        fsx::remove(&self.element_file(el.kind, &el.handle))?;
        Ok(el)
    }

    /// Moves an element to another kind, anchors and all. Only cast keep a voice.
    pub fn set_kind(&self, handle: &str, kind: Kind) -> Result<Element> {
        let mut el = self.element(handle)?;
        if el.kind == kind {
            return Ok(el);
        }
        let (file, dir) = (self.element_file(el.kind, &el.handle), self.anchor_dir(&el));
        let from = format!("{}/", self.rel(&dir));
        el.kind = kind;
        if kind != Kind::Cast {
            el.voice = None;
        }
        let new_dir = self.anchor_dir(&el);
        let to = format!("{}/", self.rel(&new_dir));
        if dir.exists() {
            fsx::rename(&dir, &new_dir)?;
        }
        self.save_element(&el)?;
        fsx::remove(&file)?;
        self.repoint_anchors(|p| p.strip_prefix(&from).map(|rest| format!("{to}{rest}")))?;
        Ok(el)
    }

    /// Swaps anchors `i` and `j`, which sets which is Image 1 in prompts. Labels and what
    /// shots say about them go along.
    pub fn swap_anchors(&self, handle: &str, i: usize, j: usize) -> Result<()> {
        let mut el = self.element(handle)?;
        let [(a, a_to), (b, b_to)] = self.anchor_swap(&el, i, j)?;
        if i == j {
            return Ok(());
        }
        let (a, b) = (&a, &b);
        let held = a.with_extension("swapping");
        fsx::rename(a, &held)?;
        fsx::rename(b, &b_to)?;
        fsx::rename(&held, &a_to)?;
        let name = |p: &Path| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        };
        let (la, lb) = (el.labels.remove(&name(a)), el.labels.remove(&name(b)));
        if let Some(l) = la {
            el.labels.insert(name(&a_to), l);
        }
        if let Some(l) = lb {
            el.labels.insert(name(&b_to), l);
        }
        self.save_element(&el)?;
        let moves = [
            (self.rel(a), self.rel(&a_to)),
            (self.rel(b), self.rel(&b_to)),
        ];
        self.repoint_anchors(|p| moves.iter().find(|(f, _)| f == p).map(|(_, t)| t.clone()))
    }

    /// Where anchors `i` and `j` go when swapped: each takes the other's number, keeping its
    /// own extension.
    pub fn anchor_swap(&self, el: &Element, i: usize, j: usize) -> Result<[(PathBuf, PathBuf); 2]> {
        let list = self.anchors(el)?;
        let (Some(a), Some(b)) = (list.get(i), list.get(j)) else {
            return invalid(format!("@{} has {} anchors", el.handle, list.len()));
        };
        let renamed = |x: &Path, y: &Path| {
            let ext = x.extension().unwrap_or_default().to_string_lossy();
            let stem = y.file_stem().unwrap_or_default().to_string_lossy();
            x.with_file_name(format!("{stem}.{ext}"))
        };
        let (a_to, b_to) = (renamed(a, b), renamed(b, a));
        for t in [&a_to, &b_to] {
            if t.exists() && t != a && t != b {
                return invalid(format!("{} is in the way", self.rel(t)));
            }
        }
        Ok([(a.clone(), a_to), (b.clone(), b_to)])
    }

    /// Anchor paths shots keep, left out or with a note, moved where `to` says.
    fn repoint_anchors(&self, to: impl Fn(&str) -> Option<String>) -> Result<()> {
        for (r, mut s) in self.shots(None)? {
            let was = s.clone();
            for p in &mut s.leave_out {
                if let Some(n) = to(p) {
                    *p = n;
                }
            }
            s.anchor_notes = std::mem::take(&mut s.anchor_notes)
                .into_iter()
                .map(|(p, n)| (to(&p).unwrap_or(p), n))
                .collect();
            if s != was {
                self.save_shot(r, &s)?;
            }
        }
        Ok(())
    }
}
