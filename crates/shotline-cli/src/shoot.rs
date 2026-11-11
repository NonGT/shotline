use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use shotline_core::ledger::{Books, money, pricing};
use shotline_core::protocol::{MediaKind, Op, Quote, Rate, Reference, Request, State, TakeFile};
use shotline_core::{
    DraftFor, Job, JobStatus, Project, ShotRef, Translated, UserConfig, build_draft, finish_draft,
    looks_english, user_dir,
};
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
        for c in &plan.built.cautions {
            eprintln!("note: {r}: {c}");
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

pub fn confirm(question: &str) -> Result<bool> {
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
    teller(name, |l| println!("{l}"))
}

/// Progress as lines, to `say`.
fn teller(name: String, mut say: impl FnMut(String)) -> impl FnMut(Event) {
    let mut last: Option<State> = None;
    let mut name = name;
    move |ev| match ev {
        Event::Reserved { total, currency } => say(format!("reserved {} {currency}", money(total))),
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
                say(format!(
                    "step {}/{of}: {what} · {plugin} {model}{r}",
                    index + 1
                ));
            }
        }
        Event::Submitted { plugin_job } => say(format!("sent to {name} ({plugin_job})")),
        Event::Cancelling => say("cancelling...".into()),
        Event::Measured(Some(a)) => say(format!(
            "{name} balance dropped {} {}",
            money(a.amount),
            a.unit
        )),
        Event::Measured(None) => say(format!(
            "{name} balance hasn't moved yet; booked the provider's price"
        )),
        Event::Learned {
            unit,
            rate,
            currency,
        } => say(format!(
            "measured {unit} at {} {currency}, kept as a fallback rate",
            shotline_core::ledger::rate(rate)
        )),
        Event::Status(s) => {
            if last != Some(s.state) {
                match s.progress {
                    Some(pr) if s.state == State::Running => {
                        say(format!("running ({}%)", (pr * 100.0).round()))
                    }
                    _ => say(format!("{:?}", s.state).to_lowercase()),
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

/// A take worked out and priced, with what it takes to run it. Fields drop in order, so the
/// runtime goes after the plugins.
struct Planned {
    opened: Opened,
    b: Books,
    plan: Plan,
    cost: String,
    rt: Runtime,
}

fn plan_take(p: &Project, r: ShotRef, from: Option<usize>, of: Option<&str>) -> Result<Planned> {
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
    let cost = if let [only] = plan.steps.as_slice() {
        price(&only.quote, only.base, &plan.currency, only.pricing)
    } else {
        format!("{} {}", money(plan.total), plan.currency)
    };
    Ok(Planned {
        opened,
        b,
        plan,
        cost,
        rt,
    })
}

/// The text model's request to put the shot's prompt into English, when that's wanted and it
/// isn't done already.
fn translation(p: &Project, r: ShotRef, translate: bool) -> Result<Option<(String, Request)>> {
    let shot = p.shot(r)?;
    if !translate
        || shot.prompt.trim().is_empty()
        || looks_english(&shot.prompt)
        || shot.translated().is_some()
    {
        return Ok(None);
    }
    to_english(p, shot.prompt).map(Some)
}

/// The text model's request to put `text` into English.
pub fn to_english(p: &Project, text: String) -> Result<(String, Request)> {
    let installed = plugins::load();
    let catalog = plugins::catalog(p, &installed)?;
    let b = build_draft(p, &catalog, &DraftFor::Text(text), None, None)?;
    let req = Request {
        op: Op::TextGenerate,
        model: b.model,
        prompt: Some(b.text),
        ..Request::default()
    };
    Ok((b.plugin, req))
}

/// Puts the prompt into English and keeps it with the shot, so every take after sends it too.
fn translate_prompt(p: &Project, r: ShotRef, plugin: &str, req: &Request) -> Result<String> {
    let from = p.shot(r)?.prompt;
    let what = format!("translation for {r}");
    let job = spend_once(p, plugin, req, Some(r), &what, None, Go::Yes)?
        .context("the translation wasn't made")?;
    let file = job
        .outputs
        .first()
        .with_context(|| format!("job {} sent back no text", job.id))?;
    let raw = std::fs::read_to_string(p.job_dir(job.id).join(file))?;
    let text = finish_draft(p, &raw).with_context(|| format!("job {}", job.id))?;
    let mut shot = p.shot(r)?;
    shot.english = Some(Translated {
        from,
        text: text.clone(),
    });
    p.save_shot(r, &shot)?;
    Ok(text)
}

/// What a take of the shot would cost, e.g. "0.40 USD", and its translation if there's one.
/// Free.
pub fn take_quote(p: &Project, r: ShotRef, translate: bool) -> Result<String> {
    let cost = plan_take(p, r, None, None)?.cost;
    Ok(match translation(p, r, translate)? {
        Some((tp, treq)) => format!("{cost} + {} to translate", quote_once(p, &tp, &treq)?),
        None => cost,
    })
}

/// Runs a take if it still costs `at`, telling `say` how it goes. Returns a line about it.
pub fn take_at(
    p: &Project,
    r: ShotRef,
    at: &str,
    translate: bool,
    mut say: impl FnMut(String),
) -> Result<String> {
    if let Some((tp, treq)) = translation(p, r, translate)? {
        let total = take_quote(p, r, translate)?;
        if total != at {
            bail!("the price changed from {at} to {total}; nothing spent");
        }
        say("translating the prompt".into());
        translate_prompt(p, r, &tp, &treq)?;
        let cost = plan_take(p, r, None, None)?.cost;
        return take_at(p, r, &cost, false, say);
    }
    // rt bound first so it drops last, after the plugins.
    let Planned {
        rt,
        opened,
        b,
        plan,
        cost,
    } = plan_take(p, r, None, None)?;
    if cost != at {
        bail!("the price changed from {at} to {cost}; nothing spent");
    }
    let opts = options();
    let outcome = rt.block_on(shotline_engine::take(
        p,
        &b,
        &opened,
        &plan,
        &opts,
        teller(plan.built.plugin.clone(), say),
    ))?;
    drop(opened);
    let spent = spent(&outcome);
    match &outcome.error {
        None => {
            let sent = to_editors(p, &outcome);
            let sent: String = sent.iter().map(|l| format!(" · {l}")).collect();
            Ok(format!("{} done · {spent}{sent}", outcome.take))
        }
        Some(e) => bail!("{} {}: {e} ({spent})", outcome.take, outcome.job.status),
    }
}

/// The finished take, handed to the editor plugins so it's at hand there.
fn to_editors(p: &Project, outcome: &shotline_engine::Outcome) -> Vec<String> {
    let Some(file) = outcome.job.outputs.first() else {
        return vec![];
    };
    // The editor runs elsewhere, so no paths relative to here.
    let film = std::path::absolute(&p.root).unwrap_or_else(|_| p.root.clone());
    let take = TakeFile {
        name: outcome.take.to_string(),
        shot: outcome.take.shot.to_string(),
        file: std::path::absolute(p.job_dir(outcome.job.id).join(file))
            .unwrap_or_else(|_| film.join(file)),
    };
    crate::plugins::send_take(&crate::plugins::load(), &film, take)
}

fn spent(outcome: &shotline_engine::Outcome) -> String {
    let spent: Vec<String> = outcome
        .take
        .costs()
        .iter()
        .map(|c| format!("{} {}", money(c.amount), c.unit))
        .collect();
    if spent.is_empty() {
        "nothing charged".to_string()
    } else {
        spent.join(" + ")
    }
}

/// Quotes, asks, then runs a take of a shot. `from` reuses the steps before it from an earlier
/// take (the latest by default).
pub fn take(
    p: &Project,
    r: ShotRef,
    yes: bool,
    from: Option<usize>,
    of: Option<&str>,
    translate: bool,
) -> Result<()> {
    if let Some((tp, treq)) = translation(p, r, translate)? {
        let total = take_quote(p, r, translate)?;
        if !yes
            && !confirm(&format!(
                "spend {total} on {r}, the prompt translated first?"
            ))?
        {
            println!("nothing spent");
            return Ok(());
        }
        println!("in English: {}", translate_prompt(p, r, &tp, &treq)?);
        return take(p, r, true, from, of, false);
    }
    // rt bound first so it drops last, after the plugins.
    let Planned {
        rt,
        opened,
        b,
        plan,
        cost,
    } = plan_take(p, r, from, of)?;
    for w in &plan.built.warnings {
        eprintln!("warning: {w}");
    }
    for c in &plan.built.cautions {
        eprintln!("note: {c}");
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
    if plan.steps.len() > 1 {
        print_steps(&plan);
    }
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

    let spent = spent(&outcome);
    match &outcome.error {
        None => {
            let file = outcome
                .job
                .outputs
                .first()
                .map(|f| p.rel(&p.job_dir(outcome.job.id).join(f)))
                .unwrap_or_default();
            println!("{} done · {spent} · {file}", outcome.take);
            for l in to_editors(p, &outcome) {
                println!("{l}");
            }
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
    let Some(job) = spend_once(
        p,
        &a.plugin,
        &req,
        None,
        &a.op.to_string(),
        None,
        a.yes.into(),
    )?
    else {
        return Ok(());
    };
    for f in &job.outputs {
        println!("{}", p.rel(&p.job_dir(job.id).join(f)));
    }
    println!("done · {} · job {}", job_cost(&job), job.id);
    Ok(())
}

/// Runs one operation outside any shot if it still costs `at`; its first output.
pub fn op_at(p: &Project, plugin: &str, req: &Request, at: &str) -> Result<PathBuf> {
    let job = spend_once(p, plugin, req, None, &req.op.to_string(), None, Go::At(at))?
        .ok_or_else(|| anyhow::anyhow!("nothing spent"))?;
    match job.outputs.first() {
        Some(f) => Ok(p.job_dir(job.id).join(f)),
        None => bail!("{plugin} made nothing"),
    }
}

pub fn job_cost(job: &Job) -> String {
    job.cost
        .as_ref()
        .map(|c| format!("{} {}", money(c.amount), c.unit))
        .unwrap_or_else(|| "nothing charged".into())
}

/// Quotes one request, asks, then runs it as a job of its own. None if the answer was no.
/// Whether to spend once the price is known.
pub enum Go<'a> {
    Ask,
    Yes,
    /// Agreed to elsewhere (the app) at this price; a different one stops it.
    At(&'a str),
}

impl From<bool> for Go<'_> {
    fn from(yes: bool) -> Self {
        if yes { Go::Yes } else { Go::Ask }
    }
}

// Fields drop in order: the plugins before their runtime.
struct Priced {
    opened: Opened,
    rt: Runtime,
    b: Books,
    quote: Quote,
    cost: String,
}

fn price_once(p: &Project, plugin_name: &str, req: &Request) -> Result<Priced> {
    let installed = plugins::load();
    let rt = plugins::runtime();
    let mut opened = Opened::new(&installed)?;
    opened.load_rates(&rt, [&plugin_name.to_string()]);
    let b = books(p, &opened.rates)?;
    let quote = rt.block_on(opened.plugin(plugin_name)?.quote(req))?;
    let c = b.rates.convert(quote.amount, &quote.unit, &b.currency)?;
    let cost = price(
        &quote,
        c.amount,
        &b.currency,
        pricing(&quote.unit, &c.source),
    );
    Ok(Priced {
        opened,
        rt,
        b,
        quote,
        cost,
    })
}

/// What a request would cost, said the way `spend_once` says it.
pub fn quote_once(p: &Project, plugin_name: &str, req: &Request) -> Result<String> {
    Ok(price_once(p, plugin_name, req)?.cost)
}

pub fn spend_once(
    p: &Project,
    plugin_name: &str,
    req: &Request,
    shot: Option<ShotRef>,
    what: &str,
    brief: Option<shotline_engine::Brief>,
    go: Go,
) -> Result<Option<Job>> {
    spend(p, plugin_name, req, (shot, None), what, brief, None, go)
}

#[allow(clippy::too_many_arguments)]
pub fn spend(
    p: &Project,
    plugin_name: &str,
    req: &Request,
    (shot, bridge): (Option<ShotRef>, Option<shotline_core::Ulid>),
    what: &str,
    brief: Option<shotline_engine::Brief>,
    who: Option<&str>,
    go: Go,
) -> Result<Option<Job>> {
    // rt bound first so it drops last, after the plugins.
    let Priced {
        rt,
        opened,
        b,
        quote,
        cost,
    } = price_once(p, plugin_name, req)?;
    let name = plugin_name.to_string();
    println!("{what} · {plugin_name} {} · {cost}", req.model);
    show_balances(&rt, &opened, &b, &[(name.clone(), quote.clone())]);
    match go {
        Go::Yes => {}
        Go::At(agreed) if agreed == cost => {}
        Go::At(agreed) => bail!("the price changed from {agreed} to {cost}; nothing spent"),
        Go::Ask => {
            if !confirm(&format!("spend {cost}?"))? {
                println!("nothing spent");
                return Ok(None);
            }
        }
    }
    let plugin = opened.plugin(plugin_name)?;
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
                bridge,
                brief,
                who,
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

/// What recording line `i` (from 0) of `r` on its own asks for: the shot's recipe's tts step if
/// it has one, else the speaker's voice as set.
fn say_request(p: &Project, r: ShotRef, i: usize) -> Result<(String, Request)> {
    let shot = p.shot(r)?;
    let line = shot
        .lines
        .get(i)
        .with_context(|| format!("{r} has no line {}", i + 1))?;
    let steps = p.steps(&shot)?;
    let step = steps.iter().find(|s| s.op == Op::AudioTts);
    shotline_engine::line_request(p, step, line)
}

fn effect_request(model: &str, text: &str, length: Option<f64>) -> Result<Request> {
    if text.trim().is_empty() {
        bail!("say what it sounds like first");
    }
    Ok(Request {
        op: Op::AudioSfx,
        model: model.to_string(),
        prompt: Some(text.to_string()),
        duration: length,
        ..Request::default()
    })
}

pub fn effect_quote(
    p: &Project,
    plugin: &str,
    model: &str,
    text: &str,
    length: Option<f64>,
) -> Result<String> {
    quote_once(p, plugin, &effect_request(model, text, length)?)
}

/// Makes a sound effect for a sound over one of `r`'s takes, or a bridge's. None if it wasn't
/// agreed to.
pub fn effect(
    p: &Project,
    r: Option<ShotRef>,
    (plugin, model): (&str, &str),
    text: &str,
    length: Option<f64>,
    go: Go,
) -> Result<Option<Job>> {
    let req = effect_request(model, text, length)?;
    let what = format!("sound for {}", over(r));
    let Some(job) = spend(p, plugin, &req, (r, None), &what, None, None, go)? else {
        return Ok(None);
    };
    if job.status != JobStatus::Done {
        bail!(
            "{what} wasn't made: {}",
            job.error.as_deref().unwrap_or("no reason given")
        );
    }
    Ok(Some(job))
}

fn over(r: Option<ShotRef>) -> String {
    r.map_or("a bridge".into(), |r| r.to_string())
}

/// What `who` saying `text` asks for, as a line of `r` would, for a sound over one of its takes.
/// Over a bridge, as `who`'s voice says it.
fn speech_request(
    p: &Project,
    r: Option<ShotRef>,
    who: &str,
    text: &str,
) -> Result<(String, Request)> {
    if text.trim().is_empty() {
        bail!("type what they say first");
    }
    let steps = match r {
        Some(r) => p.steps(&p.shot(r)?)?,
        None => vec![],
    };
    let step = steps.iter().find(|s| s.op == Op::AudioTts);
    let line = shotline_core::Line {
        who: who.trim_start_matches('@').to_string(),
        text: text.to_string(),
        at: None,
        audio: None,
    };
    shotline_engine::line_request(p, step, &line)
}

pub fn speech_quote(p: &Project, r: Option<ShotRef>, who: &str, text: &str) -> Result<String> {
    let (plugin, req) = speech_request(p, r, who, text)?;
    quote_once(p, &plugin, &req)
}

/// Records `who` saying `text`, not as one of the shot's lines. None if it wasn't agreed to.
pub fn speech(
    p: &Project,
    r: Option<ShotRef>,
    who: &str,
    text: &str,
    go: Go,
) -> Result<Option<Job>> {
    let (plugin, req) = speech_request(p, r, who, text)?;
    let what = format!("speech for {}", over(r));
    let who = who.trim_start_matches('@');
    let Some(job) = spend(p, &plugin, &req, (r, None), &what, None, Some(who), go)? else {
        return Ok(None);
    };
    if job.status != JobStatus::Done {
        bail!(
            "{what} wasn't recorded: {}",
            job.error.as_deref().unwrap_or("no reason given")
        );
    }
    Ok(Some(job))
}

pub fn say_quote(p: &Project, r: ShotRef, i: usize) -> Result<String> {
    let (plugin, req) = say_request(p, r, i)?;
    quote_once(p, &plugin, &req)
}

/// Records line `i` on its own and picks it for the shot's takes. None if it wasn't agreed to.
pub fn say(p: &Project, r: ShotRef, i: usize, go: Go) -> Result<Option<Job>> {
    let (plugin, req) = say_request(p, r, i)?;
    let what = format!("line {} of {r}", i + 1);
    let who = p.shot(r)?.lines[i].who.clone();
    let Some(job) = spend(
        p,
        &plugin,
        &req,
        (Some(r), None),
        &what,
        None,
        Some(&who),
        go,
    )?
    else {
        return Ok(None);
    };
    if job.status != JobStatus::Done {
        bail!(
            "{what} wasn't recorded: {}",
            job.error.as_deref().unwrap_or("no reason given")
        );
    }
    p.set_line_audio(r, i, Some(&job))?;
    Ok(Some(job))
}
