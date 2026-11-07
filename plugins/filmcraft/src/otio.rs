//! shotline's cut written as an OpenTimelineIO timeline, which FilmCraft imports as a sequence.
//! Its size and frame rate go in FilmCraft's own metadata; without it the sequence would be
//! 1920 x 1080.

use std::path::Path;

use serde_json::{Value, json};
use shotline_plugin::protocol::{ClipInfo, SequenceInfo, TrackInfo};

/// `fps` as FilmCraft's rational frame rate: the NTSC rates as /1001, others whole.
pub fn rational(fps: f64) -> (u32, u32) {
    let whole = fps.round();
    if (fps - whole).abs() < 0.001 {
        return (whole.max(1.0) as u32, 1);
    }
    let ntsc = (fps * 1.001).round();
    if (fps - ntsc / 1.001).abs() < 0.01 {
        return (ntsc as u32 * 1000, 1001);
    }
    ((fps * 1000.0).round() as u32, 1000)
}

fn time(seconds: f64, fps: f64) -> Value {
    json!({"OTIO_SCHEMA": "RationalTime.1", "rate": fps, "value": (seconds * fps).round()})
}

fn range(start: f64, length: f64, fps: f64) -> Value {
    json!({"OTIO_SCHEMA": "TimeRange.1", "start_time": time(start, fps), "duration": time(length, fps)})
}

/// A file URL, with what isn't plain in a path escaped.
pub fn file_url(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    let mut url = String::from(if p.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for b in p.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                url.push(b as char)
            }
            _ => url.push_str(&format!("%{b:02X}")),
        }
    }
    url
}

fn gap(length: f64, fps: f64) -> Value {
    json!({"OTIO_SCHEMA": "Gap.1", "name": "", "source_range": range(0.0, length, fps),
           "effects": [], "markers": [], "enabled": true, "metadata": {}})
}

fn clip(c: &ClipInfo, file: &Path, fps: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "Clip.2",
        "name": c.name,
        "source_range": range(c.from, c.length, fps),
        "media_references": {"DEFAULT_MEDIA": {
            "OTIO_SCHEMA": "ExternalReference.1",
            "target_url": file_url(file),
            "available_range": null,
            "metadata": {},
        }},
        "active_media_reference_key": "DEFAULT_MEDIA",
        "effects": [], "markers": [], "enabled": c.enabled, "metadata": {},
    })
}

/// Clips in time order with gaps between; ones without a file, or overlapping the one before,
/// are left out. Speed isn't carried over.
fn track(t: &TrackInfo, kind: &str, fps: f64) -> Value {
    let mut clips: Vec<&ClipInfo> = t.clips.iter().filter(|c| c.length > 0.0).collect();
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut children = vec![];
    let mut at = 0.0;
    for c in clips {
        let Some(file) = &c.file else { continue };
        if c.start < at - 1e-6 {
            continue;
        }
        if c.start > at + 1e-6 {
            children.push(gap(c.start - at, fps));
        }
        children.push(clip(c, file, fps));
        at = c.start + c.length;
    }
    json!({"OTIO_SCHEMA": "Track.1", "name": t.name, "kind": kind, "children": children,
           "effects": [], "markers": [], "enabled": t.enabled, "metadata": {}})
}

pub fn timeline(name: &str, cut: &SequenceInfo) -> Value {
    let fps = if cut.fps > 0.0 { cut.fps } else { 24.0 };
    let (num, den) = rational(fps);
    let settings = json!({
        "width": cut.width.max(2) & !1,
        "height": cut.height.max(2) & !1,
        "frame_rate": {"num": num, "den": den},
        "par": [1, 1],
        "sample_rate": 48000,
        "drop_frame": false,
        "preset": "Custom",
        "audio_master": "Stereo",
        "preview_codec": "ProRes 422",
        "max_bit_depth": false,
        "max_render_quality": false,
        "working_space": "Rec. 709",
    });
    let tracks: Vec<Value> = cut
        .video
        .iter()
        .map(|t| track(t, "Video", fps))
        .chain(cut.audio.iter().map(|t| track(t, "Audio", fps)))
        .collect();
    json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": name,
        "global_start_time": null,
        "metadata": {"filmcraft": {"settings": settings}},
        "tracks": {"OTIO_SCHEMA": "Stack.1", "name": "", "children": tracks,
                   "effects": [], "markers": [], "enabled": true, "metadata": {}},
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_rates() {
        assert_eq!(rational(24.0), (24, 1));
        assert_eq!(rational(23.976), (24000, 1001));
        assert_eq!(rational(29.97), (30000, 1001));
        assert_eq!(rational(12.5), (12500, 1000));
    }

    #[test]
    fn urls() {
        assert_eq!(
            file_url(Path::new("/film/jobs/a b/out.mp4")),
            "file:///film/jobs/a%20b/out.mp4"
        );
        assert_eq!(
            file_url(Path::new(r"C:\film\ผกา.mp4")),
            "file:///C:/film/%E0%B8%9C%E0%B8%81%E0%B8%B2.mp4"
        );
    }

    #[test]
    fn a_cut_with_gaps_and_music() {
        let c = |name: &str, file: Option<&str>, start: f64, length: f64, from: f64| ClipInfo {
            name: name.into(),
            file: file.map(Into::into),
            start,
            length,
            from,
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
                clips: vec![
                    c("01-020#1", Some("/f/b.mp4"), 3.0, 1.0, 0.5),
                    c("01-010#2", Some("/f/a.mp4"), 0.0, 2.0, 0.0),
                    c("title", None, 4.0, 1.0, 0.0),
                ],
            }],
            audio: vec![TrackInfo {
                name: "A1".into(),
                enabled: true,
                clips: vec![c("music.wav", Some("/f/music.wav"), 0.0, 4.0, 0.0)],
            }],
        };
        let t = timeline("fan", &cut);
        let s = &t["metadata"]["filmcraft"]["settings"];
        assert_eq!(
            (s["width"].as_u64(), s["height"].as_u64()),
            (Some(720), Some(1280))
        );
        let tracks = t["tracks"]["children"].as_array().unwrap();
        assert_eq!(tracks[1]["kind"], "Audio");
        let v1 = tracks[0]["children"].as_array().unwrap();
        let schemas: Vec<&str> = v1
            .iter()
            .map(|c| c["OTIO_SCHEMA"].as_str().unwrap())
            .collect();
        assert_eq!(schemas, ["Clip.2", "Gap.1", "Clip.2"]);
        assert_eq!(v1[1]["source_range"]["duration"]["value"], 24.0);
        assert_eq!(v1[2]["source_range"]["start_time"]["value"], 12.0);
        assert_eq!(
            v1[2]["media_references"]["DEFAULT_MEDIA"]["target_url"],
            "file:///f/b.mp4"
        );
    }
}
