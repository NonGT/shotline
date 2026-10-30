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

/// A film on the fake plugin, images included: Paka without anchors, one shot.
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
             aspect = \"9:16\"\nduration = 4\n[image]\nplugin = \"fake\"\nmodel = \"fake/image\"\n",
        )
        .unwrap();
    run(&["element", "add", "cast", "Paka", "--desc", "team lead, 30s"]);
    run(&["chapter", "add", "One"]);
    run(&["shot", "add", "1", "--prompt", "@paka walks"]);
}

fn ledger(tmp: &TempDir) -> usize {
    std::fs::read_to_string(tmp.path().join("film/.shotline/ledger.jsonl"))
        .unwrap_or_default()
        .lines()
        .count()
}

#[test]
fn generated_anchor() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    shotline(&tmp)
        .args(["element", "anchor", "paka", "--generate"])
        .write_stdin("n\n")
        .assert()
        .success()
        .stdout(predicate::str::starts_with(
            "anchor for @paka · fake fake/image · 0.04 USD\n",
        ))
        .stdout(predicate::str::ends_with("nothing spent\n"));
    assert_eq!(ledger(&tmp), 0);

    shotline(&tmp)
        .args(["element", "anchor", "paka", "--generate", "--yes"])
        .assert()
        .success()
        .stdout(
            predicate::str::is_match(r"\nelements/cast/paka/01\.png · 0\.04 USD · job \w+\n$")
                .unwrap(),
        );
    shotline(&tmp)
        .args([
            "element",
            "anchor",
            "paka",
            "--generate",
            "--prompt",
            "side view",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("elements/cast/paka/02.png"));
    shotline(&tmp)
        .args(["element", "ls"])
        .assert()
        .stdout("cast  @paka             Paka (2 anchors)\n");
    assert_eq!(ledger(&tmp), 4);
}

#[test]
fn storyboard_frame() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    shotline(&tmp)
        .args(["frame", "01-010", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "storyboard frame for 01-010 · fake fake/image · 0.04 USD",
        ))
        .stdout(predicate::str::contains(
            "chapters/01-one/shots/010.png · 0.04 USD",
        ));
    tmp.child("film/chapters/01-one/shots/010.png")
        .assert(predicate::path::exists());
    tmp.child("film/chapters/01-one/shots/010.toml")
        .assert(predicate::str::contains(
            "frame = \"chapters/01-one/shots/010.png\"",
        ));
    shotline(&tmp)
        .args(["prompt", "01-010"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "start frame: chapters/01-one/shots/010.png (storyboard)",
        ));
    // The frame job belongs to the shot but isn't a take.
    shotline(&tmp).args(["takes", "01-010"]).assert().stdout("");

    shotline(&tmp)
        .args(["take", "01-010", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("+ start frame"));

    tmp.child("own.jpg").write_binary(b"jpg").unwrap();
    shotline(&tmp)
        .args(["frame", "01-010", "--file", "../own.jpg"])
        .assert()
        .success()
        .stdout("chapters/01-one/shots/010.jpg\n");
    tmp.child("film/chapters/01-one/shots/010.png")
        .assert(predicate::path::missing());
    shotline(&tmp)
        .args(["frame", "01-010", "--clear"])
        .assert()
        .success()
        .stdout("01-010 has no storyboard frame now\n");
    tmp.child("film/chapters/01-one/shots/010.jpg")
        .assert(predicate::path::missing());
}
