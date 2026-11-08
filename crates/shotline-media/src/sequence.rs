//! A video editor's sequence made into one small file to preview: at each moment the highest
//! clip that's on, with its LUTs and dissolves, the sound tracks mixed. Other effects are left
//! out.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Error, Ffmpeg, Result, run};

/// Longest side of the preview, in pixels.
const LONGEST: u32 = 960;

#[derive(Debug, Clone, Default)]
pub struct Piece {
    pub path: PathBuf,
    /// For what's left out, e.g. the clip's name.
    pub label: String,
    /// Where it starts in the sequence, seconds.
    pub start: f64,
    /// How long it lasts in the sequence.
    pub length: f64,
    /// Where in the file it starts.
    pub from: f64,
    pub speed: f64,
    /// .cube files, applied in order.
    pub luts: Vec<PathBuf>,
    /// Comes in over what's under it: (from, seconds), in sequence time.
    pub fade_in: Option<(f64, f64)>,
    /// Goes out to what's under it.
    pub fade_out: Option<(f64, f64)>,
}

#[derive(Debug, Clone)]
pub struct Sequence {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub length: f64,
    /// Lowest track first: a later piece covers an earlier one where they overlap.
    pub video: Vec<Piece>,
    pub audio: Vec<Piece>,
}

/// Pieces left out, and why.
pub type Skipped = Vec<String>;

fn is_still(p: &Path) -> bool {
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    ["png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff"]
        .iter()
        .any(|e| ext.eq_ignore_ascii_case(e))
}

/// atempo takes 0.5..2, so further speeds are chained.
fn tempo(mut speed: f64) -> String {
    let mut parts = vec![];
    while speed > 2.0 + 1e-9 {
        parts.push("atempo=2".to_string());
        speed /= 2.0;
    }
    while speed < 0.5 - 1e-9 {
        parts.push("atempo=0.5".to_string());
        speed /= 0.5;
    }
    if (speed - 1.0).abs() > 1e-6 {
        parts.push(format!("atempo={speed:.6}"));
    }
    parts.iter().map(|p| format!(",{p}")).collect()
}

/// A path as a filter option's value: escaped for the option, then for the graph.
fn filter_path(p: &Path) -> String {
    let esc = |s: &str, special: &str| {
        s.chars().fold(String::new(), |mut o, c| {
            if special.contains(c) {
                o.push('\\');
            }
            o.push(c);
            o
        })
    };
    esc(&esc(&p.to_string_lossy(), "\\':"), "\\'[],;")
}

/// The preview's size: the sequence's shape, at most `LONGEST`, even.
pub fn preview_size(width: u32, height: u32) -> (u32, u32) {
    let (w, h) = (width.max(2) as f64, height.max(2) as f64);
    let k = (LONGEST as f64 / w.max(h)).min(1.0);
    let even = |x: f64| ((x * k / 2.0).round() as u32 * 2).max(2);
    (even(w), even(h))
}

impl Ffmpeg {
    pub fn has_audio(&self, path: &Path) -> bool {
        run(
            Command::new(&self.ffprobe)
                .args(["-v", "error", "-select_streams", "a"])
                .args(["-show_entries", "stream=index", "-of", "csv=p=0"])
                .arg(path),
            "ffprobe",
        )
        .is_ok_and(|o| !o.trim_ascii().is_empty())
    }

    /// Writes `seq` to `out` as an H.264 file with AAC sound.
    pub fn sequence_preview(&self, seq: &Sequence, out: &Path) -> Result<Skipped> {
        if seq.length <= 0.0 {
            return Err(Error::Invalid("the sequence is empty".into()));
        }
        let (w, h) = preview_size(seq.width, seq.height);
        let fps = if seq.fps > 0.0 { seq.fps } else { 24.0 };
        let mut skipped = vec![];
        let usable = |p: &Piece, skipped: &mut Skipped| {
            if p.length <= 0.0 || p.speed <= 0.0 {
                return false;
            }
            if !p.path.is_file() {
                skipped.push(format!("{}: {} isn't there", p.label, p.path.display()));
                return false;
            }
            true
        };
        let mut video: Vec<Piece> = vec![];
        for p in &seq.video {
            if !usable(p, &mut skipped) {
                continue;
            }
            let mut p = p.clone();
            p.luts.retain(|l| {
                let ok = l.is_file();
                if !ok {
                    skipped.push(format!("{}: LUT {} isn't a file", p.label, l.display()));
                }
                ok
            });
            video.push(p);
        }
        let mut audio = vec![];
        for p in &seq.audio {
            if !usable(p, &mut skipped) {
                continue;
            }
            if self.has_audio(&p.path) {
                audio.push(p);
            } else {
                skipped.push(format!("{}: no sound in it", p.label));
            }
        }

        let mut cmd = self.ffmpeg();
        let mut fc = vec![format!(
            "color=c=black:s={w}x{h}:r={fps}:d={:.4}[b0]",
            seq.length
        )];
        let mut n = 0;
        for (i, p) in video.iter().enumerate() {
            let used = p.length * p.speed;
            if is_still(&p.path) {
                cmd.args(["-loop", "1", "-framerate", &format!("{fps}")]);
                cmd.args(["-t", &format!("{used:.4}")]);
            } else {
                cmd.args([
                    "-ss",
                    &format!("{:.4}", p.from),
                    "-t",
                    &format!("{used:.4}"),
                ]);
            }
            cmd.arg("-i").arg(&p.path);
            let end = p.start + p.length;
            let luts: String = p
                .luts
                .iter()
                .map(|l| format!("lut3d=file={},", filter_path(l)))
                .collect();
            let mut fades = String::new();
            if p.fade_in.is_some() || p.fade_out.is_some() {
                fades.push_str(",format=yuva420p");
            }
            for (way, f) in [("in", p.fade_in), ("out", p.fade_out)] {
                if let Some((at, d)) = f.filter(|f| f.1 > 0.0) {
                    fades.push_str(&format!(",fade=t={way}:st={at:.4}:d={d:.4}:alpha=1"));
                }
            }
            // A piece run past its file's end (a dissolve's handle) holds its last frame.
            fc.push(format!(
                "[{n}:v]setpts=(PTS-STARTPTS)/{s:.6}+{st:.4}/TB,{luts}\
                 scale={w}:{h}:force_original_aspect_ratio=decrease,\
                 pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1{fades}[c{i}];\
                 [b{i}][c{i}]overlay=eof_action=repeat:enable='between(t,{st:.4},{end:.4})'[b{j}]",
                s = p.speed,
                st = p.start,
                j = i + 1,
            ));
            n += 1;
        }
        fc.push(format!("[b{}]format=yuv420p[vout]", video.len()));

        let mut mix = String::new();
        for (i, p) in audio.iter().enumerate() {
            cmd.args(["-ss", &format!("{:.4}", p.from)])
                .args(["-t", &format!("{:.4}", p.length * p.speed)])
                .arg("-i")
                .arg(&p.path);
            let ms = (p.start.max(0.0) * 1000.0).round() as u64;
            fc.push(format!(
                "[{n}:a]aformat=sample_rates=48000:channel_layouts=stereo{},adelay={ms}:all=1[a{i}]",
                tempo(p.speed)
            ));
            mix.push_str(&format!("[a{i}]"));
            n += 1;
        }
        let total = format!("{:.4}", seq.length);
        if audio.is_empty() {
            fc.push(format!("anullsrc=r=48000:cl=stereo,atrim=0:{total}[aout]"));
        } else {
            fc.push(format!(
                "{mix}amix=inputs={}:duration=longest:normalize=0,apad,atrim=0:{total}[aout]",
                audio.len()
            ));
        }

        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Written aside and moved in, so a preview cut short is never taken for a finished one.
        let part = out.with_extension("part.mp4");
        let gop = (fps * 2.0).round().max(1.0).to_string();
        cmd.args(["-filter_complex", &fc.join(";")])
            .args(["-map", "[vout]", "-map", "[aout]", "-t", &total])
            .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "26"])
            .args(["-g", &gop, "-c:a", "aac", "-b:a", "160k"])
            .args(["-movflags", "+faststart"])
            .arg(&part);
        run(&mut cmd, "making the sequence's preview")?;
        std::fs::rename(&part, out)?;
        Ok(skipped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speeds_past_atempo_s_range_are_chained() {
        assert_eq!(tempo(1.0), "");
        assert_eq!(tempo(1.5), ",atempo=1.500000");
        assert_eq!(tempo(4.0), ",atempo=2,atempo=2.000000");
        assert_eq!(tempo(0.25), ",atempo=0.5,atempo=0.500000");
    }

    #[test]
    fn paths_in_a_filter_are_escaped_twice() {
        assert_eq!(filter_path(Path::new("/a/b.cube")), "/a/b.cube");
        assert_eq!(filter_path(Path::new("C:\\x.cube")), "C\\\\:\\\\\\\\x.cube");
        assert_eq!(filter_path(Path::new("/a,b.cube")), "/a\\,b.cube");
    }

    #[test]
    fn preview_keeps_the_shape() {
        assert_eq!(preview_size(1920, 1080), (960, 540));
        assert_eq!(preview_size(1080, 1920), (540, 960));
        assert_eq!(preview_size(640, 360), (640, 360));
        assert_eq!(preview_size(0, 0), (2, 2));
    }
}
