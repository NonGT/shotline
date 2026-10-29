use std::sync::Arc;
use std::time::Duration;

use shotline_core::ledger::{Books, Budgets, Kind as LedgerKind};
use shotline_core::{
    Catalog, JobStatus, Kind, ModelOverride, Project, Shot, ShotRef, TestClock, Transition,
    UserConfig,
};
use shotline_engine::{Event, Options, plan, take};
use shotline_plugin::Plugin;
use shotline_plugin::fake::FakePlugin;
use tempfile::TempDir;

struct Film {
    _dir: TempDir,
    p: Project,
    books: Books,
    catalog: Catalog,
    fake: FakePlugin,
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
    Film {
        _dir: dir,
        p,
        books,
        catalog,
        fake,
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
    let plan = plan(&f.p, &f.catalog, &f.books, &f.fake, R).await.unwrap();
    assert_eq!(plan.quote.amount, 0.4);
    assert_eq!(plan.request.references.len(), 2);
    assert_eq!(
        plan.request.prompt.as_deref().unwrap().lines().next(),
        Some("Ref 1 and Ref 2 walks")
    );

    let mut events = vec![];
    let out = take(&f.p, &f.books, &f.fake, &plan, &fast(), |e| events.push(e))
        .await
        .unwrap();
    assert_eq!(out.error, None);
    assert_eq!(out.job.status, JobStatus::Done);
    assert_eq!(out.job.outputs, ["out.mp4"]);
    assert_eq!(out.job.cost.as_ref().unwrap().amount, 0.4);
    assert!(f.p.job_dir(out.job.id).join("out.mp4").is_file());
    assert_eq!(out.take.to_string(), "01-010#1");
    assert!(matches!(events[0], Event::Reserved(_)));
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
    let plan = plan(&f.p, &f.catalog, &f.books, &f.fake, R).await.unwrap();
    let out = take(&f.p, &f.books, &f.fake, &plan, &fast(), |_| {})
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
    let plan = plan(&f.p, &f.catalog, &f.books, &f.fake, R).await.unwrap();
    let out = take(&f.p, &f.books, &f.fake, &plan, &fast(), |_| {})
        .await
        .unwrap();
    assert_eq!(out.job.status, JobStatus::Failed);
    assert_eq!(out.job.cost, None);
    assert_eq!(kinds(&f), [LedgerKind::Reserve, LedgerKind::Release]);
}

#[tokio::test]
async fn rejected_on_submit_points_at_the_anchor() {
    let f = film("fake_reject_run = \"content_policy\"", Budgets::default()).await;
    let plan = plan(&f.p, &f.catalog, &f.books, &f.fake, R).await.unwrap();
    let out = take(&f.p, &f.books, &f.fake, &plan, &fast(), |_| {})
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
    let e = plan(&f.p, &f.catalog, &f.books, &f.fake, R)
        .await
        .unwrap_err()
        .to_string();
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
    let plan = plan(&f.p, &f.catalog, &f.books, &f.fake, R).await.unwrap();
    let e = take(&f.p, &f.books, &f.fake, &plan, &fast(), |_| {})
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
    let plan = plan(&f.p, &f.catalog, &f.books, &f.fake, R).await.unwrap();
    let opts = fast();
    opts.cancel.notify_one();
    let out = take(&f.p, &f.books, &f.fake, &plan, &opts, |_| {})
        .await
        .unwrap();
    assert_eq!(out.job.status, JobStatus::Cancelled);
    assert_eq!(kinds(&f), [LedgerKind::Reserve, LedgerKind::Release]);
}

#[tokio::test]
async fn continue_needs_the_last_frame() {
    let f = film("", Budgets::default()).await;
    let first = plan(&f.p, &f.catalog, &f.books, &f.fake, R).await.unwrap();
    take(&f.p, &f.books, &f.fake, &first, &fast(), |_| {})
        .await
        .unwrap();
    f.p.select_take(R, "1").unwrap();
    let mut second = f.p.shot(R).unwrap();
    second.transition = Transition::Continue;
    second.selected_take = None;
    let r2 = f.p.add_shot("1", None, second).unwrap();
    let e = plan(&f.p, &f.catalog, &f.books, &f.fake, r2)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("continues from 01-010, but jobs/") && e.ends_with("/last.png isn't there yet"),
        "{e}"
    );
}
