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

/// fake/text answers with the last line of what it's sent, which is the plot.
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
             aspect = \"9:16\"\nduration = 4\n[text]\nplugin = \"fake\"\nmodel = \"fake/text\"\n",
        )
        .unwrap();
    run(&[
        "element",
        "add",
        "cast",
        "ผกา",
        "--handle",
        "paka",
        "--desc",
        "หัวหน้าทีม",
    ]);
    run(&["chapter", "add", "One"]);
    run(&["shot", "add", "1", "--plot", "@paka เดินเข้าป่า"]);
}

fn ledger(tmp: &TempDir) -> usize {
    std::fs::read_to_string(tmp.path().join("film/.shotline/ledger.jsonl"))
        .unwrap_or_default()
        .lines()
        .count()
}

fn shot_file(tmp: &TempDir) -> String {
    std::fs::read_to_string(tmp.path().join("film/chapters/01-one/shots/010.toml")).unwrap()
}

#[test]
fn draft_a_prompt_from_the_plot() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    shotline(&tmp)
        .args(["prompt", "01-010"])
        .assert()
        .success()
        .stderr(predicate::str::contains("01-010 has no prompt yet"));
    shotline(&tmp)
        .args(["shot", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("@paka เดินเข้าป่า"));

    // Said no to spending.
    shotline(&tmp)
        .args(["shot", "draft", "01-010"])
        .write_stdin("n\n")
        .assert()
        .success()
        .stdout(predicate::str::starts_with(
            "video draft for 01-010 · fake fake/text · 0.001 USD\n",
        ))
        .stdout(predicate::str::ends_with("nothing spent\n"));
    assert_eq!(ledger(&tmp), 0);

    // Spent, then didn't keep it.
    shotline(&tmp)
        .args(["shot", "draft", "01-010"])
        .write_stdin("y\nn\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("\n\n@paka เดินเข้าป่า\n\n"))
        .stdout(predicate::str::contains(
            "use it as 01-010's prompt? [y/N] not saved\n",
        ));
    assert!(shot_file(&tmp).contains("prompt = \"\""));

    shotline(&tmp)
        .args(["shot", "draft", "01-010", "--yes", "--write"])
        .assert()
        .success()
        .stdout(predicate::str::ends_with("01-010 prompt saved\n"));
    assert!(shot_file(&tmp).contains("prompt = \"@paka เดินเข้าป่า\""));

    shotline(&tmp)
        .args([
            "shot", "draft", "01-010", "--as", "image", "--yes", "--write",
        ])
        .assert()
        .success()
        .stdout(predicate::str::starts_with("image draft for 01-010"))
        .stdout(predicate::str::ends_with("01-010 frame prompt saved\n"));
    assert!(shot_file(&tmp).contains("frame_prompt = \"@paka เดินเข้าป่า\""));

    // --yes alone shows the draft and keeps nothing.
    shotline(&tmp)
        .args(["shot", "draft", "01-010", "--as", "translate", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::ends_with("not saved\n"));
    assert_eq!(
        ledger(&tmp),
        8,
        "a reserve and a charge for each of 4 drafts"
    );
}

#[test]
fn a_draft_with_an_unknown_handle_is_refused() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    shotline(&tmp)
        .args(["shot", "set", "01-010", "--plot", "@ghost เดินเข้าป่า"])
        .assert()
        .success();
    shotline(&tmp)
        .args(["shot", "draft", "01-010", "--yes", "--write"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "the draft uses @ghost, which isn't an element",
        ));
    assert!(shot_file(&tmp).contains("prompt = \"\""));
}

#[test]
fn translate_a_description() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    shotline(&tmp)
        .args(["element", "draft", "paka", "--yes", "--write"])
        .assert()
        .success()
        .stdout(predicate::str::starts_with(
            "translation of @paka · fake fake/text · 0.001 USD\n",
        ))
        .stdout(predicate::str::ends_with("@paka description saved\n"));
}
