//! Venice video generation.
//!
//! `POST /video/quote` is free, `POST /video/queue` starts a job, `POST /video/retrieve` answers
//! with JSON while it runs and with the video itself once it's done. Durations are whole seconds
//! written as `"8s"`. Venice won't take a start frame (`image_url`) or end frame together with
//! reference images. Errors look like `{"error": "...", "details": {"field": {"_errors": [...]}}}`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use shotline_plugin::protocol::{
    Description, ErrorKind, MediaKind, ModelInfo, Op, Output, Outputs, PROTOCOL_VERSION,
    PluginError, Quote, Request, State, Status,
};
use shotline_plugin::util::data_url;
use shotline_plugin::{Error, Plugin, Result};

pub const KEY_VAR: &str = "VENICE_API_KEY";
const BASE: &str = "https://api.venice.ai/api/v1";

pub fn models() -> Vec<ModelInfo> {
    vec![ModelInfo {
        id: "minimax-h3-max-reference-to-video".into(),
        op: Op::VideoGenerate,
        ref_image: Some("<Picture {n}>".into()),
        max_images: Some(30),
        max_audio: Some(1),
        min_duration: Some(5.0),
        max_duration: Some(15.0),
        whole_seconds: true,
        resolutions: vec!["480P".into(), "768P".into(), "1080P".into()],
        aspects: ["adaptive", "21:9", "16:9", "4:3", "1:1", "3:4", "9:16"]
            .map(String::from)
            .to_vec(),
        start_frame: true,
        start_frame_with_refs: false,
        audio_input: true,
        ..ModelInfo::default()
    }]
}

pub struct Venice {
    key: Option<String>,
    base: String,
    http: reqwest::Client,
    /// Finished videos, kept from `poll` until `fetch`.
    done: Mutex<HashMap<String, Done>>,
    refs: Mutex<HashMap<String, Vec<usize>>>,
}

enum Done {
    Bytes(Vec<u8>),
    Url(String),
}

struct Resp {
    status: u16,
    content_type: String,
    body: Vec<u8>,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&self.body).into_owned()))
    }
}

pub fn from_env() -> Venice {
    Venice::new(std::env::var(KEY_VAR).ok(), BASE)
}

fn other(msg: impl Into<String>) -> Error {
    PluginError::new(ErrorKind::Other, msg).into()
}

impl Venice {
    pub fn new(key: Option<String>, base: &str) -> Venice {
        Venice {
            key,
            base: base.trim_end_matches('/').into(),
            http: reqwest::Client::new(),
            done: Mutex::new(HashMap::new()),
            refs: Mutex::new(HashMap::new()),
        }
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Resp> {
        let key = self
            .key
            .as_deref()
            .ok_or_else(|| Error::from(PluginError::new(ErrorKind::Auth, format!("{KEY_VAR} isn't set"))))?;
        let r = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(key)
            .json(body)
            .send()
            .await
            .map_err(|e| other(format!("can't reach Venice: {e}")))?;
        let status = r.status().as_u16();
        let content_type = r
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = r
            .bytes()
            .await
            .map_err(|e| other(format!("Venice dropped the connection: {e}")))?
            .to_vec();
        Ok(Resp {
            status,
            content_type,
            body,
        })
    }
}

/// Request fields Venice names differently.
fn field(name: &str, idx: Option<usize>, refs: &[usize]) -> String {
    match name {
        "reference_image_urls" => match idx {
            Some(i) => format!("references[{}]", refs.get(i).copied().unwrap_or(i)),
            None => "references".into(),
        },
        "image_url" => "start_frame".into(),
        "end_image_url" => "end_frame".into(),
        "aspect_ratio" => "aspect".into(),
        "audio_url" => "references".into(),
        "prompt" | "duration" | "resolution" | "model" | "seed" => name.into(),
        other => format!("params.{other}"),
    }
}

/// The first zod-style error inside `details`, with where it is.
fn first_detail(v: &Value, path: &mut Vec<String>) -> Option<(Vec<String>, String)> {
    let obj = v.as_object()?;
    if let Some(msg) = obj
        .get("_errors")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
    {
        return Some((path.clone(), msg.to_string()));
    }
    for (k, child) in obj {
        if k == "_errors" {
            continue;
        }
        path.push(k.clone());
        if let Some(hit) = first_detail(child, path) {
            return Some(hit);
        }
        path.pop();
    }
    None
}

pub fn map_error(status: u16, body: &Value, refs: &[usize]) -> PluginError {
    let text = body.to_string().to_lowercase();
    let kind = if text.contains("content policy") || text.contains("content_policy") || status == 422
    {
        ErrorKind::ContentPolicy
    } else {
        match status {
            401 | 403 => ErrorKind::Auth,
            402 | 429 => ErrorKind::Quota,
            404 => ErrorKind::NotFound,
            400 => ErrorKind::Validation,
            _ => ErrorKind::Other,
        }
    };
    let top = body
        .get("error")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| match body {
            Value::String(s) if !s.is_empty() => s.chars().take(300).collect(),
            _ => format!("Venice answered HTTP {status}"),
        });
    let detail = body.get("details").and_then(|d| first_detail(d, &mut vec![]));
    let Some((path, msg)) = detail else {
        return PluginError::new(kind, top);
    };
    let idx = path.get(1).and_then(|s| s.parse().ok());
    let f = path.first().map(|n| field(n, idx, refs));
    let mut e = PluginError::new(kind, format!("{top}: {msg}"));
    e.field = f;
    e
}

fn split(job: &str) -> Result<(&str, &str)> {
    job.split_once('#')
        .ok_or_else(|| PluginError::new(ErrorKind::NotFound, format!("no job {job}")).into())
}

fn duration(req: &Request) -> String {
    format!("{}s", req.duration.unwrap_or(5.0).round() as i64)
}

pub fn body(req: &Request) -> Result<(Map<String, Value>, Vec<usize>)> {
    if req.op != Op::VideoGenerate {
        return Err(PluginError::new(
            ErrorKind::Unsupported,
            format!("the Venice plugin only does video.generate, not {}", req.op),
        )
        .field("op")
        .into());
    }
    let mut m = Map::new();
    m.insert("model".into(), json!(req.model));
    m.insert("duration".into(), json!(duration(req)));
    if let Some(r) = &req.resolution {
        m.insert("resolution".into(), json!(r));
    }
    if let Some(a) = &req.aspect {
        m.insert("aspect_ratio".into(), json!(a));
    }
    if let Some(p) = &req.prompt {
        m.insert("prompt".into(), json!(p));
    }
    let mut images = vec![];
    let mut refs = vec![];
    let mut audio = None;
    for (i, r) in req.references.iter().enumerate() {
        let f = format!("references[{i}]");
        match r.kind {
            MediaKind::Image => {
                images.push(data_url(&r.path, &f)?);
                refs.push(i);
            }
            MediaKind::Audio if audio.is_none() => audio = Some(data_url(&r.path, &f)?),
            MediaKind::Audio => {
                return Err(PluginError::new(ErrorKind::Validation, "Venice takes one audio clip")
                    .field(f)
                    .into());
            }
            _ => {
                return Err(PluginError::new(ErrorKind::Validation, "Venice takes images and audio")
                    .field(f)
                    .into());
            }
        }
    }
    if !images.is_empty() {
        m.insert("reference_image_urls".into(), json!(images));
    }
    if let Some(a) = audio {
        m.insert("audio_url".into(), json!(a));
    }
    for (key, path, name) in [
        ("image_url", &req.start_frame, "start_frame"),
        ("end_image_url", &req.end_frame, "end_frame"),
    ] {
        if let Some(p) = path {
            if !images.is_empty() {
                return Err(PluginError::new(
                    ErrorKind::Validation,
                    "Venice doesn't take a start or end frame together with reference images",
                )
                .field(name)
                .into());
            }
            m.insert(key.into(), json!(data_url(p, name)?));
        }
    }
    for (k, v) in &req.params {
        m.insert(k.clone(), v.clone());
    }
    Ok((m, refs))
}

#[async_trait]
impl Plugin for Venice {
    async fn describe(&self) -> Result<Description> {
        Ok(Description {
            name: "venice".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: PROTOCOL_VERSION,
            env: vec![KEY_VAR.into()],
            models: models(),
        })
    }

    async fn quote(&self, req: &Request) -> Result<Quote> {
        let mut b = json!({"model": req.model, "duration": duration(req)});
        if let Some(r) = &req.resolution {
            b["resolution"] = json!(r);
        }
        let r = self.post("/video/quote", &b).await?;
        let v = r.json();
        if r.status >= 300 {
            let mut e = map_error(r.status, &v, &[]);
            if e.field.is_none() && matches!(r.status, 400 | 404) {
                e.kind = ErrorKind::Validation;
                e.field = Some("model".into());
            }
            return Err(e.into());
        }
        let amount = v
            .get("quote")
            .and_then(Value::as_f64)
            .ok_or_else(|| other(format!("Venice's quote has no price: {v}")))?;
        Ok(Quote {
            amount,
            unit: "USD".into(),
            units: req.duration,
            unit_name: Some("s".into()),
        })
    }

    async fn run(&self, req: &Request, _output_dir: &Path) -> Result<String> {
        let (b, refs) = body(req)?;
        let r = self.post("/video/queue", &Value::Object(b)).await?;
        let v = r.json();
        if r.status >= 300 {
            return Err(map_error(r.status, &v, &refs).into());
        }
        let id = v
            .get("queue_id")
            .and_then(Value::as_str)
            .ok_or_else(|| other(format!("Venice queued nothing: {v}")))?;
        let job = format!("{}#{id}", req.model);
        self.refs.lock().unwrap().insert(job.clone(), refs);
        Ok(job)
    }

    async fn poll(&self, job: &str) -> Result<Status> {
        let (model, id) = split(job)?;
        if self.done.lock().unwrap().contains_key(job) {
            return Ok(status(State::Done));
        }
        let r = self
            .post("/video/retrieve", &json!({"model": model, "queue_id": id}))
            .await?;
        if r.status < 300 && r.content_type.starts_with("video/") {
            self.done
                .lock()
                .unwrap()
                .insert(job.into(), Done::Bytes(r.body));
            return Ok(status(State::Done));
        }
        let v = r.json();
        if r.status >= 300 {
            let refs = self.refs.lock().unwrap().get(job).cloned().unwrap_or_default();
            let e = map_error(r.status, &v, &refs);
            if e.kind == ErrorKind::NotFound {
                return Err(e.into());
            }
            return Ok(Status {
                state: State::Failed,
                progress: None,
                error: Some(e),
                cost: None,
            });
        }
        let st = v
            .get("status")
            .or_else(|| v.get("state"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_uppercase();
        Ok(match st.as_str() {
            "COMPLETED" | "SUCCEEDED" | "DONE" => {
                let url = v
                    .get("url")
                    .or_else(|| v.get("download_url"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| other(format!("Venice finished without a video: {v}")))?;
                self.done
                    .lock()
                    .unwrap()
                    .insert(job.into(), Done::Url(url.into()));
                status(State::Done)
            }
            "FAILED" | "ERROR" | "CANCELED" | "CANCELLED" | "EXPIRED" => Status {
                state: State::Failed,
                progress: None,
                error: Some(map_error(200, &v, &[])),
                cost: None,
            },
            "QUEUED" | "PENDING" => status(State::Queued),
            _ => {
                let mut s = status(State::Running);
                s.progress = v
                    .get("progress")
                    .and_then(Value::as_f64)
                    .map(|p| if p > 1.0 { p / 100.0 } else { p });
                s
            }
        })
    }

    async fn fetch(&self, job: &str, output_dir: &Path) -> Result<Outputs> {
        let done = self
            .done
            .lock()
            .unwrap()
            .remove(job)
            .ok_or_else(|| Error::from(PluginError::new(ErrorKind::NotFound, format!("{job} isn't done"))))?;
        let bytes = match done {
            Done::Bytes(b) => b,
            Done::Url(u) => self
                .http
                .get(u)
                .send()
                .await
                .and_then(|r| r.error_for_status())
                .map_err(|e| other(format!("download failed: {e}")))?
                .bytes()
                .await
                .map_err(|e| other(format!("download failed: {e}")))?
                .to_vec(),
        };
        let io = |e: std::io::Error| other(e.to_string());
        std::fs::create_dir_all(output_dir).map_err(io)?;
        std::fs::write(output_dir.join("out.mp4"), bytes).map_err(io)?;
        Ok(Outputs {
            outputs: vec![Output {
                file: "out.mp4".into(),
                kind: MediaKind::Video,
            }],
            data: None,
        })
    }

    async fn cancel(&self, _job: &str) -> Result<()> {
        Err(Error::Unsupported("cancel".into()))
    }
}

fn status(state: State) -> Status {
    Status {
        state,
        progress: None,
        error: None,
        cost: None,
    }
}
