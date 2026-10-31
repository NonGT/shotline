use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::invalid;
use crate::{Error, Project, Result, fsx, handle, slug};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Element {
    pub name: String,
    pub handle: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<Voice>,
    #[serde(skip, default = "default_kind")]
    pub kind: Kind,
}

/// How a cast member sounds: which TTS plugin and voice to use for their lines.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Voice {
    pub plugin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_id: Option<String>,
    /// A recording to clone from, for plugins that do that. Relative to the project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<String>,
    /// Passed to the plugin as is, e.g. stability.
    #[serde(default, skip_serializing_if = "toml::Table::is_empty")]
    pub settings: toml::Table,
}

fn default_kind() -> Kind {
    Kind::Cast
}

const IMAGE_EXTS: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

impl Project {
    fn element_file(&self, kind: Kind, handle: &str) -> PathBuf {
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
}
