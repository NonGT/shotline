use std::sync::Arc;
use std::time::Duration;

use shotline_core::ledger::{Books, Budgets, Kind as LedgerKind};
use shotline_core::{
    Catalog, JobStatus, Kind, ModelOverride, Project, Shot, ShotRef, TestClock, Transition,
    UserConfig,
};
use std::collections::BTreeMap;

use shotline_engine::{Event, Options, Plan, take};
use shotline_plugin::Plugin;
use shotline_plugin::fake::FakePlugin;
use tempfile::TempDir;

struct Film {
    _dir: TempDir,
    p: Project,
    books: Books,
    catalog: Catalog,
    plugins: BTreeMap<String, Box<dyn Plugin>>,
}

impl Film {
    async fn plan(&self, r: ShotRef) -> anyhow::Result<Plan> {
        shotline_engine::plan(&self.p, &self.catalog, &self.books, &self.plugins, r, None).await
    }
}

async fn film(params: &str, budgets: Budgets) -> Film {
    let dir = TempDir::new().unwrap();
    let clock = TestClock::new("2026-10-30T10:00:00Z".parse().unwrap());
    let mut p = Project::init(&dir.path().join("film"), None)
        .unwrap()
        .with_clock(Arc::new(clock));
    p.config.budget = budgets;
    let img = dir.path().join("a.png");
    std::fs::write(&img, b"png").unwrap();
    p.add_element(Kind::Cast, "Paka", None, "").unwrap();
    p.add_anchor("paka", &img).unwrap();
    p.add_anchor("paka", &img).unwrap();
    p.add_chapter("One").unwrap();
    let mut model = ModelOverride {
        plugin: Some("fake".into()),
        id: Some("fake/video".into()),
        resolution: Some("480P".into()),
        aspect: Some("9:16".into()),
        ..ModelOverride::default()
    };
    model.params = toml::from_str(params).unwrap();
    p.add_shot(
        "1",
        None,
        Shot {
            prompt: "@paka walks".into(),
            duration: Some(4.0),
            model: Some(model),
            ..Shot::default()
        },
    )
    .unwrap();
    let books = p
        .books(&dir.path().join("home"), &UserConfig::default(), &[])
        .unwrap();
    let mut catalog = Catalog::builtin();
    let fake = FakePlugin::default();
    let d = fake.describe().await.unwrap();
    catalog.add_plugin("fake", d.models);
    let mut plugins: BTreeMap<String, Box<dyn Plugin>> = BTreeMap::new();
    plugins.insert("fake".into(), Box::new(fake));
    Film {
        _dir: dir,
        p,
        books,
        catalog,
        plugins,
    }
}

fn fast() -> Options {
    Options {
        poll_every: Duration::from_millis(1),
        ..Options::default()
    }
}

const R: ShotRef = ShotRef {
    chapter: 1,
    shot: 10,
};

fn kinds(f: &Film) -> Vec<LedgerKind> {
    f.books
        .ledger
        .entries()
        .unwrap()
        .iter()
        .map(|e| e.kind)
        .collect()
}

#[tokio::test]
async fn a_good_take() {
    let f = film("", Budgets::default()).await;
    let plan = f.plan(R).await.unwrap();
    assert_eq!(plan.total, 0.4);
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps[0].request.references.len(), 2);
    assert_eq!(
        plan.steps[0]
            .request
            .prompt
            .as_deref()
            .unwrap()
            .lines()
            .next(),
        Some("Ref 1 and Ref 2 walks")
    );

    let mut events = vec![];
    let out = take(&f.p, &f.books, &f.plugins, &plan, &fast(), |e| {
        events.push(e)
    })
    .await
    .unwrap();
    assert_eq!(out.error, None);
    assert_eq!(out.job.status, JobStatus::Done);
    assert_eq!(out.job.outputs, ["out.mp4"]);
    assert_eq!(out.job.cost.as_ref().unwrap().amount, 0.4);
    assert!(f.p.job_dir(out.job.id).join("out.mp4").is_file());
    assert_eq!(out.take.to_string(), "01-010#1");
    assert!(matches!(events[0], Event::Reserved { .. }));
    assert!(events.iter().any(|e| matches!(e, Event::Submitted { .. })));

    assert_eq!(kinds(&f), [LedgerKind::Reserve, LedgerKind::Charge]);
    let charge = &f.books.ledger.entries().unwrap()[1];
    assert_eq!(charge.plugin_job.as_deref(), Some("fake-1"));
    assert_eq!(charge.shot.as_deref(), Some("01-010"));
    assert_eq!(charge.job.as_deref(), Some(out.job.id.to_string().as_str()));
    f.p.select_take(R, "1").unwrap();
}

#[tokio::test]
async fn failed_and_charged() {
    let f = film(
        "fake_fail = \"content_policy\"\nfake_charge_on_fail = true",
        Budgets::default(),
    )
    .await;
    let plan = f.plan(R).await.unwrap();
    let out = take(&f.p, &f.books, &f.plugins, &plan, &fast(), |_| {})
        .await
        .unwrap();
    assert_eq!(out.job.status, JobStatus::Failed);
    assert!(
        out.error
            .unwrap()
            .starts_with("content policy: failed on request")
    );
    assert_eq!(kinds(&f), [LedgerKind::Reserve, LedgerKind::Charge]);
}

#[tokio::test]
async fn failed_and_free() {
    let f = film("fake_fail = \"other\"", Budgets::default()).await;
    let plan = f.plan(R).await.unwrap();
    let out = take(&f.p, &f.books, &f.plugins, &plan, &fast(), |_| {})
        .await
        .unwrap();
    assert_eq!(out.job.status, JobStatus::Failed);
    assert_eq!(out.job.cost, None);
    assert_eq!(kinds(&f), [LedgerKind::Reserve, LedgerKind::Release]);
}

#[tokio::test]
async fn rejected_on_submit_points_at_the_anchor() {
    let f = film("fake_reject_run = \"content_policy\"", Budgets::default()).await;
    let plan = f.plan(R).await.unwrap();
    let out = take(&f.p, &f.books, &f.plugins, &plan, &fast(), |_| {})
        .await
        .unwrap();
    assert_eq!(
        out.error.as_deref(),
        Some(
            "content policy: rejected on submit (@paka's anchor elements/cast/paka/01.png (Ref 1))"
        )
    );
    assert_eq!(kinds(&f), [LedgerKind::Reserve, LedgerKind::Release]);
}

#[tokio::test]
async fn rejected_quote_spends_nothing() {
    let f = film("fake_reject = \"validation\"", Budgets::default()).await;
    let e = f.plan(R).await.unwrap_err().to_string();
    assert_eq!(
        e,
        "validation: rejected on request ([model.params] fake_reject in shot 01-010)"
    );
    assert!(kinds(&f).is_empty());
}

#[tokio::test]
async fn over_budget_makes_no_job() {
    let f = film(
        "",
        Budgets {
            per_take: Some(0.3),
            ..Budgets::default()
        },
    )
    .await;
    let plan = f.plan(R).await.unwrap();
    let e = take(&f.p, &f.books, &f.plugins, &plan, &fast(), |_| {})
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("over the per-take budget"), "{e}");
    assert!(f.p.jobs().unwrap().is_empty());
    assert!(kinds(&f).is_empty());
}

#[tokio::test]
async fn cancel_releases() {
    let f = film("fake_polls = 100000", Budgets::default()).await;
    let plan = f.plan(R).await.unwrap();
    let opts = fast();
    opts.cancel.notify_one();
    let out = take(&f.p, &f.books, &f.plugins, &plan, &opts, |_| {})
        .await
        .unwrap();
    assert_eq!(out.job.status, JobStatus::Cancelled);
    assert_eq!(kinds(&f), [LedgerKind::Reserve, LedgerKind::Release]);
}

#[tokio::test]
async fn continue_needs_the_last_frame() {
    let f = film("", Budgets::default()).await;
    let first = f.plan(R).await.unwrap();
    take(&f.p, &f.books, &f.plugins, &first, &fast(), |_| {})
        .await
        .unwrap();
    f.p.select_take(R, "1").unwrap();
    let mut second = f.p.shot(R).unwrap();
    second.transition = Transition::Continue;
    second.selected_take = None;
    let r2 = f.p.add_shot("1", None, second).unwrap();
    let e = f.plan(r2).await.unwrap_err().to_string();
    assert!(
        e.contains("continues from 01-010, but jobs/") && e.ends_with("/last.png isn't there yet"),
        "{e}"
    );
}

/// Paka with a fake voice and one line, and a recipe tts -> video with audio -> lip sync.
async fn talking(shot_params: &str, budgets: Budgets) -> Film {
    let mut f = film(shot_params, budgets).await;
    f.p.set_voice(
        "paka",
        shotline_core::Voice {
            plugin: "fake".into(),
            model: Some("fake/tts".into()),
            voice_id: Some("v1".into()),
            ..Default::default()
        },
    )
    .unwrap();
    f.p.add_line(R, "paka", "we go on before dark").unwrap();
    f.p.config.recipes = toml::from_str::<toml::Table>(
        r#"
[[talking]]
op = "audio.tts"
[[talking]]
op = "video.generate"
audio = true
[[talking]]
op = "video.lipsync"
plugin = "fake"
model = "fake/lipsync"
"#,
    )
    .unwrap()
    .try_into()
    .unwrap();
    let mut shot = f.p.shot(R).unwrap();
    shot.recipe = Some("talking".into());
    f.p.save_shot(R, &shot).unwrap();
    f
}

#[tokio::test]
async fn a_recipe_feeds_each_step_into_the_next() {
    let f = talking("", Budgets::default()).await;
    let plan = f.plan(R).await.unwrap();
    let what: Vec<_> = plan.steps.iter().map(|s| s.what.as_str()).collect();
    assert_eq!(what, ["line 1 by @paka", "video", "video.lipsync"]);
    // 20 chars of speech, 4s of video, 4s of lip sync
    assert!(
        (plan.total - (0.02 + 0.4 + 0.2)).abs() < 1e-9,
        "{}",
        plan.total
    );

    let mut steps = vec![];
    let out = take(&f.p, &f.books, &f.plugins, &plan, &fast(), |e| {
        if let Event::Step { what, .. } = e {
            steps.push(what)
        }
    })
    .await
    .unwrap();
    assert_eq!(out.error, None);
    assert_eq!(steps.len(), 3);
    let jobs = &out.take.jobs;
    assert_eq!(jobs.len(), 3);
    assert_eq!(jobs[0].outputs, ["out.wav"]);
    // the video got the line's audio, lip sync got the video and the audio
    assert!(jobs[1].inputs.contains(&jobs[0].id.to_string()));
    assert!(jobs[2].inputs.contains(&jobs[1].id.to_string()));
    assert!(jobs[2].inputs.contains(&jobs[0].id.to_string()));
    assert_eq!(out.job.id, jobs[2].id);
    use LedgerKind::*;
    assert_eq!(
        kinds(&f),
        [Reserve, Reserve, Reserve, Charge, Charge, Charge]
    );
}

#[tokio::test]
async fn a_failed_step_releases_the_rest() {
    let f = talking("fake_fail = \"other\"", Budgets::default()).await;
    let plan = f.plan(R).await.unwrap();
    let out = take(&f.p, &f.books, &f.plugins, &plan, &fast(), |_| {})
        .await
        .unwrap();
    assert_eq!(
        out.error.as_deref(),
        Some("video: error: failed on request")
    );
    assert_eq!(out.take.jobs.len(), 2, "lip sync never ran");
    use LedgerKind::*;
    assert_eq!(
        kinds(&f),
        [Reserve, Reserve, Reserve, Charge, Release, Release]
    );
}

#[tokio::test]
async fn a_recipe_over_budget_never_starts() {
    let f = talking(
        "",
        Budgets {
            per_take: Some(0.5),
            ..Budgets::default()
        },
    )
    .await;
    let plan = f.plan(R).await.unwrap();
    let e = take(&f.p, &f.books, &f.plugins, &plan, &fast(), |_| {})
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("per-take"), "{e}");
    assert!(f.p.jobs().unwrap().is_empty());
    let s = shotline_core::ledger::report::summary(
        &f.books.ledger.entries().unwrap(),
        "USD",
        &f.books.rates,
        &Default::default(),
    )
    .unwrap();
    assert_eq!((s.spent, s.held), (0.0, 0.0));
}

#[tokio::test]
async fn redo_only_the_last_step() {
    let f = talking("", Budgets::default()).await;
    let first = f.plan(R).await.unwrap();
    take(&f.p, &f.books, &f.plugins, &first, &fast(), |_| {})
        .await
        .unwrap();
    let t1 = f.p.take(R, "1").unwrap();
    let again = shotline_engine::plan(&f.p, &f.catalog, &f.books, &f.plugins, R, Some((&t1, 2)))
        .await
        .unwrap();
    assert!(
        (again.total - 0.2).abs() < 1e-9,
        "only lip sync is paid for"
    );
    let out = take(&f.p, &f.books, &f.plugins, &again, &fast(), |_| {})
        .await
        .unwrap();
    assert_eq!(out.error, None);
    assert_eq!(out.take.number, 2);
    assert_eq!(out.take.jobs.len(), 1);
    assert!(out.job.inputs.contains(&t1.jobs[1].id.to_string()));
}

#[tokio::test]
async fn recipe_mistakes_are_caught_before_spending() {
    let f = talking("", Budgets::default()).await;
    f.p.add_line(R, "paka", "and again").unwrap();
    let e = f.plan(R).await.unwrap_err().to_string();
    assert!(e.contains("lip sync takes one audio clip"), "{e}");

    let f = talking("", Budgets::default()).await;
    let mut el = f.p.element("paka").unwrap();
    el.voice = None;
    f.p.add_element(Kind::Cast, "Kan", None, "").unwrap();
    let mut shot = f.p.shot(R).unwrap();
    shot.lines[0].who = "kan".into();
    f.p.save_shot(R, &shot).unwrap();
    let e = f.plan(R).await.unwrap_err().to_string();
    assert!(e.contains("@kan has no voice"), "{e}");
}
