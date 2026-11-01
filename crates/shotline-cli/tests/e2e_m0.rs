use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use predicates::prelude::*;

/// Runs in `dir` with an empty user folder, so installed plugins on this machine don't leak in.
fn shotline(dir: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("shotline").unwrap();
    cmd.current_dir(dir.path())
        .env("SHOTLINE_HOME", dir.path().join(".home"))
        .env("PATH", "")
        .timeout(std::time::Duration::from_secs(60));
    cmd
}

#[test]
fn plan_a_film_and_preview_prompts() {
    let tmp = TempDir::new().unwrap();
    shotline(&tmp)
        .args(["init", "forest", "--name", "Into the Forest"])
        .assert()
        .success();
    let film = tmp.child("forest");
    film.child("shotline.toml")
        .assert(predicate::path::exists());
    let run = || {
        let mut c = shotline(&tmp);
        c.current_dir(film.path());
        c
    };

    tmp.child("paka.png").write_binary(b"png").unwrap();
    run()
        .args(["element", "add", "cast", "ผกา", "--handle", "paka"])
        .assert()
        .success()
        .stdout("added ผกา @paka (cast)\n");
    run()
        .args(["element", "add", "set", "Deep forest"])
        .assert()
        .success();
    run()
        .args(["element", "anchor", "paka", "--file"])
        .arg(tmp.child("paka.png").path())
        .assert()
        .success()
        .stdout("elements/cast/paka/01.png\n");
    run()
        .args(["element", "ls"])
        .assert()
        .stdout(predicate::str::contains("@paka").and(predicate::str::contains("(1 anchor)")));

    run()
        .args(["chapter", "add", "Into the forest"])
        .assert()
        .stdout("01 chapters/01-into-the-forest\n");

    run()
        .args(["shot", "add", "1", "--plot", "เดินเข้าป่า"])
        .args(["--prompt", "@paka walks into @deep-forest"])
        .assert()
        .success()
        .stdout("01-010\n");
    run()
        .args([
            "shot",
            "add",
            "1",
            "--transition",
            "continue",
            "--duration",
            "6",
        ])
        .args(["--title", "Turn", "--prompt", "@paka turns around"])
        .assert()
        .stdout("01-020\n");
    run()
        .args([
            "shot",
            "add",
            "1",
            "--after",
            "010",
            "--prompt",
            "close on @paka",
        ])
        .assert()
        .stdout("01-015\n");

    run()
        .args(["shot", "ls"])
        .assert()
        .stdout(predicate::str::is_match("(?s)01-010.*01-015.*01-020  continue").unwrap());
    run()
        .args(["shot", "show", "01-010"])
        .assert()
        .stdout(predicate::str::contains(
            "elements = [\n    \"paka\",\n    \"deep-forest\",\n]",
        ));

    run()
        .args(["prompt", "01-010"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Image 1 walks into Deep forest"))
        .stderr(predicate::str::contains(
            "@deep-forest has no anchor images",
        ));
    run()
        .args(["prompt", "01-020"])
        .assert()
        .success()
        .stderr(predicate::str::contains("01-015 has no selected take"));
}

#[test]
fn works_from_a_subfolder_and_with_dash_c() {
    let tmp = TempDir::new().unwrap();
    shotline(&tmp).args(["init", "f"]).assert().success();
    let sub = tmp.child("f/chapters");
    shotline(&tmp)
        .current_dir(sub.path())
        .args(["chapter", "add", "One"])
        .assert()
        .success();
    shotline(&tmp)
        .args(["-C", "f", "chapter", "ls"])
        .assert()
        .stdout("01  One\n");
}

#[test]
fn errors_are_readable() {
    let tmp = TempDir::new().unwrap();
    shotline(&tmp)
        .args(["shot", "ls"])
        .assert()
        .failure()
        .stderr(predicate::str::starts_with("error: no shotline.toml in"));

    shotline(&tmp).args(["init", "f"]).assert().success();
    shotline(&tmp)
        .args(["-C", "f", "chapter", "add", "One"])
        .assert()
        .success();
    shotline(&tmp)
        .args(["-C", "f", "shot", "add", "1", "--prompt", "@ghost waves"])
        .assert()
        .failure()
        .stderr("error: unknown element @ghost\n");
    shotline(&tmp)
        .args(["-C", "f", "prompt", "01-990"])
        .assert()
        .failure()
        .stderr("error: no shot 01-990\n");
    shotline(&tmp)
        .args(["-C", "f", "element", "anchor", "x", "--generate"])
        .assert()
        .failure();
}

#[test]
fn the_film_format() {
    let tmp = TempDir::new().unwrap();
    shotline(&tmp)
        .args(["init", "f", "--aspect", "16:9", "--resolution", "1080P"])
        .assert()
        .success();
    let f = || {
        let mut c = shotline(&tmp);
        c.args(["-C", "f", "format"]);
        c
    };
    f().assert().success().stdout(
        "model       fal minimax/h3-max/reference-to-video\nshape       16:9\n\
         resolution  1080P\nduration    8 s\n",
    );
    f().args(["--duration", "5", "--aspect", "4:5"])
        .assert()
        .success()
        .stdout(predicate::str::contains("shape       4:5\n"))
        .stdout(predicate::str::contains("duration    5 s\n"))
        .stderr(predicate::str::contains(
            "warning: minimax/h3-max/reference-to-video doesn't do 4:5",
        ));
    f().args(["--model", "nope"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no video.generate model nope"));
    f().args(["--duration", "0"]).assert().failure();
    f().assert()
        .stdout(predicate::str::contains("duration    5 s\n"));
}

#[test]
fn a_shot_on_another_model() {
    let tmp = TempDir::new().unwrap();
    shotline(&tmp).args(["init", "f"]).assert().success();
    let run = |args: &[&str]| {
        let mut c = shotline(&tmp);
        c.arg("-C").arg("f").args(args);
        c.assert()
    };
    run(&["chapter", "add", "One"]).success();
    run(&["shot", "add", "1", "--prompt", "rain"]).success();
    let model = "minimax/h3-max/reference-to-video";
    run(&[
        "shot",
        "set",
        "01-010",
        "--model",
        model,
        "--resolution",
        "1080P",
    ])
    .success();
    run(&["prompt", "01-010"])
        .success()
        .stdout(predicate::str::contains(format!(
            "fal {model} · 8s · 1080P · 9:16"
        )));
    run(&["shot", "set", "01-010", "--model", "nope"])
        .failure()
        .stderr(predicate::str::contains("no video.generate model nope"));
    // Back to the film's, and nothing left behind in the shot file.
    run(&["shot", "set", "01-010", "--model", "", "--resolution", ""]).success();
    let shot = std::fs::read_to_string(
        std::fs::read_dir(tmp.path().join("f/chapters"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("shots/010.toml"),
    )
    .unwrap();
    assert!(!shot.contains("[model]"), "{shot}");

    // A shot of another shape is warned about: the render stretches it.
    let path = std::fs::read_dir(tmp.path().join("f/chapters"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("shots/010.toml");
    std::fs::write(&path, shot + "\n[model]\naspect = \"16:9\"\n").unwrap();
    run(&["prompt", "01-010"])
        .success()
        .stderr(predicate::str::contains("the render will stretch it"));
}
