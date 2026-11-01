//! The user's `.env`: API keys as `KEY=value` lines. The CLI and plugins read it; the app writes
//! it. Writing changes only the key's own line, so comments and other keys stay as they were.

use std::path::{Path, PathBuf};

use crate::{Result, fsx};

pub fn path(user_dir: &Path) -> PathBuf {
    user_dir.join(".env")
}

fn parse(line: &str) -> Option<(String, String)> {
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
}

pub fn read(path: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(path)
        .map(|t| t.lines().filter_map(parse).collect())
        .unwrap_or_default()
}

/// Sets `key`, or takes it out with `None`. Only the owner can read the file afterwards.
pub fn set(path: &Path, key: &str, value: Option<&str>) -> Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let line = value.map(|v| {
        if v.contains(|c: char| c.is_whitespace() || c == '#') {
            format!("{key}=\"{v}\"")
        } else {
            format!("{key}={v}")
        }
    });
    let mut lines = vec![];
    let mut done = false;
    for l in text.lines() {
        if parse(l).is_some_and(|(k, _)| k == key) {
            if !done {
                lines.extend(line.clone());
            }
            done = true;
        } else {
            lines.push(l.to_string());
        }
    }
    if !done {
        lines.extend(line);
    }
    let mut out = lines.join("\n");
    out.push('\n');
    fsx::write(path, &out)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_one_line_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".env");
        std::fs::write(&p, "# keys\nexport A=1\nB='two'\n").unwrap();
        set(&p, "A", Some("new")).unwrap();
        set(&p, "C", Some("has space")).unwrap();
        set(&p, "B", None).unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "# keys\nA=new\nC=\"has space\"\n"
        );
        assert_eq!(
            read(&p),
            [("A", "new"), ("C", "has space")].map(|(k, v)| (k.to_string(), v.to_string()))
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
