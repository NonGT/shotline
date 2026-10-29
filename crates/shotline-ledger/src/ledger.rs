use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::{Entry, Error, Result};

/// A JSONL file of entries, each holding the hash of the line before it, plus a `.head` file with
/// the last seq and hash so a cut-off tail is noticed too.
#[derive(Debug, Clone)]
pub struct Ledger {
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Problem {
    /// 1-based line number, 0 for the file as a whole.
    pub line: usize,
    pub message: String,
}

fn hash(line: &str) -> String {
    Sha256::digest(line.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

impl Ledger {
    pub fn new(path: impl Into<PathBuf>) -> Ledger {
        Ledger { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn sibling(&self, ext: &str) -> PathBuf {
        self.path.with_extension(ext)
    }

    fn lines(&self) -> Result<Vec<String>> {
        match std::fs::read_to_string(&self.path) {
            Ok(t) => Ok(t.lines().map(str::to_string).collect()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
            Err(e) => Err(io(&self.path)(e)),
        }
    }

    pub fn entries(&self) -> Result<Vec<Entry>> {
        self.lines()?
            .iter()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(i, l)| {
                serde_json::from_str(l).map_err(|e| Error::Corrupt {
                    line: i + 1,
                    message: e.to_string(),
                })
            })
            .collect()
    }

    /// Everything that doesn't add up: edited, removed, reordered or cut-off lines.
    pub fn verify(&self) -> Result<Vec<Problem>> {
        let lines = self.lines()?;
        let mut problems = vec![];
        let mut prev_hash = String::new();
        for (i, line) in lines.iter().enumerate() {
            let n = i + 1;
            match serde_json::from_str::<Entry>(line) {
                Err(e) => problems.push(Problem {
                    line: n,
                    message: format!("not a ledger entry: {e}"),
                }),
                Ok(e) => {
                    if e.seq != n as u64 {
                        problems.push(Problem {
                            line: n,
                            message: format!("seq is {}, expected {n}", e.seq),
                        });
                    }
                    if e.prev != prev_hash {
                        problems.push(Problem {
                            line: n,
                            message: "doesn't follow the line before it (edited or removed)".into(),
                        });
                    }
                }
            }
            prev_hash = hash(line);
        }
        let head = std::fs::read_to_string(self.sibling("head")).ok();
        let expected = (!lines.is_empty()).then(|| format!("{} {prev_hash}", lines.len()));
        match (head.as_deref().map(str::trim), expected.as_deref()) {
            (None, None) => {}
            (Some(h), Some(e)) if h == e => {}
            (None, Some(_)) => problems.push(Problem {
                line: 0,
                message: "head file is missing".into(),
            }),
            (Some(h), _) => problems.push(Problem {
                line: 0,
                message: format!(
                    "head says {h:?} but the file ends at {:?} (lines cut off or added)",
                    expected.unwrap_or_default()
                ),
            }),
        }
        Ok(problems)
    }

    /// Runs `f` on the current entries while holding the lock, then appends whatever it returns.
    /// `seq` and `prev` of the new entries are filled in here.
    pub fn transact<T>(
        &self,
        f: impl FnOnce(&[Entry]) -> Result<(Vec<Entry>, T)>,
    ) -> Result<(Vec<Entry>, T)> {
        let _lock = self.lock()?;
        let lines = self.lines()?;
        let entries = self.entries()?;
        let (mut new, out) = f(&entries)?;
        if new.is_empty() {
            return Ok((new, out));
        }
        let mut prev = lines.last().map(|l| hash(l)).unwrap_or_default();
        let mut seq = lines.len() as u64;
        let mut text = String::new();
        for e in &mut new {
            seq += 1;
            e.seq = seq;
            e.prev = prev;
            let line = serde_json::to_string(e).expect("entries serialize");
            prev = hash(&line);
            text.push_str(&line);
            text.push('\n');
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(io(dir))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(io(&self.path))?;
        file.write_all(text.as_bytes()).map_err(io(&self.path))?;
        file.sync_data().map_err(io(&self.path))?;
        let head = self.sibling("head");
        std::fs::write(&head, format!("{seq} {prev}\n")).map_err(io(&head))?;
        Ok((new, out))
    }

    fn lock(&self) -> Result<File> {
        let path = self.sibling("lock");
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io(dir))?;
        }
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io(&path))?;
        f.lock().map_err(io(&path))?;
        Ok(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Kind;

    pub fn entry(kind: Kind, amount: f64) -> Entry {
        Entry {
            seq: 0,
            time: "2026-10-30T10:00:00Z".parse().unwrap(),
            kind,
            project: None,
            job: Some("j1".into()),
            take: None,
            shot: None,
            chapter: None,
            op: None,
            plugin: None,
            model: None,
            plugin_job: None,
            units: None,
            unit_name: None,
            amount,
            unit: "USD".into(),
            base: amount,
            base_currency: "USD".into(),
            rate: 1.0,
            rate_source: "same".into(),
            note: None,
            prev: String::new(),
        }
    }

    fn ledger_with(n: usize) -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let l = Ledger::new(dir.path().join(".shotline/ledger.jsonl"));
        for i in 0..n {
            l.transact(|_| Ok((vec![entry(Kind::Adjust, i as f64)], ())))
                .unwrap();
        }
        (dir, l)
    }

    fn rewrite(l: &Ledger, f: impl FnOnce(&mut Vec<String>)) {
        let mut lines = l.lines().unwrap();
        f(&mut lines);
        std::fs::write(l.path(), lines.join("\n") + "\n").unwrap();
    }

    #[test]
    fn chain_is_filled_in() {
        let (_d, l) = ledger_with(3);
        let es = l.entries().unwrap();
        assert_eq!(es.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(es[0].prev, "");
        assert_eq!(es[1].prev, hash(&l.lines().unwrap()[0]));
        assert!(l.verify().unwrap().is_empty());
    }

    #[test]
    fn empty_ledger_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let l = Ledger::new(dir.path().join("l.jsonl"));
        assert!(l.entries().unwrap().is_empty());
        assert!(l.verify().unwrap().is_empty());
    }

    #[test]
    fn catches_an_edited_amount() {
        let (_d, l) = ledger_with(3);
        rewrite(&l, |ls| {
            ls[1] = ls[1].replace("\"amount\":1.0", "\"amount\":0.1")
        });
        let p = l.verify().unwrap();
        assert_eq!(p.len(), 1, "{p:?}");
        assert_eq!(p[0].line, 3, "the line after the edit no longer follows");
    }

    #[test]
    fn catches_a_removed_line() {
        let (_d, l) = ledger_with(3);
        rewrite(&l, |ls| {
            ls.remove(1);
        });
        let p = l.verify().unwrap();
        assert!(
            p.iter()
                .any(|p| p.line == 2 && p.message.contains("seq is 3"))
        );
        assert!(p.iter().any(|p| p.line == 0), "head disagrees too");
    }

    #[test]
    fn catches_a_cut_off_tail() {
        let (_d, l) = ledger_with(3);
        rewrite(&l, |ls| {
            ls.pop();
        });
        let p = l.verify().unwrap();
        assert_eq!(p.len(), 1);
        assert!(p[0].message.contains("cut off"));
    }

    #[test]
    fn catches_swapped_lines() {
        let (_d, l) = ledger_with(3);
        rewrite(&l, |ls| ls.swap(0, 1));
        assert!(l.verify().unwrap().len() >= 2);
    }

    #[test]
    fn concurrent_appends_stay_ordered() {
        let (_d, l) = ledger_with(0);
        std::thread::scope(|s| {
            for _ in 0..8 {
                let l = l.clone();
                s.spawn(move || {
                    for _ in 0..10 {
                        l.transact(|_| Ok((vec![entry(Kind::Adjust, 1.0)], ())))
                            .unwrap();
                    }
                });
            }
        });
        assert_eq!(l.entries().unwrap().len(), 80);
        assert!(l.verify().unwrap().is_empty());
    }
}
