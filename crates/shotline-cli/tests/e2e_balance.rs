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

fn film(tmp: &TempDir) {
    let exe = assert_cmd::cargo::cargo_bin("shotline");
    tmp.child(".home/plugins/fake/shotline-plugin.toml")
        .write_str(&format!(
            "name = \"fake\"\nexec = {:?}\nargs = [\"__fake-plugin\"]\nprotocol = 1\n",
            exe.display().to_string()
        ))
        .unwrap();
    std::fs::create_dir_all(tmp.path().join("film")).unwrap();
    shotline(tmp).args(["init", "."]).assert().success();
    tmp.child("film/clip.mp4").write_binary(b"mp4").unwrap();
    tmp.child("film/line.wav").write_binary(b"wav").unwrap();
}

fn upscale(tmp: &TempDir) -> assert_cmd::assert::Assert {
    shotline(tmp)
        .args(["run", "video.upscale", "--plugin", "fake"])
        .args(["--model", "fake/metered", "--duration", "4"])
        .args(["--input", "video=clip.mp4", "--yes"])
        .assert()
}

#[test]
fn measured_rates_are_a_fallback() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    shotline(&tmp)
        .args(["plugin", "balance", "fake"])
        .assert()
        .success()
        .stdout("fake  100 USD\n");

    // fake-second has no rate from the plugin; yours is used and the real one measured.
    tmp.child(".home/config.toml")
        .write_str("[rates]\nfake-second = \"0.05 USD\"\n")
        .unwrap();
    upscale(&tmp).success().stdout(
        predicate::str::contains("4 fake-second (0.2 USD at your rate)")
            .and(predicate::str::contains("fake balance: 100 USD"))
            .and(predicate::str::contains("fake balance dropped 0.12 USD"))
            .and(predicate::str::contains(
                "measured fake-second at 0.03 USD, kept as a fallback rate",
            )),
    );
    shotline(&tmp)
        .args(["cost", "rates"])
        .assert()
        .success()
        .stdout(
        predicate::str::is_match(r"fake-second  0\.05 USD  \S+config\.toml\n")
            .unwrap()
            .and(
                predicate::str::is_match(
                    r"fake-second  0\.03 USD  measured, 1 samples, \d{4}-\d\d-\d\d  \(not used\)",
                )
                .unwrap(),
            ),
    );

    // Without your rate the measured one stands in, and says so.
    tmp.child(".home/config.toml").write_str("").unwrap();
    upscale(&tmp).success().stdout(predicate::str::contains(
        "4 fake-second (0.12 USD at a measured rate)",
    ));
    shotline(&tmp).arg("cost").assert().success().stdout(
        predicate::str::contains("priced   measured rates 0.12, your rates 0.2\n").and(
            predicate::str::contains("balances 2 jobs read before and after, all agree\n"),
        ),
    );
    shotline(&tmp).args(["cost", "verify"]).assert().success();
}

#[test]
fn a_balance_that_drops_more_than_booked() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    shotline(&tmp)
        .args(["run", "video.lipsync", "--plugin", "fake"])
        .args(["--model", "fake/lipsync", "--duration", "6"])
        .args(["--input", "video=clip.mp4", "--input", "audio=line.wav"])
        .args(["--param", "fake_overcharge=2", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fake balance dropped 0.6 USD"));
    shotline(&tmp)
        .arg("cost")
        .assert()
        .stdout(predicate::str::contains(
            "balances 1 jobs read before and after, 1 differ, see `shotline cost reconcile`",
        ));
    tmp.child("film/shotline-fake-usage.json")
        .write_str(
            r#"{"charges": [{"job": "fake-1", "time": "2026-10-30T10:00:00Z", "amount": 0.3, "unit": "USD"}]}"#,
        )
        .unwrap();
    shotline(&tmp)
        .args(["cost", "reconcile"])
        .assert()
        .failure()
        .stdout(
            predicate::str::is_match(
                r"balances: 1 jobs read before and after, 1 differ\n  \w{26} \(fake\): booked 0\.3 USD but the balance dropped 0\.6 USD\nfake: matches \(1 charges\)\n",
            )
            .unwrap(),
        );
}

#[test]
fn a_balance_that_never_moves() {
    let tmp = TempDir::new().unwrap();
    film(&tmp);
    shotline(&tmp)
        .args(["run", "video.lipsync", "--plugin", "fake"])
        .args(["--model", "fake/lipsync", "--duration", "6"])
        .args(["--input", "video=clip.mp4", "--input", "audio=line.wav"])
        .args(["--param", "fake_no_drop=true", "--yes"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("fake balance hasn't moved yet; booked the provider's price")
                .and(predicate::str::contains("done · 0.3 USD")),
        );
    shotline(&tmp)
        .arg("cost")
        .assert()
        .stdout(predicate::str::contains("balances").not());
}
