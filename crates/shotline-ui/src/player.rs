//! Plays takes and the cut inside the app: ffmpeg decodes preview-size frames and samples into
//! pipes (see `shotline_media::play`), sound goes out through cpal and its clock picks the frame.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use eframe::egui;
use shotline_media::play::{Clock, Frame, FramePipe, PcmPipe, preview_size};
use shotline_media::{Ffmpeg, Info};

/// Longest side of the preview, in pixels.
const PREVIEW: u32 = 720;
/// The strip shown while the slider is dragged: small, and this many frames a second.
const STRIP: u32 = 240;
const STRIP_FPS: f64 = 8.0;

/// Each clip's strip, filled in the background; empty until it's there.
type Strips = Arc<Mutex<Vec<(u32, u32, Vec<Frame>)>>>;

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

/// Plays a sound on its own from `from` seconds, e.g. a recorded line; it stops when what's
/// returned is dropped.
pub fn listen(
    ff: &Ffmpeg,
    audio: &Audio,
    path: &std::path::Path,
    from: f64,
) -> anyhow::Result<PcmPipe> {
    let (pipe, rx) = ff.pcm_pipe(path, from, audio.rate, audio.channels)?;
    audio.attach(Some(rx));
    audio.set_playing(true);
    Ok(pipe)
}

pub fn hush(audio: &Audio) {
    audio.set_playing(false);
    audio.attach(None);
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
    strips: Strips,
    /// Where the slider is while it's dragged.
    scrubbing: Option<f64>,
    /// Sought, and its first frame not shown yet.
    waiting: bool,
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
        let strips: Strips = Arc::default();
        {
            let (ff, strips) = (ff.clone(), strips.clone());
            let wanted: Vec<(PathBuf, u32, u32)> = clips
                .iter()
                .map(|c| {
                    let (w, h) = preview_size(c.info.width, c.info.height, STRIP);
                    (c.path.clone(), w, h)
                })
                .collect();
            std::thread::spawn(move || {
                for (path, w, h) in wanted {
                    let frames = ff.strip(&path, w, h, STRIP_FPS).unwrap_or_default();
                    strips.lock().unwrap().push((w, h, frames));
                }
            });
        }
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
            strips,
            scrubbing: None,
            waiting: false,
        })
    }

    pub fn duration(&self) -> f64 {
        self.clips.last().map_or(0.0, |c| c.at + c.info.duration)
    }

    pub fn position(&self) -> f64 {
        self.scrubbing
            .unwrap_or_else(|| self.clips[self.index].at + self.clock.now())
    }

    /// Shows the strip's frame nearest `t` right away, while the slider is dragged; `seek`
    /// brings the sharp one once it's let go.
    pub fn scrub(&mut self, ctx: &egui::Context, t: f64) {
        let t = t.clamp(0.0, self.duration());
        self.scrubbing = Some(t);
        let i = self.clips.iter().rposition(|c| c.at <= t).unwrap_or(0);
        let local = t - self.clips[i].at;
        let strips = self.strips.lock().unwrap();
        let Some((w, h, frames)) = strips.get(i) else {
            return;
        };
        let n = ((local * STRIP_FPS).round() as usize).min(frames.len().saturating_sub(1));
        let Some(f) = frames.get(n) else {
            return;
        };
        let image = egui::ColorImage::from_rgba_unmultiplied([*w as usize, *h as usize], &f.rgba);
        match &mut self.tex {
            Some(tex) => tex.set(image, Default::default()),
            None => self.tex = Some(ctx.load_texture(&self.label, image, Default::default())),
        }
    }

    /// Frames a second of the first clip.
    pub fn fps(&self) -> f64 {
        match self.clips[0].info.fps_value() {
            f if f > 0.0 => f,
            _ => 24.0,
        }
    }

    /// The first clip's picture, in pixels.
    pub fn size(&self) -> (u32, u32) {
        let i = &self.clips[0].info;
        (i.width, i.height)
    }

    pub fn aspect(&self) -> f32 {
        let i = &self.clips[0].info;
        i.width.max(1) as f32 / i.height.max(1) as f32
    }

    /// Restarts decoding at `t` seconds into the playlist.
    pub fn seek(&mut self, t: f64, audio: Option<&Audio>) {
        let t = t.clamp(0.0, self.duration());
        self.scrubbing = None;
        self.waiting = true;
        self.index = self
            .clips
            .iter()
            .rposition(|c| c.at <= t)
            .unwrap_or(0)
            .min(self.clips.len() - 1);
        let clip = &self.clips[self.index];
        let (w, h) = preview_size(clip.info.width, clip.info.height, PREVIEW);
        let fps = match clip.info.fps_value() {
            f if f > 0.0 => f,
            _ => 24.0,
        };
        // Decoding from the very end gives no frame at all, so start on the last one.
        let local = (t - clip.at).min(clip.info.duration - 1.0 / fps).max(0.0);
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
        if self.scrubbing.is_some() {
            self.last = None;
            return true;
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
            self.waiting = false;
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
    /// Without its title and close button, for a popup that has its own.
    bare: bool,
    max_height: f32,
    /// Answers Space; off for one under a popup.
    pub keys: bool,
    /// What it last told the sound output, so a paused deck leaves a line's playback alone.
    sounding: Option<bool>,
    /// Has a close button; one that's always there doesn't.
    closable: bool,
}

/// `HH:MM:SS:FF`, as editors write it.
pub(crate) fn timecode(t: f64, fps: f64) -> String {
    let fps = fps.round().max(1.0) as u64;
    let frames = (t.max(0.0) * fps as f64).round() as u64;
    let (f, s) = (frames % fps, frames / fps);
    format!("{:02}:{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60, f)
}

pub(crate) fn clock(t: f64) -> String {
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
            bare: false,
            max_height: 420.0,
            keys: true,
            sounding: None,
            closable: true,
        }
    }

    /// Stopped on its first frame and always there: no close button.
    pub fn fixed(mut self) -> Deck {
        self.playing = false;
        self.closable = false;
        self
    }

    /// Shows the frame nearest `t` from the strips, while something is dragged; `seek` once
    /// it's let go.
    pub fn scrub(&mut self, ctx: &egui::Context, t: f64) {
        for p in &mut self.players {
            p.scrub(ctx, t);
        }
    }

    /// Stopped on the first frame, no title, up to `max_height` tall: for a popup.
    pub fn in_popup(mut self, max_height: f32) -> Deck {
        self.playing = false;
        self.bare = true;
        self.max_height = max_height;
        self
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

    /// Back to the first player alone.
    pub fn alone(&mut self) {
        self.players.truncate(1);
        self.title = self.players[0].label.clone();
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn position(&self) -> f64 {
        self.players[0].position()
    }

    pub fn pause(&mut self, audio: Option<&Audio>) {
        self.playing = false;
        self.sounding = Some(false);
        if let Some(a) = audio {
            a.set_playing(false);
        }
    }

    pub fn seek(&mut self, t: f64, audio: Option<&Audio>) {
        for p in &mut self.players {
            p.seek(t, audio);
        }
    }

    /// Returns false when the deck should be closed.
    pub fn show(&mut self, ui: &mut egui::Ui, audio: Option<&Audio>) -> bool {
        self.advance(ui.ctx(), audio);
        let open = self.show_parts(ui, audio);
        self.repaint(ui.ctx());
        if !open {
            self.pause(audio);
        }
        open
    }

    /// The Cut page's monitor: the picture on black in a `height` tall screen, and under it
    /// Play and the timecode. No slider: the timeline's playhead does that.
    pub fn monitor(&mut self, ui: &mut egui::Ui, audio: Option<&Audio>, height: f32) {
        self.advance(ui.ctx(), audio);
        let width = ui.available_width();
        let (screen, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
        ui.painter().rect_filled(screen, 4.0, egui::Color32::BLACK);
        let p = &self.players[0];
        let aspect = p.aspect().max(0.1);
        let (mut w, mut h) = (height * aspect, height);
        if w > width {
            (w, h) = (width, width / aspect);
        }
        let picture = egui::Rect::from_center_size(screen.center(), egui::vec2(w, h));
        if let Some(t) = &p.tex {
            ui.put(
                picture,
                egui::Image::new((t.id(), picture.size())).alt_text(&p.label),
            );
        }
        if let Some(e) = &p.error {
            ui.painter().text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                e,
                egui::FontId::proportional(13.0),
                ui.visuals().error_fg_color,
            );
        }
        let (t, total, fps) = (p.position(), p.duration(), p.fps());
        ui.add_space(6.0);
        let (bar, _) = ui.allocate_exact_size(egui::vec2(width, 30.0), egui::Sense::hover());
        let mut row = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(bar)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        // Centred: the button and the two timecodes, about 300 px.
        row.add_space(((width - 300.0) / 2.0).max(0.0));
        self.play_button(&mut row, audio);
        row.add_space(10.0);
        row.label(
            egui::RichText::new(timecode(t, fps))
                .monospace()
                .size(15.0)
                .color(crate::theme::PAPER),
        );
        row.label(
            egui::RichText::new(format!("/ {}", timecode(total, fps)))
                .monospace()
                .size(15.0)
                .color(crate::theme::DUST),
        );
        self.repaint(ui.ctx());
    }

    /// Moves the clocks on and keeps the sound in step.
    fn advance(&mut self, ctx: &egui::Context, audio: Option<&Audio>) {
        let now = Instant::now();
        // Quiet while the slider is dragged.
        let scrubbing = self.players.iter().any(|p| p.scrubbing.is_some());
        let sound = self.playing && !scrubbing;
        if let Some(a) = audio
            && self.sounding != Some(sound)
        {
            a.set_playing(sound);
            self.sounding = Some(sound);
        }
        let mut running = false;
        for p in &mut self.players {
            running |= p.update(ctx, audio, self.playing, now);
        }
        if self.playing && !running {
            if self.looping {
                self.seek(0.0, audio);
            } else {
                self.pause(audio);
            }
        }
    }

    /// Play or Pause, and Space for it.
    fn play_button(&mut self, ui: &mut egui::Ui, audio: Option<&Audio>) {
        let label = if self.playing { "Pause" } else { "Play" };
        // Space is a space while something's being typed.
        let key = self.keys
            && ui.memory(|m| m.focused().is_none())
            && ui.input(|i| i.key_pressed(egui::Key::Space));
        let icon: fn(&egui::Painter, egui::Rect, egui::Stroke) = if self.playing {
            crate::pause_icon
        } else {
            crate::play_icon
        };
        if crate::icon_button(ui, label, icon)
            .on_hover_text(format!("{} (Space)", label.to_lowercase()))
            .clicked()
            || key
        {
            self.playing = !self.playing;
            if self.playing {
                // Sound attaches to a pipe when it's started.
                let t = self.players[0].position();
                self.seek(t, audio);
            }
        }
    }

    fn repaint(&self, ctx: &egui::Context) {
        if self.playing {
            ctx.request_repaint();
        } else if self.players.iter().any(|p| p.tex.is_none() || p.waiting) {
            // Still waiting for the first frame, or the first after a seek.
            ctx.request_repaint_after(std::time::Duration::from_millis(15));
        }
    }

    fn show_parts(&mut self, ui: &mut egui::Ui, audio: Option<&Audio>) -> bool {
        let mut open = true;
        if !self.bare {
            ui.horizontal(|ui| {
                ui.strong(&self.title);
                if self.closable && ui.button("Close player").clicked() {
                    open = false;
                }
            });
        }
        let height = (ui.available_width() / self.players.len() as f32 / self.players[0].aspect())
            .min(self.max_height);
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
            self.play_button(ui, audio);
            let text = format!("{} / {}", clock(t), clock(total));
            ui.spacing_mut().slider_width = (ui.available_width() - 120.0).max(100.0);
            let slider = ui.add(
                egui::Slider::new(&mut t, 0.0..=total.max(0.01))
                    .show_value(false)
                    .text(text),
            );
            if slider.dragged() && slider.changed() {
                for p in &mut self.players {
                    p.scrub(ui.ctx(), t);
                }
            }
            if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                self.seek(t, audio);
            }
        });
        open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_frame_comes_sharp_after_a_scrub() {
        let Ok(ff) = Ffmpeg::find() else { return };
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("a.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i"])
            .arg("testsrc=size=640x360:rate=24:duration=2")
            .arg(&clip)
            .status()
            .unwrap();
        assert!(ok.success());
        let ctx = egui::Context::default();
        let mut p = Player::new(&ff, "a", &[clip]).unwrap();
        let start = Instant::now();
        while p.strips.lock().unwrap().is_empty() {
            assert!(start.elapsed().as_secs() < 10);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let end = p.duration();
        p.scrub(&ctx, end);
        assert_eq!(p.tex.as_ref().unwrap().size()[0], 240);
        p.seek(end, None);
        let start = Instant::now();
        while p.tex.as_ref().unwrap().size()[0] != 640 {
            assert!(start.elapsed().as_secs() < 5, "still the strip's frame");
            p.update(&ctx, None, false, Instant::now());
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

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
