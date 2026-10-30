//! Joining clips into one film: each clip is decoded to a common size and frame rate, colour
//! matched to the one before if asked, written losslessly, then everything is encoded once with
//! the clips' own audio and any extra tracks mixed in.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crate::colour::{Transform, stats};
use crate::{Error, Ffmpeg, Result};

#[derive(Debug, Clone)]
pub struct Clip {
    pub path: PathBuf,
    /// For progress, e.g. `01-020#3`.
    pub label: String,
    /// Match its colours to the end of the clip before.
    pub colormatch: bool,
}

/// Music or effects laid over the film.
#[derive(Debug, Clone)]
pub struct Track {
    pub path: PathBuf,
    /// Seconds into the film where it starts.
    pub at: f64,
    pub volume: f64,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Crossfade at each join, in seconds; 0 cuts.
    pub xfade: f64,
    /// Frames each side of a join used for colour stats.
    pub match_frames: usize,
    /// 0..1, how much of the colour correction to apply.
    pub strength: f64,
    pub crf: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            xfade: 0.0,
            match_frames: 5,
            strength: 1.0,
            crf: 18,
        }
    }
}

#[derive(Debug)]
pub enum Progress {
    Clip {
        index: usize,
        of: usize,
        label: String,
        matched: bool,
    },
    Encoding,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: String,
}

/// Renders `clips` and `tracks` to `out`, using `work` for intermediate files.
pub fn render(
    ff: &Ffmpeg,
    clips: &[Clip],
    tracks: &[Track],
    opts: &Options,
    work: &Path,
    out: &Path,
    mut on: impl FnMut(Progress),
) -> Result<Rendered> {
    let Some(first) = clips.first() else {
        return Err(Error::Invalid("nothing to render".into()));
    };
    let infos = clips
        .iter()
        .map(|c| ff.probe(&c.path))
        .collect::<Result<Vec<_>>>()?;
    // H.264 in yuv420p needs even sizes.
    let (w, h) = (infos[0].width & !1, infos[0].height & !1);
    let fps = infos[0].fps.clone();
    let fps_v = infos[0].fps_value();
    if w == 0 || h == 0 || fps_v <= 0.0 {
        return Err(Error::Invalid(format!(
            "can't tell the size or frame rate of {}",
            first.path.display()
        )));
    }
    std::fs::create_dir_all(work)?;

    let n = opts.match_frames.max(1);
    let mut fixed = vec![];
    let mut durations = vec![];
    let mut tail: VecDeque<Vec<u8>> = VecDeque::new();
    for (i, clip) in clips.iter().enumerate() {
        let matched = clip.colormatch && i > 0 && !tail.is_empty();
        on(Progress::Clip {
            index: i,
            of: clips.len(),
            label: clip.label.clone(),
            matched,
        });
        let mut frames = ff.frames(&clip.path, w, h, &fps, None)?;
        let head: Vec<Vec<u8>> = frames.by_ref().take(n).collect();
        let t = matched.then(|| {
            Transform::between(
                stats(head.iter().map(|f| &f[..])),
                stats(tail.iter().map(|f| &f[..])),
                opts.strength,
            )
        });
        let dst = work.join(format!("clip{i}.mkv"));
        let mut wr = ff.writer(&dst, w, h, &fps)?;
        tail.clear();
        let mut count = 0usize;
        for mut f in head.into_iter().chain(frames) {
            if let Some(t) = &t {
                t.apply(&mut f);
            }
            wr.write(&f)?;
            if tail.len() == n {
                tail.pop_front();
            }
            tail.push_back(f);
            count += 1;
        }
        wr.finish()?;
        if count == 0 {
            return Err(Error::Invalid(format!(
                "{} has no frames",
                clip.path.display()
            )));
        }
        fixed.push(dst);
        durations.push(count as f64 / fps_v);
    }

    let x = opts.xfade.max(0.0);
    if x > 0.0 && durations.iter().any(|d| *d <= x) {
        return Err(Error::Invalid(format!(
            "a crossfade of {x}s is longer than a clip"
        )));
    }
    let audio = infos.iter().any(|i| i.audio) || !tracks.is_empty();

    let mut cmd = ff.command();
    for (f, c) in fixed.iter().zip(clips) {
        // Video from the fixed copy, sound from the original.
        cmd.arg("-i").arg(f).arg("-i").arg(&c.path);
    }
    for t in tracks {
        cmd.arg("-i").arg(&t.path);
    }
    let k = clips.len();
    let mut fc: Vec<String> = vec![];
    let (mut vlast, mut alast) = ("[0:v]".to_string(), "[s0]".to_string());
    if audio {
        // One stretch of sound per clip, cut or padded to its picture so they stay in sync.
        for (i, (info, d)) in infos.iter().zip(&durations).enumerate() {
            let src = if info.audio {
                format!(
                    "[{}:a]aformat=sample_rates=48000:channel_layouts=stereo",
                    2 * i + 1
                )
            } else {
                "anullsrc=r=48000:cl=stereo".into()
            };
            fc.push(format!("{src},atrim=0:{d:.4},apad=whole_dur={d:.4}[s{i}]"));
        }
    }
    if x > 0.0 && k > 1 {
        let mut offset = 0.0;
        for i in 1..k {
            offset += durations[i - 1] - x;
            fc.push(format!(
                "{vlast}[{}:v]xfade=transition=fade:duration={x}:offset={offset:.4}[v{i}]",
                2 * i
            ));
            vlast = format!("[v{i}]");
            if audio {
                fc.push(format!("{alast}[s{i}]acrossfade=d={x}[a{i}]"));
                alast = format!("[a{i}]");
            }
        }
    } else {
        let ins: String = (0..k)
            .map(|i| {
                let a = if audio {
                    format!("[s{i}]")
                } else {
                    String::new()
                };
                format!("[{}:v]{a}", 2 * i)
            })
            .collect();
        let a_out = if audio { "[ac]" } else { "" };
        fc.push(format!(
            "{ins}concat=n={k}:v=1:a={}[vc]{a_out}",
            u8::from(audio)
        ));
        vlast = "[vc]".into();
        alast = "[ac]".into();
    }
    fc.push(format!("{vlast}format=yuv420p[vout]"));
    if !tracks.is_empty() {
        let mut mix = alast.clone();
        for (j, t) in tracks.iter().enumerate() {
            let ms = (t.at.max(0.0) * 1000.0).round() as u64;
            fc.push(format!(
                "[{}:a]aformat=sample_rates=48000:channel_layouts=stereo,volume={},adelay={ms}|{ms}[t{j}]",
                2 * k + j,
                t.volume
            ));
            mix.push_str(&format!("[t{j}]"));
        }
        fc.push(format!(
            "{mix}amix=inputs={}:duration=first:normalize=0[aout]",
            tracks.len() + 1
        ));
        alast = "[aout]".into();
    }

    on(Progress::Encoding);
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    cmd.args(["-filter_complex", &fc.join(";"), "-map", "[vout]"]);
    if audio {
        cmd.args(["-map", &alast, "-c:a", "aac", "-b:a", "192k"]);
    }
    cmd.args(["-c:v", "libx264", "-crf", &opts.crf.to_string()])
        .args(["-preset", "medium", "-movflags", "+faststart"])
        .arg(out);
    let o = cmd.output()?;
    if !o.status.success() {
        return Err(Error::Failed {
            what: format!("encoding {}", out.display()),
            stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
        });
    }
    let total: f64 = durations.iter().sum::<f64>() - x * (k as f64 - 1.0).max(0.0);
    Ok(Rendered {
        duration: total,
        width: w,
        height: h,
        fps,
    })
}
