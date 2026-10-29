use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use shotline_protocol::rpc;
use shotline_protocol::{FetchParams, JobRef, RunParams, UsageParams};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::{Error, Plugin, Result};

/// Serves a plugin over stdin/stdout until stdin closes. This is all a Rust plugin's main needs.
pub async fn serve<P: Plugin>(plugin: P) -> std::io::Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<rpc::Request>(&line) {
            Ok(req) => {
                let id = Some(req.id);
                match dispatch(&plugin, &req.method, req.params).await {
                    Ok(result) => ok(id, result),
                    Err(e) => err(id, e),
                }
            }
            Err(e) => rpc::Response {
                jsonrpc: "2.0".into(),
                id: None,
                result: None,
                error: Some(rpc::Error {
                    code: rpc::PARSE_ERROR,
                    message: e.to_string(),
                    data: None,
                }),
            },
        };
        let mut text = serde_json::to_string(&resp).expect("responses serialize");
        text.push('\n');
        out.write_all(text.as_bytes()).await?;
        out.flush().await?;
    }
    Ok(())
}

fn params<T: DeserializeOwned>(v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| {
        Error::Plugin(shotline_protocol::PluginError::new(
            shotline_protocol::ErrorKind::Validation,
            format!("bad params: {e}"),
        ))
    })
}

fn to_value<T: Serialize>(v: T) -> Value {
    serde_json::to_value(v).expect("results serialize")
}

async fn dispatch<P: Plugin>(p: &P, method: &str, v: Value) -> Result<Value> {
    Ok(match method {
        "describe" => to_value(p.describe().await?),
        "quote" => to_value(p.quote(&params(v)?).await?),
        "run" => {
            let RunParams {
                request,
                output_dir,
            } = params(v)?;
            json!({"job": p.run(&request, &output_dir).await?})
        }
        "poll" => to_value(p.poll(&params::<JobRef>(v)?.job).await?),
        "fetch" => {
            let FetchParams { job, output_dir } = params(v)?;
            to_value(p.fetch(&job, &output_dir).await?)
        }
        "cancel" => {
            p.cancel(&params::<JobRef>(v)?.job).await?;
            json!({})
        }
        "rates" => to_value(p.rates().await?),
        "usage" => to_value(p.usage(&params::<UsageParams>(v)?.since).await?),
        other => return Err(Error::Unsupported(other.to_string())),
    })
}

fn ok(id: Option<u64>, result: Value) -> rpc::Response {
    rpc::Response {
        jsonrpc: "2.0".into(),
        id,
        result: Some(result),
        error: None,
    }
}

fn err(id: Option<u64>, e: Error) -> rpc::Response {
    let error = match e {
        Error::Plugin(pe) => rpc::Error {
            code: rpc::PLUGIN_ERROR,
            message: pe.message.clone(),
            data: Some(to_value(pe)),
        },
        Error::Unsupported(m) => rpc::Error {
            code: rpc::METHOD_NOT_FOUND,
            message: format!("unsupported method {m}"),
            data: None,
        },
        Error::Broken { message, .. } => rpc::Error {
            code: -32603,
            message,
            data: None,
        },
    };
    rpc::Response {
        jsonrpc: "2.0".into(),
        id,
        result: None,
        error: Some(error),
    }
}
