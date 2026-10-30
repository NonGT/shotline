use std::collections::HashSet;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use shotline_ledger::report::{self, By};
use shotline_ledger::{
    Books, Budgets, Context, Error, GlobalLimit, Kind, Ledger, Rates, reconcile,
};
use shotline_protocol::{Charge, Quote};
use tempfile::TempDir;

fn usd(amount: f64) -> Quote {
    Quote {
        amount,
        unit: "USD".into(),
        units: None,
        unit_name: None,
    }
}

fn at(s: &str) -> Timestamp {
    s.parse().unwrap()
}

const T: &str = "2026-10-30T10:00:00Z";

fn books(dir: &TempDir, budgets: Budgets) -> Books {
    let mut rates = Rates::default();
    rates.add("fake-credit", 0.01, "USD", "plugin");
    Books {
        ledger: Ledger::new(dir.path().join("film/.shotline/ledger.jsonl")),
        currency: "USD".into(),
        rates,
        budgets,
        tz: TimeZone::fixed(jiff::tz::offset(7)),
        project: "film".into(),
        global: None,
    }
}

fn job(id: &str, take: &str, shot: &str, chapter: u32) -> Context {
    Context {
        job: id.into(),
        take: Some(take.into()),
        shot: Some(shot.into()),
        chapter: Some(chapter),
        op: "video.generate".into(),
        plugin: "fake".into(),
        model: "fake/video".into(),
    }
}

fn over(r: shotline_ledger::Result<shotline_ledger::Entry>) -> String {
    match r {
        Err(Error::OverBudget(m)) => m,
        other => panic!("expected over budget, got {other:?}"),
    }
}

#[test]
fn lifecycle() {
    let dir = TempDir::new().unwrap();
    let b = books(&dir, Budgets::default());
    let now = at(T);

    b.reserve(now, &job("j1", "t1", "01-010", 1), &usd(0.4))
        .unwrap();
    b.charge(now, "j1", Some("fal-123"), Some(&usd(0.38)), None)
        .unwrap();
    b.reserve(now, &job("j2", "t1", "01-010", 1), &usd(0.2))
        .unwrap();
    b.release(now, "j2", None).unwrap();
    b.reserve(now, &job("j3", "t2", "01-010", 1), &usd(0.4))
        .unwrap();
    // failed, but still charged: no actual given, the reserved amount is booked
    b.charge(now, "j3", None, None, None).unwrap();
    b.refund(now, "j3", &usd(0.4), "provider refunded the policy hit")
        .unwrap();

    let es = b.ledger.entries().unwrap();
    let kinds: Vec<_> = es.iter().map(|e| e.kind).collect();
    use Kind::*;
    assert_eq!(
        kinds,
        [Reserve, Charge, Reserve, Release, Reserve, Charge, Refund]
    );
    assert_eq!(es[1].plugin_job.as_deref(), Some("fal-123"));
    assert_eq!(
        es[1].shot.as_deref(),
        Some("01-010"),
        "copied from the reservation"
    );
    assert!(b.ledger.verify().unwrap().is_empty());

    let s = report::summary(&es, "USD", &b.rates, &HashSet::new()).unwrap();
    assert!((s.spent - 0.38).abs() < 1e-9, "{s:?}");
    assert_eq!(s.held, 0.0);

    assert!(
        b.charge(now, "j1", None, None, None).is_err(),
        "already closed"
    );
    assert!(b.release(now, "nope", None).is_err());
    b.reserve(now, &job("j4", "t3", "01-020", 1), &usd(0.1))
        .unwrap();
    assert!(
        b.reserve(now, &job("j4", "t3", "01-020", 1), &usd(0.1))
            .is_err()
    );
}

#[test]
fn budgets_stop_reservations() {
    let dir = TempDir::new().unwrap();
    let b = books(
        &dir,
        Budgets {
            per_job: Some(1.0),
            per_take: Some(1.5),
            per_chapter: Some(2.0),
            per_day: Some(2.5),
            total: Some(3.0),
        },
    );
    let now = at(T);
    let m = over(b.reserve(now, &job("big", "t0", "01-010", 1), &usd(1.2)));
    assert_eq!(
        m,
        "over the per-job budget: 0 spent or held + 1.2 would pass 1 USD"
    );

    b.reserve(now, &job("a", "t1", "01-010", 1), &usd(1.0))
        .unwrap();
    assert!(over(b.reserve(now, &job("b", "t1", "01-010", 1), &usd(0.6))).contains("per-take"));
    b.reserve(now, &job("b", "t2", "01-020", 1), &usd(1.0))
        .unwrap();
    assert!(over(b.reserve(now, &job("c", "t3", "01-030", 1), &usd(0.1))).contains("per-chapter"));
    b.reserve(now, &job("c", "t4", "02-010", 2), &usd(0.5))
        .unwrap();
    assert!(over(b.reserve(now, &job("d", "t5", "02-020", 2), &usd(0.1))).contains("daily"));

    // Releasing frees the money again.
    b.release(now, "b", None).unwrap();
    b.reserve(now, &job("d", "t5", "02-020", 2), &usd(0.5))
        .unwrap();

    // Next day (in UTC+7) the daily limit resets but the total doesn't.
    let tomorrow = at("2026-10-30T17:30:00Z");
    b.charge(now, "a", None, None, None).unwrap();
    b.charge(now, "c", None, None, None).unwrap();
    b.charge(now, "d", None, None, None).unwrap();
    b.reserve(tomorrow, &job("e", "t6", "03-010", 3), &usd(0.9))
        .unwrap();
    assert!(over(b.reserve(tomorrow, &job("f", "t7", "03-020", 3), &usd(0.2))).contains("total"));
}

#[test]
fn parallel_reservations_cant_overshoot() {
    let dir = TempDir::new().unwrap();
    let b = books(
        &dir,
        Budgets {
            total: Some(5.0),
            ..Budgets::default()
        },
    );
    let ok = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for i in 0..12 {
            let (b, ok) = (&b, &ok);
            s.spawn(move || {
                if b.reserve(at(T), &job(&format!("j{i}"), "t", "01-010", 1), &usd(1.0))
                    .is_ok()
                {
                    ok.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            });
        }
    });
    assert_eq!(ok.into_inner(), 5);
    assert!(b.ledger.verify().unwrap().is_empty());
}

#[test]
fn credits_and_currencies() {
    let dir = TempDir::new().unwrap();
    let mut b = books(&dir, Budgets::default());
    b.currency = "THB".into();
    b.rates.add("USD", 36.5, "THB", "config");
    let credits = Quote {
        amount: 80.0,
        unit: "fake-credit".into(),
        units: Some(8.0),
        unit_name: Some("s".into()),
    };
    let e = b
        .reserve(at(T), &job("j1", "t1", "01-010", 1), &credits)
        .unwrap();
    assert!((e.base - 29.2).abs() < 1e-9);
    assert_eq!(e.rate_source, "plugin+config");
    assert_eq!((e.amount, e.unit.as_str()), (80.0, "fake-credit"));

    let unknown = Quote {
        amount: 1.0,
        unit: "mystery-coin".into(),
        units: None,
        unit_name: None,
    };
    match b.reserve(at(T), &job("j2", "t1", "01-010", 1), &unknown) {
        Err(Error::NoRate(m)) => assert!(m.contains("mystery-coin to THB")),
        other => panic!("{other:?}"),
    }
    assert_eq!(b.ledger.entries().unwrap().len(), 1, "nothing written");

    // A changed rate doesn't touch old lines.
    b.rates.add("USD", 40.0, "THB", "config");
    b.charge(at(T), "j1", None, None, None).unwrap();
    let es = b.ledger.entries().unwrap();
    assert!((es[0].base - 29.2).abs() < 1e-9);
    assert!((es[1].base - 32.0).abs() < 1e-9);
}

#[test]
fn daily_limit_across_films() {
    let dir = TempDir::new().unwrap();
    let global = GlobalLimit {
        ledger: Ledger::new(dir.path().join("home/spend.jsonl")),
        currency: "USD".into(),
        per_day: Some(1.0),
    };
    let mut a = books(&dir, Budgets::default());
    a.global = Some(global.clone());
    let mut b = books(&dir, Budgets::default());
    b.ledger = Ledger::new(dir.path().join("other/.shotline/ledger.jsonl"));
    b.project = "other".into();
    b.currency = "THB".into();
    b.rates.add("USD", 36.5, "THB", "config");
    b.global = Some(global.clone());

    a.reserve(at(T), &job("j1", "t1", "01-010", 1), &usd(0.6))
        .unwrap();
    let m = over(b.reserve(at(T), &job("j1", "t1", "01-010", 1), &usd(0.6)));
    assert!(m.contains("daily (all films)"), "{m}");
    a.release(at(T), "j1", None).unwrap();
    // same job id in another film is a different job
    b.reserve(at(T), &job("j1", "t1", "01-010", 1), &usd(0.6))
        .unwrap();

    let g = global.ledger.entries().unwrap();
    assert_eq!(g.len(), 3);
    assert_eq!(g[2].project.as_deref(), Some("other"));
    assert_eq!(g[2].base_currency, "USD");
    assert!(global.ledger.verify().unwrap().is_empty());
}

#[test]
fn adjustments_need_a_reason() {
    let dir = TempDir::new().unwrap();
    let b = books(&dir, Budgets::default());
    assert!(b.adjust(at(T), &usd(1.0), " ").is_err());
    let e = b.adjust(at(T), &usd(-0.25), "fal promo credit").unwrap();
    assert_eq!(e.spent(), -0.25);
    assert!(b.refund(at(T), "never", &usd(1.0), "x").is_err());
}

#[test]
fn reports() {
    let dir = TempDir::new().unwrap();
    let b = books(&dir, Budgets::default());
    let day1 = at(T);
    let day2 = at("2026-10-31T10:00:00Z");
    let credits = |n: f64| Quote {
        amount: n,
        unit: "fake-credit".into(),
        units: None,
        unit_name: None,
    };
    b.reserve(day1, &job("j1", "t1", "01-010", 1), &usd(0.4))
        .unwrap();
    b.charge(day1, "j1", None, None, None).unwrap();
    b.reserve(day1, &job("j2", "t2", "01-010", 1), &credits(30.0))
        .unwrap();
    b.charge(day1, "j2", None, None, None).unwrap();
    b.reserve(day2, &job("j3", "t3", "01-020", 1), &usd(1.0))
        .unwrap();
    b.charge(day2, "j3", None, None, None).unwrap();
    b.reserve(day2, &job("j4", "t4", "01-020", 1), &usd(5.0))
        .unwrap();
    b.adjust(day2, &usd(0.1), "bank fee").unwrap();

    let es = b.ledger.entries().unwrap();
    let rows = report::group(&es, By::Shot, "USD", &b.rates, &b.tz).unwrap();
    let keys: Vec<_> = rows
        .iter()
        .map(|r| (r.key.as_str(), shotline_ledger::money(r.spent), r.jobs))
        .collect();
    assert_eq!(
        keys,
        [
            ("-", "0.1".into(), 0),
            ("01-010", "0.7".into(), 2),
            ("01-020", "1".into(), 1)
        ]
    );
    assert_eq!(rows[1].by_unit["fake-credit"], 30.0);

    let days = report::group(&es, By::Day, "USD", &b.rates, &b.tz).unwrap();
    assert_eq!(
        days.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(),
        ["2026-10-30", "2026-10-31"]
    );

    let selected: HashSet<String> = ["t1".to_string(), "t3".to_string()].into();
    let s = report::summary(&es, "USD", &b.rates, &selected).unwrap();
    assert_eq!(shotline_ledger::money(s.spent), "1.8");
    assert_eq!(shotline_ledger::money(s.in_cut), "1.4");
    assert_eq!(shotline_ledger::money(s.discarded), "0.3");
    assert_eq!(shotline_ledger::money(s.other), "0.1");
    assert_eq!(s.held, 5.0);

    let csv = report::csv(&rows, "USD");
    assert_eq!(
        csv,
        "key,USD,jobs,USD,fake-credit\n-,0.1,0,0.1,\n01-010,0.7,2,0.4,30\n01-020,1,1,1,\n"
    );

    // Changing the film's currency later still adds up, at today's rates.
    let mut rates = b.rates.clone();
    rates.add("USD", 36.5, "THB", "config");
    let thb = report::summary(&es, "THB", &rates, &selected).unwrap();
    assert!((thb.spent - 1.8 * 36.5).abs() < 1e-9);
}

#[test]
fn reconciling() {
    let dir = TempDir::new().unwrap();
    let b = books(&dir, Budgets::default());
    for (j, pj, amount) in [("j1", "p1", 0.4), ("j2", "p2", 0.5), ("j3", "p3", 0.2)] {
        b.reserve(at(T), &job(j, "t", "01-010", 1), &usd(amount))
            .unwrap();
        b.charge(at(T), j, Some(pj), None, None).unwrap();
    }
    b.refund(at(T), "j3", &usd(0.2), "refunded").unwrap();
    let charge = |job: Option<&str>, amount: f64| Charge {
        job: job.map(str::to_string),
        time: T.into(),
        amount,
        unit: "USD".into(),
    };
    let provider = [
        charge(Some("p1"), 0.4),
        charge(Some("p2"), 0.55),
        charge(Some("p9"), 1.0),
        charge(None, 0.05),
    ];
    let diffs = reconcile::reconcile(&b.ledger.entries().unwrap(), "fake", &provider);
    use reconcile::Diff::*;
    assert_eq!(diffs.len(), 3, "{diffs:#?}");
    assert!(diffs.contains(&OnlyProvider {
        job: None,
        provider: (0.05, "USD".into())
    }));
    assert!(diffs.contains(&Mismatch {
        job: "p2".into(),
        ledger: (0.5, "USD".into()),
        provider: (0.55, "USD".into())
    }));
    assert!(diffs.contains(&OnlyProvider {
        job: Some("p9".into()),
        provider: (1.0, "USD".into())
    }));
    // p3 was charged and refunded, nets to zero, so not reported missing
}
