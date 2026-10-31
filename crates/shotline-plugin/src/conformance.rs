//! Runs a plugin through the protocol and checks it behaves. External plugins can use this from
//! their own tests with their HTTP mocked.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use shotline_protocol::{
    Description, ErrorKind, MediaKind, ModelInfo, Op, PROTOCOL_VERSION, Request, State,
};

use crate::{Error, Plugin};

#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub problem: Option<String>,
}

pub struct Options<'a> {
    /// Also run a job per model. Never do this against a live paid API.
    pub run_jobs: bool,
    /// Scratch space for input files and outputs.
    pub work_dir: &'a Path,
    pub poll_interval: Duration,
    pub max_polls: u32,
}

impl<'a> Options<'a> {
    pub fn offline(work_dir: &'a Path) -> Self {
        Self {
            run_jobs: false,
            work_dir,
            poll_interval: Duration::from_millis(10),
            max_polls: 100,
        }
    }
}

struct Report(Vec<Check>);

impl Report {
    fn add(&mut self, name: impl Into<String>, problem: Option<String>) {
        self.0.push(Check {
            name: name.into(),
            problem,
        });
    }
}

pub fn passed(checks: &[Check]) -> bool {
    checks.iter().all(|c| c.problem.is_none())
}

pub async fn check(plugin: &dyn Plugin, opts: &Options<'_>) -> Vec<Check> {
    let mut r = Report(vec![]);
    let desc = match plugin.describe().await {
        Ok(d) => d,
        Err(e) => {
            r.add("describe", Some(e.to_string()));
            return r.0;
        }
    };
    r.add("describe", describe_problem(&desc));

    for m in &desc.models {
        let req = match sample_request(m, opts.work_dir) {
            Ok(req) => req,
            Err(e) => {
                r.add(format!("{} inputs", m.id), Some(e.to_string()));
                continue;
            }
        };
        let problem = match plugin.quote(&req).await {
            Ok(q) if !q.amount.is_finite() || q.amount < 0.0 => {
                Some(format!("bad amount {}", q.amount))
            }
            Ok(q) if q.unit.trim().is_empty() => Some("empty unit".into()),
            Ok(_) => None,
            Err(e) => Some(e.to_string()),
        };
        r.add(format!("quote {}", m.id), problem);

        if opts.run_jobs {
            r.add(
                format!("run {}", m.id),
                run_problem(plugin, m, &req, opts).await,
            );
        }
    }

    // A sample request for an operation the plugin does, with a model it doesn't have.
    let mut bogus = desc
        .models
        .first()
        .and_then(|m| sample_request(m, opts.work_dir).ok())
        .unwrap_or_default();
    bogus.model = "no/such-model".into();
    r.add(
        "unknown model is a validation error on `model`",
        match plugin.quote(&bogus).await {
            Err(Error::Plugin(e))
                if matches!(e.kind, ErrorKind::Validation | ErrorKind::NotFound)
                    && e.field.as_deref() == Some("model") =>
            {
                None
            }
            Err(e) => Some(format!("got {e}")),
            Ok(_) => Some("quoted a model that doesn't exist".into()),
        },
    );

    r.add(
        "unknown job is not_found",
        match plugin.poll("no-such-job").await {
            Err(Error::Plugin(e)) if e.kind == ErrorKind::NotFound => None,
            Err(e) => Some(format!("got {e}")),
            Ok(_) => Some("polled a job that doesn't exist".into()),
        },
    );

    match plugin.balance().await {
        Err(Error::Unsupported(_)) => {}
        Ok(b) => r.add(
            "balance",
            b.balances
                .iter()
                .find(|a| !a.amount.is_finite() || a.unit.trim().is_empty())
                .map(|a| format!("bad balance {} {:?}", a.amount, a.unit)),
        ),
        Err(e) => r.add("balance", Some(e.to_string())),
    }
    r.0
}

fn describe_problem(d: &Description) -> Option<String> {
    if d.protocol != PROTOCOL_VERSION {
        return Some(format!(
            "speaks protocol {}, shotline speaks {PROTOCOL_VERSION}",
            d.protocol
        ));
    }
    if d.name.trim().is_empty() {
        return Some("empty name".into());
    }
    if d.models.is_empty() {
        return Some("no models".into());
    }
    let mut seen = HashSet::new();
    for m in &d.models {
        if !seen.insert((&m.id, &m.op)) {
            return Some(format!("{} {} listed twice", m.id, m.op));
        }
        if let Some(t) = &m.ref_image
            && !t.contains("{n}")
        {
            return Some(format!("{} ref_image has no {{n}}", m.id));
        }
        if let (Some(lo), Some(hi)) = (m.min_duration, m.max_duration)
            && lo > hi
        {
            return Some(format!("{} min_duration > max_duration", m.id));
        }
    }
    None
}

fn output_kind(op: &Op) -> Option<MediaKind> {
    Some(match op {
        Op::ImageGenerate | Op::ImageEdit => MediaKind::Image,
        Op::VideoGenerate | Op::VideoLipsync | Op::VideoUpscale | Op::VideoInterpolate => {
            MediaKind::Video
        }
        Op::AudioTts | Op::AudioMusic | Op::AudioSfx => MediaKind::Audio,
        Op::AudioTranscribe | Op::TextGenerate => MediaKind::Data,
        Op::Custom(_) => return None,
    })
}

/// Smallest request a model should accept. Input files are placeholders, fine for mocked APIs.
fn sample_request(m: &ModelInfo, dir: &Path) -> std::io::Result<Request> {
    let input = |name: &str| -> std::io::Result<std::path::PathBuf> {
        std::fs::create_dir_all(dir)?;
        let p = dir.join(name);
        std::fs::write(&p, b"placeholder")?;
        Ok(p)
    };
    let mut req = Request {
        op: m.op.clone(),
        model: m.id.clone(),
        duration: m.min_duration.or(Some(5.0)),
        resolution: m.resolutions.first().cloned(),
        aspect: m.aspects.first().cloned(),
        ..Request::default()
    };
    match m.op {
        Op::ImageGenerate
        | Op::VideoGenerate
        | Op::AudioMusic
        | Op::AudioSfx
        | Op::TextGenerate => req.prompt = Some("conformance test".into()),
        Op::AudioTts => req.prompt = Some("conformance test".into()),
        Op::ImageEdit => {
            req.prompt = Some("conformance test".into());
            req.inputs.insert("image".into(), input("in.png")?);
        }
        Op::VideoLipsync => {
            req.inputs.insert("video".into(), input("in.mp4")?);
            req.inputs.insert("audio".into(), input("in.wav")?);
        }
        Op::VideoUpscale | Op::VideoInterpolate => {
            req.inputs.insert("video".into(), input("in.mp4")?);
        }
        Op::AudioTranscribe => {
            req.inputs.insert("audio".into(), input("in.wav")?);
        }
        Op::Custom(_) => {}
    }
    Ok(req)
}

async fn run_problem(
    plugin: &dyn Plugin,
    m: &ModelInfo,
    req: &Request,
    opts: &Options<'_>,
) -> Option<String> {
    let out = opts.work_dir.join("out").join(m.id.replace('/', "_"));
    let job = match plugin.run(req, &out).await {
        Ok(j) => j,
        Err(e) => return Some(format!("run: {e}")),
    };
    let mut status = None;
    for _ in 0..opts.max_polls {
        match plugin.poll(&job).await {
            Ok(s) if s.state.is_finished() => {
                status = Some(s);
                break;
            }
            Ok(_) => tokio::time::sleep(opts.poll_interval).await,
            Err(e) => return Some(format!("poll: {e}")),
        }
    }
    let Some(status) = status else {
        return Some(format!("not finished after {} polls", opts.max_polls));
    };
    if status.state != State::Done {
        return Some(format!("ended {:?}: {:?}", status.state, status.error));
    }
    let outputs = match plugin.fetch(&job, &out).await {
        Ok(o) => o,
        Err(e) => return Some(format!("fetch: {e}")),
    };
    if outputs.outputs.is_empty() && outputs.data.is_none() {
        return Some("fetch returned nothing".into());
    }
    for o in &outputs.outputs {
        if !out.join(&o.file).is_file() {
            return Some(format!("{} isn't in the output folder", o.file));
        }
        if let Some(k) = output_kind(&m.op)
            && o.kind != k
            && o.kind != MediaKind::Data
        {
            return Some(format!("{} is {:?}, expected {k:?}", o.file, o.kind));
        }
    }
    None
}
