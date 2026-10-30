//! WaveSpeed, for lip sync.
//!
//! `POST /api/v3/{model}` with the video and audio as data URLs starts a prediction;
//! `GET /api/v3/predictions/{id}/result` reports its status and, once completed, the output URLs.
//! `GET /api/v3/models` lists every model with a `base_price`, which for the lip sync models is
//! USD per second of video (veed/lipsync says "$0.15 per 5s" next to 0.03), so quotes are that
//! times the duration. `GET /api/v3/balance` is what's left on the account, in USD.

use std::collections::HashMap;
use std::path::Path;

use async_trait::async_trait;
use serde_json::{Map, Value, json};
use shotline_plugin::protocol::{
    Amount, Balance, Description, ErrorKind, MediaKind, ModelInfo, Op, Output, Outputs,
    PROTOCOL_VERSION, PluginError, Quote, Request, State, Status,
};
use shotline_plugin::util::data_url;
use shotline_plugin::{Error, Plugin, Result};

pub const KEY_VAR: &str = "WAVESPEED_API_KEY";
const BASE: &str = "https://api.wavespeed.ai/api/v3";

pub fn models() -> Vec<ModelInfo> {
    vec![ModelInfo {
        id: "sync/lipsync-3".into(),
        op: Op::VideoLipsync,
        ..ModelInfo::default()
    }]
}

pub struct WaveSpeed {
    key: Option<String>,
    base: String,
    http: reqwest::Client,
    /// model -> base_price, fetched once.
    prices: tokio::sync::Mutex<Option<HashMap<String, f64>>>,
}

pub fn from_env() -> WaveSpeed {
    WaveSpeed::new(std::env::var(KEY_VAR).ok(), BASE)
}

fn bad(kind: ErrorKind, msg: impl Into<String>, field: &str) -> Error {
    PluginError::new(kind, msg).field(field).into()
}

fn other(msg: impl Into<String>) -> Error {
    PluginError::new(ErrorKind::Other, msg).into()
}

fn check(req: &Request) -> Result<f64> {
    if req.op != Op::VideoLipsync {
        return Err(bad(
            ErrorKind::Unsupported,
            format!("only video.lipsync, not {}", req.op),
            "op",
        ));
    }
    if !models().iter().any(|m| m.id == req.model) {
        return Err(bad(
            ErrorKind::Validation,
            format!("no model {}", req.model),
            "model",
        ));
    }
    for name in ["video", "audio"] {
        if !req.inputs.contains_key(name) {
            return Err(bad(
                ErrorKind::Validation,
                format!("needs {name}"),
                &format!("inputs.{name}"),
            ));
        }
    }
    // The price is per second of video; the conformance kit and quotes before the video exists
    // pass the planned duration.
    req.duration.ok_or_else(|| {
        bad(
            ErrorKind::Validation,
            "needs the duration to quote",
            "duration",
        )
    })
}

pub fn map_error(status: u16, body: &Value) -> PluginError {
    let msg = ["message", "error", "detail"]
        .iter()
        .find_map(|k| body.get(*k).and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| format!("WaveSpeed answered HTTP {status}"));
    let lower = msg.to_lowercase();
    let kind = if lower.contains("balance") || lower.contains("insufficient") {
        ErrorKind::Quota
    } else if lower.contains("sensitive") || lower.contains("policy") || lower.contains("nsfw") {
        ErrorKind::ContentPolicy
    } else {
        match status {
            401 | 403 => ErrorKind::Auth,
            402 | 429 => ErrorKind::Quota,
            404 => ErrorKind::NotFound,
            400 | 422 => ErrorKind::Validation,
            _ => ErrorKind::Other,
        }
    };
    PluginError::new(kind, msg)
}

impl WaveSpeed {
    pub fn new(key: Option<String>, base: &str) -> WaveSpeed {
        WaveSpeed {
            key,
            base: base.trim_end_matches('/').into(),
            http: reqwest::Client::new(),
            prices: tokio::sync::Mutex::new(None),
        }
    }

    async fn price_per_second(&self, model: &str) -> Result<f64> {
        let mut prices = self.prices.lock().await;
        if prices.is_none() {
            let v = self
                .send(self.http.get(format!("{}/models", self.base)))
                .await?;
            let list = v
                .as_array()
                .ok_or_else(|| other(format!("WaveSpeed sent no model list: {v}")))?;
            *prices = Some(
                list.iter()
                    .filter_map(|m| {
                        Some((
                            m.get("model_id")?.as_str()?.to_string(),
                            m.get("base_price")?.as_f64()?,
                        ))
                    })
                    .collect(),
            );
        }
        prices
            .as_ref()
            .and_then(|p| p.get(model).copied())
            .ok_or_else(|| other(format!("WaveSpeed lists no price for {model}")))
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> Result<Value> {
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
            .map_err(|e| other(format!("can't reach WaveSpeed: {e}")))?;
        let status = r.status().as_u16();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        if status >= 300 {
            return Err(map_error(status, &v).into());
        }
        // Answers are wrapped as {"code": 200, "data": {...}}.
        Ok(v.get("data").cloned().unwrap_or(v))
    }
}

#[async_trait]
impl Plugin for WaveSpeed {
    async fn describe(&self) -> Result<Description> {
        Ok(Description {
            name: "wavespeed".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: PROTOCOL_VERSION,
            env: vec![KEY_VAR.into()],
            models: models(),
        })
    }

    async fn quote(&self, req: &Request) -> Result<Quote> {
        let secs = check(req)?;
        let per = self.price_per_second(&req.model).await?;
        Ok(Quote {
            amount: per * secs,
            unit: "USD".into(),
            units: Some(secs),
            unit_name: Some("s".into()),
        })
    }

    async fn run(&self, req: &Request, _output_dir: &Path) -> Result<String> {
        check(req)?;
        let mut body = Map::new();
        body.insert(
            "video".into(),
            json!(data_url(&req.inputs["video"], "inputs.video")?),
        );
        body.insert(
            "audio".into(),
            json!(data_url(&req.inputs["audio"], "inputs.audio")?),
        );
        for (k, v) in &req.params {
            body.insert(k.clone(), v.clone());
        }
        let v = self
            .send(
                self.http
                    .post(format!("{}/{}", self.base, req.model))
                    .json(&body),
            )
            .await?;
        v.get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| other(format!("WaveSpeed started nothing: {v}")))
    }

    async fn poll(&self, job: &str) -> Result<Status> {
        let v = self
            .send(
                self.http
                    .get(format!("{}/predictions/{job}/result", self.base)),
            )
            .await?;
        let st = v
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let status = |state| Status {
            state,
            progress: None,
            error: None,
            cost: None,
        };
        Ok(match st.as_str() {
            "completed" | "succeeded" => status(State::Done),
            "failed" | "error" | "canceled" | "cancelled" => Status {
                state: State::Failed,
                progress: None,
                error: Some(map_error(
                    200,
                    &json!({"message": v.get("error").and_then(Value::as_str).unwrap_or("the prediction failed")}),
                )),
                cost: None,
            },
            "created" | "queued" => status(State::Queued),
            _ => status(State::Running),
        })
    }

    async fn fetch(&self, job: &str, output_dir: &Path) -> Result<Outputs> {
        let v = self
            .send(
                self.http
                    .get(format!("{}/predictions/{job}/result", self.base)),
            )
            .await?;
        let first = v
            .get("outputs")
            .and_then(Value::as_array)
            .and_then(|a| a.first());
        let url = first
            .and_then(|o| o.as_str().or_else(|| o.get("url").and_then(Value::as_str)))
            .ok_or_else(|| other(format!("WaveSpeed's result has no output: {v}")))?;
        let bytes = self
            .http
            .get(url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| other(format!("download failed: {e}")))?
            .bytes()
            .await
            .map_err(|e| other(format!("download failed: {e}")))?;
        let io = |e: std::io::Error| other(e.to_string());
        std::fs::create_dir_all(output_dir).map_err(io)?;
        std::fs::write(output_dir.join("out.mp4"), &bytes).map_err(io)?;
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

    async fn balance(&self) -> Result<Balance> {
        let v = self
            .send(self.http.get(format!("{}/balance", self.base)))
            .await?;
        let amount = v
            .get("balance")
            .and_then(Value::as_f64)
            .ok_or_else(|| other(format!("WaveSpeed sent no balance: {v}")))?;
        Ok(Balance {
            balances: vec![Amount {
                amount,
                unit: "USD".into(),
            }],
        })
    }
}
