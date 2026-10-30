//! Venice video generation.
//!
//! `POST /video/quote` is free, `POST /video/queue` starts a job, `POST /video/retrieve` answers
//! with JSON while it runs and with the video itself once it's done. Durations are whole seconds
//! written as `"8s"`. Venice won't take a start frame (`image_url`) or end frame together with
//! reference images. Errors look like `{"error": "...", "details": {"field": {"_errors": [...]}}}`.
//! `GET /api_keys/rate_limits` has the account's balances: USD, DIEM and bundled credits.
//!
//! Images are made right away: `POST /image/generate` answers with base64 images, and requests
//! with references go to `POST /image/multi-edit` with the model's `-edit` twin, which answers
//! with the image itself. Prices come from the model lists (`GET /models?type=image` and
//! `?type=inpaint` for the edit twins).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use shotline_plugin::protocol::{
    Amount, Balance, Description, ErrorKind, MediaKind, ModelInfo, Op, Output, Outputs,
    PROTOCOL_VERSION, PluginError, Quote, Request, State, Status,
};
use shotline_plugin::util::data_url;
use shotline_plugin::{Error, Plugin, Result};

pub const KEY_VAR: &str = "VENICE_API_KEY";
const BASE: &str = "https://api.venice.ai/api/v1";

const IMAGE_ASPECTS: [&str; 8] = ["1:1", "3:2", "16:9", "21:9", "9:16", "2:3", "3:4", "4:5"];

/// Image models that have an `-edit` twin, so they can take references. (id, max references,
/// resolutions)
const IMAGES: &[(&str, usize, &[&str])] = &[
    ("nano-banana-2", 6, &["1K", "2K", "4K"]),
    ("nano-banana-pro", 6, &["1K", "2K", "4K"]),
    ("seedream-v5-lite", 6, &[]),
    ("seedream-v4", 6, &[]),
    ("qwen-image-2-1-pro", 10, &["1K", "2K"]),
    ("gpt-image-2", 6, &["1K", "2K", "4K"]),
];

pub fn models() -> Vec<ModelInfo> {
    let mut models = vec![ModelInfo {
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
    }];
    for (id, max, resolutions) in IMAGES {
        models.push(ModelInfo {
            id: id.to_string(),
            op: Op::ImageGenerate,
            ref_image: Some("image {n}".into()),
            max_images: Some(*max),
            resolutions: resolutions.iter().map(|r| r.to_string()).collect(),
            aspects: IMAGE_ASPECTS.map(String::from).to_vec(),
            ..ModelInfo::default()
        });
    }
    models
}

pub struct Venice {
    key: Option<String>,
    base: String,
    http: reqwest::Client,
    /// Finished videos and images, kept from `poll` (or `run`) until `fetch`.
    done: Mutex<HashMap<String, Done>>,
    refs: Mutex<HashMap<String, Vec<usize>>>,
    /// Model lists for image prices: (`type=image`, `type=inpaint`).
    prices: tokio::sync::Mutex<Option<(Value, Value)>>,
    next: Mutex<u64>,
}

enum Done {
    Bytes(Vec<u8>),
    Url(String),
}

struct Resp {
    status: u16,
    content_type: String,
    body: Vec<u8>,
    violation: bool,
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
            prices: tokio::sync::Mutex::new(None),
            next: Mutex::new(0),
        }
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Resp> {
        self.send(self.http.post(format!("{}{path}", self.base)).json(body))
            .await
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> Result<Resp> {
        let key = self.key.as_deref().ok_or_else(|| {
            Error::from(PluginError::new(
                ErrorKind::Auth,
                format!("{KEY_VAR} isn't set"),
            ))
        })?;
        let r = rb
            .bearer_auth(key)
            .send()
            .await
            .map_err(|e| other(format!("can't reach Venice: {e}")))?;
        let status = r.status().as_u16();
        let header = |k: &str| {
            r.headers()
                .get(k)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string()
        };
        let content_type = header("content-type");
        let violation = header("x-venice-is-content-violation") == "true";
        let body = r
            .bytes()
            .await
            .map_err(|e| other(format!("Venice dropped the connection: {e}")))?
            .to_vec();
        Ok(Resp {
            status,
            content_type,
            body,
            violation,
        })
    }

    async fn model_list(&self, kind: &str) -> Result<Value> {
        let r = self
            .send(self.http.get(format!("{}/models?type={kind}", self.base)))
            .await?;
        let v = r.json();
        if r.status >= 300 {
            return Err(map_error(r.status, &v, &[]).into());
        }
        Ok(v)
    }

    async fn image_price(&self, req: &Request) -> Result<f64> {
        let mut cached = self.prices.lock().await;
        if cached.is_none() {
            *cached = Some((
                self.model_list("image").await?,
                self.model_list("inpaint").await?,
            ));
        }
        let (images, edits) = cached.as_ref().unwrap();
        let edit = !req.references.is_empty();
        let (list, id) = if edit {
            (edits, format!("{}-edit", req.model))
        } else {
            (images, req.model.clone())
        };
        image_price(list, &id, req.resolution.as_deref(), req.references.len()).ok_or_else(|| {
            PluginError::new(
                ErrorKind::Validation,
                format!("Venice's model list has no price for {id}"),
            )
            .field("model")
            .into()
        })
    }

    async fn image(&self, req: &Request) -> Result<String> {
        let (path, b, refs) = image_body(req)?;
        let r = self.post(path, &b).await?;
        if r.status >= 300 {
            return Err(map_error(r.status, &r.json(), &refs).into());
        }
        if r.violation {
            return Err(PluginError::new(
                ErrorKind::ContentPolicy,
                "Venice flagged the image as against its terms",
            )
            .into());
        }
        let bytes = if r.content_type.starts_with("image/") {
            r.body
        } else {
            let v = r.json();
            let b64 = v
                .pointer("/images/0")
                .and_then(Value::as_str)
                .ok_or_else(|| other(format!("Venice sent no image: {v}")))?;
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| other(format!("Venice's image isn't base64: {e}")))?
        };
        let n = {
            let mut next = self.next.lock().unwrap();
            *next += 1;
            *next
        };
        let job = format!("{IMAGE_JOB}{}#{n}", req.model);
        self.done
            .lock()
            .unwrap()
            .insert(job.clone(), Done::Bytes(bytes));
        Ok(job)
    }
}

/// Image jobs are done by the time `run` returns; their handles start with this.
const IMAGE_JOB: &str = "image:";

/// USD for one image from a Venice model list entry: per resolution or flat, plus any charge
/// for input images past the ones included.
pub fn image_price(list: &Value, id: &str, resolution: Option<&str>, inputs: usize) -> Option<f64> {
    let m = list
        .get("data")?
        .as_array()?
        .iter()
        .find(|m| m.get("id").and_then(Value::as_str) == Some(id))?;
    let spec = m.get("model_spec")?;
    let pricing = spec.get("pricing")?;
    let usd = |v: &Value| v.get("usd").and_then(Value::as_f64);
    let res = resolution.or_else(|| {
        spec.pointer("/constraints/defaultResolution")
            .and_then(Value::as_str)
    });
    let base = res
        .and_then(|r| pricing.get("resolutions")?.get(r))
        .and_then(usd)
        .or_else(|| pricing.get("generation").and_then(usd))
        .or_else(|| pricing.get("inpaint").and_then(usd))?;
    let extra = match pricing.get("inputImages") {
        Some(ii) => {
            let included = ii.get("included").and_then(Value::as_u64).unwrap_or(1) as usize;
            let each = ii.get("additional").and_then(usd).unwrap_or(0.0);
            each * inputs.saturating_sub(included) as f64
        }
        None => 0.0,
    };
    Some(base + extra)
}

/// Path and body for an image request.
pub fn image_body(req: &Request) -> Result<(&'static str, Value, Vec<usize>)> {
    let mut images = vec![];
    let mut refs = vec![];
    for (i, r) in req.references.iter().enumerate() {
        let f = format!("references[{i}]");
        if r.kind != MediaKind::Image {
            return Err(PluginError::new(
                ErrorKind::Validation,
                "only images go to an image model",
            )
            .field(f)
            .into());
        }
        images.push(data_url(&r.path, &f)?);
        refs.push(i);
    }
    let mut b = Map::new();
    b.insert(
        "prompt".into(),
        json!(req.prompt.clone().unwrap_or_default()),
    );
    if let Some(a) = &req.aspect {
        b.insert("aspect_ratio".into(), json!(a));
    }
    if let Some(r) = &req.resolution {
        b.insert("resolution".into(), json!(r));
    }
    let path = if images.is_empty() {
        b.insert("model".into(), json!(req.model));
        b.insert("format".into(), json!("png"));
        if let Some(s) = req.seed {
            b.insert("seed".into(), json!(s));
        }
        "/image/generate"
    } else {
        b.insert("modelId".into(), json!(format!("{}-edit", req.model)));
        b.insert("images".into(), json!(images));
        b.insert("output_format".into(), json!("png"));
        "/image/multi-edit"
    };
    for (k, v) in &req.params {
        b.insert(k.clone(), v.clone());
    }
    Ok((path, Value::Object(b), refs))
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
        "images" => match idx {
            Some(i) => format!("references[{}]", refs.get(i).copied().unwrap_or(i)),
            None => "references".into(),
        },
        "modelId" => "model".into(),
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
    let kind =
        if text.contains("content policy") || text.contains("content_policy") || status == 422 {
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
    let detail = body
        .get("details")
        .and_then(|d| first_detail(d, &mut vec![]));
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
            format!(
                "the Venice plugin does video.generate and image.generate, not {}",
                req.op
            ),
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
                return Err(
                    PluginError::new(ErrorKind::Validation, "Venice takes one audio clip")
                        .field(f)
                        .into(),
                );
            }
            _ => {
                return Err(PluginError::new(
                    ErrorKind::Validation,
                    "Venice takes images and audio",
                )
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
        if req.op == Op::ImageGenerate {
            return Ok(Quote {
                amount: self.image_price(req).await?,
                unit: "USD".into(),
                units: Some(1.0),
                unit_name: Some("image".into()),
            });
        }
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
        if req.op == Op::ImageGenerate {
            return self.image(req).await;
        }
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
        if job.starts_with(IMAGE_JOB) {
            return Err(PluginError::new(ErrorKind::NotFound, format!("{job} is gone")).into());
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
            let refs = self
                .refs
                .lock()
                .unwrap()
                .get(job)
                .cloned()
                .unwrap_or_default();
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
        let done = self.done.lock().unwrap().remove(job).ok_or_else(|| {
            Error::from(PluginError::new(
                ErrorKind::NotFound,
                format!("{job} isn't done"),
            ))
        })?;
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
        let (file, kind) = if job.starts_with(IMAGE_JOB) {
            ("out.png", MediaKind::Image)
        } else {
            ("out.mp4", MediaKind::Video)
        };
        let io = |e: std::io::Error| other(e.to_string());
        std::fs::create_dir_all(output_dir).map_err(io)?;
        std::fs::write(output_dir.join(file), bytes).map_err(io)?;
        Ok(Outputs {
            outputs: vec![Output {
                file: file.into(),
                kind,
            }],
            data: None,
        })
    }

    async fn cancel(&self, _job: &str) -> Result<()> {
        Err(Error::Unsupported("cancel".into()))
    }

    async fn balance(&self) -> Result<Balance> {
        let r = self
            .send(self.http.get(format!("{}/api_keys/rate_limits", self.base)))
            .await?;
        let v = r.json();
        if r.status >= 300 {
            return Err(map_error(r.status, &v, &[]).into());
        }
        let b = v
            .pointer("/data/balances")
            .and_then(Value::as_object)
            .ok_or_else(|| other(format!("Venice sent no balances: {v}")))?;
        Ok(Balance {
            balances: b
                .iter()
                .filter_map(|(k, n)| {
                    let unit = match k.as_str() {
                        "USD" => "USD".to_string(),
                        k => format!("venice-{}", k.to_lowercase().replace('_', "-")),
                    };
                    Some(Amount {
                        amount: n.as_f64()?,
                        unit,
                    })
                })
                .collect(),
        })
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
