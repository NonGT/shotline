use std::path::Path;

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
    let app = App::new(film(dir), dir.join("home")).with_player(None);
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

    h.get_by_label("Play #1").click();
    h.run();
    h.get_by_label_contains("no player found");
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
