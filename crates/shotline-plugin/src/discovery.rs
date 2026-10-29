//! Finding external plugins and their descriptions.
//!
//! A plugin is either a folder `<home>/plugins/<name>/` with a `shotline-plugin.toml`, or an
//! executable called `shotline-plugin-<name>` on PATH. A folder wins over PATH for the same name.

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
                if found.iter().any(|f| f.name == name) {
                    continue;
                }
                found.push(Found {
                    name,
                    exec,
                    args: vec![],
                    source: Source::Path,
                });
            }
        }
    }
    (found, problems)
}

fn plugin_name(path: &Path) -> Option<String> {
    if !path.is_file() {
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
            std::fs::write(bin.path().join(format!("shotline-plugin-{n}{exe}")), "").unwrap();
        }
        std::fs::write(bin.path().join("unrelated"), "").unwrap();

        let (found, problems) = discover(home.path(), Some(bin.path().as_os_str()));
        let names: Vec<_> = found.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["venice", "eleven"]);
        assert_eq!(found[0].source, Source::Dir(pdir.clone()));
        assert_eq!(found[0].exec, pdir.join("run.sh"));
        assert_eq!(found[1].source, Source::Path);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("protocol 99"));
    }
}
