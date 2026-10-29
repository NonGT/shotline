//! Running shots: quote, ask, reserve, run, poll, fetch, book. Shared by the CLI and later the MCP
//! server and UI.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use shotline_core::ledger::{Books, Context, Entry};
use shotline_core::protocol::{
    ErrorKind, MediaKind, Op, PluginError, Quote, Reference, Request, State, Status,
};
use shotline_core::{Built, Catalog, Cost, Job, JobStatus, Project, ShotRef, Take, build};
use shotline_plugin::{Error as PluginErr, Plugin};
use tokio::sync::Notify;

/// What a take of a shot would send and cost, worked out before anything is spent.
#[derive(Debug, Clone)]
pub struct Plan {
    pub shot: ShotRef,
    pub built: Built,
    pub request: Request,
    pub quote: Quote,
    /// The quote in the film's currency.
    pub base: f64,
    pub currency: String,
}

fn to_json(t: &toml::Table) -> serde_json::Map<String, serde_json::Value> {
    match serde_json::to_value(t) {
        Ok(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    }
}

/// Says which shot, element or file a plugin error is about.
pub fn explain(e: &PluginError, built: &Built) -> String {
    let what = e.field.as_deref().and_then(|f| {
        if let Some(i) = f
            .strip_prefix("references[")
            .and_then(|r| r.strip_suffix(']'))
            .and_then(|n| n.parse::<usize>().ok())
        {
            return built
                .references
                .get(i)
                .map(|r| format!("@{}'s anchor {} ({})", r.handle, r.path, r.label));
        }
        if let Some(p) = f.strip_prefix("params.") {
            return Some(format!("[model.params] {p} in shot {}", built.shot));
        }
        Some(match f {
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
        })
    });
    match what {
        Some(w) => format!("{}: {} ({w})", e.kind, e.message),
        None => format!("{}: {}", e.kind, e.message),
    }
}

fn plugin_error(e: PluginErr, built: &Built) -> anyhow::Error {
    match e {
        PluginErr::Plugin(pe) => anyhow!(explain(&pe, built)),
        other => anyhow!(other),
    }
}

/// Builds the shot's prompt and request and asks the plugin for a price. Refuses things that
/// would fail anyway: a missing start frame, a price that can't be converted.
pub async fn plan(
    project: &Project,
    catalog: &Catalog,
    books: &Books,
    plugin: &dyn Plugin,
    r: ShotRef,
) -> Result<Plan> {
    let built = build(project, catalog, r)?;
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
    let request = Request {
        op: Op::VideoGenerate,
        model: built.model.clone(),
        prompt: Some(built.text.clone()),
        duration: Some(built.duration),
        resolution: Some(built.resolution.clone()),
        aspect: Some(built.aspect.clone()),
        seed: None,
        references: built
            .references
            .iter()
            .map(|x| Reference {
                kind: MediaKind::Image,
                path: project.root.join(&x.path),
            })
            .collect(),
        start_frame,
        end_frame: None,
        inputs: Default::default(),
        voice: None,
        params: to_json(&built.params),
    };
    let quote = plugin
        .quote(&request)
        .await
        .map_err(|e| plugin_error(e, &built))?;
    let base = books
        .rates
        .convert(quote.amount, &quote.unit, &books.currency)?
        .amount;
    Ok(Plan {
        shot: r,
        built,
        request,
        quote,
        base,
        currency: books.currency.clone(),
    })
}

#[derive(Debug)]
pub enum Event {
    Reserved(Box<Entry>),
    Submitted { plugin_job: String },
    Status(Status),
    Cancelling,
}

pub struct Options {
    pub poll_every: Duration,
    /// Notified to cancel the job (Ctrl-C).
    pub cancel: Arc<Notify>,
    /// Poll errors in a row before giving up on the job.
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
    pub job: Job,
    /// Why it didn't finish, if it didn't.
    pub error: Option<String>,
}

/// Runs a planned take: reserve, run, poll until finished, fetch, book what it cost. The caller
/// asks for confirmation first. Budget refusals happen before anything is sent.
pub async fn take(
    project: &Project,
    books: &Books,
    plugin: &dyn Plugin,
    plan: &Plan,
    opts: &Options,
    mut on_event: impl FnMut(Event),
) -> Result<Outcome> {
    let r = plan.shot;
    let mut job = Job::new(Op::VideoGenerate, &plan.built.plugin, &plan.built.model);
    job.id = project.clock.ulid();
    job.shot = Some(r);
    job.take = Some(project.new_take_id());
    job.inputs = plan
        .built
        .references
        .iter()
        .map(|x| x.path.clone())
        .collect();
    if let Some(sf) = &plan.built.start_frame {
        job.inputs.push(sf.path.clone());
    }
    job.params = plan.built.params.clone();

    let ctx = Context {
        job: job.id.to_string(),
        take: job.take.map(|t| t.to_string()),
        shot: Some(r.to_string()),
        chapter: Some(r.chapter),
        op: Op::VideoGenerate.to_string(),
        plugin: plan.built.plugin.clone(),
        model: plan.built.model.clone(),
    };
    let reserved = books.reserve(project.clock.now(), &ctx, &plan.quote)?;
    on_event(Event::Reserved(Box::new(reserved)));
    let mut job = project.create_job(job)?;
    let out_dir = project.job_dir(job.id);

    let finish = |job: &mut Job, status: JobStatus, error: Option<String>| -> Result<()> {
        job.status = status;
        job.error = error;
        project.save_job(job)?;
        Ok(())
    };

    let plugin_job = match plugin.run(&plan.request, &out_dir).await {
        Ok(pj) => pj,
        Err(e) => {
            let msg = plugin_error(e, &plan.built).to_string();
            books.release(project.clock.now(), &ctx.job)?;
            finish(&mut job, JobStatus::Failed, Some(msg.clone()))?;
            return outcome(project, job, Some(msg));
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
                    // Still held: it may well be running and charged. `cost` shows it as held
                    // until it's sorted out by hand.
                    let msg = format!("lost track of the job after {errors} failed polls: {e}");
                    finish(&mut job, JobStatus::Failed, Some(msg.clone()))?;
                    return outcome(project, job, Some(msg));
                }
            }
        }
    };

    let now = project.clock.now();
    let book = |cost: Option<&Quote>| -> Result<Option<Entry>> {
        match cost {
            Some(c) => Ok(Some(books.charge(
                now,
                &ctx.job,
                Some(&plugin_job),
                Some(c),
            )?)),
            None => {
                books.release(now, &ctx.job)?;
                Ok(None)
            }
        }
    };
    let cost_of = |e: &Entry| Cost {
        amount: e.amount,
        unit: e.unit.clone(),
    };

    match status.state {
        State::Done => {
            // Done means money was spent, whatever happens with the download.
            let charged = books.charge(now, &ctx.job, Some(&plugin_job), status.cost.as_ref())?;
            job.cost = Some(cost_of(&charged));
            match plugin.fetch(&plugin_job, &out_dir).await {
                Ok(out) => {
                    job.outputs = out.outputs.into_iter().map(|o| o.file).collect();
                    finish(&mut job, JobStatus::Done, None)?;
                    outcome(project, job, None)
                }
                Err(e) => {
                    let msg = format!(
                        "finished but the download failed: {}",
                        plugin_error(e, &plan.built)
                    );
                    finish(&mut job, JobStatus::Failed, Some(msg.clone()))?;
                    outcome(project, job, Some(msg))
                }
            }
        }
        State::Failed | State::Cancelled => {
            if let Some(e) = book(status.cost.as_ref())? {
                job.cost = Some(cost_of(&e));
            }
            let (st, msg) = if status.state == State::Cancelled || cancelled {
                (JobStatus::Cancelled, "cancelled".to_string())
            } else {
                let msg = match &status.error {
                    Some(pe) => explain(pe, &plan.built),
                    None => "the job failed".into(),
                };
                (JobStatus::Failed, msg)
            };
            finish(&mut job, st, Some(msg.clone()))?;
            outcome(project, job, Some(msg))
        }
        State::Queued | State::Running => unreachable!("loop only ends on a finished state"),
    }
}

fn outcome(project: &Project, job: Job, error: Option<String>) -> Result<Outcome> {
    let shot = job.shot.context("job without a shot")?;
    let take_id = job.take.context("job without a take")?;
    let take = project
        .takes(shot)?
        .into_iter()
        .find(|t| t.id == take_id)
        .context("take vanished")?;
    Ok(Outcome { take, job, error })
}
