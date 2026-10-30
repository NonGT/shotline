//! Talks to `shotline mcp` over stdio as a client would.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use assert_fs::TempDir;
use assert_fs::prelude::*;
use serde_json::{Value, json};

struct Mcp {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(tmp: &TempDir, dir: &Path) -> Mcp {
        let mut child = Command::new(assert_cmd::cargo::cargo_bin("shotline"))
            .arg("mcp")
            .current_dir(dir)
            .env("SHOTLINE_HOME", tmp.path().join(".home"))
            .env("SHOTLINE_POLL_MS", "1")
            .env("PATH", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp {
            child,
            input,
            output,
            next: 1,
        };
        let init = m.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"}
            }),
        );
        assert_eq!(init["serverInfo"]["name"], "shotline");
        assert!(
            init["instructions"]
                .as_str()
                .unwrap()
                .contains("confirm: true")
        );
        m.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        m
    }

    fn send(&mut self, v: Value) {
        writeln!(self.input, "{v}").unwrap();
        self.input.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            assert!(self.output.read_line(&mut line).unwrap() > 0, "server quit");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == id {
                assert!(v["error"].is_null(), "{v}");
                return v["result"].clone();
            }
        }
    }

    /// Calls a tool; returns whether it failed and its text.
    fn call(&mut self, name: &str, args: Value) -> (bool, String) {
        let r = self.request("tools/call", json!({"name": name, "arguments": args}));
        let text = r["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["text"].as_str().unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        (r["isError"] == true, text)
    }

    fn ok(&mut self, name: &str, args: Value) -> String {
        let (failed, text) = self.call(name, args);
        assert!(!failed, "{name}: {text}");
        text
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fake_plugin(tmp: &TempDir) {
    tmp.child(".home/plugins/fake/shotline-plugin.toml")
        .write_str(&format!(
            "name = \"fake\"\nexec = {:?}\nargs = [\"__fake-plugin\"]\nprotocol = 1\n",
            assert_cmd::cargo::cargo_bin("shotline")
                .display()
                .to_string()
        ))
        .unwrap();
}

fn ledger(tmp: &TempDir) -> Vec<String> {
    std::fs::read_to_string(tmp.path().join("film/.shotline/ledger.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| {
            serde_json::from_str::<Value>(l).unwrap()["type"]
                .as_str()
                .unwrap()
                .into()
        })
        .collect()
}

#[test]
fn lists_the_tools() {
    let tmp = TempDir::new().unwrap();
    let mut m = Mcp::start(&tmp, tmp.path());
    let tools = m.request("tools/list", json!({}));
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for want in [
        "init",
        "open_project",
        "story",
        "element_add",
        "shot_add",
        "shot_set",
        "prompt",
        "quote",
        "take",
        "run",
        "takes",
        "select",
        "timeline_render",
        "cost",
        "plugin_ls",
    ] {
        assert!(names.contains(&want), "{want} missing from {names:?}");
    }
    let take = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "take")
        .unwrap();
    assert_eq!(
        take["inputSchema"]["properties"]["confirm"]["type"],
        "boolean"
    );

    let (failed, text) = m.call("shot_ls", json!({}));
    assert!(failed);
    assert!(text.contains("no project is open"), "{text}");
}

#[test]
fn plans_and_shoots_a_film() {
    let tmp = TempDir::new().unwrap();
    fake_plugin(&tmp);
    tmp.child("p.png").write_binary(b"png").unwrap();
    let mut m = Mcp::start(&tmp, tmp.path());

    m.ok("init", json!({"dir": "film", "name": "Film"}));
    tmp.child("film/shotline.toml")
        .write_str(
            "name = \"Film\"\nplugin = \"fake\"\nmodel = \"fake/video\"\nresolution = \"480P\"\n\
             aspect = \"9:16\"\nduration = 4\n[budget]\nper_take = 0.5\n",
        )
        .unwrap();
    m.ok("story", json!({"text": "A girl walks into the forest."}));
    assert_eq!(m.ok("story", json!({})), "A girl walks into the forest.");
    assert_eq!(
        m.ok("element_add", json!({"kind": "cast", "name": "Paka"})),
        "added Paka @paka (cast)"
    );
    m.ok(
        "element_anchor",
        json!({"handle": "paka", "file": "../p.png"}),
    );
    m.ok("chapter_add", json!({"title": "One"}));
    assert_eq!(
        m.ok("shot_add", json!({"chapter": "1", "prompt": "@paka walks"})),
        "01-010"
    );
    m.ok(
        "shot_add",
        json!({"chapter": "1", "prompt": "@paka runs", "duration": 6}),
    );
    m.ok(
        "shot_set",
        json!({"shot": "01-010", "prompt": "@paka walks slowly"}),
    );
    assert!(
        m.ok("prompt", json!({"shot": "01-010"}))
            .contains("walks slowly")
    );
    assert_eq!(
        m.ok("quote", json!({"target": "01-010"})),
        "01-010  fake fake/video  4s  0.4 USD"
    );

    // No confirm: only the price.
    let text = m.ok("take", json!({"shot": "01-010"}));
    assert!(text.contains("spend 0.4 USD on 01-010?"), "{text}");
    assert!(text.contains("Nothing was spent"), "{text}");
    assert!(ledger(&tmp).is_empty());
    assert_eq!(m.ok("takes", json!({"shot": "01-010"})), "");

    let text = m.ok("take", json!({"shot": "01-010", "confirm": true}));
    assert!(text.contains("01-010#1"), "{text}");
    assert_eq!(ledger(&tmp), ["reserve", "charge"]);
    m.ok("select", json!({"shot": "01-010", "take": "1"}));
    assert!(
        m.ok("takes", json!({"shot": "01-010"}))
            .starts_with("* 01-010#1")
    );

    // 6s is 0.6, over the 0.5 a take: refused even when confirmed, before anything is held.
    let (failed, text) = m.call("take", json!({"shot": "01-020", "confirm": true}));
    assert!(failed);
    assert!(text.contains("over the per-take budget"), "{text}");
    assert_eq!(ledger(&tmp), ["reserve", "charge"]);

    let (failed, text) = m.call("run", json!({"op": "video.upscale", "plugin": "fake", "model": "fake/metered", "inputs": {"video": "nope.mp4"}}));
    assert!(failed, "{text}");
    assert_eq!(ledger(&tmp), ["reserve", "charge"]);
}

#[test]
fn opens_an_existing_film() {
    let tmp = TempDir::new().unwrap();
    let elsewhere = tmp.child("elsewhere");
    elsewhere.create_dir_all().unwrap();
    let mut m = Mcp::start(&tmp, tmp.path());
    m.ok("init", json!({"dir": "a"}));
    m.ok("chapter_add", json!({"title": "In A"}));
    let (failed, text) = m.call("open_project", json!({"dir": elsewhere.path()}));
    assert!(failed, "{text}");
    m.ok("open_project", json!({"dir": tmp.path().join("a")}));
    assert_eq!(m.ok("chapter_ls", json!({})), "01  In A");
}
