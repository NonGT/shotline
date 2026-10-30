//! Anchor images and storyboard frames.

use std::path::Path;

use anyhow::{Context, Result, bail};
use shotline_core::protocol::{MediaKind, Op, Reference, Request};
use shotline_core::{BuiltImage, ImageFor, Job, Project, ShotRef, build_image};

use crate::plugins;
use crate::shoot::{job_cost, spend_once};

pub struct Make<'a> {
    pub prompt: &'a str,
    pub plugin: Option<&'a str>,
    pub model: Option<&'a str>,
    pub yes: bool,
}

fn make(
    p: &Project,
    what: ImageFor,
    label: &str,
    shot: Option<ShotRef>,
    m: &Make,
) -> Result<Option<(Job, std::path::PathBuf)>> {
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
    let Some(job) = spend_once(p, &b.plugin, &req, shot, label, m.yes)? else {
        return Ok(None);
    };
    let file = job
        .outputs
        .first()
        .with_context(|| format!("job {} made no image", job.id))?;
    let out = p.job_dir(job.id).join(file);
    Ok(Some((job, out)))
}

/// Makes a new anchor image for an element.
pub fn anchor(p: &Project, handle: &str, m: &Make) -> Result<()> {
    let el = p.element(handle)?;
    let what = ImageFor::Anchor {
        handle: el.handle.clone(),
        prompt: m.prompt.to_string(),
    };
    let Some((job, out)) = make(p, what, &format!("anchor for @{}", el.handle), None, m)? else {
        return Ok(());
    };
    let dest = p.add_anchor(&el.handle, &out)?;
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
