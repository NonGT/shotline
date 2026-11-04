use std::sync::Arc;

use shotline_core::protocol::Op;
use shotline_core::{Cost, Job, JobStatus, Project, Shot, ShotRef, TestClock};
use tempfile::TempDir;

fn project() -> (TempDir, Project, ShotRef) {
    let dir = TempDir::new().unwrap();
    let clock = TestClock::new("2026-10-30T10:00:00Z".parse().unwrap());
    let p = Project::init(dir.path(), None)
        .unwrap()
        .with_clock(Arc::new(clock));
    p.add_chapter("One").unwrap();
    let r = p
        .add_shot(
            "1",
            None,
            Shot {
                prompt: "x".into(),
                ..Shot::default()
            },
        )
        .unwrap();
    (dir, p, r)
}

/// A take of `steps` jobs; the last one ends with `last`.
fn take(p: &Project, r: ShotRef, steps: u32, last: JobStatus) {
    let take = p.new_take_id();
    for step in 0..steps {
        let mut job = Job::new(Op::VideoGenerate, "fake", "fake/video");
        job.shot = Some(r);
        job.take = Some(take);
        job.step = step;
        let mut job = p.create_job(job).unwrap();
        job.status = if step + 1 == steps {
            last
        } else {
            JobStatus::Done
        };
        job.cost = Some(Cost {
            amount: 0.5,
            unit: "USD".into(),
        });
        p.save_job(&job).unwrap();
    }
}

#[test]
fn job_files_are_deterministic_with_a_test_clock() {
    let (dir, p, r) = project();
    let mut job = Job::new(Op::AudioTts, "fake", "fake/tts");
    job.shot = Some(r);
    job.params.insert("voice".into(), "kan".into());
    let job = p.create_job(job).unwrap();
    let text =
        std::fs::read_to_string(dir.path().join(format!("jobs/{}/job.toml", job.id))).unwrap();
    insta::assert_snapshot!(text);
    assert_eq!(
        p.job(&job.id.to_string()[..20].to_lowercase()).unwrap(),
        job
    );
}

#[test]
fn finished_jobs_cant_change() {
    let (_d, p, _) = project();
    let mut job = p
        .create_job(Job::new(Op::VideoUpscale, "fake", "fake/up"))
        .unwrap();
    job.status = JobStatus::Running;
    p.save_job(&job).unwrap();
    job.status = JobStatus::Failed;
    job.error = Some("boom".into());
    p.save_job(&job).unwrap();
    let saved = p.job(&job.id.to_string()).unwrap();
    assert!(saved.finished.is_some());

    job.status = JobStatus::Done;
    let e = p.save_job(&job).unwrap_err().to_string();
    assert!(e.contains("is failed and can't change"), "{e}");
}

#[test]
fn takes_group_and_number_jobs() {
    let (_d, p, r) = project();
    take(&p, r, 3, JobStatus::Done);
    take(&p, r, 2, JobStatus::Failed);
    take(&p, r, 1, JobStatus::Done);

    let takes = p.takes(r).unwrap();
    assert_eq!(takes.len(), 3);
    assert_eq!(
        takes.iter().map(|t| t.to_string()).collect::<Vec<_>>(),
        ["01-010#1", "01-010#2", "01-010#3"]
    );
    assert_eq!(takes[0].jobs.len(), 3);
    assert_eq!(
        takes[0].jobs.iter().map(|j| j.step).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(
        takes[0].costs(),
        [Cost {
            amount: 1.5,
            unit: "USD".into()
        }]
    );
    assert!(takes[0].output().is_some());
    assert_eq!(takes[1].status(), JobStatus::Failed);
    assert!(takes[1].output().is_none());

    let by_prefix = p.take(r, &takes[2].id.to_string()[..24]).unwrap();
    assert_eq!(by_prefix.number, 3);
    assert!(p.take(r, "9").is_err());
}

#[test]
fn select_only_done_takes() {
    let (_d, p, r) = project();
    take(&p, r, 1, JobStatus::Done);
    take(&p, r, 1, JobStatus::Failed);

    let e = p.select_take(r, "#2").unwrap_err().to_string();
    assert!(e.contains("01-010#2 is failed"), "{e}");

    let t = p.select_take(r, "1").unwrap();
    assert_eq!(p.shot(r).unwrap().selected_take, Some(t.id.to_string()));
    assert_eq!(p.selected_take(r).unwrap().unwrap().id, t.id);
}

#[test]
fn removed_takes_keep_the_others_numbers() {
    let (_d, p, r) = project();
    take(&p, r, 1, JobStatus::Done);
    take(&p, r, 2, JobStatus::Done);
    take(&p, r, 1, JobStatus::Running);
    p.select_take(r, "2").unwrap();

    let gone = p.remove_take(r, "2").unwrap();
    assert_eq!(p.shot(r).unwrap().selected_take, None);
    let left: Vec<usize> = p.takes(r).unwrap().iter().map(|t| t.number).collect();
    assert_eq!(left, [1, 3]);
    // Kept on disk, as the ledger points at it.
    assert!(p.job_dir(gone.jobs[0].id).join("job.toml").is_file());

    let e = p.remove_take(r, "3").unwrap_err().to_string();
    assert!(e.contains("still running"), "{e}");
}

#[test]
fn takes_made_from_pieces_or_brought_in() {
    use shotline_core::{Join, Piece, pieces_of};
    let (dir, p, r) = project();
    let file = dir.path().join("cut in resolve.mov");
    std::fs::write(&file, b"mov").unwrap();
    let t1 = p.import_take(r, &file).unwrap();
    assert_eq!(t1.number, 1);
    let j = t1.output().unwrap();
    assert_eq!(j.cost, None);
    assert_eq!(j.brief.as_deref(), Some("cut in resolve.mov"));
    let video = p.take_video(t1.id).unwrap();
    assert!(video.ends_with("take.mov"));
    assert_eq!(std::fs::read(&video).unwrap(), b"mov");

    let pieces = vec![
        Piece::parse(&p, r, "1:0-1.5").unwrap(),
        Piece::parse(&p, r, "fade:#1:2-3").unwrap(),
    ];
    assert_eq!(pieces[1].join, Join::Fade);
    assert_eq!((pieces[1].from, pieces[1].to), (2.0, 3.0));
    assert!(Piece::parse(&p, r, "1:2").is_err());
    let t2 = p
        .splice_take(r, &pieces, |sources, out| {
            assert_eq!(sources[0].0, video);
            std::fs::write(out, b"mp4").map_err(|e| e.to_string())
        })
        .unwrap();
    assert_eq!(t2.number, 2);
    assert_eq!(pieces_of(&t2), Some(pieces.clone()));
    assert_eq!(pieces_of(&t1), None);
    p.select_take(r, "2").unwrap();

    // A failed one stays as a failed take, like a model's.
    let err = p.splice_take(r, &pieces, |_, _| Err("ffmpeg fell over".into()));
    assert!(err.unwrap_err().to_string().contains("ffmpeg fell over"));
    let t3 = p.take(r, "3").unwrap();
    assert_eq!(t3.status(), JobStatus::Failed);
}

#[test]
fn made_images_and_what_they_were_for() {
    let (dir, p, r) = project();
    p.add_element(shotline_core::Kind::Cast, "Paka", None, "")
        .unwrap();
    let made = |bytes: &[u8], shot: Option<ShotRef>, who: Option<&str>| {
        let mut j = Job::new(Op::ImageGenerate, "fake", "fake/image");
        j.shot = shot;
        j.who = who.map(Into::into);
        let mut j = p.create_job(j).unwrap();
        std::fs::write(p.job_dir(j.id).join("out.png"), bytes).unwrap();
        j.outputs = vec!["out.png".into()];
        j.status = JobStatus::Done;
        p.save_job(&j).unwrap();
        j
    };
    let frame = made(b"frame", Some(r), None);
    made(b"side", None, Some("paka"));
    // From before jobs said whose anchor it was; still one of @paka's.
    let old = made(b"front", None, None);
    let f = dir.path().join("front.png");
    std::fs::write(&f, b"front").unwrap();
    p.add_anchor("paka", &f).unwrap();
    made(b"lost", None, None);

    let all = p.made_images().unwrap();
    let what: Vec<(Option<ShotRef>, Option<&str>)> =
        all.iter().map(|m| (m.shot, m.element.as_deref())).collect();
    // Newest first.
    assert_eq!(
        what,
        [
            (None, None),
            (None, Some("paka")),
            (None, Some("paka")),
            (Some(r), None)
        ]
    );
    assert_eq!(all[1].job.id, old.id);

    p.set_discarded(frame.id, true).unwrap();
    assert_eq!(p.made_images().unwrap().len(), 3);
}
