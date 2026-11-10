//! Links shotline to FilmCraft (github.com/storytold/filmcraft): the projects saved in the film's
//! `shared/` folder, read only. FilmCraft writes the whole file on save, so anything written into
//! it here would be lost.
//!
//! Only what a cut needs is read: sequences, their tracks, and each clip's file and timing.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use shotline_plugin::protocol::{
    ClipInfo, Description, EditInfo, EditParams, Edited, ErrorKind, ExportParams, Exported,
    ImportParams, Imported, OpenParams, Outputs, PROTOCOL_VERSION, PluginError, Quote, Request,
    SequenceInfo, Sequences, Status, TakeFile, TrackInfo, TransitionInfo,
};
use shotline_plugin::{Error, Plugin, Result};

pub mod control;
mod edit;
pub mod editor;
mod otio;

pub const SHARED_DIR: &str = "shared";
const APP: &str = "FilmCraft";

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
            app: APP.into(),
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
        let warnings = editor::cli_program()
            .and_then(|cli| editor::mismatch(&cli))
            .into_iter()
            .collect();
        Ok(Sequences {
            edits: shared_edits(film),
            warnings,
        })
    }

    async fn open(&self, ask: &OpenParams) -> Result<()> {
        open_in_filmcraft(ask).map_err(|e| fail(ErrorKind::Other, e))
    }

    async fn edit(&self, ask: &EditParams) -> Result<Edited> {
        edit::edit(ask).map_err(|e| fail(ErrorKind::Other, e))
    }

    async fn export(&self, ask: &ExportParams) -> Result<Exported> {
        edit::export(ask).map_err(|e| fail(ErrorKind::Other, e))
    }

    async fn import(&self, ask: &ImportParams) -> Result<Imported> {
        send_takes(ask).map_err(|e| fail(ErrorKind::Other, e))
    }
}

fn waiting_file(film: &Path) -> PathBuf {
    film.join(".shotline/cache/filmcraft-waiting.json")
}

fn waiting(film: &Path) -> Vec<TakeFile> {
    std::fs::read_to_string(waiting_file(film))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn keep_waiting(film: &Path, takes: &[TakeFile]) -> std::result::Result<(), String> {
    let path = waiting_file(film);
    let r = if takes.is_empty() {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    } else {
        std::fs::create_dir_all(path.parent().unwrap_or(film)).and_then(|()| {
            std::fs::write(
                &path,
                serde_json::to_string_pretty(takes).unwrap_or_default(),
            )
        })
    };
    r.map_err(|e| format!("can't keep the takes for {APP}: {e}"))
}

fn add_waiting(film: &Path, takes: &[TakeFile]) -> std::result::Result<Vec<TakeFile>, String> {
    let mut all = waiting(film);
    for t in takes {
        if !all.iter().any(|w| w.name == t.name) {
            all.push(t.clone());
        }
    }
    keep_waiting(film, &all)?;
    Ok(all)
}

/// Adds the takes to the ones waiting, and sends them all if the film's project is open in a
/// FilmCraft started from shotline.
fn send_takes(ask: &ImportParams) -> std::result::Result<Imported, String> {
    let film = &ask.film;
    let all = add_waiting(film, &ask.takes)?;
    let project = fcprojs(&own_dir(film)).into_iter().next();
    let left = match project.map(|p| editor::for_project(film, &p)) {
        Some(Ok(e)) => deliver(&e, film, &all)?,
        _ => all.clone(),
    };
    keep_waiting(film, &left)?;
    Ok(Imported {
        sent: all.len() - left.len(),
        waiting: left.len(),
    })
}

pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// The bin called `name` right inside `bin`.
fn bin_in(bin: &Value, name: &str) -> Option<u64> {
    bin["children"]
        .as_array()?
        .iter()
        .find(|c| c["children"].is_array() && c["name"] == name)
        .and_then(|c| c["id"].as_u64())
}

fn find_bin(root: &Value, id: u64) -> Option<&Value> {
    if root["id"].as_u64() == Some(id) {
        return Some(root);
    }
    root["children"]
        .as_array()?
        .iter()
        .find_map(|c| find_bin(c, id))
}

/// Imports takes into `shotline/<shot>` bins of the open project, each named as its take, and
/// returns the ones that couldn't go: all of them when another project is open in FilmCraft.
/// One already in its bin isn't imported twice; one whose file is gone is dropped.
fn deliver(
    s: &editor::Editor,
    film: &Path,
    takes: &[TakeFile],
) -> std::result::Result<Vec<TakeFile>, String> {
    if takes.is_empty() {
        return Ok(vec![]);
    }
    let project = s.read("project.inspect", json!({}))?;
    let ours = project["path"]
        .as_str()
        .and_then(|p| Path::new(p).parent())
        .is_some_and(|d| same_file(d, &own_dir(film)) || same_file(d, &film.join(SHARED_DIR)));
    if !ours {
        return Ok(takes.to_vec());
    }
    let mut root = project["root"].clone();
    let mut bins: HashMap<String, u64> = HashMap::new();
    let top = match bin_in(&root, "shotline") {
        Some(id) => id,
        None => s.exec("file.newBin", json!({"name": "shotline"}))?["bin"]
            .as_u64()
            .ok_or("file.newBin gave no bin")?,
    };
    for t in takes {
        if !t.file.is_file() {
            continue;
        }
        let bin = match bins.get(&t.shot) {
            Some(b) => *b,
            None => {
                let found = find_bin(&root, top).and_then(|b| bin_in(b, &t.shot));
                let b = match found {
                    Some(b) => b,
                    None => s.exec("file.newBin", json!({"name": t.shot, "parent": top}))?["bin"]
                        .as_u64()
                        .ok_or("file.newBin gave no bin")?,
                };
                bins.insert(t.shot.clone(), b);
                b
            }
        };
        let there = find_bin(&root, bin)
            .and_then(|b| b["children"].as_array())
            .is_some_and(|c| c.iter().any(|i| i["name"] == t.name));
        if there {
            continue;
        }
        let got = s.exec("file.import", json!({"paths": [t.file], "bin": bin}))?;
        if let Some(e) = got["errors"].as_array().and_then(|e| e.first()) {
            return Err(format!("{APP} couldn't import {}: {e}", t.name));
        }
        if let Some(item) = got["items"].as_array().and_then(|i| i.first()) {
            let _ = s.exec("clip.rename", json!({"item": item, "name": t.name}));
        }
        root = s.read("project.inspect", json!({}))?["root"].clone();
    }
    Ok(vec![])
}

fn fail(kind: ErrorKind, message: String) -> Error {
    PluginError::new(kind, message).into()
}

/// Where the film's own FilmCraft project and what's passed to it live.
pub fn own_dir(film: &Path) -> PathBuf {
    film.join(SHARED_DIR).join("filmcraft")
}

fn fcprojs(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "fcproj"))
        .collect();
    paths.sort();
    paths
}

/// Opens `ask.file`, else the film's own project, else makes that from shotline's cut: FilmCraft
/// imports the cut and saves the project itself, so the file is always one it wrote.
fn open_in_filmcraft(ask: &OpenParams) -> std::result::Result<(), String> {
    let film = &ask.film;
    if !ask.takes.is_empty() {
        add_waiting(film, &ask.takes)?;
    }
    let target = ask
        .file
        .clone()
        .or_else(|| fcprojs(&own_dir(film)).into_iter().next());
    let running = control::Session::load(film).filter(control::Session::alive);
    let s = match target {
        Some(file) => {
            if !file.is_file() {
                return Err(format!("{} isn't there", file.display()));
            }
            let s = match running {
                Some(s) => {
                    switch_to(&s, &file)?;
                    s
                }
                None => control::start(film, &[file.into()])?,
            };
            if let Some(name) = &ask.sequence {
                show_sequence(&s, name);
            }
            s
        }
        None => make_project(film, ask.cut.as_ref(), running)?,
    };
    let left = deliver(&editor::Editor::App(s.clone()), film, &waiting(film));
    let _ = s.call("ui.focus", json!({}));
    keep_waiting(film, &left?)
}

/// Opens `file` in a running FilmCraft, unless that would lose changes it hasn't saved.
fn switch_to(s: &control::Session, file: &Path) -> std::result::Result<(), String> {
    let now = s.exec("project.inspect", json!({}))?;
    if now["path"].as_str().map(Path::new) == Some(file) {
        return Ok(());
    }
    if now["dirty"] == true {
        return Err(format!(
            "{APP} has changes in {} that aren't saved; save them there first",
            now["name"].as_str().unwrap_or("its project")
        ));
    }
    s.exec("file.open", json!({"path": file})).map(drop)
}

/// The id of the sequence called `name` in the open project, from its bins.
fn sequence_id(bin: &Value, name: &str) -> Option<u64> {
    for c in bin["children"].as_array()? {
        if c["type"] == "Sequence" && c["name"] == name {
            return c["item"].as_u64();
        }
        if let Some(id) = sequence_id(c, name) {
            return Some(id);
        }
    }
    None
}

fn show_sequence(s: &control::Session, name: &str) {
    if let Ok(p) = s.exec("project.inspect", json!({}))
        && let Some(id) = sequence_id(&p["root"], name)
    {
        let _ = s.exec("sequence.open", json!({"item": id}));
    }
}

fn make_project(
    film: &Path,
    cut: Option<&SequenceInfo>,
    running: Option<control::Session>,
) -> std::result::Result<control::Session, String> {
    let cut = cut.ok_or("there's no FilmCraft project yet, and no cut to start one from")?;
    if cut.video.iter().all(|t| t.clips.is_empty()) {
        return Err("the cut has no takes yet; select some first".into());
    }
    let name = film
        .canonicalize()
        .unwrap_or_else(|_| film.to_path_buf())
        .file_name()
        .map_or("film".into(), |n| n.to_string_lossy().into_owned());
    let dir = own_dir(film);
    std::fs::create_dir_all(&dir).map_err(|e| format!("can't make {}: {e}", dir.display()))?;
    let otio = dir.join("cut.otio");
    let text = serde_json::to_string_pretty(&otio::timeline(&name, cut)).unwrap_or_default();
    std::fs::write(&otio, text).map_err(|e| format!("can't write {}: {e}", otio.display()))?;
    let s = match running {
        Some(s) => {
            let now = s.exec("project.inspect", json!({}))?;
            if now["dirty"] == true {
                return Err(format!(
                    "{APP} has changes in {} that aren't saved; save them there first",
                    now["name"].as_str().unwrap_or("its project")
                ));
            }
            s.exec("file.newProject", json!({"name": name}))?;
            s
        }
        None => control::start(film, &["--empty".into()])?,
    };
    let got = s.exec("file.import", json!({"paths": [otio]}))?;
    if let Some(e) = got["errors"].as_array().and_then(|e| e.first()) {
        return Err(format!("{APP} couldn't import the cut: {e}"));
    }
    let file = dir.join(format!("{name}.fcproj"));
    s.exec("file.saveAs", json!({"path": file}))?;
    // Takes waiting go in once the project is the film's; it's saved again with them.
    if !waiting(film).is_empty() {
        keep_waiting(
            film,
            &deliver(&editor::Editor::App(s.clone()), film, &waiting(film))?,
        )?;
        s.exec("file.save", json!({}))?;
    }
    if let Some(id) = got["sequences"].as_array().and_then(|a| a.first()) {
        let _ = s.exec("sequence.open", json!({"item": id}));
    }
    Ok(s)
}

/// Every `.fcproj` in the film's `shared/` and `shared/filmcraft/`, by name; one that can't be
/// read says why.
pub fn shared_edits(film: &Path) -> Vec<EditInfo> {
    let mut paths = fcprojs(&film.join(SHARED_DIR));
    paths.extend(fcprojs(&own_dir(film)));
    paths
        .into_iter()
        .map(|p| {
            read_edit(&p).unwrap_or_else(|error| EditInfo {
                name: p.file_stem().unwrap_or_default().to_string_lossy().into(),
                file: p,
                app: APP.into(),
                error: Some(error),
                ..EditInfo::default()
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
    /// LUTs put on clips, copied in whole; clips name them "lib:<id>".
    #[serde(default)]
    luts: Vec<Lut>,
}

#[derive(Deserialize, Default)]
struct Lut {
    id: String,
    #[serde(default)]
    source_path: Option<PathBuf>,
    #[serde(default)]
    format: String,
    #[serde(default)]
    text: String,
}

impl Lut {
    /// A file with it in: where it came from while that's unchanged, otherwise a copy.
    fn file(&self) -> Option<PathBuf> {
        if self.text.is_empty() {
            return None;
        }
        if let Some(p) = &self.source_path
            && std::fs::read_to_string(p).is_ok_and(|t| t == self.text)
        {
            return Some(p.clone());
        }
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.text.hash(&mut h);
        let ext = if self.format.is_empty() {
            "cube"
        } else {
            &self.format
        };
        let out = std::env::temp_dir()
            .join("shotline-filmcraft-luts")
            .join(format!("{:016x}.{ext}", h.finish()));
        if !out.is_file() {
            std::fs::create_dir_all(out.parent()?).ok()?;
            std::fs::write(&out, &self.text).ok()?;
        }
        Some(out)
    }
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
    #[serde(default)]
    transitions: Vec<RawTransition>,
}

#[derive(Deserialize)]
struct RawTransition {
    id: u64,
    effect: Value,
    start: i64,
    duration: i64,
    #[serde(default)]
    from: Option<u64>,
    #[serde(default)]
    to: Option<u64>,
}

#[derive(Deserialize)]
struct RawClip {
    #[serde(default)]
    id: u64,
    item: u64,
    name: String,
    start: i64,
    duration: i64,
    source_in: i64,
    #[serde(default = "one")]
    speed: f64,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    effects: Vec<Value>,
}

/// Lumetri's input and look LUTs as files, through the project's own copies; any other name
/// as it is.
fn luts(effects: &[Value], dir: &Path, table: &[Lut]) -> Vec<PathBuf> {
    effects
        .iter()
        .filter(|e| e["effect"] == "lumetri" && e["enabled"] != false)
        .flat_map(|e| {
            ["input_lut", "look_lut"].map(|k| {
                e["params"][k]["value"]["Text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
        })
        .filter(|l| !l.is_empty())
        .map(|l| {
            if let Some(f) = l
                .strip_prefix("lib:")
                .and_then(|id| table.iter().find(|t| t.id == id))
                .and_then(Lut::file)
            {
                return f;
            }
            // One letter before a colon is a drive.
            let named = l.split_once(':').is_some_and(|(a, _)| a.len() > 1);
            if named && !Path::new(&l).is_absolute() {
                PathBuf::from(l)
            } else {
                dir.join(l)
            }
        })
        .collect()
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
            transitions: t
                .transitions
                .iter()
                .map(|x| TransitionInfo {
                    id: x.id.to_string(),
                    name: x.effect["effect"].as_str().unwrap_or_default().into(),
                    from: x.from.map(|i| i.to_string()).unwrap_or_default(),
                    to: x.to.map(|i| i.to_string()).unwrap_or_default(),
                    start: secs(x.start),
                    length: secs(x.duration),
                })
                .collect(),
            name: t.name,
            enabled: if video { t.enabled } else { !t.muted },
            clips: t
                .items
                .into_iter()
                .map(|c| ClipInfo {
                    id: c.id.to_string(),
                    file: media_file(&p.items, c.item, dir),
                    name: c.name,
                    start: secs(c.start),
                    length: secs(c.duration),
                    from: secs(c.source_in),
                    speed: c.speed,
                    enabled: c.enabled,
                    luts: luts(&c.effects, dir, &p.luts),
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
        app: APP.into(),
        sequences,
        ..EditInfo::default()
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

    #[test]
    fn a_clip_s_luts_are_files_by_the_project() {
        let v = |t: &str| json!({"value": {"Text": t}});
        let effects = vec![
            json!({"effect": "motion", "params": {}}),
            json!({"effect": "lumetri", "params": {
                "input_lut": v("luts/match.cube"), "look_lut": v("lib:lut14")}}),
            json!({"effect": "lumetri", "enabled": false, "params": {"input_lut": v("/off.cube")}}),
        ];
        assert_eq!(
            luts(&effects, Path::new("/film/shared"), &[]),
            [
                PathBuf::from("/film/shared/luts/match.cube"),
                PathBuf::from("lib:lut14")
            ]
        );
        // The project's own copy, written out since where it came from is gone.
        let table = [Lut {
            id: "lut14".into(),
            source_path: Some("/gone.cube".into()),
            format: "cube".into(),
            text: "LUT_3D_SIZE 2\n".into(),
        }];
        let got = luts(&effects, Path::new("/film/shared"), &table);
        assert_eq!(std::fs::read_to_string(&got[1]).unwrap(), "LUT_3D_SIZE 2\n");
    }
    use shotline_plugin::protocol::Change;

    const T: i64 = 254_016_000_000;

    /// Shaped like FilmCraft 0.18 writes it, cut down; the parts not read are left in to show
    /// they're skipped.
    /// An absolute path on any system; `/film` has no drive on Windows.
    fn take_file() -> PathBuf {
        let root = if cfg!(windows) { r"C:\film" } else { "/film" };
        Path::new(root).join("jobs").join("01A").join("out.mp4")
    }

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
                        "media": {"File": {"path": take_file()}}, "offline": false}}},
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
                luts: vec![],
                id: "29".into(),
                name: "01-010#1".into(),
                file: Some(take_file()),
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
    async fn opening_a_missing_project_says_so() {
        let film = tempfile::tempdir().unwrap();
        let ask = |file: Option<PathBuf>| OpenParams {
            film: film.path().into(),
            file,
            ..OpenParams::default()
        };
        let e = FilmCraft
            .open(&ask(Some("/nowhere/edit.fcproj".into())))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("isn't there"), "{e}");
        // Nothing to open and nothing to make one from.
        let e = FilmCraft.open(&ask(None)).await.unwrap_err();
        assert!(e.to_string().contains("no cut"), "{e}");
    }

    #[tokio::test]
    async fn takes_wait_while_filmcraft_is_closed() {
        let film = tempfile::tempdir().unwrap();
        let take = |name: &str| TakeFile {
            name: name.into(),
            shot: "01-010".into(),
            file: film.path().join("jobs/x/out.mp4"),
        };
        let ask = |takes| ImportParams {
            film: film.path().into(),
            takes,
        };
        let got = FilmCraft
            .import(&ask(vec![take("01-010#1")]))
            .await
            .unwrap();
        assert_eq!((got.sent, got.waiting), (0, 1));
        let got = FilmCraft
            .import(&ask(vec![take("01-010#1"), take("01-010#2")]))
            .await
            .unwrap();
        assert_eq!((got.sent, got.waiting), (0, 2));
        assert_eq!(waiting(film.path()).len(), 2);
    }

    #[test]
    fn finds_bins() {
        let root = json!({"id": 0, "children": [
            {"item": 1, "name": "shotline", "type": "Sequence"},
            {"id": 3, "name": "shotline", "children": [{"id": 4, "name": "01-010", "children": []}]},
        ]});
        assert_eq!(bin_in(&root, "shotline"), Some(3));
        assert_eq!(bin_in(find_bin(&root, 3).unwrap(), "01-010"), Some(4));
        assert_eq!(bin_in(&root, "01-010"), None);
    }

    #[test]
    fn finds_a_sequence_in_the_bins() {
        let root = json!({"children": [
            {"item": 1, "name": "Main", "type": "Sequence"},
            {"id": 2, "name": "Old", "children": [{"item": 7, "name": "Alt", "type": "Sequence"}]},
        ]});
        assert_eq!(sequence_id(&root, "Alt"), Some(7));
        assert_eq!(sequence_id(&root, "Main"), Some(1));
        assert_eq!(sequence_id(&root, "Nope"), None);
    }

    /// Asks the test's FilmCraft to quit when the test ends, passed or not.
    struct Quit(control::Session);

    impl Drop for Quit {
        fn drop(&mut self) {
            let _ = self.0.call("app.quit", json!({}));
        }
    }

    fn colour_clip(path: &Path, colour: &str, secs: u32, sound: bool) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut c = std::process::Command::new("ffmpeg");
        c.args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("color=c={colour}:s=720x1280:r=24:d={secs}"));
        if sound {
            c.args(["-f", "lavfi", "-i"])
                .arg(format!("sine=f=440:d={secs}"))
                .args(["-c:a", "aac", "-shortest"]);
        }
        let ok = c
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(path)
            .status()
            .unwrap();
        assert!(ok.success());
    }

    /// Changes clips with the real FilmCraft open, then with it closed through filmcraft-cli,
    /// and takes each change back. Run as `with_filmcraft` says.
    #[tokio::test]
    #[ignore]
    async fn edits_with_filmcraft() {
        let tmp = tempfile::tempdir().unwrap();
        let film = tmp.path().join("fan");
        let (a, b, c, d) = (
            film.join("jobs/a/out.mp4"),
            film.join("jobs/b/out.mp4"),
            film.join("jobs/c/out.mp4"),
            film.join("jobs/d/out.mp4"),
        );
        colour_clip(&a, "red", 3, true);
        colour_clip(&b, "blue", 3, true);
        colour_clip(&c, "green", 4, false);
        colour_clip(&d, "yellow", 2, true);
        let piece = |name: &str, file: &Path, start: f64| ClipInfo {
            name: name.into(),
            file: Some(file.into()),
            start,
            length: 3.0,
            speed: 1.0,
            enabled: true,
            ..ClipInfo::default()
        };
        let pieces = vec![piece("01-010#1", &a, 0.0), piece("01-020#1", &b, 3.0)];
        let track = |name: &str| TrackInfo {
            name: name.into(),
            enabled: true,
            clips: pieces.clone(),
            transitions: vec![],
        };
        let cut = SequenceInfo {
            name: "cut".into(),
            width: 720,
            height: 1280,
            fps: 24.0,
            video: vec![track("V1")],
            audio: vec![track("A1")],
        };
        FilmCraft
            .open(&OpenParams {
                film: film.clone(),
                cut: Some(cut),
                ..OpenParams::default()
            })
            .await
            .unwrap();
        let s = control::Session::load(&film).unwrap();
        let _quit = Quit(s.clone());
        let project = own_dir(&film).join("fan.fcproj");
        let seq = || shared_edits(&film)[0].sequences[0].clone();
        let at = |t: f64| {
            let s = seq();
            let v = s.video[0]
                .clips
                .iter()
                .find(|c| c.start == t)
                .cloned()
                .unwrap();
            let a = s.audio[0]
                .clips
                .iter()
                .find(|c| c.start == t)
                .cloned()
                .unwrap();
            (v, a)
        };
        let lumetri_on = |id: &str| {
            let text = std::fs::read_to_string(&project).unwrap();
            let v: Value = serde_json::from_str(&text).unwrap();
            let mut found = None;
            fn walk<'a>(v: &'a Value, id: &str, found: &mut Option<&'a Value>) {
                match v {
                    Value::Object(m) => {
                        if m.get("id").map(|x| x.to_string()) == Some(id.into())
                            && m.contains_key("source_in")
                        {
                            *found = Some(v);
                        }
                        m.values().for_each(|x| walk(x, id, found));
                    }
                    Value::Array(a) => a.iter().for_each(|x| walk(x, id, found)),
                    _ => {}
                }
            }
            walk(&v, id, &mut found);
            found.unwrap()["effects"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["effect"] == "lumetri")
        };
        let ask = |clip: &str, change: Change| EditParams {
            film: film.clone(),
            file: project.clone(),
            sequence: "fan".into(),
            clip: clip.into(),
            change,
        };
        let undo = |e: &Edited| Change::Undo {
            steps: e.undo.clone().unwrap(),
        };

        // With FilmCraft open: the green take over 01-020#1, its sound left as it was.
        let (blue, _) = at(3.0);
        let done = FilmCraft
            .edit(&ask(
                &blue.id,
                Change::Replace {
                    file: c.clone(),
                    from: 0.5,
                    length: None,
                },
            ))
            .await
            .unwrap();
        assert!(done.saved, "nothing else was waiting, so it's saved");
        let (v, snd) = at(3.0);
        assert_eq!((v.file.as_ref(), v.from, v.length), (Some(&c), 0.5, 3.0));
        assert_eq!(snd.file.as_ref(), Some(&b));
        FilmCraft.edit(&ask("", undo(&done))).await.unwrap();
        let (v, _) = at(3.0);
        assert_eq!((v.file.as_ref(), v.from), (Some(&b), 0.0));
        // All 4 s of green over the red 3 s, so blue and its sound move a second later.
        let done = FilmCraft
            .edit(&ask(
                &at(0.0).0.id,
                Change::Replace {
                    file: c.clone(),
                    from: 0.0,
                    length: Some(4.0),
                },
            ))
            .await
            .unwrap();
        let (v, _) = at(0.0);
        assert_eq!((v.file.as_ref(), v.length), (Some(&c), 4.0));
        let (v, snd) = at(4.0);
        assert_eq!((v.file.as_ref(), snd.file.as_ref()), (Some(&b), Some(&b)));
        FilmCraft.edit(&ask("", undo(&done))).await.unwrap();
        let (v, snd) = at(0.0);
        assert_eq!((v.file.as_ref(), v.length), (Some(&a), 3.0));
        assert_eq!(snd.file.as_ref(), Some(&a));
        assert_eq!(at(3.0).0.file.as_ref(), Some(&b));
        let inserted = async || {
            // The green clip in at the join, the blue one and its sound moved on by its length.
            let done = FilmCraft
                .edit(&ask(
                    &at(3.0).0.id,
                    Change::Insert {
                        file: c.clone(),
                        after: false,
                        sound: false,
                    },
                ))
                .await
                .unwrap();
            let (green, (blue, snd)) = (seq().video[0].clips[1].clone(), at(7.0));
            assert_eq!((green.file.as_ref(), green.start), (Some(&c), 3.0));
            assert_eq!(blue.file.as_ref(), Some(&b));
            assert_eq!(snd.file.as_ref(), Some(&b));
            assert_eq!(seq().audio[0].clips.len(), 2, "the bridge is silent");
            done
        };
        let done = inserted().await;
        FilmCraft.edit(&ask("", undo(&done))).await.unwrap();
        assert_eq!(at(3.0).0.file.as_ref(), Some(&b));

        // A new shot's take after the last clip, with its sound.
        let done = FilmCraft
            .edit(&ask(
                &at(3.0).0.id,
                Change::Insert {
                    file: d.clone(),
                    after: true,
                    sound: true,
                },
            ))
            .await
            .unwrap();
        let (v, snd) = at(6.0);
        assert_eq!((v.file.as_ref(), snd.file.as_ref()), (Some(&d), Some(&d)));
        FilmCraft.edit(&ask("", undo(&done))).await.unwrap();
        assert_eq!(seq().video[0].clips.len(), 2);
        assert_eq!(seq().audio[0].clips.len(), 2);
        // Undo can give the clips new ids.
        let v = at(3.0).0;

        // A colour match adds a Lumetri; undo takes it off again.
        let done = FilmCraft
            .edit(&ask(&v.id, Change::Match { to: 2.9 }))
            .await
            .unwrap();
        assert!(lumetri_on(&v.id));
        FilmCraft.edit(&ask("", undo(&done))).await.unwrap();
        assert!(!lumetri_on(&v.id));

        // A dissolve into it, a shorter one over that, and both taken back.
        let fades = || seq().video[0].transitions.clone();
        let half = FilmCraft
            .edit(&ask(&v.id, Change::Crossfade { frames: 12 }))
            .await
            .unwrap();
        let t = fades();
        assert_eq!(t.len(), 1);
        assert_eq!(
            (t[0].to.as_str(), t[0].name.as_str()),
            (v.id.as_str(), "cross_dissolve")
        );
        assert!((t[0].length - 0.5).abs() < 0.01, "{}", t[0].length);
        let quarter = FilmCraft
            .edit(&ask(&v.id, Change::Crossfade { frames: 6 }))
            .await
            .unwrap();
        assert!((fades()[0].length - 0.25).abs() < 0.01);
        FilmCraft.edit(&ask("", undo(&quarter))).await.unwrap();
        assert!((fades()[0].length - 0.5).abs() < 0.01, "{:?}", fades());
        FilmCraft.edit(&ask("", undo(&half))).await.unwrap();
        assert!(fades().is_empty());

        let render = |name: &str| ExportParams {
            film: film.clone(),
            file: project.clone(),
            sequence: "fan".into(),
            out: film.join("renders").join(name),
            saved: false,
        };
        let length = |f: &Path| {
            let out = std::process::Command::new("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-show_entries",
                    "format=duration",
                    "-of",
                    "csv=p=0",
                ])
                .arg(f)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse::<f64>()
                .unwrap()
        };
        let got = FilmCraft.export(&render("open.mp4")).await.unwrap();
        assert!(
            (length(&got.file) - 6.0).abs() < 0.2,
            "{}",
            length(&got.file)
        );
        if editor::cli_program().is_some() {
            // The saved file, rendered beside the open app.
            let ask = ExportParams {
                saved: true,
                ..render("saved.mp4")
            };
            let got = FilmCraft.export(&ask).await.unwrap();
            assert!((length(&got.file) - 6.0).abs() < 0.2);
        }

        let _ = s.call("app.quit", json!({}));
        let pid = s.pid.unwrap();
        assert!((0..80).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(250));
            !editor::running_apps().contains(&pid)
        }));
        if editor::cli_program().is_none() {
            eprintln!("no filmcraft-cli; the rest is skipped");
            return;
        }

        // Closed: the same through filmcraft-cli, with a LUT this time.
        let cube = film.join("half.cube");
        let mut text = String::from("LUT_3D_SIZE 2\n");
        for b in [0, 1] {
            for g in [0, 1] {
                for r in [0, 1] {
                    text += &format!("{} {g} {b}\n", r as f64 * 0.5);
                }
            }
        }
        std::fs::write(&cube, text).unwrap();
        let (red, _) = at(0.0);
        let done = FilmCraft
            .edit(&ask(
                &red.id,
                Change::Lut {
                    file: Some(cube.clone()),
                },
            ))
            .await
            .unwrap();
        assert!(done.saved);
        assert!(lumetri_on(&red.id));
        assert_eq!(at(0.0).0.luts, std::slice::from_ref(&cube));
        FilmCraft.edit(&ask("", undo(&done))).await.unwrap();
        assert!(!lumetri_on(&red.id));
        let (blue, _) = at(3.0);
        let done = FilmCraft
            .edit(&ask(
                &blue.id,
                Change::Replace {
                    file: c.clone(),
                    from: 0.0,
                    length: None,
                },
            ))
            .await
            .unwrap();
        assert_eq!(at(3.0).0.file.as_ref(), Some(&c));
        FilmCraft.edit(&ask("", undo(&done))).await.unwrap();
        assert_eq!(at(3.0).0.file.as_ref(), Some(&b));
        let done = inserted().await;
        FilmCraft.edit(&ask("", undo(&done))).await.unwrap();
        assert_eq!(at(3.0).0.file.as_ref(), Some(&b));

        // Taken off and put back, through the CLI.
        let fades = || seq().video[0].transitions.clone();
        let blue = at(3.0).0.id;
        FilmCraft
            .edit(&ask(&blue, Change::Crossfade { frames: 12 }))
            .await
            .unwrap();
        let off = FilmCraft
            .edit(&ask(&blue, Change::Crossfade { frames: 0 }))
            .await
            .unwrap();
        assert!(fades().is_empty());
        FilmCraft.edit(&ask("", undo(&off))).await.unwrap();
        let t = fades();
        assert_eq!(t.len(), 1);
        assert!((t[0].length - 0.5).abs() < 0.01, "{t:?}");
        assert!(
            FilmCraft
                .edit(&ask(&at(0.0).0.id, Change::Crossfade { frames: 0 }))
                .await
                .is_err(),
            "nothing goes into the first clip"
        );
        let got = FilmCraft.export(&render("closed.mp4")).await.unwrap();
        assert!(
            (length(&got.file) - 6.0).abs() < 0.2,
            "{}",
            length(&got.file)
        );
    }

    /// Starts the real FilmCraft: makes the film's project from a cut, opens it again, and
    /// quits it. Run with FILMCRAFT_DATA_DIR set to a scratch folder to keep it apart from
    /// yours: `cargo test -p shotline-plugin-filmcraft -- --ignored --test-threads=1` (one
    /// FilmCraft at a time, or each sees the other's as one shotline didn't start).
    #[tokio::test]
    #[ignore]
    async fn with_filmcraft() {
        let tmp = tempfile::tempdir().unwrap();
        let film = tmp.path().join("fan");
        std::fs::create_dir_all(film.join("jobs/a")).unwrap();
        let clip = film.join("jobs/a/out.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg("color=c=red:s=720x1280:r=24:d=3")
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .unwrap();
        assert!(ok.success());
        let piece = |start: f64| ClipInfo {
            luts: vec![],
            id: String::new(),
            name: "01-010#1".into(),
            file: Some(clip.clone()),
            start,
            length: 2.0,
            from: 0.5,
            speed: 1.0,
            enabled: true,
        };
        let cut = SequenceInfo {
            name: "cut".into(),
            width: 720,
            height: 1280,
            fps: 24.0,
            video: vec![TrackInfo {
                name: "V1".into(),
                enabled: true,
                clips: vec![piece(0.0), piece(2.0)],
                transitions: vec![],
            }],
            audio: vec![],
        };
        let ask = OpenParams {
            film: film.clone(),
            cut: Some(cut),
            ..OpenParams::default()
        };
        // Made before FilmCraft was open: it waits, then goes in with the new project.
        let take = |name: &str, shot: &str| ImportParams {
            film: film.clone(),
            takes: vec![TakeFile {
                name: name.into(),
                shot: shot.into(),
                file: clip.clone(),
            }],
        };
        let got = FilmCraft.import(&take("01-010#1", "01-010")).await.unwrap();
        assert_eq!(got.waiting, 1);
        FilmCraft.open(&ask).await.unwrap();
        assert!(waiting(&film).is_empty());
        let s = control::Session::load(&film).unwrap();
        let bins = |s: &control::Session| {
            let root = s.exec("project.inspect", json!({})).unwrap()["root"].clone();
            let top = bin_in(&root, "shotline").unwrap();
            find_bin(&root, top).unwrap().clone()
        };
        let shot = |b: &Value, name: &str| -> Vec<String> {
            let b = find_bin(b, bin_in(b, name).unwrap()).unwrap();
            b["children"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["name"].as_str().unwrap().into())
                .collect()
        };
        assert_eq!(shot(&bins(&s), "01-010"), ["01-010#1"]);
        let saved = s.exec("project.inspect", json!({})).unwrap();
        assert_eq!(saved["dirty"], false, "saved with the takes in");

        // Made while it's open: straight in, once.
        let got = FilmCraft.import(&take("01-020#1", "01-020")).await.unwrap();
        assert_eq!((got.sent, got.waiting), (1, 0));
        FilmCraft.import(&take("01-020#1", "01-020")).await.unwrap();
        assert_eq!(shot(&bins(&s), "01-020"), ["01-020#1"]);
        let project = own_dir(&film).join("fan.fcproj");
        assert!(project.is_file());
        let edits = shared_edits(&film);
        assert_eq!(edits.len(), 1);
        let seq = &edits[0].sequences[0];
        assert_eq!((seq.width, seq.height, seq.fps), (720, 1280, 24.0));
        assert_eq!(seq.video[0].clips.len(), 2);
        assert_eq!(seq.video[0].clips[1].start, 2.0);
        assert_eq!(seq.video[0].clips[1].from, 0.5);

        // Asked again, the same FilmCraft shows it; nothing new is made.
        FilmCraft.open(&ask).await.unwrap();
        assert_eq!(control::Session::load(&film), Some(s.clone()));
        let now = s.exec("project.inspect", json!({})).unwrap();
        assert_eq!(now["path"].as_str().map(Path::new), Some(project.as_path()));
        // Saved, then closed: edits go through filmcraft-cli on the file.
        s.exec("file.save", json!({})).unwrap();
        let _ = s.call("app.quit", json!({}));
        let gone = |pid| {
            (0..80).any(|_| {
                std::thread::sleep(std::time::Duration::from_millis(250));
                !editor::running_apps().contains(&pid)
            })
        };
        assert!(gone(s.pid.unwrap()));
        let Some(exe) = editor::cli_program() else {
            eprintln!("no filmcraft-cli; the rest is skipped");
            return;
        };
        let got = FilmCraft.import(&take("01-030#1", "01-030")).await.unwrap();
        assert_eq!((got.sent, got.waiting), (1, 0));
        let cli = editor::Editor::cli(exe, project.clone(), &film);
        let root = cli.read("project.inspect", json!({})).unwrap()["root"].clone();
        let top = find_bin(&root, bin_in(&root, "shotline").unwrap())
            .unwrap()
            .clone();
        assert_eq!(shot(&top, "01-030"), ["01-030#1"]);
        // The CLI's save was backed up first.
        assert_eq!(
            std::fs::read_dir(editor::backups_dir(&film))
                .unwrap()
                .count(),
            1
        );

        // A FilmCraft shotline didn't start could save over the file, so the take waits.
        let mut other = std::process::Command::new(control::program())
            .arg("--empty")
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_secs(2));
        let got = FilmCraft.import(&take("01-040#1", "01-040")).await.unwrap();
        let _ = other.kill();
        let _ = other.wait();
        assert_eq!((got.sent, got.waiting), (0, 1));
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
