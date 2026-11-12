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

#[test]
fn part_of_a_clip() {
    let Some(ff) = ff() else { return };
    let dir = tempfile::tempdir().unwrap();
    let v = clip(dir.path(), "a.mp4", RED, true);
    let out = dir.path().join("cache/parts/a.mp4");
    ff.part(&v, 0.25, 0.5, &out).unwrap();
    let info = ff.probe(&out).unwrap();
    assert!((info.duration - 0.5).abs() < 0.05, "{}", info.duration);
    assert!(info.audio);
}

#[test]
fn poster() {
    let Some(ff) = ff() else { return };
    let dir = tempfile::tempdir().unwrap();
    let v = clip(dir.path(), "a.mp4", RED, false);
    let png = dir.path().join("cache/a.png");
    ff.poster(&v, &png).unwrap();
    let info = ff.probe(&png).unwrap();
    assert_eq!((info.width, info.height), (320, 180));
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
                sounds: vec![],
            },
            Clip {
                path: b.clone(),
                label: "b".into(),
                colormatch: matched,
                sounds: vec![],
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

/// Mean volume in dB of `len` seconds from `from`.
fn loudness(file: &Path, from: f64, len: f64) -> f64 {
    let ffmpeg = std::env::var("SHOTLINE_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let out = Command::new(ffmpeg)
        .args([
            "-v",
            "info",
            "-nostdin",
            "-ss",
            &from.to_string(),
            "-t",
            &len.to_string(),
            "-i",
        ])
        .arg(file)
        .args(["-af", "volumedetect", "-f", "null", "-"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr);
    text.lines()
        .find_map(|l| l.split("mean_volume:").nth(1))
        .and_then(|v| v.trim().trim_end_matches("dB").trim().parse().ok())
        .unwrap_or(-91.0)
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
        sounds: vec![],
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

    // A sound over the last clip is heard from where that clip starts.
    let mut over = silent.clone();
    over[1].sounds = vec![Track {
        path: music.path.clone(),
        at: 0.25,
        volume: 1.0,
    }];
    let heard = dir.path().join("heard.mp4");
    let r = render(
        &ff,
        &over,
        &[],
        &Options::default(),
        &dir.path().join("w4"),
        &heard,
        |_| {},
    )
    .unwrap();
    assert!(close(r.duration, 2.0), "{}", r.duration);
    assert!(ff.probe(&heard).unwrap().audio);
    let quiet_until = loudness(&heard, 0.0, 1.2);
    let loud_after = loudness(&heard, 1.3, 0.6);
    assert!(
        quiet_until < -60.0 && loud_after > -40.0,
        "{quiet_until} {loud_after}"
    );
}

/// Drains a pipe as if playback were instantly far ahead.
fn all_frames(p: &mut shotline_media::play::FramePipe, from: f64) -> Vec<f64> {
    let mut pts = vec![];
    let start = std::time::Instant::now();
    while !p.ended() && start.elapsed().as_secs() < 20 {
        // One at a time: ask for each frame exactly when it's due.
        match p.at(pts.last().map_or(from, |l| l + 1.5 / 24.0)) {
            Some(f) => {
                assert_eq!(f.rgba.len(), (p.width * p.height * 4) as usize);
                pts.push(f.pts);
            }
            None => std::thread::sleep(std::time::Duration::from_millis(2)),
        }
    }
    pts
}

#[test]
fn frame_and_sound_pipes() {
    let Some(ff) = ff() else { return };
    let dir = tempfile::tempdir().unwrap();
    let c = clip(dir.path(), "a.mp4", RED, true);

    let mut p = ff.frame_pipe(&c, 0.0, 80, 46, 24.0).unwrap();
    let pts = all_frames(&mut p, 0.0);
    assert_eq!(pts.len(), 24);
    assert!(close(pts[23], 23.0 / 24.0));

    // Seeking is a new pipe from the new time.
    let mut p = ff.frame_pipe(&c, 0.5, 80, 46, 24.0).unwrap();
    let pts = all_frames(&mut p, 0.5);
    assert_eq!(pts.len(), 12);
    assert!(close(pts[0], 0.5));

    let (_pipe, rx) = ff.pcm_pipe(&c, 0.0, 48000, 2).unwrap();
    let samples: usize = rx.iter().map(|c| c.len()).sum();
    let secs = samples as f64 / 2.0 / 48000.0;
    assert!((0.9..1.1).contains(&secs), "{secs}s of sound");
}

#[test]
fn a_strip_to_scrub_through() {
    let Some(ff) = ff() else { return };
    let dir = tempfile::tempdir().unwrap();
    let c = clip(dir.path(), "a.mp4", RED, false);
    let frames = ff.strip(&c, 40, 22, 8.0).unwrap();
    assert_eq!(frames.len(), 8);
    assert!(frames.iter().all(|f| f.rgba.len() == 40 * 22 * 4));
    assert!(close(frames[4].pts, 0.5));
}

fn line(dir: &Path, name: &str, secs: f64) -> PathBuf {
    let path = dir.join(name);
    let ffmpeg = std::env::var("SHOTLINE_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    assert!(
        Command::new(ffmpeg)
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=330:duration={secs}"))
            .args(["-c:a", "libmp3lame"])
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    path
}

#[test]
fn lines_join_into_one_clip() {
    let Some(ff) = ff() else { return };
    let dir = tempfile::tempdir().unwrap();
    let clips = vec![
        shotline_media::lines::Clip {
            path: line(dir.path(), "a.mp3", 1.0),
            at: None,
        },
        shotline_media::lines::Clip {
            path: line(dir.path(), "b.mp3", 2.0),
            at: None,
        },
    ];
    let out = dir.path().join("lines.wav");
    let starts = ff.join_lines(&clips, &out).unwrap();
    assert_eq!(starts.len(), 2);
    // mp3 pads a little, so the second line starts just past 1.3.
    assert!((starts[1] - 1.3).abs() < 0.1, "{starts:?}");
    let len = ff.audio_duration(&out).unwrap();
    assert!((len - (starts[1] + 2.0)).abs() < 0.1, "{len}");

    let timed = vec![shotline_media::lines::Clip {
        path: clips[0].path.clone(),
        at: Some(2.5),
    }];
    ff.join_lines(&timed, &out).unwrap();
    let len = ff.audio_duration(&out).unwrap();
    assert!(
        (len - 3.5).abs() < 0.1,
        "silence before a timed line: {len}"
    );
}

#[test]
fn pieces_of_takes_made_into_one() {
    use shotline_media::splice::{Join, Piece, splice};
    let Some(ff) = ff() else { return };
    let dir = tempfile::tempdir().unwrap();
    let a = clip(dir.path(), "a.mp4", RED, false);
    let b = clip(dir.path(), "b.mp4", BLUE, true);
    let piece = |path: &Path, from, to, join| Piece {
        path: path.to_path_buf(),
        from,
        to,
        join,
    };
    let opts = shotline_media::splice::Options::default();

    // Trimmed.
    let out = dir.path().join("trim.mp4");
    let r = splice(
        &ff,
        &[piece(&b, 0.25, 0.75, Join::Cut)],
        &opts,
        dir.path(),
        &out,
    )
    .unwrap();
    assert!(close(r.duration, 0.5), "{}", r.duration);
    let i = ff.probe(&out).unwrap();
    assert!(i.audio && close(i.duration, 0.5), "{i:?}");

    // 14 + 19 frames, 7 of them faded together, 6 made up, then 10.
    let out = dir.path().join("three.mp4");
    let r = splice(
        &ff,
        &[
            piece(&a, 0.0, 0.6, Join::Cut),
            piece(&b, 0.2, 1.0, Join::Fade),
            piece(&a, 0.1, 0.5, Join::Smooth),
        ],
        &opts,
        &dir.path().join("work"),
        &out,
    )
    .unwrap();
    assert!(close(r.duration, 42.0 / 24.0), "{}", r.duration);
    let i = ff.probe(&out).unwrap();
    assert_eq!((i.width, i.height), (160, 90));
    assert!(i.audio && close(i.duration, 42.0 / 24.0), "{i:?}");
    let frames = ff.frames(&out, 160, 90, "24", None).unwrap().count();
    assert_eq!(frames, 42);

    let short = splice(
        &ff,
        &[
            piece(&a, 0.0, 0.2, Join::Cut),
            piece(&b, 0.0, 0.8, Join::Fade),
        ],
        &opts,
        dir.path(),
        &dir.path().join("x.mp4"),
    );
    assert!(
        short
            .unwrap_err()
            .to_string()
            .contains("shorter than its fade")
    );
}

#[test]
fn a_sequence_made_into_a_preview() {
    use shotline_media::sequence::{Piece, Sequence};
    let Some(ff) = ff() else { return };
    let dir = tempfile::tempdir().unwrap();
    let red = clip(dir.path(), "red.mp4", RED, false);
    let blue = clip(dir.path(), "blue.mp4", BLUE, false);
    let music = tone(dir.path());
    let piece = |path: &Path, start: f64, length: f64, from: f64| Piece {
        path: path.to_path_buf(),
        label: path.display().to_string(),
        start,
        length,
        from,
        speed: 1.0,
        ..Piece::default()
    };
    let seq = Sequence {
        width: 320,
        height: 180,
        fps: 24.0,
        length: 2.0,
        // Red, a gap, red again; blue above the first half second of the red.
        video: vec![
            piece(&red, 0.0, 1.0, 0.0),
            piece(&red, 1.5, 0.5, 0.25),
            piece(&blue, 0.5, 0.5, 0.0),
            piece(&dir.path().join("gone.mp4"), 0.0, 1.0, 0.0),
        ],
        audio: vec![piece(&music, 0.5, 1.0, 0.0), piece(&red, 0.0, 1.0, 0.0)],
    };
    let out = dir.path().join("cache/seq.mp4");
    let skipped = ff.sequence_preview(&seq, &out).unwrap();
    assert_eq!(skipped.len(), 2, "{skipped:?}");
    let info = ff.probe(&out).unwrap();
    assert_eq!((info.width, info.height), (320, 180));
    assert!(info.audio);
    assert!(close(info.duration, 2.0), "{}", info.duration);
    let frames: Vec<Vec<u8>> = ff.frames(&out, 320, 180, "24/1", None).unwrap().collect();
    let at = |t: f64| mean(&frames[(t * 24.0) as usize]);
    let (r, b, black) = (at(0.25), at(0.75), at(1.25));
    assert!(r[0] > r[2], "red first {r:?}");
    assert!(b[2] > b[0], "blue over it {b:?}");
    assert!(black.iter().all(|c| *c < 20.0), "a gap is black {black:?}");
    let again = at(1.75);
    assert!(again[0] > again[2], "red again {again:?}");
}

#[test]
fn a_preview_has_luts_and_dissolves() {
    use shotline_media::sequence::{Piece, Sequence};
    let Some(ff) = ff() else { return };
    let dir = tempfile::tempdir().unwrap();
    let red = clip(dir.path(), "red.mp4", RED, false);
    let blue = clip(dir.path(), "blue.mp4", BLUE, false);
    // Everything to green, somewhere a filter graph has to be careful with.
    let cube = dir.path().join("my luts, v1 [x]/green's.cube");
    std::fs::create_dir_all(cube.parent().unwrap()).unwrap();
    std::fs::write(&cube, format!("LUT_3D_SIZE 2\n{}", "0 1 0\n".repeat(8))).unwrap();
    let seq = Sequence {
        width: 320,
        height: 180,
        fps: 24.0,
        length: 1.5,
        // Green from the LUT, then blue dissolving in over its last half second.
        video: vec![
            Piece {
                path: red.clone(),
                label: "red".into(),
                length: 1.0,
                speed: 1.0,
                luts: vec![cube.clone(), dir.path().join("gone.cube")],
                ..Piece::default()
            },
            Piece {
                path: blue.clone(),
                label: "blue".into(),
                start: 0.5,
                length: 1.0,
                speed: 1.0,
                fade_in: Some((0.5, 0.5)),
                ..Piece::default()
            },
        ],
        audio: vec![],
    };
    let out = dir.path().join("seq.mp4");
    let skipped = ff.sequence_preview(&seq, &out).unwrap();
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    let frames: Vec<Vec<u8>> = ff.frames(&out, 320, 180, "24/1", None).unwrap().collect();
    let at = |t: f64| mean(&frames[(t * 24.0) as usize]);
    let (g, mix, b) = (at(0.2), at(0.75), at(1.25));
    assert!(g[1] > 0.6 && g[0] < 0.25 && g[2] < 0.25, "green {g:?}");
    assert!(
        mix[1] > 0.45 && mix[1] < 0.85 && mix[2] > 0.15,
        "half way {mix:?}"
    );
    assert!(b[2] > b[1] && b[1] < 0.4, "blue {b:?}");
}

#[test]
fn a_match_lut_brings_the_next_clip_closer() {
    let Some(ff) = ff() else { return };
    let dir = tempfile::tempdir().unwrap();
    let a = clip(dir.path(), "a.mp4", RED, false);
    let b = clip(dir.path(), "b.mp4", BLUE, false);
    let cube = dir.path().join("luts/b.cube");
    ff.match_lut((&a, 1.0), (&b, 0.0), 5, 1.0, &cube).unwrap();
    let text = std::fs::read_to_string(&cube).unwrap();
    assert!(text.contains("LUT_3D_SIZE 33"));
    assert_eq!(text.lines().count(), 2 + 33 * 33 * 33);
    let graded = dir.path().join("b-graded.mp4");
    let ffmpeg = std::env::var("SHOTLINE_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    assert!(
        Command::new(ffmpeg)
            .args(["-v", "error", "-y", "-i"])
            .arg(&b)
            .arg("-vf")
            // Relative, since a drive letter's colon would end the option.
            .arg("lut3d=file=luts/b.cube")
            .current_dir(dir.path())
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&graded)
            .status()
            .unwrap()
            .success()
    );
    let first = |f: &Path| {
        let info = ff.probe(f).unwrap();
        ff.frames(f, info.width, info.height, &info.fps, Some(1))
            .unwrap()
            .next()
            .unwrap()
    };
    let last_a = {
        let info = ff.probe(&a).unwrap();
        ff.frames(&a, info.width, info.height, &info.fps, None)
            .unwrap()
            .last()
            .unwrap()
    };
    let before = distance(mean(&last_a), mean(&first(&b)));
    let after = distance(mean(&last_a), mean(&first(&graded)));
    assert!(after < before / 2.0, "{before} -> {after}");
}

#[test]
fn the_frame_showing_at_a_time() {
    let Some(ff) = ff() else { return };
    let dir = tempfile::tempdir().unwrap();
    // Grey rising 8 a frame, so a frame says which it is.
    let video = dir.path().join("ramp.mp4");
    let ffmpeg = std::env::var("SHOTLINE_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    assert!(
        Command::new(&ffmpeg)
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("color=c=black:size=64x64:rate=24:duration=1")
            .args(["-vf", "geq=lum='16+8*N':cb=128:cr=128"])
            .args(["-c:v", "libx264", "-qp", "0", "-pix_fmt", "yuv444p"])
            .arg(&video)
            .status()
            .unwrap()
            .success()
    );
    let png = dir.path().join("f.png");
    let number = |at: f64| {
        ff.frame_at(&video, at, &png).unwrap();
        let ffmpeg = std::env::var("SHOTLINE_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
        let out = Command::new(ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(&png)
            .args(["-f", "rawvideo", "-pix_fmt", "gray", "-"])
            .output()
            .unwrap();
        (out.stdout[0] as f64 / (8.0 * 255.0 / 219.0)).round() as i64
    };
    assert_eq!(number(0.0), 0);
    assert_eq!(number(5.0 / 24.0), 5);
    assert_eq!(number(5.5 / 24.0), 5);
    assert_eq!(number(23.0 / 24.0), 23);
    // Past the end: the last frame.
    assert_eq!(number(3.0), 23);
}

#[test]
fn a_sound_has_a_length() {
    let Some(ff) = ff() else { return };
    let dir = tempfile::tempdir().unwrap();
    let got = ff.length(&line(dir.path(), "door.mp3", 2.5)).unwrap();
    assert!((got - 2.5).abs() < 0.1, "{got}");
    assert!(ff.length(&dir.path().join("none.mp3")).is_err());
}
