use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use shotline_core::ledger::{Books, money};
use shotline_core::protocol::{Rate, State};
use shotline_core::{Project, ShotRef, UserConfig, user_dir};
use shotline_engine::{Event, Options, Plan};
use shotline_plugin::Plugin;
use tokio::runtime::Runtime;

use crate::plugins::{self, Installed};

fn plugin_name(p: &Project, r: ShotRef) -> Result<String> {
    Ok(p.shot(r)?
        .model
        .and_then(|m| m.plugin)
        .unwrap_or_else(|| p.config.plugin.clone()))
}

fn books(p: &Project, rates: &[Rate]) -> Result<Books> {
    let dir = user_dir();
    Ok(p.books(&dir, &UserConfig::load(&dir)?, rates)?)
}

/// Opened plugins by name, with whatever rates they publish.
struct Opened<'a> {
    installed: &'a Installed,
    plugins: BTreeMap<String, Box<dyn Plugin>>,
    rates: Vec<Rate>,
}

impl<'a> Opened<'a> {
    fn new(installed: &'a Installed) -> Self {
        Self {
            installed,
            plugins: BTreeMap::new(),
            rates: vec![],
        }
    }

    fn open(&mut self, rt: &Runtime, name: &str) -> Result<()> {
        if !self.plugins.contains_key(name) {
            let p = plugins::open(self.installed, name)?;
            if let Ok(r) = rt.block_on(p.rates()) {
                self.rates.extend(r.rates);
            }
            self.plugins.insert(name.to_string(), p);
        }
        Ok(())
    }

    fn get(&self, name: &str) -> &dyn Plugin {
        self.plugins[name].as_ref()
    }
}

fn price(plan: &Plan) -> String {
    let q = &plan.quote;
    if q.unit == plan.currency {
        format!("{} {}", money(q.amount), q.unit)
    } else {
        format!(
            "{} {} ({} {})",
            money(q.amount),
            q.unit,
            money(plan.base),
            plan.currency
        )
    }
}

fn targets(p: &Project, target: &str) -> Result<Vec<ShotRef>> {
    if target.contains(['-', '/']) {
        return Ok(vec![target.parse()?]);
    }
    Ok(p.shots(Some(target))?.into_iter().map(|(r, _)| r).collect())
}

/// Prices a shot or a whole chapter without spending anything.
pub fn quote(p: &Project, target: &str) -> Result<()> {
    let installed = plugins::load();
    let catalog = plugins::catalog(p, &installed)?;
    let rt = plugins::runtime();
    let mut opened = Opened::new(&installed);
    let mut total = 0.0;
    let mut currency = p.config.money.currency.clone();
    let shots = targets(p, target)?;
    for r in &shots {
        let name = plugin_name(p, *r)?;
        opened.open(&rt, &name)?;
        let plugin = opened.get(&name);
        let b = books(p, &opened.rates)?;
        let plan = rt.block_on(shotline_engine::plan(p, &catalog, &b, plugin, *r))?;
        for w in &plan.built.warnings {
            eprintln!("warning: {r}: {w}");
        }
        println!(
            "{r}  {} {}  {}s  {}",
            plan.built.plugin,
            plan.built.model,
            money(plan.built.duration),
            price(&plan)
        );
        total += plan.base;
        currency = plan.currency;
    }
    if shots.len() > 1 {
        println!("total  {} {currency}", money(total));
    }
    drop(opened);
    Ok(())
}

fn confirm(question: &str) -> Result<bool> {
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

fn poll_every() -> Duration {
    std::env::var("SHOTLINE_POLL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(5))
}

/// Quotes, asks, then runs one take of a shot.
pub fn take(p: &Project, r: ShotRef, yes: bool) -> Result<()> {
    let installed = plugins::load();
    let catalog = plugins::catalog(p, &installed)?;
    let rt = plugins::runtime();
    let mut opened = Opened::new(&installed);
    let name = plugin_name(p, r)?;
    opened.open(&rt, &name)?;
    let plugin = opened.get(&name);
    let b = books(p, &opened.rates)?;
    let plan = rt.block_on(shotline_engine::plan(p, &catalog, &b, plugin, r))?;

    for w in &plan.built.warnings {
        eprintln!("warning: {w}");
    }
    println!(
        "{r} · {} {} · {}s · {} · {} · {} reference images{}",
        plan.built.plugin,
        plan.built.model,
        money(plan.built.duration),
        plan.built.resolution,
        plan.built.aspect,
        plan.built.references.len(),
        if plan.built.start_frame.is_some() {
            " + start frame"
        } else {
            ""
        }
    );
    if !yes && !confirm(&format!("spend {} on {r}?", price(&plan)))? {
        println!("nothing spent");
        return Ok(());
    }

    let opts = Options {
        poll_every: poll_every(),
        ..Options::default()
    };
    let cancel = Arc::clone(&opts.cancel);
    let mut last: Option<State> = None;
    let outcome = rt.block_on(async {
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                cancel.notify_one();
            }
        });
        shotline_engine::take(p, &b, plugin, &plan, &opts, |ev| match ev {
            Event::Reserved(e) => println!("reserved {} {}", money(e.base), e.base_currency),
            Event::Submitted { plugin_job } => println!("sent to {name} ({plugin_job})"),
            Event::Cancelling => println!("cancelling..."),
            Event::Status(s) => {
                if last != Some(s.state) {
                    match s.progress {
                        Some(pr) if s.state == State::Running => {
                            println!("running ({}%)", (pr * 100.0).round())
                        }
                        _ => println!("{}", format!("{:?}", s.state).to_lowercase()),
                    }
                    last = Some(s.state);
                }
            }
        })
        .await
    })?;
    drop(opened);

    let cost = outcome
        .job
        .cost
        .as_ref()
        .map(|c| format!("{} {}", money(c.amount), c.unit))
        .unwrap_or_else(|| "nothing charged".into());
    match &outcome.error {
        None => {
            let file = outcome
                .job
                .outputs
                .first()
                .map(|f| p.rel(&p.job_dir(outcome.job.id).join(f)))
                .unwrap_or_default();
            println!("{} done · {cost} · {file}", outcome.take);
            if p.shot(r)?.selected_take.is_none() {
                println!("pick it with `shotline select {}`", outcome.take);
            }
            Ok(())
        }
        Some(e) => bail!("{} {}: {e} ({cost})", outcome.take, outcome.job.status),
    }
}
