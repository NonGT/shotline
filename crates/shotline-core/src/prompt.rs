use shotline_protocol::{ModelInfo, Op};

use crate::template::render;
use crate::{Catalog, Project, Result, Shot, ShotRef, Transition, handle};

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
    Anchor {
        handle: String,
        prompt: String,
        /// The images to go along instead of the element's anchors: some of them, others from
        /// anywhere, or none. The prompt names them `Image 1`, `Image 2`… in this order.
        refs: Option<Vec<std::path::PathBuf>>,
        /// Instead of [image] in shotline.toml.
        aspect: Option<String>,
        resolution: Option<String>,
    },
    /// A shot's storyboard frame: the first frame of the video, from its frame prompt.
    Frame {
        shot: ShotRef,
        /// Instead of its frame prompt, e.g. that put into English.
        prompt: Option<String>,
        /// None sends what the video would get: the elements' anchors and the shot's own.
        refs: Option<Vec<std::path::PathBuf>>,
        /// Instead of the shot's shape.
        aspect: Option<String>,
        resolution: Option<String>,
    },
    /// A picture for a shot to send along, e.g. a place or a prop that isn't an element.
    ShotImage {
        shot: ShotRef,
        prompt: String,
        /// As for an anchor; None sends the anchors of the elements the prompt names.
        refs: Option<Vec<std::path::PathBuf>>,
        /// Instead of the shot's shape.
        aspect: Option<String>,
        resolution: Option<String>,
    },
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
    if aspect != cfg.aspect {
        warnings.push(format!(
            "{r} is {aspect} but the film is {}; the render will stretch it",
            cfg.aspect
        ));
    }
    if shot.prompt.trim().is_empty() {
        warnings.push(format!(
            "{r} has no prompt yet, `shotline shot draft {r}` can write one from the plot"
        ));
    }
    let (mut text, mut references) = resolve(
        project,
        spec,
        shot.sent_prompt(),
        &shot.elements,
        &shot.leave_out,
        &mut warnings,
    )?;
    own_images(
        project,
        spec,
        r,
        &shot,
        &mut text,
        &mut references,
        &mut warnings,
    )?;
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
    for step in &steps {
        if let (Some(p), Some(m)) = (&step.plugin, &step.model)
            && catalog.find(p, m, &step.op).is_none()
        {
            warnings.push(format!(
                "{p} {m} isn't installed, so its {} step can't run",
                step.op
            ));
        }
    }
    if !shot.lines.is_empty() && !steps.iter().any(|s| s.op == Op::AudioTts) {
        warnings.push("the lines won't be spoken: the shot's recipe has no audio.tts step".into());
    }
    if steps.iter().any(|s| s.op == Op::VideoGenerate && s.audio) {
        if !spec.audio_input {
            warnings.push(format!(
                "{} doesn't take audio, so the lines have to be lip synced after",
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
        ImageFor::Anchor {
            handle,
            prompt,
            refs,
            aspect,
            ..
        } => {
            let el = project.element(handle)?;
            let (prompt, references) = match refs {
                // The element's own anchors go along so a new one keeps the same look.
                None => resolve(
                    project,
                    spec,
                    prompt,
                    std::slice::from_ref(&el.handle),
                    &[],
                    &mut warnings,
                )?,
                Some(files) => chosen(project, spec, Some(&el), prompt, files, &mut warnings)?,
            };
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
            let aspect = aspect
                .clone()
                .or_else(|| cfg.image.aspect.clone())
                .unwrap_or_else(|| cfg.aspect.clone());
            (vec![text.trim().to_string()], references, aspect)
        }
        ImageFor::Frame {
            shot: r,
            prompt,
            refs,
            aspect: picked_aspect,
            ..
        } => {
            let shot = project.shot(*r)?;
            let prompt = prompt
                .as_deref()
                .or(shot.frame_prompt.as_deref())
                .unwrap_or(shot.sent_prompt());
            let (text, references) = match refs {
                None => {
                    let (mut text, mut references) = resolve(
                        project,
                        spec,
                        prompt,
                        &shot.elements,
                        &shot.leave_out,
                        &mut warnings,
                    )?;
                    own_images(
                        project,
                        spec,
                        *r,
                        &shot,
                        &mut text,
                        &mut references,
                        &mut warnings,
                    )?;
                    (text, references)
                }
                Some(files) => chosen(project, spec, None, prompt, files, &mut warnings)?,
            };
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
            let aspect = picked_aspect
                .clone()
                .or_else(|| shot.model.and_then(|m| m.aspect))
                .unwrap_or_else(|| cfg.aspect.clone());
            (parts, references, aspect)
        }
        ImageFor::ShotImage {
            shot: r,
            prompt,
            refs,
            aspect,
            ..
        } => {
            let shot = project.shot(*r)?;
            let (text, references) = match refs {
                None => resolve(project, spec, prompt, &[], &[], &mut warnings)?,
                Some(files) => chosen(project, spec, None, prompt, files, &mut warnings)?,
            };
            let aspect = aspect
                .clone()
                .or_else(|| shot.model.and_then(|m| m.aspect))
                .unwrap_or_else(|| cfg.aspect.clone());
            (vec![text], references, aspect)
        }
    };
    add_layers(project, &references, &mut parts)?;
    let resolution = match what {
        ImageFor::Anchor {
            resolution: Some(r),
            ..
        }
        | ImageFor::ShotImage {
            resolution: Some(r),
            ..
        }
        | ImageFor::Frame {
            resolution: Some(r),
            ..
        } => Some(r.clone()),
        _ => cfg.image.resolution.clone(),
    };
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
/// prompt doesn't mention; anchors in `skip` stay home.
fn resolve(
    project: &Project,
    spec: &ModelInfo,
    prompt: &str,
    extra: &[String],
    skip: &[String],
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
        let all = project.anchors(&el)?;
        let anchors: Vec<_> = all
            .iter()
            .filter(|a| !skip.contains(&project.rel(a)))
            .cloned()
            .collect();
        if anchors.is_empty() && !all.is_empty() {
            // Every one left out on purpose.
            replacements.push((h.clone(), el.name.clone()));
            continue;
        }
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

/// A prompt with the images picked for it, in order. An element the prompt names, or `el` (the
/// one an anchor is made for), is pointed at whichever of its anchors were picked; with none of
/// them picked it's named in words.
fn chosen(
    project: &Project,
    spec: &ModelInfo,
    el: Option<&crate::Element>,
    prompt: &str,
    files: &[std::path::PathBuf],
    warnings: &mut Vec<String>,
) -> Result<(String, Vec<Reference>)> {
    let mut references: Vec<Reference> = match &spec.ref_image {
        Some(ref_image) => files
            .iter()
            .enumerate()
            .map(|(i, f)| Reference {
                label: render(ref_image, &[("n", &(i + 1).to_string())]),
                handle: String::new(),
                path: project.rel(f),
            })
            .collect(),
        None => {
            if !files.is_empty() {
                warnings.push(format!(
                    "{} doesn't take reference images, so they're left out",
                    spec.id
                ));
            }
            vec![]
        }
    };
    let mut whose: Vec<crate::Element> = el.into_iter().cloned().collect();
    for h in handle::unique(prompt) {
        if let Ok(e) = project.element(&h)
            && !whose.iter().any(|w| w.handle == e.handle)
        {
            whose.push(e);
        }
    }
    // Each element as the prompt should say it.
    let mut says = vec![];
    for e in &whose {
        let anchors: Vec<_> = project.anchors(e)?.iter().map(|a| project.rel(a)).collect();
        let mut labels = vec![];
        for r in references.iter_mut().filter(|r| anchors.contains(&r.path)) {
            r.handle = e.handle.clone();
            labels.push(r.label.clone());
        }
        says.push((e.handle.clone(), e.name.clone(), labels));
    }
    let say = |handle: &str| {
        says.iter()
            .find(|(h, ..)| h == handle)
            .map(|(_, name, labels)| match labels.as_slice() {
                [] => name.clone(),
                [one] => one.clone(),
                many => format!("{name} (shown in {})", many.join(" and ")),
            })
    };
    let mut text = String::new();
    let mut last = 0;
    for m in handle::find(prompt) {
        text.push_str(&prompt[last..m.span.start]);
        match say(&m.handle) {
            Some(t) => text.push_str(&t),
            None => text.push_str(&prompt[m.span.clone()]),
        }
        last = m.span.end;
    }
    text.push_str(&prompt[last..]);
    if let Some(max) = spec.max_images
        && references.len() > max
    {
        warnings.push(format!(
            "{} reference images but {} takes at most {max}",
            references.len(),
            spec.id,
        ));
    }
    // The element an anchor is made for is seldom named in what's typed.
    if let Some(el) = el
        && !handle::unique(prompt).contains(&el.handle)
        && let Some((_, _, shown)) = says.iter().find(|(h, ..)| *h == el.handle)
        && !shown.is_empty()
    {
        text = format!(
            "{} {} as shown in {}.",
            text.trim(),
            el.name,
            shown.join(" and ")
        );
    }
    Ok((text.trim().to_string(), references))
}

/// The shot's own images, labelled after the elements' anchors, with a line for each note.
fn own_images(
    project: &Project,
    spec: &ModelInfo,
    r: ShotRef,
    shot: &Shot,
    text: &mut String,
    references: &mut Vec<Reference>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    if shot.images.is_empty() {
        return Ok(());
    }
    let Some(ref_image) = &spec.ref_image else {
        warnings.push(format!(
            "{} doesn't take reference images, so {r}'s own images are left out",
            spec.id
        ));
        return Ok(());
    };
    let before = references.len();
    let t = project.template("shot-image")?.unwrap_or_default();
    let mut notes = vec![];
    for img in &shot.images {
        if !project.root.join(&img.file).is_file() {
            warnings.push(format!("{} is gone, so it's left out", img.file));
            continue;
        }
        let label = render(ref_image, &[("n", &(references.len() + 1).to_string())]);
        if !img.note.trim().is_empty() {
            notes.push(render(&t, &[("label", &label), ("note", img.note.trim())]));
        }
        references.push(Reference {
            label,
            handle: String::new(),
            path: img.file.clone(),
        });
    }
    if !notes.is_empty() {
        *text = format!("{}\n\n{}", text.trim(), notes.join(" "))
            .trim()
            .to_string();
    }
    // Over the limit with the elements' anchors alone is already said.
    if let Some(max) = spec.max_images
        && references.len() > max
        && before <= max
    {
        warnings.push(format!(
            "{} reference images but {} takes at most {max}",
            references.len(),
            spec.id,
        ));
    }
    Ok(())
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
