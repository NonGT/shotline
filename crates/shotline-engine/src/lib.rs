//! Running shots: plan the recipe's steps, quote them, reserve, run each in turn feeding outputs
//! forward, book what it cost. Shared by the CLI and later the MCP server and UI.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::json;
use shotline_core::ledger::{Books, Context, Entry, money};
use shotline_core::protocol::{
    ErrorKind, MediaKind, Op, PluginError, Quote, Reference, Request, State, Status,
};
use shotline_core::{Built, Catalog, Cost, Job, JobStatus, Project, ShotRef, Take, build};
use shotline_plugin::{Error as PluginErr, Plugin};
use tokio::sync::Notify;
use ulid::Ulid;

/// Where plugins come from: the CLI opens them on demand, tests hand in fakes.
pub trait Plugins {
    fn plugin(&self, name: &str) -> Result<&dyn Plugin>;
}

impl Plugins for BTreeMap<String, Box<dyn Plugin>> {
    fn plugin(&self, name: &str) -> Result<&dyn Plugin> {
        self.get(name)
            .map(|p| p.as_ref())
            .ok_or_else(|| anyhow!("plugin {name} isn't open"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepKind {
    /// Line n of the shot, spoken.
    Line(usize),
    /// The shot's video; `audio` sends the spoken lines along.
    Generate { audio: bool },
    /// Anything working on the video so far: lip sync, upscale, ...
    Process,
}

/// One job of a take.
#[derive(Debug, Clone)]
pub struct StepPlan {
    pub op: Op,
    pub plugin: String,
    pub model: String,
    /// For people: "line 1 by @kan", "video", "video.lipsync".
    pub what: String,
    pub request: Request,
    pub quote: Quote,
    /// The quote in the film's currency; 0 for a reused step.
    pub base: f64,
    /// Taken over from an earlier take instead of being run again.
    pub reused: Option<Job>,
    kind: StepKind,
}

/// What a take would run and cost, worked out before anything is spent.
#[derive(Debug, Clone)]
pub struct Plan {
    pub shot: ShotRef,
    pub built: Built,
    pub steps: Vec<StepPlan>,
    pub total: f64,
    pub currency: String,
}

fn to_json(t: &toml::Table) -> serde_json::Map<String, serde_json::Value> {
    match serde_json::to_value(t) {
        Ok(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    }
}

/// Says which shot, element or file a plugin error from the video step is about.
pub fn explain(e: &PluginError, built: &Built) -> String {
    let what = e.field.as_deref().map(|f| {
        if let Some(i) = f
            .strip_prefix("references[")
            .and_then(|r| r.strip_suffix(']'))
            .and_then(|n| n.parse::<usize>().ok())
        {
            return match built.references.get(i) {
                Some(r) => format!("@{}'s anchor {} ({})", r.handle, r.path, r.label),
                None => format!("audio clip {}", i - built.references.len() + 1),
            };
        }
        if let Some(p) = f.strip_prefix("params.") {
            return format!("[model.params] {p} in shot {}", built.shot);
        }
        match f {
            "start_frame" => match &built.start_frame {
                Some(sf) => format!("the start frame {} from {}", sf.path, sf.from),
                None => "the start frame".into(),
            },
            "prompt" => format!("the prompt of {}", built.shot),
            "model" => format!("model {} {}", built.plugin, built.model),
            "duration" => format!("duration {}s", built.duration),
            "resolution" => format!("resolution {}", built.resolution),
            "aspect" => format!("aspect {}", built.aspect),
            other => other.to_string(),
        }
    });
    match what {
        Some(w) => format!("{}: {} ({w})", e.kind, e.message),
        None => format!("{}: {}", e.kind, e.message),
    }
}

fn explain_step(e: &PluginError, step: &StepPlan, built: &Built) -> String {
    match step.kind {
        StepKind::Generate { .. } => explain(e, built),
        _ => match &e.field {
            Some(f) => format!("{}: {} ({}, {f})", e.kind, e.message, step.what),
            None => format!("{}: {} ({})", e.kind, e.message, step.what),
        },
    }
}

fn plugin_error(e: PluginErr, step: &StepPlan, built: &Built) -> anyhow::Error {
    match e {
        PluginErr::Plugin(pe) => anyhow!(explain_step(&pe, step, built)),
        other => anyhow!(other),
    }
}

/// Stands in for a file an earlier step will make, so later steps can be quoted.
fn pending(project: &Project, name: &str) -> PathBuf {
    project.root.join("jobs").join("pending").join(name)
}

/// Works out every step of a take of `r` and prices it. With `reuse`, steps before the given
/// index are taken over from that earlier take.
pub async fn plan(
    project: &Project,
    catalog: &Catalog,
    books: &Books,
    plugins: &dyn Plugins,
    r: ShotRef,
    reuse: Option<(&Take, usize)>,
) -> Result<Plan> {
    let built = build(project, catalog, r)?;
    let shot = project.shot(r)?;
    let start_frame = match &built.start_frame {
        Some(sf) => {
            let p = project.root.join(&sf.path);
            if !p.is_file() {
                bail!(
                    "{r} continues from {}, but {} isn't there yet",
                    sf.from,
                    sf.path
                );
            }
            Some(p)
        }
        None => None,
    };
    let video = Request {
        op: Op::VideoGenerate,
        model: built.model.clone(),
        prompt: Some(built.text.clone()),
        duration: Some(built.duration),
        resolution: Some(built.resolution.clone()),
        aspect: Some(built.aspect.clone()),
        references: built
            .references
            .iter()
            .map(|x| Reference {
                kind: MediaKind::Image,
                path: project.root.join(&x.path),
            })
            .collect(),
        start_frame,
        params: to_json(&built.params),
        ..Request::default()
    };

    let mut steps = vec![];
    for step in &project.steps(&shot)? {
        let mut params = to_json(&step.params);
        match &step.op {
            Op::AudioTts => {
                for (i, line) in shot.lines.iter().enumerate() {
                    let el = project.element(&line.who)?;
                    let Some(voice) = el.voice.clone() else {
                        bail!(
                            "@{} has no voice; give one with `shotline element voice {}`",
                            el.handle,
                            el.handle
                        );
                    };
                    let plugin = step.plugin.clone().unwrap_or(voice.plugin.clone());
                    let Some(model) = step.model.clone().or(voice.model.clone()) else {
                        bail!(
                            "@{}'s voice has no model and the recipe step names none",
                            el.handle
                        );
                    };
                    let mut v = json!({"settings": serde_json::to_value(&voice.settings)?});
                    if let Some(id) = &voice.voice_id {
                        v["voice_id"] = json!(id);
                    }
                    if let Some(s) = &voice.sample {
                        v["sample"] = json!(project.root.join(s));
                    }
                    steps.push((
                        StepKind::Line(i),
                        format!("line {} by @{}", i + 1, line.who),
                        plugin,
                        Request {
                            op: Op::AudioTts,
                            model,
                            prompt: Some(line.text.clone()),
                            voice: Some(v),
                            params: params.clone(),
                            ..Request::default()
                        },
                    ));
                }
            }
            Op::VideoGenerate => {
                if step.plugin.is_some() || step.model.is_some() {
                    bail!(
                        "video.generate uses the shot's model; set it on the shot, not the recipe"
                    );
                }
                let mut req = video.clone();
                if step.audio {
                    if shot.lines.is_empty() {
                        bail!(
                            "the recipe sends the lines' audio to the video model, but {r} has no lines"
                        );
                    }
                    for i in 0..shot.lines.len() {
                        req.references.push(Reference {
                            kind: MediaKind::Audio,
                            path: pending(project, &format!("line{i}.mp3")),
                        });
                    }
                }
                req.params.append(&mut params);
                steps.push((
                    StepKind::Generate { audio: step.audio },
                    "video".into(),
                    built.plugin.clone(),
                    req,
                ));
            }
            op => {
                let (Some(plugin), Some(model)) = (step.plugin.clone(), step.model.clone()) else {
                    bail!("the {op} step of the recipe needs a plugin and a model");
                };
                let mut req = Request {
                    op: op.clone(),
                    model,
                    duration: Some(built.duration),
                    params,
                    ..Request::default()
                };
                req.inputs
                    .insert("video".into(), pending(project, "video.mp4"));
                if *op == Op::VideoLipsync {
                    match shot.lines.len() {
                        1 => {}
                        0 => bail!("lip sync needs a line, {r} has none"),
                        n => bail!(
                            "lip sync takes one audio clip and {r} has {n} lines; joining them comes with the media work"
                        ),
                    }
                    req.inputs
                        .insert("audio".into(), pending(project, "line0.mp3"));
                }
                steps.push((StepKind::Process, op.to_string(), plugin, req));
            }
        }
    }
    if !steps
        .iter()
        .any(|s| matches!(s.0, StepKind::Generate { .. }))
    {
        bail!("the recipe never makes a video");
    }

    let (reuse_take, from) = match reuse {
        Some((_, n)) if n >= steps.len() => bail!("there are only {} steps", steps.len()),
        Some((t, n)) => (Some(t), n),
        None => (None, 0),
    };

    let mut out = vec![];
    let mut total = 0.0;
    for (index, (kind, what, plugin, request)) in steps.into_iter().enumerate() {
        let mut sp = StepPlan {
            op: request.op.clone(),
            plugin,
            model: request.model.clone(),
            what,
            request,
            quote: Quote {
                amount: 0.0,
                unit: books.currency.clone(),
                units: None,
                unit_name: None,
            },
            base: 0.0,
            reused: None,
            kind,
        };
        if let Some(t) = reuse_take
            && index < from
        {
            let job = t
                .jobs
                .iter()
                .find(|j| j.step as usize == index)
                .filter(|j| j.status == JobStatus::Done && j.op == sp.op)
                .with_context(|| {
                    format!("{t} has no finished {} at step {index} to reuse", sp.op)
                })?;
            sp.reused = Some(job.clone());
        } else {
            let p = plugins.plugin(&sp.plugin)?;
            sp.quote = p
                .quote(&sp.request)
                .await
                .map_err(|e| plugin_error(e, &sp, &built))?;
            sp.base = books
                .rates
                .convert(sp.quote.amount, &sp.quote.unit, &books.currency)?
                .amount;
            total += sp.base;
        }
        out.push(sp);
    }
    Ok(Plan {
        shot: r,
        built,
        steps: out,
        total,
        currency: books.currency.clone(),
    })
}

#[derive(Debug)]
pub enum Event {
    Reserved {
        total: f64,
        currency: String,
    },
    Step {
        index: usize,
        of: usize,
        what: String,
        plugin: String,
        model: String,
        reused: bool,
    },
    Submitted {
        plugin_job: String,
    },
    Status(Status),
    Cancelling,
}

pub struct Options {
    pub poll_every: Duration,
    /// Notified to cancel the running job (Ctrl-C).
    pub cancel: Arc<Notify>,
    /// Poll errors in a row before giving up on a job.
    pub max_poll_errors: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            poll_every: Duration::from_secs(5),
            cancel: Arc::new(Notify::new()),
            max_poll_errors: 5,
        }
    }
}

#[derive(Debug)]
pub struct Outcome {
    pub take: Take,
    /// The last job that ran.
    pub job: Job,
    /// Why it didn't finish, if it didn't.
    pub error: Option<String>,
}

fn context(job: &Job) -> Context {
    Context {
        job: job.id.to_string(),
        take: job.take.map(|t| t.to_string()),
        shot: job.shot.map(|s| s.to_string()),
        chapter: job.shot.map(|s| s.chapter),
        op: job.op.to_string(),
        plugin: job.plugin.clone(),
        model: job.model.clone(),
    }
}

struct Run<'a> {
    project: &'a Project,
    books: &'a Books,
    opts: &'a Options,
}

impl Run<'_> {
    /// Runs one already reserved job to the end and books it. Returns the job as it ended and
    /// why it failed, if it did.
    async fn job(
        &self,
        plugin: &dyn Plugin,
        job: Job,
        request: &Request,
        explain: &dyn Fn(PluginErr) -> String,
        on_event: &mut dyn FnMut(Event),
    ) -> Result<(Job, Option<String>)> {
        let (project, books, opts) = (self.project, self.books, self.opts);
        let ctx = context(&job);
        let mut job = project.create_job(job)?;
        let out_dir = project.job_dir(job.id);
        let finish = |job: &mut Job, status: JobStatus, error: Option<String>| -> Result<()> {
            job.status = status;
            job.error = error;
            Ok(project.save_job(job)?)
        };

        let plugin_job = match plugin.run(request, &out_dir).await {
            Ok(pj) => pj,
            Err(e) => {
                let msg = explain(e);
                books.release(project.clock.now(), &ctx.job)?;
                finish(&mut job, JobStatus::Failed, Some(msg.clone()))?;
                return Ok((job, Some(msg)));
            }
        };
        job.plugin_job = Some(plugin_job.clone());
        job.status = JobStatus::Running;
        project.save_job(&job)?;
        on_event(Event::Submitted {
            plugin_job: plugin_job.clone(),
        });

        let mut errors = 0;
        let mut cancelled = false;
        let status = loop {
            tokio::select! {
                _ = tokio::time::sleep(opts.poll_every) => {}
                _ = opts.cancel.notified(), if !cancelled => {
                    cancelled = true;
                    on_event(Event::Cancelling);
                    let _ = plugin.cancel(&plugin_job).await;
                }
            }
            match plugin.poll(&plugin_job).await {
                Ok(s) => {
                    errors = 0;
                    let done = s.state.is_finished();
                    on_event(Event::Status(s.clone()));
                    if done {
                        break s;
                    }
                }
                Err(PluginErr::Plugin(pe)) if pe.kind == ErrorKind::NotFound => {
                    break Status {
                        state: State::Failed,
                        progress: None,
                        error: Some(pe),
                        cost: None,
                    };
                }
                Err(e) => {
                    errors += 1;
                    if errors >= opts.max_poll_errors {
                        // Left held: it may well be running and charged. `cost` shows it as
                        // held until it's sorted out by hand.
                        let msg = format!("lost track of the job after {errors} failed polls: {e}");
                        finish(&mut job, JobStatus::Failed, Some(msg.clone()))?;
                        return Ok((job, Some(msg)));
                    }
                }
            }
        };

        let now = project.clock.now();
        let cost_of = |e: &Entry| Cost {
            amount: e.amount,
            unit: e.unit.clone(),
        };
        match status.state {
            State::Done => {
                // Done means money was spent, whatever happens with the download.
                let charged =
                    books.charge(now, &ctx.job, Some(&plugin_job), status.cost.as_ref())?;
                job.cost = Some(cost_of(&charged));
                match plugin.fetch(&plugin_job, &out_dir).await {
                    Ok(out) => {
                        job.outputs = out.outputs.into_iter().map(|o| o.file).collect();
                        finish(&mut job, JobStatus::Done, None)?;
                        Ok((job, None))
                    }
                    Err(e) => {
                        let msg = format!("finished but the download failed: {}", explain(e));
                        finish(&mut job, JobStatus::Failed, Some(msg.clone()))?;
                        Ok((job, Some(msg)))
                    }
                }
            }
            State::Failed | State::Cancelled => {
                match &status.cost {
                    Some(c) => {
                        let e = books.charge(now, &ctx.job, Some(&plugin_job), Some(c))?;
                        job.cost = Some(cost_of(&e));
                    }
                    None => {
                        books.release(now, &ctx.job)?;
                    }
                }
                let (st, msg) = if status.state == State::Cancelled || cancelled {
                    (JobStatus::Cancelled, "cancelled".to_string())
                } else {
                    let msg = match status.error {
                        Some(pe) => explain(PluginErr::Plugin(pe)),
                        None => "the job failed".into(),
                    };
                    (JobStatus::Failed, msg)
                };
                finish(&mut job, st, Some(msg.clone()))?;
                Ok((job, Some(msg)))
            }
            State::Queued | State::Running => {
                unreachable!("the loop only ends on a finished state")
            }
        }
    }
}

fn output(project: &Project, job: &Job) -> Result<PathBuf> {
    let file = job
        .outputs
        .first()
        .with_context(|| format!("job {} made no file", job.id))?;
    Ok(project.job_dir(job.id).join(file))
}

/// Runs a planned take. Every step is reserved first, so a take that wouldn't fit the budget
/// never starts. Each step's output feeds the next; the first failure ends the take and releases
/// what the rest had reserved. The caller asks for confirmation before this.
pub async fn take(
    project: &Project,
    books: &Books,
    plugins: &dyn Plugins,
    plan: &Plan,
    opts: &Options,
    mut on_event: impl FnMut(Event),
) -> Result<Outcome> {
    let r = plan.shot;
    let take_id = project.new_take_id();
    let mut jobs: Vec<Option<Job>> = vec![];
    for (i, sp) in plan.steps.iter().enumerate() {
        if sp.reused.is_some() {
            jobs.push(None);
            continue;
        }
        let mut job = Job::new(sp.op.clone(), &sp.plugin, &sp.model);
        job.id = project.clock.ulid();
        job.shot = Some(r);
        job.take = Some(take_id);
        job.step = i as u32;
        jobs.push(Some(job));
    }

    // Reserve everything up front; undo on the first refusal.
    let now = project.clock.now();
    let mut held: Vec<String> = vec![];
    for (sp, job) in plan.steps.iter().zip(&jobs) {
        let Some(job) = job else { continue };
        if let Err(e) = books.reserve(now, &context(job), &sp.quote) {
            for id in &held {
                books.release(now, id)?;
            }
            return Err(e.into());
        }
        held.push(job.id.to_string());
    }
    on_event(Event::Reserved {
        total: plan.total,
        currency: plan.currency.clone(),
    });

    let run = Run {
        project,
        books,
        opts,
    };
    let mut audio: BTreeMap<usize, (PathBuf, Ulid)> = BTreeMap::new();
    let mut video: Option<(PathBuf, Ulid)> = None;
    let mut last: Option<Job> = None;
    let n = plan.steps.len();
    for (i, (sp, job)) in plan.steps.iter().zip(jobs).enumerate() {
        on_event(Event::Step {
            index: i,
            of: n,
            what: sp.what.clone(),
            plugin: sp.plugin.clone(),
            model: sp.model.clone(),
            reused: sp.reused.is_some(),
        });
        let mut req = sp.request.clone();
        let mut inputs = vec![];
        match sp.kind {
            StepKind::Line(_) => {}
            StepKind::Generate { audio: with_audio } => {
                inputs = plan
                    .built
                    .references
                    .iter()
                    .map(|x| x.path.clone())
                    .collect();
                if let Some(sf) = &plan.built.start_frame {
                    inputs.push(sf.path.clone());
                }
                if with_audio {
                    let mut lines = audio.values();
                    for rf in req
                        .references
                        .iter_mut()
                        .filter(|x| x.kind == MediaKind::Audio)
                    {
                        let (path, id) = lines.next().context("a line's audio is missing")?;
                        rf.path = path.clone();
                        inputs.push(id.to_string());
                    }
                }
            }
            StepKind::Process => {
                let (v, vid) = video.clone().context("no video to work on yet")?;
                req.inputs.insert("video".into(), v);
                inputs.push(vid.to_string());
                if req.inputs.contains_key("audio") {
                    let (a, aid) = audio.get(&0).context("the line's audio is missing")?;
                    req.inputs.insert("audio".into(), a.clone());
                    inputs.push(aid.to_string());
                }
            }
        }

        let done = match (&sp.reused, job) {
            (Some(old), _) => old.clone(),
            (None, Some(mut job)) => {
                job.inputs = inputs;
                job.params = toml::Table::try_from(&req.params).unwrap_or_default();
                let plugin = plugins.plugin(&sp.plugin)?;
                let explain = |e: PluginErr| plugin_error(e, sp, &plan.built).to_string();
                let (job, err) = run.job(plugin, job, &req, &explain, &mut on_event).await?;
                if let Some(e) = err {
                    // Nothing after this runs; give back what it held.
                    let now = project.clock.now();
                    let id = job.id.to_string();
                    for later in held.iter().skip_while(|h| **h != id).skip(1) {
                        books.release(now, later)?;
                    }
                    let err = if n > 1 {
                        format!("{}: {e}", sp.what)
                    } else {
                        e
                    };
                    return outcome(project, r, take_id, job, Some(err));
                }
                job
            }
            (None, None) => unreachable!("every step that isn't reused has a job"),
        };
        match sp.kind {
            StepKind::Line(i) => {
                audio.insert(i, (output(project, &done)?, done.id));
            }
            _ => video = Some((output(project, &done)?, done.id)),
        }
        last = Some(done);
    }
    let last = last.context("nothing ran")?;
    outcome(project, r, take_id, last, None)
}

fn outcome(
    project: &Project,
    r: ShotRef,
    take: Ulid,
    job: Job,
    error: Option<String>,
) -> Result<Outcome> {
    let take = project
        .takes(r)?
        .into_iter()
        .find(|t| t.id == take)
        .context("take vanished")?;
    Ok(Outcome { take, job, error })
}

/// Runs one operation outside any shot, e.g. `shotline run video.upscale`. Reserves and books
/// like a take.
pub async fn run_once(
    project: &Project,
    books: &Books,
    plugin: (&str, &dyn Plugin),
    request: &Request,
    quote: &Quote,
    opts: &Options,
    mut on_event: impl FnMut(Event),
) -> Result<(Job, Option<String>)> {
    let (name, plugin) = plugin;
    let mut job = Job::new(request.op.clone(), name, &request.model);
    job.id = project.clock.ulid();
    job.inputs = request
        .inputs
        .values()
        .map(|p| project.rel(p))
        .chain(request.references.iter().map(|r| project.rel(&r.path)))
        .collect();
    job.params = toml::Table::try_from(&request.params).unwrap_or_default();
    let r = books.reserve(project.clock.now(), &context(&job), quote)?;
    on_event(Event::Reserved {
        total: r.base,
        currency: r.base_currency,
    });
    let explain = |e: PluginErr| match e {
        PluginErr::Plugin(pe) => match &pe.field {
            Some(f) => format!("{}: {} ({f})", pe.kind, pe.message),
            None => format!("{}: {}", pe.kind, pe.message),
        },
        other => other.to_string(),
    };
    let run = Run {
        project,
        books,
        opts,
    };
    run.job(plugin, job, request, &explain, &mut on_event).await
}

/// "0.4 USD" or "30 fake-credit (0.3 USD)".
pub fn price(quote: &Quote, base: f64, currency: &str) -> String {
    if quote.unit == currency {
        format!("{} {}", money(quote.amount), quote.unit)
    } else {
        format!(
            "{} {} ({} {currency})",
            money(quote.amount),
            quote.unit,
            money(base)
        )
    }
}
