//! Pieces for playing a clip in the app: ffmpeg decoding RGBA frames and f32 samples into pipes,
//! and the clock that decides which frame is due.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};

use crate::{Error, Ffmpeg, Result};

pub struct Frame {
    /// Seconds into the clip.
    pub pts: f64,
    pub rgba: Vec<u8>,
}

/// Decoded frames a little ahead of playback. Dropping it stops ffmpeg.
pub struct FramePipe {
    child: Option<Child>,
    rx: Receiver<Frame>,
    next: Option<Frame>,
    ended: bool,
    pub width: u32,
    pub height: u32,
}

/// Keeps the ffmpeg feeding a sample channel running; dropping it stops ffmpeg.
pub struct PcmPipe {
    child: Option<Child>,
}

const AHEAD: usize = 12;
const CHUNK: usize = 4096;

/// Even size that fits in `max` x `max`, keeping the aspect.
pub fn preview_size(width: u32, height: u32, max: u32) -> (u32, u32) {
    let s = (max as f64 / width.max(height).max(1) as f64).min(1.0);
    let even = |v: u32| ((v as f64 * s / 2.0).round() as u32 * 2).max(2);
    (even(width), even(height))
}

fn kill(child: &mut Option<Child>) {
    if let Some(c) = child.as_mut() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

impl Ffmpeg {
    /// Frames of `path` from `start` seconds on, scaled to `width` x `height`.
    pub fn frame_pipe(
        &self,
        path: &Path,
        start: f64,
        width: u32,
        height: u32,
        fps: f64,
    ) -> Result<FramePipe> {
        let mut c = self.command();
        c.args(["-ss", &format!("{start:.3}"), "-i"])
            .arg(path)
            .args(["-an", "-vf", &format!("scale={width}:{height}")])
            .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = c.spawn().map_err(|_| Error::Missing("ffmpeg".into()))?;
        let mut out = child.stdout.take().expect("piped");
        let (tx, rx) = sync_channel(AHEAD);
        let size = (width * height * 4) as usize;
        std::thread::spawn(move || {
            for n in 0.. {
                let mut rgba = vec![0; size];
                if out.read_exact(&mut rgba).is_err() {
                    break;
                }
                let pts = start + n as f64 / fps;
                if tx.send(Frame { pts, rgba }).is_err() {
                    break;
                }
            }
        });
        Ok(FramePipe::new(Some(child), rx, width, height))
    }

    /// Interleaved samples of `path` from `start` seconds on, resampled to `rate` and `channels`.
    pub fn pcm_pipe(
        &self,
        path: &Path,
        start: f64,
        rate: u32,
        channels: u16,
    ) -> Result<(PcmPipe, Receiver<Vec<f32>>)> {
        let mut c = self.command();
        c.args(["-ss", &format!("{start:.3}"), "-i"])
            .arg(path)
            .args(["-vn", "-f", "f32le", "-ar", &rate.to_string()])
            .args(["-ac", &channels.to_string(), "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = c.spawn().map_err(|_| Error::Missing("ffmpeg".into()))?;
        let out = child.stdout.take().expect("piped");
        // About a second and a half buffered at 48 kHz stereo.
        let (tx, rx) = sync_channel(36);
        std::thread::spawn(move || pump(out, tx));
        Ok((PcmPipe { child: Some(child) }, rx))
    }
}

fn floats(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

fn pump(mut out: impl Read, tx: SyncSender<Vec<f32>>) {
    let mut bytes = vec![0u8; CHUNK * 4];
    let mut have = 0;
    loop {
        let n = match out.read(&mut bytes[have..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        have += n;
        if have < bytes.len() {
            continue;
        }
        if tx.send(floats(&bytes)).is_err() {
            return;
        }
        have = 0;
    }
    let tail = floats(&bytes[..have]);
    if !tail.is_empty() {
        let _ = tx.send(tail);
    }
}

impl FramePipe {
    fn new(child: Option<Child>, rx: Receiver<Frame>, width: u32, height: u32) -> FramePipe {
        FramePipe {
            child,
            rx,
            next: None,
            ended: false,
            width,
            height,
        }
    }

    /// The newest frame due at `t`, skipping any that are already late. `None` when nothing new
    /// is due yet (keep showing the last one).
    pub fn at(&mut self, t: f64) -> Option<Frame> {
        let mut due = None;
        loop {
            if self.next.is_none() {
                match self.rx.try_recv() {
                    Ok(f) => self.next = Some(f),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.ended = true;
                        break;
                    }
                }
            }
            match &self.next {
                Some(f) if f.pts <= t => due = self.next.take(),
                _ => break,
            }
        }
        due
    }

    /// Every frame has been shown.
    pub fn ended(&self) -> bool {
        self.ended && self.next.is_none()
    }
}

impl Drop for FramePipe {
    fn drop(&mut self) {
        kill(&mut self.child);
    }
}

impl Drop for PcmPipe {
    fn drop(&mut self) {
        kill(&mut self.child);
    }
}

/// Playback position. Sound sets it when there is any, since a jump in audio is heard and a
/// late frame isn't; otherwise it follows the wall clock.
#[derive(Debug, Clone, Default)]
pub struct Clock {
    start: f64,
    t: f64,
    heard: f64,
    stalled: f64,
}

/// How long audio may stop advancing before it counts as over and the wall clock takes over.
const STALL: f64 = 0.25;

impl Clock {
    pub fn at(start: f64) -> Clock {
        Clock {
            start,
            t: start,
            ..Clock::default()
        }
    }

    pub fn now(&self) -> f64 {
        self.t
    }

    /// `dt` is wall time since the last tick while playing; `audio` is seconds of sound played
    /// since `start`, if the clip has sound.
    pub fn tick(&mut self, dt: f64, audio: Option<f64>) -> f64 {
        match audio {
            None => self.t += dt,
            Some(a) if a > self.heard => {
                self.heard = a;
                self.stalled = 0.0;
                self.t = self.t.max(self.start + a);
            }
            Some(_) => {
                self.stalled += dt;
                if self.stalled > STALL {
                    self.t += dt;
                }
            }
        }
        self.t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipe(pts: &[f64], close: bool) -> (FramePipe, Option<SyncSender<Frame>>) {
        let (tx, rx) = sync_channel(16);
        for &p in pts {
            tx.send(Frame {
                pts: p,
                rgba: vec![],
            })
            .unwrap();
        }
        let tx = if close { None } else { Some(tx) };
        (FramePipe::new(None, rx, 2, 2), tx)
    }

    #[test]
    fn shows_the_newest_due_frame() {
        let (mut p, _tx) = pipe(&[0.0, 0.04, 0.08, 0.12], false);
        assert_eq!(p.at(0.0).map(|f| f.pts), Some(0.0));
        assert!(p.at(0.02).is_none());
        // Late: 0.04 is skipped.
        assert_eq!(p.at(0.09).map(|f| f.pts), Some(0.08));
        assert!(!p.ended());
        assert_eq!(p.at(1.0).map(|f| f.pts), Some(0.12));
        assert!(!p.ended(), "ffmpeg may still be decoding");
    }

    #[test]
    fn ends_after_the_last_frame() {
        let (mut p, _) = pipe(&[5.0, 5.04], true);
        assert!(p.at(4.9).is_none());
        assert!(!p.ended());
        p.at(6.0);
        assert!(p.at(6.1).is_none());
        assert!(p.ended());
    }

    #[test]
    fn wall_clock_without_sound() {
        let mut c = Clock::at(2.0);
        c.tick(0.5, None);
        assert_eq!(c.tick(0.25, None), 2.75);
    }

    #[test]
    fn sound_drives_the_clock() {
        let mut c = Clock::at(1.0);
        // The wall says 0.1 but only 0.05 was heard: the picture waits for the sound.
        assert_eq!(c.tick(0.1, Some(0.05)), 1.05);
        assert_eq!(c.tick(0.01, Some(0.2)), 1.2);
        // A short gap holds the picture.
        assert_eq!(c.tick(0.1, Some(0.2)), 1.2);
        assert_eq!(c.tick(0.1, Some(0.2)), 1.2);
        // Past the stall limit the sound is over and the wall clock carries on.
        assert!((c.tick(0.1, Some(0.2)) - 1.3).abs() < 1e-9);
        // Sound coming back never moves the picture backwards.
        assert!((c.tick(0.0, Some(0.25)) - 1.3).abs() < 1e-9);
    }

    #[test]
    fn preview_fits() {
        assert_eq!(preview_size(1080, 1920, 640), (360, 640));
        assert_eq!(preview_size(320, 240, 640), (320, 240));
        assert_eq!(preview_size(1001, 563, 500), (500, 282));
    }
}
