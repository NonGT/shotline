use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use eframe::egui::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use shotline_core::ledger::Context;
use shotline_core::protocol::{Description, ModelInfo, Op, Quote, VoiceInfo};
use shotline_core::{Git, Job, JobStatus, Kind, Project, Shot, ShotRef, Transition, UserConfig};
use shotline_ui::{AnchorAsk, App, Host, MenuCommand, PluginInfo, Runs, Shell, Tab};

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
    // Tall enough for a shot's takes, under its references.
    Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 1200.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app)
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
    // Wide enough for the shot's settings column to be all on screen.
    h.set_size(egui::vec2(1100.0, 800.0));
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
    h.get_by_label("Copy the message").click();
    h.run();

    // The first shot has nothing to continue from.
    assert!(
        h.get_by_label("Continue from previous shot")
            .accesskit_node()
            .is_disabled()
    );

    // Lines show once they're to be spoken.
    assert!(h.query_by_label("Add line").is_none());
    h.get_by_role_and_label(Role::ComboBox, "Lines spoken")
        .click();
    h.run();
    h.get_by_label("by the video model").click();
    h.run();
    h.run();
    h.get_by_label("Add line").click();
    h.run();
    // Nothing to record yet.
    assert!(h.get_by_label("Record…").accesskit_node().is_disabled());
    let line = h.get_by_role_and_label(Role::TextInput, "Lines");
    line.focus();
    line.type_text("ไปกันเถอะ");
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    h.run();
    let s = p.shot(R).unwrap();
    assert_eq!(
        (s.lines[0].who.as_str(), s.lines[0].text.as_str()),
        ("paka", "ไปกันเถอะ")
    );
    assert!(
        h.query_by_role_and_label(Role::TextInput, "Lines")
            .is_none()
    );
    assert_eq!(field(&h, Role::TextInput, "Line 1"), "ไปกันเถอะ");

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

    // Its title changed from the right-click menu; Escape leaves it as it was.
    h.get_by_label("02 The river").click_secondary();
    h.run();
    h.get_by_label("Rename chapter").click();
    h.run();
    h.get_by_role_and_label(Role::TextInput, "Chapter title")
        .type_text(" bank");
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    assert_eq!(p.chapter("2").unwrap().title, "The river");
    h.get_by_label("02 The river").click_secondary();
    h.run();
    h.get_by_label("Rename chapter").click();
    h.run();
    h.get_by_role_and_label(Role::TextInput, "Chapter title")
        .type_text(" bank");
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    h.run();
    assert_eq!(p.chapter("2").unwrap().title, "The river bank");
    h.get_by_label("02 The river bank");
    h.get_by_label("Undo").click();
    h.run();
    h.run();
    assert_eq!(p.chapter("2").unwrap().title, "The river");

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

/// What TestHost was asked to pin.
static PINNED: std::sync::Mutex<Vec<(String, PathBuf)>> = std::sync::Mutex::new(vec![]);

impl Host for TestHost {
    /// fal built in with h3's options; venice on PATH with an older copy further along.
    fn plugins(&self) -> (Vec<PluginInfo>, Vec<String>) {
        let h3 = ModelInfo {
            id: "minimax/h3-max/reference-to-video".into(),
            op: Op::VideoGenerate,
            params_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "enable_safety_checker": {"type": "boolean", "default": true, "title": "Safety checker"},
                    "prompt_expansion_mode": {
                        "type": "string", "enum": ["disabled", "balanced", "quality"],
                        "default": "disabled", "title": "Prompt rewriting"
                    }
                }
            })),
            price: Some("$0.05 a second".into()),
            ..ModelInfo::default()
        };
        let haiku = ModelInfo {
            id: "claude-haiku-5-5".into(),
            op: Op::TextGenerate,
            price: Some("per token".into()),
            ..ModelInfo::default()
        };
        let d = |name: &str, models| Description {
            name: name.into(),
            version: "1".into(),
            protocol: 1,
            env: vec![],
            models,
        };
        let pinned = PINNED.lock().unwrap().last().cloned();
        let venice = match pinned {
            Some((_, e)) => (Runs::Pinned(e), vec![]),
            None => (
                Runs::Path("/a/shotline-plugin-venice".into()),
                vec![PathBuf::from("/b/shotline-plugin-venice")],
            ),
        };
        (
            vec![
                PluginInfo {
                    name: "fal".into(),
                    runs: Runs::Builtin,
                    skipped: vec![],
                    description: Ok(d("fal", vec![h3])),
                    missing: vec!["FAL_API_KEY".into()],
                },
                PluginInfo {
                    name: "venice".into(),
                    runs: venice.0,
                    skipped: venice.1,
                    description: Ok(d("venice", vec![haiku])),
                    missing: vec![],
                },
                PluginInfo {
                    name: "elevenlabs".into(),
                    runs: Runs::Path("/a/shotline-plugin-elevenlabs".into()),
                    skipped: vec![],
                    description: Ok(Description {
                        env: vec![ELEVENLABS_KEY.into()],
                        ..d("elevenlabs", vec![])
                    }),
                    missing: vec![],
                },
            ],
            vec![],
        )
    }

    fn pin(&self, name: &str, exec: &Path) -> anyhow::Result<String> {
        PINNED.lock().unwrap().push((name.into(), exec.into()));
        Ok(format!("{name} kept"))
    }

    fn unpin(&self, name: &str) -> anyhow::Result<String> {
        PINNED.lock().unwrap().clear();
        Ok(format!("{name} back to PATH"))
    }

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

    fn say_quote(&self, _film: &Path, shot: ShotRef, line: usize) -> anyhow::Result<String> {
        assert_eq!((shot, line), (R, 0));
        Ok("0.02 USD".into())
    }

    /// A recording as the CLI leaves one: a done tts job of the line's words, picked.
    fn say(&self, film: &Path, shot: ShotRef, line: usize, at: &str) -> anyhow::Result<PathBuf> {
        assert_eq!(at, "0.02 USD");
        let p = Project::open(film)?;
        let mut j = Job::new(Op::AudioTts, "fake", "fake/tts");
        j.shot = Some(shot);
        j.who = Some(p.shot(shot)?.lines[line].who.clone());
        j.prompt = Some(p.shot(shot)?.lines[line].text.clone());
        let mut j = p.create_job(j)?;
        let out = p.job_dir(j.id).join("out.wav");
        std::fs::write(&out, "wav")?;
        j.status = JobStatus::Done;
        j.outputs = vec!["out.wav".into()];
        p.save_job(&j)?;
        p.set_line_audio(shot, line, Some(&j))?;
        Ok(out)
    }

    fn take_quote(&self, _film: &Path, shot: ShotRef, translate: bool) -> anyhow::Result<String> {
        assert_eq!(shot, R);
        Ok(if translate {
            "0.40 USD + 0.01 USD to translate".into()
        } else {
            "0.40 USD".into()
        })
    }

    /// A take as the engine leaves one: a done job of the shot with a video.
    fn take(
        &self,
        film: &Path,
        shot: ShotRef,
        at: &str,
        translate: bool,
        on: &mut dyn FnMut(String),
    ) -> anyhow::Result<String> {
        assert_eq!((shot, at), (R, "0.40 USD"));
        assert!(!translate);
        on("running (40%)".into());
        let p = Project::open(film)?;
        let mut j = Job::new(
            Op::VideoGenerate,
            "fal",
            "minimax/h3-max/reference-to-video",
        );
        j.shot = Some(shot);
        j.take = Some(p.new_take_id());
        let mut j = p.create_job(j)?;
        std::fs::write(p.job_dir(j.id).join("out.mp4"), "mp4")?;
        j.outputs = vec!["out.mp4".into()];
        j.status = JobStatus::Done;
        p.save_job(&j)?;
        Ok("01-010#3 done · 0.40 USD".into())
    }

    fn anchor_quote(&self, _film: &Path, ask: &AnchorAsk) -> anyhow::Result<String> {
        assert_eq!(
            (ask.plugin.as_str(), ask.model.as_str()),
            ("fal", "fal-ai/nano-banana")
        );
        Ok("$0.04".into())
    }

    fn render(&self, film: &Path, on: &mut dyn FnMut(String)) -> anyhow::Result<(PathBuf, String)> {
        let t = Project::open(film)?.timeline()?;
        assert_eq!(t.colormatch.at, shotline_core::MatchAt::All);
        on("1/1 01-010#1".into());
        let out = film.join("renders").join("cut.mp4");
        std::fs::create_dir_all(out.parent().unwrap())?;
        std::fs::write(&out, "")?;
        Ok((out, "8s · 720x1280".into()))
    }

    fn rates(&self, _: &Path) -> anyhow::Result<Vec<shotline_core::ledger::RateLine>> {
        Ok(vec![shotline_core::ledger::RateLine {
            unit: "fake-credit".into(),
            rate: 0.01,
            currency: "USD".into(),
            from: "plugin fake".into(),
            used: true,
        }])
    }

    fn reconcile(&self, _: &Path) -> anyhow::Result<Vec<shotline_core::ledger::reconcile::Check>> {
        Ok(vec![shotline_core::ledger::reconcile::Check {
            title: "fake:".into(),
            ok: false,
            details: vec!["01ABC: ledger has 0.4 USD, provider doesn't".into()],
        }])
    }

    fn fetch_image(&self, film: &Path, url: &str) -> anyhow::Result<PathBuf> {
        assert_eq!(url, "https://example.com/dress.png");
        let out = film.join(".shotline").join("dress.png");
        image::RgbImage::new(8, 8).save(&out)?;
        Ok(out)
    }

    fn make_anchor(&self, film: &Path, ask: &AnchorAsk, at: &str) -> anyhow::Result<PathBuf> {
        if ask.frame {
            assert_eq!((ask.shot, ask.prompt.as_str()), (Some(R), "at the edge"));
            assert!(ask.outfit);
            let saved = Project::open(film)?.shot(R)?.frame_prompt;
            assert_eq!(saved.as_deref(), Some("at the edge"));
            // No pictures unless some are added.
            assert!(ask.refs.is_empty(), "{:?}", ask.refs);
            let out = film.join(".shotline").join("frame.png");
            image::RgbImage::new(8, 8).save(&out)?;
            return Ok(out);
        }
        if ask.shot.is_some() {
            assert_eq!((ask.handle.as_str(), ask.prompt.as_str()), ("", "the café"));
            assert!(ask.refs.is_empty());
            let out = film.join(".shotline").join("cafe.png");
            image::RgbImage::new(8, 8).save(&out)?;
            return Ok(out);
        }
        assert_eq!(
            (ask.handle.as_str(), ask.prompt.as_str(), at),
            ("paka", "side view", "$0.04")
        );
        assert_eq!(ask.refs, [film.join(".shotline").join("dress.png")]);
        assert!(!ask.outfit, "unticked, so the clothes can change");
        assert_eq!(
            (ask.aspect.as_deref(), ask.resolution.as_deref()),
            (Some("1:1"), Some("2K"))
        );
        let out = film.join(".shotline").join("made.png");
        image::RgbImage::new(8, 8).save(&out)?;
        Ok(out)
    }
}

fn anchors(p: &Project) -> Vec<PathBuf> {
    p.anchors(&p.element("paka").unwrap()).unwrap()
}

#[test]
fn caret_after_thai_sara_am() {
    let tmp = tempfile::tempdir().unwrap();
    let app = App::new(film(tmp.path()), tmp.path().join("home")).silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    // ำ shapes into two glyphs; a row still needs one per char or every caret after it is off.
    let text = "ทำให้มีป้ายทะเบียน";
    let g = h.ctx.fonts_mut(|f| {
        f.layout_no_wrap(
            text.into(),
            egui::FontId::proportional(14.0),
            egui::Color32::WHITE,
        )
    });
    let glyphs = &g.rows[0].glyphs;
    assert_eq!(glyphs.len(), text.chars().count());
    let ai = text.chars().position(|c| c == 'ใ').unwrap();
    assert_eq!(glyphs[ai].chr, 'ใ');
    assert!(glyphs[ai].pos.x > glyphs[ai - 1].pos.x);
}

#[test]
fn edit_the_cut_and_render() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let r: ShotRef = "01-010".parse().unwrap();
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .silent();
    app.set_plugins(vec![], Some(Arc::new(TestHost)));
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 1000.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label("Cut").click();
    h.run();
    // Nothing selected yet, so there's nothing to render.
    h.get_by_label("1 shot without a take: they're left out of Play and stop a render.");
    h.get_by_label("Render").click();
    h.run();
    assert!(!tmp.path().join("film/renders").exists());

    h.get_by_role_and_label(Role::ComboBox, "Colour match")
        .click();
    h.run();
    h.get_by_label("every join").click();
    h.run();
    h.run();
    assert_eq!(
        p.timeline().unwrap().colormatch.at,
        shotline_core::MatchAt::All
    );

    p.select_take(r, "1").unwrap();
    h.get_by_label("Reload").click();
    h.run();
    h.get_by_label("Render").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label_contains("renders/cut.mp4 · 8s")
        .is_some()));
    h.get_by_label("renders/cut.mp4");

    h.get_by_label("Undo").click();
    h.run();
    assert_eq!(
        p.timeline().unwrap().colormatch.at,
        shotline_core::MatchAt::Continue
    );
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
        aspects: vec!["9:16".into(), "1:1".into(), "4:3".into()],
        resolutions: vec!["1K".into(), "2K".into()],
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
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 1800.0))
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
    // The film's own unless picked: 9:16 in a new film.
    h.get_by_role_and_label(Role::ComboBox, "Size").click();
    h.run();
    h.get_by_label("4:3 landscape");
    h.get_by_label("1:1 square").click();
    h.run();
    h.get_by_role_and_label(Role::ComboBox, "Resolution")
        .click();
    h.run();
    h.get_by_label("2K").click();
    h.run();
    let prompt = h.get_by_role_and_label(Role::MultilineTextInput, "Image prompt");
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
    h.get_by_label("Outfit exactly as in their reference images")
        .click();
    h.run();
    h.get_by_label("Get price").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make it for $0.04")
        .is_some()));
    assert!(anchors(&p).is_empty());
    h.get_by_label("Make it for $0.04").click();
    assert!(wait_for(&mut h, |_| !anchors(&p).is_empty()));
    h.run();
    assert_eq!(anchors(&p).len(), 1);
    // Done with, so it closes.
    assert!(h.query_by_label("Get price").is_none());
    // The anchor it made doesn't go along with the next one.
    h.get_by_label("Make image…").click();
    h.run();
    h.get_by_label("None: the image comes from the words alone.");
    h.get_by_label("Close").click();
    h.run();
    assert!(h.query_by_label("Get price").is_none());
    // Undone like an image added from a file.
    h.get_by_label("Undo").click();
    h.run();
    assert!(anchors(&p).is_empty());
}

#[test]
fn a_shot_s_pictures_and_a_take() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let png = tmp.path().join("in.png");
    image::RgbImage::new(8, 8).save(&png).unwrap();
    p.add_anchor("paka", &png).unwrap();
    p.add_element(Kind::Prop, "Jeep", None, "an old green jeep")
        .unwrap();
    p.add_anchor("jeep", &png).unwrap();
    p.add_anchor("jeep", &png).unwrap();
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .silent();
    app.set_plugins(
        vec![Description {
            name: "fal".into(),
            version: "1".into(),
            protocol: 1,
            env: vec![],
            models: vec![
                ModelInfo {
                    id: "minimax/h3-max/reference-to-video".into(),
                    op: Op::VideoGenerate,
                    ref_image: Some("Image {n}".into()),
                    max_images: Some(9),
                    ..ModelInfo::default()
                },
                ModelInfo {
                    id: "fal-ai/nano-banana".into(),
                    op: Op::ImageGenerate,
                    ref_image: Some("Image {n}".into()),
                    ..ModelInfo::default()
                },
            ],
        }],
        Some(Arc::new(TestHost)),
    );
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1200.0, 1800.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label_contains("01-010  Paka walks in").click();
    // Its posters and pictures load in the background first.
    wait(&mut h, 0.5);
    h.run();
    h.get_by_label("References");
    h.get_by_label("up to 9");
    h.get_by_label("Image 1 · @paka");

    // A picture of the shot's own, made with a prompt.
    h.get_by_label("Make image…").click();
    h.run();
    // The panel pushes what follows down rather than lying over it.
    assert!(
        h.get_by_label("Shoot a take…").rect().top() > h.get_by_label("Get price").rect().bottom()
    );
    let prompt = h.get_by_role_and_label(Role::MultilineTextInput, "Image prompt");
    prompt.focus();
    prompt.type_text("the café");
    h.run();
    h.get_by_label("Get price").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make it for $0.04")
        .is_some()));
    h.get_by_label("Make it for $0.04").click();
    assert!(wait_for(&mut h, |_| !p.shot(R).unwrap().images.is_empty()));
    h.run();
    let img = &p.shot(R).unwrap().images[0];
    assert_eq!(img.file, "chapters/01-into-the-forest/shots/010/01.png");
    h.get_by_label("Image 2");

    let note = h.get_by_role_and_label(Role::MultilineTextInput, "Note on Image 2");
    note.focus();
    note.type_text("the café at night");
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    h.run();
    assert_eq!(p.shot(R).unwrap().images[0].note, "the café at night");
    // The prompt as it's sent names it.
    h.get_by_label_contains("What the model gets").click();
    h.run();
    h.get_by_label_contains("Image 2: the café at night.");
    // An element's anchor gets a note for this shot too.
    let note = h.get_by_role_and_label(Role::MultilineTextInput, "Note on Image 1");
    note.focus();
    note.type_text("from behind");
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    h.run();
    assert_eq!(
        p.shot(R).unwrap().anchor_notes["elements/cast/paka/01.png"],
        "from behind"
    );
    h.get_by_label_contains("Image 1: from behind. Image 2: the café at night.");

    h.get_by_label("Remove Image 2").click();
    h.run();
    assert!(p.shot(R).unwrap().images.is_empty());
    assert!(!p.root.join(&img.file).exists());
    h.get_by_label("Undo").click();
    // Put back, the picture is decoded again in the background.
    wait(&mut h, 0.3);
    h.run();
    assert_eq!(p.shot(R).unwrap().images.len(), 1);
    assert!(p.root.join(&img.file).exists());

    // The shot's own picture can go along with the next one, listed under the shot.
    h.get_by_label("Make image…").click();
    h.run();
    h.get_all_by_role_and_label(Role::ComboBox, "Browse existing")
        .last()
        .unwrap()
        .click();
    h.run();
    h.get_by_label("01-010  Paka walks in");
    h.get_by_label("Elements");
    let before = h.query_all_by_label("Image 1").count();
    h.get_by_label("Picture 1  the café at night").click();
    h.run();
    assert!(h.query_all_by_label("Image 1").count() > before);
    h.get_by_label("Close").click();
    h.run();

    // A take: priced, then shot on a second click.
    let before = p.takes(R).unwrap().len();
    h.get_by_label("Shoot a take…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Shoot it for 0.40 USD")
        .is_some()));
    assert_eq!(p.takes(R).unwrap().len(), before);
    h.get_by_label("Shoot it for 0.40 USD").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("01-010#3 done · 0.40 USD")
        .is_some()));
    assert_eq!(p.takes(R).unwrap().len(), before + 1);
    h.get_by_label("01-010#3 poster");

    // An element's anchor left out of this shot only, then sent again from Add an anchor.
    let anchor = p.shot(R).unwrap();
    assert!(anchor.leave_out.is_empty());
    h.get_by_label("Image 1 · @paka");
    h.get_by_label("Leave out Image 1").click();
    h.run();
    h.run();
    let left = p.shot(R).unwrap().leave_out;
    assert_eq!(left, ["elements/cast/paka/01.png"]);
    assert!(h.query_by_label("Image 1 · @paka").is_none());
    h.get_all_by_role_and_label(Role::ComboBox, "Browse existing")
        .next()
        .unwrap()
        .click();
    h.run();
    h.get_by_label("@paka 1").click();
    h.run();
    h.run();
    assert!(p.shot(R).unwrap().leave_out.is_empty());
    h.get_by_label("Image 1 · @paka");

    // One of the jeep's pictures picked from Elements: the jeep comes along with only that one.
    // The shot's own, before the image panel's further down.
    h.get_all_by_role_and_label(Role::ComboBox, "Browse existing")
        .next()
        .unwrap()
        .click();
    h.run();
    h.get_by_label("@jeep 2").click();
    h.run();
    h.run();
    let s = p.shot(R).unwrap();
    assert_eq!(s.elements, ["paka", "jeep"]);
    assert_eq!(s.leave_out, ["elements/props/jeep/01.png"]);
    h.get_by_label("Image 2 · @jeep");
    h.get_by_label("Leave out Image 2").click();
    h.run();
    h.run();
    let s = p.shot(R).unwrap();
    assert_eq!(s.elements, ["paka"]);
    assert!(s.leave_out.is_empty());

    // A storyboard frame made from the shot, then taken away and back with undo.
    h.get_by_label("Make frame…").click();
    h.run();
    assert!(
        h.query_by_role_and_label(Role::MultilineTextInput, "Image prompt")
            .is_none()
    );
    let fp = h.get_by_role_and_label(Role::MultilineTextInput, "Frame prompt");
    fp.focus();
    fp.type_text("at the edge");
    h.run();
    h.get_by_label("Get price").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make it for $0.04")
        .is_some()));
    h.get_by_label("Make it for $0.04").click();
    assert!(wait_for(&mut h, |_| p.shot(R).unwrap().frame.is_some()));
    h.run();
    let frame = p.shot(R).unwrap().frame.unwrap();
    assert_eq!(frame, "chapters/01-into-the-forest/shots/010.png");
    h.get_by_label("Remove storyboard frame").click();
    h.run();
    assert!(p.shot(R).unwrap().frame.is_none());
    assert!(!p.root.join(&frame).exists());
    h.get_by_label("Undo").click();
    h.run();
    assert_eq!(p.shot(R).unwrap().frame.as_deref(), Some(frame.as_str()));
    assert!(p.root.join(&frame).exists());
}

#[test]
fn translate_only_when_it_helps() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let home = tmp.path().join("home");
    let mut app = App::new(p, home.clone()).with_viewer(None).silent();
    let model = |id: &str, op| ModelInfo {
        id: id.into(),
        op,
        ..ModelInfo::default()
    };
    // A name nobody has set, so the file is what counts.
    let var = "SHOTLINE_TEST_VENICE_TRANSLATE_KEY";
    app.set_plugins(
        vec![
            Description {
                name: "fal".into(),
                version: "1".into(),
                protocol: 1,
                env: vec![],
                models: vec![model("fal-ai/nano-banana", Op::ImageGenerate)],
            },
            Description {
                name: "venice".into(),
                version: "1".into(),
                protocol: 1,
                env: vec![var.into()],
                models: vec![model("claude-haiku-5-5", Op::TextGenerate)],
            },
        ],
        Some(Arc::new(TestHost)),
    );
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 1000.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label("Elements").click();
    h.run();
    h.get_by_label_contains("@paka").click();
    h.run();
    h.get_by_label("Make image…").click();
    h.run();
    let typed = |h: &mut Harness<'static, App>, t: &str| {
        let prompt = h.get_by_role_and_label(Role::MultilineTextInput, "Image prompt");
        prompt.focus();
        prompt.type_text(t);
        h.run();
    };
    let offered =
        |h: &Harness<'static, App>| h.query_by_label("Translate to English first").is_some();
    let env = shotline_core::dotenv::path(&home);
    shotline_core::dotenv::set(&env, var, Some("k")).unwrap();
    // Not offered for English.
    typed(&mut h, "side view");
    assert!(!offered(&h));
    typed(&mut h, " มุมข้าง");
    assert!(offered(&h));
    h.get_by_label("Translate to English first").click();
    h.run();

    // A shot's prompt too, and the take's price takes the translation in.
    h.get_by_label("Shots").click();
    h.run();
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    let prompt = h.get_by_role_and_label(Role::MultilineTextInput, "Prompt");
    prompt.focus();
    prompt.type_text(" ในป่า");
    h.run();
    h.get_by_label("Translate to English first").click();
    h.run();
    h.get_by_label("Shoot a take…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label_contains("0.40 USD + 0.01 USD to translate")
        .is_some()));

    // No key for the text model: nothing to translate with.
    shotline_core::dotenv::set(&env, var, None).unwrap();
    h.run();
    assert!(!offered(&h));
}

/// A name nobody has set, so the file is what counts.
const ELEVENLABS_KEY: &str = "SHOTLINE_TEST_ELEVENLABS_KEY";

#[test]
fn keys_in_preferences() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let var = ELEVENLABS_KEY;
    let shell = Shell::new(None, home.clone(), tmp.path())
        .with_plugins(vec![], Some(Arc::new(TestHost)))
        .testing();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 900.0))
        .build_ui_state(|ui, s: &mut Shell| s.show(ui), shell);
    h.run();
    let open = |h: &mut Harness<Shell>| {
        h.state_mut().command(MenuCommand::Preferences);
        h.run();
        h.get_by_role_and_label(Role::Button, "elevenlabs").click();
        h.run();
    };
    open(&mut h);
    h.get_by_label("not set");
    assert!(h.get_by_label("Check").accesskit_node().is_disabled());

    // Typed, it waits for OK.
    let field = h.get_by_role_and_label(Role::TextInput, var);
    field.focus();
    field.type_text("sk-123");
    h.run();
    h.get_by_label("saved on OK");
    assert!(h.get_by_label("Check").accesskit_node().is_disabled());
    // Escape doesn't throw it away.
    h.key_press(egui::Key::Escape);
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    h.get_by_label("OK saves the changes; Cancel throws them away");
    h.get_by_label("OK").click();
    h.run();
    assert!(h.query_by_label("Preferences").is_none());
    let env = home.join(".env");
    assert_eq!(
        std::fs::read_to_string(&env).unwrap(),
        format!("{var}=sk-123\n")
    );

    open(&mut h);
    h.get_by_label("in .env");
    h.get_by_label("Check").click();
    assert!(wait_for_shell(&mut h, |h| h
        .query_by_label("works: 3.5 USD left")
        .is_some()));

    // Cancel leaves it.
    h.get_by_label("Remove").click();
    h.run();
    h.get_by_label("taken out on OK");
    h.get_by_label("Cancel").click();
    h.run();
    assert!(h.query_by_label("Preferences").is_none());
    assert_eq!(
        std::fs::read_to_string(&env).unwrap(),
        format!("{var}=sk-123\n")
    );

    open(&mut h);
    h.get_by_label("Remove").click();
    h.run();
    h.get_by_label("OK").click();
    h.run();
    assert_eq!(std::fs::read_to_string(&env).unwrap(), "\n");
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
            "stability": {"type": "number", "minimum": 0, "maximum": 1, "default": 0.5,
                "description": "Higher is steadier"},
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
        .with_max_steps(16)
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
    // The plugin is asked for its voices in the background; its repaint can land mid-run.
    assert!(wait_for(&mut h, |h| h
        .query_by_role_and_label(Role::ComboBox, "Voice")
        .is_some()));
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
    // Settings as the model describes them, each with what it does under it.
    h.get_by_label("Higher is steadier");
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
    // It opens at once with a spinner while the big one decodes.
    h.step();
    h.step();
    assert!(h.query_by_label("elements/cast/paka/01.png big").is_none());
    h.get_by_label("Close");
    assert!(wait_for(&mut h, |h| h
        .query_by_label("elements/cast/paka/01.png big")
        .is_some()));
    // A new modal takes a frame or two to find its place.
    h.run();
    // Pinched in, a click doesn't close it; pinched back out, it does.
    let big = "elements/cast/paka/01.png big";
    h.get_by_label(big).hover();
    h.run();
    h.event(egui::Event::Zoom(2.0));
    h.run();
    h.get_by_label(big).click();
    h.run();
    h.get_by_label(big).hover();
    h.run();
    h.event(egui::Event::Zoom(0.25));
    h.run();
    h.get_by_label(big).click();
    h.run();
    assert!(h.query_by_label(big).is_none());

    h.get_by_label("elements/cast/paka/01.png").click();
    assert!(wait_for(&mut h, |h| h.query_by_label(big).is_some()));
    h.run();
    h.get_by_label("Close").click();
    h.run();
    assert!(h.query_by_label("elements/cast/paka/01.png big").is_none());

    // A label says what it shows, saved once the box is left; Enter doesn't break the line.
    let label = h.get_by_role_and_label(
        Role::MultilineTextInput,
        "Label of elements/cast/paka/01.png",
    );
    label.focus();
    label.type_text("side view");
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    h.run();
    assert_eq!(p.element("paka").unwrap().labels["01.png"], "side view");

    // Taking an anchor off takes its label too, and both come back on undo.
    h.get_by_label("Remove").click();
    h.run();
    assert!(!anchor.exists());
    assert!(p.element("paka").unwrap().labels.is_empty());
    h.get_by_label("Undo").click();
    h.run();
    assert_eq!(std::fs::read(&anchor).unwrap(), bytes);
    assert_eq!(p.element("paka").unwrap().labels["01.png"], "side view");

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

    h.get_by_label("Use in the cut #1").click();
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
    let mut h = Harness::builder()
        .with_max_steps(16)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
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
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(900.0, 900.0))
        .build_ui_state(|ui, s: &mut Shell| s.show(ui), shell);
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
    h.get_by_role_and_label(Role::ComboBox, "Shape").click();
    h.run();
    h.get_by_label("16:9 wide, for YouTube").click();
    h.run();
    h.get_by_label("Create film").click();
    h.run();
    h.run();
    assert_eq!(Project::open(&dir).unwrap().config.name, "Rain");
    assert_eq!(Project::open(&dir).unwrap().config.aspect, "16:9");
    assert!(h.state().film().is_some());
    h.get_by_label("Shots");
    if let Some(g) = &git {
        assert!(g.is_repo(&dir));
        h.get_by_label_contains("git history started at");
    }

    // The next time it's in the list.
    let shell = Shell::new(None, home, tmp.path()).testing();
    // Tall enough for the list under Linux's in-window menu bar.
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(900.0, 900.0))
        .build_ui_state(|ui, s: &mut Shell| s.show(ui), shell);
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
    let mut h = Harness::builder()
        .with_max_steps(16)
        .build_ui_state(|ui, s: &mut Shell| s.show(ui), shell);
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
    let mut h = Harness::builder()
        .with_max_steps(16)
        .build_ui_state(|ui, s: &mut Shell| s.show(ui), shell);
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
    // Its posters are tried in the background first.
    wait(&mut h, 0.5);
    h.run();
    assert_eq!(field(&h, Role::MultilineTextInput, "Plot"), "ผกาเดินเข้าป่า");
    h.get_by_label("failed");
    h.get_by_label("content policy");

    h.get_by_label("Use in the cut #1").click();
    h.run();
    // Not a real video, so there's no last frame to take.
    h.get_by_label_contains("01-010#1 selected; can't take its last frame");
    h.get_by_label("in the cut");
    assert!(h.query_by_label("Use in the cut #1").is_none());
    let p = Project::open(&tmp.path().join("film")).unwrap();
    let shot = p
        .shot(ShotRef {
            chapter: 1,
            shot: 10,
        })
        .unwrap();
    assert!(shot.selected_take.is_some());

    // Not a real video.
    h.get_by_label("01-010#1 poster").click();
    h.run();
    h.get_by_label_contains("can't play 01-010#1");
    h.get_by_label("Close").click();
    h.run();

    // Removed, the shot stops using it; the others keep their numbers; undo brings it back.
    h.get_by_label("Remove #1").click();
    h.run();
    h.get_by_label("01-010#1 removed");
    assert!(h.query_by_label("Remove #1").is_none());
    h.get_by_label("Remove #2");
    let p = Project::open(&tmp.path().join("film")).unwrap();
    let r = ShotRef {
        chapter: 1,
        shot: 10,
    };
    assert_eq!(p.shot(r).unwrap().selected_take, None);
    h.get_by_label("Undo").click();
    h.run();
    h.get_by_label("Remove #1");
    assert!(p.shot(r).unwrap().selected_take.is_some());
}

#[test]
fn a_selected_take_s_last_frame() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let next = p
        .add_shot(
            "1",
            None,
            Shot {
                title: "Paka stops".into(),
                ..Shot::default()
            },
        )
        .unwrap();
    let take = p.take(R, "1").unwrap();
    let video = p.job_dir(take.output().unwrap().id).join("out.mp4");
    if clip(&video).is_none() {
        return;
    }
    let app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 1400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();

    // Nothing to continue from until the shot before has a take in the cut.
    h.get_by_label_contains(&format!("{next}  Paka stops"))
        .click();
    h.run();
    let cont = h.get_by_label("Continue from previous shot");
    assert!(cont.accesskit_node().is_disabled());

    // Selected, its last frame is taken, once.
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    h.get_by_label("Use in the cut #1").click();
    h.run();
    h.get_by_label("01-010#1 selected");
    let last = p.last_frame(&take).unwrap();
    assert!(last.is_file());

    // It's there to pick under Takes.
    h.get_by_label_contains(&format!("{next}  Paka stops"))
        .click();
    h.run();
    h.get_by_role_and_label(Role::ComboBox, "Browse existing frames")
        .click();
    h.run();
    h.run();
    h.get_by_label("Takes");
    h.get_by_label("Last frame of 01-010#1").click();
    h.run();
    h.run();
    let frame = p.shot(next).unwrap().frame.unwrap();
    assert_eq!(
        std::fs::read(p.root.join(frame)).unwrap(),
        std::fs::read(&last).unwrap()
    );

    h.get_by_label("Continue from previous shot").click();
    h.run();
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    h.run();
    assert_eq!(p.shot(next).unwrap().transition, Transition::Continue);
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
    // What it was asked for, as written and in English.
    let mut first_job = takes[0].jobs[0].clone();
    first_job.brief = Some("ผกาเดินเข้าป่า".into());
    first_job.english = Some("Paka walks into the forest".into());
    std::fs::write(
        p.job_dir(first_job.id).join("job.toml"),
        toml::to_string(&first_job).unwrap(),
    )
    .unwrap();

    let app = App::new(p, tmp.path().join("home"))
        .with_viewer(None)
        .silent();
    // Tall enough that the takes stay in view under the player.
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size([1000.0, 1800.0])
        .allow_missing_glyphs()
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.step();
    h.get_by_label_contains("01-010").click();
    wait(&mut h, 0.1);
    // The take in the cut is under the title, stopped; a space typed in the plot is a space.
    assert!(wait_for(&mut h, |h| h.query_by_label("01-010#1").is_some()));
    assert!(!h.state().is_playing());
    h.get_by_label("160 × 90");
    h.get_by_label("1.0 s");
    h.get_by_label("fake · fake/video");
    h.get_by_label("ผกาเดินเข้าป่า");
    h.get_by_label("Paka walks into the forest");
    let plot = h.get_by_role_and_label(Role::MultilineTextInput, "Plot");
    plot.focus();
    plot.type_text(" ");
    h.key_press(egui::Key::Space);
    wait(&mut h, 0.1);
    assert!(!h.state().is_playing());
    h.key_press(egui::Key::Escape);
    wait(&mut h, 0.1);
    // Out of the field, space plays it and stops it.
    h.key_press(egui::Key::Space);
    assert!(wait_for(&mut h, |h| h.state().is_playing()));
    h.key_press(egui::Key::Space);
    assert!(wait_for(&mut h, |h| !h.state().is_playing()));

    // From a take's popup, the other takes play beside it.
    let poster = tmp.path().join(format!(
        "film/.shotline/cache/posters/{}.png",
        takes[0].jobs[0].id
    ));
    assert!(wait_for(&mut h, |_| poster.is_file()));
    wait(&mut h, 0.5);
    h.get_by_label("01-010#1 poster").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_role_and_label(Role::ComboBox, "Compare with")
        .is_some()));
    h.get_by_role_and_label(Role::ComboBox, "Compare with")
        .click();
    assert!(wait_for(&mut h, |h| h.query_by_label("01-010#3").is_some()));
    h.get_by_label("01-010#3").click();
    // Beside it in the popup: its name under its picture, and both playing.
    assert!(wait_for(&mut h, |h| h
        .query_all_by_label("01-010#3")
        .count()
        == 2));
    assert!(h.state().is_playing());
    // The second player moves Pause; click it where it settles.
    wait(&mut h, 0.3);
    h.get_by_label("Pause").click();
    assert!(wait_for(&mut h, |h| !h.state().is_playing()));
    h.get_by_label("Close").click();
    wait(&mut h, 0.1);

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

    // On its track, the playhead starts the cut where it's put, and the take opens its shot.
    h.get_by_role_and_label(Role::Slider, "Playhead").click();
    assert!(wait_for(&mut h, |h| h
        .query_all_by_label("the cut")
        .count()
        == 2));
    h.get_by_role_and_label(Role::Button, "01-010#1").click();
    wait(&mut h, 0.1);
    assert_eq!(h.state().tab(), Tab::Shots);
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
    // Per day and model, and the bars grow in again for another span.
    h.get_by_label("spent per day, last 7 days");
    h.get_by_label("fake/video  0.4 USD");
    h.get_by_label("30 days").click();
    h.run();
    h.get_by_label("spent per day, last 30 days");

    // The ledger: checks, rates and the providers' side, then corrections.
    h.set_size(egui::vec2(1000.0, 1800.0));
    h.state_mut().set_plugins(vec![], Some(Arc::new(TestHost)));
    h.run();
    // Checked without asking, and a tab stays picked.
    h.get_by_label("This film: 2 lines, none changed by hand");
    h.get_by_label("Check the ledger").click();
    h.run();
    h.get_by_label("This film: 2 lines, none changed by hand");
    h.get_by_label("Rates").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("fake-credit")
        .is_some()));
    h.get_by_label("Compare with providers").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("01ABC: ledger has 0.4 USD, provider doesn't")
        .is_some()));

    h.get_by_label("Adjust…").click();
    h.run();
    let f = h.get_by_role_and_label(Role::TextInput, "Amount");
    f.focus();
    f.type_text("-0.1");
    h.run();
    let f = h.get_by_role_and_label(Role::TextInput, "Note");
    f.focus();
    f.type_text("promo");
    h.run();
    h.run();
    h.get_by_label("Record adjustment").click();
    // The bars grow in again with the new numbers.
    h.run_steps(8);
    h.get_by_label("0.3 USD");
    h.get_by_label("adjusted -0.1 USD");

    h.get_by_label("Refund…").click();
    h.run();
    h.get_by_role_and_label(Role::ComboBox, "Job").click();
    h.run();
    h.get_by_label_contains("fake/video · 0.4 USD").click();
    h.run();
    assert_eq!(field(&h, Role::TextInput, "Amount"), "0.4");
    h.get_by_label("Record refund").click();
    h.run_steps(8);
    h.get_by_label("refunded 0.4 USD");
    h.get_by_label("-0.1 USD");
}

#[test]
fn release_what_a_dead_job_held() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let home = tmp.path().join("home");
    // A job the app was closed on: held, never sent.
    let j = p
        .create_job(Job::new(
            shotline_core::protocol::Op::ImageGenerate,
            "fake",
            "fake/image",
        ))
        .unwrap();
    let books = p
        .books(&home, &UserConfig::load(&home).unwrap(), &[])
        .unwrap();
    let ctx = Context {
        job: j.id.to_string(),
        take: None,
        shot: None,
        chapter: None,
        op: "image.generate".into(),
        plugin: "fake".into(),
        model: "fake/image".into(),
    };
    let quote = Quote {
        amount: 0.05,
        unit: "USD".into(),
        units: None,
        unit_name: None,
    };
    books.reserve(jiff::Timestamp::now(), &ctx, &quote).unwrap();

    let app = App::new(p.clone(), home).with_viewer(None).silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 1800.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label("Cost").click();
    h.run();
    h.get_by_label("0.05 USD held for jobs still running");
    h.get_by_label("Release…").click();
    h.run();
    h.get_by_role_and_label(Role::ComboBox, "Job").click();
    h.run();
    h.get_by_label_contains("fake/image · 0.05 USD").click();
    h.run();
    h.get_by_label_contains("still pending as far as its job.toml says");
    // Not until you say nothing is running it.
    h.get_by_label("Release").click();
    h.run();
    assert!(p.job(&j.id.to_string()).unwrap().status == JobStatus::Pending);
    h.get_by_label("Nothing is running it any more").click();
    h.run();
    h.get_by_label("Release").click();
    h.run_steps(8);
    h.get_by_label(&format!("released {}, nothing spent", j.id));
    assert!(
        h.query_by_label("0.05 USD held for jobs still running")
            .is_none()
    );
    assert_eq!(
        p.job(&j.id.to_string()).unwrap().status,
        JobStatus::Cancelled
    );
}

#[test]
fn warn_about_stuck_jobs_on_open() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let home = tmp.path().join("home");
    let books = p
        .books(&home, &UserConfig::load(&home).unwrap(), &[])
        .unwrap();
    // One left unsent ten minutes ago, one that may well be on its way right now.
    let now = jiff::Timestamp::now();
    let mut ids = vec![];
    for (model, ago) in [("fake/image", 10), ("fake/video", 0)] {
        let j = p
            .create_job(Job::new(
                shotline_core::protocol::Op::ImageGenerate,
                "fake",
                model,
            ))
            .unwrap();
        let ctx = Context {
            job: j.id.to_string(),
            take: None,
            shot: None,
            chapter: None,
            op: "image.generate".into(),
            plugin: "fake".into(),
            model: model.into(),
        };
        let quote = Quote {
            amount: 0.05,
            unit: "USD".into(),
            units: None,
            unit_name: None,
        };
        let at = now - jiff::SignedDuration::from_mins(ago);
        books.reserve(at, &ctx, &quote).unwrap();
        ids.push(j.id);
    }

    let open = || {
        let app = App::new(p.clone(), home.clone()).with_viewer(None).silent();
        let mut h = Harness::builder()
            .with_max_steps(16)
            .with_size(egui::vec2(1400.0, 1800.0))
            .build_ui_state(|ui, app: &mut App| app.show(ui), app);
        h.run();
        h
    };
    let mut h = open();
    h.get_by_label_contains("A fake/image job from ");
    h.get_by_label_contains("never finished and still holds 0.05 USD.");
    h.get_by_label("Dismiss").click();
    h.run();
    assert!(h.query_by_label_contains("never finished").is_none());

    let mut h = open();
    h.get_by_label("Review…").click();
    h.run();
    assert!(h.query_by_label_contains("never finished").is_none());
    h.get_by_label_contains("still pending as far as its job.toml says");
    h.get_by_label("Nothing is running it any more").click();
    h.run();
    h.get_by_label("Release").click();
    h.run_steps(8);
    h.get_by_label(&format!("released {}, nothing spent", ids[0]));
}

#[test]
fn set_the_format_on_the_story_tab() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    p.select_take("01-010".parse().unwrap(), "1").unwrap();
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .silent();
    app.set_plugins(
        vec![Description {
            name: "fal".into(),
            version: "1".into(),
            protocol: 1,
            env: vec![],
            models: vec![ModelInfo {
                id: "minimax/h3-max/reference-to-video".into(),
                op: Op::VideoGenerate,
                aspects: vec!["9:16".into(), "16:9".into()],
                resolutions: vec!["768P".into(), "1080P".into()],
                ..ModelInfo::default()
            }],
        }],
        Some(Arc::new(TestHost)),
    );
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 1000.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label("Story").click();
    h.run();
    h.get_by_label_contains("already has a take. Takes keep the shape");
    let aspect = || Project::open(&p.root).unwrap().format();
    h.get_by_role_and_label(Role::ComboBox, "Shape").click();
    h.run();
    h.get_by_label("16:9 wide").click();
    h.run();
    h.get_by_role_and_label(Role::ComboBox, "Resolution")
        .click();
    h.run();
    h.get_by_label("1080P").click();
    h.run();
    let f = aspect();
    assert_eq!(
        (f.aspect.as_str(), f.resolution.as_str()),
        ("16:9", "1080P")
    );
    h.get_by_label("Undo").click();
    h.run();
    h.get_by_label("Undo").click();
    h.run();
    let f = aspect();
    assert_eq!((f.aspect.as_str(), f.resolution.as_str()), ("9:16", "768P"));
}

#[test]
fn how_lines_are_spoken() {
    let tmp = tempfile::tempdir().unwrap();
    let mut h = harness(tmp.path());
    h.run();
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    let p = Project::open(&tmp.path().join("film")).unwrap();
    h.get_by_role_and_label(Role::ComboBox, "Lines spoken")
        .click();
    h.run();
    h.get_by_label("lip synced after");
    h.get_by_label("by the video model").click();
    h.run();
    h.run();
    assert_eq!(p.shot(R).unwrap().recipe.as_deref(), Some("spoken"));
    h.get_by_role_and_label(Role::ComboBox, "Lines spoken")
        .click();
    h.run();
    h.get_by_label("no").click();
    h.run();
    h.run();
    assert_eq!(p.shot(R).unwrap().recipe, None);
}

#[test]
fn a_shot_on_another_model() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .silent();
    let video = |id: &str, res: &[&str]| ModelInfo {
        id: id.into(),
        op: Op::VideoGenerate,
        resolutions: res.iter().map(|r| r.to_string()).collect(),
        ..ModelInfo::default()
    };
    app.set_plugins(
        vec![Description {
            name: "fal".into(),
            version: "1".into(),
            protocol: 1,
            env: vec![],
            models: vec![
                video("minimax/h3-max/reference-to-video", &["768P", "1080P"]),
                video("bytedance/seedance/v2", &["480p", "1080p"]),
            ],
        }],
        Some(Arc::new(TestHost)),
    );
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1400.0, 1000.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    let model = || p.shot(R).unwrap().model;

    h.get_by_role_and_label(Role::ComboBox, "Model").click();
    h.run();
    h.get_by_label("fal · bytedance/seedance/v2").click();
    h.run();
    h.run();
    let m = model().unwrap();
    assert_eq!(m.id.as_deref(), Some("bytedance/seedance/v2"));
    // The film's 768P isn't one of this model's.
    h.get_by_label("seedance doesn't do 768P");
    h.get_by_role_and_label(Role::ComboBox, "Resolution")
        .click();
    h.run();
    h.get_by_label("1080p").click();
    h.run();
    h.run();
    assert_eq!(model().unwrap().resolution.as_deref(), Some("1080p"));
    assert!(h.query_by_label_contains("doesn't do").is_none());

    // Back to the film's: the shot file loses its [model].
    h.get_by_role_and_label(Role::ComboBox, "Resolution")
        .click();
    h.run();
    h.get_by_label("Film's (768P)").click();
    h.run();
    h.get_by_role_and_label(Role::ComboBox, "Model").click();
    h.run();
    h.get_by_label("Film's (h3-max)").click();
    h.run();
    h.run();
    assert_eq!(model(), None);
}

#[test]
fn details_of_a_made_picture() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    // An anchor "Make image" made: a copy of its job's output.
    let mut j = p
        .create_job(Job::new(Op::ImageGenerate, "venice", "seedream-v4"))
        .unwrap();
    let out = p.job_dir(j.id).join("out.png");
    image::RgbImage::new(8, 8).save(&out).unwrap();
    j.outputs = vec!["out.png".into()];
    j.brief = Some("ใส่ชุดสีแดง".into());
    j.english = Some("in a red dress".into());
    j.prompt = Some("A reference image of Paka for a film: in a red dress".into());
    j.status = JobStatus::Done;
    p.save_job(&j).unwrap();
    p.add_anchor("paka", &out).unwrap();
    // And one added from a file.
    let other = tmp.path().join("mine.png");
    image::RgbImage::new(9, 9).save(&other).unwrap();
    p.add_anchor("paka", &other).unwrap();

    let app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 1000.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label("Elements").click();
    h.run();
    h.get_by_label("@paka  Paka").click();
    h.run();
    let open = |h: &mut Harness<'static, App>, name: &str| {
        h.get_by_label(name).click();
        assert!(wait_for(h, |h| h
            .query_by_label(&format!("{name} big"))
            .is_some()));
        h.run();
        h.get_by_label("Details").click();
        h.run();
    };
    open(&mut h, "elements/cast/paka/01.png");
    h.get_by_label_contains("venice · seedream-v4 · ");
    h.get_by_label("You wrote");
    h.get_by_label("ใส่ชุดสีแดง");
    h.get_by_label("In English");
    h.get_by_label("in a red dress");
    h.get_by_label("A reference image of Paka for a film: in a red dress");
    h.get_by_label("Hide details").click();
    h.run();
    assert!(h.query_by_label("You wrote").is_none());
    h.get_by_label("Close").click();
    h.run();

    open(&mut h, "elements/cast/paka/02.png");
    h.get_by_label("Added from a file, so there's no prompt for it.");
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
        .with_max_steps(16)
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
        .with_max_steps(16)
        .allow_missing_glyphs()
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label("Shots").click();
    h.run();
    assert_eq!(field(&h, Role::MultilineTextInput, "Plot"), "ผกาเดินเข้าป่า");
}

#[test]
fn a_take_in_its_popup() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let mut j = Job::new(Op::VideoGenerate, "fake", "fake/video");
    j.shot = Some(R);
    j.take = Some(p.new_take_id());
    let mut j = p.create_job(j).unwrap();
    if clip(&p.job_dir(j.id).join("out.mp4")).is_none() {
        return;
    }
    j.status = JobStatus::Done;
    j.outputs = vec!["out.mp4".into()];
    j.prompt = Some("Paka walks into the trees, a test pattern".into());
    p.save_job(&j).unwrap();

    let app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size([1000.0, 1800.0])
        .allow_missing_glyphs()
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.step();
    h.get_by_label_contains("01-010").click();
    h.step();
    let poster = p.root.join(format!(".shotline/cache/posters/{}.png", j.id));
    assert!(wait_for(&mut h, |_| poster.is_file()));
    h.get_by_label("01-010#3 poster").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Paka walks into the trees, a test pattern")
        .is_some()));
    // Waits to be played.
    assert!(!h.state().is_playing());
    h.get_by_label("Play");
    h.get_by_label("Close").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Paka walks into the trees, a test pattern")
        .is_none()));
}

#[test]
fn remake_replaces_a_line_s_recording() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    p.add_line(R, "paka", "ไปกันเถอะ", None).unwrap();
    let mut s = p.shot(R).unwrap();
    s.recipe = Some("spoken".into());
    p.save_shot(R, &s).unwrap();
    TestHost.say(&p.root, R, 0, "0.02 USD").unwrap();
    let old = p.shot(R).unwrap().lines[0].audio.clone().unwrap();

    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .silent();
    app.set_plugins(vec![], Some(Arc::new(TestHost)));
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 1200.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();

    // Back to the words, without ▼ to pick another; Cancel keeps what it had.
    h.get_by_label("Remake…").click();
    h.run();
    assert_eq!(field(&h, Role::TextInput, "Line 1"), "ไปกันเถอะ");
    assert!(h.query_by_label("Pick a recording for line 1").is_none());
    h.get_by_label("Cancel").click();
    h.run();
    h.get_by_role_and_label(Role::ComboBox, "Line 1");

    // Recorded again, the new one takes the old one's place.
    h.get_by_label("Remake…").click();
    h.run();
    let words = h.get_by_role_and_label(Role::TextInput, "Line 1");
    words.focus();
    words.type_text(" [whispers]");
    h.run();
    h.get_by_label("Record…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Record it for 0.02 USD")
        .is_some()));
    h.get_by_label("Record it for 0.02 USD").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_role_and_label(Role::ComboBox, "Line 1")
        .is_some()));
    h.run();
    let line = p.shot(R).unwrap().lines[0].clone();
    assert_eq!(line.text, "ไปกันเถอะ [whispers]");
    assert_ne!(line.audio.as_deref(), Some(old.as_str()));
    assert!(p.job(&old).is_err(), "the old recording is still there");
}

#[test]
fn a_line_recorded_and_picked() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    p.add_line(R, "paka", "ไปกันเถอะ", None).unwrap();
    let mut s = p.shot(R).unwrap();
    s.recipe = Some("spoken".into());
    p.save_shot(R, &s).unwrap();
    // Something Paka said in another shot.
    let other = p
        .add_shot(
            "1",
            None,
            Shot {
                plot: "ผกาหันกลับมา".into(),
                ..Shot::default()
            },
        )
        .unwrap();
    p.add_line(other, "paka", "ใครน่ะ", None).unwrap();
    TestHost.say(&p.root, other, 0, "0.02 USD").unwrap();
    // One said another way before, no longer picked.
    let mut s = p.shot(R).unwrap();
    s.lines[0].text = "ไปกันเถอะ [whispers]".into();
    p.save_shot(R, &s).unwrap();
    TestHost.say(&p.root, R, 0, "0.02 USD").unwrap();
    let mut s = p.shot(R).unwrap();
    s.lines[0].text = "ไปกันเถอะ".into();
    s.lines[0].audio = None;
    p.save_shot(R, &s).unwrap();

    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .silent();
    app.set_plugins(vec![], Some(Arc::new(TestHost)));
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 1200.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    h.get_by_label("Record…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Record it for 0.02 USD")
        .is_some()));
    h.get_by_label("Record it for 0.02 USD").click();
    // Recorded, the words give way to the dropdown, on what was just made.
    assert!(wait_for(&mut h, |h| h
        .query_by_role_and_label(Role::ComboBox, "Line 1")
        .is_some()));
    h.run();
    let first = p.shot(R).unwrap().lines[0].audio.clone();
    assert!(first.is_some());

    // The one said before is there to pick, words and all.
    h.get_by_role_and_label(Role::ComboBox, "Line 1").click();
    h.run();
    h.get_by_label("ไปกันเถอะ [whispers]").click();
    h.run();
    h.run();
    let s = p.shot(R).unwrap();
    assert_eq!(s.lines[0].text, "ไปกันเถอะ [whispers]");
    assert!(s.lines[0].audio.is_some() && s.lines[0].audio != first);

    // Both are there to go back to; the other shot's after Show all.
    h.get_by_role_and_label(Role::ComboBox, "Line 1").click();
    h.run();
    h.get_by_label("Play ไปกันเถอะ");
    assert!(h.query_by_label("ใครน่ะ").is_none());
    h.get_by_label("Show all (1 from other shots)").click();
    h.run();
    h.get_by_label("ใครน่ะ");
    h.get_by_label("ไปกันเถอะ").click();
    h.run();
    h.run();
    let s = p.shot(R).unwrap();
    assert_eq!(
        (s.lines[0].audio.clone(), s.lines[0].text.as_str()),
        (first.clone(), "ไปกันเถอะ")
    );

    // Deleted, it's gone, and the line goes back to its words once the list is closed.
    h.get_by_role_and_label(Role::ComboBox, "Line 1").click();
    h.run();
    h.get_by_label("Delete ไปกันเถอะ").click();
    h.run();
    h.run();
    assert!(h.query_by_label("Play ไปกันเถอะ").is_none());
    h.key_press(egui::Key::Escape);
    h.run();
    h.run();
    assert_eq!(field(&h, Role::TextInput, "Line 1"), "ไปกันเถอะ");
    h.get_by_label("Undo").click();
    h.run();
    h.run();
    assert_eq!(p.shot(R).unwrap().lines[0].audio, first);
    h.get_by_role_and_label(Role::ComboBox, "Line 1");

    // Another shot's line sends that one, so it can't be deleted here; it's a way there.
    h.get_by_role_and_label(Role::ComboBox, "Line 1").click();
    h.run();
    h.get_by_label("Show all (1 from other shots)").click();
    h.run();
    h.get_by_label("1 other line");
    h.get_by_label("Delete ไปกันเถอะ");
    assert!(h.query_by_label("Delete ใครน่ะ").is_none());
    h.get_by_label(&format!("Go to {other}")).click();
    h.run();
    h.run();
    assert_eq!(field(&h, Role::MultilineTextInput, "Plot"), "ผกาหันกลับมา");

    // A new line can take one recorded before, words and all, from ▼ beside Record….
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    h.get_by_label("Add line").click();
    h.run();
    h.get_by_role_and_label(Role::ComboBox, "Pick a recording")
        .click();
    h.run();
    h.get_by_label("ไปกันเถอะ [whispers]").click();
    h.run();
    h.run();
    let s = p.shot(R).unwrap();
    assert_eq!(s.lines.len(), 2);
    assert_eq!(s.lines[1].text, "ไปกันเถอะ [whispers]");
    assert!(s.lines[1].audio.is_some() && s.lines[1].audio != s.lines[0].audio);
    h.get_by_role_and_label(Role::ComboBox, "Line 2");
}

#[test]
fn lines_dragged_into_order() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    p.add_line(R, "paka", "หนึ่ง", Some(1.0)).unwrap();
    p.add_line(R, "paka", "สอง", None).unwrap();
    let mut s = p.shot(R).unwrap();
    s.recipe = Some("spoken".into());
    p.save_shot(R, &s).unwrap();
    let app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 1200.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    let starts: Vec<_> = h
        .get_all_by_role_and_label(Role::SpinButton, "Starts")
        .collect();
    assert_eq!(starts[0].value().as_deref(), Some("1 s"));
    assert_eq!(starts[1].value().as_deref(), Some("after"));
    let from = h.get_by_label("Move line 2").rect().center();
    let to = h.get_by_label("Move line 1").rect().center() - egui::vec2(0.0, 6.0);
    let pointer = |h: &mut Harness<App>, e: egui::Event| {
        h.event(e);
        h.step();
    };
    pointer(&mut h, egui::Event::PointerMoved(from));
    pointer(
        &mut h,
        egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        },
    );
    for k in 1..=10 {
        let at = from + (to - from) * (k as f32 / 10.0);
        pointer(&mut h, egui::Event::PointerMoved(at));
    }
    pointer(
        &mut h,
        egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        },
    );
    h.run();
    let s = p.shot(R).unwrap();
    // Moved, each keeping its own start.
    assert_eq!((s.lines[0].text.as_str(), s.lines[0].at), ("สอง", None));
    assert_eq!(
        (s.lines[1].text.as_str(), s.lines[1].at),
        ("หนึ่ง", Some(1.0))
    );
}

#[test]
fn plugins_in_preferences() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let shell = Shell::new(Some(p.clone()), tmp.path().join("home"), tmp.path())
        .with_plugins(vec![], Some(Arc::new(TestHost)))
        .testing();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 900.0))
        .build_ui_state(|ui, s: &mut Shell| s.show(ui), shell);
    h.run();
    let open = |h: &mut Harness<Shell>| {
        h.state_mut().command(MenuCommand::Preferences);
        h.run();
    };
    open(&mut h);
    // The first plugin's page.
    h.get_by_label("built into shotline");
    h.get_by_label("needs FAL_API_KEY");

    // Options go into the film on OK, as one step to undo.
    const H3: &str = "minimax/h3-max/reference-to-video";
    let safety = format!("Safety checker for {H3}");
    h.get_by_role_and_label(Role::CheckBox, &safety).click();
    h.run();
    h.get_by_role_and_label(Role::ComboBox, &format!("Prompt rewriting for {H3}"))
        .click();
    h.run();
    h.get_by_label("quality").click();
    h.run();
    let cfg = || Project::open(&p.root).unwrap().config;
    assert!(cfg().params.is_empty());
    h.get_by_label("OK").click();
    h.run();
    let h3 = || cfg().params["fal"][H3].clone();
    assert_eq!(h3()["enable_safety_checker"].as_bool(), Some(false));
    assert_eq!(h3()["prompt_expansion_mode"].as_str(), Some("quality"));
    h.state_mut().command(MenuCommand::Undo);
    h.run();
    assert!(cfg().params.is_empty());
    h.state_mut().command(MenuCommand::Redo);
    h.run();

    // Back to the default by hand.
    open(&mut h);
    h.get_all_by_label("Use default").next().unwrap().click();
    h.run();
    h.get_all_by_label("Use default").next().unwrap().click();
    h.run();
    h.get_by_label("OK").click();
    h.run();
    assert!(cfg().params.is_empty());

    // The copy further along PATH, kept on OK.
    open(&mut h);
    h.get_by_role_and_label(Role::Button, "venice").click();
    h.run();
    h.get_by_label("Also found, not used");
    h.get_by_label("Use this one").click();
    h.run();
    h.get_by_label("keeps /b/shotline-plugin-venice");
    assert!(PINNED.lock().unwrap().is_empty());
    h.get_by_label("OK").click();
    h.run();
    assert_eq!(
        PINNED.lock().unwrap().last().unwrap().1,
        PathBuf::from("/b/shotline-plugin-venice")
    );
    open(&mut h);
    h.get_by_role_and_label(Role::Button, "venice").click();
    h.run();
    h.get_by_label("kept");
    h.get_by_label("Let PATH decide").click();
    h.run();
    h.get_by_label("OK").click();
    h.run();
    open(&mut h);
    h.get_by_role_and_label(Role::Button, "venice").click();
    h.run();
    h.get_by_label("Also found, not used");
}

#[test]
fn default_models_in_preferences() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let shell = Shell::new(Some(p.clone()), tmp.path().join("home"), tmp.path())
        .with_plugins(vec![], Some(Arc::new(TestHost)))
        .testing();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 900.0))
        .build_ui_state(|ui, s: &mut Shell| s.show(ui), shell);
    h.run();
    h.state_mut().command(MenuCommand::Preferences);
    h.run();
    h.get_by_role_and_label(Role::Button, "Default Models")
        .click();
    h.run();
    // How the picked one is charged, and what's in the way.
    h.get_by_label("$0.05 a second");
    h.get_by_label("needs FAL_API_KEY");
    let translating = h.get_by_role_and_label(Role::ComboBox, "Translating");
    assert_eq!(translating.value().as_deref(), Some("same as drafting"));

    // Picked from the list, which shows each one's price too.
    h.get_by_role_and_label(Role::ComboBox, "Translating")
        .click();
    h.run();
    // A click on the window can put it back on top; the open list still draws above it.
    let window = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("preferences"));
    h.ctx.move_to_top(window);
    h.run();
    let item = h
        .get_by_label("venice · claude-haiku-5-5\nper token")
        .rect()
        .center();
    let on_top = h.ctx.layer_id_at(item).unwrap();
    assert_ne!(on_top, window, "the list is under the window");
    h.get_by_label("venice · claude-haiku-5-5\nper token")
        .click();
    h.run();
    let cfg = || Project::open(&p.root).unwrap().config;
    assert_eq!(cfg().translate, None);
    h.get_by_label("OK").click();
    h.run();
    assert_eq!(
        cfg().translate,
        Some(shotline_core::ModelRef::new("venice", "claude-haiku-5-5"))
    );
    h.state_mut().command(MenuCommand::Undo);
    h.run();
    assert_eq!(cfg().translate, None);
}

#[test]
fn budget_in_preferences() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let home = tmp.path().join("home");
    let shell = Shell::new(Some(p.clone()), home.clone(), tmp.path())
        .with_plugins(vec![], Some(Arc::new(TestHost)))
        .testing();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 900.0))
        .build_ui_state(|ui, s: &mut Shell| s.show(ui), shell);
    h.run();
    h.state_mut().command(MenuCommand::Preferences);
    h.run();
    h.get_by_role_and_label(Role::Button, "Budget").click();
    h.run();

    let set = |h: &mut Harness<Shell>, name: &str, to: &str| {
        h.get_by_role_and_label(Role::TextInput, name).focus();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.run();
        h.key_press(egui::Key::Backspace);
        if !to.is_empty() {
            h.get_by_role_and_label(Role::TextInput, name).type_text(to);
        }
        h.run();
    };
    set(&mut h, "Per day", "abc");
    h.get_by_label("OK").click();
    h.run();
    h.get_by_label("Per day: \"abc\" isn't an amount; leave it empty for no limit");

    set(&mut h, "Per day", "30");
    set(&mut h, "Per take", "");
    set(&mut h, "Per day, all films", "50");
    h.get_by_label("OK").click();
    h.run();
    let budget = || Project::open(&p.root).unwrap().config.budget;
    assert_eq!(budget().per_day, Some(30.0));
    assert_eq!(budget().per_take, None);
    let user = UserConfig::load(&home).unwrap();
    assert_eq!(user.budget.per_day, Some(50.0));

    h.state_mut().command(MenuCommand::Undo);
    h.run();
    assert_ne!(budget().per_day, Some(30.0));
}

#[test]
fn a_take_from_pieces() {
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
    let other = tmp.path().join("from an editor.mp4");
    clip(&other).unwrap();
    assert_eq!(p.import_take(r, &other).unwrap().number, 3);

    let app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size([1100.0, 1800.0])
        .allow_missing_glyphs()
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.step();
    h.get_by_label_contains("01-010").click();
    wait(&mut h, 0.1);
    h.get_by_label("brought in from from an editor.mp4");

    h.get_by_label("Make a take from pieces…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Add piece")
        .is_some()));
    // The whole of #1, then the second half of #3, faded in.
    h.get_by_label("Add piece").click();
    wait(&mut h, 0.1);
    h.get_by_role_and_label(Role::RadioButton, "cut from #3")
        .click();
    wait(&mut h, 0.2);
    h.get_by_label("Add piece").click();
    wait(&mut h, 0.1);
    h.get_by_label_contains("2. #3  0.00–1.00s").click();
    wait(&mut h, 0.1);
    h.get_by_label("changing piece 2");
    h.get_by_role_and_label(Role::SpinButton, "Start").focus();
    // Focused, it turns into a box to type in.
    h.run();
    h.get_by_role_and_label(Role::SpinButton, "Start")
        .type_text("0.5");
    h.run();
    h.key_press(egui::Key::Enter);
    wait(&mut h, 0.1);
    h.get_by_label("Done").click();
    h.get_by_role_and_label(Role::RadioButton, "Fade into piece 2")
        .click();
    wait(&mut h, 0.1);
    h.get_by_label("Make take").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label_contains("01-010#4 made")
        .is_some()));
    let made = p.take(r, "4").unwrap();
    let pieces = shotline_core::pieces_of(&made).unwrap();
    assert_eq!(pieces.len(), 2);
    assert_eq!(pieces[1].join, shotline_core::Join::Fade);
    assert_eq!((pieces[1].from, pieces[1].to), (0.5, 1.0));
    h.get_by_label("from pieces of #1, #3");

    // Opened again with its pieces, to change.
    h.get_by_label("Change pieces #4").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label_contains("2. #3  0.50–1.00s")
        .is_some()));
}

#[test]
fn made_images_stay_to_pick_again() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let made = |brief: &str, shot: Option<ShotRef>, who: Option<&str>| {
        let mut j = Job::new(Op::ImageGenerate, "fake", "fake/image");
        j.shot = shot;
        j.who = who.map(Into::into);
        j.brief = Some(brief.into());
        let mut j = p.create_job(j).unwrap();
        std::fs::write(p.job_dir(j.id).join("out.png"), brief).unwrap();
        j.outputs = vec!["out.png".into()];
        j.status = JobStatus::Done;
        p.save_job(&j).unwrap();
        j
    };
    made("first try", Some(R), None);
    let second = made("second try", Some(R), None);
    made("paka side view", None, Some("paka"));

    let app = App::new(p.clone(), tmp.path().join("home")).silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 1800.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.step();
    h.get_by_label_contains("01-010").click();
    h.run();

    // The first frame made is still there after the second, and goes back in.
    h.get_by_role_and_label(Role::ComboBox, "Browse existing frames")
        .click();
    h.run();
    h.run();
    // Other things' too, after this shot's.
    assert_eq!(h.query_all_by_label_contains("paka side view").count(), 2);
    // Its Throw out, then the row.
    h.get_all_by_label_contains("first try")
        .last()
        .unwrap()
        .click();
    h.run();
    let frame = p.shot(R).unwrap().frame.unwrap();
    assert_eq!(std::fs::read(p.root.join(frame)).unwrap(), b"first try");

    // The frame's one is in use, so it stays; the other is thrown out, and undo puts it back.
    h.get_by_role_and_label(Role::ComboBox, "Browse existing frames")
        .click();
    h.run();
    h.get_by_label("in use");
    // Each row's Throw out comes before it.
    let rm = h.get_all_by_label_contains("second try").next().unwrap();
    assert!(!rm.accesskit_node().is_disabled());
    let first = h.get_all_by_label_contains("first try").next().unwrap();
    assert!(first.accesskit_node().is_disabled());
    rm.click();
    h.run();
    let left: Vec<String> = p
        .made_images()
        .unwrap()
        .iter()
        .filter_map(|m| m.job.brief.clone())
        .collect();
    assert_eq!(left, ["paka side view", "first try"]);
    h.get_by_label("Undo").click();
    h.run();
    assert_eq!(p.made_images().unwrap().len(), 3);
    assert!(!p.job(&second.id.to_string()).unwrap().discarded);
}

#[test]
fn remake_starts_from_what_a_picture_was_made_with() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    // Made from three pictures, one of them since deleted.
    let mut inputs = vec![];
    for n in ["a", "b", "c"] {
        let f = p.root.join(format!("refs/{n}.png"));
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, n).unwrap();
        inputs.push(format!("refs/{n}.png"));
    }
    inputs.push("refs/gone.png".into());
    let mut j = Job::new(Op::ImageGenerate, "fal", "fal-ai/nano-banana");
    j.shot = Some(R);
    j.inputs = inputs;
    j.brief = Some("ผกายืนริมป่า".into());
    let mut j = p.create_job(j).unwrap();
    let out = p.job_dir(j.id).join("out.png");
    std::fs::write(&out, "the frame").unwrap();
    j.outputs = vec!["out.png".into()];
    j.status = JobStatus::Done;
    p.save_job(&j).unwrap();
    p.set_frame(R, Some(&out)).unwrap();
    p.add_shot_image(R, &out, "the forest").unwrap();
    let mut shot = p.shot(R).unwrap();
    shot.frame_prompt = Some("wide, dusk".into());
    p.save_shot(R, &shot).unwrap();

    let mut app = App::new(p, tmp.path().join("home")).silent();
    app.set_plugins(
        vec![Description {
            name: "fal".into(),
            version: "1".into(),
            protocol: 1,
            env: vec![],
            models: vec![ModelInfo {
                id: "fal-ai/nano-banana".into(),
                op: Op::ImageGenerate,
                ref_image: Some("Image {n}".into()),
                ..ModelInfo::default()
            }],
        }],
        Some(Arc::new(TestHost)),
    );
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 1800.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.step();
    h.get_by_label_contains("01-010").click();
    h.run();
    // Make frame starts from the shot's frame prompt, with no pictures.
    h.get_by_label("Make frame…").click();
    h.run();
    h.get_by_label("None: the image comes from the words alone.");
    let fp = h.get_by_role_and_label(Role::MultilineTextInput, "Frame prompt");
    assert_eq!(fp.value().as_deref(), Some("wide, dusk"));
    h.get_by_label("Close").click();
    h.run();

    // Remake starts from what that frame was made with: what was typed and the pictures still
    // there.
    h.get_by_label("Remake…").click();
    h.run();
    h.get_by_label("Image 3");
    assert!(h.query_by_label("Image 4").is_none());
    let fp = h.get_by_role_and_label(Role::MultilineTextInput, "Frame prompt");
    assert_eq!(fp.value().as_deref(), Some("ผกายืนริมป่า"));
    h.get_by_label("Close").click();
    h.run();

    // A picture of the shot's own made here, the same way.
    h.get_by_label_contains("Remake ").click();
    h.run();
    h.get_by_label("Image 3");
    let prompt = h.get_by_role_and_label(Role::MultilineTextInput, "Image prompt");
    assert_eq!(prompt.value().as_deref(), Some("ผกายืนริมป่า"));
}

#[test]
fn no_more_lines_than_the_video_model_takes() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let model = p.config.model.clone();
    p.add_line(R, "paka", "ไปกันเถอะ", None).unwrap();
    let mut s = p.shot(R).unwrap();
    s.recipe = Some(shotline_core::SPOKEN.into());
    p.save_shot(R, &s).unwrap();
    let video = |mix: bool| Description {
        name: "fal".into(),
        version: "1".into(),
        protocol: 1,
        env: vec![],
        models: vec![ModelInfo {
            id: model.clone(),
            op: Op::VideoGenerate,
            max_audio: Some(1),
            audio_input: true,
            audio_mix: mix,
            ..ModelInfo::default()
        }],
    };
    let show = |mix: bool| {
        let mut app = App::new(p.clone(), tmp.path().join("home"))
            .with_viewer(None)
            .silent();
        app.set_plugins(vec![video(mix)], None);
        let mut h = Harness::builder()
            .with_max_steps(16)
            .with_size(egui::vec2(1000.0, 1200.0))
            .build_ui_state(|ui, app: &mut App| app.show(ui), app);
        h.run();
        h.get_by_label_contains("01-010  Paka walks in").click();
        h.run();
        h.get_by_label("Add line").accesskit_node().is_disabled()
    };
    // One clip a line, and it takes one.
    assert!(show(false));
    // Mixed into one track, any number.
    assert!(!show(true));
}

#[test]
fn a_recorded_line_has_a_player_under_it() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    p.add_line(R, "paka", "ไปกันเถอะ", None).unwrap();
    let long = "เร็วเข้า [stops abruptly] ... [apologetically] ขอโทษนะ ลืมตัวไปหน่อย ".repeat(6);
    p.add_line(R, "paka", long.trim(), None).unwrap();
    let mut s = p.shot(R).unwrap();
    s.recipe = Some(shotline_core::SPOKEN.into());
    p.save_shot(R, &s).unwrap();
    let ffmpeg = std::env::var("SHOTLINE_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    for (line, secs) in [(0, "1"), (1, "0.5")] {
        let mut j = Job::new(Op::AudioTts, "fake", "fake/tts");
        j.shot = Some(R);
        j.who = Some("paka".into());
        j.prompt = Some(p.shot(R).unwrap().lines[line].text.clone());
        let mut j = p.create_job(j).unwrap();
        let out = p.job_dir(j.id).join("out.wav");
        let made = Command::new(&ffmpeg)
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:duration={secs}"))
            .arg(&out)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            eprintln!("skipped: no ffmpeg");
            return;
        }
        j.status = JobStatus::Done;
        j.outputs = vec!["out.wav".into()];
        p.save_job(&j).unwrap();
        p.set_line_audio(R, line, Some(&j)).unwrap();
    }
    let app = App::new(p, tmp.path().join("home"))
        .with_viewer(None)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1300.0, 1200.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    h.get_by_label("Play line 1");
    h.get_by_label("0.0 / 1.0 s");
    h.get_by_label("0.0 s in the shot");
    // The second follows the first, 0.3 s after it ends.
    h.get_by_label("0.0 / 0.5 s");
    h.get_by_label("1.3 s in the shot");
    // Long words are cut short, not drawn over who says them.
    let who = h.get_all_by_label("Who says it").nth(1).unwrap().rect();
    let words = h.get_by_role_and_label(Role::ComboBox, "Line 2").rect();
    assert!(who.right() <= words.left(), "{who:?} {words:?}");
    assert!(words.width() > 150.0, "{words:?}");

    // Picked up, the players stay, so nothing below jumps.
    let below = h.get_by_label("Play line 2").rect();
    let grip = h.get_by_label("Move line 1").rect().center();
    h.event(egui::Event::PointerMoved(grip));
    h.event(egui::Event::PointerButton {
        pos: grip,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Default::default(),
    });
    h.step();
    h.event(egui::Event::PointerMoved(grip + egui::vec2(0.0, 4.0)));
    h.event(egui::Event::PointerMoved(grip + egui::vec2(0.0, 4.0)));
    h.step();
    h.step();
    assert_eq!(h.get_by_label("Play line 2").rect(), below);
    h.event(egui::Event::PointerButton {
        pos: grip + egui::vec2(0.0, 4.0),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Default::default(),
    });
    h.run();
    // Both together: the second ends at 1.8 s.
    h.get_by_label("1.8 s");

    // Dragged to the middle of the first, it waits there.
    let bar = h.get_by_label("Where in line 1").rect();
    let (from, to) = (bar.left_center(), bar.center());
    h.event(egui::Event::PointerMoved(from));
    h.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Default::default(),
    });
    h.step();
    h.event(egui::Event::PointerMoved(to));
    h.step();
    h.event(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Default::default(),
    });
    h.run();
    h.get_by_label("0.5 / 1.0 s");
    h.get_by_label("0.5 s in the shot");
}
