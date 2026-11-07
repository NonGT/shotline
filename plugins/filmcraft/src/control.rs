//! Talking to a FilmCraft the plugin started: its control port takes one JSON request a line,
//! each with a token only the plugin and that FilmCraft know. The port and token are kept in
//! the film's cache (out of git) so later calls reach the same FilmCraft.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How long FilmCraft gets to start and answer.
const START: Duration = Duration::from_secs(40);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub port: u16,
    pub token: String,
}

pub fn session_file(film: &Path) -> PathBuf {
    film.join(".shotline/cache/filmcraft-session.json")
}

impl Session {
    pub fn load(film: &Path) -> Option<Session> {
        serde_json::from_str(&std::fs::read_to_string(session_file(film)).ok()?).ok()
    }

    fn save(&self, film: &Path) -> std::io::Result<()> {
        let path = session_file(film);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        o.open(&path)?
            .write_all(serde_json::to_string(self).unwrap_or_default().as_bytes())
    }

    /// One request, its result or FilmCraft's error.
    pub fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(500))
            .map_err(|e| format!("FilmCraft isn't answering: {e}"))?;
        // FilmCraft waits up to a minute for its UI to answer.
        s.set_read_timeout(Some(Duration::from_secs(70))).ok();
        let line = json!({"id": 1, "token": self.token, "method": method, "params": params});
        s.write_all(format!("{line}\n").as_bytes())
            .map_err(|e| e.to_string())?;
        let mut reply = String::new();
        BufReader::new(s)
            .read_line(&mut reply)
            .map_err(|e| format!("FilmCraft didn't answer {method}: {e}"))?;
        let v: Value = serde_json::from_str(&reply)
            .map_err(|_| format!("FilmCraft answered {method} with {}", reply.trim()))?;
        if v["ok"] == true {
            Ok(v["result"].clone())
        } else {
            Err(v["error"].as_str().unwrap_or("failed").to_string())
        }
    }

    /// An engine command, as in FilmCraft's menus.
    pub fn exec(&self, command: &str, params: Value) -> Result<Value, String> {
        self.call(
            "engine.execute",
            json!({"command": command, "params": params}),
        )
        .map_err(|e| format!("{command}: {e}"))
    }

    pub fn alive(&self) -> bool {
        self.call("perf.stats", json!({})).is_ok()
    }
}

fn token() -> Result<String, String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| format!("no random numbers for a token: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn free_port() -> Result<u16, String> {
    let l = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| e.to_string())?;
    Ok(l.local_addr().map_err(|e| e.to_string())?.port())
}

/// The FilmCraft program: FILMCRAFT if set, the app on macOS, else `filmcraft` on PATH.
pub fn program() -> PathBuf {
    if let Some(p) = std::env::var_os("FILMCRAFT") {
        return p.into();
    }
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let apps = [
            Some(PathBuf::from("/Applications")),
            home.map(|h| h.join("Applications")),
        ];
        for a in apps.into_iter().flatten() {
            let p = a.join("FilmCraft.app/Contents/MacOS/FilmCraft");
            if p.is_file() {
                return p;
            }
        }
    }
    "filmcraft".into()
}

/// Starts FilmCraft listening on a new port with a new token, and waits until it answers.
pub fn start(film: &Path, args: &[OsString]) -> Result<Session, String> {
    let s = Session {
        port: free_port()?,
        token: token()?,
    };
    let exe = program();
    let mut c = std::process::Command::new(&exe);
    c.env("FILMCRAFT_CONTROL_TOKEN", &s.token)
        .args(["--control", &s.port.to_string()])
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Its own process group, so it stays open when the plugin is done.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut c, 0);
    let mut child = c.spawn().map_err(|e| {
        format!(
            "can't start FilmCraft ({}): {e}; install it, or set FILMCRAFT to its program",
            exe.display()
        )
    })?;
    s.save(film)
        .map_err(|e| format!("can't keep FilmCraft's port: {e}"))?;
    let until = Instant::now() + START;
    while Instant::now() < until {
        if s.alive() {
            return Ok(s);
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("FilmCraft closed as it started ({status})"));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err("FilmCraft started but didn't answer on its control port".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_new_each_time() {
        let (a, b) = (token().unwrap(), token().unwrap());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
    }

    #[test]
    fn the_session_is_kept_for_the_film() {
        let film = tempfile::tempdir().unwrap();
        assert!(Session::load(film.path()).is_none());
        let s = Session {
            port: 4242,
            token: "ab".into(),
        };
        s.save(film.path()).unwrap();
        assert_eq!(Session::load(film.path()), Some(s));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(session_file(film.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "only the owner reads the token");
        }
    }

    #[test]
    fn nobody_on_the_port_is_not_alive() {
        let s = Session {
            port: free_port().unwrap(),
            token: "x".into(),
        };
        assert!(!s.alive());
    }
}
