use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use shotline_core::ledger::report::By;
use shotline_core::{Kind, ModelOverride, Project, Shot, ShotRef, Transition, Voice, build};

mod cost;
mod plugins;
mod shoot;
mod timeline;

#[derive(Parser)]
#[command(
    name = "shotline",
    version,
    about = "Plan and shoot AI films shot by shot"
)]
struct Cli {
    /// Project folder (default: the one containing the current directory)
    #[arg(short = 'C', long, global = true)]
    project: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new project folder
    Init {
        dir: PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    /// Cast, sets and props
    #[command(subcommand)]
    Element(ElementCmd),
    /// Chapters group shots, numbered 01, 02, ...
    #[command(subcommand)]
    Chapter(ChapterCmd),
    /// Shots, numbered 010, 020, ... within a chapter
    #[command(subcommand)]
    Shot(ShotCmd),
    /// Show the exact prompt and references a shot would send, without spending anything
    Prompt { shot: ShotRef },
    /// Price a shot (01-010) or a whole chapter (1) without spending anything
    Quote { target: String },
    /// Quote, ask, then generate a take of a shot
    Take {
        shot: ShotRef,
        /// Don't ask before spending
        #[arg(long)]
        yes: bool,
        /// Reuse the steps before this one from an earlier take and run the rest
        #[arg(long)]
        from_step: Option<usize>,
        /// Which take to reuse from (default: the latest)
        #[arg(long, requires = "from_step")]
        of: Option<String>,
    },
    /// Run one operation outside any shot, e.g. `run video.upscale --plugin p --model m --input video=a.mp4`
    Run {
        op: shotline_core::protocol::Op,
        #[arg(long)]
        plugin: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        duration: Option<f64>,
        /// Named input file: --input video=take.mp4
        #[arg(long = "input", value_parser = key_path)]
        inputs: Vec<(String, PathBuf)>,
        /// Reference file (image, audio or video)
        #[arg(long = "ref")]
        refs: Vec<PathBuf>,
        /// Plugin parameter: --param seed=7 (JSON values are parsed)
        #[arg(long = "param", value_parser = key_value)]
        params: Vec<(String, String)>,
        #[arg(long)]
        yes: bool,
    },
    /// List a shot's takes
    Takes { shot: ShotRef },
    /// Pick the take a shot uses: `select 01-010 3` or `select 01-010#3`
    Select { shot: String, take: Option<String> },
    /// Installed plugins
    #[command(subcommand)]
    Plugin(PluginCmd),
    /// What the film has cost
    #[command(args_conflicts_with_subcommands = true)]
    Cost {
        #[command(subcommand)]
        cmd: Option<CostCmd>,
        /// chapter, shot, take, op, plugin or day
        #[arg(long, default_value = "shot")]
        by: By,
        #[arg(long)]
        csv: bool,
    },
    /// The cut: shots in order with their selected takes
    Timeline {
        #[command(subcommand)]
        cmd: Option<TimelineCmd>,
    },
    /// Serves the built-in fake plugin over stdio, for tests and dry runs
    #[command(name = "__fake-plugin", hide = true)]
    FakePlugin,
}

#[derive(Subcommand)]
enum CostCmd {
    /// Check nobody edited the ledger
    Verify,
    /// Every rate in use and where it comes from
    Rates,
    /// Compare the ledger with what providers say they charged and how their balances moved
    Reconcile {
        #[arg(long)]
        plugin: Option<String>,
    },
    /// Record a correction, e.g. a fee or promo credit (negative)
    Adjust {
        #[arg(allow_hyphen_values = true)]
        amount: f64,
        unit: String,
        #[arg(long)]
        note: String,
    },
    /// Record money back for a job
    Refund {
        job: String,
        amount: f64,
        unit: String,
        #[arg(long, default_value = "")]
        note: String,
    },
}

#[derive(Subcommand)]
enum TimelineCmd {
    /// Join the selected takes into one video with ffmpeg
    Render {
        /// Where to write it (default: renders/<time>.mp4)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Skip colour matching
        #[arg(long)]
        no_colormatch: bool,
        /// Crossfade at each join in seconds, instead of timeline.toml's
        #[arg(long)]
        xfade: Option<f64>,
    },
}

#[derive(Subcommand)]
enum PluginCmd {
    Ls,
    /// Check a plugin talks the protocol properly. Never spends anything.
    Doctor {
        name: String,
    },
    /// What's left on each provider account
    Balance {
        name: Option<String>,
    },
}

#[derive(Subcommand)]
enum ElementCmd {
    Add {
        /// cast, set or prop
        kind: Kind,
        name: String,
        #[arg(long)]
        handle: Option<String>,
        #[arg(long, default_value = "")]
        desc: String,
    },
    /// Add an anchor image to an element
    Anchor {
        handle: String,
        #[arg(long, required_unless_present = "generate")]
        file: Option<PathBuf>,
        #[arg(long)]
        generate: bool,
    },
    /// Give a cast member a voice for their lines
    Voice {
        handle: String,
        #[arg(long)]
        plugin: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        voice_id: Option<String>,
        /// A recording to clone from, for plugins that do that
        #[arg(long)]
        sample: Option<PathBuf>,
        /// Voice setting passed to the plugin, e.g. --set stability=0.5
        #[arg(long = "set", value_parser = key_value)]
        settings: Vec<(String, String)>,
    },
    Ls,
}

#[derive(Subcommand)]
enum ChapterCmd {
    Add { title: String },
    Ls,
}

#[derive(Subcommand)]
enum ShotCmd {
    Add {
        chapter: String,
        /// Insert after this shot number (e.g. 010) instead of at the end
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value = "")]
        title: String,
        #[arg(long, default_value = "")]
        plot: String,
        #[arg(long)]
        prompt: String,
        #[arg(long)]
        duration: Option<f64>,
        #[arg(long, default_value = "cut")]
        transition: Transition,
        /// Plugin for this shot instead of the project default
        #[arg(long)]
        plugin: Option<String>,
        /// Model for this shot instead of the project default
        #[arg(long)]
        model: Option<String>,
        /// Recipe from shotline.toml, e.g. talking
        #[arg(long)]
        recipe: Option<String>,
    },
    /// Add a line of dialogue: `shot line 01-010 paka "..."`
    Line {
        shot: ShotRef,
        who: String,
        text: String,
    },
    Ls {
        chapter: Option<String>,
    },
    Show {
        shot: ShotRef,
    },
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let open = || -> Result<Project> {
        Ok(match &cli.project {
            Some(dir) => Project::open(dir)?,
            None => Project::discover(&std::env::current_dir()?)?,
        })
    };

    match cli.cmd {
        Cmd::Init { ref dir, ref name } => {
            let p = Project::init(dir, name.as_deref())?;
            println!("created {} in {}", p.config.name, dir.display());
        }

        Cmd::Element(ElementCmd::Add {
            kind,
            ref name,
            ref handle,
            ref desc,
        }) => {
            let el = open()?.add_element(kind, name, handle.as_deref(), desc)?;
            println!("added {} @{} ({})", el.name, el.handle, el.kind);
        }
        Cmd::Element(ElementCmd::Anchor {
            ref handle,
            ref file,
            generate,
        }) => {
            if generate {
                bail!("generating anchors isn't available yet, use --file");
            }
            let p = open()?;
            let dest = p.add_anchor(handle, file.as_ref().unwrap())?;
            println!("{}", p.rel(&dest));
        }
        Cmd::Element(ElementCmd::Voice {
            ref handle,
            ref plugin,
            ref model,
            ref voice_id,
            ref sample,
            ref settings,
        }) => {
            let p = open()?;
            let sample = match sample {
                Some(f) => {
                    let full = std::env::current_dir()?.join(f);
                    if !full.is_file() {
                        bail!("{} doesn't exist", f.display());
                    }
                    Some(p.rel(&full))
                }
                None => None,
            };
            let mut set = toml::Table::new();
            for (k, v) in settings {
                let v = v
                    .parse::<toml::Value>()
                    .unwrap_or_else(|_| toml::Value::String(v.clone()));
                set.insert(k.clone(), v);
            }
            let el = p.set_voice(
                handle,
                Voice {
                    plugin: plugin.clone(),
                    model: model.clone(),
                    voice_id: voice_id.clone(),
                    sample,
                    settings: set,
                },
            )?;
            println!("@{} speaks with {plugin}", el.handle);
        }
        Cmd::Element(ElementCmd::Ls) => {
            let p = open()?;
            for el in p.elements()? {
                let n = p.anchors(&el)?.len();
                let s = if n == 1 { "" } else { "s" };
                println!(
                    "{:<5} @{:<16} {} ({n} anchor{s})",
                    el.kind, el.handle, el.name
                );
            }
        }

        Cmd::Chapter(ChapterCmd::Add { ref title }) => {
            let p = open()?;
            let c = p.add_chapter(title)?;
            println!("{} {}", c.id(), p.rel(&c.dir));
        }
        Cmd::Chapter(ChapterCmd::Ls) => {
            for c in open()?.chapters()? {
                println!("{}  {}", c.id(), c.title);
            }
        }

        Cmd::Shot(ShotCmd::Add {
            ref chapter,
            ref after,
            ref title,
            ref plot,
            ref prompt,
            duration,
            transition,
            ref plugin,
            ref model,
            ref recipe,
        }) => {
            let after = after.as_deref().map(parse_shot_number).transpose()?;
            let model = (plugin.is_some() || model.is_some()).then(|| ModelOverride {
                plugin: plugin.clone(),
                id: model.clone(),
                ..ModelOverride::default()
            });
            let shot = Shot {
                title: title.clone(),
                plot: plot.clone(),
                prompt: prompt.clone(),
                duration,
                transition,
                model,
                recipe: recipe.clone(),
                ..Shot::default()
            };
            println!("{}", open()?.add_shot(chapter, after, shot)?);
        }
        Cmd::Shot(ShotCmd::Line {
            shot,
            ref who,
            ref text,
        }) => {
            let s = open()?.add_line(shot, who, text)?;
            println!(
                "{shot} has {} line{}",
                s.lines.len(),
                if s.lines.len() == 1 { "" } else { "s" }
            );
        }
        Cmd::Shot(ShotCmd::Ls { ref chapter }) => {
            let p = open()?;
            for (r, s) in p.shots(chapter.as_deref())? {
                let secs = s.duration.unwrap_or(p.config.duration);
                let label = [&s.title, &s.plot, &s.prompt]
                    .into_iter()
                    .find(|t| !t.is_empty())
                    .unwrap();
                println!("{r}  {:<8} {secs:>4}s  {label}", s.transition);
            }
        }
        Cmd::Shot(ShotCmd::Show { shot }) => {
            let p = open()?;
            p.shot(shot)?;
            println!("# {shot}");
            print!("{}", std::fs::read_to_string(p.shot_path(shot)?)?);
        }

        Cmd::Prompt { shot } => {
            let p = open()?;
            let installed = plugins::load();
            let b = build(&p, &plugins::catalog(&p, &installed)?, shot)?;
            for w in &b.warnings {
                eprintln!("warning: {w}");
            }
            println!(
                "shot {} · {} {} · {}s · {} · {}",
                b.shot, b.plugin, b.model, b.duration, b.resolution, b.aspect
            );
            if let Some(sf) = &b.start_frame {
                println!("start frame: {} (from {})", sf.path, sf.from);
            }
            if !b.references.is_empty() {
                println!("references:");
                for r in &b.references {
                    println!("  {:<12} @{:<16} {}", r.label, r.handle, r.path);
                }
            }
            println!("\n{}", b.text);
        }

        Cmd::Quote { ref target } => shoot::quote(&open()?, target)?,
        Cmd::Take {
            shot,
            yes,
            from_step,
            ref of,
        } => shoot::take(&open()?, shot, yes, from_step, of.as_deref())?,
        Cmd::Run {
            ref op,
            ref plugin,
            ref model,
            ref prompt,
            duration,
            ref inputs,
            ref refs,
            ref params,
            yes,
        } => shoot::run(
            &open()?,
            shoot::RunArgs {
                op: op.clone(),
                plugin: plugin.clone(),
                model: model.clone(),
                prompt: prompt.clone(),
                duration,
                inputs: inputs.clone(),
                refs: refs.clone(),
                params: params.clone(),
                yes,
            },
        )?,

        Cmd::Takes { shot } => {
            let p = open()?;
            let selected = p.shot(shot)?.selected_take;
            for t in p.takes(shot)? {
                let mark = if selected.as_deref() == Some(&t.id.to_string()) {
                    "*"
                } else {
                    " "
                };
                let cost = t
                    .costs()
                    .iter()
                    .map(|c| format!("{} {}", shotline_core::ledger::money(c.amount), c.unit))
                    .collect::<Vec<_>>()
                    .join(" + ");
                let steps = t.jobs.len();
                let s = if steps == 1 { "" } else { "s" };
                println!(
                    "{mark} {t}  {:<9} {steps} step{s}  {cost}  {}",
                    t.status(),
                    t.id
                );
            }
        }
        Cmd::Select { ref shot, ref take } => {
            let (shot, take) = match (shot.split_once('#'), take) {
                (Some((s, t)), None) => (s.parse::<ShotRef>()?, t.to_string()),
                (None, Some(t)) => (shot.parse::<ShotRef>()?, t.clone()),
                _ => bail!("give the take as `01-010 3` or `01-010#3`"),
            };
            let t = open()?.select_take(shot, &take)?;
            println!("{t} selected");
        }

        Cmd::Cost { ref cmd, by, csv } => {
            let p = open()?;
            match cmd {
                None => cost::show(&p, by, csv)?,
                Some(CostCmd::Verify) => cost::verify(&p)?,
                Some(CostCmd::Rates) => cost::rates(&p, &plugins::load())?,
                Some(CostCmd::Reconcile { plugin }) => {
                    cost::reconcile_cmd(&p, &plugins::load(), plugin.as_deref())?
                }
                Some(CostCmd::Adjust { amount, unit, note }) => {
                    cost::adjust(&p, *amount, unit, note)?
                }
                Some(CostCmd::Refund {
                    job,
                    amount,
                    unit,
                    note,
                }) => cost::refund(&p, job, *amount, unit, note)?,
            }
        }
        Cmd::Plugin(PluginCmd::Ls) => plugins::ls(&plugins::load()),
        Cmd::Plugin(PluginCmd::Doctor { ref name }) => plugins::doctor(&plugins::load(), name)?,
        Cmd::Plugin(PluginCmd::Balance { ref name }) => {
            plugins::balance(&plugins::load(), name.as_deref())?
        }
        Cmd::Timeline { ref cmd } => {
            let p = open()?;
            match cmd {
                None => timeline::show(&p)?,
                Some(TimelineCmd::Render {
                    output,
                    no_colormatch,
                    xfade,
                }) => timeline::render(&p, output.as_deref(), !no_colormatch, *xfade)?,
            }
        }
        Cmd::FakePlugin => {
            plugins::runtime().block_on(shotline_plugin::serve(
                shotline_plugin::fake::FakePlugin::default(),
            ))?;
        }
    }
    Ok(())
}

fn key_value(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| format!("expected key=value, got {s:?}"))
}

fn key_path(s: &str) -> Result<(String, PathBuf), String> {
    key_value(s).map(|(k, v)| (k, PathBuf::from(v)))
}

/// "010" or "01-010" -> 10
fn parse_shot_number(s: &str) -> Result<u32> {
    let n = s.rsplit(['-', '/']).next().unwrap_or(s);
    match n.parse() {
        Ok(n) => Ok(n),
        Err(_) => bail!("bad shot number {s:?}"),
    }
}
