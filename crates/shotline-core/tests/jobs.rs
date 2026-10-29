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
