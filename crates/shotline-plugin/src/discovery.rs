//! Finding external plugins and their descriptions.
//!
//! A plugin is either a folder `<home>/plugins/<name>/` with a `shotline-plugin.toml`, or an
//! executable called `shotline-plugin-<name>` on PATH. A folder wins over PATH for the same name,
//! and an earlier PATH entry over a later one; the ones passed over are kept in `skipped`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use shotline_protocol::{Description, PROTOCOL_VERSION};

use crate::{Plugin, ProcessPlugin};

pub const MANIFEST: &str = "shotline-plugin.toml";
const PREFIX: &str = "shotline-plugin-";

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub name: String,
    /// Relative to the plugin folder, or absolute.
    pub exec: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    pub protocol: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    Dir(PathBuf),
    Path,
}

#[derive(Debug, Clone)]
pub struct Found {
    pub name: String,
    pub exec: PathBuf,
    pub args: Vec<String>,
    pub source: Source,
    /// Programs on PATH with the same name that aren't used.
    pub skipped: Vec<PathBuf>,
}

/// Plugins found, plus problems with ones that couldn't be used.
pub fn discover(home: &Path, path_var: Option<&OsStr>) -> (Vec<Found>, Vec<String>) {
    let mut found: Vec<Found> = vec![];
    let mut problems = vec![];

    let mut dirs: Vec<PathBuf> = std::fs::read_dir(home.join("plugins"))
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    dirs.sort();
    for dir in dirs {
        let file = dir.join(MANIFEST);
        if !file.is_file() {
            continue;
        }
        let m: Manifest = match std::fs::read_to_string(&file)
            .map_err(|e| e.to_string())
            .and_then(|t| toml::from_str(&t).map_err(|e| e.to_string()))
        {
            Ok(m) => m,
            Err(e) => {
                problems.push(format!("{}: {e}", file.display()));
                continue;
            }
        };
        if m.protocol != PROTOCOL_VERSION {
            problems.push(format!(
                "{}: speaks protocol {}, shotline speaks {PROTOCOL_VERSION}",
                m.name, m.protocol
            ));
            continue;
        }
        found.push(Found {
            exec: dir.join(&m.exec),
            name: m.name,
            args: m.args,
            source: Source::Dir(dir),
            skipped: vec![],
        });
    }

    if let Some(path_var) = path_var {
        for dir in std::env::split_paths(path_var) {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
            entries.sort();
            for exec in entries {
                let Some(name) = plugin_name(&exec) else {
                    continue;
                };
                if let Some(f) = found.iter_mut().find(|f| f.name == name) {
                    // The same file twice on PATH isn't another copy.
                    if f.exec != exec && !f.skipped.contains(&exec) {
                        f.skipped.push(exec);
                    }
                    continue;
                }
                found.push(Found {
                    name,
                    exec,
                    args: vec![],
                    source: Source::Path,
                    skipped: vec![],
                });
            }
        }
    }
    (found, problems)
}

/// A file that can be run; on Windows that's the .exe the name check asks for. Leaves out what
/// sits next to programs, like cargo's `.d` files.
fn runnable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    path.is_file()
}

/// Makes `<home>/plugins/<name>/` point at `exec`, so that one is used whatever PATH says.
/// Only the manifest is written; the program stays where it is.
pub fn pin(home: &Path, name: &str, exec: &Path) -> Result<PathBuf, String> {
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return Err(format!("{name:?} isn't a plugin name"));
    }
    let exec = std::path::absolute(exec).map_err(|e| e.to_string())?;
    if !exec.is_file() {
        return Err(format!("{} isn't a file", exec.display()));
    }
    let dir = home.join("plugins").join(name);
    if dir.exists() && !pinned(&dir) {
        return Err(format!(
            "{} is a plugin folder of its own; take it away first",
            dir.display()
        ));
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut m = toml::Table::new();
    m.insert("name".into(), name.into());
    m.insert("exec".into(), exec.display().to_string().into());
    m.insert("protocol".into(), i64::from(PROTOCOL_VERSION).into());
    let text = toml::to_string(&m).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(MANIFEST), text).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Takes away a folder `pin` made; PATH decides again.
pub fn unpin(home: &Path, name: &str) -> Result<(), String> {
    let dir = home.join("plugins").join(name);
    if !pinned(&dir) {
        return Err(format!("{name} isn't pinned"));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())
}

/// A folder holding only a manifest that points elsewhere, as `pin` makes them.
pub fn pinned(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    let names: Vec<_> = rd.filter_map(|e| e.ok().map(|e| e.file_name())).collect();
    if names.len() != 1 || names[0] != MANIFEST {
        return false;
    }
    std::fs::read_to_string(dir.join(MANIFEST))
        .ok()
        .and_then(|t| toml::from_str::<Manifest>(&t).ok())
        .is_some_and(|m| m.exec.is_absolute())
}

fn plugin_name(path: &Path) -> Option<String> {
    if !runnable(path) {
        return None;
    }
    let file = path.file_name()?.to_str()?;
    let file = if cfg!(windows) {
        file.strip_suffix(".exe")?
    } else {
        file
    };
    let name = file.strip_prefix(PREFIX)?;
    (!name.is_empty()).then(|| name.to_string())
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub found: Found,
    pub description: Result<Description, String>,
}

#[derive(Serialize, Deserialize)]
struct Cached {
    key: String,
    description: Description,
}

fn cache_key(f: &Found) -> Option<String> {
    let meta = std::fs::metadata(&f.exec).ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    Some(format!(
        "{}|{}|{}|{}",
        f.exec.display(),
        f.args.join(" "),
        meta.len(),
        mtime.as_nanos()
    ))
}

/// Asks each plugin to describe itself, reusing answers cached while the executable is unchanged.
/// Plugins get no secrets for this.
pub async fn describe_all(found: Vec<Found>, cache_dir: &Path) -> Vec<Entry> {
    let mut out = vec![];
    for f in found {
        let cache_file = cache_dir.join(format!("{}.json", f.name));
        let key = cache_key(&f);
        let cached = std::fs::read_to_string(&cache_file)
            .ok()
            .and_then(|t| serde_json::from_str::<Cached>(&t).ok())
            .filter(|c| Some(&c.key) == key.as_ref());
        let description = match cached {
            Some(c) => Ok(c.description),
            None => {
                let p = ProcessPlugin::new(&f.name, &f.exec, f.args.clone(), vec![]);
                match p.describe().await {
                    Ok(d) if d.protocol != PROTOCOL_VERSION => Err(format!(
                        "speaks protocol {}, shotline speaks {PROTOCOL_VERSION}",
                        d.protocol
                    )),
                    Ok(d) => {
                        if let Some(key) = key {
                            let c = Cached {
                                key,
                                description: d.clone(),
                            };
                            let _ = std::fs::create_dir_all(cache_dir);
                            let _ = std::fs::write(&cache_file, serde_json::to_string(&c).unwrap());
                        }
                        Ok(d)
                    }
                    Err(e) => Err(e.to_string()),
                }
            }
        };
        out.push(Entry {
            found: f,
            description,
        });
    }
    out
}

/// KEY=VALUE lines; `#` comments and surrounding quotes are dropped.
pub fn read_dotenv(path: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let line = line.strip_prefix("export ").unwrap_or(line);
            if line.starts_with('#') {
                return None;
            }
            let (k, v) = line.split_once('=')?;
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(v);
            Some((k.trim().to_string(), v.to_string()))
        })
        .collect()
}

/// Values for the variables a plugin declared, from the environment first, then the .env file.
/// Returns the values and the names that weren't found.
pub fn resolve_env(declared: &[String], dotenv: &Path) -> (Vec<(String, String)>, Vec<String>) {
    let file = read_dotenv(dotenv);
    let mut values = vec![];
    let mut missing = vec![];
    for name in declared {
        let v = std::env::var(name)
            .ok()
            .or_else(|| file.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()));
        match v {
            Some(v) => values.push((name.clone(), v)),
            None => missing.push(name.clone()),
        }
    }
    (values, missing)
}

/// A plugin ready to do work, with only its declared variables passed in.
pub fn open(entry: &Entry, dotenv: &Path) -> Result<ProcessPlugin, String> {
    let desc = entry.description.as_ref().map_err(|e| e.clone())?;
    let (env, _) = resolve_env(&desc.env, dotenv);
    Ok(ProcessPlugin::new(
        &entry.found.name,
        &entry.found.exec,
        entry.found.args.clone(),
        env,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty file that can be run.
    fn program(p: &Path) {
        std::fs::write(p, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn dotenv() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".env");
        std::fs::write(&p, "# keys\nA=1\nexport B = \"two\"\nC='3'\nnot a line\n").unwrap();
        assert_eq!(
            read_dotenv(&p),
            [("A", "1"), ("B", "two"), ("C", "3")].map(|(k, v)| (k.to_string(), v.to_string()))
        );
    }

    #[test]
    fn manifests_and_path() {
        let home = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let pdir = home.path().join("plugins/venice");
        std::fs::create_dir_all(&pdir).unwrap();
        std::fs::write(
            pdir.join(MANIFEST),
            "name = \"venice\"\nexec = \"run.sh\"\nprotocol = 1\n",
        )
        .unwrap();
        let old = home.path().join("plugins/old");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(
            old.join(MANIFEST),
            "name = \"old\"\nexec = \"x\"\nprotocol = 99\n",
        )
        .unwrap();
        let exe = if cfg!(windows) { ".exe" } else { "" };
        for n in ["venice", "eleven"] {
            program(&bin.path().join(format!("shotline-plugin-{n}{exe}")));
        }
        // What cargo leaves next to a program isn't one.
        std::fs::write(bin.path().join("shotline-plugin-venice.d"), "").unwrap();
        std::fs::write(bin.path().join("unrelated"), "").unwrap();

        let (found, problems) = discover(home.path(), Some(bin.path().as_os_str()));
        let names: Vec<_> = found.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["venice", "eleven"]);
        assert_eq!(found[0].source, Source::Dir(pdir.clone()));
        assert_eq!(found[0].exec, pdir.join("run.sh"));
        assert_eq!(found[1].source, Source::Path);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("protocol 99"));
        // The folder wins; the PATH copy is passed over and said so.
        let on_path = bin.path().join(format!("shotline-plugin-venice{exe}"));
        assert_eq!(found[0].skipped, [on_path]);
    }

    #[test]
    fn earlier_on_path_wins_and_pinning_overrides_it() {
        let home = tempfile::tempdir().unwrap();
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let exe = if cfg!(windows) { ".exe" } else { "" };
        let file = format!("shotline-plugin-venice{exe}");
        for d in [&a, &b] {
            program(&d.path().join(&file));
        }
        let path = std::env::join_paths([a.path(), b.path(), a.path()]).unwrap();
        let (found, _) = discover(home.path(), Some(&path));
        assert_eq!(found[0].exec, a.path().join(&file));
        assert_eq!(found[0].skipped, [b.path().join(&file)]);

        let dir = pin(home.path(), "venice", &b.path().join(&file)).unwrap();
        assert!(pinned(&dir));
        let (found, _) = discover(home.path(), Some(&path));
        assert_eq!(found[0].source, Source::Dir(dir.clone()));
        assert_eq!(found[0].exec, b.path().join(&file));

        unpin(home.path(), "venice").unwrap();
        assert!(!dir.exists());
        assert_eq!(
            discover(home.path(), Some(&path)).0[0].exec,
            a.path().join(&file)
        );

        // A folder someone made by hand is theirs.
        let own = home.path().join("plugins/mine");
        std::fs::create_dir_all(&own).unwrap();
        std::fs::write(
            own.join(MANIFEST),
            "name = \"mine\"\nexec = \"run.sh\"\nprotocol = 1\n",
        )
        .unwrap();
        assert!(pin(home.path(), "mine", &a.path().join(&file)).is_err());
        assert!(unpin(home.path(), "mine").is_err());
        assert!(own.join(MANIFEST).is_file());
    }
}
