use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use shotline_protocol::rpc;
use shotline_protocol::{
    Description, FetchParams, JobRef, Outputs, PluginError, Quote, Rates, Request, RunParams,
    Status, Usage, UsageParams,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::{Error, Plugin, Result};

/// Environment every plugin gets regardless of what it declares, so it can find its interpreter
/// and a temp dir.
const BASE_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "SYSTEMROOT",
    "TEMP",
    "TMP",
    "TMPDIR",
    "LANG",
    "LC_ALL",
];

const STDERR_LINES: usize = 20;

/// Drop it before the tokio runtime it was used on. On Windows, runtime shutdown waits for the
/// thread reading the plugin's stderr, which only finishes once the plugin has exited.
pub struct ProcessPlugin {
    name: String,
    exec: PathBuf,
    args: Vec<String>,
    env: Vec<(String, String)>,
    conn: Mutex<Option<Conn>>,
    /// Scales every timeout, for tests.
    pub timeout_scale: f64,
}

struct Conn {
    _child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    stderr: Arc<std::sync::Mutex<VecDeque<String>>>,
    next_id: u64,
}

fn timeout_for(method: &str) -> Duration {
    Duration::from_secs(match method {
        "run" => 300,
        "fetch" => 600,
        _ => 60,
    })
}

impl ProcessPlugin {
    /// `env` is what the plugin gets on top of the base variables, normally the values of the
    /// variables it declared in `describe`.
    pub fn new(
        name: impl Into<String>,
        exec: impl Into<PathBuf>,
        args: Vec<String>,
        env: Vec<(String, String)>,
    ) -> Self {
        Self {
            name: name.into(),
            exec: exec.into(),
            args,
            env,
            conn: Mutex::new(None),
            timeout_scale: 1.0,
        }
    }

    fn broken(&self, message: impl Into<String>) -> Error {
        Error::Broken {
            plugin: self.name.clone(),
            message: message.into(),
        }
    }

    fn spawn(&self) -> Result<Conn> {
        let mut cmd = Command::new(&self.exec);
        cmd.args(&self.args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for key in BASE_ENV {
            if let Ok(v) = std::env::var(key) {
                cmd.env(key, v);
            }
        }
        cmd.envs(self.env.iter().map(|(k, v)| (k, v)));
        let mut child = cmd
            .spawn()
            .map_err(|e| self.broken(format!("can't start {}: {e}", self.exec.display())))?;

        let stderr = Arc::new(std::sync::Mutex::new(VecDeque::new()));
        let tail = stderr.clone();
        let mut err_lines = BufReader::new(child.stderr.take().unwrap()).lines();
        tokio::spawn(async move {
            while let Ok(Some(line)) = err_lines.next_line().await {
                let mut t = tail.lock().unwrap();
                if t.len() == STDERR_LINES {
                    t.pop_front();
                }
                t.push_back(line);
            }
        });

        Ok(Conn {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()).lines(),
            _child: child,
            stderr,
            next_id: 1,
        })
    }

    async fn call<P: Serialize, R: DeserializeOwned>(&self, method: &str, params: P) -> Result<R> {
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            *guard = Some(self.spawn()?);
        }
        let conn = guard.as_mut().unwrap();
        let id = conn.next_id;
        conn.next_id += 1;

        let limit = timeout_for(method).mul_f64(self.timeout_scale);
        let res = tokio::time::timeout(limit, exchange(conn, id, method, params)).await;
        let res = match res {
            Ok(r) => r,
            Err(_) => Err(format!("no reply to {method} after {limit:?}")),
        };
        match res {
            Ok(resp) => {
                drop(guard);
                self.decode(method, resp)
            }
            Err(msg) => {
                // Give the stderr reader a moment to catch the last lines of a crash.
                tokio::time::sleep(Duration::from_millis(50)).await;
                let tail = conn
                    .stderr
                    .lock()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>();
                *guard = None;
                let msg = if tail.is_empty() {
                    msg
                } else {
                    format!("{msg}\n{}", tail.join("\n"))
                };
                Err(self.broken(msg))
            }
        }
    }

    fn decode<R: DeserializeOwned>(&self, method: &str, resp: rpc::Response) -> Result<R> {
        if let Some(e) = resp.error {
            return Err(match e.code {
                rpc::PLUGIN_ERROR => match e.data.map(serde_json::from_value::<PluginError>) {
                    Some(Ok(pe)) => Error::Plugin(pe),
                    _ => self.broken(format!(
                        "{method} failed without an error body: {}",
                        e.message
                    )),
                },
                rpc::METHOD_NOT_FOUND => Error::Unsupported(method.to_string()),
                _ => self.broken(format!("{method}: {}", e.message)),
            });
        }
        let result = resp.result.unwrap_or(Value::Null);
        serde_json::from_value(result).map_err(|e| self.broken(format!("bad {method} reply: {e}")))
    }
}

/// Sends one request and reads lines until its response. Errors are messages for `Broken`.
async fn exchange<P: Serialize>(
    conn: &mut Conn,
    id: u64,
    method: &str,
    params: P,
) -> std::result::Result<rpc::Response, String> {
    let req = rpc::Request {
        jsonrpc: "2.0".into(),
        id,
        method: method.into(),
        params: serde_json::to_value(params).expect("params serialize"),
    };
    let mut line = serde_json::to_string(&req).expect("requests serialize");
    line.push('\n');
    let io = |e: std::io::Error| format!("lost the plugin process: {e}");
    conn.stdin.write_all(line.as_bytes()).await.map_err(io)?;
    conn.stdin.flush().await.map_err(io)?;
    loop {
        let Some(line) = conn.stdout.next_line().await.map_err(io)? else {
            return Err("plugin exited".into());
        };
        if line.trim().is_empty() {
            continue;
        }
        let resp: rpc::Response = serde_json::from_str(&line)
            .map_err(|e| format!("wrote something that isn't a response ({e}): {line}"))?;
        // Stale replies from an earlier timed out call are skipped.
        if resp.id == Some(id) || resp.id.is_none() {
            return Ok(resp);
        }
    }
}

#[async_trait]
impl Plugin for ProcessPlugin {
    async fn describe(&self) -> Result<Description> {
        self.call("describe", json!({})).await
    }

    async fn quote(&self, req: &Request) -> Result<Quote> {
        self.call("quote", req).await
    }

    async fn run(&self, req: &Request, output_dir: &Path) -> Result<String> {
        let params = RunParams {
            request: req.clone(),
            output_dir: output_dir.to_path_buf(),
        };
        let r: JobRef = self.call("run", params).await?;
        Ok(r.job)
    }

    async fn poll(&self, job: &str) -> Result<Status> {
        self.call("poll", JobRef { job: job.into() }).await
    }

    async fn fetch(&self, job: &str, output_dir: &Path) -> Result<Outputs> {
        let params = FetchParams {
            job: job.into(),
            output_dir: output_dir.to_path_buf(),
        };
        self.call("fetch", params).await
    }

    async fn cancel(&self, job: &str) -> Result<()> {
        let _: Value = self.call("cancel", JobRef { job: job.into() }).await?;
        Ok(())
    }

    async fn rates(&self) -> Result<Rates> {
        self.call("rates", json!({})).await
    }

    async fn usage(&self, since: &str) -> Result<Usage> {
        self.call(
            "usage",
            UsageParams {
                since: since.into(),
            },
        )
        .await
    }
}
