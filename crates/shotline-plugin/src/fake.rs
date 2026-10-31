//! A plugin that pretends to generate things, for tests and dry runs. Behaviour is steered per
//! request through `params`:
//!
//! - `fake_reject = "<kind>"`: quote and run fail right away with that error kind
//! - `fake_reject_run = "<kind>"`: quote works, run fails right away
//! - `fake_fail = "<kind>"`: the job runs, then fails with that kind
//! - `fake_charge_on_fail = true`: a failed job still reports a cost
//! - `fake_polls = n`: polls that say running before it finishes (default 1)
//! - `fake_overcharge = x`: the balance drops by x times the price instead of the price
//! - `fake_no_drop = true`: the balance never drops, as if billing were slow
//!
//! `usage` answers with the contents of `shotline-fake-usage.json` in the working directory, if
//! there is one. The balance starts at 100 USD for each process. `fake/metered` quotes in
//! `fake-second`, which has no rate anywhere; the balance drops 0.03 USD a second. `fake/text`
//! answers with the last line of its prompt, and `fake/tts` with a silent WAV a twentieth of a
//! second long per character.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Value, json};
use shotline_protocol::{
    Amount, Balance, Description, ErrorKind, MediaKind, ModelInfo, Op, Output, Outputs,
    PROTOCOL_VERSION, PluginError, Quote, Rate, Rates, Request, State, Status, Usage,
};

use crate::{Plugin, Result};

pub const KEY_VAR: &str = "SHOTLINE_FAKE_KEY";

pub struct FakePlugin {
    jobs: Mutex<HashMap<String, Job>>,
    next: Mutex<u64>,
    balance: Mutex<f64>,
}

impl Default for FakePlugin {
    fn default() -> Self {
        Self {
            jobs: Mutex::default(),
            next: Mutex::default(),
            balance: Mutex::new(100.0),
        }
    }
}

struct Job {
    req: Request,
    polls_left: u64,
    fail: Option<ErrorKind>,
    charge_on_fail: bool,
    cancelled: bool,
    billed: bool,
}

fn models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            id: "fake/video".into(),
            op: Op::VideoGenerate,
            ref_image: Some("Ref {n}".into()),
            max_images: Some(4),
            max_audio: Some(1),
            min_duration: Some(1.0),
            max_duration: Some(10.0),
            resolutions: vec!["480P".into(), "720P".into()],
            aspects: vec!["16:9".into(), "9:16".into()],
            start_frame: true,
            start_frame_with_refs: true,
            audio_input: true,
            ..ModelInfo::default()
        },
        ModelInfo {
            id: "fake/credits".into(),
            op: Op::VideoGenerate,
            ref_image: Some("<Pic {n}>".into()),
            max_images: Some(2),
            min_duration: Some(2.0),
            max_duration: Some(6.0),
            whole_seconds: true,
            resolutions: vec!["480P".into()],
            aspects: vec!["9:16".into()],
            ..ModelInfo::default()
        },
        ModelInfo {
            id: "fake/tts".into(),
            op: Op::AudioTts,
            ..ModelInfo::default()
        },
        ModelInfo {
            id: "fake/lipsync".into(),
            op: Op::VideoLipsync,
            ..ModelInfo::default()
        },
        ModelInfo {
            id: "fake/metered".into(),
            op: Op::VideoUpscale,
            ..ModelInfo::default()
        },
        ModelInfo {
            id: "fake/smooth".into(),
            op: Op::VideoInterpolate,
            ..ModelInfo::default()
        },
        ModelInfo {
            id: "fake/text".into(),
            op: Op::TextGenerate,
            ..ModelInfo::default()
        },
        ModelInfo {
            id: "fake/image".into(),
            op: Op::ImageGenerate,
            ref_image: Some("image {n}".into()),
            max_images: Some(4),
            aspects: vec!["1:1".into(), "9:16".into(), "16:9".into()],
            ..ModelInfo::default()
        },
    ]
}

/// 16-bit mono 16 kHz WAV of silence.
fn silence(secs: f64) -> Vec<u8> {
    let rate = 16_000u32;
    let data = (secs * rate as f64) as u32 * 2;
    let mut w = Vec::with_capacity(44 + data as usize);
    w.extend(b"RIFF");
    w.extend((36 + data).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    w.extend(1u16.to_le_bytes());
    w.extend(1u16.to_le_bytes());
    w.extend(rate.to_le_bytes());
    w.extend((rate * 2).to_le_bytes());
    w.extend(2u16.to_le_bytes());
    w.extend(16u16.to_le_bytes());
    w.extend(b"data");
    w.extend(data.to_le_bytes());
    w.resize(44 + data as usize, 0);
    w
}

fn kind_param(req: &Request, key: &str) -> Result<Option<ErrorKind>> {
    match req.params.get(key) {
        None => Ok(None),
        Some(v) => serde_json::from_value(v.clone()).map(Some).map_err(|_| {
            PluginError::new(ErrorKind::Validation, format!("bad {key}"))
                .field(format!("params.{key}"))
                .into()
        }),
    }
}

fn check(req: &Request) -> Result<ModelInfo> {
    if let Some(kind) = kind_param(req, "fake_reject")? {
        return Err(PluginError::new(kind, "rejected on request")
            .field("params.fake_reject")
            .into());
    }
    let Some(m) = models().into_iter().find(|m| m.id == req.model) else {
        return Err(
            PluginError::new(ErrorKind::Validation, format!("no model {}", req.model))
                .field("model")
                .into(),
        );
    };
    if m.op != req.op {
        return Err(
            PluginError::new(ErrorKind::Validation, format!("{} does {}", m.id, m.op))
                .field("op")
                .into(),
        );
    }
    if let Some(max) = m.max_images
        && req.references.len() > max
    {
        return Err(PluginError::new(
            ErrorKind::Validation,
            format!("at most {max} reference images"),
        )
        .field(format!("references[{max}]"))
        .into());
    }
    let needs: &[&str] = match m.op {
        Op::VideoLipsync => &["video", "audio"],
        _ => &[],
    };
    for n in needs {
        if !req.inputs.contains_key(*n) {
            return Err(
                PluginError::new(ErrorKind::Validation, format!("needs {n}"))
                    .field(format!("inputs.{n}"))
                    .into(),
            );
        }
    }
    Ok(m)
}

fn price(req: &Request) -> Quote {
    let secs = req.duration.unwrap_or(5.0);
    match req.model.as_str() {
        "fake/credits" => Quote {
            amount: 10.0 * secs,
            unit: "fake-credit".into(),
            units: Some(secs),
            unit_name: Some("s".into()),
        },
        "fake/tts" => {
            let chars = req.prompt.as_deref().unwrap_or("").chars().count() as f64;
            Quote {
                amount: 0.001 * chars,
                unit: "USD".into(),
                units: Some(chars),
                unit_name: Some("chars".into()),
            }
        }
        "fake/metered" => Quote {
            amount: secs,
            unit: "fake-second".into(),
            units: Some(secs),
            unit_name: Some("s".into()),
        },
        "fake/image" => Quote {
            amount: 0.04,
            unit: "USD".into(),
            units: Some(1.0),
            unit_name: Some("image".into()),
        },
        "fake/text" => Quote {
            amount: 0.001,
            unit: "USD".into(),
            units: None,
            unit_name: None,
        },
        "fake/smooth" => Quote {
            amount: 0.0,
            unit: "USD".into(),
            units: Some(secs),
            unit_name: Some("s".into()),
        },
        "fake/lipsync" => Quote {
            amount: 0.05 * secs,
            unit: "USD".into(),
            units: Some(secs),
            unit_name: Some("s".into()),
        },
        _ => Quote {
            amount: 0.1 * secs,
            unit: "USD".into(),
            units: Some(secs),
            unit_name: Some("s".into()),
        },
    }
}

/// What the account really loses for a job, in USD.
fn billed(req: &Request) -> f64 {
    if req.params.get("fake_no_drop") == Some(&json!(true)) {
        return 0.0;
    }
    let q = price(req);
    let usd = match q.unit.as_str() {
        "fake-credit" => q.amount * 0.01,
        "fake-second" => q.amount * 0.03,
        _ => q.amount,
    };
    let factor = req
        .params
        .get("fake_overcharge")
        .and_then(Value::as_f64)
        .unwrap_or(1.0);
    usd * factor
}

fn not_found(job: &str) -> crate::Error {
    PluginError::new(ErrorKind::NotFound, format!("no job {job}")).into()
}

#[async_trait]
impl Plugin for FakePlugin {
    async fn describe(&self) -> Result<Description> {
        Ok(Description {
            name: "fake".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: PROTOCOL_VERSION,
            env: vec![KEY_VAR.into()],
            models: models(),
        })
    }

    async fn quote(&self, req: &Request) -> Result<Quote> {
        check(req)?;
        Ok(price(req))
    }

    async fn run(&self, req: &Request, _output_dir: &Path) -> Result<String> {
        check(req)?;
        if let Some(kind) = kind_param(req, "fake_reject_run")? {
            return Err(PluginError::new(kind, "rejected on submit")
                .field("references[0]")
                .into());
        }
        let fail = kind_param(req, "fake_fail")?;
        let polls = req.params.get("fake_polls").and_then(Value::as_u64);
        let charge_on_fail = req.params.get("fake_charge_on_fail") == Some(&json!(true));
        let mut next = self.next.lock().unwrap();
        *next += 1;
        let id = format!("fake-{next}");
        self.jobs.lock().unwrap().insert(
            id.clone(),
            Job {
                req: req.clone(),
                polls_left: polls.unwrap_or(1),
                fail,
                charge_on_fail,
                cancelled: false,
                billed: false,
            },
        );
        Ok(id)
    }

    async fn poll(&self, job: &str) -> Result<Status> {
        let mut jobs = self.jobs.lock().unwrap();
        let j = jobs.get_mut(job).ok_or_else(|| not_found(job))?;
        if j.cancelled {
            return Ok(Status {
                state: State::Cancelled,
                progress: None,
                error: None,
                cost: None,
            });
        }
        if j.polls_left > 0 {
            j.polls_left -= 1;
            return Ok(Status {
                state: State::Running,
                progress: Some(0.5),
                error: None,
                cost: None,
            });
        }
        let charged = j.fail.is_none() || j.charge_on_fail;
        if charged && !j.billed {
            j.billed = true;
            *self.balance.lock().unwrap() -= billed(&j.req);
        }
        Ok(match j.fail {
            Some(kind) => Status {
                state: State::Failed,
                progress: None,
                error: Some(PluginError::new(kind, "failed on request")),
                cost: j.charge_on_fail.then(|| price(&j.req)),
            },
            None => Status {
                state: State::Done,
                progress: Some(1.0),
                error: None,
                cost: Some(price(&j.req)),
            },
        })
    }

    async fn fetch(&self, job: &str, output_dir: &Path) -> Result<Outputs> {
        let req = {
            let jobs = self.jobs.lock().unwrap();
            jobs.get(job).ok_or_else(|| not_found(job))?.req.clone()
        };
        let (file, kind) = match req.op {
            Op::AudioTts | Op::AudioMusic | Op::AudioSfx => ("out.wav", MediaKind::Audio),
            Op::ImageGenerate | Op::ImageEdit => ("out.png", MediaKind::Image),
            Op::TextGenerate => ("out.txt", MediaKind::Data),
            _ => ("out.mp4", MediaKind::Video),
        };
        let prompt = req.prompt.as_deref().unwrap_or("").trim();
        let body = match req.op {
            Op::TextGenerate => format!("{}\n", prompt.lines().last().unwrap_or("")).into_bytes(),
            Op::AudioTts => silence(prompt.chars().count() as f64 / 20.0),
            _ => format!("fake {kind:?} from {job}\n").into_bytes(),
        };
        let io = |e: std::io::Error| PluginError::new(ErrorKind::Other, e.to_string());
        std::fs::create_dir_all(output_dir).map_err(io)?;
        std::fs::write(output_dir.join(file), body).map_err(io)?;
        let mut env: Vec<String> = std::env::vars().map(|(k, _)| k).collect();
        env.sort();
        Ok(Outputs {
            outputs: vec![Output {
                file: file.into(),
                kind,
            }],
            data: Some(json!({ "env": env })),
        })
    }

    async fn cancel(&self, job: &str) -> Result<()> {
        let mut jobs = self.jobs.lock().unwrap();
        jobs.get_mut(job).ok_or_else(|| not_found(job))?.cancelled = true;
        Ok(())
    }

    async fn rates(&self) -> Result<Rates> {
        Ok(Rates {
            rates: vec![Rate {
                unit: "fake-credit".into(),
                amount: 0.01,
                currency: "USD".into(),
            }],
        })
    }

    async fn balance(&self) -> Result<Balance> {
        Ok(Balance {
            balances: vec![Amount {
                amount: *self.balance.lock().unwrap(),
                unit: "USD".into(),
            }],
        })
    }

    async fn usage(&self, _since: &str) -> Result<Usage> {
        match std::fs::read_to_string("shotline-fake-usage.json") {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| PluginError::new(ErrorKind::Other, e.to_string()).into()),
            Err(_) => Ok(Usage::default()),
        }
    }
}
