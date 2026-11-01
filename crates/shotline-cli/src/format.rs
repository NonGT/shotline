//! The shape, size, length and model every shot is made at.

use anyhow::Result;
use shotline_core::protocol::Op;
use shotline_core::{Format, Project};

use crate::plugins;

#[derive(Default)]
pub struct Change<'a> {
    pub plugin: Option<&'a str>,
    pub model: Option<&'a str>,
    pub aspect: Option<&'a str>,
    pub resolution: Option<&'a str>,
    pub duration: Option<f64>,
}

impl Change<'_> {
    fn is_empty(&self) -> bool {
        self.plugin.is_none()
            && self.model.is_none()
            && self.aspect.is_none()
            && self.resolution.is_none()
            && self.duration.is_none()
    }
}

pub fn run(p: &mut Project, c: &Change) -> Result<()> {
    if c.is_empty() {
        show(&p.format());
        return Ok(());
    }
    let was = p.format();
    let f = apply(p, c)?;
    show(&f);
    for w in check(p, &f)? {
        eprintln!("warning: {w}");
    }
    if f.aspect != was.aspect || f.resolution != was.resolution {
        let made = p.selected_take_ids()?.len();
        if made > 0 {
            eprintln!(
                "warning: {made} shot(s) already have a take, made at {} {}; they stay that way \
                 until shot again",
                was.aspect, was.resolution
            );
        }
    }
    Ok(())
}

/// Sets what `c` says, keeping the rest.
pub fn apply(p: &mut Project, c: &Change) -> Result<Format> {
    let mut f = p.format();
    let set = |v: &mut String, new: Option<&str>| {
        if let Some(n) = new {
            *v = n.to_string();
        }
    };
    set(&mut f.plugin, c.plugin);
    set(&mut f.model, c.model);
    set(&mut f.aspect, c.aspect);
    set(&mut f.resolution, c.resolution);
    if let Some(d) = c.duration {
        f.duration = d;
    }
    // Refused before anything is written: a model nothing provides can't make a shot.
    video_model(p, &f.plugin, &f.model)?;
    p.set_format(&f)?;
    Ok(f)
}

/// Refuses a video model nothing installed provides.
pub fn video_model(p: &Project, plugin: &str, model: &str) -> Result<()> {
    let installed = plugins::load();
    plugins::catalog(p, &installed)?.get(plugin, model, &Op::VideoGenerate)?;
    Ok(())
}

/// What the model says it can't do.
fn check(p: &Project, f: &Format) -> Result<Vec<String>> {
    let installed = plugins::load();
    let catalog = plugins::catalog(p, &installed)?;
    let spec = &catalog.get(&f.plugin, &f.model, &Op::VideoGenerate)?.info;
    let mut out = vec![];
    for (v, list) in [
        (&f.aspect, &spec.aspects),
        (&f.resolution, &spec.resolutions),
    ] {
        if !list.is_empty() && !list.contains(v) {
            out.push(format!("{} doesn't do {v} ({})", f.model, list.join(", ")));
        }
    }
    Ok(out)
}

fn show(f: &Format) {
    println!("model       {} {}", f.plugin, f.model);
    println!("shape       {}", f.aspect);
    println!("resolution  {}", f.resolution);
    println!("duration    {} s", f.duration);
}
