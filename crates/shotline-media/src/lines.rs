//! A shot's spoken lines laid out on one track, for lip sync models that take a single clip.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Error, Ffmpeg, Result, run};

/// Silence between lines that don't say when they start.
pub const GAP: f64 = 0.3;

pub struct Clip {
    pub path: PathBuf,
    /// Seconds into the shot; without it the line follows the one before.
    pub at: Option<f64>,
}

/// Where each clip starts, given their lengths.
pub fn starts(clips: &[(Option<f64>, f64)]) -> Vec<f64> {
    let mut end: Option<f64> = None;
    clips
        .iter()
        .map(|&(at, len)| {
            let start = at.unwrap_or(match end {
                Some(e) => e + GAP,
                None => 0.0,
            });
            end = Some(start + len);
            start
        })
        .collect()
}

impl Ffmpeg {
    pub fn audio_duration(&self, path: &Path) -> Result<f64> {
        let out = run(
            Command::new(&self.ffprobe)
                .args(["-v", "error", "-show_entries", "format=duration"])
                .args(["-of", "default=noprint_wrappers=1:nokey=1"])
                .arg(path),
            &format!("ffprobe {}", path.display()),
        )?;
        String::from_utf8_lossy(&out)
            .trim()
            .parse()
            .map_err(|_| Error::Invalid(format!("{} has no length", path.display())))
    }

    /// Mixes the clips into one mono 48 kHz WAV, each placed where it starts. Returns the starts.
    pub fn join_lines(&self, clips: &[Clip], out: &Path) -> Result<Vec<f64>> {
        if clips.is_empty() {
            return Err(Error::Invalid("no lines to join".into()));
        }
        let mut timed = vec![];
        for c in clips {
            timed.push((c.at, self.audio_duration(&c.path)?));
        }
        let starts = starts(&timed);
        let mut cmd = self.ffmpeg();
        let mut graph = String::new();
        for (i, (c, s)) in clips.iter().zip(&starts).enumerate() {
            cmd.arg("-i").arg(&c.path);
            let ms = (s * 1000.0).round() as u64;
            graph += &format!(
                "[{i}:a]aformat=sample_rates=48000:channel_layouts=mono,adelay={ms}:all=1[a{i}];"
            );
        }
        for i in 0..clips.len() {
            graph += &format!("[a{i}]");
        }
        graph += &format!(
            "amix=inputs={}:normalize=0:duration=longest[out]",
            clips.len()
        );
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        run(
            cmd.args(["-filter_complex", &graph, "-map", "[out]"])
                .args(["-c:a", "pcm_s16le"])
                .arg(out),
            &format!("joining {} lines", clips.len()),
        )?;
        Ok(starts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_follow_each_other_unless_timed() {
        assert_eq!(starts(&[(None, 1.0), (None, 2.0)]), [0.0, 1.3]);
        assert_eq!(starts(&[(Some(0.5), 1.0), (None, 1.0)]), [0.5, 1.8]);
        // A set start can overlap or leave a long pause.
        assert_eq!(starts(&[(None, 2.0), (Some(1.0), 1.0)]), [0.0, 1.0]);
        assert_eq!(
            starts(&[(None, 1.0), (Some(4.0), 1.0), (None, 1.0)]),
            [0.0, 4.0, 5.3]
        );
    }
}
