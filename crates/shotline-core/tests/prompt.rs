use std::fs;
use std::sync::Arc;

use shotline_core::protocol::Op;
use shotline_core::{
    Built, Catalog, Job, JobStatus, Kind, ModelOverride, Project, Shot, ShotRef, TestClock,
    Transition,
};
use tempfile::TempDir;

/// Two cast members with anchors, one set without, one chapter.
fn film() -> (TempDir, Project) {
    let dir = TempDir::new().unwrap();
    let clock = TestClock::new("2026-10-30T10:00:00Z".parse().unwrap());
    let p = Project::init(dir.path(), Some("Forest"))
        .unwrap()
        .with_clock(Arc::new(clock));
    let img = dir.path().join("in.png");
    fs::write(&img, b"png").unwrap();
    p.add_element(Kind::Cast, "Paka", None, "").unwrap();
    p.add_element(Kind::Cast, "Kan", None, "").unwrap();
    p.add_element(Kind::Set, "Deep forest", None, "").unwrap();
    p.add_anchor("paka", &img).unwrap();
    p.add_anchor("paka", &img).unwrap();
    p.add_anchor("kan", &img).unwrap();
    p.add_chapter("Into the forest").unwrap();
    fs::write(
        dir.path().join("style.md"),
        "<!-- note -->\nMuted greens, 35mm, soft overcast light.\n",
    )
    .unwrap();
    (dir, p)
}

fn add(p: &Project, prompt: &str, f: impl FnOnce(&mut Shot)) -> ShotRef {
    let mut shot = Shot {
        prompt: prompt.into(),
        ..Shot::default()
    };
    f(&mut shot);
    p.add_shot("1", None, shot).unwrap()
}

fn build(p: &Project, r: ShotRef) -> shotline_core::Result<Built> {
    shotline_core::build(p, &p.catalog(Catalog::builtin())?, r)
}

/// A finished one-step take for a shot.
fn finished_take(p: &Project, r: ShotRef) {
    let mut job = Job::new(
        Op::VideoGenerate,
        "fal",
        "minimax/h3-max/reference-to-video",
    );
    job.shot = Some(r);
    job.take = Some(p.new_take_id());
    let mut job = p.create_job(job).unwrap();
    job.status = JobStatus::Done;
    job.outputs = vec!["out.mp4".into()];
    p.save_job(&job).unwrap();
}

fn venice() -> Option<ModelOverride> {
    Some(ModelOverride {
        plugin: Some("venice".into()),
        id: Some("minimax-h3-max-reference-to-video".into()),
        ..Default::default()
    })
}

#[test]
fn cut_on_fal() {
    let (_d, p) = film();
    let r = add(&p, "@kan argues with @paka in @deep-forest.", |_| {});
    insta::assert_debug_snapshot!(build(&p, r).unwrap());
}

#[test]
fn cut_on_venice() {
    let (_d, p) = film();
    let r = add(&p, "@kan argues with @paka.", |s| {
        s.model = venice();
        s.duration = Some(7.5);
    });
    insta::assert_debug_snapshot!(build(&p, r).unwrap());
}

#[test]
fn continue_with_selected_take() {
    let (_d, p) = film();
    let first = add(&p, "@paka walks in.", |_| {});
    finished_take(&p, first);
    p.select_take(first, "1").unwrap();
    let r = add(&p, "@paka stops and turns.", |s| {
        s.transition = Transition::Continue
    });
    insta::assert_debug_snapshot!(build(&p, r).unwrap());
}

#[test]
fn continue_on_venice_warns() {
    let (_d, p) = film();
    add(&p, "@paka walks in.", |_| {});
    let r = add(&p, "@paka stops.", |s| {
        s.transition = Transition::Continue;
        s.model = venice();
    });
    let b = build(&p, r).unwrap();
    assert_eq!(b.start_frame, None);
    insta::assert_debug_snapshot!(b.warnings);
}

#[test]
fn project_templates_and_layer_switches() {
    let (dir, p) = film();
    fs::create_dir_all(dir.path().join("prompts")).unwrap();
    fs::write(dir.path().join("prompts/style.md"), "Look: {style}").unwrap();
    let r = add(&p, "@paka waits.", |_| {});

    let b = build(&p, r).unwrap();
    assert!(b.text.contains("Look: Muted greens"), "{}", b.text);

    let mut p = p;
    p.config.prompt.style = false;
    p.config.prompt.identity = false;
    assert_eq!(build(&p, r).unwrap().text, "Image 1 and Image 2 waits.");
}

#[test]
fn too_many_references() {
    let (dir, p) = film();
    let img = dir.path().join("in.png");
    for _ in 0..8 {
        p.add_anchor("kan", &img).unwrap();
    }
    let r = add(&p, "@paka and @kan", |_| {});
    let b = build(&p, r).unwrap();
    assert_eq!(b.references.len(), 11);
    assert!(b.warnings.iter().any(|w| w.contains("at most 9")));
}

#[test]
fn selected_take_that_is_gone() {
    let (_d, p) = film();
    add(&p, "@paka walks in.", |s| {
        s.selected_take = Some("01JBX9Q3T7K2M8V5N4R6P1W0YZ".into())
    });
    let r = add(&p, "@paka stops.", |s| s.transition = Transition::Continue);
    let b = build(&p, r).unwrap();
    assert!(
        b.warnings
            .iter()
            .any(|w| w.contains("selected take is gone")),
        "{:?}",
        b.warnings
    );
}

#[test]
fn unknown_model() {
    let (_d, p) = film();
    let r = add(&p, "@paka", |s| {
        s.model = Some(ModelOverride {
            plugin: Some("nobody".into()),
            ..Default::default()
        })
    });
    let e = build(&p, r).unwrap_err().to_string();
    assert!(e.contains("from plugin nobody"), "{e}");
}
