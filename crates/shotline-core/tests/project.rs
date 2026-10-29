use std::fs;

use shotline_core::{Kind, Project, Shot, ShotRef, Transition};
use tempfile::TempDir;

fn new_project() -> (TempDir, Project) {
    let dir = TempDir::new().unwrap();
    let p = Project::init(dir.path(), Some("Forest")).unwrap();
    (dir, p)
}

#[test]
fn init_lays_out_the_folder() {
    let (dir, p) = new_project();
    for path in [
        "shotline.toml",
        "story.md",
        "style.md",
        "elements/cast",
        "chapters",
        ".shotline",
    ] {
        assert!(dir.path().join(path).exists(), "{path} missing");
    }
    assert_eq!(p.config.name, "Forest");
    assert!(
        Project::init(dir.path(), None).is_err(),
        "init twice should fail"
    );
}

#[test]
fn discover_walks_up() {
    let (dir, _) = new_project();
    let deep = dir.path().join("chapters/x/y");
    fs::create_dir_all(&deep).unwrap();
    assert_eq!(Project::discover(&deep).unwrap().root, dir.path());
    assert!(Project::discover(&std::env::temp_dir()).is_err());
}

#[test]
fn elements_round_trip() {
    let (_dir, p) = new_project();
    p.add_element(Kind::Cast, "Paka", None, "lead, 30s")
        .unwrap();
    p.add_element(Kind::Set, "ป่าลึก", Some("@deep-forest"), "")
        .unwrap();

    let els = p.elements().unwrap();
    assert_eq!(els.len(), 2);
    assert_eq!(els[0].handle, "paka");
    assert_eq!(els[0].kind, Kind::Cast);
    assert_eq!(els[1].name, "ป่าลึก");
    assert_eq!(els[1].kind, Kind::Set);

    assert!(
        p.add_element(Kind::Prop, "Paka", None, "").is_err(),
        "duplicate handle"
    );
    assert!(
        p.add_element(Kind::Cast, "มะลิ", None, "").is_err(),
        "no ascii to slug"
    );
    assert!(
        p.add_element(Kind::Cast, "x", Some("Bad Handle"), "")
            .is_err()
    );
}

#[test]
fn anchors_are_numbered() {
    let (dir, p) = new_project();
    p.add_element(Kind::Cast, "Paka", None, "").unwrap();
    let img = dir.path().join("face.PNG");
    fs::write(&img, b"png").unwrap();

    let a = p.add_anchor("@paka", &img).unwrap();
    let b = p.add_anchor("paka", &img).unwrap();
    assert!(a.ends_with("elements/cast/paka/01.png"));
    assert!(b.ends_with("elements/cast/paka/02.png"));

    fs::remove_file(&a).unwrap();
    let c = p.add_anchor("paka", &img).unwrap();
    assert!(c.ends_with("03.png"), "must not overwrite 02");

    let txt = dir.path().join("notes.txt");
    fs::write(&txt, b"").unwrap();
    assert!(p.add_anchor("paka", &txt).is_err());
    assert!(p.add_anchor("nobody", &img).is_err());
}

#[test]
fn shots_get_ids_and_elements() {
    let (_dir, p) = new_project();
    p.add_element(Kind::Cast, "Paka", None, "").unwrap();
    p.add_element(Kind::Cast, "Kan", None, "").unwrap();
    let ch = p.add_chapter("Into the Forest").unwrap();
    assert!(ch.dir.ends_with("chapters/01-into-the-forest"));
    p.add_chapter("ทางกลับ").unwrap();

    let shot = |prompt: &str| Shot {
        prompt: prompt.into(),
        ..Shot::default()
    };
    let a = p.add_shot("1", None, shot("@paka walks")).unwrap();
    let b = p.add_shot("01", None, shot("@kan follows @paka")).unwrap();
    let c = p
        .add_shot("01-into-the-forest", Some(10), shot("wide"))
        .unwrap();
    let d = p.add_shot("2", None, shot("@kan alone")).unwrap();
    assert_eq!(
        [a, b, c, d].map(|r| r.to_string()),
        ["01-010", "01-020", "01-015", "02-010"]
    );

    assert_eq!(p.shot(b).unwrap().elements, ["kan", "paka"]);
    let order: Vec<_> = p.shots(None).unwrap().into_iter().map(|(r, _)| r).collect();
    assert_eq!(order, [a, c, b, d]);
    assert_eq!(p.previous_shot(d).unwrap(), Some(b), "crosses chapters");
    assert_eq!(p.previous_shot(a).unwrap(), None);

    assert!(p.add_shot("1", None, shot("@ghost")).is_err());
    assert!(p.add_shot("9", None, shot("x")).is_err());
}

#[test]
fn shot_survives_save_and_load() {
    let (_dir, p) = new_project();
    p.add_chapter("One").unwrap();
    let shot = Shot {
        title: "Rest stop".into(),
        plot: "พักเท้ากลางทาง".into(),
        prompt: "Handheld medium shot".into(),
        duration: Some(6.5),
        transition: Transition::Continue,
        selected_take: Some("t002".into()),
        ..Shot::default()
    };
    let r = p.add_shot("1", None, shot.clone()).unwrap();
    assert_eq!(
        r,
        ShotRef {
            chapter: 1,
            shot: 10
        }
    );
    assert_eq!(p.shot(r).unwrap(), shot);
}
