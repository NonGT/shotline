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
    // What was written and what the model got are kept with the job.
    let jobs = std::fs::read_dir(tmp.path().join("film/jobs")).unwrap();
    let last = jobs.map(|e| e.unwrap().path()).max().unwrap();
    let job = std::fs::read_to_string(last.join("job.toml")).unwrap();
    assert!(job.contains("brief = \"side view\""), "{job}");
    let job: toml::Table = toml::from_str(&job).unwrap();
    assert!(
        job["prompt"]
            .as_str()
            .unwrap()
            .starts_with("A reference image of Paka"),
        "{job}"
    );
    shotline(&tmp)
        .args(["element", "ls"])
        .assert()
        .stdout("cast  @paka             Paka (2 anchors)\n");
    assert_eq!(ledger(&tmp), 4);

    // Picked references: one anchor left out entirely, a file from elsewhere instead.
    tmp.child("dress.png")
        .write_binary(b"\x89PNG fake")
        .unwrap();
    shotline(&tmp)
        .args(["element", "anchor", "paka", "--generate", "--no-anchors"])
        .args([
            "--ref",
            "../dress.png",
            "--prompt",
            "in the dress from image 1",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("elements/cast/paka/03.png"));
    let jobs = std::fs::read_dir(tmp.path().join("film/jobs")).unwrap();
    let last = jobs.map(|e| e.unwrap().path()).max().unwrap();
    let job = std::fs::read_to_string(last.join("job.toml")).unwrap();
    assert!(job.contains("dress.png"), "{job}");
    assert!(!job.contains("01.png"), "{job}");
    shotline(&tmp)
        .args([
            "element",
            "anchor",
            "paka",
            "--generate",
            "--ref",
            "nope.png",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nope.png doesn't exist"));
    // A different shape from the film.
    shotline(&tmp)
        .args([
            "element",
            "anchor",
            "paka",
            "--generate",
            "--aspect",
            "1:1",
            "--resolution",
            "2K",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("elements/cast/paka/04.png"));
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

    // Its own size and no pictures along.
    shotline(&tmp)
        .args([
            "frame",
            "01-010",
            "--no-refs",
            "--aspect",
            "1:1",
            "--resolution",
            "2K",
            "--yes",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("chapters/01-one/shots/010.png"));
    shotline(&tmp)
        .args(["frame", "01-010", "--no-refs", "--clear"])
        .assert()
        .failure();

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

#[test]
fn translated_first() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    let toml = std::fs::read_to_string(tmp.path().join("film/shotline.toml")).unwrap();
    std::fs::write(
        tmp.path().join("film/shotline.toml"),
        toml + "[text]\nplugin = \"fake\"\nmodel = \"fake/text\"\n",
    )
    .unwrap();
    let make = |extra: &[&str]| {
        let mut c = shotline(&tmp);
        c.args([
            "element",
            "anchor",
            "paka",
            "--generate",
            "--prompt",
            "มุมข้าง",
        ])
        .args(extra);
        c
    };
    // Both prices before anything is spent.
    make(&["--translate"])
        .write_stdin("n\n")
        .assert()
        .success()
        .stdout(predicate::str::starts_with(
            "anchor for @paka, translated first · 0.04 USD + 0.001 USD to translate\n",
        ))
        .stdout(predicate::str::ends_with("nothing spent\n"));
    assert_eq!(ledger(&tmp), 0);

    // fake/text answers with the last line it's sent: the text to translate.
    make(&["--translate", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("in English: มุมข้าง\n"))
        .stdout(predicate::str::contains("elements/cast/paka/01.png"));
    let jobs = std::fs::read_dir(tmp.path().join("film/jobs")).unwrap();
    let last = jobs.map(|e| e.unwrap().path()).max().unwrap();
    let job: toml::Table =
        toml::from_str(&std::fs::read_to_string(last.join("job.toml")).unwrap()).unwrap();
    assert_eq!(job["op"].as_str(), Some("image.generate"));
    assert_eq!(job["brief"].as_str(), Some("มุมข้าง"));
    assert_eq!(job["english"].as_str(), Some("มุมข้าง"));

    // English needs nothing done: one job, no translation.
    shotline(&tmp)
        .args([
            "element",
            "anchor",
            "paka",
            "--generate",
            "--prompt",
            "side view",
        ])
        .args(["--translate", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("translation").not());
}

#[test]
fn a_shot_s_prompt_translated_once() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    let toml = std::fs::read_to_string(tmp.path().join("film/shotline.toml")).unwrap();
    std::fs::write(
        tmp.path().join("film/shotline.toml"),
        toml + "[text]\nplugin = \"fake\"\nmodel = \"fake/text\"\n",
    )
    .unwrap();
    shotline(&tmp)
        .args(["shot", "set", "01-010", "--prompt", "@paka เดินเข้าป่า"])
        .assert()
        .success();
    shotline(&tmp)
        .args(["take", "01-010", "--translate"])
        .write_stdin("n\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("to translate on 01-010"))
        .stdout(predicate::str::ends_with("nothing spent\n"));
    // fake/text answers with the last line it's sent: the text to translate.
    shotline(&tmp)
        .args(["take", "01-010", "--translate", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("in English: @paka เดินเข้าป่า\n"))
        .stdout(predicate::str::contains("01-010#1 done"));
    tmp.child("film/chapters/01-one/shots/010.toml")
        .assert(predicate::str::contains("[english]"));
    // Kept: the next take doesn't translate again.
    shotline(&tmp)
        .args(["take", "01-010", "--translate", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("translation").not());

    shotline(&tmp)
        .args(["frame", "01-010", "--translate", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("translation for 01-010"));
}

#[test]
fn pictures_of_a_shot() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    tmp.child("cafe.png").write_binary(b"\x89PNG cafe").unwrap();
    tmp.child("film/anchor.png")
        .write_binary(b"\x89PNG paka")
        .unwrap();
    shotline(&tmp)
        .args(["element", "anchor", "paka", "--file", "anchor.png"])
        .assert()
        .success();
    shotline(&tmp)
        .args(["shot", "image", "01-010", "--file", "../cafe.png"])
        .args(["--note", "the café at night"])
        .assert()
        .success()
        .stdout("chapters/01-one/shots/010/01.png\n");
    // After Paka's anchor, named with its note.
    shotline(&tmp)
        .args(["prompt", "01-010"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Ref 1 walks\n\nRef 2: the café at night.",
        ))
        .stdout(predicate::str::contains(
            "Ref 2                          chapters/01-one/shots/010/01.png",
        ));

    shotline(&tmp)
        .args([
            "shot",
            "image",
            "01-010",
            "--generate",
            "--prompt",
            "@paka's desk",
        ])
        .args(["--note", "her desk", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "picture for 01-010 · fake fake/image · 0.04 USD",
        ))
        .stdout(predicate::str::contains("chapters/01-one/shots/010/02.png"));
    // Paka's anchor went along, named in the image prompt.
    let jobs = std::fs::read_dir(tmp.path().join("film/jobs")).unwrap();
    let last = jobs.map(|e| e.unwrap().path()).max().unwrap();
    let job: toml::Table =
        toml::from_str(&std::fs::read_to_string(last.join("job.toml")).unwrap()).unwrap();
    assert!(
        job["prompt"]
            .as_str()
            .unwrap()
            .starts_with("image 1's desk"),
        "{job}"
    );
    assert_eq!(job["shot"].as_str(), Some("01-010"));

    shotline(&tmp)
        .args([
            "shot",
            "image",
            "01-010",
            "--edit",
            "1",
            "--note",
            "the café",
        ])
        .assert()
        .success();
    shotline(&tmp)
        .args(["shot", "image", "01-010", "--remove", "2"])
        .assert()
        .success()
        .stdout("chapters/01-one/shots/010/02.png taken off 01-010\n");
    tmp.child("film/chapters/01-one/shots/010/02.png")
        .assert(predicate::path::missing());
    shotline(&tmp)
        .args(["shot", "image", "01-010"])
        .assert()
        .success()
        .stdout("1  chapters/01-one/shots/010/01.png  the café\n");
    // Paka's anchor left out of this shot: her name instead, the shot's picture is Ref 1.
    shotline(&tmp)
        .args([
            "shot",
            "set",
            "01-010",
            "--leave-out",
            "elements/cast/paka/01.png",
        ])
        .assert()
        .success();
    shotline(&tmp)
        .args(["prompt", "01-010"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Paka walks\n\nRef 1: the café."));
    shotline(&tmp)
        .args([
            "shot",
            "set",
            "01-010",
            "--put-back",
            "elements/cast/paka/01.png",
        ])
        .assert()
        .success();
    tmp.child("film/chapters/01-one/shots/010.toml")
        .assert(predicate::str::contains("leave_out").not());
    // A shot folder doesn't count as a shot.
    shotline(&tmp)
        .args(["shot", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("01-010").count(1));
}
