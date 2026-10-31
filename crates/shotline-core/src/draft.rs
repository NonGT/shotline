//! English drafts of Thai writing, made by a text model: a shot's prompt from its plot, or a
//! straight translation.

use std::fmt;
use std::str::FromStr;

use shotline_protocol::Op;

use crate::error::invalid;
use crate::template::render;
use crate::{Catalog, Error, Project, Result, ShotRef, handle};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Draft {
    /// A video prompt: action, camera, light.
    #[default]
    Video,
    /// A still, for a storyboard frame.
    Image,
    /// Word for word, nothing added.
    Translate,
}

impl Draft {
    fn template(self) -> &'static str {
        match self {
            Draft::Video => "draft-video",
            Draft::Image => "draft-image",
            Draft::Translate => "translate",
        }
    }
}

impl fmt::Display for Draft {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.pad(match self {
            Draft::Video => "video",
            Draft::Image => "image",
            Draft::Translate => "translate",
        })
    }
}

impl FromStr for Draft {
    type Err = Error;

    fn from_str(s: &str) -> Result<Draft> {
        match s {
            "video" => Ok(Draft::Video),
            "image" => Ok(Draft::Image),
            "translate" => Ok(Draft::Translate),
            _ => invalid(format!("a draft is video, image or translate, not {s:?}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DraftFor {
    /// A shot's prompt, from its plot.
    Shot(ShotRef, Draft),
    /// An element's description, translated.
    Element(String),
}

/// A text request, worked out without spending anything.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltDraft {
    pub plugin: String,
    pub model: String,
    pub text: String,
}

/// The request for a draft, on the project's text model unless `plugin`/`model` say otherwise.
pub fn build_draft(
    project: &Project,
    catalog: &Catalog,
    what: &DraftFor,
    plugin: Option<&str>,
    model: Option<&str>,
) -> Result<BuiltDraft> {
    let cfg = &project.config;
    let plugin = plugin.unwrap_or(&cfg.text.plugin).to_string();
    let model = model.unwrap_or(&cfg.text.model).to_string();
    catalog.get(&plugin, &model, &Op::TextGenerate)?;
    let text = match what {
        DraftFor::Shot(r, how) => {
            let shot = project.shot(*r)?;
            let plot = shot.plot.trim();
            if plot.is_empty() {
                return invalid(format!("{r} has no plot to draft from"));
            }
            let t = project.template(how.template())?.unwrap_or_default();
            if *how == Draft::Translate {
                render(&t, &[("text", plot)])
            } else {
                let elements: Vec<String> = project
                    .elements()?
                    .iter()
                    .map(|e| {
                        let mut s = format!("@{}: {} ({})", e.handle, e.name, e.kind);
                        if !e.description.is_empty() {
                            s += &format!(", {}", e.description);
                        }
                        s
                    })
                    .collect();
                let mut speakers: Vec<String> = vec![];
                for l in &shot.lines {
                    let who = format!("@{}", l.who.trim_start_matches('@'));
                    if !speakers.contains(&who) {
                        speakers.push(who);
                    }
                }
                let elements = if elements.is_empty() {
                    "none".to_string()
                } else {
                    elements.join("\n")
                };
                let speakers = if speakers.is_empty() {
                    "nobody".to_string()
                } else {
                    speakers.join(", ")
                };
                let prompt = match shot.prompt.trim() {
                    "" => "none",
                    p => p,
                };
                let duration = shot.duration.unwrap_or(cfg.duration).to_string();
                render(
                    &t,
                    &[
                        ("elements", &elements),
                        ("speakers", &speakers),
                        ("prompt", prompt),
                        ("duration", &duration),
                        ("plot", plot),
                    ],
                )
            }
        }
        DraftFor::Element(h) => {
            let el = project.element(h)?;
            let desc = el.description.trim();
            if desc.is_empty() {
                return invalid(format!("@{} has no description to translate", el.handle));
            }
            let t = project.template("translate")?.unwrap_or_default();
            render(&t, &[("text", desc)])
        }
    };
    Ok(BuiltDraft {
        plugin,
        model,
        text,
    })
}

/// Tidies what the model sent back and makes sure every @handle in it is an element.
pub fn finish_draft(project: &Project, raw: &str) -> Result<String> {
    let mut s = raw.trim();
    if let Some(rest) = s.strip_prefix("```") {
        // Drop the fence line, which may name a language.
        s = rest.split_once('\n').map_or("", |(_, r)| r);
        s = s.trim_end().strip_suffix("```").unwrap_or(s).trim();
    }
    if s.len() > 1 && s.starts_with('"') && s.ends_with('"') {
        s = s[1..s.len() - 1].trim();
    }
    if s.is_empty() {
        return invalid("the model sent back nothing");
    }
    for h in handle::unique(s) {
        if project.find_element(&h)?.is_none() {
            return invalid(format!("the draft uses @{h}, which isn't an element"));
        }
    }
    Ok(s.to_string())
}
