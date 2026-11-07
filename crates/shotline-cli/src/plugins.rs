use std::path::Path;

use anyhow::{Result, bail};
use shotline_builtin::fal::{self, Fal};
use shotline_core::protocol::{Description, EditInfo, OpenParams, VoiceInfo};
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
        editor: false,
        app: String::new(),
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
            Source::Dir(d) if discovery::pinned(d) => {
                format!("{} (pinned)", e.found.exec.display())
            }
            Source::Dir(d) => d.display().to_string(),
            Source::Path => e.found.exec.display().to_string(),
        },
    }
}

fn skipped(p: &Available) -> &[std::path::PathBuf] {
    match &p.origin {
        Origin::External(e) => &e.found.skipped,
        Origin::Builtin => &[],
    }
}

/// Makes the plugin always run `path`, or the program it runs now.
pub fn pin(installed: &Installed, name: &str, path: Option<&Path>) -> Result<String> {
    let exec = match path {
        Some(p) => p.to_path_buf(),
        None => match installed
            .plugins
            .iter()
            .find(|p| p.name == name)
            .map(|p| &p.origin)
        {
            Some(Origin::External(e)) => e.found.exec.clone(),
            Some(Origin::Builtin) => bail!("{name} is built in; give the program to use"),
            None => bail!("no plugin called {name}; `shotline plugin ls` shows what's installed"),
        },
    };
    let dir = discovery::pin(&user_dir(), name, &exec).map_err(anyhow::Error::msg)?;
    Ok(format!(
        "{name} now always runs {} ({})",
        exec.display(),
        dir.display()
    ))
}

pub fn unpin(name: &str) -> Result<String> {
    discovery::unpin(&user_dir(), name).map_err(anyhow::Error::msg)?;
    Ok(format!("{name} is found on PATH again"))
}

/// One of a model's options for the whole film, or taken out without a value.
pub fn set_option(
    p: &mut Project,
    plugin: &str,
    model: &str,
    key: &str,
    value: Option<&str>,
) -> Result<String> {
    let v = value.map(|s| {
        serde_json::from_str::<serde_json::Value>(s)
            .ok()
            .and_then(|j| toml::Value::try_from(j).ok())
            .unwrap_or_else(|| toml::Value::String(s.to_string()))
    });
    let said = v.as_ref().map(|v| v.to_string());
    p.set_param(plugin, model, key, v)?;
    Ok(match said {
        Some(v) => format!("{plugin} {model}: {key} = {v}"),
        None => format!("{plugin} {model}: {key} taken out"),
    })
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
                    let opts = options(m);
                    if opts.is_empty() {
                        println!("  {:<18} {}", m.op, m.id);
                    } else {
                        println!("  {:<18} {}  options: {}", m.op, m.id, opts.join(", "));
                    }
                }
                if d.editor {
                    println!(
                        "  {:<18} the film's projects in a video editor",
                        "sequences"
                    );
                }
                print_env(&d.env, &env_file);
            }
        }
        for s in skipped(p) {
            println!("  not used: {}", s.display());
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

/// The option names a model's params schema lists.
fn options(m: &shotline_core::protocol::ModelInfo) -> Vec<String> {
    m.params_schema
        .as_ref()
        .and_then(|s| s.get("properties"))
        .and_then(|p| p.as_object())
        .map(|p| p.keys().cloned().collect())
        .unwrap_or_default()
}

/// The plugins as the app's Preferences shows them.
#[cfg(feature = "ui")]
pub fn infos(installed: &Installed) -> (Vec<shotline_ui::PluginInfo>, Vec<String>) {
    use shotline_ui::{PluginInfo, Runs};
    let env_file = dotenv();
    let list = installed
        .plugins
        .iter()
        .map(|p| {
            let runs = match &p.origin {
                Origin::Builtin => Runs::Builtin,
                Origin::External(e) => match &e.found.source {
                    Source::Dir(d) if discovery::pinned(d) => Runs::Pinned(e.found.exec.clone()),
                    Source::Dir(d) => Runs::Folder(d.clone()),
                    Source::Path => Runs::Path(e.found.exec.clone()),
                },
            };
            let missing = match &p.description {
                Ok(d) => discovery::resolve_env(&d.env, &env_file).1,
                Err(_) => vec![],
            };
            PluginInfo {
                name: p.name.clone(),
                runs,
                skipped: skipped(p).to_vec(),
                description: p.description.clone(),
                missing,
            }
        })
        .collect();
    (list, installed.problems.clone())
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
fn left(b: &shotline_core::protocol::Balance) -> String {
    let left: Vec<String> = b
        .balances
        .iter()
        .map(|a| format!("{} {}", shotline_core::ledger::money(a.amount), a.unit))
        .collect();
    left.join(", ")
}

/// One plugin's balance in a line, which also shows its key works.
pub fn balance_of(installed: &Installed, name: &str) -> Result<String> {
    let rt = runtime();
    let plugin = open(installed, name)?;
    match rt.block_on(plugin.balance()) {
        Ok(b) => Ok(format!("{} left", left(&b))),
        Err(shotline_plugin::Error::Unsupported(_)) => {
            bail!("{name} has no free way to check its key")
        }
        Err(e) => bail!("{e}"),
    }
}

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
            Ok(b) => println!("{}  {}", p.name, left(&b)),
            Err(shotline_plugin::Error::Unsupported(_)) => {
                println!("{}  can't tell", p.name)
            }
            Err(e) => println!("{}  {e}", p.name),
        }
    }
    Ok(())
}

/// The voices a plugin's audio.tts can use.
pub fn voices(installed: &Installed, name: &str) -> Result<Vec<VoiceInfo>> {
    // The plugin goes before its runtime, or Windows waits on it forever.
    let rt = runtime();
    let plugin = open(installed, name)?;
    match rt.block_on(plugin.voices()) {
        Ok(v) => Ok(v.voices),
        Err(shotline_plugin::Error::Unsupported(_)) => bail!("{name} can't list its voices"),
        Err(e) => bail!("{name}: {e}"),
    }
}

/// What the editor plugins say about the film: their projects, sequences and clips. One that
/// fails is skipped with a line in the returned problems.
pub fn sequences(installed: &Installed, film: &Path) -> (Vec<EditInfo>, Vec<String>) {
    let rt = runtime();
    let (mut edits, mut problems) = (vec![], vec![]);
    for p in &installed.plugins {
        if !p.description.as_ref().is_ok_and(|d| d.editor) {
            continue;
        }
        let got = open(installed, &p.name).and_then(|plugin| {
            rt.block_on(plugin.sequences(film))
                .map_err(|e| anyhow::anyhow!("{}: {e}", p.name))
        });
        match got {
            Ok(s) => edits.extend(s.edits.into_iter().map(|mut e| {
                e.plugin = p.name.clone();
                e
            })),
            Err(e) => problems.push(e.to_string()),
        }
    }
    (edits, problems)
}

/// Asks editor plugin `name` to open the film's project, making it if need be.
pub fn open_edit(installed: &Installed, name: &str, ask: &OpenParams) -> Result<()> {
    let plugin = open(installed, name)?;
    match runtime().block_on(plugin.open(ask)) {
        Ok(()) => Ok(()),
        Err(shotline_plugin::Error::Unsupported(_)) => bail!("{name} can't open its projects"),
        Err(e) => bail!("{name}: {e}"),
    }
}

pub fn print_voices(installed: &Installed, name: &str) -> Result<()> {
    for v in voices(installed, name)? {
        match &v.description {
            Some(d) => println!("{}  {}  {d}", v.id, v.name),
            None => println!("{}  {}", v.id, v.name),
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
