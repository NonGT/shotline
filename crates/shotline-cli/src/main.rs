use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use shotline_core::{Kind, ModelOverride, Project, Shot, ShotRef, Transition, build};

mod plugins;

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
    /// List a shot's takes
    Takes { shot: ShotRef },
    /// Pick the take a shot uses: `select 01-010 3` or `select 01-010#3`
    Select { shot: String, take: Option<String> },
    /// Installed plugins
    #[command(subcommand)]
    Plugin(PluginCmd),
    /// Serves the built-in fake plugin over stdio, for tests and dry runs
    #[command(name = "__fake-plugin", hide = true)]
    FakePlugin,
}

#[derive(Subcommand)]
enum PluginCmd {
    Ls,
    /// Check a plugin talks the protocol properly. Never spends anything.
    Doctor {
        name: String,
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
                ..Shot::default()
            };
            println!("{}", open()?.add_shot(chapter, after, shot)?);
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
                    .map(|c| format!("{} {}", c.amount, c.unit))
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

        Cmd::Plugin(PluginCmd::Ls) => plugins::ls(&plugins::load()),
        Cmd::Plugin(PluginCmd::Doctor { ref name }) => plugins::doctor(&plugins::load(), name)?,
        Cmd::FakePlugin => {
            plugins::runtime().block_on(shotline_plugin::serve(
                shotline_plugin::fake::FakePlugin::default(),
            ))?;
        }
    }
    Ok(())
}

/// "010" or "01-010" -> 10
fn parse_shot_number(s: &str) -> Result<u32> {
    let n = s.rsplit(['-', '/']).next().unwrap_or(s);
    match n.parse() {
        Ok(n) => Ok(n),
        Err(_) => bail!("bad shot number {s:?}"),
    }
}
