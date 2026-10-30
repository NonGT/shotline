//! fal.ai through its queue API.
//!
//! Submit `POST {queue}/{model}` with the input fields at the top level of the body (wrapping them
//! in `{"input": ...}` like the JS SDK makes fal fail with 422 after queueing). Status and result
//! live under `{queue}/{owner}/{app}/requests/{id}`. Prices come from the pricing estimate
//! endpoint, which quotes the 480P rate; other resolutions are multiples of it.
//!
//! Image models (nano-banana) have a separate `<model>/edit` endpoint that takes `image_urls`;
//! requests with references go there.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use async_trait::async_trait;
use base64::Engine;
use serde_json::{Map, Value, json};
use shotline_plugin::protocol::{
    Description, ErrorKind, MediaKind, ModelInfo, Op, Output, Outputs, PROTOCOL_VERSION,
    PluginError, Quote, Request, State, Status,
};
use shotline_plugin::{Error, Plugin, Result};

pub const KEY_VAR: &str = "FAL_API_KEY";
const QUEUE: &str = "https://queue.fal.run";
const API: &str = "https://api.fal.ai/v1";

/// Price multiple over the 480P estimate, per model and resolution.
const RESOLUTION_PRICE: &[(&str, &[(&str, f64)])] = &[(
    "minimax/h3-max/reference-to-video",
    &[("480P", 1.0), ("768P", 1.6), ("1080P", 3.2)],
)];

const IMAGE_ASPECTS: [&str; 10] = [
    "21:9", "16:9", "3:2", "4:3", "5:4", "1:1", "4:5", "3:4", "2:3", "9:16",
];

pub fn models() -> Vec<ModelInfo> {
    let image = |id: &str, resolutions: &[&str]| ModelInfo {
        id: id.into(),
        op: Op::ImageGenerate,
        ref_image: Some("image {n}".into()),
        resolutions: resolutions.iter().map(|r| r.to_string()).collect(),
        aspects: IMAGE_ASPECTS.map(String::from).to_vec(),
        ..ModelInfo::default()
    };
    let mut models = vec![ModelInfo {
        id: "minimax/h3-max/reference-to-video".into(),
        op: Op::VideoGenerate,
        ref_image: Some("Image {n}".into()),
        max_images: Some(9),
        max_audio: Some(3),
        min_duration: Some(0.92),
        max_duration: Some(15.0),
        resolutions: vec!["480P".into(), "768P".into(), "1080P".into()],
        aspects: ["adaptive", "21:9", "16:9", "4:3", "1:1", "3:4", "9:16"]
            .map(String::from)
            .to_vec(),
        start_frame: true,
        start_frame_with_refs: true,
        audio_input: true,
        ..ModelInfo::default()
    }];
    // fal prices pro per image without saying whether 2K or 4K cost more, so only 1K for now.
    models.push(image("fal-ai/nano-banana", &[]));
    models.push(image("fal-ai/nano-banana-pro", &["1K"]));
    models
}

fn is_image(model: &str) -> bool {
    models()
        .iter()
        .any(|m| m.id == model && m.op == Op::ImageGenerate)
}

/// Where a request goes: image requests with references go to the model's edit endpoint.
pub fn endpoint(req: &Request) -> String {
    if req.op == Op::ImageGenerate && !req.references.is_empty() {
        format!("{}/edit", req.model)
    } else {
        req.model.clone()
    }
}

pub struct Fal {
    key: Option<String>,
    queue: String,
    api: String,
    http: reqwest::Client,
    /// Which request list index came from which request field, per job, for error messages.
    jobs: Mutex<HashMap<String, Map<String, Value>>>,
}

impl Fal {
    /// Key from FAL_API_KEY; base URLs from SHOTLINE_FAL_QUEUE_URL / SHOTLINE_FAL_API_URL when
    /// set, for tests.
    pub fn from_env(key: Option<String>) -> Fal {
        let queue = std::env::var("SHOTLINE_FAL_QUEUE_URL").unwrap_or_else(|_| QUEUE.into());
        let api = std::env::var("SHOTLINE_FAL_API_URL").unwrap_or_else(|_| API.into());
        Fal::new(key, &queue, &api)
    }

    pub fn new(key: Option<String>, queue: &str, api: &str) -> Fal {
        Fal {
            key,
            queue: queue.trim_end_matches('/').into(),
            api: api.trim_end_matches('/').into(),
            http: reqwest::Client::new(),
            jobs: Mutex::new(HashMap::new()),
        }
    }

    fn key(&self) -> Result<&str> {
        self.key
            .as_deref()
            .ok_or_else(|| PluginError::new(ErrorKind::Auth, format!("{KEY_VAR} isn't set")).into())
    }

    async fn send(&self, req: reqwest::RequestBuilder, refs: &RefMap) -> Result<Value> {
        let auth = format!("Key {}", self.key()?);
        let resp = req
            .header("Authorization", auth)
            .send()
            .await
            .map_err(|e| PluginError::new(ErrorKind::Other, format!("can't reach fal: {e}")))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let body: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        if (200..300).contains(&status) {
            return Ok(body);
        }
        Err(map_error(status, &body, refs).into())
    }
}

/// Positions in fal's lists that map back to request fields.
#[derive(Debug, Default, Clone)]
pub struct RefMap {
    pub images: Vec<usize>,
    pub audio: Vec<usize>,
    pub video: Vec<usize>,
}

fn data_url(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .map_err(|e| PluginError::new(ErrorKind::Validation, format!("{}: {e}", path.display())))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{mime};base64,{b64}"))
}

/// The JSON fal gets for a request, and where each list item came from.
pub fn input(req: &Request) -> Result<(Map<String, Value>, RefMap)> {
    if req.op == Op::ImageGenerate {
        return image_input(req);
    }
    if req.op != Op::VideoGenerate {
        return Err(PluginError::new(
            ErrorKind::Unsupported,
            format!(
                "the built-in fal plugin does video.generate and image.generate, not {}",
                req.op
            ),
        )
        .field("op")
        .into());
    }
    let mut m = Map::new();
    let mut refs = RefMap::default();
    if let Some(p) = &req.prompt {
        m.insert("prompt".into(), json!(p));
    }
    if let Some(d) = req.duration {
        m.insert("duration".into(), json!(d));
    }
    if let Some(r) = &req.resolution {
        m.insert("resolution".into(), json!(r));
    }
    if let Some(a) = &req.aspect {
        m.insert("aspect_ratio".into(), json!(a));
    }
    if let Some(s) = req.seed {
        m.insert("seed".into(), json!(s));
    }
    let (mut images, mut audio, mut video) = (vec![], vec![], vec![]);
    for (i, r) in req.references.iter().enumerate() {
        let url = data_url(&r.path)?;
        match r.kind {
            MediaKind::Image => {
                images.push(url);
                refs.images.push(i);
            }
            MediaKind::Audio => {
                audio.push(url);
                refs.audio.push(i);
            }
            MediaKind::Video => {
                video.push(url);
                refs.video.push(i);
            }
            MediaKind::Data => {}
        }
    }
    for (key, list) in [
        ("reference_image_urls", images),
        ("reference_audio_urls", audio),
        ("reference_video_urls", video),
    ] {
        if !list.is_empty() {
            m.insert(key.into(), json!(list));
        }
    }
    if let Some(p) = &req.start_frame {
        m.insert("image_url".into(), json!(data_url(p)?));
    }
    if let Some(p) = &req.end_frame {
        m.insert("end_image_url".into(), json!(data_url(p)?));
    }
    // h3 requires it; disabled keeps our prompt as written unless the shot says otherwise.
    m.insert("prompt_expansion_mode".into(), json!("disabled"));
    for (k, v) in &req.params {
        m.insert(k.clone(), v.clone());
    }
    Ok((m, refs))
}

fn image_input(req: &Request) -> Result<(Map<String, Value>, RefMap)> {
    let mut m = Map::new();
    let mut refs = RefMap::default();
    if let Some(p) = &req.prompt {
        m.insert("prompt".into(), json!(p));
    }
    if let Some(a) = &req.aspect {
        m.insert("aspect_ratio".into(), json!(a));
    }
    if let Some(r) = &req.resolution {
        m.insert("resolution".into(), json!(r));
    }
    if let Some(s) = req.seed {
        m.insert("seed".into(), json!(s));
    }
    let mut images = vec![];
    for (i, r) in req.references.iter().enumerate() {
        if r.kind != MediaKind::Image {
            return Err(PluginError::new(
                ErrorKind::Validation,
                "only images go to an image model",
            )
            .field(format!("references[{i}]"))
            .into());
        }
        images.push(data_url(&r.path)?);
        refs.images.push(i);
    }
    if !images.is_empty() {
        m.insert("image_urls".into(), json!(images));
    }
    m.insert("num_images".into(), json!(1));
    m.insert("output_format".into(), json!("png"));
    for (k, v) in &req.params {
        m.insert(k.clone(), v.clone());
    }
    Ok((m, refs))
}

/// fal's field names back to request fields.
fn field(loc: &[Value], refs: &RefMap) -> Option<String> {
    let loc: Vec<&Value> = loc.iter().filter(|v| v.as_str() != Some("body")).collect();
    let name = loc.first()?.as_str()?;
    let idx = loc.get(1).and_then(|v| v.as_u64()).map(|i| i as usize);
    let list = |l: &[usize]| match idx {
        Some(i) => Some(match l.get(i) {
            Some(r) => format!("references[{r}]"),
            None => format!("references[{i}]"),
        }),
        None => Some("references".into()),
    };
    match name {
        "prompt" | "duration" | "resolution" | "seed" => Some(name.into()),
        "aspect_ratio" => Some("aspect".into()),
        "image_url" => Some("start_frame".into()),
        "end_image_url" => Some("end_frame".into()),
        "reference_image_urls" | "image_urls" => list(&refs.images),
        "reference_audio_urls" => list(&refs.audio),
        "reference_video_urls" => list(&refs.video),
        other => Some(format!("params.{other}")),
    }
}

fn kind_for(status: u16, text: &str) -> ErrorKind {
    if text.contains("content_policy") {
        return ErrorKind::ContentPolicy;
    }
    match status {
        401 => ErrorKind::Auth,
        403 if text.contains("balance") || text.contains("locked") => ErrorKind::Quota,
        403 => ErrorKind::Auth,
        402 | 429 => ErrorKind::Quota,
        404 => ErrorKind::NotFound,
        400 | 422 => ErrorKind::Validation,
        _ => ErrorKind::Other,
    }
}

/// FastAPI-style `{"detail": [{"loc", "msg", "type"}]}` or `{"detail": "..."}` into one error.
pub fn map_error(status: u16, body: &Value, refs: &RefMap) -> PluginError {
    let text = body.to_string();
    let mut kind = kind_for(status, &text);
    if let Some(items) = body.get("detail").and_then(Value::as_array) {
        let msgs: Vec<String> = items
            .iter()
            .map(|d| {
                d.get("msg")
                    .and_then(Value::as_str)
                    .unwrap_or("invalid")
                    .to_string()
            })
            .collect();
        let first = items.first();
        let f = first
            .and_then(|d| d.get("loc"))
            .and_then(Value::as_array)
            .and_then(|loc| field(loc, refs));
        if first
            .and_then(|d| d.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|t| t.contains("content_policy"))
        {
            kind = ErrorKind::ContentPolicy;
        }
        let mut e = PluginError::new(kind, msgs.join("; "));
        e.field = f;
        return e;
    }
    let msg = ["detail", "error", "message"]
        .iter()
        .find_map(|k| body.get(*k).and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| match body {
            Value::String(s) if !s.is_empty() => s.chars().take(500).collect(),
            _ => format!("fal answered HTTP {status}"),
        });
    PluginError::new(kind, msg)
}

/// owner/app part of a model id, which is what request URLs use.
fn app(model: &str) -> String {
    model.split('/').take(2).collect::<Vec<_>>().join("/")
}

/// Job handles are `model#request_id` so a job can be picked up again after a restart.
fn split_job(job: &str) -> Result<(&str, &str)> {
    job.split_once('#')
        .ok_or_else(|| PluginError::new(ErrorKind::NotFound, format!("no job {job}")).into())
}

fn price_multiple(model: &str, resolution: Option<&str>) -> f64 {
    RESOLUTION_PRICE
        .iter()
        .find(|(m, _)| *m == model)
        .and_then(|(_, table)| {
            let r = resolution?;
            table.iter().find(|(x, _)| *x == r).map(|(_, n)| *n)
        })
        .unwrap_or(1.0)
}

#[async_trait]
impl Plugin for Fal {
    async fn describe(&self) -> Result<Description> {
        Ok(Description {
            name: "fal".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: PROTOCOL_VERSION,
            env: vec![KEY_VAR.into()],
            models: models(),
        })
    }

    async fn quote(&self, req: &Request) -> Result<Quote> {
        let image = is_image(&req.model);
        let units = if image {
            1.0
        } else {
            req.duration.unwrap_or(5.0)
        };
        let body = json!({
            "estimate_type": "unit_price",
            "endpoints": {endpoint(req): {"unit_quantity": units}},
        });
        let r = self
            .send(
                self.http
                    .post(format!("{}/models/pricing/estimate", self.api))
                    .json(&body),
                &RefMap::default(),
            )
            .await
            .map_err(|e| match e {
                // An unknown endpoint id is the caller's model being wrong.
                Error::Plugin(mut pe)
                    if matches!(pe.kind, ErrorKind::NotFound | ErrorKind::Validation) =>
                {
                    pe.kind = ErrorKind::Validation;
                    pe.field = Some("model".into());
                    Error::Plugin(pe)
                }
                e => e,
            })?;
        let Some(base) = r.get("total_cost").and_then(Value::as_f64) else {
            return Err(PluginError::new(
                ErrorKind::Other,
                format!("fal's estimate has no total_cost: {r}"),
            )
            .into());
        };
        let currency = r
            .get("currency")
            .and_then(Value::as_str)
            .unwrap_or("USD")
            .to_string();
        Ok(Quote {
            amount: base * price_multiple(&req.model, req.resolution.as_deref()),
            unit: currency,
            units: Some(units),
            unit_name: Some(if image { "image" } else { "s" }.into()),
        })
    }

    async fn run(&self, req: &Request, _output_dir: &Path) -> Result<String> {
        let (body, refs) = input(req)?;
        let r = self
            .send(
                self.http
                    .post(format!("{}/{}", self.queue, endpoint(req)))
                    .json(&Value::Object(body)),
                &refs,
            )
            .await?;
        let Some(id) = r.get("request_id").and_then(Value::as_str) else {
            return Err(
                PluginError::new(ErrorKind::Other, format!("fal queued nothing: {r}")).into(),
            );
        };
        let job = format!("{}#{id}", endpoint(req));
        let mut saved = Map::new();
        saved.insert("images".into(), json!(refs.images));
        saved.insert("audio".into(), json!(refs.audio));
        saved.insert("video".into(), json!(refs.video));
        self.jobs.lock().unwrap().insert(job.clone(), saved);
        Ok(job)
    }

    async fn poll(&self, job: &str) -> Result<Status> {
        let (model, id) = split_job(job)?;
        let url = format!("{}/{}/requests/{id}/status", self.queue, app(model));
        let s = self.send(self.http.get(url), &self.refs(job)).await?;
        let status = s.get("status").and_then(Value::as_str).unwrap_or("");
        let failed = |s: &Value| {
            let et = s.get("error_type").and_then(Value::as_str).unwrap_or("");
            let msg = s
                .get("error")
                .map(|e| e.as_str().map(str::to_string).unwrap_or(e.to_string()))
                .unwrap_or_else(|| "fal reported a failure".into());
            let kind = if format!("{et}{msg}").contains("content_policy") {
                ErrorKind::ContentPolicy
            } else {
                ErrorKind::Other
            };
            Status {
                state: State::Failed,
                progress: None,
                error: Some(PluginError::new(kind, msg)),
                cost: None,
            }
        };
        Ok(match status {
            "IN_QUEUE" => Status {
                state: State::Queued,
                progress: None,
                error: None,
                cost: None,
            },
            "IN_PROGRESS" => Status {
                state: State::Running,
                progress: None,
                error: None,
                cost: None,
            },
            "COMPLETED" if s.get("error").is_some_and(|e| !e.is_null()) => failed(&s),
            "COMPLETED" => Status {
                state: State::Done,
                progress: Some(1.0),
                error: None,
                // fal doesn't say what it charged; shotline books the quote.
                cost: None,
            },
            _ => failed(&s),
        })
    }

    async fn fetch(&self, job: &str, output_dir: &Path) -> Result<Outputs> {
        let (model, id) = split_job(job)?;
        let url = format!("{}/{}/requests/{id}", self.queue, app(model));
        let body = self.send(self.http.get(url), &self.refs(job)).await?;
        let mut outputs = vec![];
        for (key, kind, file) in [
            ("video", MediaKind::Video, "out.mp4"),
            ("image", MediaKind::Image, "out.png"),
            ("images", MediaKind::Image, "out.png"),
            ("audio", MediaKind::Audio, "out.mp3"),
        ] {
            // Image models answer with a list; one image is asked for.
            let v = body.get(key).map(|v| v.get(0).unwrap_or(v));
            let Some(src) = v.and_then(|v| v.get("url")).and_then(Value::as_str) else {
                continue;
            };
            let bytes = self
                .http
                .get(src)
                .send()
                .await
                .and_then(|r| r.error_for_status())
                .map_err(|e| PluginError::new(ErrorKind::Other, format!("download failed: {e}")))?
                .bytes()
                .await
                .map_err(|e| PluginError::new(ErrorKind::Other, format!("download failed: {e}")))?;
            let io = |e: std::io::Error| PluginError::new(ErrorKind::Other, e.to_string());
            std::fs::create_dir_all(output_dir).map_err(io)?;
            std::fs::write(output_dir.join(file), &bytes).map_err(io)?;
            outputs.push(Output {
                file: file.into(),
                kind,
            });
        }
        if outputs.is_empty() {
            return Err(PluginError::new(
                ErrorKind::Other,
                format!("fal's result has no file in it: {body}"),
            )
            .into());
        }
        let mut data = body.clone();
        for k in ["video", "image", "images", "audio"] {
            data.as_object_mut().map(|m| m.remove(k));
        }
        Ok(Outputs {
            outputs,
            data: Some(data).filter(|d| d.as_object().is_some_and(|m| !m.is_empty())),
        })
    }

    async fn cancel(&self, job: &str) -> Result<()> {
        let (model, id) = split_job(job)?;
        let url = format!("{}/{}/requests/{id}/cancel", self.queue, app(model));
        self.send(self.http.put(url), &RefMap::default()).await?;
        Ok(())
    }
}

impl Fal {
    fn refs(&self, job: &str) -> RefMap {
        let jobs = self.jobs.lock().unwrap();
        let Some(m) = jobs.get(job) else {
            return RefMap::default();
        };
        let list = |k: &str| -> Vec<usize> {
            m.get(k)
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default()
        };
        RefMap {
            images: list("images"),
            audio: list("audio"),
            video: list("video"),
        }
    }
}
