//! Anchor images and storyboard frames.

use std::path::Path;

use anyhow::{Context, Result, bail};
use shotline_core::protocol::{MediaKind, Op, Reference, Request};
use shotline_core::{BuiltImage, ImageFor, Job, Project, ShotRef, build_image};

use crate::plugins;
use crate::shoot::{Go, job_cost, quote_once, spend_once};

pub struct Make<'a> {
    pub prompt: &'a str,
    pub plugin: Option<&'a str>,
    pub model: Option<&'a str>,
    pub yes: bool,
    /// The price agreed to in the app.
    pub at: Option<&'a str>,
    /// Anchors only: the images to go along instead of the element's own.
    pub refs: Option<Vec<std::path::PathBuf>>,
}

impl Make<'_> {
    fn go(&self) -> Go<'_> {
        match self.at {
            Some(c) => Go::At(c),
            None => self.yes.into(),
        }
    }
}

fn request(p: &Project, what: ImageFor, m: &Make) -> Result<(String, Request)> {
    let installed = plugins::load();
    let catalog = plugins::catalog(p, &installed)?;
    let b: BuiltImage = build_image(p, &catalog, &what, m.plugin, m.model)?;
    for w in &b.warnings {
        eprintln!("warning: {w}");
    }
    let req = Request {
        op: Op::ImageGenerate,
        model: b.model.clone(),
        prompt: Some(b.text.clone()),
        resolution: b.resolution.clone(),
        aspect: Some(b.aspect.clone()),
        references: b
            .references
            .iter()
            .map(|r| Reference {
                kind: MediaKind::Image,
                path: p.root.join(&r.path),
            })
            .collect(),
        ..Request::default()
    };
    Ok((b.plugin, req))
}

fn make(
    p: &Project,
    what: ImageFor,
    label: &str,
    shot: Option<ShotRef>,
    m: &Make,
) -> Result<Option<(Job, std::path::PathBuf)>> {
    let (plugin, req) = request(p, what, m)?;
    let Some(job) = spend_once(p, &plugin, &req, shot, label, m.go())? else {
        return Ok(None);
    };
    let file = job
        .outputs
        .first()
        .with_context(|| format!("job {} made no image", job.id))?;
    let out = p.job_dir(job.id).join(file);
    Ok(Some((job, out)))
}

fn anchor_for(p: &Project, handle: &str, m: &Make) -> Result<(String, ImageFor)> {
    let el = p.element(handle)?;
    let what = ImageFor::Anchor {
        handle: el.handle.clone(),
        prompt: m.prompt.to_string(),
        refs: m.refs.clone(),
    };
    Ok((el.handle, what))
}

/// The images an anchor is made from, when it isn't just the element's own: those (unless left
/// out), then the files and URLs given.
pub fn picked(
    p: &Project,
    handle: &str,
    refs: &[String],
    no_anchors: bool,
) -> Result<Option<Vec<std::path::PathBuf>>> {
    if refs.is_empty() && !no_anchors {
        return Ok(None);
    }
    let mut out = if no_anchors {
        vec![]
    } else {
        p.anchors(&p.element(handle)?)?
    };
    for r in refs {
        if crate::fetch::is_url(r) {
            out.push(crate::fetch::image(p, r)?);
        } else {
            let f = std::env::current_dir()?.join(r);
            if !f.is_file() {
                bail!("{r} doesn't exist");
            }
            out.push(f);
        }
    }
    Ok(Some(out))
}

/// What a new anchor image would cost.
pub fn anchor_quote(p: &Project, handle: &str, m: &Make) -> Result<String> {
    let (plugin, req) = request(p, anchor_for(p, handle, m)?.1, m)?;
    quote_once(p, &plugin, &req)
}

/// Makes an image for an element, left in its job's folder. None if nothing was spent.
pub fn anchor_image(
    p: &Project,
    handle: &str,
    m: &Make,
) -> Result<Option<(Job, std::path::PathBuf)>> {
    let (handle, what) = anchor_for(p, handle, m)?;
    make(p, what, &format!("anchor for @{handle}"), None, m)
}

/// Makes a new anchor image for an element.
pub fn anchor(p: &Project, handle: &str, m: &Make) -> Result<()> {
    let Some((job, out)) = anchor_image(p, handle, m)? else {
        return Ok(());
    };
    let dest = p.add_anchor(handle, &out)?;
    println!("{} · {} · job {}", p.rel(&dest), job_cost(&job), job.id);
    Ok(())
}

/// Next to the shot's file: chapters/01-x/shots/010.png.
fn frame_path(p: &Project, r: ShotRef, from: &Path) -> Result<std::path::PathBuf> {
    let ext = from
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_ascii_lowercase();
    Ok(p.shot_path(r)?.with_extension(ext))
}

fn set_frame(p: &Project, r: ShotRef, from: &Path) -> Result<String> {
    let mut shot = p.shot(r)?;
    if let Some(old) = &shot.frame {
        let _ = std::fs::remove_file(p.root.join(old));
    }
    let dest = frame_path(p, r, from)?;
    std::fs::copy(from, &dest).with_context(|| format!("copying {}", from.display()))?;
    shot.frame = Some(p.rel(&dest));
    p.save_shot(r, &shot)?;
    Ok(p.rel(&dest))
}

pub enum Frame<'a> {
    Make(Make<'a>),
    File(&'a Path),
    Clear,
}

/// Gives a shot a storyboard frame to start its video on.
pub fn frame(p: &Project, r: ShotRef, how: Frame) -> Result<()> {
    match how {
        Frame::Make(m) => {
            let Some((job, out)) = make(
                p,
                ImageFor::Frame(r),
                &format!("storyboard frame for {r}"),
                Some(r),
                &m,
            )?
            else {
                return Ok(());
            };
            let rel = set_frame(p, r, &out)?;
            println!("{rel} · {} · job {}", job_cost(&job), job.id);
        }
        Frame::File(f) => {
            let full = std::env::current_dir()?.join(f);
            if !full.is_file() {
                bail!("{} doesn't exist", f.display());
            }
            println!("{}", set_frame(p, r, &full)?);
        }
        Frame::Clear => {
            let mut shot = p.shot(r)?;
            match shot.frame.take() {
                Some(old) => {
                    let _ = std::fs::remove_file(p.root.join(&old));
                    p.save_shot(r, &shot)?;
                    println!("{r} has no storyboard frame now");
                }
                None => println!("{r} has no storyboard frame"),
            }
        }
    }
    Ok(())
}
