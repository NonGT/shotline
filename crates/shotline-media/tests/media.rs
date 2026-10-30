//! Needs ffmpeg; each test passes without doing anything when it isn't installed.

use std::path::{Path, PathBuf};
use std::process::Command;

use shotline_media::colour::stats;
use shotline_media::render::{Clip, Options, Track, render};
use shotline_media::{Ffmpeg, Info};

fn ff() -> Option<Ffmpeg> {
    match Ffmpeg::find() {
        Ok(f) => Some(f),
        Err(e) => {
            eprintln!("skipped: {e}");
            None
        }
    }
}

/// A second of test pattern, tinted by `mix` (a colorchannelmixer setting), with a tone if asked.
fn clip(dir: &Path, name: &str, mix: &str, tone: bool) -> PathBuf {
    let path = dir.join(name);
    let ffmpeg = std::env::var("SHOTLINE_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let mut c = Command::new(ffmpeg);
    c.args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc=size=160x90:rate=24:duration=1");
    if tone {
        c.args(["-f", "lavfi", "-i", "sine=frequency=440:duration=1"]);
    }
    c.args(["-vf", &format!("colorchannelmixer={mix}")])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"]);
    if tone {
        c.args(["-c:a", "aac", "-shortest"]);
    }
    assert!(c.arg(&path).status().unwrap().success());
    path
}

fn tone(dir: &Path) -> PathBuf {
    let path = dir.join("music.wav");
    let ffmpeg = std::env::var("SHOTLINE_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    assert!(
        Command::new(ffmpeg)
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("sine=frequency=220:duration=2")
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    path
}

const RED: &str = "rr=1:gg=0.5:bb=0.4";
const BLUE: &str = "rr=0.4:gg=0.6:bb=1";

#[test]
fn last_frame() {
    let Some(ff) = ff() else { return };
    let dir = tempfile::tempdir().unwrap();
    let v = clip(dir.path(), "a.mp4", RED, false);
    let png = dir.path().join("jobs/x/last.png");
    ff.last_frame(&v, &png).unwrap();
    let info = ff.probe(&png).unwrap();
    assert_eq!((info.width, info.height), (160, 90));
}

fn mean(frame: &[u8]) -> [f64; 3] {
    stats([frame]).0
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| (a[i] - b[i]).powi(2)).sum::<f64>().sqrt()
}

/// How far apart the colours are either side of the join between two one-second clips.
fn seam(ff: &Ffmpeg, out: &Path) -> f64 {
    let info = ff.probe(out).unwrap();
    let frames: Vec<Vec<u8>> = ff
        .frames(out, info.width, info.height, &info.fps, None)
        .unwrap()
        .collect();
    distance(mean(&frames[23]), mean(&frames[24]))
}

#[test]
fn colour_match_brings_clips_closer() {
    let Some(ff) = ff() else { return };
    let dir = tempfile::tempdir().unwrap();
    let a = clip(dir.path(), "a.mp4", RED, false);
    let b = clip(dir.path(), "b.mp4", BLUE, false);
    let go = |matched: bool, name: &str| {
        let clips = [
            Clip {
                path: a.clone(),
                label: "a".into(),
                colormatch: false,
            },
            Clip {
                path: b.clone(),
                label: "b".into(),
                colormatch: matched,
            },
        ];
        let out = dir.path().join(name);
        let mut seen = vec![];
        render(
            &ff,
            &clips,
            &[],
            &Options::default(),
            &dir.path().join(format!("work-{name}")),
            &out,
            |p| seen.push(format!("{p:?}")),
        )
        .unwrap();
        assert!(
            seen.iter()
                .any(|s| s.contains(&format!("matched: {matched}")))
        );
        seam(&ff, &out)
    };
    let plain = go(false, "plain.mp4");
    let matched = go(true, "matched.mp4");
    assert!(
        matched < plain / 3.0,
        "seam {matched} after matching vs {plain} before"
    );
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.15
}

#[test]
fn renders_clips_and_tracks() {
    let Some(ff) = ff() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clips: Vec<Clip> = [
        ("a.mp4", RED, false),
        ("b.mp4", BLUE, true),
        ("c.mp4", RED, false),
    ]
    .iter()
    .map(|(n, mix, t)| Clip {
        path: clip(dir.path(), n, mix, *t),
        label: n.to_string(),
        colormatch: false,
    })
    .collect();
    let music = Track {
        path: tone(dir.path()),
        at: 0.5,
        volume: 0.5,
    };

    let out = dir.path().join("renders/cut.mp4");
    let r = render(
        &ff,
        &clips,
        std::slice::from_ref(&music),
        &Options::default(),
        &dir.path().join("work"),
        &out,
        |_| {},
    )
    .unwrap();
    assert!(close(r.duration, 3.0), "{}", r.duration);
    let Info {
        width,
        height,
        duration,
        audio,
        ..
    } = ff.probe(&out).unwrap();
    assert_eq!((width, height, audio), (160, 90, true));
    assert!(close(duration, 3.0), "{duration}");

    let faded = dir.path().join("faded.mp4");
    let opts = Options {
        xfade: 0.25,
        ..Options::default()
    };
    let r = render(
        &ff,
        &clips,
        &[],
        &opts,
        &dir.path().join("work2"),
        &faded,
        |_| {},
    )
    .unwrap();
    assert!(close(r.duration, 2.5), "{}", r.duration);
    assert!(close(ff.probe(&faded).unwrap().duration, 2.5));

    // Without any sound there's no audio stream.
    let silent: Vec<Clip> = clips
        .iter()
        .filter(|c| c.label != "b.mp4")
        .cloned()
        .collect();
    let quiet = dir.path().join("quiet.mp4");
    render(
        &ff,
        &silent,
        &[],
        &Options::default(),
        &dir.path().join("w3"),
        &quiet,
        |_| {},
    )
    .unwrap();
    assert!(!ff.probe(&quiet).unwrap().audio);
}
