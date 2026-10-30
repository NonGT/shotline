//! ElevenLabs text to speech.
//!
//! `POST /v1/text-to-speech/{voice_id}?output_format=mp3_44100_128` with `xi-api-key` answers with
//! the audio straight away, so `run` does the work and `poll` just says it's done. ElevenLabs bills
//! in credits per character; quotes count one credit per character (`elevenlabs-credit`).
//! `GET /v1/user/subscription` (needs a key with `user_read`) gives what a credit costs, the next
//! invoice over the month's credits, and how many credits are left.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use shotline_plugin::protocol::{
    Amount, Balance, Description, ErrorKind, MediaKind, ModelInfo, Op, Output, Outputs,
    PROTOCOL_VERSION, PluginError, Quote, Rate, Rates, Request, State, Status,
};
use shotline_plugin::{Error, Plugin, Result};

pub const KEY_VAR: &str = "ELEVENLABS_API_KEY";
pub const UNIT: &str = "elevenlabs-credit";
const BASE: &str = "https://api.elevenlabs.io/v1";

/// eleven_multilingual_v2 doesn't read Thai; eleven_v4 does.
const MODELS: &[&str] = &[
    "eleven_v4",
    "eleven_v3",
    "eleven_multilingual_v2",
    "eleven_flash_v2_5",
    "eleven_turbo_v2_5",
];

pub fn models() -> Vec<ModelInfo> {
    MODELS
        .iter()
        .map(|id| ModelInfo {
            id: (*id).into(),
            op: Op::AudioTts,
            ..ModelInfo::default()
        })
        .collect()
}

pub struct ElevenLabs {
    key: Option<String>,
    base: String,
    http: reqwest::Client,
    jobs: Mutex<HashMap<String, Vec<u8>>>,
    next: Mutex<u64>,
}

pub fn from_env() -> ElevenLabs {
    ElevenLabs::new(std::env::var(KEY_VAR).ok(), BASE)
}

fn bad(kind: ErrorKind, msg: impl Into<String>, field: &str) -> Error {
    PluginError::new(kind, msg).field(field).into()
}

fn check(req: &Request) -> Result<()> {
    if req.op != Op::AudioTts {
        return Err(bad(
            ErrorKind::Unsupported,
            format!("only audio.tts, not {}", req.op),
            "op",
        ));
    }
    if !MODELS.contains(&req.model.as_str()) {
        return Err(bad(
            ErrorKind::Validation,
            format!("no model {}", req.model),
            "model",
        ));
    }
    if req.prompt.as_deref().unwrap_or("").trim().is_empty() {
        return Err(bad(ErrorKind::Validation, "nothing to say", "prompt"));
    }
    Ok(())
}

/// The voice settings shotline passes: `{"voice_id": ..., "settings": {...}}`.
fn voice_id(req: &Request) -> Result<&str> {
    req.voice
        .as_ref()
        .and_then(|v| v.get("voice_id"))
        .and_then(Value::as_str)
        .ok_or_else(|| bad(ErrorKind::Validation, "needs a voice_id", "voice"))
}

pub fn body(req: &Request) -> Result<Map<String, Value>> {
    let mut m = Map::new();
    m.insert("text".into(), json!(req.prompt.clone().unwrap_or_default()));
    m.insert("model_id".into(), json!(req.model));
    if let Some(s) = req.voice.as_ref().and_then(|v| v.get("settings")) {
        m.insert("voice_settings".into(), s.clone());
    }
    if let Some(seed) = req.seed {
        m.insert("seed".into(), json!(seed));
    }
    for (k, v) in &req.params {
        m.insert(k.clone(), v.clone());
    }
    Ok(m)
}

pub fn map_error(status: u16, body: &Value) -> PluginError {
    let text = body.to_string();
    let kind = if text.contains("quota_exceeded") || status == 402 || status == 429 {
        ErrorKind::Quota
    } else {
        match status {
            401 | 403 => ErrorKind::Auth,
            404 => ErrorKind::NotFound,
            400 | 422 => ErrorKind::Validation,
            _ => ErrorKind::Other,
        }
    };
    let detail = body.get("detail");
    if let Some(items) = detail.and_then(Value::as_array) {
        let first = items.first();
        let msg = first
            .and_then(|d| d.get("msg"))
            .and_then(Value::as_str)
            .unwrap_or("invalid request");
        let loc: Vec<&str> = first
            .and_then(|d| d.get("loc"))
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(Value::as_str)
                    .filter(|s| *s != "body")
                    .collect()
            })
            .unwrap_or_default();
        let field = loc.first().map(|f| match *f {
            "text" => "prompt".to_string(),
            "model_id" => "model".to_string(),
            "voice_settings" | "voice_id" => "voice".to_string(),
            other => format!("params.{other}"),
        });
        let mut e = PluginError::new(kind, msg);
        e.field = field;
        return e;
    }
    let msg = detail
        .and_then(|d| d.get("message"))
        .and_then(Value::as_str)
        .or_else(|| detail.and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| format!("ElevenLabs answered HTTP {status}"));
    let mut e = PluginError::new(kind, msg);
    if detail.and_then(|d| d.get("status")).and_then(Value::as_str) == Some("voice_not_found") {
        e.kind = ErrorKind::Validation;
        e.field = Some("voice".into());
    }
    e
}

impl ElevenLabs {
    pub fn new(key: Option<String>, base: &str) -> ElevenLabs {
        ElevenLabs {
            key,
            base: base.trim_end_matches('/').into(),
            http: reqwest::Client::new(),
            jobs: Mutex::new(HashMap::new()),
            next: Mutex::new(0),
        }
    }
}

impl ElevenLabs {
    fn key(&self) -> Result<&str> {
        self.key.as_deref().ok_or_else(|| {
            Error::from(PluginError::new(
                ErrorKind::Auth,
                format!("{KEY_VAR} isn't set"),
            ))
        })
    }

    async fn subscription(&self) -> Result<Value> {
        let r = self
            .http
            .get(format!("{}/user/subscription", self.base))
            .header("xi-api-key", self.key()?)
            .send()
            .await
            .map_err(|e| {
                PluginError::new(ErrorKind::Other, format!("can't reach ElevenLabs: {e}"))
            })?;
        let status = r.status().as_u16();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        if status >= 300 {
            return Err(map_error(status, &v).into());
        }
        Ok(v)
    }
}

/// What a credit costs: the next invoice spread over the month's credits. Nothing on a free plan.
pub fn credit_rate(sub: &Value) -> Option<Rate> {
    let cents = sub.pointer("/next_invoice/amount_due_cents")?.as_f64()?;
    let credits = sub.get("character_limit")?.as_f64()?;
    let currency = sub.get("currency")?.as_str()?.to_uppercase();
    (cents > 0.0 && credits > 0.0).then(|| Rate {
        unit: UNIT.into(),
        amount: cents / 100.0 / credits,
        currency,
    })
}

fn not_found(job: &str) -> Error {
    PluginError::new(ErrorKind::NotFound, format!("no job {job}")).into()
}

#[async_trait]
impl Plugin for ElevenLabs {
    async fn describe(&self) -> Result<Description> {
        Ok(Description {
            name: "elevenlabs".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: PROTOCOL_VERSION,
            env: vec![KEY_VAR.into()],
            models: models(),
        })
    }

    async fn quote(&self, req: &Request) -> Result<Quote> {
        check(req)?;
        let chars = req.prompt.as_deref().unwrap_or("").chars().count() as f64;
        Ok(Quote {
            amount: chars,
            unit: UNIT.into(),
            units: Some(chars),
            unit_name: Some("chars".into()),
        })
    }

    async fn run(&self, req: &Request, _output_dir: &Path) -> Result<String> {
        check(req)?;
        let voice = voice_id(req)?;
        let key = self.key()?;
        let r = self
            .http
            .post(format!(
                "{}/text-to-speech/{voice}?output_format=mp3_44100_128",
                self.base
            ))
            .header("xi-api-key", key)
            .json(&Value::Object(body(req)?))
            .send()
            .await
            .map_err(|e| {
                PluginError::new(ErrorKind::Other, format!("can't reach ElevenLabs: {e}"))
            })?;
        let status = r.status().as_u16();
        let bytes = r
            .bytes()
            .await
            .map_err(|e| PluginError::new(ErrorKind::Other, e.to_string()))?;
        if status >= 300 {
            let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            return Err(map_error(status, &v).into());
        }
        let mut n = self.next.lock().unwrap();
        *n += 1;
        let job = format!("tts-{n}");
        self.jobs
            .lock()
            .unwrap()
            .insert(job.clone(), bytes.to_vec());
        Ok(job)
    }

    async fn poll(&self, job: &str) -> Result<Status> {
        if !self.jobs.lock().unwrap().contains_key(job) {
            return Err(not_found(job));
        }
        Ok(Status {
            state: State::Done,
            progress: Some(1.0),
            error: None,
            cost: None,
        })
    }

    async fn fetch(&self, job: &str, output_dir: &Path) -> Result<Outputs> {
        let bytes = self
            .jobs
            .lock()
            .unwrap()
            .remove(job)
            .ok_or_else(|| not_found(job))?;
        let io = |e: std::io::Error| Error::from(PluginError::new(ErrorKind::Other, e.to_string()));
        std::fs::create_dir_all(output_dir).map_err(io)?;
        std::fs::write(output_dir.join("out.mp3"), bytes).map_err(io)?;
        Ok(Outputs {
            outputs: vec![Output {
                file: "out.mp3".into(),
                kind: MediaKind::Audio,
            }],
            data: None,
        })
    }

    async fn cancel(&self, job: &str) -> Result<()> {
        self.jobs
            .lock()
            .unwrap()
            .remove(job)
            .ok_or_else(|| not_found(job))?;
        Ok(())
    }

    async fn rates(&self) -> Result<Rates> {
        Ok(Rates {
            rates: credit_rate(&self.subscription().await?)
                .into_iter()
                .collect(),
        })
    }

    async fn balance(&self) -> Result<Balance> {
        let sub = self.subscription().await?;
        let n = |k: &str| sub.get(k).and_then(Value::as_f64);
        let (Some(limit), Some(used)) = (n("character_limit"), n("character_count")) else {
            return Err(PluginError::new(
                ErrorKind::Other,
                format!("ElevenLabs sent no credit counts: {sub}"),
            )
            .into());
        };
        Ok(Balance {
            balances: vec![Amount {
                amount: limit - used,
                unit: UNIT.into(),
            }],
        })
    }
}
