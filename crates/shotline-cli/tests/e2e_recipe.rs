use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use predicates::prelude::*;

fn shotline(tmp: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("shotline").unwrap();
    cmd.current_dir(tmp.path().join("film"))
        .env("SHOTLINE_HOME", tmp.path().join(".home"))
        .env("SHOTLINE_POLL_MS", "1")
        .env("PATH", "")
        .timeout(std::time::Duration::from_secs(60));
    cmd
}

/// One talking shot through the fake plugin: tts, video with the audio, lip sync.
fn film(tmp: &TempDir) {
    let exe = assert_cmd::cargo::cargo_bin("shotline");
    tmp.child(".home/plugins/fake/shotline-plugin.toml")
        .write_str(&format!(
            "name = \"fake\"\nexec = {:?}\nargs = [\"__fake-plugin\"]\nprotocol = 1\n",
            exe.display().to_string()
        ))
        .unwrap();
    std::fs::create_dir_all(tmp.path().join("film")).unwrap();
    let run = |args: &[&str]| {
        shotline(tmp).args(args).assert().success();
    };
    run(&["init", "."]);
    tmp.child("film/shotline.toml")
        .write_str(
            r#"name = "Film"
plugin = "fake"
model = "fake/video"
resolution = "480P"
aspect = "9:16"
duration = 4

[[recipe.talking]]
op = "audio.tts"

[[recipe.talking]]
op = "video.generate"
audio = true

[[recipe.talking]]
op = "video.lipsync"
plugin = "fake"
model = "fake/lipsync"
"#,
        )
        .unwrap();
    tmp.child("p.png").write_binary(b"png").unwrap();
    run(&["element", "add", "cast", "Paka"]);
    run(&["element", "anchor", "paka", "--file", "../p.png"]);
    run(&["chapter", "add", "One"]);
    run(&[
        "shot",
        "add",
        "1",
        "--prompt",
        "@paka speaks",
        "--recipe",
        "talking",
    ]);
}

#[test]
fn voices_and_lines() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    shotline(&tmp)
        .args(["shot", "line", "01-010", "paka", "เราต้องไปต่อก่อนมืด"])
        .assert()
        .success()
        .stdout("01-010 has 1 line\n");
    shotline(&tmp)
        .args(["prompt", "01-010"])
        .assert()
        .stderr(predicate::str::contains("@paka has a line but no voice"));
    shotline(&tmp)
        .args(["quote", "01-010"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "@paka has no voice; give one with `shotline element voice paka`",
        ));

    shotline(&tmp)
        .args([
            "element",
            "voice",
            "paka",
            "--plugin",
            "fake",
            "--model",
            "fake/tts",
            "--voice-id",
            "v1",
            "--set",
            "stability=0.5",
        ])
        .assert()
        .success()
        .stdout("@paka speaks with fake\n");
    tmp.child("film/elements/cast/paka.toml").assert(
        predicate::str::contains("[voice]\nplugin = \"fake\"")
            .and(predicate::str::contains("stability = 0.5")),
    );
    shotline(&tmp)
        .args(["element", "add", "set", "Forest"])
        .assert()
        .success();
    shotline(&tmp)
        .args(["shot", "line", "01-010", "forest", "hi"])
        .assert()
        .failure()
        .stderr("error: @forest is a set, only cast speak\n");
}

#[test]
fn a_talking_take() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    shotline(&tmp)
        .args([
            "element",
            "voice",
            "paka",
            "--plugin",
            "fake",
            "--model",
            "fake/tts",
            "--voice-id",
            "v1",
        ])
        .assert()
        .success();
    shotline(&tmp)
        .args(["shot", "line", "01-010", "paka", "we go on before dark"])
        .assert()
        .success();

    // 20 chars * 0.001 + 4s * 0.1 + 4s * 0.05
    shotline(&tmp)
        .args(["quote", "01-010"])
        .assert()
        .success()
        .stdout(
            predicate::str::starts_with("01-010  4s  0.62 USD\n")
                .and(predicate::str::contains(
                    "0  line 1 by @paka    fake fake/tts  0.02 USD",
                ))
                .and(predicate::str::contains(
                    "2  video.lipsync      fake fake/lipsync  0.2 USD",
                )),
        );

    shotline(&tmp)
        .args(["take", "01-010", "--yes"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("reserved 0.62 USD")
                .and(predicate::str::contains(
                    "step 1/3: line 1 by @paka · fake fake/tts",
                ))
                .and(predicate::str::contains(
                    "step 3/3: video.lipsync · fake fake/lipsync",
                ))
                .and(
                    predicate::str::is_match(r"01-010#1 done · 0.62 USD · jobs/\w{26}/out.mp4")
                        .unwrap(),
                ),
        );
    shotline(&tmp)
        .args(["takes", "01-010"])
        .assert()
        .stdout(predicate::str::contains(
            "01-010#1  done      3 steps  0.62 USD",
        ));

    // Redo only the lip sync, reusing the speech and the video.
    shotline(&tmp)
        .args(["take", "01-010", "--from-step", "2", "--yes"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("reused from")
                .and(predicate::str::contains("reserved 0.2 USD"))
                .and(predicate::str::contains(
                    "step 1/3: line 1 by @paka · fake fake/tts (reused)",
                ))
                .and(predicate::str::contains("01-010#2 done · 0.2 USD")),
        );
    shotline(&tmp).args(["cost", "verify"]).assert().success();
    shotline(&tmp)
        .args(["cost", "--by", "op"])
        .assert()
        .stdout(predicate::str::is_match(r"video\.lipsync\s+0\.4\s+2").unwrap());
}

#[test]
fn run_one_operation() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    tmp.child("film/clip.mp4").write_binary(b"mp4").unwrap();
    tmp.child("film/line.wav").write_binary(b"wav").unwrap();
    shotline(&tmp)
        .args([
            "run",
            "video.lipsync",
            "--plugin",
            "fake",
            "--model",
            "fake/lipsync",
            "--duration",
            "6",
        ])
        .args([
            "--input",
            "video=clip.mp4",
            "--input",
            "audio=line.wav",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::starts_with("video.lipsync · fake fake/lipsync · 0.3 USD\n").and(
                predicate::str::is_match(r"jobs/\w{26}/out.mp4\ndone · 0.3 USD · job \w{26}\n")
                    .unwrap(),
            ),
        );
    shotline(&tmp)
        .args([
            "run",
            "video.lipsync",
            "--plugin",
            "fake",
            "--model",
            "fake/lipsync",
            "--input",
            "video=nope.mp4",
        ])
        .assert()
        .failure()
        .stderr("error: nope.mp4 doesn't exist\n");
    shotline(&tmp)
        .args(["run", "video.dance", "--plugin", "fake", "--model", "x"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("custom ones start with x."));
    shotline(&tmp)
        .arg("cost")
        .assert()
        .stdout(predicate::str::contains(
            "spent    0.3 USD  (in the cut 0, thrown away 0, other 0.3)",
        ));
}
