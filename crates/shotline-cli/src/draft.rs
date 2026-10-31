//! English prompts and descriptions drafted by the text model.

use anyhow::{Context, Result};
use shotline_core::protocol::{Op, Request};
use shotline_core::{Draft, DraftFor, Project, ShotRef, build_draft, finish_draft};

use crate::plugins;
use crate::shoot::{confirm, job_cost, spend_once};

pub struct Ask<'a> {
    pub plugin: Option<&'a str>,
    pub model: Option<&'a str>,
    pub yes: bool,
    pub write: bool,
}

impl Ask<'_> {
    /// Without --write it asks, unless --yes says nobody is there to answer.
    fn keep(&self, question: &str) -> Result<bool> {
        Ok(self.write || (!self.yes && confirm(question)?))
    }
}

fn make(
    p: &Project,
    what: DraftFor,
    label: &str,
    shot: Option<ShotRef>,
    a: &Ask,
) -> Result<Option<String>> {
    let installed = plugins::load();
    let catalog = plugins::catalog(p, &installed)?;
    let b = build_draft(p, &catalog, &what, a.plugin, a.model)?;
    let req = Request {
        op: Op::TextGenerate,
        model: b.model.clone(),
        prompt: Some(b.text),
        ..Request::default()
    };
    let Some(job) = spend_once(p, &b.plugin, &req, shot, label, a.yes)? else {
        return Ok(None);
    };
    let file = job
        .outputs
        .first()
        .with_context(|| format!("job {} sent back no text", job.id))?;
    let raw = std::fs::read_to_string(p.job_dir(job.id).join(file))?;
    let text = finish_draft(p, &raw).with_context(|| format!("job {}", job.id))?;
    println!("{} · job {}\n\n{text}\n", job_cost(&job), job.id);
    Ok(Some(text))
}

pub fn shot(p: &Project, r: ShotRef, how: Draft, a: &Ask) -> Result<()> {
    let label = format!("{how} draft for {r}");
    let Some(text) = make(p, DraftFor::Shot(r, how), &label, Some(r), a)? else {
        return Ok(());
    };
    let field = match how {
        Draft::Image => "frame prompt",
        Draft::Video | Draft::Translate => "prompt",
    };
    if !a.keep(&format!("use it as {r}'s {field}?"))? {
        println!("not saved");
        return Ok(());
    }
    let mut s = p.shot(r)?;
    match how {
        Draft::Image => s.frame_prompt = Some(text),
        Draft::Video | Draft::Translate => s.prompt = text,
    }
    p.save_shot(r, &s)?;
    println!("{r} {field} saved");
    Ok(())
}

pub fn element(p: &Project, handle: &str, a: &Ask) -> Result<()> {
    let el = p.element(handle)?;
    let label = format!("translation of @{}", el.handle);
    let Some(text) = make(p, DraftFor::Element(el.handle.clone()), &label, None, a)? else {
        return Ok(());
    };
    if !a.keep(&format!("use it as @{}'s description?", el.handle))? {
        println!("not saved");
        return Ok(());
    }
    p.set_description(&el.handle, &text)?;
    println!("@{} description saved", el.handle);
    Ok(())
}
