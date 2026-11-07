//! Links shotline to FilmCraft (github.com/storytold/filmcraft): the projects saved in the film's
//! `shared/` folder, read only. FilmCraft writes the whole file on save, so anything written into
//! it here would be lost.
//!
//! Only what a cut needs is read: sequences, their tracks, and each clip's file and timing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use shotline_plugin::protocol::{
    ClipInfo, Description, EditInfo, Outputs, PROTOCOL_VERSION, Quote, Request, SequenceInfo,
    Sequences, Status, TrackInfo,
};
use shotline_plugin::{Error, Plugin, Result};

pub const SHARED_DIR: &str = "shared";

/// The newest project schema this was checked against. A newer one is still read; it's only
/// named when the file can't be.
const KNOWN_SCHEMA: u64 = 17;

/// FilmCraft's time unit: every frame and sample rate divides it evenly.
const TICKS_PER_SECOND: f64 = 254_016_000_000.0;

pub struct FilmCraft;

#[async_trait]
impl Plugin for FilmCraft {
    async fn describe(&self) -> Result<Description> {
        Ok(Description {
            name: "filmcraft".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: PROTOCOL_VERSION,
            editor: true,
            env: vec![],
            models: vec![],
        })
    }

    async fn quote(&self, _: &Request) -> Result<Quote> {
        Err(Error::Unsupported("quote".into()))
    }

    async fn run(&self, _: &Request, _: &Path) -> Result<String> {
        Err(Error::Unsupported("run".into()))
    }

    async fn poll(&self, _: &str) -> Result<Status> {
        Err(Error::Unsupported("poll".into()))
    }

    async fn fetch(&self, _: &str, _: &Path) -> Result<Outputs> {
        Err(Error::Unsupported("fetch".into()))
    }

    async fn cancel(&self, _: &str) -> Result<()> {
        Err(Error::Unsupported("cancel".into()))
    }

    async fn sequences(&self, film: &Path) -> Result<Sequences> {
        Ok(Sequences {
            edits: shared_edits(film),
        })
    }
}

/// Every `.fcproj` in the film's `shared/`, by name; one that can't be read says why.
pub fn shared_edits(film: &Path) -> Vec<EditInfo> {
    let Ok(entries) = std::fs::read_dir(film.join(SHARED_DIR)) else {
        return vec![];
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "fcproj"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            read_edit(&p).unwrap_or_else(|error| EditInfo {
                name: p.file_stem().unwrap_or_default().to_string_lossy().into(),
                file: p,
                error: Some(error),
                sequences: vec![],
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct File {
    project: Proj,
}

#[derive(Deserialize)]
struct Proj {
    name: String,
    root: Bin,
    items: BTreeMap<String, Item>,
}

#[derive(Deserialize)]
struct Bin {
    children: Vec<Value>,
}

#[derive(Deserialize)]
struct Item {
    name: String,
    kind: Value,
}

#[derive(Deserialize)]
struct RawSequence {
    settings: Settings,
    video_tracks: Vec<RawTrack>,
    audio_tracks: Vec<RawTrack>,
}

#[derive(Deserialize)]
struct Settings {
    width: u32,
    height: u32,
    frame_rate: Rate,
}

#[derive(Deserialize)]
struct Rate {
    num: i64,
    den: i64,
}

#[derive(Deserialize)]
struct RawTrack {
    name: String,
    items: Vec<RawClip>,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    muted: bool,
}

#[derive(Deserialize)]
struct RawClip {
    item: u64,
    name: String,
    start: i64,
    duration: i64,
    source_in: i64,
    #[serde(default = "one")]
    speed: f64,
    #[serde(default = "yes")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

fn one() -> f64 {
    1.0
}

fn secs(ticks: i64) -> f64 {
    ticks as f64 / TICKS_PER_SECOND
}

/// Reads one `.fcproj`, or says why it can't.
pub fn read_edit(path: &Path) -> std::result::Result<EditInfo, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let file: File = serde_json::from_str(&text).map_err(|e| {
        let newer = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.get("schema_version")?.as_u64())
            .filter(|v| *v > KNOWN_SCHEMA);
        match newer {
            Some(v) => format!(
                "{} is from a newer FilmCraft (project schema v{v}) and can't be read yet: {e}",
                path.display()
            ),
            None => format!(
                "{} isn't a FilmCraft project shotline can read: {e}",
                path.display()
            ),
        }
    })?;
    let p = file.project;
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut order = vec![];
    walk(&p.root.children, &mut order);
    // Ones no bin lists still count, after the others.
    for id in p.items.keys().filter_map(|k| k.parse::<u64>().ok()) {
        if !order.contains(&id) {
            order.push(id);
        }
    }
    let mut sequences = vec![];
    for id in order {
        let Some(item) = p.items.get(&id.to_string()) else {
            continue;
        };
        let Some(raw) = item.kind.get("Sequence") else {
            continue;
        };
        let s: RawSequence = serde_json::from_value(raw.clone()).map_err(|e| {
            format!(
                "{}: sequence {:?} can't be read: {e}",
                path.display(),
                item.name
            )
        })?;
        let track = |t: RawTrack, video: bool| TrackInfo {
            name: t.name,
            enabled: if video { t.enabled } else { !t.muted },
            clips: t
                .items
                .into_iter()
                .map(|c| ClipInfo {
                    file: media_file(&p.items, c.item, dir),
                    name: c.name,
                    start: secs(c.start),
                    length: secs(c.duration),
                    from: secs(c.source_in),
                    speed: c.speed,
                    enabled: c.enabled,
                })
                .collect(),
        };
        let r = &s.settings.frame_rate;
        sequences.push(SequenceInfo {
            name: item.name.clone(),
            width: s.settings.width,
            height: s.settings.height,
            fps: if r.den == 0 {
                0.0
            } else {
                r.num as f64 / r.den as f64
            },
            video: s.video_tracks.into_iter().map(|t| track(t, true)).collect(),
            audio: s
                .audio_tracks
                .into_iter()
                .map(|t| track(t, false))
                .collect(),
        });
    }
    Ok(EditInfo {
        file: path.to_path_buf(),
        name: p.name,
        error: None,
        sequences,
    })
}

/// Item ids in the order the bins list them, depth first.
fn walk(children: &[Value], out: &mut Vec<u64>) {
    for c in children {
        if let Some(id) = c.get("Item").and_then(Value::as_u64) {
            out.push(id);
        } else if let Some(kids) = c
            .get("Bin")
            .and_then(|b| b.get("children"))
            .and_then(Value::as_array)
        {
            walk(kids, out);
        }
    }
}

/// The file an item plays, through subclips; relative paths are from the project's folder.
fn media_file(items: &BTreeMap<String, Item>, id: u64, dir: &Path) -> Option<PathBuf> {
    let mut id = id;
    // A subclip's parent could in theory be another subclip; a few hops is plenty.
    for _ in 0..8 {
        let kind = &items.get(&id.to_string())?.kind;
        if let Some(parent) = kind.get("Subclip").and_then(|s| s.get("parent")) {
            id = parent.as_u64()?;
            continue;
        }
        let path = kind.get("Media")?.get("media")?.get("File")?.get("path")?;
        let path = Path::new(path.as_str()?);
        return Some(if path.is_absolute() {
            path.to_path_buf()
        } else {
            dir.join(path)
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i64 = 254_016_000_000;

    /// Shaped like FilmCraft 0.18 writes it, cut down; the parts not read are left in to show
    /// they're skipped.
    fn fixture(dir: &Path, schema: u64) -> PathBuf {
        let clip = |id: u64, item: u64, name: &str, start: i64, dur: i64, from: i64| {
            serde_json::json!({
                "id": id, "item": item, "name": name, "label": "Iris",
                "start": start, "duration": dur, "source_in": from,
                "speed": 1.0, "enabled": true, "effects": [{"effect": "opacity", "enabled": true, "params": {}}]
            })
        };
        let track = |id: u64, kind: &str, name: &str, items: Vec<Value>| {
            serde_json::json!({
                "id": id, "kind": kind, "name": name, "items": items, "transitions": [],
                "locked": false, "sync_lock": true, "enabled": true, "muted": false, "solo": false,
                "channels": "Stereo", "volume_db": 0.0, "pan": 0.0
            })
        };
        let doc = serde_json::json!({
            "format": "filmcraft.project",
            "schema_version": schema,
            "generator": "FilmCraft 0.18.0",
            "project": {
                "name": "Rain edit",
                "settings": {"renderer": "FilmCraft GPU Acceleration (wgpu)"},
                "root": {"id": 0, "name": "Rain edit", "children": [
                    {"Bin": {"id": 1, "name": "shotline", "children": [{"Item": 5}, {"Item": 6}, {"Item": 7}]}},
                    {"Item": 30},
                    {"Item": 21}
                ]},
                "items": {
                    "5": {"id": 5, "name": "01-010#1", "label": "Iris", "kind": {"Media": {
                        "media": {"File": {"path": "/film/jobs/01A/out.mp4"}}, "offline": false}}},
                    "6": {"id": 6, "name": "music.wav", "label": "Iris", "kind": {"Media": {
                        "media": {"File": {"path": "../music.wav"}}}}},
                    "7": {"id": 7, "name": "bars", "label": "Iris", "kind": {"Media": {
                        "media": {"Generator": {"Demo": "OceanSunset"}}}}},
                    "8": {"id": 8, "name": "01-010#1.Subclip", "label": "Iris", "kind": {"Subclip": {
                        "parent": 5, "range": {"start": 0, "duration": T}}}},
                    "21": {"id": 21, "name": "Main edit", "label": "Forest", "kind": {"Sequence": {
                        "settings": {"width": 720, "height": 1280, "frame_rate": {"num": 24000, "den": 1001},
                            "par": [1, 1], "sample_rate": 48000},
                        "video_tracks": [
                            track(15, "Video", "Video 1", vec![
                                clip(29, 5, "01-010#1", 0, 2 * T, T / 2),
                                clip(31, 8, "01-010#1.Subclip", 2 * T, T, 0),
                            ]),
                            track(16, "Video", "Video 2", vec![clip(32, 7, "bars", T, T / 2, 0)]),
                        ],
                        "audio_tracks": [track(18, "Audio", "Audio 1", vec![clip(30, 6, "music.wav", 0, 3 * T, 0)])],
                        "markers": [], "mark_in": null, "mark_out": null, "start_timecode": 0
                    }}},
                    "30": {"id": 30, "name": "Teaser", "label": "Forest", "kind": {"Sequence": {
                        "settings": {"width": 720, "height": 1280, "frame_rate": {"num": 24, "den": 1}},
                        "video_tracks": [], "audio_tracks": []
                    }}}
                },
                "next_id": 33
            },
            "view": {"open_sequences": [21], "active_sequence": 21}
        });
        let path = dir.join("edit.fcproj");
        std::fs::write(&path, doc.to_string()).unwrap();
        path
    }

    #[test]
    fn reads_sequences_tracks_and_clips() {
        let tmp = tempfile::tempdir().unwrap();
        let e = read_edit(&fixture(tmp.path(), 17)).unwrap();
        assert_eq!(e.name, "Rain edit");
        let names: Vec<&str> = e.sequences.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Teaser", "Main edit"]);
        let s = &e.sequences[1];
        assert_eq!((s.width, s.height), (720, 1280));
        assert!((s.fps - 23.976).abs() < 0.001);
        assert_eq!(s.length(), 3.0);
        let v1 = &s.video[0];
        assert_eq!(v1.name, "Video 1");
        assert_eq!(
            v1.clips[0],
            ClipInfo {
                name: "01-010#1".into(),
                file: Some("/film/jobs/01A/out.mp4".into()),
                start: 0.0,
                length: 2.0,
                from: 0.5,
                speed: 1.0,
                enabled: true,
            }
        );
        // A subclip plays its parent's file.
        assert_eq!(v1.clips[1].file, v1.clips[0].file);
        // A generator has no file.
        assert_eq!(s.video[1].clips[0].file, None);
        // Relative to the project's folder.
        assert_eq!(
            s.audio[0].clips[0].file,
            Some(tmp.path().join("../music.wav"))
        );
    }

    #[test]
    fn a_newer_schema_is_read_while_it_can_be() {
        let tmp = tempfile::tempdir().unwrap();
        let path = fixture(tmp.path(), KNOWN_SCHEMA + 5);
        assert_eq!(read_edit(&path).unwrap().sequences.len(), 2);

        let broken =
            serde_json::json!({"schema_version": KNOWN_SCHEMA + 5, "project": {"name": 3}});
        std::fs::write(&path, broken.to_string()).unwrap();
        let e = read_edit(&path).unwrap_err();
        assert!(
            e.contains("from a newer FilmCraft (project schema v22)"),
            "{e}"
        );
    }

    #[tokio::test]
    async fn answers_sequences_for_shared() {
        let tmp = tempfile::tempdir().unwrap();
        let film = tmp.path().join("film");
        let plugin = FilmCraft;
        assert!(plugin.sequences(&film).await.unwrap().edits.is_empty());
        let dir = film.join(SHARED_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        fixture(&dir, 17);
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        std::fs::write(dir.join("broken.fcproj"), "{").unwrap();
        let edits = plugin.sequences(&film).await.unwrap().edits;
        assert_eq!(edits.len(), 2);
        assert_eq!(
            (edits[0].name.as_str(), edits[0].file.clone()),
            ("broken", dir.join("broken.fcproj"))
        );
        assert!(edits[0].error.is_some());
        assert_eq!(edits[1].sequences.len(), 2);
    }

    #[tokio::test]
    async fn conforms() {
        let dir = tempfile::tempdir().unwrap();
        let checks = shotline_plugin::conformance::check(
            &FilmCraft,
            &shotline_plugin::conformance::Options::offline(dir.path()),
        )
        .await;
        let failed: Vec<_> = checks.iter().filter(|c| c.problem.is_some()).collect();
        assert!(failed.is_empty(), "{failed:#?}");
    }
}
