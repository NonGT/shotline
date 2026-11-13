use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use eframe::egui::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use shotline_core::ledger::Context;
use shotline_core::protocol::{
    Change, ClipInfo, Description, EditInfo, EditParams, Edited, ExportParams, ModelInfo, Op,
    OpenParams, Quote, Request, SequenceInfo, TrackInfo, TransitionInfo, VoiceInfo,
};
use shotline_core::{Git, Job, JobStatus, Kind, Project, Shot, ShotRef, Transition, UserConfig};
use shotline_ui::{
    AnchorAsk, App, EditFix, EditWarning, Host, MenuCommand, PluginInfo, Runs, Shell, Tab,
};

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
        .checking_edits_every(EDITS_EVERY)
        .silent();
    // Tall enough for a shot's takes, under its references.
    Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app)
}

fn field(h: &Harness<'static, App>, role: Role, label: &str) -> String {
    h.get_by_role_and_label(role, label)
        .value()
        .unwrap_or_default()
}

/// How often the app looks for the editor's saves, so tests that save for it don't wait long.
const EDITS_EVERY: Duration = Duration::from_millis(50);

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

    // Lines show either way; they're recorded once they're to be spoken.
    h.get_by_label("Add line").click();
    h.run();
    h.get_by_role_and_label(Role::ComboBox, "Lines spoken")
        .click();
    h.run();
    h.get_by_label("By the video model").click();
    h.run();
    h.run();
    // Nothing to record yet, or to bring in a recording of.
    assert!(h.get_by_label("Record…").accesskit_node().is_disabled());
    assert!(
        h.get_by_label("From a file…")
            .accesskit_node()
            .is_disabled()
    );
    let line = h.get_by_role_and_label(Role::TextInput, "Lines");
    line.focus();
    line.type_text("ไปกันเถอะ");
    h.run();
    assert!(
        !h.get_by_label("From a file…")
            .accesskit_node()
            .is_disabled()
    );
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
    h.get_by_label("not recorded");

    h.get_by_label("More for line 1").click();
    h.run();
    h.get_by_label("Delete line").click();
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
    h.get_by_label_contains("01-010  Paka walks in")
        .click_secondary();
    h.run();
    h.get_by_label("Insert a shot after").click();
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

/// Holds the café picture back while set, to see it being made.
static HOLD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// The same for a picture made again.
static HOLD_AGAIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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
            editor: false,
            app: String::new(),
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
                    // Missing for Preferences only, so tests can still shoot with fal.
                    missing: if std::thread::current().name() != Some(shotline_ui::KEYS_THREAD) {
                        vec!["FAL_API_KEY".into()]
                    } else {
                        vec![]
                    },
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
        // Only Rachel has a sample to hear; this one can't be fetched.
        let rachel = VoiceInfo {
            preview: Some("/nowhere/rachel.mp3".into()),
            ..v("abc", "Rachel")
        };
        Ok(vec![rachel, v("def", "Somchai")])
    }

    fn balance(&self, plugin: &str) -> anyhow::Result<String> {
        assert_eq!(plugin, "elevenlabs");
        Ok("3.5 USD left".into())
    }

    fn say_quote(&self, film: &Path, shot: ShotRef, line: usize) -> anyhow::Result<String> {
        assert_eq!(shot, R);
        let words = Project::open(film)?.shot(shot)?.lines[line].text.clone();
        if words.contains("[fail]") {
            anyhow::bail!(
                "elevenlabs: the voice settings aren't valid for this model: stability must be \
                 one of 0.0, 0.5 or 1.0"
            );
        }
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

    fn speech_quote(
        &self,
        _film: &Path,
        _shot: Option<ShotRef>,
        who: &str,
        text: &str,
    ) -> anyhow::Result<String> {
        assert_eq!(who, "paka");
        assert!(!text.is_empty());
        Ok("0.02 USD".into())
    }

    fn effect_quote(
        &self,
        _film: &Path,
        plugin: &str,
        model: &str,
        text: &str,
        length: Option<f64>,
    ) -> anyhow::Result<String> {
        assert_eq!(
            (plugin, model, text, length),
            (
                "elevenlabs",
                "eleven_text_to_sound_v2",
                "a door slams",
                None
            )
        );
        Ok("0.01 USD".into())
    }

    fn effect(
        &self,
        film: &Path,
        shot: Option<ShotRef>,
        _plugin: &str,
        model: &str,
        text: &str,
        _length: Option<f64>,
        at: &str,
    ) -> anyhow::Result<String> {
        assert_eq!(at, "0.01 USD");
        let p = Project::open(film)?;
        let mut j = Job::new(Op::AudioSfx, "elevenlabs", model);
        j.shot = shot;
        j.prompt = Some(text.into());
        let mut j = p.create_job(j)?;
        std::fs::write(p.job_dir(j.id).join("out.mp3"), "mp3")?;
        j.status = JobStatus::Done;
        j.outputs = vec!["out.mp3".into()];
        p.save_job(&j)?;
        Ok(j.id.to_string())
    }

    /// Speech as the CLI leaves it: a done tts job of the words.
    fn speech(
        &self,
        film: &Path,
        shot: Option<ShotRef>,
        who: &str,
        text: &str,
        at: &str,
    ) -> anyhow::Result<String> {
        assert_eq!(at, "0.02 USD");
        let p = Project::open(film)?;
        let mut j = Job::new(Op::AudioTts, "fake", "fake/tts");
        j.shot = shot;
        j.who = Some(who.into());
        j.prompt = Some(text.into());
        let mut j = p.create_job(j)?;
        std::fs::write(p.job_dir(j.id).join("out.wav"), "wav")?;
        j.status = JobStatus::Done;
        j.outputs = vec!["out.wav".into()];
        p.save_job(&j)?;
        Ok(j.id.to_string())
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
        stop: Arc<std::sync::atomic::AtomicBool>,
    ) -> anyhow::Result<String> {
        assert_eq!((shot, at), (R, "0.40 USD"));
        assert!(!translate);
        on("running (40%)".into());
        let p = Project::open(film)?;
        // A shot that says so runs until it's stopped.
        if p.shot(shot)?.prompt.contains("[hold]") {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(10));
            }
            on("running (60%)".into());
            anyhow::bail!("cancelled");
        }
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

    /// An editor plugin's answer, from shared/edits.json when a test writes one.
    /// With a fix for the first warning when editor-fix.txt says what its button says.
    fn sequences(&self, film: &Path) -> (Vec<EditInfo>, Vec<EditWarning>) {
        let edits = std::fs::read_to_string(film.join("shared/edits.json"))
            .map(|t| serde_json::from_str(&t).unwrap())
            .unwrap_or_default();
        let fix = std::fs::read_to_string(film.join("editor-fix.txt"))
            .ok()
            .map(|label| EditFix {
                plugin: "filmcraft".into(),
                id: "update-cli".into(),
                label,
            });
        let warnings = std::fs::read_to_string(film.join("editor-warnings.txt"))
            .map(|t| {
                t.lines()
                    .enumerate()
                    .map(|(i, l)| EditWarning {
                        text: l.into(),
                        fix: fix.clone().filter(|_| i == 0),
                    })
                    .collect()
            })
            .unwrap_or_default();
        (edits, warnings)
    }

    /// Puts it right: the warnings go.
    fn fix_edit(&self, plugin: &str, film: &Path, id: &str) -> anyhow::Result<String> {
        assert_eq!((plugin, id), ("filmcraft", "update-cli"));
        std::fs::remove_file(film.join("editor-warnings.txt"))?;
        Ok("filmcraft-cli is now 0.18.0".into())
    }

    /// Writes a stand-in video where it was asked to. Previews are of the saved file; Render
    /// takes what's open in the editor.
    fn export_edit(&self, _plugin: &str, ask: &ExportParams) -> anyhow::Result<PathBuf> {
        let to_keep = ask.out.parent().is_some_and(|d| d.ends_with("renders"));
        anyhow::ensure!(
            ask.saved != to_keep,
            "saved = {} for {:?}",
            ask.saved,
            ask.out
        );
        // The preview at half size; a render to keep at full.
        let scale = if to_keep { None } else { Some(0.5) };
        anyhow::ensure!(
            ask.scale == scale,
            "scale {:?} for {:?}",
            ask.scale,
            ask.out
        );
        std::fs::create_dir_all(ask.out.parent().unwrap())?;
        std::fs::write(&ask.out, "mp4")?;
        Ok(ask.out.clone())
    }

    fn op_quote(&self, _film: &Path, _plugin: &str, _req: &Request) -> anyhow::Result<String> {
        Ok("0.00 USD".into())
    }

    /// Copies its input next to it as `.done.mp4`, noting what it was given.
    fn run_op(
        &self,
        film: &Path,
        plugin: &str,
        req: &Request,
        at: &str,
    ) -> anyhow::Result<PathBuf> {
        assert_eq!(at, "0.00 USD");
        std::fs::write(
            film.join(format!("ran-{plugin}.json")),
            serde_json::to_string(req)?,
        )?;
        let input = &req.inputs["video"];
        let out = input.with_extension("done.mp4");
        std::fs::copy(input, &out)?;
        Ok(out)
    }

    fn bridge_quote(
        &self,
        _film: &Path,
        _bridge: shotline_core::Ulid,
        _translate: bool,
    ) -> anyhow::Result<String> {
        Ok("0.40 USD".into())
    }

    /// A take of the bridge: its start frame as the video.
    fn bridge_take(
        &self,
        film: &Path,
        bridge: shotline_core::Ulid,
        at: &str,
        _translate: bool,
    ) -> anyhow::Result<PathBuf> {
        assert_eq!(at, "0.40 USD");
        let p = Project::open(film)?;
        let b = p.bridge(bridge)?;
        let mut j = Job::new(Op::VideoGenerate, "fal", &b.model);
        j.bridge = Some(bridge);
        let mut j = p.create_job(j)?;
        let out = p.job_dir(j.id).join("out.mp4");
        std::fs::copy(p.root.join(&b.start), &out)?;
        j.outputs = vec!["out.mp4".into()];
        j.status = JobStatus::Done;
        p.save_job(&j)?;
        Ok(out)
    }

    /// Notes each change asked for, a line each; a change can be taken back with ["back"].
    fn edit_clip(&self, plugin: &str, ask: &EditParams) -> anyhow::Result<Edited> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(ask.film.join(format!("edited-by-{plugin}.jsonl")))?;
        writeln!(f, "{}", serde_json::to_string(ask)?)?;
        let undo = match ask.change {
            Change::Undo { .. } => None,
            _ => Some(serde_json::json!(["back"])),
        };
        Ok(Edited { undo, saved: true })
    }

    /// Notes what it was asked, in the film's folder.
    fn open_edit(&self, plugin: &str, ask: &OpenParams) -> anyhow::Result<()> {
        std::fs::write(
            ask.film.join(format!("opened-by-{plugin}.json")),
            serde_json::to_string(ask)?,
        )?;
        Ok(())
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
            // The shot's own stays as it was until the frame is kept.
            let saved = Project::open(film)?.shot(R)?.frame_prompt;
            assert_eq!(saved, None);
            // What the video gets, to start with.
            let own = film.join("chapters/01-into-the-forest/shots/010/01.png");
            assert_eq!(ask.refs, [film.join("elements/cast/paka/01.png"), own]);
            let out = film.join(".shotline").join("frame.png");
            image::RgbImage::new(8, 8).save(&out)?;
            return Ok(out);
        }
        if ask.prompt == "ผกายืนริมป่า" {
            while HOLD_AGAIN.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(10));
            }
            let out = film.join(".shotline").join("again.png");
            std::fs::write(&out, "made again")?;
            return Ok(out);
        }
        if ask.shot.is_some() {
            assert_eq!((ask.handle.as_str(), ask.prompt.as_str()), ("", "the café"));
            while HOLD.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(ask.refs.is_empty());
            let out = film.join(".shotline").join("cafe.png");
            picture(&out, 288, 512, [92, 104, 84], [28, 30, 26]);
            return Ok(out);
        }
        assert_eq!(
            (ask.handle.as_str(), ask.prompt.as_str(), at),
            ("paka", "side view", "$0.04")
        );
        assert_eq!(ask.refs, [film.join(".shotline").join("dress.png")]);
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
        .checking_edits_every(EDITS_EVERY)
        .silent();
    app.set_plugins(vec![], Some(Arc::new(TestHost)));
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 2400.0))
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
        .checking_edits_every(EDITS_EVERY)
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
            editor: false,
            app: String::new(),
            env: vec![],
            models: vec![model("fal-ai/flux"), model("fal-ai/nano-banana")],
        }],
        Some(Arc::new(TestHost)),
    );
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 2400.0))
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
    assert!(
        h.query_by_role_and_label(Role::TextInput, "Image address")
            .is_none()
    );
    h.get_by_label("Add a reference…").click();
    h.run();
    h.get_by_label("From a web address…").click();
    h.run();
    let url = h.get_by_role_and_label(Role::TextInput, "Image address");
    url.focus();
    url.type_text("https://example.com/dress.png");
    h.run();
    h.get_by_label("Fetch").click();
    assert!(wait_for(&mut h, |h| h
        .query_all_by_label("Image 1")
        .count()
        > 0));
    h.run();
    // Nobody's clothes to keep: it's not one of the cast's anchors.
    assert!(
        h.query_by_label("Outfit exactly as in their reference images")
            .is_none()
    );
    h.get_by_label("Make…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make for $0.04")
        .is_some()));
    assert!(anchors(&p).is_empty());
    h.get_by_label("Make for $0.04").click();
    // Shown first, used on Keep.
    assert!(wait_for(&mut h, |h| h.query_by_label("Keep").is_some()));
    assert!(anchors(&p).is_empty());
    wait(&mut h, 0.3);
    h.run_steps(4);
    h.get_by_label("Keep").click();
    // Its thumbnail loads in the background, slowly on some machines.
    wait(&mut h, 1.0);
    h.run_steps(4);
    assert_eq!(anchors(&p).len(), 1);
    // Done with, so it closes.
    assert!(h.query_by_label("Make…").is_none());
    // The anchor it made doesn't go along with the next one.
    h.get_by_label("Make image…").click();
    h.run();
    h.get_by_label("None: the image comes from the words alone.");
    h.get_by_label("Close").click();
    h.run();
    assert!(h.query_by_label("Make…").is_none());
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
        .checking_edits_every(EDITS_EVERY)
        .silent();
    app.set_plugins(
        vec![Description {
            name: "fal".into(),
            version: "1".into(),
            protocol: 1,
            editor: false,
            app: String::new(),
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
        .with_size(egui::vec2(1200.0, 2400.0))
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
    // A popup of its own, over the page.
    h.get_by_label("New picture for 01-010");
    let prompt = h.get_by_role_and_label(Role::MultilineTextInput, "Image prompt");
    prompt.focus();
    prompt.type_text("the café");
    h.run();
    h.get_by_label("Make…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make for $0.04")
        .is_some()));
    HOLD.store(true, std::sync::atomic::Ordering::Relaxed);
    h.get_by_label("Make for $0.04").click();
    h.run_steps(4);
    // Closed while it's made, it carries on, on the jobs list by Reload.
    h.get_by_label("Close").click();
    h.run_steps(4);
    assert!(h.query_by_label("New picture for 01-010").is_none());
    h.get_by_label("Jobs: 1 running").click();
    h.run_steps(4);
    h.get_by_label_contains("New picture for 01-010 · making · 0:")
        .click();
    h.run_steps(4);
    h.get_by_label("New picture for 01-010");
    h.get_by_label_contains("making the image");
    h.get_by_label("Close").click();
    HOLD.store(false, std::sync::atomic::Ordering::Relaxed);
    // Made out of sight, it goes in place by itself, and off the list.
    assert!(wait_for(&mut h, |_| !p.shot(R).unwrap().images.is_empty()));
    h.run();
    assert!(h.query_by_label_contains("Jobs:").is_none());
    let running = p.root.join(".shotline/cache/running");
    assert_eq!(std::fs::read_dir(running).unwrap().count(), 0);
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
        .query_by_label("Shoot a take for 0.40 USD")
        .is_some()));
    assert_eq!(p.takes(R).unwrap().len(), before);
    // A price is for what was there when it was asked; an edit means asking again.
    let prompt = h.get_by_role_and_label(Role::MultilineTextInput, "Prompt");
    prompt.focus();
    prompt.type_text(" slowly");
    h.run();
    assert!(h.query_by_label("Shoot a take for 0.40 USD").is_none());
    h.get_by_label("Shoot a take…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Shoot a take for 0.40 USD")
        .is_some()));
    h.get_by_label("Shoot a take for 0.40 USD").click();
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
    h.get_by_label("Make…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make for $0.04")
        .is_some()));
    // The popup centres again as it grows.
    wait(&mut h, 0.3);
    h.run();
    h.get_by_label("Make for $0.04").click();
    assert!(wait_for(&mut h, |h| h.query_by_label("Keep").is_some()));
    assert!(p.shot(R).unwrap().frame.is_none());
    wait(&mut h, 0.3);
    h.run_steps(4);
    h.get_by_label("Keep").click();
    h.run();
    let s = p.shot(R).unwrap();
    assert_eq!(s.frame_prompt.as_deref(), Some("at the edge"));
    let frame = s.frame.unwrap();
    // The test host makes no job, so it and the shot's picture read as files: nothing to make
    // again from.
    assert_eq!(h.query_all_by_label("from a file").count(), 2);
    assert!(h.query_by_label("Remake…").is_none());
    // With a frame, a new one is under Replace.
    assert!(h.query_by_label("Make frame…").is_none());
    h.get_by_label("Replace…");
    assert_eq!(frame, "chapters/01-into-the-forest/shots/010.png");
    h.get_by_label("Remove storyboard frame").click();
    h.run();
    assert!(p.shot(R).unwrap().frame.is_none());
    assert!(!p.root.join(&frame).exists());
    h.get_by_label("Undo").click();
    h.run();
    assert_eq!(p.shot(R).unwrap().frame.as_deref(), Some(frame.as_str()));
    assert!(p.root.join(&frame).exists());

    // An anchor left out stays listed, muted, to put back.
    h.get_by_label("Leave out Image 1").click();
    h.run();
    h.run();
    assert_eq!(p.shot(R).unwrap().leave_out, ["elements/cast/paka/01.png"]);
    h.get_by_label("Left out (1)").click();
    h.run();
    h.get_by_label("Put back elements/cast/paka/01.png").click();
    h.run();
    h.run();
    assert!(p.shot(R).unwrap().leave_out.is_empty());
    assert!(h.query_by_label("Left out (1)").is_none());
}

#[test]
fn image_buttons_say_what_they_need() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let app = App::new(p, tmp.path().join("home")).silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.step();
    h.get_by_label_contains("01-010").click();
    h.run();
    // No image model: shown, but off.
    for b in ["Make image…", "Make frame…"] {
        assert!(h.get_by_label(b).accesskit_node().is_disabled(), "{b}");
    }
}

#[test]
fn translate_only_when_it_helps() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let home = tmp.path().join("home");
    let mut app = App::new(p, home.clone())
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
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
                editor: false,
                app: String::new(),
                env: vec![],
                models: vec![model("fal-ai/nano-banana", Op::ImageGenerate)],
            },
            Description {
                name: "venice".into(),
                version: "1".into(),
                protocol: 1,
                editor: false,
                app: String::new(),
                env: vec![var.into()],
                models: vec![model("claude-haiku-5-5", Op::TextGenerate)],
            },
        ],
        Some(Arc::new(TestHost)),
    );
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 2400.0))
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
    h.key_press(egui::Key::Escape);
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
        .checking_edits_every(EDITS_EVERY)
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
            editor: false,
            app: String::new(),
            env: vec![],
            models: vec![tts("eleven_v3"), tts("eleven_flash_v2_5")],
        }],
        Some(voices),
    );
    // Tall enough for the whole voice form without scrolling.
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    // Anchors decode on another thread and repaint when done, which can take a while.
    h.run_ok();
    h.get_by_label("Elements").click();
    h.run_ok();

    let name = h.get_by_role_and_label(Role::TextInput, "New element");
    name.focus();
    name.type_text("Mali");
    h.run_ok();
    h.get_by_label("Add element").click();
    h.run_ok();
    h.run_ok();
    assert_eq!(p.element("mali").unwrap().name, "Mali");
    h.get_by_label("added @mali");

    // It opens; what's typed is saved once the box is left.
    let desc = h.get_by_role_and_label(Role::MultilineTextInput, "Description");
    desc.focus();
    desc.type_text("young woman, short black hair");
    h.run_ok();
    h.get_by_role_and_label(Role::ComboBox, "Plugin").click();
    h.run_ok();
    h.get_by_label("elevenlabs").click();
    // The plugin is asked for its voices in the background; its repaint can land mid-run.
    assert!(wait_for(&mut h, |h| h
        .query_by_role_and_label(Role::ComboBox, "Voice")
        .is_some()));
    h.run_ok();
    h.get_by_role_and_label(Role::ComboBox, "Model").click();
    h.run_ok();
    h.get_by_label("eleven_v3").click();
    h.run_ok();
    // The plugin is asked for its voices in the background.
    assert!(wait_for(&mut h, |h| h
        .query_by_role_and_label(Role::ComboBox, "Voice")
        .is_some()));
    h.get_by_role_and_label(Role::ComboBox, "Voice").click();
    h.run_ok();
    h.get_by_label("Somchai").click();
    h.run_ok();
    // Settings as the model describes them, each with what it does under it.
    h.get_by_label("Higher is steadier");
    h.get_by_role_and_label(Role::CheckBox, "boost").click();
    h.run_ok();
    h.get_by_role_and_label(Role::ComboBox, "mood").click();
    h.run_ok();
    h.get_by_label("angry").click();
    h.run_ok();
    h.run_ok();
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
    h.run_ok();
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
    h.run_ok();
    // Pinched in, a click doesn't close it; pinched back out, it does.
    let big = "elements/cast/paka/01.png big";
    h.get_by_label(big).hover();
    h.run_ok();
    h.event(egui::Event::Zoom(2.0));
    h.run_ok();
    h.get_by_label(big).click();
    h.run_ok();
    h.get_by_label(big).hover();
    h.run_ok();
    h.event(egui::Event::Zoom(0.25));
    h.run_ok();
    h.get_by_label(big).click();
    h.run_ok();
    assert!(h.query_by_label(big).is_none());

    h.get_by_label("elements/cast/paka/01.png").click();
    assert!(wait_for(&mut h, |h| h.query_by_label(big).is_some()));
    h.run_ok();
    h.get_by_label("Close").click();
    h.run_ok();
    assert!(h.query_by_label("elements/cast/paka/01.png big").is_none());

    // A label says what it shows, saved once the box is left; Enter doesn't break the line.
    let label = h.get_by_role_and_label(
        Role::MultilineTextInput,
        "Label of elements/cast/paka/01.png",
    );
    label.focus();
    label.type_text("side view");
    h.run_ok();
    h.key_press(egui::Key::Enter);
    h.run_ok();
    h.key_press(egui::Key::Escape);
    h.run_ok();
    h.run_ok();
    assert_eq!(p.element("paka").unwrap().labels["01.png"], "side view");

    // Taking an anchor off takes its label too, and both come back on undo.
    h.get_by_label("Remove").click();
    h.run_ok();
    assert!(!anchor.exists());
    assert!(p.element("paka").unwrap().labels.is_empty());
    h.get_by_label("Undo").click();
    h.run_ok();
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
        .checking_edits_every(EDITS_EVERY)
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
        .checking_edits_every(EDITS_EVERY)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 2400.0))
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

#[test]
fn a_continue_shot_keeps_its_start_until_told() {
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
    let (video, _) = another_take(&p);
    if clip(&video).is_none() {
        return;
    }
    let first = p.select_take(R, "1").unwrap();
    let third = p.take(R, "3").unwrap();
    let app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    let open = |h: &mut Harness<App>, name: &str| {
        h.get_by_label_contains(name).click();
        h.run();
        h.run();
    };

    // On, it starts from the take in the cut, shown where the storyboard frame was.
    open(&mut h, &format!("{next}  Paka stops"));
    h.get_by_label("Continue from previous shot").click();
    h.run();
    assert!(h.query_by_label("Storyboard frame").is_none());
    h.get_by_label("Previous shot last frame");
    assert!(wait_for(&mut h, |h| h
        .query_by_label("last frame of 01-010#1")
        .is_some()));
    assert!(
        h.query_by_label("Start on #3's last frame instead")
            .is_none()
    );
    open(&mut h, "01-010  Paka walks in");
    assert_eq!(
        p.shot(next).unwrap().continue_from,
        Some(first.id.to_string())
    );

    // Its take can't go, and another in the cut is asked about first.
    assert!(h.get_by_label("Remove #1").accesskit_node().is_disabled());
    h.get_by_label("Use in the cut #3").click();
    wait(&mut h, 0.1);
    h.get_by_label("Use #3 in the cut?");
    h.get_by_label("Use #3").click();
    h.run();
    h.run();
    assert_eq!(p.shot(R).unwrap().selected_take, Some(third.id.to_string()));
    assert_eq!(
        p.shot(next).unwrap().continue_from,
        Some(first.id.to_string())
    );

    open(&mut h, &format!("{next}  Paka stops"));
    h.get_by_label("last frame of 01-010#1");
    h.get_by_label_contains("01-010 plays #3 in the cut now");
    h.get_by_label("Start on #3's last frame instead").click();
    h.run();
    h.run();
    assert!(
        h.query_by_label("Start on #3's last frame instead")
            .is_none()
    );
    open(&mut h, "01-010  Paka walks in");
    assert_eq!(
        p.shot(next).unwrap().continue_from,
        Some(third.id.to_string())
    );
    assert!(!h.get_by_label("Remove #1").accesskit_node().is_disabled());
    assert!(h.get_by_label("Remove #3").accesskit_node().is_disabled());
}

#[test]
fn an_editor_s_sequence_on_the_cut_page() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    app.set_plugins(vec![], Some(Arc::new(TestHost)));
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label("Cut").click();
    wait(&mut h, 0.1);
    // Without an editor plugin, a word on what one would do.
    h.get_by_label_contains("With an editor plugin like filmcraft");
    assert!(h.query_by_label("Open in FilmCraft").is_none());
    h.state_mut()
        .set_plugins(vec![editor_plugin()], Some(Arc::new(TestHost)));
    wait(&mut h, 0.1);

    // As the plugin reads it: a take of the film's, by its path, and a file of its own.
    let take = p.take(R, "1").unwrap();
    let video = p.job_dir(take.output().unwrap().id).join("out.mp4");
    let clip = |name: &str, file: PathBuf, start: f64| ClipInfo {
        name: name.into(),
        file: Some(file),
        start,
        length: 1.0,
        speed: 1.0,
        enabled: true,
        ..ClipInfo::default()
    };
    let track = |clips| TrackInfo {
        name: "1".into(),
        enabled: true,
        clips,
        transitions: vec![],
    };
    let edits = vec![
        EditInfo {
            file: p.root.join("shared/broken.fcproj"),
            name: "broken".into(),
            error: Some("not JSON".into()),
            ..EditInfo::default()
        },
        EditInfo {
            file: p.root.join("shared/edit.fcproj"),
            name: "Rain edit".into(),
            app: "FilmCraft".into(),
            plugin: "filmcraft".into(),
            error: None,
            sequences: vec![SequenceInfo {
                name: "Main edit".into(),
                width: 720,
                height: 1280,
                fps: 24.0,
                video: vec![track(vec![clip("out.mp4", video, 0.0)])],
                audio: vec![track(vec![clip("rain.wav", p.root.join("rain.wav"), 1.0)])],
            }],
        },
    ];
    std::fs::create_dir_all(p.root.join("shared")).unwrap();
    std::fs::write(
        p.root.join("shared/edits.json"),
        serde_json::to_string(&edits).unwrap(),
    )
    .unwrap();
    h.get_by_label("Reload").click();
    // Read on another thread, which can take a while on a slow machine.
    assert!(wait_for(&mut h, |h| h
        .query_by_label("broken.fcproj can't be read")
        .is_some()));
    // The editor's cut is the film's cut now: shown in place of shotline's, with nothing to
    // switch to while it's the only sequence.
    assert!(wait_for(&mut h, |h| h
        .query_by_label_contains("720 × 1280 · 24 fps")
        .is_some()));
    h.get_by_role_and_label(Role::Button, "rain.wav");
    assert!(h.query_by_label("This film's cut").is_none());
    assert!(h.query_by_label("edit.fcproj · Main edit").is_none());
    // Saved again in the editor, it's read again without asking.
    let mut edits = edits;
    edits[1].sequences[0].audio[0]
        .clips
        .push(clip("thunder.wav", p.root.join("thunder.wav"), 2.0));
    std::fs::write(
        p.root.join("shared/edits.json"),
        serde_json::to_string(&edits).unwrap(),
    )
    .unwrap();
    assert!(wait_for(&mut h, |h| h
        .query_by_role_and_label(Role::Button, "thunder.wav")
        .is_some()));
    // To change it, it opens in its own app through the plugin that read it.
    h.get_by_label("Open in FilmCraft").click();
    let opened = p.root.join("opened-by-filmcraft.json");
    assert!(wait_for(&mut h, |_| opened.is_file()));
    let ask: OpenParams = serde_json::from_str(&std::fs::read_to_string(&opened).unwrap()).unwrap();
    assert_eq!(ask.file, Some(p.root.join("shared/edit.fcproj")));
    assert_eq!(ask.sequence.as_deref(), Some("Main edit"));
    assert!(ask.cut.is_none());
    // The take goes by its name and opens its shot.
    h.get_by_role_and_label(Role::Button, "01-010#1").click();
    h.run();
    assert_eq!(h.state().tab(), Tab::Shots);
}

fn editor_plugin() -> Description {
    Description {
        name: "filmcraft".into(),
        version: "0.1.0".into(),
        protocol: 1,
        env: vec![],
        models: vec![],
        editor: true,
        app: "FilmCraft".into(),
    }
}

/// A third take of the shot, done, with a video; the first take's video and the new one's.
fn another_take(p: &Project) -> (PathBuf, PathBuf) {
    let first = p.take(R, "1").unwrap();
    let video = p.job_dir(first.output().unwrap().id).join("out.mp4");
    let mut j = Job::new(Op::VideoGenerate, "fake", "fake/video");
    j.shot = Some(R);
    j.take = Some(p.new_take_id());
    let mut j = p.create_job(j).unwrap();
    std::fs::write(p.job_dir(j.id).join("out.mp4"), "mp4").unwrap();
    j.outputs = vec!["out.mp4".into()];
    j.status = JobStatus::Done;
    p.save_job(&j).unwrap();
    (video, p.job_dir(j.id).join("out.mp4"))
}

fn editor_clip(id: &str, file: &Path, start: f64) -> ClipInfo {
    ClipInfo {
        luts: vec![],
        id: id.into(),
        name: "out.mp4".into(),
        file: Some(file.into()),
        start,
        length: 1.0,
        from: 0.25,
        speed: 1.0,
        enabled: true,
    }
}

/// What the editor plugin reads: one project with "Main edit", these clips on V1.
fn write_edits(p: &Project, clips: Vec<ClipInfo>, transitions: Vec<TransitionInfo>) {
    write_edits_with(p, clips, transitions, vec![]);
}

/// The same, with these clips on A1.
fn write_edits_with(
    p: &Project,
    clips: Vec<ClipInfo>,
    transitions: Vec<TransitionInfo>,
    sounds: Vec<ClipInfo>,
) {
    let audio = if sounds.is_empty() {
        vec![]
    } else {
        vec![TrackInfo {
            name: "1".into(),
            enabled: true,
            clips: sounds,
            transitions: vec![],
        }]
    };
    let edits = vec![EditInfo {
        file: p.root.join("shared/edit.fcproj"),
        name: "Rain edit".into(),
        app: "FilmCraft".into(),
        plugin: "filmcraft".into(),
        error: None,
        sequences: vec![SequenceInfo {
            name: "Main edit".into(),
            width: 720,
            height: 1280,
            fps: 24.0,
            video: vec![TrackInfo {
                name: "1".into(),
                enabled: true,
                clips,
                transitions,
            }],
            audio,
        }],
    }];
    std::fs::create_dir_all(p.root.join("shared")).unwrap();
    std::fs::write(
        p.root.join("shared/edits.json"),
        serde_json::to_string(&edits).unwrap(),
    )
    .unwrap();
}

/// The Cut page showing "Main edit", and the changes the plugin was asked for.
fn on_main_edit(tmp: &Path, p: &Project) -> (Harness<'static, App>, impl Fn() -> Vec<EditParams>) {
    on_main_edit_with(tmp, p, "01-010#1")
}

/// The same, once the clip shows as `take`.
fn on_main_edit_with(
    tmp: &Path,
    p: &Project,
    take: &str,
) -> (Harness<'static, App>, impl Fn() -> Vec<EditParams>) {
    on_main_edit_picking(tmp, p, take, vec![])
}

/// The same, with file dialogs answered by `picks`.
fn on_main_edit_picking(
    tmp: &Path,
    p: &Project,
    take: &str,
    picks: Vec<PathBuf>,
) -> (Harness<'static, App>, impl Fn() -> Vec<EditParams>) {
    let mut app = App::new(p.clone(), tmp.join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .picking(picks)
        .silent();
    let local = Description {
        name: "local".into(),
        version: "0.1.0".into(),
        protocol: 1,
        env: vec![],
        models: vec![ModelInfo {
            id: "ffmpeg/minterpolate".into(),
            op: Op::VideoInterpolate,
            ..ModelInfo::default()
        }],
        editor: false,
        app: String::new(),
    };
    let fal = Description {
        name: "fal".into(),
        version: "0.1.0".into(),
        protocol: 1,
        env: vec![],
        models: vec![ModelInfo {
            id: "minimax/h3-max/reference-to-video".into(),
            op: Op::VideoGenerate,
            min_duration: Some(0.92),
            max_duration: Some(15.0),
            resolutions: vec!["480P".into(), "768P".into(), "1080P".into()],
            start_frame: true,
            end_frame: true,
            ..ModelInfo::default()
        }],
        editor: false,
        app: String::new(),
    };
    let sfx = Description {
        name: "elevenlabs".into(),
        version: "0.1.0".into(),
        protocol: 1,
        env: vec![],
        models: vec![ModelInfo {
            id: "eleven_text_to_sound_v2".into(),
            op: Op::AudioSfx,
            ..ModelInfo::default()
        }],
        editor: false,
        app: String::new(),
    };
    app.set_plugins(
        vec![editor_plugin(), local, fal, sfx],
        Some(Arc::new(TestHost)),
    );
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label("Cut").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_role_and_label(Role::Button, take)
        .is_some()));
    // The player above settles first, a preview or why there's none, so nothing moves under
    // a click.
    assert!(wait_for(&mut h, |h| {
        h.query_by_label_contains("can't make a preview to play")
            .is_some()
            || h.query_all_by_label("edit.fcproj · Main edit").count() == 1
    }));
    wait(&mut h, 0.1);
    let asked = p.root.join("edited-by-filmcraft.jsonl");
    let lines = move || -> Vec<EditParams> {
        std::fs::read_to_string(&asked)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    };
    (h, lines)
}

#[test]
fn swaps_a_take_in_an_editor_and_takes_it_back() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let (video, third) = another_take(&p);
    // A real one, so its length goes with it.
    let real = real_video(&third);
    write_edits(&p, vec![editor_clip("7", &video, 0.0)], vec![]);
    let (mut h, lines) = on_main_edit(tmp.path(), &p);
    // A clip with the editor's id is picked rather than opened.
    h.get_by_role_and_label(Role::Button, "01-010#1").click();
    wait(&mut h, 0.1);
    assert_eq!(h.state().tab(), Tab::Cut);
    h.get_by_label("Open the shot");
    assert!(
        h.query_by_label("Match colour to the clip before")
            .is_some()
    );
    h.get_by_label("Swap take").click();
    wait(&mut h, 0.1);
    h.get_by_label("01-010#3").click();
    assert!(wait_for(&mut h, |_| lines().len() == 1));
    let ask = &lines()[0];
    assert_eq!(
        (ask.clip.as_str(), ask.sequence.as_str()),
        ("7", "Main edit")
    );
    assert_eq!(ask.file, p.root.join("shared/edit.fcproj"));
    let Change::Replace { file, from, length } = &ask.change else {
        panic!("{:?}", ask.change)
    };
    assert_eq!((file, *from), (&third, 0.25));
    // All of the 2 s take from 0.25 s in.
    match length {
        Some(l) => assert!(real && (l - 1.75).abs() < 0.05, "{l}"),
        None => assert!(!real),
    }
    let undo = "Undo swap 01-010#1 for 01-010#3";
    assert!(wait_for(&mut h, |h| h.query_by_label(undo).is_some()));
    // The timeline's panel grows a frame later to fit it.
    wait(&mut h, 0.1);
    h.get_by_label(undo).click();
    assert!(wait_for(&mut h, |_| lines().len() == 2));
    assert_eq!(
        lines()[1].change,
        Change::Undo {
            steps: serde_json::json!(["back"])
        }
    );
    assert!(wait_for(&mut h, |h| h.query_by_label(undo).is_none()));
}

#[test]
fn dissolves_a_join_in_an_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let (video, third) = another_take(&p);
    let clips = vec![editor_clip("7", &video, 0.0), editor_clip("8", &third, 1.0)];
    write_edits(&p, clips.clone(), vec![]);
    let (mut h, lines) = on_main_edit(tmp.path(), &p);
    h.get_by_label("Join 01-010#1 to 01-010#3").click();
    wait(&mut h, 0.1);
    h.get_by_label("Match colour to 01-010#1");
    assert!(h.query_by_label("Straight cut").is_none());
    h.get_by_label("Dissolve").click();
    assert!(wait_for(&mut h, |_| lines().len() == 1));
    assert_eq!(lines()[0].clip, "8");
    assert_eq!(lines()[0].change, Change::Crossfade { frames: 12 });
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Undo 12-frame dissolve into 01-010#3")
        .is_some()));
    // Once the editor has it, it can come off again.
    let fade = TransitionInfo {
        id: "10".into(),
        name: "cross_dissolve".into(),
        from: "7".into(),
        to: "8".into(),
        start: 0.75,
        length: 0.5,
    };
    write_edits(&p, clips, vec![fade]);
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Straight cut")
        .is_some()));
    h.get_by_label("Change dissolve");
    // The tools wrap onto a second line, and the panel grows a frame later to fit them.
    wait(&mut h, 0.1);
    h.get_by_label("Straight cut").click();
    assert!(wait_for(&mut h, |_| lines().len() == 2));
    assert_eq!(lines()[1].change, Change::Crossfade { frames: 0 });
}

/// Two seconds of test pattern at 24 fps, in place of a stand-in file.
fn real_video(path: &Path) -> bool {
    let ffmpeg = std::env::var("SHOTLINE_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    Command::new(ffmpeg)
        .args(["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i"])
        .arg("testsrc=size=160x90:rate=24:duration=2")
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-f", "mp4"])
        .arg(path)
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn puts_a_shot_added_later_into_the_editors_sequence() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let first = p.take(R, "1").unwrap();
    let video = p.job_dir(first.output().unwrap().id).join("out.mp4");
    write_edits(&p, vec![editor_clip("7", &video, 0.0)], vec![]);
    let r = p.add_shot("1", None, Shot::default()).unwrap();
    let file = tmp.path().join("brought.mp4");
    std::fs::write(&file, "mp4").unwrap();
    let t = p.import_take(r, &file).unwrap();
    let (mut h, lines) = on_main_edit(tmp.path(), &p);
    let put = format!("Put {t} in");
    assert!(wait_for(&mut h, |h| h.query_by_label(&put).is_some()));
    h.get_by_label(&put).click();
    assert!(wait_for(&mut h, |_| lines().len() == 1));
    let made = p.job_dir(t.output().unwrap().id).join("take.mp4");
    assert_eq!(lines()[0].clip, "7");
    assert_eq!(
        lines()[0].change,
        Change::Insert {
            file: std::path::absolute(&made).unwrap(),
            after: true,
            sound: true,
        }
    );
    // Selected too, so the Shots page and the film's own cut agree.
    assert_eq!(p.selected_take(r).unwrap().map(|x| x.id), Some(t.id));
}

#[test]
fn a_take_used_on_the_shot_page_goes_in_the_editors_sequence() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let (video, third) = another_take(&p);
    write_edits(&p, vec![editor_clip("7", &video, 0.0)], vec![]);
    let r = p
        .add_shot(
            "1",
            None,
            Shot {
                title: "Paka stops".into(),
                ..Shot::default()
            },
        )
        .unwrap();
    let file = tmp.path().join("brought.mp4");
    std::fs::write(&file, "mp4").unwrap();
    let t = p.import_take(r, &file).unwrap();
    let (mut h, lines) = on_main_edit(tmp.path(), &p);
    h.get_by_label("Shots").click();
    h.run();
    let open = |h: &mut Harness<App>, name: &str| {
        h.get_by_label_contains(name).click();
        h.run();
        h.run();
    };

    // In place of the shot's clip, from the same point.
    open(&mut h, "01-010  Paka walks in");
    h.get_by_label("Use in the cut #3").click();
    assert!(wait_for(&mut h, |_| lines().len() == 1));
    assert_eq!(lines()[0].clip, "7");
    assert_eq!(
        lines()[0].change,
        Change::Replace {
            file: std::path::absolute(&third).unwrap(),
            from: 0.25,
            length: None,
        }
    );

    // A shot the sequence hasn't got goes after the one before it.
    open(&mut h, &format!("{r}  Paka stops"));
    h.get_by_label(&format!("Use in the cut #{}", t.number))
        .click();
    assert!(wait_for(&mut h, |_| lines().len() == 2));
    let made = p.job_dir(t.output().unwrap().id).join("take.mp4");
    assert_eq!(lines()[1].clip, "7");
    assert_eq!(
        lines()[1].change,
        Change::Insert {
            file: std::path::absolute(&made).unwrap(),
            after: true,
            sound: true,
        }
    );
}

#[test]
fn lays_a_sound_over_a_take_in_the_editors_sequence() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let first = p.take(R, "1").unwrap();
    let video = p.job_dir(first.output().unwrap().id).join("out.mp4");
    write_edits(&p, vec![editor_clip("7", &video, 0.0)], vec![]);
    let door = tmp.path().join("Door.wav");
    std::fs::write(&door, "wav").unwrap();
    let (mut h, lines) = on_main_edit_picking(tmp.path(), &p, "01-010#1", vec![door]);
    h.get_by_role_and_label(Role::Button, "01-010#1").click();
    wait(&mut h, 0.1);
    h.get_by_label("Sounds…").click();
    wait(&mut h, 0.1);
    h.get_by_label("Sounds over 01-010#1");
    // Nobody has a voice, so there's no one to say anything.
    h.get_by_label_contains("Give one of the cast a voice");
    h.get_by_label("From a file…").click();
    assert!(wait_for(&mut h, |_| lines().len() == 1));
    let got = p.take_sounds(&first).unwrap();
    assert_eq!(got.len(), 1);
    let (sound, _, copied) = &got[0];
    // From where the clip starts using the take.
    assert_eq!(sound.at, 0.25);
    assert_eq!(lines()[0].clip, "7");
    assert_eq!(
        lines()[0].change,
        Change::Sound {
            file: std::path::absolute(copied).unwrap(),
            at: 0.25,
            length: None,
            gain: 0.0,
        }
    );

    // Once the editor has it, taking it off lifts it out of the sequence too.
    let mut laid = editor_clip("9", copied, 0.0);
    laid.from = 0.0;
    write_edits_with(&p, vec![editor_clip("7", &video, 0.0)], vec![], vec![laid]);
    wait(&mut h, 0.3);
    // A file brought in can't be made again.
    assert!(h.query_by_label("Remake Door.wav").is_none());
    h.get_by_label("in editor");

    // Moved where it lies: laid again from there in the editor, and the old clip lifted.
    let start = h.get_by_label("Start of Door.wav");
    start.focus();
    h.run();
    h.get_by_label("Start of Door.wav").type_text("1");
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(wait_for(&mut h, |_| lines().len() == 3));
    assert_eq!(p.take_sounds(&first).unwrap()[0].0.at, 1.0);
    assert_eq!(
        (lines()[1].clip.as_str(), &lines()[1].change),
        (
            "7",
            &Change::Sound {
                file: std::path::absolute(copied).unwrap(),
                at: 1.0,
                length: None,
                gain: 0.0,
            }
        )
    );
    assert_eq!(
        (lines()[2].clip.as_str(), &lines()[2].change),
        ("9", &Change::Lift)
    );
    // The editor as it is now: only the one laid from 1 s.
    let mut laid = editor_clip("10", copied, 0.0);
    laid.from = 0.0;
    write_edits_with(&p, vec![editor_clip("7", &video, 0.0)], vec![], vec![laid]);
    wait(&mut h, 0.3);
    h.get_by_label("Remove Door.wav").click();
    assert!(wait_for(&mut h, |_| lines().len() == 4));
    assert_eq!(
        (lines()[3].clip.as_str(), &lines()[3].change),
        ("10", &Change::Lift)
    );
    assert!(p.take_sounds(&first).unwrap().is_empty());
}

#[test]
fn one_of_the_cast_says_something_over_a_take() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let first = p.take(R, "1").unwrap();
    let video = p.job_dir(first.output().unwrap().id).join("out.mp4");
    write_edits(&p, vec![editor_clip("7", &video, 0.0)], vec![]);
    p.set_voice(
        "paka",
        shotline_core::Voice {
            plugin: "elevenlabs".into(),
            model: Some("eleven_v3".into()),
            voice_id: Some("v1".into()),
            settings: Default::default(),
        },
    )
    .unwrap();
    let (mut h, lines) = on_main_edit(tmp.path(), &p);
    h.get_by_role_and_label(Role::Button, "01-010#1").click();
    wait(&mut h, 0.1);
    h.get_by_label("Sounds…").click();
    wait(&mut h, 0.1);
    assert!(
        h.get_by_label("Make the voice-over")
            .accesskit_node()
            .is_disabled()
    );
    h.get_by_label("What they say").focus();
    h.get_by_label("What they say").type_text("Over here!");
    h.run();
    h.get_by_label("Make the voice-over").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make for 0.02 USD")
        .is_some()));
    h.get_by_label("Make for 0.02 USD").click();
    assert!(wait_for(&mut h, |_| lines().len() == 1));
    let got = p.take_sounds(&first).unwrap();
    assert_eq!(got.len(), 1);
    let (sound, job, made) = &got[0];
    assert_eq!(
        (sound.at, job.prompt.as_deref()),
        (0.25, Some("Over here!"))
    );
    assert_eq!(
        lines()[0].change,
        Change::Sound {
            file: std::path::absolute(made).unwrap(),
            at: 0.25,
            length: None,
            gain: 0.0,
        }
    );
    h.get_by_label("Play @paka: Over here!");

    // A sound effect, as long as the model makes it, once the editor has the speech.
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make the sound effect")
        .is_some_and(|b| b.accesskit_node().is_disabled())));
    h.get_by_label("What it sounds like").focus();
    h.get_by_label("What it sounds like")
        .type_text("a door slams");
    h.run();
    assert!(wait_for(&mut h, |h| !h
        .get_by_label("Make the sound effect")
        .accesskit_node()
        .is_disabled()));
    h.get_by_label("Make the sound effect").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make for 0.01 USD")
        .is_some()));
    h.get_by_label("Make for 0.01 USD").click();
    assert!(wait_for(&mut h, |_| lines().len() == 2));
    let got = p.take_sounds(&first).unwrap();
    assert_eq!(got.len(), 2);
    let (_, job, made) = &got[1];
    assert_eq!(job.op, Op::AudioSfx);
    assert_eq!(
        lines()[1].change,
        Change::Sound {
            file: std::path::absolute(made).unwrap(),
            at: 0.25,
            length: None,
            gain: 0.0,
        }
    );
    h.get_by_label("Play a door slams");
    // Made again: priced straight away, then in the old one's place, there and in the editor.
    let (_, old, said) = &got[0];
    let mut laid = editor_clip("9", said, 0.0);
    laid.from = 0.0;
    write_edits_with(&p, vec![editor_clip("7", &video, 0.0)], vec![], vec![laid]);
    wait(&mut h, 0.3);
    h.get_by_label("Remake @paka: Over here!").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make for 0.02 USD")
        .is_some()));
    h.get_by_label_contains("in place of @paka: Over here!");
    h.get_by_label("Make for 0.02 USD").click();
    assert!(wait_for(&mut h, |_| lines().len() == 4));
    let got = p.take_sounds(&first).unwrap();
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|(_, j, _)| j.id != old.id));
    let (sound, job, made) = got.iter().find(|(_, j, _)| j.who.is_some()).unwrap();
    assert_eq!(
        (sound.at, job.prompt.as_deref()),
        (0.25, Some("Over here!"))
    );
    let changes: Vec<_> = lines()[2..]
        .iter()
        .map(|l| (l.clip.clone(), l.change.clone()))
        .collect();
    assert!(changes.contains(&(
        "7".into(),
        Change::Sound {
            file: std::path::absolute(made).unwrap(),
            at: 0.25,
            length: None,
            gain: 0.0,
        }
    )));
    assert!(changes.contains(&("9".into(), Change::Lift)));

    // An effect is asked for again with its model.
    // Once the editor has taken the old speech out.
    assert!(wait_for(&mut h, |h| !h
        .get_by_label("Remake a door slams")
        .accesskit_node()
        .is_disabled()));
    h.get_by_label("Remake a door slams").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make for 0.01 USD")
        .is_some()));
    h.get_by_label("Cancel").click();
    h.run();
    assert!(h.query_by_label_contains("in place of").is_none());
}

#[test]
fn a_bridge_in_the_cut_has_sounds_too() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let first = p.take(R, "1").unwrap();
    let video = p.job_dir(first.output().unwrap().id).join("out.mp4");
    let id = p
        .new_bridge(&shotline_core::Bridge {
            sequence: "Main edit".into(),
            length: 2.0,
            ..Default::default()
        })
        .unwrap();
    let mut j = Job::new(Op::VideoGenerate, "fal", "fake/r2v");
    j.bridge = Some(id);
    let mut j = p.create_job(j).unwrap();
    let made = p.job_dir(j.id).join("out.mp4");
    std::fs::write(&made, "mp4").unwrap();
    j.status = JobStatus::Done;
    j.outputs = vec!["out.mp4".into()];
    p.save_job(&j).unwrap();
    let mut bridge = editor_clip("9", &made, 1.0);
    bridge.from = 0.0;
    write_edits(&p, vec![editor_clip("7", &video, 0.0), bridge], vec![]);
    p.set_voice(
        "paka",
        shotline_core::Voice {
            plugin: "elevenlabs".into(),
            model: Some("eleven_v3".into()),
            voice_id: Some("v1".into()),
            settings: Default::default(),
        },
    )
    .unwrap();
    let door = tmp.path().join("Door.wav");
    std::fs::write(&door, "wav").unwrap();
    let (mut h, lines) = on_main_edit_picking(tmp.path(), &p, "01-010#1", vec![door]);
    assert!(wait_for(&mut h, |h| h
        .query_by_role_and_label(Role::Button, "bridge #1")
        .is_some()));
    h.get_by_role_and_label(Role::Button, "bridge #1").click();
    wait(&mut h, 0.1);
    h.get_by_label("Sounds…").click();
    wait(&mut h, 0.1);
    h.get_by_label("Sounds over bridge #1");
    h.get_by_label("From a file…").click();
    assert!(wait_for(&mut h, |_| lines().len() == 1));
    let over = shotline_core::Under::Bridge(id, j.id);
    let got = p.sounds(over).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(p.bridge(id).unwrap().sounds.len(), 1);
    assert_eq!(lines()[0].clip, "9");
    assert_eq!(
        lines()[0].change,
        Change::Sound {
            file: std::path::absolute(&got[0].2).unwrap(),
            at: 0.0,
            length: None,
            gain: 0.0,
        }
    );

    // Speech too, made without a shot.
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make the voice-over")
        .is_some()));
    h.get_by_label("What they say").focus();
    h.get_by_label("What they say").type_text("Over here!");
    h.run();
    h.get_by_label("Make the voice-over").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make for 0.02 USD")
        .is_some()));
    h.get_by_label("Make for 0.02 USD").click();
    assert!(wait_for(&mut h, |_| lines().len() == 2));
    let got = p.sounds(over).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[1].1.shot, None);
    assert_eq!(lines()[1].clip, "9");
}

#[test]
fn bridges_a_join_in_an_editor() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let (video, third) = another_take(&p);
    if !real_video(&video) || !real_video(&third) {
        eprintln!("skipped: no ffmpeg");
        return;
    }
    let clips = vec![editor_clip("7", &video, 0.0), editor_clip("8", &third, 1.0)];
    write_edits(&p, clips, vec![]);
    let (mut h, lines) = on_main_edit(tmp.path(), &p);
    h.get_by_label("Join 01-010#1 to 01-010#3").click();
    wait(&mut h, 0.1);
    h.get_by_label("Bridge…").click();
    // The frames either side of the join, taken from the clips' files.
    assert!(wait_for(&mut h, |_| p
        .bridges()
        .unwrap()
        .first()
        .is_some_and(|(_, b)| !b.end.is_empty())));
    let (id, b) = p.bridges().unwrap().remove(0);
    assert_eq!(
        (b.from.file.as_str(), b.to.file.as_str()),
        (p.rel(&video).as_str(), p.rel(&third).as_str())
    );
    assert!(
        (b.from.at - (1.25 - 0.5 / 24.0)).abs() < 1e-6,
        "{}",
        b.from.at
    );
    assert!((b.to.at - (0.25 + 0.5 / 24.0)).abs() < 1e-6, "{}", b.to.at);
    assert!(p.root.join(&b.start).is_file() && p.root.join(&b.end).is_file());
    assert_eq!(
        (b.sequence.as_str(), b.plugin.as_str()),
        ("Main edit", "fal")
    );
    assert_eq!((b.length, b.resolution.as_deref()), (2.0, Some("768P")));
    h.get_by_label("Bridge 01-010#1 to 01-010#3");

    h.get_by_label("Make a take…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make a take for 0.40 USD")
        .is_some()));
    h.get_by_label("Make a take for 0.40 USD").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Use in the cut #1")
        .is_some()));
    let (n, job) = p.bridge_takes(id).unwrap().remove(0);
    assert_eq!(n, 1);
    // Away from Go, whose tip would cover it.
    h.get_by_label("Bridge 01-010#1 to 01-010#3").hover();
    wait(&mut h, 0.1);
    h.get_by_label("Use in the cut #1").click();
    assert!(wait_for(&mut h, |_| lines().len() == 1));
    let made = p.bridge_video(&job).unwrap();
    assert_eq!(lines()[0].clip, "8");
    assert_eq!(
        lines()[0].change,
        Change::Insert {
            file: std::path::absolute(&made).unwrap(),
            after: false,
            sound: false,
        }
    );
    assert_eq!(p.bridge(id).unwrap().used, Some(job.id));

    // Once the editor has it in, the window says so and the clip is named for it.
    let mut bridge = editor_clip("9", &made, 1.0);
    bridge.from = 0.0;
    bridge.length = 2.0;
    let mut after = editor_clip("8", &third, 3.0);
    after.from = 0.25;
    write_edits(
        &p,
        vec![editor_clip("7", &video, 0.0), bridge, after],
        vec![],
    );
    assert!(wait_for(&mut h, |h| h
        .query_by_label("in the cut")
        .is_some()));
    h.get_by_label("Close").click();
    wait(&mut h, 0.1);
    assert!(h.query_by_label("Bridge 01-010#1 to 01-010#3").is_none());
    assert!(p.bridge(id).is_ok(), "a bridge with takes stays");
    h.get_by_role_and_label(Role::Button, "bridge #1").click();
    wait(&mut h, 0.1);
    h.get_by_label("Bridge…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("in the cut")
        .is_some()));
    assert!(h.get_by_label("Remove #1").accesskit_node().is_disabled());

    // Its picture opens it in a popup of its own; Esc closes just that.
    h.get_by_label("bridge #1 poster").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Save frame")
        .is_some()));
    h.key_press(egui::Key::Escape);
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Save frame")
        .is_none()));
    h.get_by_label("Bridge 01-010#1 to 01-010#3");

    // A second take, not used, comes off the list.
    h.get_by_label("Make a take…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make a take for 0.40 USD")
        .is_some()));
    h.get_by_label("Make a take for 0.40 USD").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Remove #2")
        .is_some()));
    h.get_by_label("Bridge 01-010#1 to 01-010#3").hover();
    wait(&mut h, 0.1);
    // The list scrolls, and the second one is below where it shows.
    h.get_by_label("Remove #2").scroll_to_me();
    wait(&mut h, 0.1);
    h.get_by_label("Remove #2").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Remove #2")
        .is_none()));
    assert_eq!(p.bridge_takes(id).unwrap().len(), 1);
    assert_eq!(p.bridge(id).unwrap().used, Some(job.id));

    // Another take of the shot after it goes in: still the same join, with new frames to take.
    let other = p.take(R, "1").unwrap();
    let other = p.job_dir(other.output().unwrap().id).join("out.mp4");
    assert!(real_video(&other));
    let mut bridge = editor_clip("9", &made, 1.0);
    bridge.from = 0.0;
    bridge.length = 2.0;
    let mut after = editor_clip("10", &other, 3.0);
    after.from = 0.25;
    write_edits(
        &p,
        vec![editor_clip("7", &video, 0.0), bridge, after],
        vec![],
    );
    let changed = "The cut changed here since these frames were taken.";
    assert!(wait_for(&mut h, |h| h.query_by_label(changed).is_some()));
    h.get_by_label("in the cut");
    h.get_by_label_contains("Bridge 01-010#1 to ").hover();
    wait(&mut h, 0.1);
    h.get_by_label("Use the new frames").click();
    assert!(wait_for(&mut h, |_| p.bridge(id).unwrap().to.file == p.rel(&other)));
}

#[test]
fn a_bridge_without_takes_goes_when_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let (video, third) = another_take(&p);
    if !real_video(&video) || !real_video(&third) {
        eprintln!("skipped: no ffmpeg");
        return;
    }
    write_edits(
        &p,
        vec![editor_clip("7", &video, 0.0), editor_clip("8", &third, 1.0)],
        vec![],
    );
    let (mut h, _) = on_main_edit(tmp.path(), &p);
    h.get_by_label("Join 01-010#1 to 01-010#3").click();
    wait(&mut h, 0.1);
    h.get_by_label("Bridge…").click();
    // Free once the frames either side of the join are taken.
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make a take…")
        .is_some_and(|b| !b.accesskit_node().is_disabled())));
    h.get_by_label("Make a take…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make a take for 0.40 USD")
        .is_some()));
    assert_eq!(p.bridges().unwrap().len(), 1);
    h.get_by_label("Close").click();
    wait(&mut h, 0.1);
    assert!(p.bridges().unwrap().is_empty());
    assert!(!p.root.join("bridges").read_dir().unwrap().any(|_| true));
}

#[test]
fn selects_the_takes_an_editor_uses() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    p.select_take(R, "1").unwrap();
    let (_, third) = another_take(&p);
    write_edits(&p, vec![editor_clip("7", &third, 0.0)], vec![]);
    let (mut h, _) = on_main_edit_with(tmp.path(), &p, "01-010#3");
    let says = "FilmCraft uses 01-010#3 (selected: 01-010#1).";
    h.get_by_label(says);
    h.get_by_label("Select these takes").click();
    assert!(wait_for(&mut h, |h| h.query_by_label(says).is_none()));
    assert_eq!(p.selected_take(R).unwrap().unwrap().number, 3);
}

#[test]
fn renders_an_editor_sequence() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let (video, _) = another_take(&p);
    write_edits(&p, vec![editor_clip("7", &video, 0.0)], vec![]);
    let (mut h, _) = on_main_edit(tmp.path(), &p);
    // Its file isn't a real video, so the player says so above first.
    assert!(wait_for(&mut h, |h| h
        .query_by_label_contains("can't make a preview to play")
        .is_some()));
    h.get_by_label("Render").click();
    let made = || {
        std::fs::read_dir(p.root.join("renders"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with("Main_edit-"))
    };
    assert!(wait_for(&mut h, |_| made()));
    assert!(wait_for(&mut h, |h| h
        .query_by_label_contains("Rendered renders/Main_edit-")
        .is_some()));
}

#[test]
fn pinches_and_scrolls_the_timeline() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let (one, two) = another_take(&p);
    write_edits(
        &p,
        vec![editor_clip("7", &one, 0.0), editor_clip("8", &two, 7.0)],
        vec![],
    );
    let (mut h, _) = on_main_edit(tmp.path(), &p);
    let shown =
        |h: &Harness<App>, take: &str| h.query_by_role_and_label(Role::Button, take).is_some();
    assert!(
        shown(&h, "01-010#1") && shown(&h, "01-010#3"),
        "the whole of it"
    );
    assert!(h.query_by_label("Scroll the timeline").is_none());

    // Pinched in on the first clip, the last is out of sight, with a bar to get to it.
    let first = h
        .get_by_role_and_label(Role::Button, "01-010#1")
        .rect()
        .center();
    h.hover_at(first);
    h.run_ok();
    h.event(egui::Event::Zoom(6.0));
    wait(&mut h, 0.1);
    assert!(shown(&h, "01-010#1") && !shown(&h, "01-010#3"));
    let bar = h.get_by_label("Scroll the timeline").rect();
    let end = egui::pos2(bar.right() - 2.0, bar.center().y);
    h.hover_at(end);
    h.drag_at(end);
    h.step();
    h.drop_at(end);
    wait(&mut h, 0.1);
    assert!(!shown(&h, "01-010#1") && shown(&h, "01-010#3"));

    // Pinched all the way out, it's whole again.
    h.hover_at(first);
    h.run_ok();
    h.event(egui::Event::Zoom(0.01));
    wait(&mut h, 0.1);
    assert!(shown(&h, "01-010#1") && shown(&h, "01-010#3"));
    assert!(h.query_by_label("Scroll the timeline").is_none());
}

#[test]
fn the_editor_renders_the_preview_when_asked() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let (video, _) = another_take(&p);
    write_edits(&p, vec![editor_clip("7", &video, 0.0)], vec![]);
    let (mut h, _) = on_main_edit(tmp.path(), &p);
    let full = || {
        std::fs::read_dir(p.root.join(".shotline/cache/sequences"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with("-full.mp4"))
    };
    wait(&mut h, 0.3);
    h.get_by_label_contains("Preview not updated");
    assert!(!full(), "not by itself");
    h.get_by_label("Update preview").click();
    assert!(wait_for(&mut h, |_| full()));
    assert!(wait_for(&mut h, |h| h
        .query_by_label_contains("Preview rendered by")
        .is_some()));
    assert!(h.query_by_label("Update preview").is_none());
}

#[test]
fn a_picked_sound_clip_has_its_tools_above() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let first = p.take(R, "1").unwrap();
    let video = p.job_dir(first.output().unwrap().id).join("out.mp4");
    let door = tmp.path().join("Door.wav");
    std::fs::write(&door, "wav").unwrap();
    p.add_sound((&first).into(), &door, 0.5).unwrap();
    let (_, _, laid) = p.take_sounds(&first).unwrap().remove(0);
    let mut own = editor_clip("8", &video, 0.0);
    own.name = "own".into();
    let mut door_clip = editor_clip("9", &laid, 0.0);
    door_clip.from = 0.0;
    write_edits(&p, vec![editor_clip("7", &video, 0.0)], vec![]);
    let (mut h, _) = on_main_edit(tmp.path(), &p);
    write_edits_with(&p, vec![editor_clip("7", &video, 0.0)], vec![], vec![own]);
    wait(&mut h, 0.3);

    // The take's own sound: picked, with its tools above, not its shot.
    let clips = h
        .get_all_by_role_and_label(Role::Button, "01-010#1")
        .count();
    assert_eq!(clips, 2, "the picture and its sound");
    h.get_all_by_role_and_label(Role::Button, "01-010#1")
        .last()
        .unwrap()
        .click();
    wait(&mut h, 0.1);
    h.get_by_label("01-010#1's own sound");
    h.get_by_label("Open the shot");
    assert!(
        h.query_by_label("Swap take").is_none(),
        "not the picture's tools"
    );
    h.get_by_label("Sounds (1)…").click();
    wait(&mut h, 0.1);
    h.get_by_label("Sounds over 01-010#1");
    h.get_by_label("Door.wav");
    h.get_by_label("Close").click();
    wait(&mut h, 0.1);

    // A sound laid over it says what it is and what it's over.
    door_clip.name = "Door.wav".into();
    write_edits_with(
        &p,
        vec![editor_clip("7", &video, 0.0)],
        vec![],
        vec![door_clip],
    );
    wait(&mut h, 0.3);
    h.get_by_role_and_label(Role::Button, "Door.wav").click();
    wait(&mut h, 0.1);
    assert_eq!(
        h.get_all_by_label("Door.wav").count(),
        2,
        "the clip and its name above"
    );
    h.get_by_label("over 01-010#1");
    h.get_by_label("Sounds over 01-010#1…").click();
    wait(&mut h, 0.1);
    h.get_by_label("Sounds over 01-010#1");
    h.get_by_label("Close").click();
    wait(&mut h, 0.1);
    // Clicked again, it's let go.
    h.get_by_role_and_label(Role::Button, "Door.wav").click();
    wait(&mut h, 0.1);
    assert!(h.query_by_label("over 01-010#1").is_none());
}

#[test]
fn processes_the_used_part_of_a_clip() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let (video, _) = another_take(&p);
    if clip(&video).is_none() {
        return;
    }
    write_edits(&p, vec![editor_clip("7", &video, 0.0)], vec![]);
    let (mut h, lines) = on_main_edit(tmp.path(), &p);
    // The preview shows in the player above first, which moves everything down.
    assert!(wait_for(&mut h, |h| h
        .query_all_by_label("edit.fcproj · Main edit")
        .count()
        == 1));
    wait(&mut h, 0.2);
    h.get_by_role_and_label(Role::Button, "01-010#1").click();
    wait(&mut h, 0.1);
    h.get_by_label("Process").click();
    wait(&mut h, 0.1);
    h.get_by_label("video.interpolate · local ffmpeg/minterpolate")
        .click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Run for 0.00 USD")
        .is_some()));
    h.get_by_label_contains("ffmpeg/minterpolate on 01-010#1");
    h.get_by_label("Run for 0.00 USD").click();
    assert!(wait_for(&mut h, |_| lines().len() == 1));
    let ran: Request =
        serde_json::from_str(&std::fs::read_to_string(p.root.join("ran-local.json")).unwrap())
            .unwrap();
    assert_eq!(ran.model, "ffmpeg/minterpolate");
    let part = &ran.inputs["video"];
    assert!(part.starts_with(p.root.join(".shotline/cache/clip-parts")));
    // The clip uses 1 s from 0.25 s in; with 0.25 s before it and 0.5 s after, that's all of
    // the 1 s file.
    let ff = shotline_media::Ffmpeg::find().unwrap();
    let length = ff.probe(part).unwrap().duration;
    assert!((length - 1.0).abs() < 0.1, "{length}");
    assert_eq!(
        lines()[0].change,
        Change::Replace {
            file: part.with_extension("done.mp4"),
            from: 0.25,
            length: None,
        }
    );
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Undo ffmpeg/minterpolate on 01-010#1")
        .is_some()));
}

#[test]
fn matches_a_join_with_shotlines_lut() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let (video, third) = another_take(&p);
    if clip(&video).is_none() || clip(&third).is_none() {
        return;
    }
    let mut first = editor_clip("7", &video, 0.0);
    first.from = 0.0;
    let mut second = editor_clip("8", &third, 1.0);
    second.from = 0.0;
    write_edits(&p, vec![first, second], vec![]);
    let (mut h, lines) = on_main_edit(tmp.path(), &p);
    h.get_by_label("Join 01-010#1 to 01-010#3").click();
    wait(&mut h, 0.1);
    h.get_by_label("Match with shotline's LUT").click();
    assert!(wait_for(&mut h, |_| lines().len() == 1));
    let ask = &lines()[0];
    assert_eq!(ask.clip, "8");
    let Change::Lut { file: Some(cube) } = &ask.change else {
        panic!("{:?}", ask.change);
    };
    assert!(cube.starts_with(p.root.join(".shotline/cache/luts")));
    assert!(
        std::fs::read_to_string(cube)
            .unwrap()
            .contains("LUT_3D_SIZE 33")
    );
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Undo shotline's colour match on 01-010#3")
        .is_some()));
}

#[test]
fn shows_the_editor_cut_shotline_made_first() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let seq = |name: &str| SequenceInfo {
        name: name.into(),
        width: 720,
        height: 1280,
        fps: 24.0,
        ..SequenceInfo::default()
    };
    let edit = |file: &str, s: &str| EditInfo {
        file: p.root.join(file),
        name: file.into(),
        app: "FilmCraft".into(),
        plugin: "filmcraft".into(),
        error: None,
        sequences: vec![seq(s)],
    };
    let edits = vec![
        edit("shared/other.fcproj", "Someone's"),
        edit("shared/filmcraft/Rain.fcproj", "Rain"),
    ];
    std::fs::create_dir_all(p.root.join("shared")).unwrap();
    let write = || {
        std::fs::write(
            p.root.join("shared/edits.json"),
            serde_json::to_string(&edits).unwrap(),
        )
        .unwrap()
    };
    write();
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    app.set_plugins(vec![editor_plugin()], Some(Arc::new(TestHost)));
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label("Cut").click();
    let shown = "shared/filmcraft/Rain.fcproj";
    assert!(wait_for(&mut h, |h| h.query_by_label(shown).is_some()));
    assert!(h.query_by_label("This film's cut").is_none());
    // Two sequences to pick from; the pick stays when the editor saves again.
    h.get_by_label("other.fcproj · Someone's").click();
    h.run();
    h.get_by_label("shared/other.fcproj");
    write();
    wait(&mut h, 0.3);
    h.get_by_label("shared/other.fcproj");
}

#[test]
fn shows_an_editor_warning_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let says = "filmcraft: filmcraft-cli 0.17.2 doesn't match FilmCraft 0.18.0, so shotline won't \
                change the project while FilmCraft is closed. To update it, get \
                filmcraft-cli-0.18.0-macos-universal.zip from \
                https://github.com/storytold/filmcraft/releases/tag/v0.18.0";
    std::fs::write(p.root.join("editor-warnings.txt"), says).unwrap();
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    app.set_plugins(vec![editor_plugin()], Some(Arc::new(TestHost)));
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label("Cut").click();
    assert!(wait_for(&mut h, |h| h.query_by_label(says).is_some()));
    assert!(h.query_by_label("Update filmcraft-cli to 0.18.0").is_none());
}

#[test]
fn updates_filmcraft_cli_from_its_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let says = "filmcraft: filmcraft-cli 0.17.2 doesn't match FilmCraft 0.18.0";
    std::fs::write(p.root.join("editor-warnings.txt"), says).unwrap();
    std::fs::write(
        p.root.join("editor-fix.txt"),
        "Update filmcraft-cli to 0.18.0",
    )
    .unwrap();
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    app.set_plugins(vec![editor_plugin()], Some(Arc::new(TestHost)));
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label("Cut").click();
    assert!(wait_for(&mut h, |h| h.query_by_label(says).is_some()));
    h.get_by_label("Update filmcraft-cli to 0.18.0").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("filmcraft-cli is now 0.18.0")
        .is_some()));
    assert!(wait_for(&mut h, |h| h.query_by_label(says).is_none()));
}

#[test]
fn starts_an_editor_project_from_the_cut() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let take = p.take(R, "1").unwrap();
    let video = p.job_dir(take.output().unwrap().id).join("out.mp4");
    if clip(&video).is_none() {
        return;
    }
    p.select_take(R, "1").unwrap();
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    app.set_plugins(vec![editor_plugin()], Some(Arc::new(TestHost)));
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label("Cut").click();
    wait(&mut h, 0.1);
    // No project yet: the button is there anyway, and makes one from this cut.
    h.get_by_label_contains("the editor's cut is the film's cut");
    h.get_by_label("Open in FilmCraft").click();
    let opened = p.root.join("opened-by-filmcraft.json");
    assert!(wait_for(&mut h, |_| opened.is_file()));
    let ask: OpenParams = serde_json::from_str(&std::fs::read_to_string(&opened).unwrap()).unwrap();
    assert_eq!(ask.film, p.root);
    assert!(ask.file.is_none());
    let cut = ask.cut.unwrap();
    assert_eq!((cut.width, cut.height, cut.fps), (160, 90, 24.0));
    let v1 = &cut.video[0].clips;
    assert_eq!(v1.len(), 1);
    assert_eq!(v1[0].name, "01-010#1");
    assert_eq!(v1[0].file.as_ref(), Some(&video));
    assert!((v1[0].length - 1.0).abs() < 0.1, "{}", v1[0].length);
    // Its sound under it.
    assert_eq!(cut.audio[0].clips.len(), 1);
    // Every finished take goes in its bins too, by absolute path.
    let names: Vec<&str> = ask.takes.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["01-010#1"]);
    assert_eq!(ask.takes[0].shot, "01-010");
    assert!(ask.takes[0].file.is_absolute());
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
/// At least a few frames too: a panel takes its new size a frame later, and a slow machine may
/// draw only one in the time.
fn wait<S>(h: &mut Harness<S>, secs: f64) {
    let until = Instant::now() + Duration::from_secs_f64(secs);
    let mut frames = 0;
    while Instant::now() < until || frames < 4 {
        h.step();
        frames += 1;
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
        .checking_edits_every(EDITS_EVERY)
        .silent();
    // Tall enough that the takes stay in view under the player.
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size([1000.0, 2400.0])
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
    // Either side can go in the cut from here; the one in it can't be removed.
    h.get_by_label("#1 is in the cut");
    assert!(
        h.get_by_label("Remove this take")
            .accesskit_node()
            .is_disabled()
    );
    let film = Project::open(&tmp.path().join("film")).unwrap();
    let in_cut = |f: &Project| f.shot(r).unwrap().selected_take;
    h.get_by_label("Use #3 in the cut").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("#3 is in the cut")
        .is_some()));
    assert_ne!(in_cut(&film), Some(takes[0].id.to_string()));
    h.get_by_label("Use #1 in the cut").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("#1 is in the cut")
        .is_some()));
    assert_eq!(in_cut(&film), Some(takes[0].id.to_string()));
    h.get_by_label("Close").click();
    wait(&mut h, 0.1);

    h.get_by_label("Cut").click();
    // The cut's player is there already, stopped, with its picture; it can't be closed.
    assert!(wait_for(&mut h, |h| h
        .query_all_by_label("the cut")
        .count()
        == 1));
    assert!(!h.state().is_playing());
    assert!(h.query_by_label("Close player").is_none());
    h.get_by_label("Play").click();
    assert!(wait_for(&mut h, |h| h.state().is_playing()));
    // The cut doesn't loop: it stops after its one second.
    wait(&mut h, 1.0);
    assert!(wait_for(&mut h, |h| !h.state().is_playing()));

    // Clicked on the ruler, the playhead moves there without playing; the take opens its shot.
    h.get_by_role_and_label(Role::Slider, "Playhead").click();
    wait(&mut h, 0.2);
    assert!(!h.state().is_playing());
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
    h.get_by_label("Spent on this film");
    // The total, and all of it thrown away.
    assert_eq!(h.get_all_by_label("0.4 USD").count(), 2);
    h.get_by_label("thrown away");
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
    // The total, and a tile with the same.
    assert!(h.get_all_by_label("-0.1 USD").count() >= 1);
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

    let app = App::new(p.clone(), home)
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 2400.0))
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
        let app = App::new(p.clone(), home.clone())
            .with_viewer(None)
            .checking_edits_every(EDITS_EVERY)
            .silent();
        let mut h = Harness::builder()
            .with_max_steps(16)
            .with_size(egui::vec2(1400.0, 2400.0))
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
        .checking_edits_every(EDITS_EVERY)
        .silent();
    app.set_plugins(
        vec![Description {
            name: "fal".into(),
            version: "1".into(),
            protocol: 1,
            editor: false,
            app: String::new(),
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
        .with_size(egui::vec2(1100.0, 2400.0))
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
    h.get_by_label("By the video model").click();
    h.run();
    h.run();
    assert_eq!(p.shot(R).unwrap().recipe.as_deref(), Some("spoken"));
    // Said as what it does, with the model it lip syncs with and where that's changed.
    h.get_by_role_and_label(Role::ComboBox, "Lines spoken")
        .click();
    h.run();
    h.get_by_label("Lip sync after (wavespeed sync/lipsync-3)")
        .click();
    h.run();
    h.run();
    assert_eq!(p.shot(R).unwrap().recipe.as_deref(), Some("lip-synced"));
    h.get_by_label("The lip sync is priced along with each take.");
    h.get_by_label("Change…").click();
    h.run();
    assert!(h.state().wants_prefs());
    h.get_by_role_and_label(Role::ComboBox, "Lines spoken")
        .click();
    h.run();
    h.get_by_label("No").click();
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
        .checking_edits_every(EDITS_EVERY)
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
            editor: false,
            app: String::new(),
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
        .with_size(egui::vec2(1400.0, 2400.0))
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
        .checking_edits_every(EDITS_EVERY)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 2400.0))
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
        .checking_edits_every(EDITS_EVERY)
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
        .checking_edits_every(EDITS_EVERY)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size([1000.0, 2400.0])
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
fn the_take_in_the_cut_opens_in_its_popup() {
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
    p.save_job(&j).unwrap();
    p.select_take(R, "3").unwrap();

    let app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size([1000.0, 2400.0])
        .allow_missing_glyphs()
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.step();
    h.get_by_label_contains("01-010").click();
    assert!(wait_for(&mut h, |h| h.query_by_label("01-010#3").is_some()));
    assert!(h.query_by_label("Compare with").is_none());
    h.get_by_label("01-010#3").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Compare with")
        .is_some()));
    // The page's own player has one too once its frame is in, so there can be two.
    assert!(wait_for(&mut h, |h| h
        .query_all_by_label("Save frame")
        .next()
        .is_some()));
}

#[test]
fn record_again_keeps_the_old_recording() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    p.set_voice(
        "paka",
        shotline_core::Voice {
            plugin: "elevenlabs".into(),
            model: Some("eleven_v3".into()),
            voice_id: Some("v1".into()),
            settings: Default::default(),
        },
    )
    .unwrap();
    p.add_line(R, "paka", "ไปกันเถอะ", None).unwrap();
    let mut s = p.shot(R).unwrap();
    s.recipe = Some("spoken".into());
    p.save_shot(R, &s).unwrap();
    TestHost.say(&p.root, R, 0, "0.02 USD").unwrap();
    let old = p.shot(R).unwrap().lines[0].audio.clone().unwrap();

    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    app.set_plugins(vec![], Some(Arc::new(TestHost)));
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();

    // The words stay editable; changed, the recording is out of date but still sent.
    h.get_by_label("recorded");
    let words = h.get_by_role_and_label(Role::TextInput, "Line 1");
    words.focus();
    words.type_text(" [whispers]");
    h.run();
    h.get_by_label("out of date");
    h.key_press(egui::Key::Enter);
    wait(&mut h, 0.2);
    assert_eq!(
        p.shot(R).unwrap().lines[0].audio.as_deref(),
        Some(old.as_str())
    );

    // Recorded again, the new one is sent and the old one stays to go back to.
    h.get_by_label("Record again…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Record for 0.02 USD")
        .is_some()));
    h.get_by_label("Record for 0.02 USD").click();
    assert!(wait_for(&mut h, |_| p.shot(R).unwrap().lines[0]
        .audio
        .as_deref()
        != Some(old.as_str())));
    h.run();
    let line = p.shot(R).unwrap().lines[0].clone();
    assert_eq!(line.text, "ไปกันเถอะ [whispers]");
    assert!(p.job(&old).is_ok());
    h.get_by_label("recorded");
}

#[test]
fn a_line_recorded_and_picked() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    p.set_voice(
        "paka",
        shotline_core::Voice {
            plugin: "elevenlabs".into(),
            model: Some("eleven_v3".into()),
            voice_id: Some("v1".into()),
            settings: Default::default(),
        },
    )
    .unwrap();
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
        .checking_edits_every(EDITS_EVERY)
        .silent();
    app.set_plugins(vec![], Some(Arc::new(TestHost)));
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    h.get_by_label("Record…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Record for 0.02 USD")
        .is_some()));
    h.get_by_label("Record for 0.02 USD").click();
    // Recorded, the line says so and sends what was just made.
    assert!(wait_for(&mut h, |h| h.query_by_label("recorded").is_some()));
    h.run();
    let first = p.shot(R).unwrap().lines[0].audio.clone();
    assert!(first.is_some());

    // The one said before is there to pick, words and all.
    h.get_by_label("More for line 1").click();
    h.run();
    h.get_by_label("Recordings by @paka…").click();
    h.run();
    h.get_by_label("ไปกันเถอะ [whispers]").click();
    h.run();
    h.run();
    let s = p.shot(R).unwrap();
    assert_eq!(s.lines[0].text, "ไปกันเถอะ [whispers]");
    assert!(s.lines[0].audio.is_some() && s.lines[0].audio != first);

    // Both are there to go back to; the other shot's after Show all.
    h.get_by_label("More for line 1").click();
    h.run();
    h.get_by_label("Recordings by @paka…").click();
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
    h.get_by_label("More for line 1").click();
    h.run();
    h.get_by_label("Recordings by @paka…").click();
    h.run();
    h.get_by_label("Delete ไปกันเถอะ").click();
    h.run();
    h.run();
    assert!(h.query_by_label("Play ไปกันเถอะ").is_none());
    h.key_press(egui::Key::Escape);
    h.run();
    h.run();
    assert_eq!(field(&h, Role::TextInput, "Line 1"), "ไปกันเถอะ");
    h.get_by_label("not recorded");
    h.get_by_label("Undo").click();
    h.run();
    h.run();
    assert_eq!(p.shot(R).unwrap().lines[0].audio, first);
    h.get_by_label("recorded");

    // Another shot's line sends that one, so it can't be deleted here; it's a way there.
    h.get_by_label("More for line 1").click();
    h.run();
    h.get_by_label("Recordings by @paka…").click();
    h.run();
    h.get_by_label("Show all (1 from other shots)").click();
    h.run();
    assert!(
        h.get_by_label("Delete ใครน่ะ")
            .accesskit_node()
            .is_disabled()
    );
    h.get_by_label(&format!("Go to {other}")).click();
    h.run();
    h.run();
    assert_eq!(field(&h, Role::MultilineTextInput, "Plot"), "ผกาหันกลับมา");

    // A new line can take one recorded before, words and all, from Pick one….
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();
    h.get_by_label("Add line").click();
    h.run();
    h.get_by_label("Pick one…").click();
    h.run();
    h.get_by_label("ไปกันเถอะ [whispers]").click();
    h.run();
    h.run();
    let s = p.shot(R).unwrap();
    assert_eq!(s.lines.len(), 2);
    assert_eq!(s.lines[1].text, "ไปกันเถอะ [whispers]");
    assert!(s.lines[1].audio.is_some() && s.lines[1].audio != s.lines[0].audio);
    assert_eq!(field(&h, Role::TextInput, "Line 2"), "ไปกันเถอะ [whispers]");
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
        .checking_edits_every(EDITS_EVERY)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 2400.0))
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
        .checking_edits_every(EDITS_EVERY)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size([1100.0, 2400.0])
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
    // A busy machine can miss a click made while the pieces are still being laid out.
    let picked = |h: &Harness<App>| {
        h.query_by_role_and_label(Role::RadioButton, "cut from #3")
            .is_some_and(|n| n.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True))
    };
    if !wait_for(&mut h, picked) {
        h.get_by_role_and_label(Role::RadioButton, "cut from #3")
            .click();
        assert!(wait_for(&mut h, picked));
    }
    h.get_by_label("Add piece").click();
    // Its length comes from probing the file on another thread.
    let seen = wait_for(&mut h, |h| {
        h.query_by_label_contains("2. #3  0.00–1.00s").is_some()
    });
    if !seen {
        let rows: Vec<String> = h
            .query_all_by(|n| n.label().is_some_and(|l| l.contains(". #")))
            .map(|n| n.accesskit_node().label().unwrap_or_default().to_string())
            .collect();
        panic!("no 2. #3 row; rows: {rows:?}");
    }
    wait(&mut h, 0.1);
    h.get_by_label_contains("2. #3  0.00–1.00s").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("changing piece 2")
        .is_some()));
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
    h.get_by_label("More for #4").click();
    h.run();
    h.get_by_label("Change pieces").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label_contains("2. #3  0.50–1.00s")
        .is_some()));

    // Unchanged, Esc just closes; with a piece more, it asks first.
    h.key_press(egui::Key::Escape);
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Add piece")
        .is_none()));
    h.get_by_label("More for #4").click();
    h.run();
    h.get_by_label("Change pieces").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Add piece")
        .is_some()));
    wait(&mut h, 0.1);
    h.get_by_label("Add piece").click();
    wait(&mut h, 0.1);
    h.key_press(egui::Key::Escape);
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Discard 3 pieces?")
        .is_some()));
    h.get_by_label("Keep working").click();
    wait(&mut h, 0.1);
    h.get_by_label("Make take");
    h.key_press(egui::Key::Escape);
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Discard 3 pieces?")
        .is_some()));
    h.get_by_label("Discard").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Add piece")
        .is_none()));
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
        // The shot's made as frames, the anchor not.
        j.frame = shot.is_some();
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
        .with_size(egui::vec2(1100.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.step();
    h.get_by_label_contains("01-010").click();
    h.run();

    // The first frame made is still there after the second, and goes back in.
    h.get_by_role_and_label(Role::ComboBox, "Browse existing frames")
        .click();
    h.run();
    h.run();
    // Only frames, until asked for the rest.
    assert_eq!(h.query_all_by_label_contains("paka side view").count(), 0);
    h.get_by_label("Show all (1 more)").click();
    h.run();
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

    let mut app = App::new(p.clone(), tmp.path().join("home")).silent();
    app.set_plugins(
        vec![Description {
            name: "fal".into(),
            version: "1".into(),
            protocol: 1,
            editor: false,
            app: String::new(),
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
        .with_size(egui::vec2(1100.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.step();
    h.get_by_label_contains("01-010").click();
    h.run();
    // A new frame starts from the shot's frame prompt and what the video gets.
    h.get_by_label("Replace…").click();
    h.run();
    h.get_by_label("With a new one…").click();
    h.run();
    h.get_by_label("Make a frame for 01-010");
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
    h.get_by_label("Remake picture 1 of 01-010");

    // Kept, it takes the old one's place and note; undo brings the old one back.
    h.get_by_label("Make…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label_contains("Make for")
        .is_some()));
    h.get_by_label_contains("Make for").click();
    assert!(wait_for(&mut h, |h| h.query_by_label("Keep").is_some()));
    wait(&mut h, 0.3);
    h.run_steps(4);
    h.get_by_label("Keep").click();
    h.run();
    let shot = p.shot(R).unwrap();
    assert_eq!(shot.images.len(), 1);
    assert_eq!(shot.images[0].note, "the forest");
    let file = p.root.join(&shot.images[0].file);
    assert_eq!(std::fs::read(&file).unwrap(), b"made again");
    h.get_by_label("Undo").click();
    h.run();
    let shot = p.shot(R).unwrap();
    assert_eq!(shot.images.len(), 1);
    let file = p.root.join(&shot.images[0].file);
    assert_eq!(std::fs::read(&file).unwrap(), b"the frame");

    // Made out of sight while another picture went in before it, it still takes the place of
    // the one it was made again for.
    HOLD_AGAIN.store(true, std::sync::atomic::Ordering::Relaxed);
    h.get_by_label_contains("Remake ").click();
    h.run();
    h.get_by_label("Make…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label_contains("Make for")
        .is_some()));
    h.get_by_label_contains("Make for").click();
    h.run_steps(4);
    h.get_by_label("Close").click();
    h.run_steps(4);
    let mut shot = p.shot(R).unwrap();
    let mut before = shot.images[0].clone();
    before.file = "refs/a.png".into();
    before.note = "put in first".into();
    shot.images.insert(0, before);
    p.save_shot(R, &shot).unwrap();
    HOLD_AGAIN.store(false, std::sync::atomic::Ordering::Relaxed);
    assert!(wait_for(&mut h, |_| {
        let s = p.shot(R).unwrap();
        std::fs::read(p.root.join(&s.images[1].file)).is_ok_and(|b| b == b"made again")
    }));
    let shot = p.shot(R).unwrap();
    assert_eq!(shot.images.len(), 2);
    assert_eq!(shot.images[0].file, "refs/a.png");
    assert_eq!(shot.images[1].note, "the forest");
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
        editor: false,
        app: String::new(),
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
            .checking_edits_every(EDITS_EVERY)
            .silent();
        app.set_plugins(vec![video(mix)], None);
        let mut h = Harness::builder()
            .with_max_steps(16)
            .with_size(egui::vec2(1000.0, 2400.0))
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
        .checking_edits_every(EDITS_EVERY)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1300.0, 2400.0))
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
    let words = h.get_by_role_and_label(Role::TextInput, "Line 2").rect();
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

#[test]
fn plays_an_editor_sequence() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let take = p.take(R, "1").unwrap();
    let video = p.job_dir(take.output().unwrap().id).join("out.mp4");
    if clip(&video).is_none() {
        return;
    }
    let piece = |name: &str, file: Option<PathBuf>, start: f64| ClipInfo {
        name: name.into(),
        file,
        start,
        length: 1.0,
        speed: 1.0,
        enabled: true,
        ..ClipInfo::default()
    };
    let track = |clips| TrackInfo {
        name: "1".into(),
        enabled: true,
        clips,
        transitions: vec![],
    };
    let edits = vec![EditInfo {
        file: p.root.join("shared/edit.fcproj"),
        name: "Rain edit".into(),
        error: None,
        sequences: vec![SequenceInfo {
            name: "Main edit".into(),
            width: 160,
            height: 90,
            fps: 24.0,
            video: vec![
                track(vec![piece("out.mp4", Some(video.clone()), 0.0)]),
                track(vec![piece("Title", None, 0.5)]),
            ],
            audio: vec![track(vec![piece("out.mp4", Some(video), 0.0)])],
        }],
        ..EditInfo::default()
    }];
    std::fs::create_dir_all(p.root.join("shared")).unwrap();
    std::fs::write(
        p.root.join("shared/edits.json"),
        serde_json::to_string(&edits).unwrap(),
    )
    .unwrap();
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    app.set_plugins(vec![], Some(Arc::new(TestHost)));
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label("Cut").click();
    // A preview is made on its own, then shows in the player: its title, then its picture
    // once the first frame is in.
    assert!(wait_for(&mut h, |h| h
        .query_all_by_label("edit.fcproj · Main edit")
        .count()
        == 1));
    assert!(!h.state().is_playing());
    h.get_by_label("Play").click();
    assert!(wait_for(&mut h, |h| h.state().is_playing()));
    h.get_by_label_contains("Left out of the preview: Title: made in the editor");
    let made: Vec<_> = std::fs::read_dir(p.root.join(".shotline/cache/sequences"))
        .unwrap()
        .collect();
    assert_eq!(made.len(), 1);
    wait(&mut h, 1.2);
    assert!(wait_for(&mut h, |h| !h.state().is_playing()));

    // Made once: the playhead moves in it without making it again.
    h.get_by_role_and_label(Role::Slider, "Playhead").click();
    wait(&mut h, 0.2);
    assert_eq!(
        std::fs::read_dir(p.root.join(".shotline/cache/sequences"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn a_frame_opened_from_the_bridge_shows_above_it() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let (video, third) = another_take(&p);
    if !real_video(&video) || !real_video(&third) {
        eprintln!("skipped: no ffmpeg");
        return;
    }
    write_edits(
        &p,
        vec![editor_clip("7", &video, 0.0), editor_clip("8", &third, 1.0)],
        vec![],
    );
    let (mut h, _) = on_main_edit(tmp.path(), &p);
    h.get_by_label("Join 01-010#1 to 01-010#3").click();
    wait(&mut h, 0.1);
    h.get_by_label("Bridge…").click();
    assert!(wait_for(&mut h, |_| p
        .bridges()
        .unwrap()
        .first()
        .is_some_and(|(_, b)| !b.end.is_empty())));
    let end = p.bridges().unwrap()[0].1.end.clone();
    let big = format!("{end} big");
    // Twice, with a click on the bridge's own window in between, which raises it.
    for _ in 0..2 {
        assert!(wait_for(&mut h, |h| h
            .query_by_role_and_label(Role::Image, "first frame after")
            .is_some()));
        h.get_by_role_and_label(Role::Image, "first frame after")
            .click();
        assert!(wait_for(&mut h, |h| h.query_by_label(&big).is_some()));
        wait(&mut h, 0.2);
        // Clicking the picture closes it, which it only can when it's on top.
        h.get_by_label(&big).click();
        assert!(wait_for(&mut h, |h| h.query_by_label(&big).is_none()));
        h.get_by_label("Bridge 01-010#1 to 01-010#3").click();
        wait(&mut h, 0.1);
    }
}

#[test]
fn the_bridges_takes_get_the_room_the_window_has() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let (video, third) = another_take(&p);
    if !real_video(&video) || !real_video(&third) {
        eprintln!("skipped: no ffmpeg");
        return;
    }
    write_edits(
        &p,
        vec![editor_clip("7", &video, 0.0), editor_clip("8", &third, 1.0)],
        vec![],
    );
    let (mut h, _) = on_main_edit(tmp.path(), &p);
    h.get_by_label("Join 01-010#1 to 01-010#3").click();
    wait(&mut h, 0.1);
    h.get_by_label("Bridge…").click();
    assert!(wait_for(&mut h, |_| p
        .bridges()
        .unwrap()
        .first()
        .is_some_and(|(_, b)| !b.end.is_empty())));
    let id = p.bridges().unwrap()[0].0;
    for _ in 0..6 {
        let mut j = Job::new(
            Op::VideoGenerate,
            "fal",
            "minimax/h3-max/reference-to-video",
        );
        j.bridge = Some(id);
        let mut j = p.create_job(j).unwrap();
        std::fs::copy(&video, p.job_dir(j.id).join("out.mp4")).unwrap();
        j.outputs = vec!["out.mp4".into()];
        j.status = JobStatus::Done;
        p.save_job(&j).unwrap();
    }
    h.get_by_label("Close").click();
    wait(&mut h, 0.1);
    // The join stays picked.
    h.get_by_label("Bridge…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Remove #6")
        .is_some()));
    wait(&mut h, 0.2);
    // All six show without scrolling: each one's button is on the window, not under its edge.
    for n in 1..=6 {
        let at = h.get_by_label(&format!("Remove #{n}")).rect().center();
        assert_eq!(
            h.ctx.layer_id_at(at).map(|l| l.id),
            Some(egui::Id::unique("bridge")),
            "#{n} at {at:?}"
        );
    }
    // And the window still fits: its title is on screen.
    let top = h.get_by_label("Bridge 01-010#1 to 01-010#3").rect();
    assert!(top.top() >= 0.0, "{top:?}");
}

/// A picture to look at: a soft vertical blend, so thumbnails show where they sit.
fn picture(path: &Path, w: u32, h: u32, top: [u8; 3], bottom: [u8; 3]) {
    let img = image::RgbImage::from_fn(w, h, |_, y| {
        let t = y as f32 / h as f32;
        let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
        image::Rgb([
            mix(top[0], bottom[0]),
            mix(top[1], bottom[1]),
            mix(top[2], bottom[2]),
        ])
    });
    img.save(path).unwrap();
}

fn snap(h: &mut Harness<App>, name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/shots");
    std::fs::create_dir_all(&dir).unwrap();
    h.render()
        .unwrap()
        .save(dir.join(format!("{name}.png")))
        .unwrap();
}

/// Renders the main screens to target/shots, to look at after a change to how they look:
/// `cargo test -p shotline-ui --test ui shots -- --ignored`.
#[test]
#[ignore]
fn shots() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let png = tmp.path().join("paka.png");
    picture(&png, 300, 400, [120, 96, 80], [60, 48, 40]);
    p.add_anchor("paka", &png).unwrap();
    let own = tmp.path().join("door.png");
    picture(&own, 400, 300, [70, 84, 70], [30, 36, 30]);
    p.add_shot_image(R, &own, "the doorway").unwrap();
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .silent();
    app.set_plugins(
        vec![Description {
            name: "fal".into(),
            version: "1".into(),
            protocol: 1,
            editor: false,
            app: String::new(),
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
        .with_max_steps(64)
        .with_size(egui::vec2(1280.0, 800.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(4);
    h.get_by_label_contains("01-010  Paka walks in").click();
    wait(&mut h, 0.5);
    h.run_steps(4);
    snap(&mut h, "shot");
    h.get_by_label("Make image…").scroll_to_me();
    h.run_steps(4);
    snap(&mut h, "shot-lower");
    h.get_by_label("More for #1").scroll_to_me();
    wait(&mut h, 0.5);
    h.run_steps(4);
    snap(&mut h, "takes");
    h.get_by_label_contains("01-010  Paka walks in")
        .click_secondary();
    h.run_steps(4);
    snap(&mut h, "shot-menu");
    h.key_press(egui::Key::Escape);
    h.get_by_label("Make image…").scroll_to_me();
    h.run_steps(4);

    h.get_by_label("Make image…").click();
    h.run_steps(4);
    let prompt = h.get_by_role_and_label(Role::MultilineTextInput, "Image prompt");
    prompt.focus();
    prompt.type_text("the café");
    wait(&mut h, 0.3);
    h.run_steps(4);
    snap(&mut h, "maker");
    h.get_by_label("Make…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Make for $0.04")
        .is_some()));
    wait(&mut h, 0.3);
    h.run_steps(4);
    h.get_by_label("Make for $0.04").click();
    assert!(wait_for(&mut h, |h| h.query_by_label("Keep").is_some()));
    wait(&mut h, 0.5);
    h.run_steps(4);
    snap(&mut h, "made");
}

/// The other pages, like `shots`: `cargo test -p shotline-ui --test ui pages -- --ignored`.
#[test]
#[ignore]
fn pages() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let png = tmp.path().join("paka.png");
    picture(&png, 300, 400, [120, 96, 80], [60, 48, 40]);
    p.add_anchor("paka", &png).unwrap();
    let shell = Shell::new(Some(p.clone()), tmp.path().join("home"), tmp.path())
        .with_plugins(vec![], Some(Arc::new(TestHost)))
        .with_git(None)
        .testing();
    let mut h = Harness::builder()
        .with_max_steps(64)
        .with_size(egui::vec2(1280.0, 800.0))
        .build_ui_state(|ui, s: &mut Shell| s.show(ui), shell);
    h.run_steps(4);
    for page in ["Story", "Elements", "Cut", "Cost"] {
        h.get_by_label(page).click();
        wait(&mut h, 0.5);
        h.run_steps(4);
        snap_shell(&mut h, &page.to_lowercase());
        if page == "Elements" {
            h.get_by_label_contains("@paka").click();
            wait(&mut h, 0.5);
            h.run_steps(4);
            snap_shell(&mut h, "element");
        }
    }
    h.get_by_label("Settings").click();
    wait(&mut h, 0.3);
    h.run_steps(4);
    snap_shell(&mut h, "prefs");
    h.key_press(egui::Key::Escape);
    h.state_mut().command(MenuCommand::CloseFilm);
    wait(&mut h, 0.3);
    h.run_steps(4);
    snap_shell(&mut h, "start");
}

fn snap_shell(h: &mut Harness<Shell>, name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/shots");
    std::fs::create_dir_all(&dir).unwrap();
    h.render()
        .unwrap()
        .save(dir.join(format!("{name}.png")))
        .unwrap();
}

#[test]
fn a_running_take_can_be_cancelled() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let mut s = p.shot(R).unwrap();
    s.prompt = "@paka walks into the trees [hold]".into();
    p.save_shot(R, &s).unwrap();
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    app.set_plugins(
        vec![Description {
            name: "fal".into(),
            version: "1".into(),
            protocol: 1,
            editor: false,
            app: String::new(),
            env: vec![],
            models: vec![ModelInfo {
                id: "minimax/h3-max/reference-to-video".into(),
                op: Op::VideoGenerate,
                ref_image: Some("Image {n}".into()),
                ..ModelInfo::default()
            }],
        }],
        Some(Arc::new(TestHost)),
    );
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1200.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(4);
    h.get_by_label_contains("01-010  Paka walks in").click();
    wait(&mut h, 0.3);
    let before = p.takes(R).unwrap().len();
    h.get_by_label("Shoot a take…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Shoot a take for 0.40 USD")
        .is_some()));
    h.get_by_label("Shoot a take for 0.40 USD").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label_contains("running (40%)")
        .is_some()));
    h.get_by_label("Cancel").click();
    // The host is told, and says it stopped.
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Shoot a take…")
        .is_some()));
    h.get_by_label_contains("01-010: cancelled");
    assert!(h.query_by_label("Cancel").is_none());
    assert_eq!(p.takes(R).unwrap().len(), before);
}

#[test]
fn a_failed_take_is_shot_again_after_its_price() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    app.set_plugins(
        vec![Description {
            name: "fal".into(),
            version: "1".into(),
            protocol: 1,
            editor: false,
            app: String::new(),
            env: vec![],
            models: vec![ModelInfo {
                id: "minimax/h3-max/reference-to-video".into(),
                op: Op::VideoGenerate,
                ref_image: Some("Image {n}".into()),
                ..ModelInfo::default()
            }],
        }],
        Some(Arc::new(TestHost)),
    );
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1200.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(4);
    h.get_by_label_contains("01-010  Paka walks in").click();
    wait(&mut h, 0.3);
    // The failed one shows why, and offers to go again, priced first.
    h.get_by_label("content policy");
    h.get_by_label("Shoot again #2").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Shoot a take for 0.40 USD")
        .is_some()));
    assert!(h.query_by_label("Shoot again #1").is_none());
    // Newest first.
    let rows: Vec<f32> = ["Remove #1", "Remove #2"]
        .iter()
        .map(|l| h.get_by_label(l).rect().top())
        .collect();
    assert!(rows[1] < rows[0]);
}

#[test]
fn a_frame_kept_while_continuing_shows_faded() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let png = p.root.join("still.png");
    picture(&png, 90, 160, [90, 90, 90], [40, 40, 40]);
    let next = p
        .add_shot(
            "1",
            None,
            Shot {
                title: "Paka stops".into(),
                transition: Transition::Continue,
                frame: Some("still.png".into()),
                ..Shot::default()
            },
        )
        .unwrap();
    let app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(4);
    h.get_by_label_contains(&format!("{next}  Paka stops"))
        .click();
    wait(&mut h, 0.3);
    h.get_by_label("Unused storyboard frame");
    h.get_by_label("Its storyboard frame isn't used while it continues.");
    h.get_by_label("Remove").click();
    assert!(wait_for(&mut h, |_| p.shot(next).unwrap().frame.is_none()));
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Unused storyboard frame")
        .is_none()));
}

#[test]
fn shots_are_duplicated_moved_and_deleted_from_their_menu() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let b = p
        .add_shot(
            "1",
            None,
            Shot {
                title: "Paka stops".into(),
                ..Shot::default()
            },
        )
        .unwrap();
    let app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 1200.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(4);
    let menu = |h: &mut Harness<App>, row: &str, item: &str| {
        h.get_by_label_contains(row).click_secondary();
        h.run();
        h.get_by_label(item).click();
        h.run();
        h.run();
    };
    let order = |p: &Project| -> Vec<String> {
        p.shots(None)
            .unwrap()
            .iter()
            .map(|(r, s)| format!("{r} {}", s.title))
            .collect()
    };
    let first = order(&p);

    menu(&mut h, &format!("{b}  Paka stops"), "Duplicate");
    assert_eq!(order(&p).len(), 3);
    h.get_by_label_contains("is a copy of");

    menu(&mut h, &format!("{b}  Paka stops"), "Move up");
    let now = order(&p);
    assert!(now[0].ends_with("Paka stops"), "{now:?}");
    assert!(p.shot(b).is_err());

    // The first shot can't go up.
    let top = p.shots(None).unwrap()[0].0;
    h.get_by_label_contains(&format!("{top}  Paka stops"))
        .click_secondary();
    h.run();
    assert!(h.get_by_label("Move up").accesskit_node().is_disabled());
    h.key_press(egui::Key::Escape);
    h.run();

    menu(&mut h, &format!("{top}  Paka stops"), "Delete shot");
    assert!(p.shot(top).is_err());
    assert_eq!(order(&p).len(), 2);

    // Undo puts it back, then puts it back where it was.
    for _ in 0..2 {
        h.get_by_label("Undo").click();
        h.run();
        h.run();
    }
    assert!(p.shot(b).is_ok(), "{:?}", order(&p));
    h.get_by_label("Undo").click();
    h.run();
    h.run();
    assert_eq!(order(&p), first);
}

#[test]
fn lines_say_why_and_ask_before_adding() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    p.add_element(Kind::Cast, "Nana", None, "").unwrap();
    p.add_line(R, "paka", "ไปกันเถอะ", None).unwrap();
    let mut s = p.shot(R).unwrap();
    s.recipe = Some("spoken".into());
    p.save_shot(R, &s).unwrap();
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    app.set_plugins(vec![], Some(Arc::new(TestHost)));
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();

    // No voice: Record… can't, and says where to give one.
    assert!(h.get_by_label("Record…").accesskit_node().is_disabled());
    h.get_by_label("@paka has no voice yet");
    h.get_by_label("Give @paka a voice…").click();
    h.run();
    assert_eq!(h.state().tab(), Tab::Elements);

    p.set_voice(
        "paka",
        shotline_core::Voice {
            plugin: "elevenlabs".into(),
            model: Some("eleven_v3".into()),
            voice_id: Some("abc".into()),
            settings: Default::default(),
        },
    )
    .unwrap();
    h.get_by_label("Shots").click();
    wait(&mut h, 0.3);
    h.get_by_label_contains("01-010  Paka walks in").click();
    h.run();

    // What went wrong asking the price shows whole, under the line, until it's dismissed.
    let words = h.get_by_role_and_label(Role::TextInput, "Line 1");
    words.focus();
    words.type_text(" [fail]");
    h.key_press(egui::Key::Enter);
    h.run();
    h.get_by_label("Record…").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label_contains("stability must be one of 0.0, 0.5 or 1.0")
        .is_some()));
    h.get_by_label("Dismiss").click();
    h.run();
    assert!(h.query_by_label_contains("stability must be").is_none());

    // A new line is priced from its words, and only added once it's paid for.
    h.get_by_label("Add line").click();
    h.run();
    let draft = h.get_by_role_and_label(Role::TextInput, "Lines");
    draft.focus();
    draft.type_text("Over here!");
    h.run();
    h.get_all_by_label("Record…").last().unwrap().click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Record for 0.02 USD")
        .is_some()));
    assert_eq!(p.shot(R).unwrap().lines.len(), 1);
    h.get_by_label("Cancel").click();
    h.run();
    assert_eq!(p.shot(R).unwrap().lines.len(), 1);
    h.get_all_by_label("Record…").last().unwrap().click();
    assert!(wait_for(&mut h, |h| h
        .query_by_label("Record for 0.02 USD")
        .is_some()));
    h.get_by_label("Record for 0.02 USD").click();
    assert!(wait_for(&mut h, |_| p
        .shot(R)
        .unwrap()
        .lines
        .get(1)
        .is_some_and(|l| l.audio.is_some())));
    assert_eq!(p.shot(R).unwrap().lines[1].text, "Over here!");

    // Unpicked from ⋯, it's recorded with each take again.
    wait(&mut h, 0.2);
    h.get_by_label("More for line 2").click();
    h.run();
    h.get_by_label("None: record with each take").click();
    h.run();
    h.run();
    assert_eq!(p.shot(R).unwrap().lines[1].audio, None);
    h.get_by_label("Undo").click();
    h.run();
    h.run();
    assert!(p.shot(R).unwrap().lines[1].audio.is_some());

    // Said by someone else, Paka's recording isn't sent; undo brings it back.
    h.get_all_by_label("Who says it").nth(1).unwrap().click();
    h.run();
    h.get_by_label("@nana").click();
    h.run();
    wait(&mut h, 0.2);
    h.get_by_label_contains("line 2 of 01-010 is @nana's now");
    assert_eq!(p.shot(R).unwrap().lines[1].audio, None);
}

#[test]
fn a_new_voice_flags_lines_recorded_with_the_old() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    p.set_voice(
        "paka",
        shotline_core::Voice {
            plugin: "elevenlabs".into(),
            model: Some("eleven_v3".into()),
            voice_id: Some("abc".into()),
            settings: Default::default(),
        },
    )
    .unwrap();
    p.add_line(R, "paka", "ไปกันเถอะ", None).unwrap();
    TestHost.say(&p.root, R, 0, "0.02 USD").unwrap();
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    app.set_plugins(
        vec![Description {
            name: "elevenlabs".into(),
            version: "1".into(),
            protocol: 1,
            editor: false,
            app: String::new(),
            env: vec![],
            models: vec![ModelInfo {
                id: "eleven_v3".into(),
                op: Op::AudioTts,
                ..ModelInfo::default()
            }],
        }],
        Some(Arc::new(TestHost)),
    );
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1100.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label("Elements").click();
    h.run();
    h.get_by_label_contains("@paka").click();
    assert!(wait_for(&mut h, |h| h
        .query_by_role_and_label(Role::ComboBox, "Voice")
        .is_some()));
    // Rachel can be heard before she's picked; Somchai has no sample.
    h.get_by_label("Hear Rachel");
    h.get_by_role_and_label(Role::ComboBox, "Voice").click();
    h.run();
    assert!(h.query_by_label("Hear Somchai").is_none());
    h.get_by_label("Somchai").click();
    wait(&mut h, 0.3);
    h.run();
    assert_eq!(
        p.element("paka")
            .unwrap()
            .voice
            .unwrap()
            .voice_id
            .as_deref(),
        Some("def")
    );
    h.get_by_label("1 line was recorded with the old voice.");
    h.get_by_label("Show them").click();
    h.run();
    h.get_by_label_contains("01-010 line 1: ไปกันเถอะ").click();
    h.run();
    h.run();
    assert_eq!(h.state().tab(), Tab::Shots);
}

/// The Lines panel and its recordings popup, like `shots`.
#[test]
#[ignore]
fn lines_look() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    p.add_element(Kind::Cast, "Nana", None, "").unwrap();
    p.set_voice(
        "paka",
        shotline_core::Voice {
            plugin: "elevenlabs".into(),
            model: Some("eleven_v3".into()),
            voice_id: Some("abc".into()),
            settings: Default::default(),
        },
    )
    .unwrap();
    p.add_line(R, "paka", "ไปกันเถอะ", None).unwrap();
    p.add_line(R, "paka", "Over here!", Some(2.0)).unwrap();
    p.add_line(R, "nana", "Wait for me", None).unwrap();
    TestHost.say(&p.root, R, 0, "0.02 USD").unwrap();
    TestHost.say(&p.root, R, 1, "0.02 USD").unwrap();
    let mut s = p.shot(R).unwrap();
    s.recipe = Some("spoken".into());
    s.lines[1].text = "Over here, quick!".into();
    p.save_shot(R, &s).unwrap();
    let mut app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    app.set_plugins(vec![], Some(Arc::new(TestHost)));
    let mut h = Harness::builder()
        .with_max_steps(64)
        .with_size(egui::vec2(1280.0, 800.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(4);
    h.get_by_label_contains("01-010  Paka walks in").click();
    wait(&mut h, 0.3);
    h.get_by_label("Add line").scroll_to_me();
    h.run_steps(4);
    snap(&mut h, "lines");
    h.get_by_label("More for line 1").click();
    h.run_steps(4);
    snap(&mut h, "line-menu");
    h.get_by_label("Recordings by @paka…").click();
    h.run_steps(6);
    snap(&mut h, "recordings");
}

#[test]
fn a_sound_that_cant_be_remade_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let p = film(tmp.path());
    let first = p.take(R, "1").unwrap();
    // Made by a model that isn't installed now.
    let mut j = Job::new(Op::AudioSfx, "elevenlabs", "old_sfx_model");
    j.shot = Some(R);
    j.prompt = Some("rain on a roof".into());
    let mut j = p.create_job(j).unwrap();
    std::fs::write(p.job_dir(j.id).join("out.wav"), "wav").unwrap();
    j.status = JobStatus::Done;
    j.outputs = vec!["out.wav".into()];
    p.save_job(&j).unwrap();
    p.add_sound_job((&first).into(), &j.id.to_string(), 0.0)
        .unwrap();
    p.select_take(R, "1").unwrap();
    let app = App::new(p.clone(), tmp.path().join("home"))
        .with_viewer(None)
        .checking_edits_every(EDITS_EVERY)
        .silent();
    let mut h = Harness::builder()
        .with_max_steps(16)
        .with_size(egui::vec2(1000.0, 2400.0))
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run();
    h.get_by_label_contains("01-010  Paka walks in").click();
    wait(&mut h, 0.2);
    h.get_by_label("Sounds (1)… #1").click();
    wait(&mut h, 0.2);
    assert!(
        h.get_by_label("Remake rain on a roof")
            .accesskit_node()
            .is_disabled()
    );
    h.get_by_label_contains("elevenlabs · old_sfx_model isn't installed");
    // No editor: it's said once where the sounds go, not tagged on each row.
    h.get_by_label("Mixed in when shotline renders the cut.");
    assert!(h.query_by_label("render only").is_none());
    // No plugin to make effects with: says so instead of leaving the section out.
    h.get_by_label_contains("No installed plugin makes sound effects");
}
