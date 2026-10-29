use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::{Entry, Error, Kind, Result, money};

/// Hard limits in the film's currency. Unset means no limit.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Budgets {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_job: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_take: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_chapter: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_day: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<f64>,
}

/// Money spent plus money still held for jobs that haven't finished, over the entries `f` keeps.
pub fn exposure(entries: &[Entry], f: impl Fn(&Entry) -> bool) -> f64 {
    let closed: HashSet<_> = entries
        .iter()
        .filter(|e| matches!(e.kind, Kind::Charge | Kind::Release))
        .filter_map(Entry::job_key)
        .collect();
    entries
        .iter()
        .filter(|e| f(e))
        .map(|e| match e.kind {
            Kind::Reserve if e.job_key().is_some_and(|k| !closed.contains(&k)) => e.base,
            _ => e.spent(),
        })
        .sum()
}

pub(crate) struct Scope<'a> {
    pub name: &'a str,
    pub limit: Option<f64>,
    pub current: f64,
}

/// Fails if adding `amount` would take any scope over its limit.
pub(crate) fn check(amount: f64, currency: &str, scopes: &[Scope]) -> Result<()> {
    for s in scopes {
        let Some(limit) = s.limit else { continue };
        if s.current + amount > limit + 1e-9 {
            return Err(Error::OverBudget(format!(
                "over the {} budget: {} spent or held + {} would pass {} {currency}",
                s.name,
                money(s.current),
                money(amount),
                money(limit)
            )));
        }
    }
    Ok(())
}
