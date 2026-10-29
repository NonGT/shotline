use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use predicates::prelude::*;

fn shotline(dir: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("shotline").unwrap();
    cmd.current_dir(dir.path());
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
        let mut c = Command::cargo_bin("shotline").unwrap();
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
    let mut cmd = Command::cargo_bin("shotline").unwrap();
    cmd.current_dir(sub.path())
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
