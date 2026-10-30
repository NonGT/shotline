use std::path::Path;

use anyhow::{Result, bail};
use shotline_core::ledger::money;
use shotline_core::{MatchAt, Project};
use shotline_media::Ffmpeg;
use shotline_media::render::{self, Clip, Options, Progress, Track};

pub fn show(p: &Project) -> Result<()> {
    let tl = p.timeline()?;
    let cut = p.cut(&tl)?;
    if cut.is_empty() {
        println!("no shots yet");
        return Ok(());
    }
    let mut ready = 0.0;
    for (i, c) in cut.iter().enumerate() {
        let secs = p.shot(c.shot)?.duration.unwrap_or(p.config.duration);
        match (&c.take, &c.video) {
            (Some(t), Some(_)) => {
                ready += secs;
                let matched = if c.colormatch(tl.colormatch.at, i == 0) {
                    " · colour matched"
                } else {
                    ""
                };
                println!(
                    "{}  {t}  {}s  {}{matched}",
                    c.shot,
                    money(secs),
                    c.transition
                );
            }
            _ => println!("{}  no take selected", c.shot),
        }
    }
    let n = cut.iter().filter(|c| c.video.is_some()).count();
    println!("{} shots, {n} ready, {}s", cut.len(), money(ready));
    for t in &tl.tracks {
        println!(
            "track  {} at {}s, volume {}",
            t.file,
            money(t.at),
            money(t.volume)
        );
    }
    Ok(())
}

pub fn render(
    p: &Project,
    output: Option<&Path>,
    colormatch: bool,
    xfade: Option<f64>,
) -> Result<()> {
    let tl = p.timeline()?;
    let cut = p.cut(&tl)?;
    if cut.is_empty() {
        bail!("no shots to render");
    }
    let missing: Vec<String> = cut
        .iter()
        .filter(|c| c.video.is_none())
        .map(|c| c.shot.to_string())
        .collect();
    if !missing.is_empty() {
        bail!(
            "no take selected for {}; pick one with `shotline select`",
            missing.join(", ")
        );
    }
    let at = if colormatch {
        tl.colormatch.at
    } else {
        MatchAt::None
    };
    let clips: Vec<Clip> = cut
        .iter()
        .enumerate()
        .map(|(i, c)| Clip {
            path: c.video.clone().expect("checked above"),
            label: c.take.as_ref().map(|t| t.to_string()).unwrap_or_default(),
            colormatch: c.colormatch(at, i == 0),
        })
        .collect();
    let mut tracks = vec![];
    for t in &tl.tracks {
        let path = p.root.join(&t.file);
        if !path.is_file() {
            bail!("timeline.toml: track {} isn't there", t.file);
        }
        tracks.push(Track {
            path,
            at: t.at,
            volume: t.volume,
        });
    }
    let opts = Options {
        xfade: xfade.unwrap_or(tl.xfade),
        match_frames: tl.colormatch.frames,
        strength: tl.colormatch.strength,
        ..Options::default()
    };
    let out = match output {
        Some(o) => std::env::current_dir()?.join(o),
        None => p
            .root
            .join("renders")
            .join(format!("{}.mp4", p.clock.now().strftime("%Y%m%d-%H%M%S"))),
    };
    let ff = Ffmpeg::find()?;
    let work = p.root.join(".shotline").join("cache").join("render");
    let result = render::render(&ff, &clips, &tracks, &opts, &work, &out, |pr| match pr {
        Progress::Clip {
            index,
            of,
            label,
            matched,
        } => {
            let m = if matched { "  colour matched" } else { "" };
            println!("[{}/{of}] {label}{m}", index + 1);
        }
        Progress::Encoding => println!("encoding"),
    });
    let _ = std::fs::remove_dir_all(&work);
    let r = result?;
    println!(
        "{} · {}s · {}x{}",
        p.rel(&out),
        money(r.duration),
        r.width,
        r.height
    );
    Ok(())
}
