//! Comparing the ledger with what a provider says it charged.

use std::collections::BTreeMap;

use shotline_protocol::Charge;

use crate::{Entry, Kind, Rates};

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

/// A job whose provider balance was read before and after it.
#[derive(Debug, Clone, PartialEq)]
pub struct BalanceCheck {
    pub job: String,
    pub plugin: String,
    /// What the ledger booked, in the provider's unit.
    pub booked: (f64, String),
    /// What the balance dropped by.
    pub measured: (f64, String),
    /// The booked amount in the measured unit, when there's a rate between them.
    pub booked_as_measured: Option<f64>,
}

impl BalanceCheck {
    /// Close enough for providers that round, or can't be compared.
    pub fn agrees(&self) -> bool {
        match self.booked_as_measured {
            Some(b) => {
                let m = self.measured.0;
                (b - m).abs() <= 0.005_f64.max(b.abs().max(m.abs()) * 0.02)
            }
            None => true,
        }
    }
}

/// Every charge and release that has a measured balance drop next to it.
pub fn balance_checks(entries: &[Entry], rates: &Rates) -> Vec<BalanceCheck> {
    entries
        .iter()
        .filter(|e| matches!(e.kind, Kind::Charge | Kind::Release))
        .filter_map(|e| {
            let m = e.measured.as_ref()?;
            Some(BalanceCheck {
                job: e.job.clone().unwrap_or_default(),
                plugin: e.plugin.clone().unwrap_or_default(),
                booked: (e.amount, e.unit.clone()),
                measured: (m.amount, m.unit.clone()),
                booked_as_measured: rates
                    .convert(e.amount, &e.unit, &m.unit)
                    .ok()
                    .map(|c| c.amount),
            })
        })
        .collect()
}
