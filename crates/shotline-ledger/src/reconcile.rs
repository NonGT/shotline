//! Comparing the ledger with what a provider says it charged.

use std::collections::BTreeMap;

use shotline_protocol::Charge;

use crate::{Entry, Kind};

#[derive(Debug, Clone, PartialEq)]
pub enum Diff {
    /// Both know the job but disagree on the amount.
    Mismatch {
        job: String,
        ledger: (f64, String),
        provider: (f64, String),
    },
    /// The provider charged for something the ledger doesn't have.
    OnlyProvider {
        job: Option<String>,
        provider: (f64, String),
    },
    /// The ledger has a charge the provider doesn't report.
    OnlyLedger { job: String, ledger: (f64, String) },
}

type Totals = BTreeMap<String, BTreeMap<String, f64>>;

fn add(t: &mut Totals, job: &str, unit: &str, amount: f64) {
    *t.entry(job.into())
        .or_default()
        .entry(unit.into())
        .or_default() += amount;
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6_f64.max(a.abs().max(b.abs()) * 1e-6)
}

/// Matches by the plugin's job ids. Ledger lines without one (manual adjustments) are left out.
pub fn reconcile(entries: &[Entry], plugin: &str, charges: &[Charge]) -> Vec<Diff> {
    let mut ours = Totals::new();
    for e in entries.iter().filter(|e| {
        e.plugin.as_deref() == Some(plugin) && matches!(e.kind, Kind::Charge | Kind::Refund)
    }) {
        if let Some(j) = &e.plugin_job {
            add(&mut ours, j, &e.unit, e.spent_original());
        }
    }
    let mut theirs = Totals::new();
    let mut diffs = vec![];
    for c in charges {
        match &c.job {
            Some(j) => add(&mut theirs, j, &c.unit, c.amount),
            None => diffs.push(Diff::OnlyProvider {
                job: None,
                provider: (c.amount, c.unit.clone()),
            }),
        }
    }

    for (job, units) in &theirs {
        for (unit, &amount) in units {
            match ours.get(job).and_then(|u| u.get(unit)) {
                Some(&mine) if close(mine, amount) => {}
                Some(&mine) => diffs.push(Diff::Mismatch {
                    job: job.clone(),
                    ledger: (mine, unit.clone()),
                    provider: (amount, unit.clone()),
                }),
                None => diffs.push(Diff::OnlyProvider {
                    job: Some(job.clone()),
                    provider: (amount, unit.clone()),
                }),
            }
        }
    }
    for (job, units) in &ours {
        for (unit, &amount) in units {
            let known = theirs.get(job).is_some_and(|u| u.contains_key(unit));
            if !known && !close(amount, 0.0) {
                diffs.push(Diff::OnlyLedger {
                    job: job.clone(),
                    ledger: (amount, unit.clone()),
                });
            }
        }
    }
    diffs
}
