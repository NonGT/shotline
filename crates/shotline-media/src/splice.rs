//! A take made from pieces of others: each piece is cut out, colour matched to the one before,
//! and joined with a cut, a fade or made-up frames in between, then encoded once.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::colour::{Transform, stats};
use crate::render::Rendered;
use crate::{Error, Ffmpeg, Result};

/// How a piece meets the one before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Join {
    #[default]
    Cut,
    /// The two overlap and fade into each other.
    Fade,
    /// Frames in between are made up by motion interpolation, for two pieces that almost line up.
    Smooth,
}

#[derive(Debug, Clone)]
pub struct Piece {
    pub path: PathBuf,
    /// Seconds into the file.
    pub from: f64,
    pub to: f64,
    /// Ignored for the first piece.
    pub join: Join,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Length of a fade, seconds.
    pub fade: f64,
    /// Length of the made-up frames at a smooth join, seconds.
    pub smooth: f64,
    pub colormatch: bool,
    pub match_frames: usize,
    pub crf: u32,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            fade: 0.3,
            smooth: 0.25,
            colormatch: true,
            match_frames: 5,
            crf: 18,
        }
    }
}

/// Puts `pieces` together into `out`, using `work` for the picture before it's encoded.
pub fn splice(
    ff: &Ffmpeg,
    pieces: &[Piece],
    opts: &Options,
    work: &Path,
    out: &Path,
) -> Result<Rendered> {
    let Some(first) = pieces.first() else {
        return Err(Error::Invalid("no pieces".into()));
    };
    let infos = pieces
        .iter()
        .map(|p| ff.probe(&p.path))
        .collect::<Result<Vec<_>>>()?;
    let (w, h) = (infos[0].width & !1, infos[0].height & !1);
    let fps = infos[0].fps.clone();
    let fps_v = infos[0].fps_value();
    if w == 0 || h == 0 || fps_v <= 0.0 {
        return Err(Error::Invalid(format!(
            "can't tell the size or frame rate of {}",
            first.path.display()
        )));
    }
    let fade = ((opts.fade * fps_v).round() as usize).max(1);
    let between = ((opts.smooth * fps_v).round() as usize).max(1);
    for (i, p) in pieces.iter().enumerate() {
        if p.from < 0.0 || p.to <= p.from {
            return Err(Error::Invalid(format!(
                "piece {} runs from {:.2}s to {:.2}s",
                i + 1,
                p.from,
                p.to
            )));
        }
        let frames = ((p.to - p.from) * fps_v).round() as usize;
        let fades = (i > 0 && p.join == Join::Fade)
            || pieces.get(i + 1).is_some_and(|n| n.join == Join::Fade);
        if fades && frames <= fade {
            return Err(Error::Invalid(format!(
                "piece {} is shorter than its fade of {}s",
                i + 1,
                opts.fade
            )));
        }
    }
    std::fs::create_dir_all(work)?;
    let video = work.join("pieces.mkv");
    let mut wr = ff.writer(&video, w, h, &fps)?;

    let n = opts.match_frames.max(1);
    // Held back from the writer: a fade changes them, and the colours and made-up frames are
    // worked out from them.
    let keep = n.max(fade);
    let mut held: VecDeque<Vec<u8>> = VecDeque::new();
    // Frames of picture each piece gave, and what was added at its join.
    let mut counts = vec![];
    let mut added = vec![];
    for (i, p) in pieces.iter().enumerate() {
        let want = ((p.to - p.from) * fps_v).round() as usize;
        let mut frames: Vec<Vec<u8>> = ff
            .frames_from(&p.path, p.from, w, h, &fps, Some(want))?
            .collect();
        if frames.is_empty() {
            return Err(Error::Invalid(format!(
                "piece {} has no frames; {} is {:.2}s long",
                i + 1,
                p.path.display(),
                infos[i].duration
            )));
        }
        if opts.colormatch && i > 0 && !held.is_empty() {
            let head = frames.iter().take(n).map(|f| &f[..]);
            let tail = held
                .iter()
                .skip(held.len().saturating_sub(n))
                .map(|f| &f[..]);
            let t = Transform::between(stats(head), stats(tail), 1.0);
            for f in &mut frames {
                t.apply(f);
            }
        }
        counts.push(frames.len());
        let join = if i == 0 { Join::Cut } else { p.join };
        let mut rest = frames.into_iter();
        match join {
            Join::Cut => added.push(0isize),
            Join::Fade => {
                let k = fade.min(held.len());
                let start = held.len() - k;
                for j in 0..k {
                    let Some(b) = rest.next() else { break };
                    let a = (j + 1) as f32 / (k + 1) as f32;
                    blend(&mut held[start + j], &b, a);
                }
                added.push(-(k as isize));
            }
            Join::Smooth => {
                let next = rest.next().expect("not empty");
                let made = match held.back() {
                    Some(last) => bridge(ff, last, &next, w, h, fps_v, between)?,
                    None => vec![],
                };
                added.push(made.len() as isize);
                for f in made.into_iter().chain([next]) {
                    push(&mut held, f, keep, &mut wr)?;
                }
            }
        }
        for f in rest {
            push(&mut held, f, keep, &mut wr)?;
        }
    }
    for f in held {
        wr.write(&f)?;
    }
    wr.finish()?;

    let lengths: Vec<f64> = counts.iter().map(|c| *c as f64 / fps_v).collect();
    let audio = infos.iter().any(|i| i.audio);
    let mut cmd = ff.command();
    cmd.arg("-i").arg(&video);
    if audio {
        for (p, len) in pieces.iter().zip(&lengths) {
            cmd.args(["-ss", &format!("{:.4}", p.from), "-t", &format!("{len:.4}")])
                .arg("-i")
                .arg(&p.path);
        }
        cmd.args(["-filter_complex", &sound(&infos, &lengths, &added, fps_v)])
            .args([
                "-map", "0:v", "-map", "[aout]", "-c:a", "aac", "-b:a", "192k",
            ]);
    } else {
        cmd.args(["-map", "0:v"]);
    }
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    cmd.args(["-c:v", "libx264", "-crf", &opts.crf.to_string()])
        .args([
            "-pix_fmt",
            "yuv420p",
            "-preset",
            "medium",
            "-movflags",
            "+faststart",
        ])
        .arg(out);
    let o = cmd.output()?;
    if !o.status.success() {
        return Err(Error::Failed {
            what: format!("encoding {}", out.display()),
            stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
        });
    }
    let frames = counts.iter().sum::<usize>() as isize + added.iter().sum::<isize>();
    Ok(Rendered {
        duration: frames as f64 / fps_v,
        width: w,
        height: h,
        fps,
    })
}

fn push(
    held: &mut VecDeque<Vec<u8>>,
    f: Vec<u8>,
    keep: usize,
    wr: &mut crate::Writer,
) -> Result<()> {
    held.push_back(f);
    while held.len() > keep {
        wr.write(&held.pop_front().expect("longer than keep"))?;
    }
    Ok(())
}

/// `a` becomes `a * (1 - t) + b * t`.
fn blend(a: &mut [u8], b: &[u8], t: f32) {
    for (x, y) in a.iter_mut().zip(b) {
        *x = (*x as f32 * (1.0 - t) + *y as f32 * t).round() as u8;
    }
}

/// `count` frames that move from `a` to `b`. minterpolate gives frames up to its second-to-last
/// input, so `b` goes in twice.
fn bridge(
    ff: &Ffmpeg,
    a: &[u8],
    b: &[u8],
    w: u32,
    h: u32,
    fps: f64,
    count: usize,
) -> Result<Vec<Vec<u8>>> {
    let rate = format!("{}/{}", (fps * 1000.0).round() as u64, 1000 * (count + 1));
    let mut c = ff.command();
    c.args(["-f", "rawvideo", "-pix_fmt", "rgb24"])
        .args(["-s", &format!("{w}x{h}"), "-r", &rate, "-i", "-"])
        .args([
            "-vf",
            &format!("minterpolate=fps={fps}:mi_mode=mci:mc_mode=aobmc:me_mode=bidir:vsbmc=1"),
        ])
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|_| Error::Missing("ffmpeg".into()))?;
    let mut input = child.stdin.take().expect("piped");
    let frames = [a.to_vec(), b.to_vec(), b.to_vec()];
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        for f in &frames {
            input.write_all(f)?;
        }
        Ok(())
    });
    let mut bytes = vec![];
    child
        .stdout
        .take()
        .expect("piped")
        .read_to_end(&mut bytes)?;
    let mut err = String::new();
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    let _ = writer.join();
    if !child.wait()?.success() {
        return Err(Error::Failed {
            what: "making frames between two pieces".into(),
            stderr: err.trim().to_string(),
        });
    }
    let size = (w * h * 3) as usize;
    // The first is `a` itself and the last `b`.
    Ok(bytes
        .chunks_exact(size)
        .skip(1)
        .take(count)
        .map(<[u8]>::to_vec)
        .collect())
}

/// The filter that joins the pieces' sound the way their pictures were joined. Input 0 is the
/// picture; piece `i` is input `i + 1`.
fn sound(infos: &[crate::Info], lengths: &[f64], added: &[isize], fps: f64) -> String {
    let mut fc = vec![];
    for (i, (info, d)) in infos.iter().zip(lengths).enumerate() {
        let src = if info.audio {
            format!(
                "[{}:a]aformat=sample_rates=48000:channel_layouts=stereo",
                i + 1
            )
        } else {
            "anullsrc=r=48000:cl=stereo".into()
        };
        fc.push(format!("{src},atrim=0:{d:.4},apad=whole_dur={d:.4}[s{i}]"));
    }
    let mut last = "[s0]".to_string();
    for (i, a) in added.iter().enumerate().skip(1) {
        let d = a.unsigned_abs() as f64 / fps;
        if *a < 0 {
            fc.push(format!("{last}[s{i}]acrossfade=d={d:.4}[j{i}]"));
        } else if *a > 0 {
            // Quiet under the made-up frames.
            fc.push(format!("anullsrc=r=48000:cl=stereo,atrim=0:{d:.4}[g{i}]"));
            fc.push(format!("{last}[g{i}][s{i}]concat=n=3:v=0:a=1[j{i}]"));
        } else {
            fc.push(format!("{last}[s{i}]concat=n=2:v=0:a=1[j{i}]"));
        }
        last = format!("[j{i}]");
    }
    fc.push(format!("{last}anull[aout]"));
    fc.join(";")
}
