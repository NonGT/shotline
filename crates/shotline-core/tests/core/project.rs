use std::fs;

use shotline_core::{Format, Kind, ModelRef, Project, Shot, ShotRef, Task, Transition};
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
fn an_anchor_s_label_goes_with_it() {
    let (dir, p) = new_project();
    p.add_element(Kind::Cast, "Paka", None, "").unwrap();
    let img = dir.path().join("face.png");
    fs::write(&img, b"png").unwrap();
    let a = p.add_anchor("paka", &img).unwrap();
    let b = p.add_anchor("paka", &img).unwrap();

    let mut el = p.element("paka").unwrap();
    el.labels.insert("01.png".into(), "side view".into());
    el.labels.insert("02.png".into(), "red dress".into());
    p.save_element(&el).unwrap();
    let el = p.element("paka").unwrap();
    assert_eq!(el.label(&a), Some("side view"));

    p.remove_anchor("paka", &a).unwrap();
    assert!(!a.exists());
    let el = p.element("paka").unwrap();
    assert_eq!(el.label(&a), None);
    assert_eq!(el.label(&b), Some("red dress"));
}

#[test]
fn budgets_are_set_and_cleared() {
    let (dir, mut p) = new_project();
    assert_eq!(p.config.budget.per_day, Some(10.0));
    let b = shotline_core::ledger::Budgets {
        per_day: Some(30.0),
        ..Default::default()
    };
    p.set_budget(&b).unwrap();
    assert_eq!(p.config.budget.per_day, Some(30.0));
    assert_eq!(p.config.budget.per_take, None);
    p.set_budget(&Default::default()).unwrap();
    let text = fs::read_to_string(dir.path().join("shotline.toml")).unwrap();
    assert!(!text.contains("[budget]"), "{text}");
    let bad = shotline_core::ledger::Budgets {
        total: Some(-1.0),
        ..Default::default()
    };
    assert!(p.set_budget(&bad).is_err());

    let home = dir.path().join("home");
    shotline_core::UserConfig::set_per_day(&home, Some(40.0)).unwrap();
    let u = shotline_core::UserConfig::load(&home).unwrap();
    assert_eq!(u.budget.per_day, Some(40.0));
    shotline_core::UserConfig::set_per_day(&home, None).unwrap();
    let u = shotline_core::UserConfig::load(&home).unwrap();
    assert_eq!(u.budget.per_day, None);
}

#[test]
fn a_chapter_s_title_changes_but_not_its_folder() {
    let (_dir, p) = new_project();
    let ch = p.add_chapter("Into the Forest").unwrap();
    let r = p.add_shot("1", None, Shot::default()).unwrap();
    let c = p.rename_chapter("1", "  The woods ").unwrap();
    assert_eq!(c.title, "The woods");
    assert_eq!(c.dir, ch.dir);
    assert_eq!(p.chapter("01").unwrap().title, "The woods");
    assert!(p.shot(r).is_ok());
    assert!(p.rename_chapter("1", " ").is_err());
    assert!(p.rename_chapter("9", "Nowhere").is_err());
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

#[test]
fn lines_are_edited_and_removed() {
    let (_dir, p) = new_project();
    p.add_element(Kind::Cast, "Paka", None, "").unwrap();
    p.add_element(Kind::Set, "Forest", None, "").unwrap();
    p.add_chapter("Into the forest").unwrap();
    let r = p.add_shot("1", None, Shot::default()).unwrap();
    p.add_line(r, "paka", "hi", None).unwrap();
    p.add_line(r, "@paka", "bye", Some(2.0)).unwrap();

    let s = p.set_line(r, 0, "paka", "hello", Some(0.5)).unwrap();
    assert_eq!(
        (s.lines[0].text.as_str(), s.lines[0].at),
        ("hello", Some(0.5))
    );
    assert!(p.set_line(r, 0, "forest", "hi", None).is_err());
    assert!(p.set_line(r, 0, "paka", " ", None).is_err());
    assert!(p.set_line(r, 5, "paka", "hi", None).is_err());

    let s = p.remove_line(r, 0).unwrap();
    assert_eq!(s.lines.len(), 1);
    assert_eq!(p.shot(r).unwrap().lines[0].text, "bye");
    assert!(p.remove_line(r, 1).is_err());
}

#[test]
fn a_recording_made_elsewhere_is_one_of_the_speakers() {
    let (dir, p) = new_project();
    p.add_element(Kind::Cast, "Paka", None, "").unwrap();
    p.add_chapter("Into the forest").unwrap();
    let r = p.add_shot("1", None, Shot::default()).unwrap();
    let file = dir.path().join("Paka says hi.MP3");
    fs::write(&file, "mp3").unwrap();
    assert!(p.import_recording(r, "paka", " ", &file).is_err());
    assert!(
        p.import_recording(r, "paka", "hi", &dir.path().join("none.mp3"))
            .is_err()
    );

    let j = p.import_recording(r, "@paka", "hi", &file).unwrap();
    assert_eq!(j.outputs, vec!["out.mp3".to_string()]);
    assert_eq!(fs::read(p.job_dir(j.id).join("out.mp3")).unwrap(), b"mp3");
    assert_eq!(j.brief.as_deref(), Some("Paka says hi.MP3"));
    let all = p.recordings_by("paka", Some(r)).unwrap();
    assert_eq!(all.iter().map(|x| x.id).collect::<Vec<_>>(), vec![j.id]);

    p.add_line(r, "paka", "hi", None).unwrap();
    p.set_line_audio(r, 0, Some(&j)).unwrap();
    assert_eq!(p.line_audio(r, 0).unwrap().map(|x| x.id), Some(j.id));
}

#[test]
fn a_sound_goes_over_one_take() {
    let (dir, p) = new_project();
    p.add_chapter("Into the forest").unwrap();
    let r = p.add_shot("1", None, Shot::default()).unwrap();
    let video = dir.path().join("brought.mp4");
    fs::write(&video, "mp4").unwrap();
    let one = p.import_take(r, &video).unwrap();
    let two = p.import_take(r, &video).unwrap();
    let file = dir.path().join("Door.WAV");
    fs::write(&file, "wav").unwrap();
    assert!(
        p.add_sound((&one).into(), &dir.path().join("none.wav"), 0.0)
            .is_err()
    );
    assert!(p.add_sound((&one).into(), &file, -1.0).is_err());

    let j = p.add_sound((&one).into(), &file, 1.5).unwrap();
    assert_eq!(j.outputs, vec!["out.wav".to_string()]);
    assert_eq!(j.brief.as_deref(), Some("Door.WAV"));
    let got = p.take_sounds(&one).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!((got[0].0.at, got[0].1.id), (1.5, j.id));
    assert_eq!(fs::read(&got[0].2).unwrap(), b"wav");
    assert!(p.take_sounds(&two).unwrap().is_empty());

    p.remove_sound((&one).into(), &j.id.to_string()).unwrap();
    assert!(p.take_sounds(&one).unwrap().is_empty());
    assert!(p.job(&j.id.to_string()).unwrap().discarded);
    assert!(p.remove_sound((&one).into(), &j.id.to_string()).is_err());

    // A bridge's take keeps its sounds in the bridge.
    let b = p.new_bridge(&shotline_core::Bridge::default()).unwrap();
    let take = p.clock.ulid();
    let over = shotline_core::Under::Bridge(b, take);
    let k = p.add_sound(over, &file, 0.5).unwrap();
    assert_eq!(k.shot, None);
    assert_eq!(p.bridge(b).unwrap().sounds[0].take, take.to_string());
    assert_eq!(p.sounds(over).unwrap()[0].1.id, k.id);
    assert!(
        p.sounds(shotline_core::Under::Bridge(b, p.clock.ulid()))
            .unwrap()
            .is_empty()
    );
    p.remove_sound(over, &k.id.to_string()).unwrap();
    assert!(p.bridge(b).unwrap().sounds.is_empty());
}

#[test]
fn format_is_set_without_touching_the_rest() {
    let (dir, mut p) = new_project();
    let path = dir.path().join("shotline.toml");
    let text = fs::read_to_string(&path).unwrap() + "\n[rates]\nfake-credit = \"0.01 USD\"\n";
    fs::write(&path, text).unwrap();
    p.reread().unwrap();
    let f = Format {
        aspect: "16:9".into(),
        resolution: "1080P".into(),
        duration: 5.0,
        ..p.format()
    };
    p.set_format(&f).unwrap();
    assert_eq!(p.format(), f);
    assert_eq!(Project::open(dir.path()).unwrap().format(), f);
    assert_eq!(p.config.rates["fake-credit"], "0.01 USD");
    assert_eq!(p.config.budget.per_take, Some(2.0));

    for bad in [
        Format {
            duration: 0.0,
            ..f.clone()
        },
        Format {
            aspect: " ".into(),
            ..f.clone()
        },
    ] {
        assert!(p.set_format(&bad).is_err());
    }
    assert_eq!(p.format(), f);
}

#[test]
fn default_models_for_each_job() {
    let (dir, mut p) = new_project();
    let cfg = |p: &Project, t| p.config.model_for(t).map(|m| (m.plugin, m.model));
    // What a new film starts with.
    assert_eq!(cfg(&p, Task::Translate), cfg(&p, Task::Draft));
    assert_eq!(
        cfg(&p, Task::LipSync),
        Some(("wavespeed".into(), "sync/lipsync-3".into()))
    );
    assert_eq!(cfg(&p, Task::Voice), None);
    assert_eq!(cfg(&p, Task::Assistant), None);

    let m = |a: &str, b: &str| ModelRef::new(a, b);
    p.set_model(Task::Video, Some(&m("venice", "h3"))).unwrap();
    p.set_model(Task::Voice, Some(&m("elevenlabs", "eleven_v3")))
        .unwrap();
    p.set_model(Task::LipSync, Some(&m("other", "sync-2")))
        .unwrap();
    p.set_model(Task::Image, Some(&m("venice", "seedream-v4")))
        .unwrap();
    assert_eq!(
        (p.config.plugin.as_str(), p.config.model.as_str()),
        ("venice", "h3")
    );
    assert_eq!(p.config.image.model, "seedream-v4");
    let text = fs::read_to_string(dir.path().join("shotline.toml")).unwrap();
    assert!(text.contains("[tts]"), "{text}");

    // The built-in lip-synced recipe uses the film's.
    let s = Shot {
        recipe: Some(shotline_core::LIP_SYNCED.into()),
        ..Shot::default()
    };
    let sync = p.steps(&s).unwrap().pop().unwrap();
    assert_eq!(sync.model.as_deref(), Some("sync-2"));

    // Some can go back to unset; video and images can't.
    p.set_model(Task::Voice, None).unwrap();
    p.set_model(Task::LipSync, None).unwrap();
    assert_eq!(cfg(&p, Task::Voice), None);
    assert_eq!(cfg(&p, Task::LipSync).unwrap().1, "sync/lipsync-3");
    assert!(p.set_model(Task::Video, None).is_err());
    assert!(p.set_model(Task::Image, Some(&m("", "x"))).is_err());
    assert_eq!(p.config.name, "Forest");
}

#[test]
fn a_model_s_options_for_the_whole_film() {
    let (dir, mut p) = new_project();
    const H3: &str = "minimax/h3-max/reference-to-video";
    p.set_param("fal", H3, "enable_safety_checker", Some(false.into()))
        .unwrap();
    p.set_param("fal", H3, "prompt_expansion_mode", Some("quality".into()))
        .unwrap();
    let text = fs::read_to_string(dir.path().join("shotline.toml")).unwrap();
    assert!(
        text.contains("[params.fal.\"minimax/h3-max/reference-to-video\"]"),
        "{text}"
    );
    assert_eq!(p.config.name, "Forest");

    // The shot's own wins.
    let mut over = toml::Table::new();
    over.insert("prompt_expansion_mode".into(), "disabled".into());
    let t = p.config.params_for("fal", H3, &over);
    assert_eq!(t["enable_safety_checker"].as_bool(), Some(false));
    assert_eq!(t["prompt_expansion_mode"].as_str(), Some("disabled"));
    assert!(
        p.config
            .params_for("venice", H3, &toml::Table::new())
            .is_empty()
    );

    // Taken out, no empty tables stay behind.
    p.set_param("fal", H3, "enable_safety_checker", None)
        .unwrap();
    p.set_param("fal", H3, "prompt_expansion_mode", None)
        .unwrap();
    assert!(p.config.params.is_empty());
    let text = fs::read_to_string(dir.path().join("shotline.toml")).unwrap();
    assert!(!text.contains("params"), "{text}");
}

#[test]
fn a_replaced_picture_keeps_its_place_and_note() {
    let (dir, p) = new_project();
    p.add_chapter("Night").unwrap();
    let r = p.add_shot("1", None, Shot::default()).unwrap();
    let png = |name: &str, bytes: &[u8]| {
        let f = dir.path().join(name);
        fs::write(&f, bytes).unwrap();
        f
    };
    p.add_shot_image(r, &png("a.png", b"a"), "the door")
        .unwrap();
    p.add_shot_image(r, &png("b.png", b"b"), "the street")
        .unwrap();
    let old = p.root.join(&p.shot(r).unwrap().images[0].file);

    let new = p.replace_shot_image(r, 0, &png("c.png", b"c")).unwrap();
    let s = p.shot(r).unwrap();
    assert_eq!(s.images.len(), 2);
    assert_eq!(s.images[0].note, "the door");
    assert_eq!(p.root.join(&s.images[0].file), new);
    assert_eq!(fs::read(&new).unwrap(), b"c");
    assert_eq!(s.images[1].note, "the street");
    assert!(!old.exists());
    assert!(p.replace_shot_image(r, 5, &new).is_err());
}
