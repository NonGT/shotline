use std::path::Path;

use anyhow::{Result, bail};
use shotline_builtin::fal::{self, Fal};
use shotline_core::protocol::Description;
use shotline_core::{Catalog, Project, user_dir};
use shotline_plugin::conformance::{self, Options};
use shotline_plugin::discovery::{self, Entry, Source};
use shotline_plugin::{Plugin, protocol::PROTOCOL_VERSION};

pub enum Origin {
    Builtin,
    External(Box<Entry>),
}

pub struct Available {
    pub name: String,
    pub description: Result<Description, String>,
    pub origin: Origin,
}

pub struct Installed {
    pub plugins: Vec<Available>,
    pub problems: Vec<String>,
}

pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn fal_description() -> Description {
    Description {
        name: "fal".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol: PROTOCOL_VERSION,
        env: vec![fal::KEY_VAR.into()],
        models: fal::models(),
    }
}

/// Installed external plugins plus the built-in ones. An external plugin with a built-in's name
/// replaces it.
pub fn load() -> Installed {
    let home = user_dir();
    let (found, problems) = discovery::discover(&home, std::env::var_os("PATH").as_deref());
    let entries = runtime().block_on(discovery::describe_all(
        found,
        &home.join("cache").join("describe"),
    ));
    let mut plugins: Vec<Available> = entries
        .into_iter()
        .map(|e| Available {
            name: e.found.name.clone(),
            description: e.description.clone(),
            origin: Origin::External(Box::new(e)),
        })
        .collect();
    if !plugins.iter().any(|p| p.name == "fal") {
        plugins.insert(
            0,
            Available {
                name: "fal".into(),
                description: Ok(fal_description()),
                origin: Origin::Builtin,
            },
        );
    }
    Installed { plugins, problems }
}

fn dotenv() -> std::path::PathBuf {
    user_dir().join(".env")
}

/// A plugin ready to work, with its keys. Drop it before the runtime it's used on.
pub fn open(installed: &Installed, name: &str) -> Result<Box<dyn Plugin>> {
    let Some(p) = installed.plugins.iter().find(|p| p.name == name) else {
        bail!("no plugin called {name}; `shotline plugin ls` shows what's installed");
    };
    Ok(match &p.origin {
        Origin::Builtin => {
            let (vals, _) = discovery::resolve_env(&[fal::KEY_VAR.to_string()], &dotenv());
            Box::new(Fal::from_env(vals.into_iter().next().map(|(_, v)| v)))
        }
        Origin::External(e) => Box::new(discovery::open(e, &dotenv()).map_err(anyhow::Error::msg)?),
    })
}

/// Built-in models, then installed plugins, then the project's models.toml.
pub fn catalog(project: &Project, installed: &Installed) -> Result<Catalog> {
    let mut c = Catalog::builtin();
    for p in &installed.plugins {
        if let Ok(d) = &p.description {
            c.add_plugin(&d.name, d.models.clone());
        }
    }
    Ok(project.catalog(c)?)
}

fn warn_problems(installed: &Installed) {
    for p in &installed.problems {
        eprintln!("warning: {p}");
    }
}

fn place(p: &Available) -> String {
    match &p.origin {
        Origin::Builtin => "built in".into(),
        Origin::External(e) => match &e.found.source {
            Source::Dir(d) => d.display().to_string(),
            Source::Path => e.found.exec.display().to_string(),
        },
    }
}

pub fn ls(installed: &Installed) {
    warn_problems(installed);
    let env_file = dotenv();
    for p in &installed.plugins {
        match &p.description {
            Err(msg) => println!("{}  broken: {msg}\n  {}", p.name, place(p)),
            Ok(d) => {
                println!("{} {}  {}", d.name, d.version, place(p));
                for m in &d.models {
                    println!("  {:<18} {}", m.op, m.id);
                }
                print_env(&d.env, &env_file);
            }
        }
    }
    if !installed
        .plugins
        .iter()
        .any(|p| matches!(p.origin, Origin::External(_)))
    {
        println!(
            "\nno other plugins in {} or on PATH",
            user_dir().join("plugins").display()
        );
    }
}

fn print_env(declared: &[String], env_file: &Path) {
    let (_, missing) = discovery::resolve_env(declared, env_file);
    for v in declared {
        let state = if missing.contains(v) {
            "missing"
        } else {
            "set"
        };
        println!("  env {v} ({state})");
    }
}

/// Describe, env and offline conformance checks. Only calls `quote`, which plugins must keep free.
/// What's left on each provider account, for plugins that can tell. Reading it is free.
pub fn balance(installed: &Installed, only: Option<&str>) -> Result<()> {
    if let Some(n) = only
        && !installed.plugins.iter().any(|p| p.name == n)
    {
        bail!("no plugin called {n}; `shotline plugin ls` shows what's installed");
    }
    let rt = runtime();
    for p in &installed.plugins {
        if only.is_some_and(|n| n != p.name) || p.description.is_err() {
            continue;
        }
        let plugin = open(installed, &p.name)?;
        match rt.block_on(plugin.balance()) {
            Ok(b) => {
                let left: Vec<String> = b
                    .balances
                    .iter()
                    .map(|a| format!("{} {}", shotline_core::ledger::money(a.amount), a.unit))
                    .collect();
                println!("{}  {}", p.name, left.join(", "));
            }
            Err(shotline_plugin::Error::Unsupported(_)) => {
                println!("{}  can't tell", p.name)
            }
            Err(e) => println!("{}  {e}", p.name),
        }
    }
    Ok(())
}

pub fn doctor(installed: &Installed, name: &str) -> Result<()> {
    warn_problems(installed);
    let Some(p) = installed.plugins.iter().find(|p| p.name == name) else {
        bail!("no plugin called {name}; `shotline plugin ls` shows what's installed");
    };
    let desc = match &p.description {
        Ok(d) => d,
        Err(msg) => bail!("{name} doesn't describe itself: {msg}"),
    };
    println!("{} {}, protocol {}", desc.name, desc.version, desc.protocol);

    let env_file = dotenv();
    let (_, missing) = discovery::resolve_env(&desc.env, &env_file);
    for v in &missing {
        println!(
            "missing env {v} (set it or put it in {})",
            env_file.display()
        );
    }

    // The runtime must outlive the plugin: on Windows, shutting it down waits on the thread
    // reading the plugin's stderr, which only ends once the plugin is dropped and exits.
    let rt = runtime();
    let plugin = open(installed, name)?;
    let work = std::env::temp_dir().join(format!("shotline-doctor-{name}-{}", std::process::id()));
    let checks = rt.block_on(conformance::check(
        plugin.as_ref(),
        &Options::offline(&work),
    ));
    let _ = std::fs::remove_dir_all(&work);
    for c in &checks {
        match &c.problem {
            None => println!("  ok    {}", c.name),
            Some(p) => println!("  FAIL  {}: {p}", c.name),
        }
    }
    if !conformance::passed(&checks) || !missing.is_empty() {
        bail!("{name} has problems");
    }
    Ok(())
}
