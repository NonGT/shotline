use std::collections::BTreeSet;

use anyhow::{Result, bail};
use shotline_core::ledger::reconcile::{Diff, reconcile};
use shotline_core::ledger::report::{self, By};
use shotline_core::ledger::{Books, money};
use shotline_core::protocol::Quote;
use shotline_core::{Project, UserConfig, user_dir};

use crate::plugins::{self, Installed};

fn books(p: &Project) -> Result<Books> {
    let dir = user_dir();
    let user = UserConfig::load(&dir)?;
    Ok(p.books(&dir, &user, &[])?)
}

fn units(m: &std::collections::BTreeMap<String, f64>) -> String {
    m.iter()
        .map(|(u, v)| format!("{} {u}", money(*v)))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn show(p: &Project, by: By, csv: bool) -> Result<()> {
    let b = books(p)?;
    let entries = b.ledger.entries()?;
    let cur = &b.currency;
    let rows = report::group(&entries, by, cur, &b.rates, &b.tz)?;
    if csv {
        print!("{}", report::csv(&rows, cur));
        return Ok(());
    }
    if entries.is_empty() {
        println!("nothing spent yet");
        return Ok(());
    }
    let s = report::summary(&entries, cur, &b.rates, &p.selected_take_ids()?)?;
    println!(
        "spent    {} {cur}  (in the cut {}, thrown away {}, other {})",
        money(s.spent),
        money(s.in_cut),
        money(s.discarded),
        money(s.other)
    );
    if s.held > 0.0 {
        println!("held     {} {cur} for jobs still running", money(s.held));
    }
    println!("by unit  {}", units(&s.by_unit));
    let secs = p.cut_seconds()?;
    if secs > 0.0 {
        println!(
            "cut      {}s, {} {cur} a second ({} counting everything)",
            money(secs),
            money(s.in_cut / secs),
            money(s.spent / secs)
        );
    }
    if rows.is_empty() {
        return Ok(());
    }
    let label = format!("{by:?}").to_lowercase();
    let w = rows
        .iter()
        .map(|r| r.key.len())
        .max()
        .unwrap_or(0)
        .max(label.len());
    println!("\n{label:<w$}  {:>10}  jobs  units", cur);
    for r in &rows {
        println!(
            "{:<w$}  {:>10}  {:>4}  {}",
            r.key,
            money(r.spent),
            r.jobs,
            units(&r.by_unit)
        );
    }
    Ok(())
}

pub fn verify(p: &Project) -> Result<()> {
    let b = books(p)?;
    let mut bad = false;
    let mut ledgers = vec![("film", b.ledger.clone())];
    if let Some(g) = &b.global {
        ledgers.push(("all films", g.ledger.clone()));
    }
    for (name, l) in ledgers {
        let problems = l.verify()?;
        let n = l.entries().map(|e| e.len()).unwrap_or(0);
        if problems.is_empty() {
            println!("{name}: {n} lines, ok  ({})", l.path().display());
        } else {
            bad = true;
            println!("{name}: {}", l.path().display());
            for pr in problems {
                match pr.line {
                    0 => println!("  {}", pr.message),
                    n => println!("  line {n}: {}", pr.message),
                }
            }
        }
    }
    if bad {
        bail!("the ledger has been changed by hand");
    }
    Ok(())
}

pub fn reconcile_cmd(p: &Project, installed: &Installed, only: Option<&str>) -> Result<()> {
    let b = books(p)?;
    let entries = b.ledger.entries()?;
    let names: BTreeSet<&str> = entries
        .iter()
        .filter_map(|e| e.plugin.as_deref())
        .filter(|n| only.is_none_or(|o| o == *n))
        .collect();
    if names.is_empty() {
        println!("no charges to check");
        return Ok(());
    }
    let rt = plugins::runtime();
    let mut differ = false;
    for name in names {
        if !installed.plugins.iter().any(|p| p.name == name) {
            println!("{name}: not installed, skipped");
            continue;
        }
        let plugin = plugins::open(installed, name)?;
        let since = entries
            .iter()
            .filter(|e| e.plugin.as_deref() == Some(name))
            .map(|e| e.time)
            .min()
            .unwrap();
        let usage = match rt.block_on(plugin.usage(&since.to_string())) {
            Ok(u) => u,
            Err(shotline_plugin::Error::Unsupported(_)) => {
                println!("{name}: doesn't report usage, skipped");
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let diffs = reconcile(&entries, name, &usage.charges);
        if diffs.is_empty() {
            println!("{name}: matches ({} charges)", usage.charges.len());
            continue;
        }
        differ = true;
        println!("{name}:");
        let amt = |(a, u): &(f64, String)| format!("{} {u}", money(*a));
        for d in diffs {
            match d {
                Diff::Mismatch {
                    job,
                    ledger,
                    provider,
                } => println!(
                    "  {job}: ledger {} but provider {}",
                    amt(&ledger),
                    amt(&provider)
                ),
                Diff::OnlyProvider { job, provider } => println!(
                    "  {}: provider charged {}, not in the ledger",
                    job.as_deref().unwrap_or("(no job id)"),
                    amt(&provider)
                ),
                Diff::OnlyLedger { job, ledger } => {
                    println!("  {job}: ledger has {}, provider doesn't", amt(&ledger))
                }
            }
        }
    }
    if differ {
        bail!("the ledger and the providers disagree");
    }
    Ok(())
}

fn quote(amount: f64, unit: &str) -> Quote {
    Quote {
        amount,
        unit: unit.into(),
        units: None,
        unit_name: None,
    }
}

pub fn adjust(p: &Project, amount: f64, unit: &str, note: &str) -> Result<()> {
    let e = books(p)?.adjust(p.clock.now(), &quote(amount, unit), note)?;
    println!("adjusted {} {}", money(e.base), e.base_currency);
    Ok(())
}

pub fn refund(p: &Project, job: &str, amount: f64, unit: &str, note: &str) -> Result<()> {
    let id = p.job(job)?.id.to_string();
    let e = books(p)?.refund(p.clock.now(), &id, &quote(amount, unit), note)?;
    println!("refunded {} {}", money(e.base), e.base_currency);
    Ok(())
}
