use std::path::Path;

use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use predicates::prelude::*;
use shotline_core::protocol::Op;
use shotline_core::{Job, JobStatus, Project, ShotRef};

/// A user folder with the fake plugin installed: the shotline binary itself, run as
/// `shotline __fake-plugin`.
fn home_with_fake(tmp: &TempDir) {
    let exe = assert_cmd::cargo::cargo_bin("shotline");
    let manifest = format!(
        "name = \"fake\"\nexec = {:?}\nargs = [\"__fake-plugin\"]\nprotocol = 1\n",
        exe.display().to_string()
    );
    tmp.child(".home/plugins/fake/shotline-plugin.toml")
        .write_str(&manifest)
        .unwrap();
}

fn shotline(tmp: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("shotline").unwrap();
    cmd.current_dir(tmp.path())
        .env("SHOTLINE_HOME", tmp.path().join(".home"))
        .env("PATH", "")
        .timeout(std::time::Duration::from_secs(60))
        .env_remove("SHOTLINE_FAKE_KEY");
    cmd
}

#[test]
fn plugin_ls_shows_models_and_env() {
    let tmp = TempDir::new().unwrap();
    shotline(&tmp)
        .args(["plugin", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("no plugins in"));

    home_with_fake(&tmp);
    shotline(&tmp)
        .args(["plugin", "ls"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("video.generate     fake/video")
                .and(predicate::str::contains("audio.tts          fake/tts"))
                .and(predicate::str::contains("env SHOTLINE_FAKE_KEY (missing)")),
        );
    shotline(&tmp)
        .env("SHOTLINE_FAKE_KEY", "k")
        .args(["plugin", "ls"])
        .assert()
        .stdout(predicate::str::contains("env SHOTLINE_FAKE_KEY (set)"));

    // The key can also come from the user .env file.
    tmp.child(".home/.env")
        .write_str("SHOTLINE_FAKE_KEY=from-file\n")
        .unwrap();
    shotline(&tmp)
        .args(["plugin", "ls"])
        .assert()
        .stdout(predicate::str::contains("(set)"));
}

#[test]
fn plugin_doctor() {
    let tmp = TempDir::new().unwrap();
    home_with_fake(&tmp);
    shotline(&tmp)
        .args(["plugin", "doctor", "fake"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("missing env SHOTLINE_FAKE_KEY"))
        .stderr("error: fake has problems\n");
    shotline(&tmp)
        .env("SHOTLINE_FAKE_KEY", "k")
        .args(["plugin", "doctor", "fake"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("ok    quote fake/video")
                .and(predicate::str::contains("FAIL").not()),
        );
    shotline(&tmp)
        .args(["plugin", "doctor", "nope"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no plugin called nope"));
}

#[test]
fn broken_plugin_is_reported_not_fatal() {
    let tmp = TempDir::new().unwrap();
    tmp.child(".home/plugins/bad/shotline-plugin.toml")
        .write_str("name = \"bad\"\nexec = \"/no/such/thing\"\nprotocol = 1\n")
        .unwrap();
    tmp.child(".home/plugins/old/shotline-plugin.toml")
        .write_str("name = \"old\"\nexec = \"x\"\nprotocol = 7\n")
        .unwrap();
    shotline(&tmp)
        .args(["plugin", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "bad  broken: plugin bad: can't start",
        ))
        .stderr(predicate::str::contains("old: speaks protocol 7"));
}

fn project_with_shots(tmp: &TempDir) -> Project {
    shotline(tmp).args(["init", "film"]).assert().success();
    let film = tmp.path().join("film");
    let run = |args: &[&str]| {
        shotline(tmp)
            .current_dir(&film)
            .args(args)
            .assert()
            .success();
    };
    run(&["element", "add", "cast", "Paka"]);
    tmp.child("p.png").write_binary(b"png").unwrap();
    run(&["element", "anchor", "paka", "--file", "../p.png"]);
    run(&["chapter", "add", "One"]);
    run(&[
        "shot",
        "add",
        "1",
        "--prompt",
        "@paka walks",
        "--plugin",
        "fake",
        "--model",
        "fake/video",
    ]);
    run(&[
        "shot",
        "add",
        "1",
        "--prompt",
        "@paka turns",
        "--transition",
        "continue",
    ]);
    Project::open(&film).unwrap()
}

fn add_take(p: &Project, r: ShotRef, status: JobStatus) {
    let mut job = Job::new(Op::VideoGenerate, "fake", "fake/video");
    job.shot = Some(r);
    job.take = Some(p.new_take_id());
    let mut job = p.create_job(job).unwrap();
    job.status = status;
    job.cost = Some(shotline_core::Cost {
        amount: 0.4,
        unit: "USD".into(),
    });
    p.save_job(&job).unwrap();
}

#[test]
fn prompt_uses_plugin_models() {
    let tmp = TempDir::new().unwrap();
    home_with_fake(&tmp);
    project_with_shots(&tmp);
    shotline(&tmp)
        .current_dir(tmp.path().join("film"))
        .args(["prompt", "01-010"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("fake fake/video")
                .and(predicate::str::contains("Ref 1 walks")),
        )
        // project default is 768P, the fake model only does 480P/720P
        .stderr(predicate::str::contains("doesn't do 768P"));
}

#[test]
fn takes_and_select() {
    let tmp = TempDir::new().unwrap();
    let p = project_with_shots(&tmp);
    let r: ShotRef = "01-010".parse().unwrap();
    add_take(&p, r, JobStatus::Done);
    add_take(&p, r, JobStatus::Failed);
    let film = tmp.path().join("film");

    shotline(&tmp)
        .current_dir(&film)
        .args(["select", "01-010#2"])
        .assert()
        .failure()
        .stderr("error: 01-010#2 is failed, only done takes can be selected\n");
    shotline(&tmp)
        .current_dir(&film)
        .args(["select", "01-010", "1"])
        .assert()
        .success()
        .stdout("01-010#1 selected\n");
    shotline(&tmp)
        .current_dir(&film)
        .args(["takes", "01-010"])
        .assert()
        .stdout(
            predicate::str::is_match(
                r"(?m)^\* 01-010#1  done      1 step  0.4 USD  \w{26}\n  01-010#2  failed",
            )
            .unwrap(),
        );

    // The continue shot now starts from the selected take's last frame.
    let take = p.selected_take(r).unwrap().unwrap();
    shotline(&tmp)
        .current_dir(&film)
        .args(["prompt", "01-020"])
        .assert()
        .stdout(predicate::str::contains(format!(
            "start frame: jobs/{}/last.png (from 01-010)",
            take.jobs[0].id
        )));
}

#[test]
fn gitignore_keeps_job_files_but_not_media() {
    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let tmp = TempDir::new().unwrap();
    shotline(&tmp).args(["init", "film"]).assert().success();
    let film = tmp.path().join("film");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(&film)
            .args(args)
            .output()
            .unwrap()
    };
    git(&["init", "-q"]);
    let job = film.join("jobs/01JBX9Q3T7K2M8V5N4R6P1W0YZ");
    std::fs::create_dir_all(&job).unwrap();
    for f in ["job.toml", "out.mp4", "last.png"] {
        std::fs::write(job.join(f), "x").unwrap();
    }
    let ignored = |p: &Path| {
        git(&["check-ignore", "-q", p.to_str().unwrap()])
            .status
            .success()
    };
    assert!(ignored(&job.join("out.mp4")));
    assert!(ignored(&job.join("last.png")));
    assert!(!ignored(&job.join("job.toml")));
    assert!(!ignored(&film.join("shotline.toml")));
}
