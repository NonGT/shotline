//! Everything done to media files, through ffmpeg and ffprobe run as separate processes: last
//! frames, colour matching, and rendering the timeline.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub mod colour;
pub mod lines;
pub mod play;
pub mod render;
pub mod sequence;
pub mod splice;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("can't run {0}; install ffmpeg or point SHOTLINE_FFMPEG / SHOTLINE_FFPROBE at it")]
    Missing(String),
    #[error("{what} failed: {stderr}")]
    Failed { what: String, stderr: String },
    #[error("{0}")]
    Invalid(String),
    #[error("stopped")]
    Stopped,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub struct Ffmpeg {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    pub width: u32,
    pub height: u32,
    /// As ffprobe gives it, e.g. `24/1` or `30000/1001`.
    pub fps: String,
    pub duration: f64,
    /// The picture's own length; the sound can run on a little past it.
    pub video_duration: f64,
    pub audio: bool,
}

impl Info {
    pub fn fps_value(&self) -> f64 {
        match self.fps.split_once('/') {
            Some((n, d)) => {
                n.parse::<f64>().unwrap_or(0.0) / d.parse::<f64>().unwrap_or(1.0).max(1e-9)
            }
            None => self.fps.parse().unwrap_or(0.0),
        }
    }
}

fn run(cmd: &mut Command, what: &str) -> Result<Vec<u8>> {
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(Error::Failed {
            what: what.into(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(out.stdout)
}

impl Ffmpeg {
    /// From SHOTLINE_FFMPEG and SHOTLINE_FFPROBE, or `ffmpeg`/`ffprobe` on PATH.
    pub fn find() -> Result<Ffmpeg> {
        let var = |v: &str, default: &str| {
            std::env::var_os(v)
                .map(PathBuf::from)
                .unwrap_or_else(|| default.into())
        };
        let ff = Ffmpeg {
            ffmpeg: var("SHOTLINE_FFMPEG", "ffmpeg"),
            ffprobe: var("SHOTLINE_FFPROBE", "ffprobe"),
        };
        for exe in [&ff.ffmpeg, &ff.ffprobe] {
            let ok = Command::new(exe)
                .arg("-version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                return Err(Error::Missing(exe.display().to_string()));
            }
        }
        Ok(ff)
    }

    fn ffmpeg(&self) -> Command {
        let mut c = Command::new(&self.ffmpeg);
        c.args(["-v", "error", "-nostdin", "-y"]);
        c
    }

    pub fn probe(&self, path: &Path) -> Result<Info> {
        let out = run(
            Command::new(&self.ffprobe)
                .args([
                    "-v",
                    "error",
                    "-show_streams",
                    "-show_format",
                    "-of",
                    "json",
                ])
                .arg(path),
            &format!("ffprobe {}", path.display()),
        )?;
        let v: serde_json::Value = serde_json::from_slice(&out)
            .map_err(|e| Error::Invalid(format!("ffprobe {}: {e}", path.display())))?;
        let streams = v["streams"].as_array().cloned().unwrap_or_default();
        let video = streams
            .iter()
            .find(|s| s["codec_type"] == "video")
            .ok_or_else(|| Error::Invalid(format!("{} has no video", path.display())))?;
        let num = |x: &serde_json::Value| x.as_u64().unwrap_or(0) as u32;
        let secs = |x: &serde_json::Value| x.as_str().and_then(|d| d.parse::<f64>().ok());
        let duration = secs(&v["format"]["duration"]).unwrap_or(0.0);
        Ok(Info {
            width: num(&video["width"]),
            height: num(&video["height"]),
            fps: video["r_frame_rate"].as_str().unwrap_or("24/1").into(),
            duration,
            video_duration: secs(&video["duration"]).unwrap_or(duration),
            audio: streams.iter().any(|s| s["codec_type"] == "audio"),
        })
    }

    /// How long any media file runs, sound only included, in seconds.
    pub fn length(&self, path: &Path) -> Result<f64> {
        let out = run(
            Command::new(&self.ffprobe)
                .args([
                    "-v",
                    "error",
                    "-show_entries",
                    "format=duration",
                    "-of",
                    "csv=p=0",
                ])
                .arg(path),
            &format!("ffprobe {}", path.display()),
        )?;
        String::from_utf8_lossy(&out)
            .trim()
            .parse()
            .map_err(|_| Error::Invalid(format!("{} has no length", path.display())))
    }

    /// Writes the last frame of `video` as a PNG.
    /// shotline's colour match for a join as a .cube LUT for the clip after it: its first
    /// `frames` from `next_start` onto the last ones of `prev` before `prev_end` (seconds into
    /// each file).
    pub fn match_lut(
        &self,
        (prev, prev_end): (&Path, f64),
        (next, next_start): (&Path, f64),
        frames: usize,
        strength: f64,
        out: &Path,
    ) -> Result<()> {
        let info = self.probe(prev)?;
        let fps = info.fps_value().max(1.0);
        let n = frames.max(1);
        // Small is plenty for stats, and keeps the aspect so no bars count.
        let w = 160u32;
        let h =
            ((w as f64 * info.height.max(1) as f64 / info.width.max(1) as f64) as u32).max(2) & !1;
        let fps_s = format!("{fps}");
        let from = (prev_end - n as f64 / fps).max(0.0);
        let tail: Vec<Vec<u8>> = self
            .frames_from(prev, from, w, h, &fps_s, Some(n))?
            .collect();
        let head: Vec<Vec<u8>> = self
            .frames_from(next, next_start, w, h, &fps_s, Some(n))?
            .collect();
        if tail.is_empty() || head.is_empty() {
            return Err(Error::Invalid("no frames to match".into()));
        }
        let t = colour::Transform::between(
            colour::stats(head.iter().map(|f| &f[..])),
            colour::stats(tail.iter().map(|f| &f[..])),
            strength,
        );
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(out, t.cube(33))?;
        Ok(())
    }

    /// `length` seconds of `video` from `from`, cut on the frame (so encoded again).
    pub fn part(&self, video: &Path, from: f64, length: f64, out: &Path) -> Result<()> {
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        run(
            self.ffmpeg()
                .args(["-ss", &format!("{from:.4}"), "-i"])
                .arg(video)
                .args(["-t", &format!("{length:.4}")])
                .args([
                    "-c:v", "libx264", "-crf", "16", "-pix_fmt", "yuv420p", "-c:a", "aac",
                ])
                .arg(out),
            &format!("cutting {}", video.display()),
        )?;
        Ok(())
    }

    /// The sound of `from`, a file or a URL like a voice's preview, as a WAV at `out`.
    pub fn sound_to_wav(&self, from: &str, out: &Path) -> Result<()> {
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        run(
            Command::new(&self.ffmpeg)
                .args(["-v", "error", "-y", "-i", from, "-vn"])
                .arg(out),
            "getting the sound",
        )?;
        Ok(())
    }

    pub fn last_frame(&self, video: &Path, png: &Path) -> Result<()> {
        if let Some(dir) = png.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Decode the last half second, overwriting the image each frame.
        run(
            self.ffmpeg()
                .args(["-sseof", "-0.5", "-i"])
                .arg(video)
                .args(["-update", "1", "-an"])
                .arg(png),
            &format!("taking the last frame of {}", video.display()),
        )?;
        if !png.is_file() {
            return Err(Error::Invalid(format!("no frames in {}", video.display())));
        }
        Ok(())
    }

    /// The frame of `video` showing at `at` seconds, as a PNG at full size.
    pub fn frame_at(&self, video: &Path, at: f64, png: &Path) -> Result<()> {
        if let Some(dir) = png.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let info = self.probe(video)?;
        let fps = info.fps_value().max(1.0);
        let last = ((info.video_duration * fps).ceil() - 1.0).max(0.0);
        let n = (at * fps + 1e-6).floor().clamp(0.0, last);
        // Input seeking drops what comes before, so half a frame early lands on frame n.
        let from = ((n - 0.5) / fps).max(0.0);
        let _ = std::fs::remove_file(png);
        run(
            self.ffmpeg()
                .args(["-ss", &format!("{from:.6}"), "-i"])
                .arg(video)
                .args(["-frames:v", "1", "-update", "1", "-an"])
                .arg(png),
            &format!("taking a frame of {}", video.display()),
        )?;
        if !png.is_file() {
            return Err(Error::Invalid(format!(
                "no frame at {at:.3}s in {}",
                video.display()
            )));
        }
        Ok(())
    }

    /// A small still from near the start of `video`, to show it by.
    pub fn poster(&self, video: &Path, png: &Path) -> Result<()> {
        if let Some(dir) = png.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Half a second in skips a black first frame; a shorter clip falls back to its first.
        for at in ["0.5", "0"] {
            run(
                self.ffmpeg()
                    .args(["-ss", at, "-i"])
                    .arg(video)
                    .args(["-frames:v", "1", "-vf", "scale=320:-2", "-an"])
                    .arg(png),
                &format!("taking a still from {}", video.display()),
            )?;
            if png.is_file() {
                return Ok(());
            }
        }
        Err(Error::Invalid(format!("no frames in {}", video.display())))
    }

    /// Decodes `path` to RGB24 frames of exactly `width` x `height` (scaled and padded to fit)
    /// at `fps`, at most `limit` of them.
    pub fn frames(
        &self,
        path: &Path,
        width: u32,
        height: u32,
        fps: &str,
        limit: Option<usize>,
    ) -> Result<Frames> {
        self.frames_from(path, 0.0, width, height, fps, limit)
    }

    /// Like `frames`, starting `from` seconds in.
    pub fn frames_from(
        &self,
        path: &Path,
        from: f64,
        width: u32,
        height: u32,
        fps: &str,
        limit: Option<usize>,
    ) -> Result<Frames> {
        let vf = format!(
            "scale={width}:{height}:force_original_aspect_ratio=decrease,\
             pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps={fps}"
        );
        let mut c = self.ffmpeg();
        if from > 0.0 {
            c.args(["-ss", &format!("{from:.4}")]);
        }
        c.arg("-i").arg(path).args(["-an", "-vf", &vf]);
        if let Some(n) = limit {
            c.args(["-frames:v", &n.to_string()]);
        }
        c.args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = c.spawn().map_err(|_| Error::Missing("ffmpeg".into()))?;
        let out = child.stdout.take().expect("piped");
        Ok(Frames {
            child,
            out,
            size: (width * height * 3) as usize,
        })
    }

    /// Takes RGB24 frames and writes them losslessly (FFV1 in Matroska).
    pub fn writer(&self, path: &Path, width: u32, height: u32, fps: &str) -> Result<Writer> {
        let mut c = self.ffmpeg();
        c.args(["-f", "rawvideo", "-pix_fmt", "rgb24"])
            .args(["-s", &format!("{width}x{height}"), "-r", fps, "-i", "-"])
            .args(["-c:v", "ffv1", "-pix_fmt", "bgr0"])
            .arg(path)
            .stdin(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().map_err(|_| Error::Missing("ffmpeg".into()))?;
        let input = child.stdin.take().expect("piped");
        Ok(Writer {
            child,
            input: Some(input),
            path: path.to_path_buf(),
        })
    }

    pub(crate) fn command(&self) -> Command {
        self.ffmpeg()
    }
}

pub struct Frames {
    child: Child,
    out: ChildStdout,
    size: usize,
}

impl Iterator for Frames {
    type Item = Vec<u8>;

    fn next(&mut self) -> Option<Vec<u8>> {
        let mut buf = vec![0; self.size];
        self.out.read_exact(&mut buf).ok()?;
        Some(buf)
    }
}

impl Drop for Frames {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct Writer {
    child: Child,
    input: Option<ChildStdin>,
    path: PathBuf,
}

impl Writer {
    pub fn write(&mut self, frame: &[u8]) -> Result<()> {
        self.input
            .as_mut()
            .expect("open until finish")
            .write_all(frame)?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        drop(self.input.take());
        let mut err = String::new();
        if let Some(mut e) = self.child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        if !self.child.wait()?.success() {
            return Err(Error::Failed {
                what: format!("writing {}", self.path.display()),
                stderr: err.trim().to_string(),
            });
        }
        Ok(())
    }
}
