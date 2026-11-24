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

pub fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(io(path))
}

/// Writes through a file next to `path` and renames it over, so the app's watcher or another
/// process reading at the same moment never sees it half written. The file keeps its mode.
pub fn write(path: &Path, contents: &str) -> Result<()> {
    write_with_mode(path, contents, None)
}

/// [`write`] for a file only its owner may read, such as .env with keys in it.
pub fn write_private(path: &Path, contents: &str) -> Result<()> {
    write_with_mode(path, contents, Some(0o600))
}

fn write_with_mode(path: &Path, contents: &str, mode: Option<u32>) -> Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        mkdir(parent)?;
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = path.with_file_name(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let mut open = fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let kept = fs::metadata(path)
            .ok()
            .map(|m| m.permissions().mode() & 0o777);
        if let Some(m) = mode.or(kept) {
            open.mode(m);
        }
    }
    #[cfg(not(unix))]
    let _ = mode;
    let done = open
        .open(&tmp)
        .and_then(|mut f| f.write_all(contents.as_bytes()))
        .map_err(io(&tmp))
        .and_then(|()| fs::rename(&tmp, path).map_err(io(path)));
    if done.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    done
}

pub fn mkdir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(io(path))
}

pub fn remove(path: &Path) -> Result<()> {
    fs::remove_file(path).map_err(io(path))
}

pub fn remove_dir(path: &Path) -> Result<()> {
    fs::remove_dir_all(path).map_err(io(path))
}

pub fn rename(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        mkdir(parent)?;
    }
    fs::rename(from, to).map_err(io(from))
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

#[cfg(test)]
mod write_tests {
    use super::*;

    #[test]
    fn writes_whole_and_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a/shot.toml");
        write(&p, "one").unwrap();
        write(&p, "two").unwrap();
        assert_eq!(read(&p).unwrap(), "two");
        assert_eq!(list(&tmp.path().join("a")).unwrap(), vec![p.clone()]);
    }

    #[cfg(unix)]
    #[test]
    fn keeps_the_mode() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join(".env");
        write_private(&p, "A=1\n").unwrap();
        write(&p, "A=2\n").unwrap();
        let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
