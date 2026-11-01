use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use eframe::egui::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use shotline_core::ledger::Context;
use shotline_core::protocol::{Description, ModelInfo, Op, Quote, VoiceInfo};
use shotline_core::{Git, Job, JobStatus, Kind, Project, Shot, ShotRef, Transition, UserConfig};
use shotline_ui::{AnchorAsk, App, Host, MenuCommand, Shell, Tab};

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
    Harness::builder().build_ui_state(|ui, app: &mut App| app.show(ui), app)
}

fn field(h: &Harness<'static, App>, role: Role, label: &str) -> String {
    h.get_by_role_and_label(role, label)
        .value()
        .unwrap_or_default()
}

const R: ShotRef = ShotRef {
    chapter: 1,
    shot: 10,
};

#[test]
fn edit_a_shot() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = harness(tmp.path());
    h.run();
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    let p = Project::open(&tmp.path().join("film")).unwrap();

    // Nothing is written while typing, only once the box is left.
    let plot = h.get_by_role_and_label(Role::MultilineTextInput, "Plot");
    plot.focus();
    plot.type_text(" ตอนเย็น");
    h.run();
    assert_eq!(p.shot(R).unwrap().plot, "ผกาเดินเข้าป่า");
    h.key_press(egui::Key::Escape);
    h.run();
    h.run();
    assert_eq!(p.shot(R).unwrap().plot, "ผกาเดินเข้าป่า ตอนเย็น");
    h.get_by_label("01-010 saved");

    h.get_by_label("from the shot before").click();
    h.run();
    h.run();
    assert_eq!(p.shot(R).unwrap().transition, Transition::Continue);

    let line = h.get_by_role_and_label(Role::TextInput, "Lines");
    line.focus();
    line.type_text("ไปกันเถอะ");
    h.run();
    h.get_by_label("Add line").click();
    h.run();
    h.run();
    let s = p.shot(R).unwrap();
    assert_eq!(
        (s.lines[0].who.as_str(), s.lines[0].text.as_str()),
        ("paka", "ไปกันเถอะ")
    );
    assert_eq!(field(&h, Role::TextInput, "Lines"), "");

    h.get_by_label("Remove").click();
    h.run();
    h.run();
    assert!(p.shot(R).unwrap().lines.is_empty());
    h.get_by_label("line 1 of 01-010 removed");
}

#[test]
fn edit_the_story_and_add_shots() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = harness(tmp.path());
    h.run();
    let p = Project::open(&tmp.path().join("film")).unwrap();
    let story = std::fs::read_to_string(p.root.join("story.md")).unwrap();

    h.get_by_label("Story").click();
    h.run();
    let style = h.get_by_role_and_label(Role::MultilineTextInput, "Style");
    style.focus();
    style.type_text("Rainy greens, handheld.");
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    h.run();
    assert!(p.style().unwrap().ends_with("Rainy greens, handheld."));
    assert_eq!(
        std::fs::read_to_string(p.root.join("story.md")).unwrap(),
        story
    );
    h.get_by_label("style saved");

    h.get_by_label("Shots").click();
    h.run();
    let title = h.get_by_role_and_label(Role::TextInput, "New chapter");
    title.focus();
    title.type_text("The river");
    h.run();
    h.get_by_label("Add chapter").click();
    h.run();
    h.run();
    assert_eq!(p.chapter("2").unwrap().title, "The river");
    let adds = h.query_all_by_label("Add shot").count();
    assert_eq!(adds, 2);

    // A shot after 01-010, then one at the end of chapter 2.
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    h.get_by_label("Add a shot after this one").click();
    h.run();
    h.run();
    let new = ShotRef {
        chapter: 1,
        shot: 20,
    };
    assert!(p.shot(new).is_ok());
    h.get_by_label("added 01-020");
    // And it's open, empty.
    assert_eq!(field(&h, Role::MultilineTextInput, "Plot"), "");
    h.query_all_by_label("Add shot").nth(1).unwrap().click();
    h.run();
    h.run();
    let end = ShotRef {
        chapter: 2,
        shot: 10,
    };
    assert!(p.shot(end).is_ok());

    // Undo takes them back one by one, chapter folder and all.
    h.get_by_label("Undo").click();
    h.run();
    assert!(p.shot(end).is_err());
    h.get_by_label("Undo").click();
    h.run();
    assert!(p.shot(new).is_err());
    h.get_by_label("Undo").click();
    h.run();
    assert!(p.chapter("2").is_err());
    assert_eq!(
        std::fs::read_dir(p.root.join("chapters")).unwrap().count(),
        1
    );
    h.get_by_label("Undo").click();
    h.run();
    assert_eq!(p.style().unwrap(), "");

    h.get_by_label("Redo").click();
    h.run();
    h.get_by_label("Redo").click();
    h.run();
    assert_eq!(p.chapter("2").unwrap().title, "The river");
}

/// Plugins as the app sees them: elevenlabs has two voices and 3.5 USD left.
struct TestHost;

impl Host for TestHost {
    fn voices(&self, plugin: &str) -> anyhow::Result<Vec<VoiceInfo>> {
        assert_eq!(plugin, "elevenlabs");
        let v = |id: &str, name: &str| VoiceInfo {
            id: id.into(),
            name: name.into(),
            ..VoiceInfo::default()
        };
        Ok(vec![v("abc", "Rachel"), v("def", "Somchai")])
    }

    fn balance(&self, plugin: &str) -> anyhow::Result<String> {
        assert_eq!(plugin, "elevenlabs");
        Ok("3.5 USD left".into())
    }

    fn anchor_quote(&self, _film: &Path, ask: &AnchorAsk) -> anyhow::Result<String> {
        assert_eq!(
            (ask.plugin.as_str(), ask.model.as_str()),
            ("fal", "fal-ai/nano-banana")
        );
        Ok("$0.04".into())
    }

    fn fetch_image(&self, film: &Path, url: &str) -> anyhow::Result<PathBuf> {
        assert_eq!(url, "https://example.com/dress.png");
        let out = film.join(".shotline").join("dress.png");
        image::RgbImage::new(8, 8).save(&out)?;
        Ok(out)
    }

    fn make_anchor(&self, film: &Path, ask: &AnchorAsk, at: &str) -> anyhow::Result<PathBuf> {
        assert_eq!(
            (ask.handle.as_str(), ask.prompt.as_str(), at),
            ("paka", "side view", "$0.04")
        );
        assert_eq!(ask.refs, [film.join(".shotline").join("dress.png")]);
        let out = film.join(".shotline").join("made.png");
        image::RgbImage::new(8, 8).save(&out)?;
        Ok(out)
    }
}

fn anchors(p: &Project) -> Vec<PathBuf> {
    p.anchors(&p.element("paka").unwrap()).unwrap()
}

#[test]
fn make_an_anchor_image() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .silent();
    let model = |id: &str| ModelInfo {
        id: id.into(),
        op: Op::ImageGenerate,
        ref_image: Some("Image {n}".into()),
        max_images: Some(2),
        ..ModelInfo::default()
    };
    app.set_plugins(
        vec![Description {
            name: "fal".into(),
            version: "1".into(),
            protocol: 1,
            env: vec![],
            models: vec![model("fal-ai/flux"), model("fal-ai/nano-banana")],
        }],
        Some(Arc::new(TestHost)),
    );
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 1000.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label("Elements").click();
    h.run();
    h.get_by_label_contains("@paka").click();
    h.run();
    h.get_by_label("Make image…").click();
    h.run();
    h.get_by_role_and_label(Role::ComboBox, "Image model")
        .click();
    h.run();
    h.get_by_label("fal · fal-ai/nano-banana").click();
    h.run();
    let prompt = h.get_by_role_and_label(Role::TextInput, "Image prompt");
    prompt.focus();
    prompt.type_text("side view");
    h.run();
    // Paka has no anchors yet, so a picture from the web goes along instead.
    h.get_by_label("None: the image comes from the words alone.");
    let url = h.get_by_role_and_label(Role::TextInput, "Image address");
    url.focus();
    url.type_text("https://example.com/dress.png");
    h.run();
    h.get_by_label("Fetch").click();
    assert!(wait_for(&mut h, |h| h
        .query_all_by_label("Image 1")
        .count()
        > 0));
    h.get_by_label("Get price").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make it for $0.04")
        .is_some()));
    assert!(anchors(&p).is_empty());
    h.get_by_label("Make it for $0.04").click();
    assert!(wait_for(&mut h, |_| !anchors(&p).is_empty()));
    h.run();
    assert_eq!(anchors(&p).len(), 1);
    // Undone like an image added from a file.
    h.get_by_label("Undo").click();
    h.run();
    assert!(anchors(&p).is_empty());
}

#[test]
fn keys_in_settings() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    // A name nobody has set, so the file is what counts.
    let var = "SHOTLINE_TEST_ELEVENLABS_KEY";
    let plugins = vec![Description {
        name: "elevenlabs".into(),
        version: "1".into(),
        protocol: 1,
        env: vec![var.into()],
        models: vec![],
    }];
    let shell = Shell::new(None, home.clone(), tmp.path())
        .with_plugins(plugins, Some(Arc::new(TestHost)))
        .testing();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 800.0))
        .build_ui_state(|ui, s: &mut Shell| s.show(ui), shell);
    h.run();
    h.state_mut().command(MenuCommand::Settings);
    h.run();
    h.get_by_label("not set");
    // Nothing to check with yet.
    h.get_by_label("Check").click();
    h.run();
    assert!(h.query_by_label_contains("works").is_none());

    let field = h.get_by_role_and_label(Role::TextInput, var);
    field.focus();
    field.type_text("sk-123");
    h.run();
    h.get_by_label("Save").click();
    h.run();
    let env = home.join(".env");
    assert_eq!(
        std::fs::read_to_string(&env).unwrap(),
        format!("{var}=sk-123\n")
    );
    h.get_by_label("in .env");

    h.get_by_label("Check").click();
    assert!(wait_for_shell(&mut h, |h| h
        .query_by_label("works: 3.5 USD left")
        .is_some()));

    h.get_by_label("Remove").click();
    h.run();
    assert_eq!(std::fs::read_to_string(&env).unwrap(), "\n");
    h.get_by_label("not set");
    h.get_by_label("Close").click();
    h.run();
    assert!(h.query_by_label("Keys").is_none());
}

fn wait_for_shell(h: &mut Harness<Shell>, done: impl Fn(&Harness<Shell>) -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if done(h) {
            return true;
        }
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    done(h)
}

#[test]
fn edit_elements() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let png = tmp.path().join("a.png");
    image::RgbImage::new(8, 8).save(&png).unwrap();
    let anchor = p.add_anchor("paka", &png).unwrap();
    let bytes = std::fs::read(&anchor).unwrap();
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .silent();
    let tts = |id: &str| ModelInfo {
        id: id.into(),
        op: Op::AudioTts,
        voice_schema: Some(serde_json::json!({"type": "object", "properties": {
            "stability": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.5},
            "boost": {"type": "boolean", "default": false},
            "mood": {"type": "string", "enum": ["calm", "angry"]}
        }})),
        ..ModelInfo::default()
    };
    let voices: Arc<dyn Host> = Arc::new(TestHost);
    app.set_plugins(
        vec![Description {
            name: "elevenlabs".into(),
            version: "1".into(),
            protocol: 1,
            env: vec![],
            models: vec![tts("eleven_v3"), tts("eleven_flash_v2_5")],
        }],
        Some(voices),
    );
    // Tall enough for the whole voice form without scrolling.
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 1100.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label("Elements").click();
    h.run();

    let name = h.get_by_role_and_label(Role::TextInput, "New element");
    name.focus();
    name.type_text("Mali");
    h.run();
    h.get_by_label("Add element").click();
    h.run();
    h.run();
    assert_eq!(p.element("mali").unwrap().name, "Mali");
    h.get_by_label("added @mali");

    // It opens; what's typed is saved once the box is left.
    let desc = h.get_by_role_and_label(Role::MultilineTextInput, "Description");
    desc.focus();
    desc.type_text("young woman, short black hair");
    h.run();
    h.get_by_role_and_label(Role::ComboBox, "Plugin").click();
    h.run();
    h.get_by_label("elevenlabs").click();
    h.run();
    h.get_by_role_and_label(Role::ComboBox, "Model").click();
    h.run();
    h.get_by_label("eleven_v3").click();
    h.run();
    // The plugin is asked for its voices in the background.
    assert!(wait_for(&mut h, |h| h
        .query_by_role_and_label(Role::ComboBox, "Voice")
        .is_some()));
    h.get_by_role_and_label(Role::ComboBox, "Voice").click();
    h.run();
    h.get_by_label("Somchai").click();
    h.run();
    // Settings as the model describes them.
    h.get_by_role_and_label(Role::CheckBox, "boost").click();
    h.run();
    h.get_by_role_and_label(Role::ComboBox, "mood").click();
    h.run();
    h.get_by_label("angry").click();
    h.run();
    h.run();
    let mali = p.element("mali").unwrap();
    assert_eq!(mali.description, "young woman, short black hair");
    let voice = mali.voice.unwrap();
    assert_eq!(voice.plugin, "elevenlabs");
    assert_eq!(voice.model.as_deref(), Some("eleven_v3"));
    assert_eq!(voice.voice_id.as_deref(), Some("def"));
    assert_eq!(voice.settings["boost"].as_bool(), Some(true));
    assert_eq!(voice.settings["mood"].as_str(), Some("angry"));
    // Left alone, it isn't written: the provider's default applies.
    assert!(!voice.settings.contains_key("stability"));

    // Clicking an anchor shows it big.
    h.get_by_label("@paka  Paka").click();
    h.run();
    h.get_by_label("elements/cast/paka/01.png").click();
    h.run();
    h.get_by_label("elements/cast/paka/01.png big");
    h.get_by_label("Close").click();
    h.run();
    assert!(h.query_by_label("elements/cast/paka/01.png big").is_none());

    // Taking an anchor off can be undone.
    h.get_by_label("Remove").click();
    h.run();
    assert!(!anchor.exists());
    h.get_by_label("Undo").click();
    h.run();
    assert_eq!(std::fs::read(&anchor).unwrap(), bytes);

    // And so can adding the element, after the edits to it.
    while h.query_by_label("Undo").is_some() {
        h.get_by_label("Undo").click();
        h.run();
    }
    assert!(p.find_element("mali").unwrap().is_none());
}

#[test]
fn undo_and_redo() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = harness(tmp.path());
    h.run();
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    let p = Project::open(&tmp.path().join("film")).unwrap();

    // Two fields saved at once come back as one step.
    let plot = h.get_by_role_and_label(Role::MultilineTextInput, "Plot");
    plot.focus();
    plot.type_text("!");
    h.run();
    let prompt = h.get_by_role_and_label(Role::MultilineTextInput, "Prompt");
    prompt.focus();
    prompt.type_text("!");
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    h.run();
    let edited = p.shot(R).unwrap();
    assert_eq!(edited.plot, "ผกาเดินเข้าป่า!");
    assert_eq!(edited.prompt, "@paka walks into the trees!");

    h.get_by_label("Undo").click();
    h.run();
    h.run();
    let s = p.shot(R).unwrap();
    assert_eq!(s.plot, "ผกาเดินเข้าป่า");
    assert_eq!(s.prompt, "@paka walks into the trees");
    assert_eq!(field(&h, Role::MultilineTextInput, "Plot"), "ผกาเดินเข้าป่า");
    h.get_by_label("undid the change to 01-010");
    assert!(h.query_by_label("Undo").is_none());

    h.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        egui::Key::Z,
    );
    h.run();
    h.run();
    assert_eq!(p.shot(R).unwrap(), edited);
    h.get_by_label("redid the change to 01-010");

    h.get_by_label("Select #1").click();
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    h.run();
    assert_eq!(p.shot(R).unwrap(), edited);

    // Changed by something else since: undo leaves it alone.
    let mut s = edited.clone();
    s.title = "Paka runs".into();
    p.save_shot(R, &s).unwrap();
    h.get_by_label("Undo").click();
    h.run();
    h.run();
    assert_eq!(p.shot(R).unwrap(), s);
    h.get_by_label("01-010 was changed outside the app since");
}

#[test]
fn keeps_history_with_git() {
    let Some(git) = Git::find() else {
        eprintln!("skipped: no git");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let app = App::new(film(tmp.path()), tmp.path().join("home"))
        .with_viewer(None)
        .with_git(Git::find().map(|g| g.with_identity("Test", "test@example.com")))
        .silent();
    let mut h = Harness::builder().build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    let dir = tmp.path().join("film");

    h.get_by_label("Keep history with git").click();
    h.run();
    h.get_by_label("Start history").click();
    h.run();
    h.get_by_label_contains("git history started at");
    assert!(git.is_repo(&dir));
    assert!(h.query_by_label_contains("Commit").is_none());

    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    let plot = h.get_by_role_and_label(Role::MultilineTextInput, "Plot");
    plot.focus();
    plot.type_text("!");
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    h.run();
    h.get_by_label("Undo");
    h.get_by_label("Commit 1 change").click();
    h.run();
    assert_eq!(field(&h, Role::TextInput, "Message"), "edit 01-010");
    h.get_by_label("Commit").click();
    h.run();
    h.run();
    h.get_by_label_contains(": edit 01-010");
    assert!(git.changes(&dir).unwrap().is_empty());
    // Committed is the new starting point.
    assert!(h.query_by_label("Undo").is_none());
    assert!(h.query_by_label_contains("Commit").is_none());
}

#[test]
fn start_a_film() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let git = Git::find().map(|g| g.with_identity("Test", "test@example.com"));
    let shell = Shell::new(None, home.clone(), tmp.path())
        .with_git(git.clone())
        .testing();
    let mut h = Harness::builder().build_ui_state(|ui, s: &mut Shell| s.show(ui), shell);
    h.run();
    let name = h.get_by_role_and_label(Role::TextInput, "Name");
    name.focus();
    name.type_text("Rain");
    h.run();
    let dir = tmp.path().join("rain");
    h.get_by_label(&format!("Makes {}", dir.display()));

    // Not into a folder that already has something in it.
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "").unwrap();
    h.run();
    h.get_by_label_contains("already has files in it");
    h.get_by_label("Create film").click();
    h.run();
    assert!(!dir.join("shotline.toml").exists());
    std::fs::remove_file(dir.join("notes.txt")).unwrap();
    h.run();
    h.get_by_label("Create film").click();
    h.run();
    h.run();
    assert_eq!(Project::open(&dir).unwrap().config.name, "Rain");
    assert!(h.state().film().is_some());
    h.get_by_label("Shots");
    if let Some(g) = &git {
        assert!(g.is_repo(&dir));
        h.get_by_label_contains("git history started at");
    }

    // The next time it's in the list.
    let shell = Shell::new(None, home, tmp.path()).testing();
    let mut h = Harness::builder().build_ui_state(|ui, s: &mut Shell| s.show(ui), shell);
    h.run();
    h.get_by_label_contains("Rain  ").click();
    h.run();
    assert!(h.state().film().is_some());
}

#[test]
fn file_and_edit_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let shell = Shell::new(Some(p.clone()), tmp.path().join("home"), tmp.path())
        .with_git(None)
        .testing();
    let mut h = Harness::builder().build_ui_state(|ui, s: &mut Shell| s.show(ui), shell);
    h.run();

    // Edit > Undo takes back the last save.
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    let plot = h.get_by_role_and_label(Role::MultilineTextInput, "Plot");
    plot.focus();
    plot.type_text("!");
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    h.state_mut().command(MenuCommand::Undo);
    h.run();
    assert_eq!(p.shot(R).unwrap().plot, "ผกาเดินเข้าป่า");

    // File > New Film over the open one, given up, then made.
    // The shortcut works without the menu too, as on Windows and Linux.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.run();
    h.get_by_label("Cancel").click();
    h.run();
    assert!(h.query_by_label("Create film").is_none());
    assert_eq!(h.state().film().unwrap().project().config.name, "Rain");
    h.state_mut().command(MenuCommand::NewFilm);
    h.run();
    let name = h.get_by_role_and_label(Role::TextInput, "Name");
    name.focus();
    name.type_text("Sun");
    h.run();
    // Next to the film that was open.
    h.get_by_label(&format!("Makes {}", tmp.path().join("sun").display()));
    h.get_by_label("Create film").click();
    h.run();
    h.run();
    assert_eq!(h.state().film().unwrap().project().config.name, "Sun");

    h.state_mut().command(MenuCommand::CloseFilm);
    h.run();
    assert!(h.state().film().is_none());
    h.get_by_label_contains("Sun  ");
    h.get_by_label_contains("Rain  ");
}

#[test]
fn menu_in_the_window() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let shell = Shell::new(Some(p.clone()), tmp.path().join("home"), tmp.path())
        .with_git(None)
        .with_window_menu()
        .testing();
    let mut h = Harness::builder().build_ui_state(|ui, s: &mut Shell| s.show(ui), shell);
    h.run();
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();

    // Select All reaches the box being typed in.
    let plot = h.get_by_role_and_label(Role::MultilineTextInput, "Plot");
    plot.focus();
    h.run();
    h.get_by_label("Edit").click();
    h.run();
    h.get_by_label_contains("Select All").click();
    h.run();
    h.get_by_role_and_label(Role::MultilineTextInput, "Plot")
        .type_text("rain");
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    h.run();
    assert_eq!(p.shot(R).unwrap().plot, "rain");

    h.get_by_label("Edit").click();
    h.run();
    h.get_by_label_contains("Ctrl+Z").click();
    h.run();
    assert_eq!(p.shot(R).unwrap().plot, "ผกาเดินเข้าป่า");

    h.get_by_label("File").click();
    h.run();
    h.get_by_label_contains("Close Film").click();
    h.run();
    assert!(h.state().film().is_none());
    h.get_by_label("File").click();
    h.run();
    h.get_by_label("Open Recent").click();
    h.run();
    h.get_by_role_and_label(Role::Button, "Rain").click();
    h.run();
    assert!(h.state().film().is_some());
}

#[test]
fn browse_and_select_a_take() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = harness(tmp.path());
    h.run();
    assert_eq!(h.state().tab(), Tab::Shots);
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    assert_eq!(field(&h, Role::MultilineTextInput, "Plot"), "ผกาเดินเข้าป่า");
    h.get_by_label("failed");
    h.get_by_label("content policy");

    h.get_by_label("Select #1").click();
    h.run();
    h.get_by_label("01-010#1 selected");
    h.get_by_label("• #1");
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

/// Steps the app until `done`, for up to five seconds: slow CI machines take a while to start ffmpeg.
fn wait_for(h: &mut Harness<App>, done: impl Fn(&Harness<App>) -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if done(h) {
            return true;
        }
        h.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    done(h)
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
    assert!(wait_for(&mut h, |h| h.state().is_playing()));
    // The title, and the picture's alt text.
    assert!(wait_for(&mut h, |h| h
        .query_all_by_label("01-010#1")
        .count()
        == 2));
    h.get_by_label("Pause");

    h.get_by_label("Compare #3").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("01-010#1 vs 01-010#3")
        .is_some()));
    assert!(h.state().is_playing());
    h.get_by_label("Pause").click();
    assert!(wait_for(&mut h, |h| !h.state().is_playing()));

    h.get_by_label("Cut").click();
    wait(&mut h, 0.1);
    h.get_by_label("Play the cut").click();
    wait(&mut h, 0.3);
    assert!(wait_for(&mut h, |h| h
        .query_all_by_label("the cut")
        .count()
        == 2));
    assert!(h.state().is_playing());
    // The cut doesn't loop: it stops after its one second.
    wait(&mut h, 1.0);
    assert!(wait_for(&mut h, |h| !h.state().is_playing()));
    h.get_by_label("Close player").click();
    assert!(wait_for(&mut h, |h| h
        .query_all_by_label("the cut")
        .count()
        == 0));
}

#[test]
fn elements_story_and_cost() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = harness(tmp.path());
    h.run();
    h.get_by_label("Elements").click();
    h.run();
    h.get_by_label("@paka  Paka").click();
    h.run();
    assert_eq!(
        field(&h, Role::MultilineTextInput, "Description"),
        "team lead, 30s"
    );

    h.get_by_label("Story").click();
    h.run();
    assert!(field(&h, Role::MultilineTextInput, "Story").contains("## Logline"));

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
    assert_eq!(field(&h, Role::MultilineTextInput, "Plot"), "ผกาเดินเข้าป่า");
}
