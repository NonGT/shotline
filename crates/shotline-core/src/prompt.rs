use shotline_protocol::{ModelInfo, Op};

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
    /// The shot it continues from; none for the shot's own storyboard frame.
    pub from: Option<ShotRef>,
    /// The image sent, relative to the project root, `/` separated. Taken from `video` when it
    /// isn't there yet.
    pub path: String,
    /// The selected take of `from`.
    pub video: Option<String>,
}

impl StartFrame {
    /// Where it comes from, for messages.
    pub fn source(&self) -> String {
        match self.from {
            Some(r) => r.to_string(),
            None => "its storyboard frame".into(),
        }
    }
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

/// What an image is for.
#[derive(Debug, Clone, PartialEq)]
pub enum ImageFor {
    /// A new anchor image of an element, from its description and any anchors it has.
    Anchor { handle: String, prompt: String },
    /// A shot's storyboard frame: the first frame of the video.
    Frame(ShotRef),
}

/// An image request, worked out without spending anything.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltImage {
    pub plugin: String,
    pub model: String,
    pub resolution: Option<String>,
    pub aspect: String,
    pub references: Vec<Reference>,
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
    if shot.prompt.trim().is_empty() {
        warnings.push(format!(
            "{r} has no prompt yet, `shotline shot draft {r}` can write one from the plot"
        ));
    }
    let (text, references) = resolve(project, spec, &shot.prompt, &shot.elements, &mut warnings)?;
    let mut parts = vec![text];
    add_layers(project, &references, &mut parts)?;

    let layers = &cfg.prompt;
    let mut start_frame = None;
    if shot.transition == Transition::Continue {
        match project.previous_shot(r)? {
            None => warnings.push("first shot can't continue from anything, it's a cut".into()),
            Some(prev) => {
                check_start_frame(spec, &references, &mut warnings);
                let selected = project.shot(prev)?.selected_take;
                match (selected.is_some(), project.selected_take(prev)?) {
                    (_, Some(take)) => {
                        let job = take.output().expect("only done takes get selected");
                        match job.outputs.first() {
                            Some(file) => {
                                start_frame = Some(StartFrame {
                                    from: Some(prev),
                                    path: format!("jobs/{}/last.png", job.id),
                                    video: Some(format!("jobs/{}/{file}", job.id)),
                                })
                            }
                            None => warnings
                                .push(format!("{take} made no file, so there's no start frame")),
                        }
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
    if let Some(frame) = &shot.frame {
        if shot.transition == Transition::Continue {
            warnings.push(
                "it continues from the shot before, so its storyboard frame isn't used".into(),
            );
        } else {
            check_start_frame(spec, &references, &mut warnings);
            start_frame = Some(StartFrame {
                from: None,
                path: frame.clone(),
                video: None,
            });
            if layers.continuity
                && let Some(t) = project.template("frame")?
            {
                parts.push(render(&t, &[]));
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
    check_size(spec, Some(&resolution), &aspect, &mut warnings);

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

/// The prompt for an anchor image or a storyboard frame, on the project's image model unless
/// `plugin`/`model` say otherwise.
pub fn build_image(
    project: &Project,
    catalog: &Catalog,
    what: &ImageFor,
    plugin: Option<&str>,
    model: Option<&str>,
) -> Result<BuiltImage> {
    let cfg = &project.config;
    let plugin = plugin.unwrap_or(&cfg.image.plugin).to_string();
    let model = model.unwrap_or(&cfg.image.model).to_string();
    let spec = &catalog.get(&plugin, &model, &Op::ImageGenerate)?.info;
    let mut warnings = vec![];
    let (mut parts, references, aspect) = match what {
        ImageFor::Anchor { handle, prompt } => {
            let el = project.element(handle)?;
            // The element's own anchors go along so a new one keeps the same look.
            let (prompt, references) = resolve(
                project,
                spec,
                prompt,
                std::slice::from_ref(&el.handle),
                &mut warnings,
            )?;
            // Making the first anchor of an element is the point, not something to warn about.
            let first = format!("@{} has no anchor images", el.handle);
            warnings.retain(|w| !w.starts_with(&first));
            let t = project.template("anchor")?.unwrap_or_default();
            let text = render(
                &t,
                &[
                    ("kind", &el.kind.to_string()),
                    ("name", &el.name),
                    ("description", &el.description),
                    ("prompt", &prompt),
                ],
            );
            let aspect = cfg
                .image
                .aspect
                .clone()
                .unwrap_or_else(|| cfg.aspect.clone());
            (vec![text.trim().to_string()], references, aspect)
        }
        ImageFor::Frame(r) => {
            let shot = project.shot(*r)?;
            let prompt = shot.frame_prompt.as_deref().unwrap_or(&shot.prompt);
            let (text, references) = resolve(project, spec, prompt, &shot.elements, &mut warnings)?;
            let mut parts = vec![];
            if let Some(t) = project.template("storyboard")? {
                parts.push(render(&t, &[]));
            }
            parts.push(text);
            if shot.transition == Transition::Continue {
                warnings.push(format!(
                    "{r} continues from the shot before, so a storyboard frame won't be used"
                ));
            }
            let aspect = shot
                .model
                .and_then(|m| m.aspect)
                .unwrap_or_else(|| cfg.aspect.clone());
            (parts, references, aspect)
        }
    };
    add_layers(project, &references, &mut parts)?;
    let resolution = cfg.image.resolution.clone();
    check_size(spec, resolution.as_ref(), &aspect, &mut warnings);
    Ok(BuiltImage {
        plugin,
        model,
        resolution,
        aspect,
        references,
        text: parts.join("\n\n"),
        warnings,
    })
}

/// The prompt with @handles swapped for reference labels (or names when the model takes no
/// references), and the anchor images to send, in order. `extra` are handles to attach that the
/// prompt doesn't mention.
fn resolve(
    project: &Project,
    spec: &ModelInfo,
    prompt: &str,
    extra: &[String],
    warnings: &mut Vec<String>,
) -> Result<(String, Vec<Reference>)> {
    // Mentioned handles first, in prompt order, then the extra ones.
    let mut handles = handle::unique(prompt);
    for h in extra {
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
        // "Image 1 and Image 2 walks" reads as two people; say they're one.
        let rep = match labels.as_slice() {
            [one] => one.clone(),
            many => format!("{} (shown in {})", el.name, many.join(" and ")),
        };
        replacements.push((h.clone(), rep));
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
    for m in handle::find(prompt) {
        text.push_str(&prompt[last..m.span.start]);
        match replacements.iter().find(|(h, _)| *h == m.handle) {
            Some((_, rep)) => text.push_str(rep),
            None => text.push_str(&prompt[m.span.clone()]),
        }
        last = m.span.end;
    }
    text.push_str(&prompt[last..]);
    Ok((text.trim().to_string(), references))
}

/// style.md and the identity note.
fn add_layers(project: &Project, references: &[Reference], parts: &mut Vec<String>) -> Result<()> {
    let layers = &project.config.prompt;
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
    Ok(())
}

fn check_start_frame(spec: &ModelInfo, references: &[Reference], warnings: &mut Vec<String>) {
    if !spec.start_frame {
        warnings.push(format!("{} doesn't take a start frame", spec.id));
    } else if !references.is_empty() && !spec.start_frame_with_refs {
        warnings.push(format!(
            "{} doesn't take a start frame together with reference images",
            spec.id
        ));
    }
}

fn check_size(
    spec: &ModelInfo,
    resolution: Option<&String>,
    aspect: &String,
    warnings: &mut Vec<String>,
) {
    if let Some(resolution) = resolution
        && !spec.resolutions.is_empty()
        && !spec.resolutions.contains(resolution)
    {
        warnings.push(format!(
            "{} doesn't do {resolution} ({})",
            spec.id,
            spec.resolutions.join(", ")
        ));
    }
    if !spec.aspects.is_empty() && !spec.aspects.contains(aspect) {
        warnings.push(format!(
            "{} doesn't do {aspect} ({})",
            spec.id,
            spec.aspects.join(", ")
        ));
    }
}
