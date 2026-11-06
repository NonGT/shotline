//! Needs ffmpeg on PATH; passes without doing anything when it isn't there.

use std::path::{Path, PathBuf};
use std::process::Command as Std;

use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use predicates::prelude::*;

struct Tools {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

fn tools() -> Option<Tools> {
    let find = |name: &str| {
        let exe = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|d| d.join(&exe))
            .find(|p| p.is_file())
    };
    match (find("ffmpeg"), find("ffprobe")) {
        (Some(ffmpeg), Some(ffprobe)) => Some(Tools { ffmpeg, ffprobe }),
        _ => {
            eprintln!("skipped: no ffmpeg");
            None
        }
    }
}

fn shotline(tmp: &TempDir, t: &Tools) -> Command {
    let mut cmd = Command::cargo_bin("shotline").unwrap();
    cmd.current_dir(tmp.path().join("film"))
        .env("SHOTLINE_HOME", tmp.path().join(".home"))
        .env("SHOTLINE_POLL_MS", "1")
        .env("SHOTLINE_FFMPEG", &t.ffmpeg)
        .env("SHOTLINE_FFPROBE", &t.ffprobe)
        .env("PATH", "")
        .timeout(std::time::Duration::from_secs(120));
    cmd
}

/// Replaces a take's fake output with a second of tinted test pattern.
fn clip(t: &Tools, path: &Path, mix: &str, tone: bool) {
    let mut c = Std::new(&t.ffmpeg);
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
    assert!(c.args(["-f", "mp4"]).arg(path).status().unwrap().success());
}

/// Takes a shot, selects it and gives it a real clip.
fn shoot(tmp: &TempDir, t: &Tools, shot: &str, mix: &str, tone: bool) {
    let out = shotline(tmp, t)
        .args(["take", shot, "--yes"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let out = String::from_utf8(out).unwrap();
    let file = out
        .split_whitespace()
        .find(|w| w.starts_with("jobs/") && w.ends_with("out.mp4"))
        .unwrap_or_else(|| panic!("no output in {out}"));
    clip(t, &tmp.path().join("film").join(file), mix, tone);
    shotline(tmp, t)
        .args(["select", shot, "1"])
        .assert()
        .success();
}

fn duration(t: &Tools, path: &Path) -> f64 {
    let out = Std::new(&t.ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
fn chain_match_and_render() {
    let Some(t) = tools() else { return };
    let tmp = TempDir::new().unwrap();
    let exe = assert_cmd::cargo::cargo_bin("shotline");
    tmp.child(".home/plugins/fake/shotline-plugin.toml")
        .write_str(&format!(
            "name = \"fake\"\nexec = {:?}\nargs = [\"__fake-plugin\"]\nprotocol = 1\n",
            exe.display().to_string()
        ))
        .unwrap();
    std::fs::create_dir_all(tmp.path().join("film")).unwrap();
    let run = |args: &[&str]| {
        shotline(&tmp, &t).args(args).assert().success();
    };
    run(&["init", "."]);
    tmp.child("film/shotline.toml")
        .write_str(
            "name = \"Film\"\nplugin = \"fake\"\nmodel = \"fake/video\"\nresolution = \"480P\"\naspect = \"9:16\"\nduration = 1\n",
        )
        .unwrap();
    tmp.child("p.png").write_binary(b"png").unwrap();
    run(&["element", "add", "cast", "Paka"]);
    run(&["element", "anchor", "paka", "--file", "../p.png"]);
    run(&["chapter", "add", "One"]);
    run(&["shot", "add", "1", "--prompt", "@paka walks"]);
    run(&["shot", "add", "1", "--prompt", "@paka keeps walking"]);
    run(&["shot", "add", "1", "--prompt", "@paka stops"]);
    tmp.child("film/chapters/01-one/shots/020.toml")
        .write_str(
            &std::fs::read_to_string(tmp.path().join("film/chapters/01-one/shots/020.toml"))
                .unwrap()
                .replace("transition = \"cut\"", "transition = \"continue\""),
        )
        .unwrap();

    shoot(&tmp, &t, "01-010", "rr=1:gg=0.5:bb=0.4", false);
    // The continue shot starts on 01-010's last frame, taken when it was selected; 01-020's is
    // taken once it's selected too.
    shoot(&tmp, &t, "01-020", "rr=0.4:gg=0.6:bb=1", true);
    let last: Vec<_> = glob(&tmp.path().join("film/jobs"), "last.png");
    assert_eq!(last.len(), 2, "{last:?}");

    shotline(&tmp, &t)
        .args(["timeline", "render"])
        .assert()
        .failure()
        .stderr("error: no take selected for 01-030; pick one with `shotline select`\n");
    shoot(&tmp, &t, "01-030", "rr=1:gg=0.5:bb=0.4", false);

    tmp.child("film/music.wav").write_binary(&[]).unwrap();
    assert!(
        Std::new(&t.ffmpeg)
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=220:duration=2"
            ])
            .arg(tmp.path().join("film/music.wav"))
            .status()
            .unwrap()
            .success()
    );
    tmp.child("film/timeline.toml")
        .write_str("[[track]]\nfile = \"music.wav\"\nat = 0.5\nvolume = 0.5\n")
        .unwrap();
    shotline(&tmp, &t)
        .arg("timeline")
        .assert()
        .success()
        .stdout(
            "01-010  01-010#1  1s  cut\n\
         01-020  01-020#1  1s  continue · colour matched\n\
         01-030  01-030#1  1s  cut\n\
         3 shots, 3 ready, 3s\n\
         track  music.wav at 0.5s, volume 0.5\n",
        );

    shotline(&tmp, &t)
        .args(["timeline", "render", "-o", "cut.mp4"])
        .assert()
        .success()
        .stdout(
            "[1/3] 01-010#1\n\
             [2/3] 01-020#1  colour matched\n\
             [3/3] 01-030#1\n\
             encoding\n\
             cut.mp4 · 3s · 160x90\n",
        );
    let d = duration(&t, &tmp.path().join("film/cut.mp4"));
    assert!((d - 3.0).abs() < 0.15, "{d}");
    assert!(!tmp.path().join("film/.shotline/cache/render").exists());

    shotline(&tmp, &t)
        .args(["timeline", "render", "--no-colormatch", "--xfade", "0.25"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("colour matched")
                .not()
                .and(predicate::str::is_match(r"renders/\d{8}-\d{6}\.mp4 · 2\.5s").unwrap()),
        );
    assert!(
        std::fs::read_to_string(tmp.path().join("film/.gitignore"))
            .unwrap()
            .contains("renders/")
    );
}

fn glob(dir: &Path, name: &str) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.ok()?.path().join(name);
            p.is_file().then_some(p)
        })
        .collect()
}

#[test]
fn bring_in_and_splice() {
    let Some(t) = tools() else { return };
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("film")).unwrap();
    let run = |args: &[&str]| -> String {
        let out = shotline(&tmp, &t).args(args).assert().success();
        String::from_utf8(out.get_output().stdout.clone()).unwrap()
    };
    run(&["init", "."]);
    run(&["chapter", "add", "One"]);
    run(&["shot", "add", "1", "--prompt", "a walk"]);
    clip(&t, &tmp.path().join("red.mp4"), "rr=1:gg=0.5:bb=0.4", false);
    clip(&t, &tmp.path().join("blue.mp4"), "rr=0.4:gg=0.6:bb=1", true);
    assert!(run(&["import", "01-010", "../red.mp4"]).contains("01-010#1 brought in"));
    run(&["import", "01-010", "../blue.mp4"]);

    let out = run(&[
        "splice",
        "01-010",
        "1:0-0.5",
        "fade:2:0.2-1",
        "smooth:1:0.5-1",
    ]);
    assert!(out.contains("01-010#3 made from 3 pieces"), "{out}");
    let takes = run(&["takes", "01-010"]);
    assert!(takes.contains("brought in from red.mp4"), "{takes}");
    assert!(takes.contains("from pieces of #1, #2"), "{takes}");
    run(&["select", "01-010", "3"]);
    // Two brought in, then the one made; job folders sort by when they were made.
    let mut made = glob(&tmp.path().join("film/jobs"), "take.mp4");
    made.sort();
    assert_eq!(made.len(), 3);
    // 12 + 19 frames with 7 faded together, 6 made up, then 12.
    let d = duration(&t, &made[2]);
    assert!((d - 42.0 / 24.0).abs() < 0.15, "{d}");

    shotline(&tmp, &t)
        .args(["splice", "01-010", "9:0-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no take 9"));
}
