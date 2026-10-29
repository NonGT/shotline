use std::path::PathBuf;

use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use predicates::prelude::*;
use shotline_core::ledger::Context;
use shotline_core::protocol::{Op, Quote};
use shotline_core::{Job, JobStatus, Project, Shot, ShotRef, UserConfig};

fn shotline(tmp: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("shotline").unwrap();
    cmd.current_dir(tmp.path().join("film"))
        .env("SHOTLINE_HOME", tmp.path().join(".home"))
        .env("PATH", "");
    cmd
}

fn usd(amount: f64) -> Quote {
    Quote {
        amount,
        unit: "USD".into(),
        units: Some(8.0),
        unit_name: Some("s".into()),
    }
}

/// A film with two shots: 01-010 has a selected $0.40 take and a thrown away $0.38 one,
/// 01-020 has a running job holding $0.50.
fn film(tmp: &TempDir) -> (Project, Vec<String>) {
    let p = Project::init(&tmp.path().join("film"), Some("Film")).unwrap();
    p.add_chapter("One").unwrap();
    for _ in 0..2 {
        p.add_shot(
            "1",
            None,
            Shot {
                prompt: "x".into(),
                duration: Some(8.0),
                ..Shot::default()
            },
        )
        .unwrap();
    }
    let home: PathBuf = tmp.path().join(".home");
    let books = p.books(&home, &UserConfig::default(), &[]).unwrap();
    let now = p.clock.now();
    let r010: ShotRef = "01-010".parse().unwrap();
    let mut jobs = vec![];
    for (shot, amount, finish) in [
        (r010, 0.4, true),
        (r010, 0.38, true),
        ("01-020".parse().unwrap(), 0.5, false),
    ] {
        let mut j = Job::new(Op::VideoGenerate, "fake", "fake/video");
        j.shot = Some(shot);
        j.take = Some(p.new_take_id());
        let mut j = p.create_job(j).unwrap();
        let ctx = Context {
            job: j.id.to_string(),
            take: j.take.map(|t| t.to_string()),
            shot: Some(shot.to_string()),
            chapter: Some(shot.chapter),
            op: "video.generate".into(),
            plugin: "fake".into(),
            model: "fake/video".into(),
        };
        books.reserve(now, &ctx, &usd(amount)).unwrap();
        if finish {
            books
                .charge(
                    now,
                    &ctx.job,
                    Some(&format!("fake-{}", jobs.len() + 1)),
                    None,
                )
                .unwrap();
            j.status = JobStatus::Done;
            p.save_job(&j).unwrap();
        }
        jobs.push(j.id.to_string());
    }
    p.select_take(r010, "1").unwrap();
    (p, jobs)
}

#[test]
fn empty_film() {
    let tmp = TempDir::new().unwrap();
    Project::init(&tmp.path().join("film"), None).unwrap();
    shotline(&tmp)
        .arg("cost")
        .assert()
        .success()
        .stdout("nothing spent yet\n");
    shotline(&tmp).args(["cost", "verify"]).assert().success();
}

#[test]
fn summary_and_groups() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    shotline(&tmp).arg("cost").assert().success().stdout(
        predicate::str::contains("spent    0.78 USD  (in the cut 0.4, thrown away 0.38, other 0)")
            .and(predicate::str::contains(
                "held     0.5 USD for jobs still running",
            ))
            .and(predicate::str::contains(
                "cut      8s, 0.05 USD a second (0.0975 counting everything)",
            ))
            .and(predicate::str::is_match(r"01-010\s+0.78\s+2  0.78 USD").unwrap()),
    );
    shotline(&tmp)
        .args(["cost", "--by", "plugin", "--csv"])
        .assert()
        .success()
        .stdout("key,USD,jobs,USD\nfake,0.78,2,0.78\n");
    shotline(&tmp)
        .args(["cost", "--by", "colour"])
        .assert()
        .failure();
}

#[test]
fn verify_catches_edits() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    shotline(&tmp)
        .args(["cost", "verify"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("film: 5 lines, ok")
                .and(predicate::str::contains("all films: 5 lines, ok")),
        );

    let path = tmp.path().join("film/.shotline/ledger.jsonl");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        text.replacen("\"amount\":0.38", "\"amount\":0.03", 1),
    )
    .unwrap();
    shotline(&tmp)
        .args(["cost", "verify"])
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "line 4: doesn't follow the line before it",
        ))
        .stderr("error: the ledger has been changed by hand\n");
}

#[test]
fn adjust_and_refund() {
    let tmp = TempDir::new().unwrap();
    let (_, jobs) = film(&tmp);
    tmp.child("film/shotline.toml")
        .write_str("name = \"Film\"\n[rates]\nfake-credit = \"0.01 USD\"\n")
        .unwrap();
    shotline(&tmp)
        .args(["cost", "adjust", "-50", "fake-credit", "--note", "promo"])
        .assert()
        .success()
        .stdout("adjusted -0.5 USD\n");
    shotline(&tmp)
        .args(["cost", "adjust", "1", "USD", "--note", ""])
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs a note"));
    shotline(&tmp)
        .args([
            "cost",
            "refund",
            &jobs[1][..20],
            "0.38",
            "USD",
            "--note",
            "policy hit",
        ])
        .assert()
        .success()
        .stdout("refunded 0.38 USD\n");
    shotline(&tmp)
        .arg("cost")
        .assert()
        .stdout(predicate::str::contains(
            "spent    -0.1 USD  (in the cut 0.4, thrown away 0, other -0.5)",
        ));
    shotline(&tmp).args(["cost", "verify"]).assert().success();
}

#[test]
fn reconcile_against_the_fake_plugin() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    let exe = assert_cmd::cargo::cargo_bin("shotline");
    tmp.child(".home/plugins/fake/shotline-plugin.toml")
        .write_str(&format!(
            "name = \"fake\"\nexec = {:?}\nargs = [\"__fake-plugin\"]\nprotocol = 1\n",
            exe.display().to_string()
        ))
        .unwrap();

    let usage = |charges: &str| {
        tmp.child("film/shotline-fake-usage.json")
            .write_str(&format!("{{\"charges\": [{charges}]}}"))
            .unwrap();
    };
    usage(
        r#"{"job": "fake-1", "time": "2026-10-30T10:00:00Z", "amount": 0.4, "unit": "USD"},
           {"job": "fake-2", "time": "2026-10-30T10:00:00Z", "amount": 0.38, "unit": "USD"}"#,
    );
    shotline(&tmp)
        .args(["cost", "reconcile"])
        .assert()
        .success()
        .stdout("fake: matches (2 charges)\n");

    usage(
        r#"{"job": "fake-1", "time": "2026-10-30T10:00:00Z", "amount": 0.45, "unit": "USD"},
           {"job": "fake-7", "time": "2026-10-30T10:00:00Z", "amount": 1, "unit": "USD"}"#,
    );
    shotline(&tmp)
        .args(["cost", "reconcile"])
        .assert()
        .failure()
        .stdout(
            predicate::str::contains("fake-1: ledger 0.4 USD but provider 0.45 USD")
                .and(predicate::str::contains(
                    "fake-7: provider charged 1 USD, not in the ledger",
                ))
                .and(predicate::str::contains(
                    "fake-2: ledger has 0.38 USD, provider doesn't",
                )),
        );
}
