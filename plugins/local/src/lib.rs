//! Free tools on this machine: frame interpolation and upscaling with ffmpeg, and RIFE
//! interpolation when `rife-ncnn-vulkan` is on PATH. Everything quotes 0 USD.
//!
//! Jobs run on a blocking thread and write straight into the output folder `run` is given.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use shotline_plugin::protocol::{
    Description, ErrorKind, MediaKind, ModelInfo, Op, Output, Outputs, PROTOCOL_VERSION,
    PluginError, Quote, Request, State, Status,
};
use shotline_plugin::{Error, Plugin, Result};

pub const MINTERPOLATE: &str = "ffmpeg/minterpolate";
pub const LANCZOS: &str = "ffmpeg/lanczos";
pub const RIFE: &str = "rife";
const RIFE_EXE: &str = "rife-ncnn-vulkan";

#[derive(Default)]
pub struct Local {
    jobs: Arc<Mutex<HashMap<String, Job>>>,
    next: Mutex<u64>,
}

#[derive(Clone)]
enum Job {
    Running,
    Done(PathBuf),
    Failed(String),
}

fn on_path(exe: &str) -> Option<PathBuf> {
    let name = format!("{exe}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&name))
        .find(|p| p.is_file())
}

pub fn models() -> Vec<ModelInfo> {
    let mut m = vec![
        ModelInfo {
            id: MINTERPOLATE.into(),
            op: Op::VideoInterpolate,
            ..ModelInfo::default()
        },
        ModelInfo {
            id: LANCZOS.into(),
            op: Op::VideoUpscale,
            ..ModelInfo::default()
        },
    ];
    if on_path(RIFE_EXE).is_some() {
        m.push(ModelInfo {
            id: RIFE.into(),
            op: Op::VideoInterpolate,
            ..ModelInfo::default()
        });
    }
    m
}

fn invalid(msg: impl Into<String>, field: &str) -> Error {
    PluginError::new(ErrorKind::Validation, msg)
        .field(field)
        .into()
}

fn check(req: &Request) -> Result<PathBuf> {
    let Some(m) = models().into_iter().find(|m| m.id == req.model) else {
        let hint = if req.model == RIFE {
            format!("; install {RIFE_EXE} and put it on PATH")
        } else {
            String::new()
        };
        return Err(invalid(format!("no model {}{hint}", req.model), "model"));
    };
    if m.op != req.op {
        return Err(invalid(format!("{} does {}", m.id, m.op), "op"));
    }
    let video = req
        .inputs
        .get("video")
        .ok_or_else(|| invalid("needs a video", "inputs.video"))?;
    if !video.is_file() {
        return Err(invalid(
            format!("{} isn't there", video.display()),
            "inputs.video",
        ));
    }
    Ok(video.clone())
}

fn number(req: &Request, key: &str) -> Option<f64> {
    req.params.get(key).and_then(Value::as_f64)
}

fn ffmpeg() -> PathBuf {
    std::env::var_os("SHOTLINE_FFMPEG")
        .map(PathBuf::from)
        .unwrap_or_else(|| "ffmpeg".into())
}

fn ffprobe() -> PathBuf {
    std::env::var_os("SHOTLINE_FFPROBE")
        .map(PathBuf::from)
        .unwrap_or_else(|| "ffprobe".into())
}

fn exec(cmd: &mut Command, what: &str) -> std::result::Result<String, String> {
    let out = cmd.output().map_err(|e| format!("can't run {what}: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.trim().lines().rev().take(5).collect();
        return Err(format!(
            "{what} failed: {}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn fps(video: &Path) -> std::result::Result<f64, String> {
    let out = exec(
        Command::new(ffprobe())
            .args(["-v", "error", "-select_streams", "v:0"])
            .args(["-show_entries", "stream=r_frame_rate", "-of", "csv=p=0"])
            .arg(video),
        "ffprobe",
    )?;
    let s = out.trim();
    let v = match s.split_once('/') {
        Some((n, d)) => n.parse::<f64>().unwrap_or(0.0) / d.parse::<f64>().unwrap_or(1.0),
        None => s.parse().unwrap_or(0.0),
    };
    if v > 0.0 {
        Ok(v)
    } else {
        Err(format!("can't tell the frame rate of {}", video.display()))
    }
}

/// The encode every model ends with, keeping the source's sound if it has any.
fn encode(cmd: &mut Command, out: &Path) {
    cmd.args(["-c:v", "libx264", "-crf", "16", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "copy", "-movflags", "+faststart"])
        .arg(out);
}

fn work(req: &Request, video: &Path, dir: &Path) -> std::result::Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let out = dir.join("out.mp4");
    let mut cmd = Command::new(ffmpeg());
    cmd.args(["-v", "error", "-nostdin", "-y"]);
    match req.model.as_str() {
        MINTERPOLATE => {
            let target = match number(req, "fps") {
                Some(f) => f,
                None => fps(video)? * 2.0,
            };
            cmd.arg("-i").arg(video).args([
                "-vf",
                &format!(
                    "minterpolate=fps={target}:mi_mode=mci:mc_mode=aobmc:me_mode=bidir:vsbmc=1"
                ),
            ]);
            encode(&mut cmd, &out);
            exec(&mut cmd, "ffmpeg")?;
        }
        LANCZOS => {
            let s = number(req, "scale").unwrap_or(2.0);
            cmd.arg("-i").arg(video).args([
                "-vf",
                &format!("scale=trunc(iw*{s}/2)*2:trunc(ih*{s}/2)*2:flags=lanczos"),
            ]);
            encode(&mut cmd, &out);
            exec(&mut cmd, "ffmpeg")?;
        }
        RIFE => {
            let rife = on_path(RIFE_EXE).ok_or(format!("{RIFE_EXE} isn't on PATH"))?;
            let src_fps = fps(video)?;
            let tmp = dir.join("rife");
            let (frames, doubled) = (tmp.join("in"), tmp.join("out"));
            for d in [&frames, &doubled] {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            exec(
                Command::new(ffmpeg())
                    .args(["-v", "error", "-nostdin", "-y", "-i"])
                    .arg(video)
                    .arg(frames.join("%08d.png")),
                "ffmpeg",
            )?;
            let mut r = Command::new(rife);
            r.arg("-i").arg(&frames).arg("-o").arg(&doubled);
            if let Some(m) = req.params.get("rife_model").and_then(Value::as_str) {
                r.args(["-m", m]);
            }
            exec(&mut r, RIFE_EXE)?;
            cmd.args(["-framerate", &(src_fps * 2.0).to_string(), "-i"])
                .arg(doubled.join("%08d.png"))
                .arg("-i")
                .arg(video)
                .args(["-map", "0:v", "-map", "1:a?", "-shortest"]);
            encode(&mut cmd, &out);
            let res = exec(&mut cmd, "ffmpeg");
            let _ = std::fs::remove_dir_all(&tmp);
            res?;
        }
        other => return Err(format!("no model {other}")),
    }
    Ok(out)
}

#[async_trait]
impl Plugin for Local {
    async fn describe(&self) -> Result<Description> {
        Ok(Description {
            name: "local".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: PROTOCOL_VERSION,
            env: vec![],
            models: models(),
        })
    }

    async fn quote(&self, req: &Request) -> Result<Quote> {
        check(req)?;
        Ok(Quote {
            amount: 0.0,
            unit: "USD".into(),
            units: None,
            unit_name: None,
        })
    }

    async fn run(&self, req: &Request, output_dir: &Path) -> Result<String> {
        let video = check(req)?;
        let id = {
            let mut n = self.next.lock().unwrap();
            *n += 1;
            format!("local-{n}")
        };
        self.jobs.lock().unwrap().insert(id.clone(), Job::Running);
        let (jobs, req, dir, key) = (
            self.jobs.clone(),
            req.clone(),
            output_dir.to_path_buf(),
            id.clone(),
        );
        tokio::task::spawn_blocking(move || {
            let state = match work(&req, &video, &dir) {
                Ok(out) => Job::Done(out),
                Err(e) => Job::Failed(e),
            };
            jobs.lock().unwrap().insert(key, state);
        });
        Ok(id)
    }

    async fn poll(&self, job: &str) -> Result<Status> {
        let state = self.jobs.lock().unwrap().get(job).cloned();
        let (state, error) = match state {
            None => {
                return Err(PluginError::new(ErrorKind::NotFound, format!("no job {job}")).into());
            }
            Some(Job::Running) => (State::Running, None),
            Some(Job::Done(_)) => (State::Done, None),
            Some(Job::Failed(e)) => (State::Failed, Some(PluginError::new(ErrorKind::Other, e))),
        };
        Ok(Status {
            state,
            progress: None,
            error,
            cost: None,
        })
    }

    async fn fetch(&self, job: &str, output_dir: &Path) -> Result<Outputs> {
        let Some(Job::Done(out)) = self.jobs.lock().unwrap().get(job).cloned() else {
            return Err(PluginError::new(ErrorKind::NotFound, format!("{job} isn't done")).into());
        };
        let dest = output_dir.join("out.mp4");
        if dest != out {
            let io = |e: std::io::Error| PluginError::new(ErrorKind::Other, e.to_string());
            std::fs::create_dir_all(output_dir).map_err(io)?;
            std::fs::copy(&out, &dest).map_err(io)?;
        }
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
