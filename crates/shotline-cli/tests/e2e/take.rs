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

/// A film using the fake plugin: one cast member with two anchors, two shots of 4s.
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
            "name = \"Film\"\nplugin = \"fake\"\nmodel = \"fake/video\"\nresolution = \"480P\"\n\
             aspect = \"9:16\"\nduration = 4\n[budget]\nper_take = 2\n",
        )
        .unwrap();
    tmp.child("p.png").write_binary(b"png").unwrap();
    run(&["element", "add", "cast", "Paka"]);
    run(&["element", "anchor", "paka", "--file", "../p.png"]);
    run(&["element", "anchor", "paka", "--file", "../p.png"]);
    run(&["chapter", "add", "One"]);
    run(&["shot", "add", "1", "--prompt", "@paka walks"]);
    run(&[
        "shot",
        "add",
        "1",
        "--prompt",
        "@paka runs",
        "--duration",
        "6",
    ]);
}

fn ledger_types(tmp: &TempDir) -> Vec<String> {
    std::fs::read_to_string(tmp.path().join("film/.shotline/ledger.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

#[test]
fn quote_a_shot_and_a_chapter() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    shotline(&tmp)
        .args(["quote", "01-010"])
        .assert()
        .success()
        .stdout("01-010  fake fake/video  4s  0.4 USD\n");
    shotline(&tmp)
        .args(["quote", "1"])
        .assert()
        .success()
        .stdout("01-010  fake fake/video  4s  0.4 USD\n01-020  fake fake/video  6s  0.6 USD\ntotal  1 USD\n");
    assert!(ledger_types(&tmp).is_empty(), "quoting spends nothing");
}

#[test]
fn take_asks_first() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    shotline(&tmp)
        .args(["take", "01-010"])
        .write_stdin("n\n")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "spend 0.4 USD on 01-010? [y/N] nothing spent",
        ));
    assert!(ledger_types(&tmp).is_empty());
    assert!(
        !tmp.path().join("film/jobs").exists()
            || std::fs::read_dir(tmp.path().join("film/jobs"))
                .unwrap()
                .next()
                .is_none()
    );

    shotline(&tmp)
        .args(["take", "01-010"])
        .write_stdin("y\n")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("reserved 0.4 USD")
                .and(predicate::str::contains("sent to fake (fake-1)"))
                .and(
                    predicate::str::is_match(r"01-010#1 done · 0.4 USD · jobs/\w{26}/out.mp4")
                        .unwrap(),
                )
                .and(predicate::str::contains(
                    "pick it with `shotline select 01-010#1`",
                )),
        );
    assert_eq!(ledger_types(&tmp), ["reserve", "charge"]);

    shotline(&tmp)
        .args(["select", "01-010#1"])
        .assert()
        .success();
    shotline(&tmp)
        .arg("cost")
        .assert()
        .stdout(predicate::str::contains(
            "spent    0.4 USD  (in the cut 0.4, thrown away 0, other 0)",
        ));
    shotline(&tmp).args(["cost", "verify"]).assert().success();
}

#[test]
fn take_failures() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    let shot = tmp.child("film/chapters/01-one/shots/010.toml");
    let base = std::fs::read_to_string(shot.path()).unwrap();

    // Charged even though it failed.
    shot.write_str(&format!(
        "{base}\n[model.params]\nfake_fail = \"content_policy\"\nfake_charge_on_fail = true\n"
    ))
    .unwrap();
    shotline(&tmp)
        .args(["take", "01-010", "--yes"])
        .assert()
        .failure()
        .stderr("error: 01-010#1 failed: content policy: failed on request (0.4 USD)\n");

    // Rejected on submit: nothing charged, and the message names the anchor.
    shot.write_str(&format!(
        "{base}\n[model.params]\nfake_reject_run = \"content_policy\"\n"
    ))
    .unwrap();
    shotline(&tmp)
        .args(["take", "01-010", "--yes"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("@paka's anchor elements/cast/paka/01.png")
                .and(predicate::str::contains("(nothing charged)")),
        );

    // Rejected while quoting: never gets near the ledger.
    shot.write_str(&format!(
        "{base}\n[model.params]\nfake_reject = \"validation\"\n"
    ))
    .unwrap();
    shotline(&tmp)
        .args(["take", "01-010", "--yes"])
        .assert()
        .failure()
        .stderr(
            "error: validation: rejected on request ([model.params] fake_reject in shot 01-010)\n",
        );

    assert_eq!(
        ledger_types(&tmp),
        ["reserve", "charge", "reserve", "release"]
    );
    shotline(&tmp).args(["takes", "01-010"]).assert().stdout(
        predicate::str::contains("01-010#1  failed")
            .and(predicate::str::contains("01-010#2  failed")),
    );
}

#[test]
fn budget_stops_a_take() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    // 6s at 0.1/s is 0.6; allow 0.5 a take
    let cfg = tmp.child("film/shotline.toml");
    let text = std::fs::read_to_string(cfg.path())
        .unwrap()
        .replace("per_take = 2", "per_take = 0.5");
    cfg.write_str(&text).unwrap();
    shotline(&tmp)
        .args(["take", "01-020", "--yes"])
        .assert()
        .failure()
        .stderr("error: over the per-take budget: 0 spent or held + 0.6 would pass 0.5 USD\n");
    assert!(ledger_types(&tmp).is_empty());
}

#[test]
fn credits_need_a_rate_or_a_plugin_one() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    shotline(&tmp)
        .args([
            "shot",
            "add",
            "1",
            "--prompt",
            "@paka",
            "--model",
            "fake/credits",
            "--duration",
            "3",
        ])
        .assert()
        .success();
    // The fake plugin publishes 0.01 USD per fake-credit, so this converts without config.
    shotline(&tmp)
        .args(["quote", "01-030"])
        .assert()
        .success()
        .stdout("01-030  fake fake/credits  3s  30 fake-credit (0.3 USD)\n");
}
