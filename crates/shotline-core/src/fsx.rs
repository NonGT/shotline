use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Error, Result};

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

pub fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(io(path))
}

pub fn write(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        mkdir(parent)?;
    }
    fs::write(path, contents).map_err(io(path))
}

pub fn mkdir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(io(path))
}

pub fn copy(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        mkdir(parent)?;
    }
    fs::copy(from, to).map(|_| ()).map_err(io(from))
}

pub fn read_toml<T: DeserializeOwned>(path: &Path) -> Result<T> {
    toml::from_str(&read(path)?).map_err(|source| Error::Toml {
        path: path.to_path_buf(),
        source,
    })
}

pub fn write_toml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = toml::to_string_pretty(value).expect("project types always serialize");
    write(path, &text)
}

/// Entries of a folder sorted by name. A missing folder is just empty.
pub fn list(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(io(dir)(e)),
    };
    let mut paths = entries
        .map(|e| e.map(|e| e.path()).map_err(io(dir)))
        .collect::<Result<Vec<_>>>()?;
    paths.sort();
    Ok(paths)
}
