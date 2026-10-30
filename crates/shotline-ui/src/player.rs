//! Plays takes and the cut inside the app: ffmpeg decodes preview-size frames and samples into
//! pipes (see `shotline_media::play`), sound goes out through cpal and its clock picks the frame.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use eframe::egui;
use shotline_media::play::{Clock, FramePipe, PcmPipe, preview_size};
use shotline_media::{Ffmpeg, Info};

/// Longest side of the preview, in pixels.
const PREVIEW: u32 = 720;

#[derive(Default)]
struct Shared {
    rx: Option<Receiver<Vec<f32>>>,
    buf: Vec<f32>,
    pos: usize,
    /// Samples that reached the device since the last attach.
    played: u64,
    playing: bool,
}

/// The sound output. One for the whole app; whichever player has the sound feeds it.
pub struct Audio {
    _stream: cpal::Stream,
    shared: Arc<Mutex<Shared>>,
    pub rate: u32,
    pub channels: u16,
}

fn fill<T: SizedSample + FromSample<f32>>(s: &mut Shared, out: &mut [T]) {
    let mut i = 0;
    while s.playing && i < out.len() {
        if s.pos >= s.buf.len() {
            match s.rx.as_ref().map(|r| r.try_recv()) {
                Some(Ok(chunk)) => {
                    s.buf = chunk;
                    s.pos = 0;
                }
                _ => break,
            }
        }
        let n = (out.len() - i).min(s.buf.len() - s.pos);
        for (o, v) in out[i..i + n].iter_mut().zip(&s.buf[s.pos..s.pos + n]) {
            *o = T::from_sample(*v);
        }
        s.pos += n;
        s.played += n as u64;
        i += n;
    }
    for o in &mut out[i..] {
        *o = T::EQUILIBRIUM;
    }
}

fn stream<T: SizedSample + FromSample<f32>>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    shared: Arc<Mutex<Shared>>,
) -> Option<cpal::Stream> {
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
                if let Ok(mut s) = shared.lock() {
                    fill(&mut s, out);
                }
            },
            |_| {},
            None,
        )
        .ok()
}

impl Audio {
    /// `None` without a usable output device; players then run silent on the wall clock.
    pub fn open() -> Option<Audio> {
        let device = cpal::default_host().default_output_device()?;
        let config = device.default_output_config().ok()?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let format = config.sample_format();
        let config: cpal::StreamConfig = config.into();
        let (rate, channels) = (config.sample_rate, config.channels);
        let s = shared.clone();
        let stream = match format {
            SampleFormat::F32 => stream::<f32>(&device, config, s),
            SampleFormat::I16 => stream::<i16>(&device, config, s),
            SampleFormat::U16 => stream::<u16>(&device, config, s),
            SampleFormat::I32 => stream::<i32>(&device, config, s),
            _ => None,
        }?;
        stream.play().ok()?;
        Some(Audio {
            _stream: stream,
            shared,
            rate,
            channels,
        })
    }

    fn attach(&self, rx: Option<Receiver<Vec<f32>>>) {
        let mut s = self.shared.lock().unwrap();
        *s = Shared {
            rx,
            playing: s.playing,
            ..Shared::default()
        };
    }

    fn set_playing(&self, on: bool) {
        self.shared.lock().unwrap().playing = on;
    }

    fn heard(&self) -> f64 {
        let played = self.shared.lock().unwrap().played;
        played as f64 / self.channels.max(1) as f64 / self.rate.max(1) as f64
    }
}

struct Clip {
    path: PathBuf,
    info: Info,
    /// Where it starts in the playlist.
    at: f64,
}

/// One picture: a take, or the cut as a playlist of takes.
pub struct Player {
    ff: Ffmpeg,
    clips: Vec<Clip>,
    pub label: String,
    index: usize,
    frames: Option<FramePipe>,
    pcm: Option<PcmPipe>,
    clock: Clock,
    /// Plays the sound; the others in a deck are silent and follow the wall clock.
    pub sound: bool,
    last: Option<Instant>,
    tex: Option<egui::TextureHandle>,
    pub error: Option<String>,
}

impl Player {
    pub fn new(ff: &Ffmpeg, label: &str, files: &[PathBuf]) -> anyhow::Result<Player> {
        let mut clips = vec![];
        let mut at = 0.0;
        for path in files {
            let info = ff.probe(path)?;
            let d = info.duration;
            clips.push(Clip {
                path: path.clone(),
                info,
                at,
            });
            at += d;
        }
        anyhow::ensure!(!clips.is_empty(), "nothing to play");
        Ok(Player {
            ff: ff.clone(),
            clips,
            label: label.into(),
            index: 0,
            frames: None,
            pcm: None,
            clock: Clock::at(0.0),
            sound: false,
            last: None,
            tex: None,
            error: None,
        })
    }

    pub fn duration(&self) -> f64 {
        self.clips.last().map_or(0.0, |c| c.at + c.info.duration)
    }

    pub fn position(&self) -> f64 {
        self.clips[self.index].at + self.clock.now()
    }

    pub fn aspect(&self) -> f32 {
        let i = &self.clips[0].info;
        i.width.max(1) as f32 / i.height.max(1) as f32
    }

    /// Restarts decoding at `t` seconds into the playlist.
    pub fn seek(&mut self, t: f64, audio: Option<&Audio>) {
        let t = t.clamp(0.0, self.duration());
        self.index = self
            .clips
            .iter()
            .rposition(|c| c.at <= t)
            .unwrap_or(0)
            .min(self.clips.len() - 1);
        let clip = &self.clips[self.index];
        let local = (t - clip.at).max(0.0);
        let (w, h) = preview_size(clip.info.width, clip.info.height, PREVIEW);
        let fps = match clip.info.fps_value() {
            f if f > 0.0 => f,
            _ => 24.0,
        };
        self.frames = None;
        self.pcm = None;
        self.clock = Clock::at(local);
        match self.ff.frame_pipe(&clip.path, local, w, h, fps) {
            Ok(p) => self.frames = Some(p),
            Err(e) => self.error = Some(e.to_string()),
        }
        if self.sound
            && let Some(a) = audio
        {
            let rx = if clip.info.audio {
                match self.ff.pcm_pipe(&clip.path, local, a.rate, a.channels) {
                    Ok((p, rx)) => {
                        self.pcm = Some(p);
                        Some(rx)
                    }
                    Err(_) => None,
                }
            } else {
                None
            };
            a.attach(rx);
        }
    }

    fn audible(&self, audio: Option<&Audio>) -> Option<f64> {
        let a = audio?;
        (self.sound && self.pcm.is_some()).then(|| a.heard())
    }

    /// Moves the clock and shows the frame that's due. Returns false once the playlist is over.
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        audio: Option<&Audio>,
        playing: bool,
        now: Instant,
    ) -> bool {
        if self.frames.is_none() && self.error.is_none() {
            self.seek(0.0, audio);
        }
        let t = if playing {
            let dt = self.last.map_or(0.0, |l| (now - l).as_secs_f64());
            self.clock.tick(dt, self.audible(audio))
        } else {
            self.clock.now()
        };
        self.last = playing.then_some(now);
        let Some(frames) = self.frames.as_mut() else {
            return false;
        };
        if let Some(f) = frames.at(t) {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [frames.width as usize, frames.height as usize],
                &f.rgba,
            );
            match &mut self.tex {
                Some(tex) => tex.set(image, Default::default()),
                None => self.tex = Some(ctx.load_texture(&self.label, image, Default::default())),
            }
        }
        if playing && frames.ended() {
            if self.index + 1 < self.clips.len() {
                let next = self.clips[self.index + 1].at;
                self.seek(next, audio);
            } else {
                return false;
            }
        }
        true
    }

    pub fn show(&self, ui: &mut egui::Ui, height: f32) {
        let size = egui::vec2(height * self.aspect(), height);
        match &self.tex {
            Some(t) => {
                ui.add(egui::Image::new((t.id(), size)).alt_text(&self.label));
            }
            None => {
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
            }
        }
    }
}

/// One or more players under shared controls: a take, two takes side by side, or the cut.
pub struct Deck {
    pub players: Vec<Player>,
    playing: bool,
    /// The cut doesn't loop; single takes and comparisons do, to watch them again.
    looping: bool,
    title: String,
}

fn clock(t: f64) -> String {
    format!("{}:{:04.1}", (t / 60.0) as u32, t % 60.0)
}

impl Deck {
    pub fn new(title: &str, mut first: Player, looping: bool) -> Deck {
        first.sound = true;
        Deck {
            players: vec![first],
            playing: true,
            looping,
            title: title.into(),
        }
    }

    /// Adds a silent player next to the others and starts everything from the top.
    pub fn compare(&mut self, p: Player, audio: Option<&Audio>) {
        self.players.truncate(1);
        self.players.push(p);
        self.title = self
            .players
            .iter()
            .map(|p| p.label.as_str())
            .collect::<Vec<_>>()
            .join(" vs ");
        self.seek(0.0, audio);
        self.playing = true;
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn pause(&mut self, audio: Option<&Audio>) {
        self.playing = false;
        if let Some(a) = audio {
            a.set_playing(false);
        }
    }

    fn seek(&mut self, t: f64, audio: Option<&Audio>) {
        for p in &mut self.players {
            p.seek(t, audio);
        }
    }

    /// Returns false when the deck should be closed.
    pub fn show(&mut self, ui: &mut egui::Ui, audio: Option<&Audio>) -> bool {
        let now = Instant::now();
        if let Some(a) = audio {
            a.set_playing(self.playing);
        }
        let mut running = false;
        for p in &mut self.players {
            running |= p.update(ui.ctx(), audio, self.playing, now);
        }
        if self.playing && !running {
            if self.looping {
                self.seek(0.0, audio);
            } else {
                self.pause(audio);
            }
        }
        let mut open = true;
        ui.horizontal(|ui| {
            ui.strong(&self.title);
            if ui.button("Close player").clicked() {
                open = false;
            }
        });
        let height = (ui.available_width() / self.players.len() as f32 / self.players[0].aspect())
            .min(420.0);
        ui.horizontal(|ui| {
            for p in &self.players {
                ui.vertical(|ui| {
                    p.show(ui, height);
                    if self.players.len() > 1 {
                        ui.weak(&p.label);
                    }
                    if let Some(e) = &p.error {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                });
            }
        });
        let first = &self.players[0];
        let (mut t, total) = (first.position(), first.duration());
        ui.horizontal(|ui| {
            let label = if self.playing { "Pause" } else { "Play" };
            if ui.button(label).clicked() || ui.input(|i| i.key_pressed(egui::Key::Space)) {
                self.playing = !self.playing;
            }
            let text = format!("{} / {}", clock(t), clock(total));
            ui.spacing_mut().slider_width = (ui.available_width() - 120.0).max(100.0);
            let slider = ui.add(
                egui::Slider::new(&mut t, 0.0..=total.max(0.01))
                    .show_value(false)
                    .text(text),
            );
            if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                self.seek(t, audio);
            }
        });
        if self.playing {
            ui.ctx().request_repaint();
        } else if self.players.iter().any(|p| p.tex.is_none()) {
            // Still waiting for the first frame.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(15));
        }
        if !open {
            self.pause(audio);
        }
        open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a sound device, so it only runs by hand: `cargo test -p shotline-ui -- --ignored`.
    #[test]
    #[ignore]
    fn sound_drives_the_picture() {
        let ff = Ffmpeg::find().unwrap();
        let audio = Audio::open().expect("a sound device");
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("a.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x90:rate=24:duration=2",
            ])
            .args([
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-shortest",
            ])
            .arg(&clip)
            .status()
            .unwrap();
        assert!(ok.success());
        let ctx = egui::Context::default();
        let mut p = Player::new(&ff, "a", &[clip]).unwrap();
        p.sound = true;
        p.seek(0.0, Some(&audio));
        audio.set_playing(true);
        let start = Instant::now();
        while p.update(&ctx, Some(&audio), true, Instant::now()) {
            std::thread::sleep(std::time::Duration::from_millis(10));
            assert!(start.elapsed().as_secs() < 5);
        }
        let heard = audio.heard();
        let took = start.elapsed().as_secs_f64();
        eprintln!("heard {heard:.2}s, took {took:.2}s, at {:.2}", p.position());
        assert!(heard > 1.5, "{heard}");
        assert!((1.8..2.6).contains(&took), "{took}");
    }
}
