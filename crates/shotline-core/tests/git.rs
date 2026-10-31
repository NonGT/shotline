use shotline_core::{Git, Project, Shot, ShotRef};

fn git() -> Option<Git> {
    let g = Git::find().map(|g| g.with_identity("Test", "test@example.com"));
    if g.is_none() {
        eprintln!("skipped: no git");
    }
    g
}

#[test]
fn commits_the_film_and_nothing_else() {
    let Some(git) = git() else { return };
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("notes.txt");
    let film = tmp.path().join("film");
    let p = Project::init(&film, Some("Rain")).unwrap();
    assert!(!git.is_repo(&film));

    // The film is one folder of a bigger repository.
    git.init(tmp.path(), "start").unwrap();
    assert!(git.is_repo(&film));
    assert!(git.changes(&film).unwrap().is_empty());

    p.add_chapter("Rain").unwrap();
    let r: ShotRef = p
        .add_shot(
            "1",
            None,
            Shot {
                plot: "ฝนตก".into(),
                ..Shot::default()
            },
        )
        .unwrap();
    std::fs::write(&outside, "not the film's").unwrap();
    let changes = git.changes(&film).unwrap();
    assert!(changes.contains(&"chapters/01-rain/shots/010.toml".to_string()));
    assert!(changes.iter().all(|c| !c.contains("notes")));
    assert_eq!(
        shotline_core::change_summary(&changes),
        format!("edit chapter 01 and {r}")
    );

    let hash = git.commit(&film, "edit 01-010").unwrap();
    assert!(!hash.is_empty());
    assert!(git.changes(&film).unwrap().is_empty());
    assert_eq!(git.changes(tmp.path()).unwrap(), vec!["notes.txt"]);
}
