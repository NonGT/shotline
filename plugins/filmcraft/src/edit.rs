//! One change to a clip, and how to take it back. The clip is found in a snapshot of the project
//! as the editor has it: the file itself for the CLI, a copy the app writes for the app. Undo is
//! a list of FilmCraft commands made from that snapshot, so it only touches the clip it changed
//! and leaves whatever was done in FilmCraft since alone.

use std::path::Path;

use serde_json::{Value, json};
use shotline_plugin::protocol::{Change, EditParams, Edited};

use crate::editor::{self, Editor};

const TICKS_PER_SECOND: f64 = 254_016_000_000.0;

fn ticks(seconds: f64) -> i64 {
    (seconds * TICKS_PER_SECOND).round() as i64
}

/// The project as the editor has it now.
fn snapshot(e: &Editor, film: &Path, project: &Path) -> Result<Value, String> {
    let path = match e {
        Editor::App(_) => {
            let copy = film.join(".shotline/cache/filmcraft-snapshot.fcproj");
            if let Some(dir) = copy.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            e.read("file.saveCopy", json!({"path": copy}))?;
            copy
        }
        Editor::Cli(_) => project.to_path_buf(),
    };
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if matches!(e, Editor::App(_)) {
        let _ = std::fs::remove_file(&path);
    }
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// What the change needs to know about the clip.
struct Found {
    sequence: u64,
    clip: Value,
    track: u64,
    /// The sound that goes with it: same item, same place, on an audio track.
    sound: Option<(Value, u64)>,
}

fn find(snap: &Value, sequence: &str, clip: &str) -> Result<Found, String> {
    let items = snap["project"]["items"]
        .as_object()
        .ok_or("the project has no items")?;
    let (id, seq) = items
        .iter()
        .find(|(_, i)| i["name"] == sequence && i["kind"]["Sequence"].is_object())
        .ok_or_else(|| format!("there's no sequence {sequence:?}"))?;
    let seq = &seq["kind"]["Sequence"];
    let sequence: u64 = id.parse().map_err(|_| "a sequence without an id")?;
    let want: u64 = clip
        .parse()
        .map_err(|_| format!("{clip:?} isn't a FilmCraft clip id"))?;
    let tracks = |kind: &str| seq[kind].as_array().cloned().unwrap_or_default();
    for t in tracks("video_tracks") {
        let Some(c) = t["items"]
            .as_array()
            .and_then(|cs| cs.iter().find(|c| c["id"].as_u64() == Some(want)))
        else {
            continue;
        };
        let sound = tracks("audio_tracks").into_iter().find_map(|a| {
            let s = a["items"].as_array()?.iter().find(|s| {
                s["item"] == c["item"] && s["start"] == c["start"] && s["duration"] == c["duration"]
            })?;
            Some((s.clone(), a["id"].as_u64()?))
        });
        return Ok(Found {
            sequence,
            clip: c.clone(),
            track: t["id"].as_u64().ok_or("a track without an id")?,
            sound,
        });
    }
    Err(format!(
        "clip {clip} isn't on a video track of {sequence:?} any more"
    ))
}

fn step(id: &str, params: Value) -> Value {
    json!({"id": id, "params": params})
}

/// Puts `item` over the found clip's place.
fn place(f: &Found, item: &Value, from: i64) -> Value {
    let mut p = json!({
        "item": item,
        "track": f.track,
        "time": f.clip["start"],
        "insert": false,
        "sourceIn": from,
        "duration": f.clip["duration"],
        "audio": f.sound.is_some(),
    });
    if let Some((_, track)) = &f.sound {
        p["audioTrack"] = json!(track);
    }
    step("timeline.place", p)
}

/// The clip's Lumetri, if it has one: its place among the clip's effects and its values.
fn lumetri(clip: &Value) -> Option<(usize, Value)> {
    clip["effects"]
        .as_array()?
        .iter()
        .enumerate()
        .find(|(_, e)| e["effect"] == "lumetri")
        .map(|(i, e)| (i, e["params"].clone()))
}

/// Steps that set the clip's Lumetri back to `before`, given it's `after` now.
fn lumetri_back(clip: u64, before: Option<(usize, Value)>, after: &Value) -> Vec<Value> {
    let Some((index, old)) = before else {
        // It had none: take off the one the change added.
        return match lumetri(after) {
            Some((i, _)) => vec![step("effects.remove", json!({"clip": clip, "index": i}))],
            None => vec![],
        };
    };
    let now = lumetri(after).map(|(_, p)| p).unwrap_or_default();
    let mut steps = vec![];
    for (name, was) in old.as_object().into_iter().flatten() {
        if now.get(name) == Some(was) {
            continue;
        }
        let v = &was["value"];
        let value = if let Some(f) = v.get("Float") {
            f.clone()
        } else if let Some(p) = v.get("Vec2") {
            json!([p["x"], p["y"]])
        } else if let Some(b) = v.get("Bool") {
            b.clone()
        } else if let Some(c) = v.get("Choice") {
            c.clone()
        } else if let Some(t) = v.get("Text") {
            let cmd = match name.as_str() {
                "input_lut" => "lumetri.setInputLut",
                "look_lut" => "lumetri.setLook",
                _ => continue,
            };
            steps.push(step(cmd, json!({"clip": clip, "lut": t})));
            continue;
        } else {
            // Curves and the like can't be set from outside; a match or a LUT doesn't change them.
            continue;
        };
        steps.push(step(
            "effects.setParam",
            json!({"clip": clip, "effect": index, "param": name, "value": value}),
        ));
    }
    steps
}

pub fn edit(ask: &EditParams) -> Result<Edited, String> {
    let e = editor::for_project(&ask.film, &ask.file)?;
    // Saved for the user only when nothing of theirs was waiting to be.
    let clean = match &e {
        Editor::App(_) => e.read("project.inspect", json!({}))?["dirty"] == false,
        Editor::Cli(_) => true,
    };
    let undo = match &ask.change {
        Change::Undo { steps } => {
            let steps = steps.as_array().ok_or("nothing to undo")?;
            e.batch(steps)?;
            None
        }
        change => Some(Value::Array(apply(&e, ask, change)?)),
    };
    let saved = match &e {
        Editor::Cli(_) => true,
        Editor::App(_) if clean => e.exec("file.save", json!({})).is_ok(),
        Editor::App(_) => false,
    };
    Ok(Edited { undo, saved })
}

/// Makes the change and gives back the steps that take it back.
fn apply(e: &Editor, ask: &EditParams, change: &Change) -> Result<Vec<Value>, String> {
    let before = snapshot(e, &ask.film, &ask.file)?;
    let f = find(&before, &ask.sequence, &ask.clip)?;
    let clip = f.clip["id"].as_u64().ok_or("a clip without an id")?;
    let open = step("sequence.open", json!({"item": f.sequence}));
    match change {
        Change::Replace { file, from } => {
            if !file.is_file() {
                return Err(format!("{} isn't there", file.display()));
            }
            let got = e.exec("file.import", json!({"paths": [file]}))?;
            if let Some(err) = got["errors"].as_array().and_then(|a| a.first()) {
                return Err(format!(
                    "FilmCraft couldn't import {}: {err}",
                    file.display()
                ));
            }
            let item = got["items"]
                .as_array()
                .and_then(|a| a.first())
                .cloned()
                .ok_or("FilmCraft imported nothing")?;
            e.batch(&[open.clone(), place(&f, &item, ticks(*from))])?;
            Ok(vec![
                open,
                place(
                    &f,
                    &f.clip["item"],
                    f.clip["source_in"].as_i64().unwrap_or(0),
                ),
            ])
        }
        Change::Lut { file } => {
            let p = match file {
                Some(path) => json!({"clip": clip, "path": path}),
                None => json!({"clip": clip, "lut": ""}),
            };
            e.batch(&[open.clone(), step("lumetri.setInputLut", p)])?;
            colour_undo(e, ask, &f, clip, open)
        }
        Change::Match { to } => {
            let p = json!({"clip": clip, "referenceTime": ticks(*to), "faceDetection": false});
            e.batch(&[open.clone(), step("lumetri.applyMatch", p)])?;
            colour_undo(e, ask, &f, clip, open)
        }
        Change::Undo { .. } => unreachable!("undo has no snapshot"),
    }
}

fn colour_undo(
    e: &Editor,
    ask: &EditParams,
    f: &Found,
    clip: u64,
    open: Value,
) -> Result<Vec<Value>, String> {
    let after = snapshot(e, &ask.film, &ask.file)?;
    let now = find(&after, &ask.sequence, &ask.clip)?;
    let mut steps = vec![open];
    steps.extend(lumetri_back(clip, lumetri(&f.clip), &now.clip));
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(effects: Value) -> Value {
        json!({"id": 5, "item": 2, "start": 0, "duration": 10, "source_in": 0, "effects": effects})
    }

    fn lum(params: Value) -> Value {
        json!({"effect": "lumetri", "params": params})
    }

    #[test]
    fn a_new_lumetri_is_taken_off() {
        let after = clip(json!([{"effect": "motion"}, lum(json!({}))]));
        let steps = lumetri_back(5, None, &after);
        assert_eq!(
            steps,
            [step("effects.remove", json!({"clip": 5, "index": 1}))]
        );
    }

    #[test]
    fn changed_values_are_set_back() {
        let v = |x: Value| json!({"value": x});
        let before = json!({
            "saturation": v(json!({"Float": 80.0})),
            "wheel_midtones": v(json!({"Vec2": {"x": 0.0, "y": 0.0}})),
            "input_lut": v(json!({"Text": ""})),
            "exposure": v(json!({"Float": 1.5})),
            "curve_rgb": v(json!({"Curve": []})),
        });
        let after = clip(json!([lum(json!({
            "saturation": v(json!({"Float": 107.6})),
            "wheel_midtones": v(json!({"Vec2": {"x": -0.6, "y": -0.5}})),
            "input_lut": v(json!({"Text": "lib:lut14"})),
            "exposure": v(json!({"Float": 1.5})),
            "curve_rgb": v(json!({"Curve": [1]})),
        }))]));
        let mut steps = lumetri_back(5, Some((0, before)), &after);
        steps.sort_by_key(|s| s.to_string());
        assert_eq!(
            steps,
            [
                step(
                    "effects.setParam",
                    json!({"clip": 5, "effect": 0, "param": "saturation", "value": 80.0})
                ),
                step(
                    "effects.setParam",
                    json!({"clip": 5, "effect": 0, "param": "wheel_midtones", "value": [0.0, 0.0]})
                ),
                step("lumetri.setInputLut", json!({"clip": 5, "lut": ""})),
            ]
        );
    }

    #[test]
    fn finds_a_clip_and_its_sound() {
        let c = |id: u64, start: i64| json!({"id": id, "item": 2, "start": start, "duration": 10});
        let snap = json!({"project": {"items": {
            "1": {"name": "Main", "kind": {"Sequence": {
                "video_tracks": [{"id": 4, "items": [c(5, 0), c(6, 10)]}],
                "audio_tracks": [{"id": 7, "items": [c(8, 0), c(9, 10)]}],
            }}},
            "2": {"name": "01-010#1", "kind": {"Media": {}}},
        }}});
        let f = find(&snap, "Main", "6").unwrap();
        assert_eq!((f.sequence, f.track), (1, 4));
        assert_eq!(
            f.sound.as_ref().map(|(s, t)| (s["id"].clone(), *t)),
            Some((json!(9), 7))
        );
        let p = place(&f, &json!(3), 12);
        assert_eq!(p["params"]["time"], 10);
        assert_eq!(p["params"]["audioTrack"], 7);
        assert!(
            find(&snap, "Main", "8").is_err(),
            "a sound clip isn't a picture"
        );
        assert!(find(&snap, "Other", "6").is_err());
    }
}
