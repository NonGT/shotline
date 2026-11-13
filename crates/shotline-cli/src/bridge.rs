//! Takes of a bridge: one video job from the frame before a join to the frame after it.

use anyhow::{Context as _, Result, bail};
use shotline_core::protocol::{Op, Request};
use shotline_core::{Bridge, Job, Project, Translated, Ulid, finish_draft, looks_english};

use crate::shoot::{self, Go};

/// What it sends: the prompt in English when there is one, and the note on motion.
pub fn request(p: &Project, b: &Bridge) -> Result<(String, Request)> {
    if b.plugin.is_empty() || b.model.is_empty() {
        bail!("the bridge has no model");
    }
    let mut prompt = b.translated().unwrap_or(&b.prompt).trim().to_string();
    let note = p.template("bridge")?.unwrap_or_default();
    if !note.trim().is_empty() {
        if !prompt.is_empty() {
            prompt += "\n\n";
        }
        prompt += note.trim();
    }
    let req = Request {
        op: Op::VideoGenerate,
        model: b.model.clone(),
        prompt: Some(prompt),
        duration: Some(b.length),
        resolution: b.resolution.clone(),
        start_frame: Some(p.root.join(&b.start)),
        end_frame: Some(p.root.join(&b.end)),
        params: shotline_engine::film_params(p, &b.plugin, &b.model, &Default::default()),
        ..Request::default()
    };
    Ok((b.plugin.clone(), req))
}

fn translation(p: &Project, b: &Bridge, translate: bool) -> Result<Option<(String, Request)>> {
    if !translate
        || b.prompt.trim().is_empty()
        || looks_english(&b.prompt)
        || b.translated().is_some()
    {
        return Ok(None);
    }
    shoot::to_english(p, b.prompt.clone()).map(Some)
}

/// What a take would cost, with its translation if there's one. Free.
pub fn quote(p: &Project, id: Ulid, translate: bool) -> Result<String> {
    let b = p.bridge(id)?;
    let (plugin, req) = request(p, &b)?;
    let cost = shoot::quote_once(p, &plugin, &req)?;
    Ok(match translation(p, &b, translate)? {
        Some((tp, treq)) => format!(
            "{cost} + {} to translate",
            shoot::quote_once(p, &tp, &treq)?
        ),
        None => cost,
    })
}

/// Makes a take if it still costs `at`; the finished job.
pub fn take_at(p: &Project, id: Ulid, at: &str, translate: bool) -> Result<Job> {
    let b = p.bridge(id)?;
    if let Some((tp, treq)) = translation(p, &b, translate)? {
        let total = quote(p, id, translate)?;
        if total != at {
            bail!("the price changed from {at} to {total}; nothing spent");
        }
        let job = shoot::spend_once(
            p,
            &tp,
            &treq,
            None,
            "translation for a bridge",
            None,
            Go::Yes,
        )?
        .context("the translation wasn't made")?;
        let file = job
            .outputs
            .first()
            .with_context(|| format!("job {} sent back no text", job.id))?;
        let raw = std::fs::read_to_string(p.job_dir(job.id).join(file))?;
        let text = finish_draft(p, &raw).with_context(|| format!("job {}", job.id))?;
        let mut b = p.bridge(id)?;
        b.english = Some(Translated {
            from: b.prompt.clone(),
            text,
        });
        p.save_bridge(id, &b)?;
        let (plugin, req) = request(p, &b)?;
        let cost = shoot::quote_once(p, &plugin, &req)?;
        return spend(p, id, &plugin, &req, &cost);
    }
    let (plugin, req) = request(p, &b)?;
    spend(p, id, &plugin, &req, at)
}

fn spend(p: &Project, id: Ulid, plugin: &str, req: &Request, at: &str) -> Result<Job> {
    shoot::spend(
        p,
        plugin,
        req,
        shoot::For {
            bridge: Some(id),
            ..shoot::For::default()
        },
        "bridge",
        None,
        Go::At(at),
    )?
    .context("nothing spent")
}

pub fn ls(p: &Project) -> Result<()> {
    for (id, b) in p.bridges()? {
        let takes = p.bridge_takes(id)?;
        let used = b
            .used
            .and_then(|u| takes.iter().find(|(_, j)| j.id == u))
            .map(|(n, _)| format!(", #{n} in the cut"))
            .unwrap_or_default();
        println!(
            "{id}  {} {:.3}s to {} {:.3}s · {:.2}s · {} take(s){used}",
            b.from.file,
            b.from.at,
            b.to.file,
            b.to.at,
            b.length,
            takes.len(),
        );
    }
    Ok(())
}
