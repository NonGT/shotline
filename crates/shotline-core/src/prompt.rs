use shotline_protocol::Op;

use crate::template::render;
use crate::{Catalog, Project, Result, ShotRef, Transition, handle};

#[derive(Debug, Clone, PartialEq)]
pub struct Reference {
    /// What the prompt calls it, e.g. `Image 1`.
    pub label: String,
    pub handle: String,
    /// Relative to the project root, `/` separated.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StartFrame {
    pub from: ShotRef,
    /// Relative to the project root, `/` separated.
    pub path: String,
}

/// Everything that would be sent for a shot, worked out without spending anything.
#[derive(Debug, Clone, PartialEq)]
pub struct Built {
    pub shot: ShotRef,
    pub plugin: String,
    pub model: String,
    pub resolution: String,
    pub aspect: String,
    pub duration: f64,
    pub references: Vec<Reference>,
    pub start_frame: Option<StartFrame>,
    pub params: toml::Table,
    pub text: String,
    pub warnings: Vec<String>,
}

pub fn build(project: &Project, catalog: &Catalog, r: ShotRef) -> Result<Built> {
    let shot = project.shot(r)?;
    let cfg = &project.config;
    let ov = shot.model.clone().unwrap_or_default();
    let plugin = ov.plugin.unwrap_or_else(|| cfg.plugin.clone());
    let model = ov.id.unwrap_or_else(|| cfg.model.clone());
    let resolution = ov.resolution.unwrap_or_else(|| cfg.resolution.clone());
    let aspect = ov.aspect.unwrap_or_else(|| cfg.aspect.clone());
    let duration = shot.duration.unwrap_or(cfg.duration);
    let spec = &catalog.get(&plugin, &model, &Op::VideoGenerate)?.info;
    let mut warnings = vec![];

    // Mentioned handles first, in prompt order, then any listed by hand in `elements`.
    let mut handles = handle::unique(&shot.prompt);
    for h in &shot.elements {
        if !handles.contains(h) {
            handles.push(h.clone());
        }
    }

    let mut references = vec![];
    let mut replacements = vec![];
    for h in &handles {
        let el = project.element(h)?;
        let anchors = project.anchors(&el)?;
        if anchors.is_empty() {
            warnings.push(format!(
                "@{h} has no anchor images, the prompt uses its name"
            ));
            replacements.push((h.clone(), el.name.clone()));
            continue;
        }
        let Some(ref_image) = &spec.ref_image else {
            replacements.push((h.clone(), el.name.clone()));
            continue;
        };
        let mut labels = vec![];
        for path in anchors {
            let n = (references.len() + 1).to_string();
            let label = render(ref_image, &[("n", &n)]);
            labels.push(label.clone());
            references.push(Reference {
                label,
                handle: h.clone(),
                path: project.rel(&path),
            });
        }
        replacements.push((h.clone(), labels.join(" and ")));
    }
    if spec.ref_image.is_none() && !handles.is_empty() {
        warnings.push(format!(
            "{} doesn't take reference images, the prompt uses names",
            spec.id
        ));
    }
    if let Some(max) = spec.max_images
        && references.len() > max
    {
        warnings.push(format!(
            "{} reference images but {} takes at most {max}",
            references.len(),
            spec.id,
        ));
    }

    let mut text = String::new();
    let mut last = 0;
    for m in handle::find(&shot.prompt) {
        text.push_str(&shot.prompt[last..m.span.start]);
        match replacements.iter().find(|(h, _)| *h == m.handle) {
            Some((_, rep)) => text.push_str(rep),
            None => text.push_str(&shot.prompt[m.span.clone()]),
        }
        last = m.span.end;
    }
    text.push_str(&shot.prompt[last..]);
    let mut parts = vec![text.trim().to_string()];

    let layers = &cfg.prompt;
    if layers.style {
        let style = project.style()?;
        if !style.is_empty()
            && let Some(t) = project.template("style")?
        {
            parts.push(render(&t, &[("style", &style)]));
        }
    }
    if layers.identity
        && !references.is_empty()
        && let Some(t) = project.template("identity")?
    {
        parts.push(render(&t, &[]));
    }

    let mut start_frame = None;
    if shot.transition == Transition::Continue {
        match project.previous_shot(r)? {
            None => warnings.push("first shot can't continue from anything, it's a cut".into()),
            Some(prev) => {
                if !spec.start_frame {
                    warnings.push(format!("{} doesn't take a start frame", spec.id));
                } else if !references.is_empty() && !spec.start_frame_with_refs {
                    warnings.push(format!(
                        "{} doesn't take a start frame together with reference images",
                        spec.id
                    ));
                }
                let selected = project.shot(prev)?.selected_take;
                match (selected.is_some(), project.selected_take(prev)?) {
                    (_, Some(take)) => {
                        let job = take.output().expect("only done takes get selected");
                        start_frame = Some(StartFrame {
                            from: prev,
                            path: format!("jobs/{}/last.png", job.id),
                        })
                    }
                    (true, None) => warnings.push(format!(
                        "{prev}'s selected take is gone, so there's no start frame"
                    )),
                    (false, None) => warnings.push(format!(
                        "{prev} has no selected take yet, so there's no start frame"
                    )),
                }
                if layers.continuity
                    && let Some(t) = project.template("continue")?
                {
                    parts.push(render(&t, &[]));
                }
            }
        }
    }

    if layers.model_notes
        && let Some(notes) = &spec.notes
        && let Some(t) = project.template(&format!("models/{notes}"))?
    {
        parts.push(render(&t, &[]));
    }

    for line in &shot.lines {
        match project.find_element(&line.who)? {
            Some(el) if el.voice.is_some() => {}
            Some(_) => warnings.push(format!("@{} has a line but no voice", line.who)),
            None => warnings.push(format!("a line is by @{}, who doesn't exist", line.who)),
        }
    }
    let steps = project.steps(&shot)?;
    if !shot.lines.is_empty() && !steps.iter().any(|s| s.op == Op::AudioTts) {
        warnings.push("the lines won't be spoken: the shot's recipe has no audio.tts step".into());
    }
    if steps.iter().any(|s| s.op == Op::VideoGenerate && s.audio) {
        if !spec.audio_input {
            warnings.push(format!(
                "{} doesn't take audio, use a video.lipsync step",
                spec.id
            ));
        } else if let Some(max) = spec.max_audio
            && shot.lines.len() > max
        {
            warnings.push(format!(
                "{} lines but {} takes at most {max} audio clips",
                shot.lines.len(),
                spec.id
            ));
        }
    }

    let (lo, hi) = (
        spec.min_duration.unwrap_or(0.0),
        spec.max_duration.unwrap_or(f64::INFINITY),
    );
    if duration < lo || duration > hi {
        warnings.push(format!("{duration}s is outside {}'s {lo}-{hi}s", spec.id));
    }
    if spec.whole_seconds && duration.fract() != 0.0 {
        warnings.push(format!("{} only takes whole seconds", spec.id));
    }
    if !spec.resolutions.is_empty() && !spec.resolutions.contains(&resolution) {
        warnings.push(format!(
            "{} doesn't do {resolution} ({})",
            spec.id,
            spec.resolutions.join(", ")
        ));
    }
    if !spec.aspects.is_empty() && !spec.aspects.contains(&aspect) {
        warnings.push(format!(
            "{} doesn't do {aspect} ({})",
            spec.id,
            spec.aspects.join(", ")
        ));
    }

    Ok(Built {
        shot: r,
        plugin,
        model,
        resolution,
        aspect,
        duration,
        references,
        start_frame,
        params: ov.params,
        text: parts.join("\n\n"),
        warnings,
    })
}
