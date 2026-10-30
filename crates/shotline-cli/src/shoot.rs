use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use shotline_core::ledger::{Books, money, pricing};
use shotline_core::protocol::{MediaKind, Op, Quote, Rate, Reference, Request, State};
use shotline_core::{Job, Project, ShotRef, UserConfig, user_dir};
use shotline_engine::{Event, Options, Plan, Plugins, price};
use shotline_plugin::Plugin;
use tokio::runtime::Runtime;

use crate::plugins::{self, Installed};

/// Every installed plugin, opened (which costs nothing until one is called), plus the rates of
/// the ones a command actually uses.
struct Opened {
    plugins: BTreeMap<String, Box<dyn Plugin>>,
    rates: Vec<Rate>,
}

impl Opened {
    fn new(installed: &Installed) -> Result<Self> {
        let mut plugins = BTreeMap::new();
        for p in &installed.plugins {
            if p.description.is_ok() {
                plugins.insert(p.name.clone(), plugins::open(installed, &p.name)?);
            }
        }
        Ok(Self {
            plugins,
            rates: vec![],
        })
    }

    fn load_rates<'a>(&mut self, rt: &Runtime, names: impl IntoIterator<Item = &'a String>) {
        for n in names {
            if let Some(p) = self.plugins.get(n)
                && let Ok(r) = rt.block_on(p.rates())
            {
                self.rates.extend(r.rates);
            }
        }
    }
}

impl Plugins for Opened {
    fn plugin(&self, name: &str) -> Result<&dyn Plugin> {
        self.plugins.get(name).map(|p| p.as_ref()).with_context(|| {
            format!("no plugin called {name}; `shotline plugin ls` shows what's installed")
        })
    }
}

/// Plugins a take of `r` would call, so their rates can be loaded before quoting.
fn needed(p: &Project, r: ShotRef) -> Result<BTreeSet<String>> {
    let shot = p.shot(r)?;
    let mut names = BTreeSet::new();
    names.insert(
        shot.model
            .as_ref()
            .and_then(|m| m.plugin.clone())
            .unwrap_or_else(|| p.config.plugin.clone()),
    );
    for step in p.steps(&shot)? {
        if let Some(n) = step.plugin {
            names.insert(n);
        } else if step.op == Op::AudioTts {
            for line in &shot.lines {
                if let Some(v) = p.find_element(&line.who)?.and_then(|e| e.voice) {
                    names.insert(v.plugin);
                }
            }
        }
    }
    Ok(names)
}

fn books(p: &Project, rates: &[Rate]) -> Result<Books> {
    let dir = user_dir();
    Ok(p.books(&dir, &UserConfig::load(&dir)?, rates)?)
}

fn targets(p: &Project, target: &str) -> Result<Vec<ShotRef>> {
    if target.contains(['-', '/']) {
        return Ok(vec![target.parse()?]);
    }
    Ok(p.shots(Some(target))?.into_iter().map(|(r, _)| r).collect())
}

fn print_steps(plan: &Plan) {
    for (i, s) in plan.steps.iter().enumerate() {
        let cost = match &s.reused {
            Some(j) => format!("reused from {}", j.id),
            None => price(&s.quote, s.base, &plan.currency, s.pricing),
        };
        println!("  {i}  {:<18} {} {}  {cost}", s.what, s.plugin, s.model);
    }
}

/// Prices a shot or a whole chapter without spending anything.
pub fn quote(p: &Project, target: &str) -> Result<()> {
    let installed = plugins::load();
    let catalog = plugins::catalog(p, &installed)?;
    let rt = plugins::runtime();
    let mut opened = Opened::new(&installed)?;
    let shots = targets(p, target)?;
    let mut total = 0.0;
    let mut currency = p.config.money.currency.clone();
    for r in &shots {
        opened.load_rates(&rt, &needed(p, *r)?);
        let b = books(p, &opened.rates)?;
        let plan = rt.block_on(shotline_engine::plan(p, &catalog, &b, &opened, *r, None))?;
        for w in &plan.built.warnings {
            eprintln!("warning: {r}: {w}");
        }
        if let [only] = plan.steps.as_slice() {
            println!(
                "{r}  {} {}  {}s  {}",
                only.plugin,
                only.model,
                money(plan.built.duration),
                price(&only.quote, only.base, &plan.currency, only.pricing)
            );
        } else {
            println!(
                "{r}  {}s  {} {}",
                money(plan.built.duration),
                money(plan.total),
                plan.currency
            );
            print_steps(&plan);
        }
        total += plan.total;
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

fn options() -> Options {
    let poll_every = std::env::var("SHOTLINE_POLL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(5));
    Options {
        poll_every,
        learn: Some(user_dir()),
        ..Options::default()
    }
}

/// What's left with each provider about to be used, and a warning when a quote won't fit.
fn show_balances(rt: &Runtime, opened: &Opened, b: &Books, quotes: &[(String, Quote)]) {
    let names: BTreeSet<&String> = quotes.iter().map(|(n, _)| n).collect();
    for name in names {
        let Some(p) = opened.plugins.get(name) else {
            continue;
        };
        let Ok(bal) = rt.block_on(p.balance()) else {
            continue;
        };
        if bal.balances.is_empty() {
            continue;
        }
        let left: Vec<String> = bal
            .balances
            .iter()
            .map(|a| format!("{} {}", money(a.amount), a.unit))
            .collect();
        println!("{name} balance: {}", left.join(", "));
        // Only warns when every quote converts into one of the balances and none fits.
        let short = bal.balances.iter().all(|a| {
            let need: Option<f64> = quotes
                .iter()
                .filter(|(n, _)| n == name)
                .map(|(_, q)| {
                    b.rates
                        .convert(q.amount, &q.unit, &a.unit)
                        .ok()
                        .map(|c| c.amount)
                })
                .sum();
            need.is_some_and(|n| n > a.amount)
        });
        if short {
            eprintln!("warning: {name} balance looks too low for this");
        }
    }
}

/// Prints progress; remembers the last state so it isn't repeated every poll.
fn printer(name: String) -> impl FnMut(Event) {
    let mut last: Option<State> = None;
    let mut name = name;
    move |ev| match ev {
        Event::Reserved { total, currency } => {
            println!("reserved {} {currency}", money(total))
        }
        Event::Step {
            index,
            of,
            what,
            plugin,
            model,
            reused,
        } => {
            last = None;
            name = plugin.clone();
            if of > 1 {
                let r = if reused { " (reused)" } else { "" };
                println!("step {}/{of}: {what} · {plugin} {model}{r}", index + 1);
            }
        }
        Event::Submitted { plugin_job } => println!("sent to {name} ({plugin_job})"),
        Event::Cancelling => println!("cancelling..."),
        Event::Measured(Some(a)) => {
            println!("{name} balance dropped {} {}", money(a.amount), a.unit)
        }
        Event::Measured(None) => {
            println!("{name} balance hasn't moved yet; booked the provider's price")
        }
        Event::Learned {
            unit,
            rate,
            currency,
        } => println!(
            "measured {unit} at {} {currency}, kept as a fallback rate",
            shotline_core::ledger::rate(rate)
        ),
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
    }
}

fn on_ctrl_c(opts: &Options) {
    let cancel = Arc::clone(&opts.cancel);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            cancel.notify_one();
        }
    });
}

/// Quotes, asks, then runs a take of a shot. `from` reuses the steps before it from an earlier
/// take (the latest by default).
pub fn take(
    p: &Project,
    r: ShotRef,
    yes: bool,
    from: Option<usize>,
    of: Option<&str>,
) -> Result<()> {
    let installed = plugins::load();
    let catalog = plugins::catalog(p, &installed)?;
    let rt = plugins::runtime();
    let mut opened = Opened::new(&installed)?;
    opened.load_rates(&rt, &needed(p, r)?);
    let b = books(p, &opened.rates)?;
    let reuse = match from {
        None => None,
        Some(n) => {
            let t = match of {
                Some(spec) => p.take(r, spec)?,
                None => p
                    .takes(r)?
                    .pop()
                    .with_context(|| format!("{r} has no take to reuse steps from"))?,
            };
            Some((t, n))
        }
    };
    let plan = rt.block_on(shotline_engine::plan(
        p,
        &catalog,
        &b,
        &opened,
        r,
        reuse.as_ref().map(|(t, n)| (t, *n)),
    ))?;

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
    let cost = if let [only] = plan.steps.as_slice() {
        price(&only.quote, only.base, &plan.currency, only.pricing)
    } else {
        print_steps(&plan);
        format!("{} {}", money(plan.total), plan.currency)
    };
    let quotes: Vec<(String, Quote)> = plan
        .steps
        .iter()
        .filter(|s| s.reused.is_none())
        .map(|s| (s.plugin.clone(), s.quote.clone()))
        .collect();
    show_balances(&rt, &opened, &b, &quotes);
    if !yes && !confirm(&format!("spend {cost} on {r}?"))? {
        println!("nothing spent");
        return Ok(());
    }

    let opts = options();
    let outcome = rt.block_on(async {
        on_ctrl_c(&opts);
        shotline_engine::take(
            p,
            &b,
            &opened,
            &plan,
            &opts,
            printer(plan.built.plugin.clone()),
        )
        .await
    })?;
    drop(opened);

    let spent: Vec<String> = outcome
        .take
        .costs()
        .iter()
        .map(|c| format!("{} {}", money(c.amount), c.unit))
        .collect();
    let spent = if spent.is_empty() {
        "nothing charged".to_string()
    } else {
        spent.join(" + ")
    };
    match &outcome.error {
        None => {
            let file = outcome
                .job
                .outputs
                .first()
                .map(|f| p.rel(&p.job_dir(outcome.job.id).join(f)))
                .unwrap_or_default();
            println!("{} done · {spent} · {file}", outcome.take);
            if p.shot(r)?.selected_take.is_none() {
                println!("pick it with `shotline select {}`", outcome.take);
            }
            Ok(())
        }
        Some(e) => bail!("{} {}: {e} ({spent})", outcome.take, outcome.job.status),
    }
}

pub struct RunArgs {
    pub op: Op,
    pub plugin: String,
    pub model: String,
    pub prompt: Option<String>,
    pub duration: Option<f64>,
    pub inputs: Vec<(String, PathBuf)>,
    pub refs: Vec<PathBuf>,
    pub params: Vec<(String, String)>,
    pub yes: bool,
}

fn guess_kind(path: &std::path::Path) -> MediaKind {
    let m = shotline_plugin::util::mime(path);
    if m.starts_with("image/") {
        MediaKind::Image
    } else if m.starts_with("audio/") {
        MediaKind::Audio
    } else if m.starts_with("video/") {
        MediaKind::Video
    } else {
        MediaKind::Data
    }
}

/// One operation outside any shot: an upscale, a test line of speech, someone's x.* tool.
pub fn run(p: &Project, a: RunArgs) -> Result<()> {
    let abs = |path: &PathBuf| -> Result<PathBuf> {
        let full = std::env::current_dir()?.join(path);
        if !full.is_file() {
            bail!("{} doesn't exist", path.display());
        }
        Ok(full)
    };
    let mut req = Request {
        op: a.op.clone(),
        model: a.model.clone(),
        prompt: a.prompt.clone(),
        duration: a.duration,
        ..Request::default()
    };
    for (name, path) in &a.inputs {
        req.inputs.insert(name.clone(), abs(path)?);
    }
    for path in &a.refs {
        let path = abs(path)?;
        req.references.push(Reference {
            kind: guess_kind(&path),
            path,
        });
    }
    for (k, v) in &a.params {
        let v = serde_json::from_str(v).unwrap_or(serde_json::Value::String(v.clone()));
        req.params.insert(k.clone(), v);
    }
    let Some(job) = spend_once(p, &a.plugin, &req, None, &a.op.to_string(), a.yes)? else {
        return Ok(());
    };
    for f in &job.outputs {
        println!("{}", p.rel(&p.job_dir(job.id).join(f)));
    }
    println!("done · {} · job {}", job_cost(&job), job.id);
    Ok(())
}

pub fn job_cost(job: &Job) -> String {
    job.cost
        .as_ref()
        .map(|c| format!("{} {}", money(c.amount), c.unit))
        .unwrap_or_else(|| "nothing charged".into())
}

/// Quotes one request, asks, then runs it as a job of its own. None if the answer was no.
pub fn spend_once(
    p: &Project,
    plugin_name: &str,
    req: &Request,
    shot: Option<ShotRef>,
    what: &str,
    yes: bool,
) -> Result<Option<Job>> {
    let installed = plugins::load();
    let rt = plugins::runtime();
    let mut opened = Opened::new(&installed)?;
    let name = plugin_name.to_string();
    opened.load_rates(&rt, [&name]);
    let b = books(p, &opened.rates)?;
    let plugin = opened.plugin(plugin_name)?;
    let quote = rt.block_on(plugin.quote(req))?;
    let c = b.rates.convert(quote.amount, &quote.unit, &b.currency)?;
    let cost = price(
        &quote,
        c.amount,
        &b.currency,
        pricing(&quote.unit, &c.source),
    );
    println!("{what} · {plugin_name} {} · {cost}", req.model);
    show_balances(&rt, &opened, &b, &[(name.clone(), quote.clone())]);
    if !yes && !confirm(&format!("spend {cost}?"))? {
        println!("nothing spent");
        return Ok(None);
    }
    let opts = options();
    let (job, err) = rt.block_on(async {
        on_ctrl_c(&opts);
        shotline_engine::run_once(
            p,
            &b,
            (plugin_name, plugin),
            shotline_engine::Once {
                request: req,
                quote: &quote,
                shot,
            },
            &opts,
            printer(name.clone()),
        )
        .await
    })?;
    drop(opened);
    match err {
        None => Ok(Some(job)),
        Some(e) => bail!("job {} {}: {e} ({})", job.id, job.status, job_cost(&job)),
    }
}
