use anyhow::{Result, bail};
use shotline_core::{Catalog, Project, user_dir};
use shotline_plugin::conformance::{self, Options};
use shotline_plugin::discovery::{self, Entry, Source};

pub struct Installed {
    pub entries: Vec<Entry>,
    pub problems: Vec<String>,
}

pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

pub fn load() -> Installed {
    let home = user_dir();
    let (found, problems) = discovery::discover(&home, std::env::var_os("PATH").as_deref());
    let entries = runtime().block_on(discovery::describe_all(
        found,
        &home.join("cache").join("describe"),
    ));
    Installed { entries, problems }
}

/// Built-in models, then installed plugins, then the project's models.toml.
pub fn catalog(project: &Project, installed: &Installed) -> Result<Catalog> {
    let mut c = Catalog::builtin();
    for e in &installed.entries {
        if let Ok(d) = &e.description {
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

pub fn ls(installed: &Installed) {
    warn_problems(installed);
    if installed.entries.is_empty() {
        println!(
            "no plugins in {} or on PATH",
            user_dir().join("plugins").display()
        );
        return;
    }
    let dotenv = user_dir().join(".env");
    for e in &installed.entries {
        let place = match &e.found.source {
            Source::Dir(d) => d.display().to_string(),
            Source::Path => e.found.exec.display().to_string(),
        };
        match &e.description {
            Err(msg) => println!("{}  broken: {msg}\n  {place}", e.found.name),
            Ok(d) => {
                println!("{} {}  {place}", d.name, d.version);
                for m in &d.models {
                    println!("  {:<18} {}", m.op, m.id);
                }
                let (_, missing) = discovery::resolve_env(&d.env, &dotenv);
                for v in &d.env {
                    let state = if missing.contains(v) {
                        "missing"
                    } else {
                        "set"
                    };
                    println!("  env {v} ({state})");
                }
            }
        }
    }
}

/// Describe, env and offline conformance checks. Only calls `quote`, which plugins must keep free.
pub fn doctor(installed: &Installed, name: &str) -> Result<()> {
    warn_problems(installed);
    let Some(entry) = installed.entries.iter().find(|e| e.found.name == name) else {
        bail!("no plugin called {name}; `shotline plugin ls` shows what's installed");
    };
    let desc = match &entry.description {
        Ok(d) => d,
        Err(msg) => bail!("{name} doesn't describe itself: {msg}"),
    };
    println!("{} {}, protocol {}", desc.name, desc.version, desc.protocol);

    let dotenv = user_dir().join(".env");
    let (_, missing) = discovery::resolve_env(&desc.env, &dotenv);
    for v in &missing {
        println!("missing env {v} (set it or put it in {})", dotenv.display());
    }

    // The runtime must outlive the plugin: on Windows, shutting it down waits on the thread
    // reading the plugin's stderr, which only ends once the plugin is dropped and exits.
    let rt = runtime();
    let plugin = discovery::open(entry, &dotenv).map_err(anyhow::Error::msg)?;
    let work = std::env::temp_dir().join(format!("shotline-doctor-{name}-{}", std::process::id()));
    let checks = rt.block_on(conformance::check(&plugin, &Options::offline(&work)));
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
