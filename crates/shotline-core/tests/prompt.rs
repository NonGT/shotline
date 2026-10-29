use std::fs;

use shotline_core::{Kind, ModelOverride, Project, Shot, ShotRef, Transition, build};
use tempfile::TempDir;

/// Two cast members with anchors, one set without, one chapter.
fn film() -> (TempDir, Project) {
    let dir = TempDir::new().unwrap();
    let p = Project::init(dir.path(), Some("Forest")).unwrap();
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

fn venice() -> Option<ModelOverride> {
    Some(ModelOverride {
        provider: Some("venice".into()),
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
    add(&p, "@paka walks in.", |s| {
        s.selected_take = Some("t002".into())
    });
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
