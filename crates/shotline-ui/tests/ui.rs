use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use shotline_core::ledger::Context;
use shotline_core::protocol::{Op, Quote};
use shotline_core::{Job, JobStatus, Kind, Project, Shot, ShotRef, UserConfig};
use shotline_ui::{App, Tab};

/// Paka, one shot with a done take and a failed one, and 0.4 USD spent on the done one.
fn film(dir: &Path) -> Project {
    let p = Project::init(&dir.join("film"), Some("Rain")).unwrap();
    p.add_element(Kind::Cast, "Paka", None, "team lead, 30s")
        .unwrap();
    p.add_chapter("Into the forest").unwrap();
    let r = p
        .add_shot(
            "1",
            None,
            Shot {
                title: "Paka walks in".into(),
                plot: "ผกาเดินเข้าป่า".into(),
                prompt: "@paka walks into the trees".into(),
                ..Shot::default()
            },
        )
        .unwrap();
    let take = |status, out: bool| {
        let mut j = Job::new(Op::VideoGenerate, "fake", "fake/video");
        j.shot = Some(r);
        j.take = Some(p.new_take_id());
        let mut j = p.create_job(j).unwrap();
        j.status = status;
        if out {
            std::fs::write(p.job_dir(j.id).join("out.mp4"), "mp4").unwrap();
            j.outputs = vec!["out.mp4".into()];
        } else {
            j.error = Some("content policy".into());
        }
        p.save_job(&j).unwrap();
        j
    };
    let done = take(JobStatus::Done, true);
    take(JobStatus::Failed, false);

    let home = dir.join("home");
    let books = p
        .books(&home, &UserConfig::load(&home).unwrap(), &[])
        .unwrap();
    let now = jiff::Timestamp::now();
    let ctx = Context {
        job: done.id.to_string(),
        take: done.take.map(|t| t.to_string()),
        shot: Some(r.to_string()),
        chapter: Some(1),
        op: "video.generate".into(),
        plugin: "fake".into(),
        model: "fake/video".into(),
    };
    let quote = Quote {
        amount: 0.4,
        unit: "USD".into(),
        units: None,
        unit_name: None,
    };
    books.reserve(now, &ctx, &quote).unwrap();
    books
        .charge(now, &done.id.to_string(), None, None, None)
        .unwrap();
    p
}

fn harness(dir: &Path) -> Harness<'static, App> {
    let app = App::new(film(dir), dir.join("home"))
        .with_viewer(None)
        .silent();
    // Thai needs a system font, which CI may not have.
    Harness::builder()
        .allow_missing_glyphs()
        .build_ui_state(|ui, app: &mut App| app.show(ui), app)
}

#[test]
fn browse_and_select_a_take() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = harness(tmp.path());
    h.run();
    assert_eq!(h.state().tab(), Tab::Shots);
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    h.get_by_label("ผกาเดินเข้าป่า");
    h.get_by_label("failed");
    h.get_by_label("content policy");

    h.get_by_label("Select #1").click();
    h.run();
    h.get_by_label("01-010#1 selected");
    h.get_by_label("#1 (selected)");
    assert!(h.query_by_label("Select #1").is_none());
    let p = Project::open(&tmp.path().join("film")).unwrap();
    let shot = p
        .shot(ShotRef {
            chapter: 1,
            shot: 10,
        })
        .unwrap();
    assert!(shot.selected_take.is_some());

    // Not a real video.
    h.get_by_label("Play #1").click();
    h.run();
    h.get_by_label_contains("can't play 01-010#1");
}

/// A second of test pattern with a tone, or None without ffmpeg.
fn clip(path: &Path) -> Option<()> {
    let ffmpeg = std::env::var("SHOTLINE_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let ok = Command::new(ffmpeg)
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc=size=160x90:rate=24:duration=1")
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=1"])
        .args([
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ])
        .arg(path)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("skipped: no ffmpeg");
    }
    ok.then_some(())
}

/// Steps the app for a while in real time, as playback needs.
fn wait(h: &mut Harness<App>, secs: f64) {
    let until = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < until {
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn plays_compares_and_plays_the_cut() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let r = ShotRef {
        chapter: 1,
        shot: 10,
    };
    let takes = p.takes(r).unwrap();
    let first = p.job_dir(takes[0].jobs[0].id).join("out.mp4");
    if clip(&first).is_none() {
        return;
    }
    let mut j = Job::new(Op::VideoGenerate, "fake", "fake/video");
    j.shot = Some(r);
    j.take = Some(p.new_take_id());
    let mut j = p.create_job(j).unwrap();
    std::fs::copy(&first, p.job_dir(j.id).join("out.mp4")).unwrap();
    j.status = JobStatus::Done;
    j.outputs = vec!["out.mp4".into()];
    p.save_job(&j).unwrap();
    p.select_take(r, "1").unwrap();

    let app = App::new(p, tmp.path().join("home"))
        .with_viewer(None)
        .silent();
    // Tall enough that the takes stay in view under the player.
    let mut h = Harness::builder()
        .with_size([1000.0, 1400.0])
        .allow_missing_glyphs()
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.step();
    h.get_by_label_contains("01-010").click();
    wait(&mut h, 0.1);
    assert!(h.query_by_label("Compare #3").is_none());
    h.get_by_label("Play #1").click();
    wait(&mut h, 0.3);
    assert!(h.state().is_playing());
    // The title, and the picture's alt text.
    assert_eq!(h.query_all_by_label("01-010#1").count(), 2);
    h.get_by_label("Pause");

    h.get_by_label("Compare #3").click();
    wait(&mut h, 0.3);
    h.get_by_label("01-010#1 vs 01-010#3");
    assert!(h.state().is_playing());
    h.get_by_label("Pause").click();
    wait(&mut h, 0.1);
    assert!(!h.state().is_playing());

    h.get_by_label("Cut").click();
    wait(&mut h, 0.1);
    h.get_by_label("Play the cut").click();
    wait(&mut h, 0.3);
    assert_eq!(h.query_all_by_label("the cut").count(), 2);
    assert!(h.state().is_playing());
    // The cut doesn't loop: it stops after its one second.
    wait(&mut h, 1.5);
    assert!(!h.state().is_playing());
    h.get_by_label("Close player").click();
    wait(&mut h, 0.1);
    assert_eq!(h.query_all_by_label("the cut").count(), 0);
}

#[test]
fn elements_story_and_cost() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = harness(tmp.path());
    h.run();
    h.get_by_label("Elements").click();
    h.run();
    h.get_by_label("@paka");
    h.get_by_label("no anchor images");

    h.get_by_label("Story").click();
    h.run();
    h.get_by_label_contains("## Logline");

    h.get_by_label("Cost").click();
    h.run();
    h.get_by_label("0.4 USD");
    h.get_by_label("in the cut 0, thrown away 0.4, other 0");
    h.get_by_label("01-010");
}

#[test]
fn opens_where_it_was_left() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let home = tmp.path().join("home");
    let app = App::new(p.clone(), home.clone())
        .with_viewer(None)
        .silent()
        .remember_place();
    let mut h = Harness::builder()
        .allow_missing_glyphs()
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    h.get_by_label("Cut").click();
    h.run();

    let app = App::new(p, home).silent().remember_place();
    assert_eq!(app.tab(), Tab::Cut);
    let mut h = Harness::builder()
        .allow_missing_glyphs()
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label("Shots").click();
    h.run();
    h.get_by_label("ผกาเดินเข้าป่า");
}
