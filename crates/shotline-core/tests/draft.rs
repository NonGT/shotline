use std::fs;

use shotline_core::{
    BuiltDraft, Catalog, Draft, DraftFor, Kind, Line, Project, Shot, ShotRef, build_draft,
    finish_draft,
};
use tempfile::TempDir;

const TEXT: &str = r#"
[[model]]
plugin = "venice"
id = "claude-haiku-5-5"
op = "text.generate"
"#;

fn film() -> (TempDir, Project, ShotRef) {
    let dir = TempDir::new().unwrap();
    let p = Project::init(dir.path(), Some("Forest")).unwrap();
    p.add_element(Kind::Cast, "ผกา", Some("paka"), "หัวหน้าทีม อายุ 30")
        .unwrap();
    p.add_element(Kind::Set, "Deep forest", None, "").unwrap();
    p.add_chapter("Into the forest").unwrap();
    fs::write(dir.path().join("models.toml"), TEXT).unwrap();
    let r = p
        .add_shot(
            "1",
            None,
            Shot {
                plot: "ผกาเดินเข้าป่าตอนเย็น".into(),
                prompt: String::new(),
                duration: Some(6.0),
                lines: vec![Line {
                    who: "paka".into(),
                    text: "ไปกันเถอะ".into(),
                    at: None,
                    audio: None,
                }],
                ..Shot::default()
            },
        )
        .unwrap();
    (dir, p, r)
}

fn draft(p: &Project, what: DraftFor) -> shotline_core::Result<BuiltDraft> {
    build_draft(p, &p.catalog(Catalog::builtin())?, &what, None, None)
}

#[test]
fn video_draft_has_the_plot_and_elements_but_not_the_lines() {
    let (_dir, p, r) = film();
    let b = draft(&p, DraftFor::Shot(r, Draft::Video)).unwrap();
    assert_eq!(
        (b.plugin.as_str(), b.model.as_str()),
        ("venice", "claude-haiku-5-5")
    );
    let t = &b.text;
    assert!(t.contains("in 6 seconds"), "{t}");
    assert!(t.contains("@paka: ผกา (cast), หัวหน้าทีม อายุ 30"), "{t}");
    assert!(t.contains("@deep-forest: Deep forest (set)"), "{t}");
    assert!(t.contains("Speaking in this shot: @paka"), "{t}");
    assert!(t.contains("improve on: none"), "{t}");
    assert!(t.ends_with("ผกาเดินเข้าป่าตอนเย็น"), "{t}");
    assert!(!t.contains("ไปกันเถอะ"), "lines stay out: {t}");

    let b = draft(&p, DraftFor::Shot(r, Draft::Image)).unwrap();
    assert!(b.text.contains("one still frame"), "{}", b.text);
}

#[test]
fn translations() {
    let (dir, p, r) = film();
    let b = draft(&p, DraftFor::Shot(r, Draft::Translate)).unwrap();
    assert!(b.text.starts_with("Translate the text below"));
    assert!(b.text.ends_with("\n\nผกาเดินเข้าป่าตอนเย็น"));
    assert!(!b.text.contains("@paka"));

    let b = draft(&p, DraftFor::Element("paka".into())).unwrap();
    assert!(b.text.ends_with("\n\nหัวหน้าทีม อายุ 30"));
    let e = draft(&p, DraftFor::Element("deep-forest".into())).unwrap_err();
    assert!(e.to_string().contains("no description"), "{e}");

    // A project's own template wins.
    fs::create_dir_all(dir.path().join("prompts")).unwrap();
    fs::write(dir.path().join("prompts/translate.md"), "EN: {text}").unwrap();
    let b = draft(&p, DraftFor::Element("paka".into())).unwrap();
    assert_eq!(b.text, "EN: หัวหน้าทีม อายุ 30");
}

#[test]
fn needs_a_plot_and_a_text_model() {
    let (_dir, p, _) = film();
    let r = p.add_shot("1", None, Shot::default()).unwrap();
    let e = draft(&p, DraftFor::Shot(r, Draft::Video)).unwrap_err();
    assert!(e.to_string().contains("no plot"), "{e}");

    let e = build_draft(
        &p,
        &p.catalog(Catalog::builtin()).unwrap(),
        &DraftFor::Shot(r, Draft::Video),
        None,
        Some("no-such-model"),
    )
    .unwrap_err();
    assert!(e.to_string().contains("no-such-model"), "{e}");
}

#[test]
fn finishing() {
    let (_dir, p, _) = film();
    assert_eq!(
        finish_draft(&p, "```text\n@paka walks.\n```\n").unwrap(),
        "@paka walks."
    );
    assert_eq!(
        finish_draft(&p, "\"@paka by @deep-forest\"").unwrap(),
        "@paka by @deep-forest"
    );
    let e = finish_draft(&p, "@pakaa walks").unwrap_err();
    assert!(e.to_string().contains("@pakaa"), "{e}");
    assert!(finish_draft(&p, "  \n").is_err());
}

#[test]
fn drafts_know_the_story() {
    let (dir, p, r) = film();
    // A new film's story.md is only headings.
    let b = draft(&p, DraftFor::Shot(r, Draft::Video)).unwrap();
    assert!(
        b.text.contains("don't retell it:\nnot written yet\n"),
        "{}",
        b.text
    );

    fs::write(
        dir.path().join("story.md"),
        "# Forest\n\n## Logline\nผกาพาทีมหลงป่า\n\n## World\n\n## Synopsis\n<!-- todo -->\nทีมเดินเข้าป่า\nแล้วหลงทาง\n",
    )
    .unwrap();
    assert_eq!(
        p.story_brief().unwrap(),
        "Logline: ผกาพาทีมหลงป่า\n\nSynopsis: ทีมเดินเข้าป่า\nแล้วหลงทาง"
    );
    for how in [Draft::Video, Draft::Image] {
        let t = draft(&p, DraftFor::Shot(r, how)).unwrap().text;
        assert!(t.contains("Logline: ผกาพาทีมหลงป่า"), "{t}");
        assert!(t.ends_with("ผกาเดินเข้าป่าตอนเย็น"), "{t}");
    }
    let t = draft(&p, DraftFor::Shot(r, Draft::Translate)).unwrap().text;
    assert!(!t.contains("หลงป่า"), "{t}");

    fs::write(dir.path().join("story.md"), "ก".repeat(3000)).unwrap();
    let brief = p.story_brief().unwrap();
    assert_eq!(brief.chars().count(), 1501);
    assert!(brief.ends_with("ก…"));
}
