use std::collections::{BTreeMap, HashSet};
use std::str::FromStr;

use jiff::tz::TimeZone;

use crate::budget::exposure;
use crate::{Entry, Error, Rates, Result, money};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    Chapter,
    Shot,
    Take,
    Op,
    Plugin,
    Day,
}

impl FromStr for By {
    type Err = Error;

    fn from_str(s: &str) -> Result<By> {
        Ok(match s {
            "chapter" => By::Chapter,
            "shot" => By::Shot,
            "take" => By::Take,
            "op" => By::Op,
            "plugin" => By::Plugin,
            "day" => By::Day,
            _ => {
                return Err(Error::Invalid(format!(
                    "can't group by {s:?}: chapter, shot, take, op, plugin or day"
                )));
            }
        })
    }
}

/// What an entry spent in `currency`. Lines booked in another currency (the film's currency was
/// changed later) are converted at today's rates.
fn value(e: &Entry, currency: &str, rates: &Rates) -> Result<f64> {
    if e.base_currency == currency {
        return Ok(e.spent());
    }
    Ok(rates.convert(e.spent_original(), &e.unit, currency)?.amount)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub key: String,
    pub spent: f64,
    /// Spent per provider unit, to compare with the provider's own numbers.
    pub by_unit: BTreeMap<String, f64>,
    pub jobs: usize,
}

pub fn group(
    entries: &[Entry],
    by: By,
    currency: &str,
    rates: &Rates,
    tz: &TimeZone,
) -> Result<Vec<Row>> {
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    let mut jobs: BTreeMap<String, HashSet<&str>> = BTreeMap::new();
    for e in entries.iter().filter(|e| e.spent() != 0.0) {
        let key = match by {
            By::Chapter => e.chapter.map(|c| format!("{c:02}")),
            By::Shot => e.shot.clone(),
            By::Take => e.take.clone(),
            By::Op => e.op.clone(),
            By::Plugin => e.plugin.clone(),
            By::Day => Some(e.time.to_zoned(tz.clone()).date().to_string()),
        }
        .unwrap_or_else(|| "-".into());
        let row = rows.entry(key.clone()).or_insert_with(|| Row {
            key: key.clone(),
            spent: 0.0,
            by_unit: BTreeMap::new(),
            jobs: 0,
        });
        row.spent += value(e, currency, rates)?;
        *row.by_unit.entry(e.unit.clone()).or_default() += e.spent_original();
        if let Some(j) = &e.job {
            jobs.entry(key).or_default().insert(j);
        }
    }
    Ok(rows
        .into_values()
        .map(|mut r| {
            r.jobs = jobs.get(&r.key).map_or(0, HashSet::len);
            r
        })
        .collect())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub currency: String,
    pub spent: f64,
    /// Reserved for jobs that haven't finished.
    pub held: f64,
    /// On takes that are selected, i.e. in the film.
    pub in_cut: f64,
    /// On takes that aren't.
    pub discarded: f64,
    /// Not tied to a take: adjustments, one-off runs.
    pub other: f64,
    pub by_unit: BTreeMap<String, f64>,
}

pub fn summary(
    entries: &[Entry],
    currency: &str,
    rates: &Rates,
    selected_takes: &HashSet<String>,
) -> Result<Summary> {
    let mut s = Summary {
        currency: currency.into(),
        spent: 0.0,
        held: exposure(entries, |_| true) - entries.iter().map(Entry::spent).sum::<f64>(),
        in_cut: 0.0,
        discarded: 0.0,
        other: 0.0,
        by_unit: BTreeMap::new(),
    };
    for e in entries.iter().filter(|e| e.spent() != 0.0) {
        let v = value(e, currency, rates)?;
        s.spent += v;
        match &e.take {
            Some(t) if selected_takes.contains(t) => s.in_cut += v,
            Some(_) => s.discarded += v,
            None => s.other += v,
        }
        *s.by_unit.entry(e.unit.clone()).or_default() += e.spent_original();
    }
    Ok(s)
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub fn csv(rows: &[Row], currency: &str) -> String {
    let units: Vec<&String> = rows
        .iter()
        .flat_map(|r| r.by_unit.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut out = format!("key,{},jobs", csv_field(currency));
    for u in &units {
        out.push(',');
        out.push_str(&csv_field(u));
    }
    out.push('\n');
    for r in rows {
        out.push_str(&format!(
            "{},{},{}",
            csv_field(&r.key),
            money(r.spent),
            r.jobs
        ));
        for u in &units {
            out.push(',');
            out.push_str(&r.by_unit.get(*u).map(|v| money(*v)).unwrap_or_default());
        }
        out.push('\n');
    }
    out
}
