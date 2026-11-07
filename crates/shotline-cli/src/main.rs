use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use shotline_core::ledger::report::By;
use shotline_core::{
    Draft, Git, Kind, ModelOverride, Project, Shot, ShotRef, Transition, Voice, build,
};

mod cost;
mod draft;
mod fetch;
mod format;
mod image;
mod mcp;
mod plugins;
mod shoot;
mod timeline;

#[derive(Parser)]
#[command(
    name = "shotline",
    version,
    about = "Plan and shoot AI films shot by shot"
)]
struct Cli {
    /// Project folder (default: the one containing the current directory)
    #[arg(short = 'C', long, global = true)]
    project: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new project folder
    Init {
        dir: PathBuf,
        #[arg(long)]
        name: Option<String>,
        /// Don't start a git repository for the film
        #[arg(long)]
        no_git: bool,
        /// Shape of every shot, e.g. 9:16 (vertical, the default) or 16:9
        #[arg(long)]
        aspect: Option<String>,
        /// e.g. 768P (the default) or 1080P, as the video model names it
        #[arg(long)]
        resolution: Option<String>,
    },
    /// Show the shape, resolution, length and video model every shot is made at, or set them
    Format {
        #[arg(long)]
        aspect: Option<String>,
        #[arg(long)]
        resolution: Option<String>,
        /// Seconds, for shots that don't set their own
        #[arg(long)]
        duration: Option<f64>,
        #[arg(long)]
        plugin: Option<String>,
        #[arg(long)]
        model: Option<String>,
    },
    /// Cast, sets and props
    #[command(subcommand)]
    Element(ElementCmd),
    /// Show story.md, or replace it
    Story {
        #[arg(long)]
        set: Option<String>,
    },
    /// Show style.md, or replace it
    Style {
        #[arg(long)]
        set: Option<String>,
    },
    /// Chapters group shots, numbered 01, 02, ...
    #[command(subcommand)]
    Chapter(ChapterCmd),
    /// Shots, numbered 010, 020, ... within a chapter
    #[command(subcommand)]
    Shot(ShotCmd),
    /// Show the exact prompt and references a shot would send, without spending anything
    Prompt { shot: ShotRef },
    /// Give a shot a storyboard frame to start on: made by the image model, or --file, or --clear
    Frame {
        shot: ShotRef,
        #[arg(long, conflicts_with = "clear")]
        file: Option<PathBuf>,
        #[arg(long)]
        clear: bool,
        /// An image to go along instead of the shot's references, a file or an http(s) URL
        #[arg(long = "ref", conflicts_with_all = ["file", "clear", "no_refs"])]
        refs: Vec<String>,
        /// Send no reference images at all
        #[arg(long, conflicts_with_all = ["file", "clear"])]
        no_refs: bool,
        /// e.g. 1:1 or 9:16, instead of the shot's
        #[arg(long, conflicts_with_all = ["file", "clear"])]
        aspect: Option<String>,
        /// e.g. 1K or 2K, for models that have a choice
        #[arg(long, conflicts_with_all = ["file", "clear"])]
        resolution: Option<String>,
        /// Put the frame prompt into English with the text model first, if it isn't
        #[arg(long, conflicts_with_all = ["file", "clear"])]
        translate: bool,
        #[command(flatten)]
        image: ImageArgs,
    },
    /// Price a shot (01-010) or a whole chapter (1) without spending anything
    Quote { target: String },
    /// Quote, ask, then generate a take of a shot
    Take {
        shot: ShotRef,
        /// Don't ask before spending
        #[arg(long)]
        yes: bool,
        /// Reuse the steps before this one from an earlier take and run the rest
        #[arg(long)]
        from_step: Option<usize>,
        /// Which take to reuse from (default: the latest)
        #[arg(long, requires = "from_step")]
        of: Option<String>,
        /// Put the prompt into English with the text model first, if it isn't; kept with the
        /// shot for the takes after
        #[arg(long)]
        translate: bool,
    },
    /// Run one operation outside any shot, e.g. `run video.upscale --plugin p --model m --input video=a.mp4`
    Run {
        op: shotline_core::protocol::Op,
        #[arg(long)]
        plugin: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        duration: Option<f64>,
        /// Named input file: --input video=take.mp4
        #[arg(long = "input", value_parser = key_path)]
        inputs: Vec<(String, PathBuf)>,
        /// Reference file (image, audio or video)
        #[arg(long = "ref")]
        refs: Vec<PathBuf>,
        /// Plugin parameter: --param seed=7 (JSON values are parsed)
        #[arg(long = "param", value_parser = key_value)]
        params: Vec<(String, String)>,
        #[arg(long)]
        yes: bool,
    },
    /// List a shot's takes
    Takes { shot: ShotRef },
    /// Pick the take a shot uses: `select 01-010 3` or `select 01-010#3`
    Select { shot: String, take: Option<String> },
    /// A new take put together from pieces of others, made here for nothing:
    /// `splice 01-010 2:0-3.2 fade:4:2.1-6` (take:from-to, in seconds, joined by a cut unless
    /// it starts with fade: or smooth:)
    Splice {
        shot: ShotRef,
        #[arg(required = true)]
        pieces: Vec<String>,
        /// Seconds a fade lasts
        #[arg(long, default_value_t = 0.3)]
        fade: f64,
        /// Leave each piece's colours as they are
        #[arg(long)]
        no_colormatch: bool,
    },
    /// A new take from a video made somewhere else, e.g. cut in an editor
    Import { shot: ShotRef, file: PathBuf },
    /// Installed plugins
    #[command(subcommand)]
    Plugin(PluginCmd),
    /// What the film has cost
    #[command(args_conflicts_with_subcommands = true)]
    Cost {
        #[command(subcommand)]
        cmd: Option<CostCmd>,
        /// chapter, shot, take, op, plugin or day
        #[arg(long, default_value = "shot")]
        by: By,
        #[arg(long)]
        csv: bool,
    },
    /// The cut: shots in order with their selected takes
    Timeline {
        #[command(subcommand)]
        cmd: Option<TimelineCmd>,
    },
    /// Serve the commands as MCP tools over stdio, for an agent to drive
    Mcp,
    /// Open the desktop app on the film
    Ui { dir: Option<PathBuf> },
    /// Serves the built-in fake plugin over stdio, for tests and dry runs
    #[command(name = "__fake-plugin", hide = true)]
    FakePlugin,
}

#[derive(clap::Args)]
struct ImageArgs {
    /// Image plugin instead of [image] in shotline.toml
    #[arg(long)]
    plugin: Option<String>,
    /// Image model instead of [image] in shotline.toml
    #[arg(long)]
    model: Option<String>,
    /// Don't ask before spending
    #[arg(long)]
    yes: bool,
    /// Keep only faces and hair from the reference images, so the clothes can change
    #[arg(long)]
    any_outfit: bool,
}

impl ImageArgs {
    fn make<'a>(&'a self, prompt: &'a str) -> image::Make<'a> {
        image::Make {
            prompt,
            plugin: self.plugin.as_deref(),
            model: self.model.as_deref(),
            yes: self.yes,
            at: None,
            refs: None,
            aspect: None,
            resolution: None,
            translate: false,
            outfit: !self.any_outfit,
        }
    }
}

#[derive(clap::Args)]
struct TextArgs {
    /// Text plugin instead of [text] in shotline.toml
    #[arg(long)]
    plugin: Option<String>,
    /// Text model instead of [text] in shotline.toml
    #[arg(long)]
    model: Option<String>,
    /// Save the draft without asking
    #[arg(long)]
    write: bool,
    /// Don't ask before spending (nor whether to save, without --write)
    #[arg(long)]
    yes: bool,
}

impl TextArgs {
    fn ask(&self) -> draft::Ask<'_> {
        draft::Ask {
            plugin: self.plugin.as_deref(),
            model: self.model.as_deref(),
            yes: self.yes,
            write: self.write,
        }
    }
}

#[derive(Subcommand)]
enum CostCmd {
    /// Check nobody edited the ledger
    Verify,
    /// Every rate in use and where it comes from
    Rates,
    /// Compare the ledger with what providers say they charged and how their balances moved
    Reconcile {
        #[arg(long)]
        plugin: Option<String>,
    },
    /// Record a correction, e.g. a fee or promo credit (negative)
    Adjust {
        #[arg(allow_hyphen_values = true)]
        amount: f64,
        unit: String,
        #[arg(long)]
        note: String,
    },
    /// Money held for jobs that haven't ended; with a job, let go of it
    Release {
        /// The job, or the start of its id; leave out to list what's held
        job: Option<String>,
        #[arg(long, default_value = "")]
        note: String,
        /// The job isn't running any more, whatever its job.toml says
        #[arg(long)]
        sure: bool,
    },
    /// Record money back for a job
    Refund {
        job: String,
        amount: f64,
        unit: String,
        #[arg(long, default_value = "")]
        note: String,
    },
}

#[derive(Subcommand)]
enum TimelineCmd {
    /// Join the selected takes into one video with ffmpeg
    Render {
        /// Where to write it (default: renders/<time>.mp4)
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Skip colour matching
        #[arg(long)]
        no_colormatch: bool,
        /// Crossfade at each join in seconds, instead of timeline.toml's
        #[arg(long)]
        xfade: Option<f64>,
    },
}

#[derive(Subcommand)]
enum PluginCmd {
    Ls,
    /// Check a plugin talks the protocol properly. Never spends anything.
    Doctor {
        name: String,
    },
    /// What's left on each provider account
    Balance {
        name: Option<String>,
    },
    /// The voices a text to speech plugin can use, for `element voice --voice-id`
    Voices {
        name: String,
    },
    /// Always use this program for the plugin, whatever PATH says (default: the one in use)
    Pin {
        name: String,
        path: Option<PathBuf>,
    },
    /// Undo `pin`; PATH decides again
    Unpin {
        name: String,
    },
    /// Set one of a model's options for the whole film; without a value, take it out
    Option {
        plugin: String,
        model: String,
        key: String,
        /// JSON (true, 3, "x") or plain text
        value: Option<String>,
    },
}

#[derive(Subcommand)]
enum ElementCmd {
    Add {
        /// cast, set or prop
        kind: Kind,
        name: String,
        #[arg(long)]
        handle: Option<String>,
        #[arg(long, default_value = "")]
        desc: String,
    },
    /// Add an anchor image to an element, from a file or made by the image model
    Anchor {
        handle: String,
        #[arg(
            long,
            required_unless_present = "generate",
            conflicts_with = "generate"
        )]
        file: Option<PathBuf>,
        #[arg(long)]
        generate: bool,
        /// What to add to the element's description, e.g. "side view"
        #[arg(long, default_value = "", requires = "generate")]
        prompt: String,
        /// Another image to go along, a file or an http(s) URL; named Image 2, Image 3… after
        /// the element's own anchors
        #[arg(long = "ref", requires = "generate")]
        refs: Vec<String>,
        /// Leave the element's own anchors out
        #[arg(long, requires = "generate")]
        no_anchors: bool,
        /// e.g. 1:1 or 9:16, instead of [image] in shotline.toml
        #[arg(long, requires = "generate")]
        aspect: Option<String>,
        /// e.g. 1K or 2K, for models that have a choice
        #[arg(long, requires = "generate")]
        resolution: Option<String>,
        /// Put the prompt into English with the text model first, if it isn't (a second,
        /// small spend)
        #[arg(long, requires = "generate")]
        translate: bool,
        #[command(flatten)]
        image: ImageArgs,
    },
    /// Translate an element's description into English with the text model
    Draft {
        handle: String,
        #[command(flatten)]
        text: TextArgs,
    },
    /// Give a cast member a voice for their lines
    Voice {
        handle: String,
        #[arg(long)]
        plugin: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        voice_id: Option<String>,
        /// Voice setting passed to the plugin, e.g. --set stability=0.5
        #[arg(long = "set", value_parser = key_value)]
        settings: Vec<(String, String)>,
    },
    Ls,
}

#[derive(Subcommand)]
enum ChapterCmd {
    Add {
        title: String,
    },
    /// Change a chapter's title; its folder keeps its name
    Rename {
        chapter: String,
        title: String,
    },
    Ls,
}

#[derive(Subcommand)]
enum ShotCmd {
    Add {
        chapter: String,
        /// Insert after this shot number (e.g. 010) instead of at the end
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value = "")]
        title: String,
        #[arg(long, default_value = "")]
        plot: String,
        /// Can be left out and drafted from the plot later with `shot draft`
        #[arg(long, default_value = "")]
        prompt: String,
        #[arg(long)]
        duration: Option<f64>,
        #[arg(long, default_value = "cut")]
        transition: Transition,
        /// Plugin for this shot instead of the project default
        #[arg(long)]
        plugin: Option<String>,
        /// Model for this shot instead of the project default
        #[arg(long)]
        model: Option<String>,
        /// Recipe from shotline.toml, e.g. talking
        #[arg(long)]
        recipe: Option<String>,
    },
    /// Change a shot: `shot set 01-010 --prompt ...`
    Set {
        shot: ShotRef,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        plot: Option<String>,
        #[arg(long)]
        prompt: Option<String>,
        /// What the storyboard frame is made from instead of the prompt; "" clears it
        #[arg(long)]
        frame_prompt: Option<String>,
        #[arg(long)]
        duration: Option<f64>,
        #[arg(long)]
        transition: Option<Transition>,
        /// How lines are spoken: spoken, lip-synced, a recipe from shotline.toml, or "" for not
        /// at all
        #[arg(long)]
        recipe: Option<String>,
        /// Video model instead of the film's; "" goes back to the film's
        #[arg(long)]
        model: Option<String>,
        /// The model's plugin (default: the film's)
        #[arg(long, requires = "model")]
        plugin: Option<String>,
        /// Instead of the film's; "" goes back to the film's
        #[arg(long)]
        resolution: Option<String>,
        /// Don't send this anchor of a named element with this shot, e.g.
        /// elements/cast/paka/02.png
        #[arg(long)]
        leave_out: Vec<String>,
        /// Send a left out anchor again
        #[arg(long)]
        put_back: Vec<String>,
    },
    /// Add a line of dialogue: `shot line 01-010 paka "..."`
    Line {
        shot: ShotRef,
        who: String,
        text: String,
        /// Seconds into the shot it starts (default: after the line before)
        #[arg(long)]
        at: Option<f64>,
    },
    /// Record a line on its own to hear it, and send that recording with the shot's takes.
    /// Without options, records a new one (after the price); `--ls` lists what's recorded.
    Say {
        shot: ShotRef,
        /// The line, from 1
        line: usize,
        #[arg(long, conflicts_with_all = ["pick", "fresh"])]
        ls: bool,
        /// Send recording n (from `--ls`) instead
        #[arg(long = "use", value_name = "N", conflicts_with = "fresh")]
        pick: Option<usize>,
        /// Record it fresh with each take again
        #[arg(long)]
        fresh: bool,
        #[arg(long, short)]
        yes: bool,
    },
    /// A picture of the shot's own sent along with its prompt, after the elements' anchors.
    /// Without options, lists them.
    Image {
        shot: ShotRef,
        #[arg(long, conflicts_with_all = ["generate", "remove", "edit"])]
        file: Option<PathBuf>,
        #[arg(long, conflicts_with_all = ["remove", "edit"])]
        generate: bool,
        /// What to make, naming elements as @handles
        #[arg(long, default_value = "", requires = "generate")]
        prompt: String,
        /// What it shows; the video prompt gets "Image 3: <note>."
        #[arg(long, default_value = "")]
        note: String,
        /// An image to go along instead of the named elements' anchors, a file or an http(s)
        /// URL
        #[arg(long = "ref", requires = "generate")]
        refs: Vec<String>,
        /// e.g. 1:1 or 9:16, instead of the shot's
        #[arg(long, requires = "generate")]
        aspect: Option<String>,
        /// e.g. 1K or 2K, for models that have a choice
        #[arg(long, requires = "generate")]
        resolution: Option<String>,
        /// Put the prompt into English with the text model first, if it isn't
        #[arg(long, requires = "generate")]
        translate: bool,
        /// Take image n (from 1) off the shot
        #[arg(long, conflicts_with = "edit")]
        remove: Option<usize>,
        /// Change image n's note to --note
        #[arg(long)]
        edit: Option<usize>,
        #[command(flatten)]
        image: ImageArgs,
    },
    /// Draft the prompt in English from the plot with the text model
    Draft {
        shot: ShotRef,
        /// video (the prompt), image (the storyboard frame's prompt) or translate (the plot
        /// word for word, as the prompt)
        #[arg(long = "as", default_value = "video")]
        how: Draft,
        #[command(flatten)]
        text: TextArgs,
    },
    Ls {
        chapter: Option<String>,
    },
    Show {
        shot: ShotRef,
    },
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(feature = "ui")]
fn ui(p: Option<Project>) -> Result<()> {
    let descriptions = plugins::load()
        .plugins
        .into_iter()
        .filter_map(|p| p.description.ok())
        .collect();
    shotline_ui::run(
        p,
        shotline_core::user_dir(),
        descriptions,
        std::sync::Arc::new(Plugins),
    )
}

/// The installed plugins, loaded again for each ask so keys saved since are used.
#[cfg(feature = "ui")]
struct Plugins;

#[cfg(feature = "ui")]
impl shotline_ui::Host for Plugins {
    fn plugins(&self) -> (Vec<shotline_ui::PluginInfo>, Vec<String>) {
        plugins::infos(&plugins::load())
    }

    fn pin(&self, name: &str, exec: &Path) -> Result<String> {
        plugins::pin(&plugins::load(), name, Some(exec))
    }

    fn unpin(&self, name: &str) -> Result<String> {
        plugins::unpin(name)
    }

    fn voices(&self, plugin: &str) -> Result<Vec<shotline_core::protocol::VoiceInfo>> {
        plugins::voices(&plugins::load(), plugin)
    }

    fn balance(&self, plugin: &str) -> Result<String> {
        plugins::balance_of(&plugins::load(), plugin)
    }

    fn sequences(&self, film: &Path) -> (Vec<shotline_core::protocol::EditInfo>, Vec<String>) {
        plugins::sequences(&plugins::load(), film)
    }

    fn open_edit(&self, plugin: &str, ask: &shotline_core::protocol::OpenParams) -> Result<()> {
        plugins::open_edit(&plugins::load(), plugin, ask)
    }

    fn anchor_quote(&self, film: &Path, ask: &shotline_ui::AnchorAsk) -> Result<String> {
        let p = Project::open(film)?;
        image::quote(&p, target(ask), &anchor_make(ask, None))
    }

    fn render(&self, film: &Path, on: &mut dyn FnMut(String)) -> Result<(PathBuf, String)> {
        use shotline_media::render::Progress;
        let p = Project::open(film)?;
        let (out, r) = timeline::render_with(&p, None, true, None, |pr| match pr {
            Progress::Clip {
                index, of, label, ..
            } => on(format!("{}/{of} {label}", index + 1)),
            Progress::Encoding => on("encoding".into()),
        })?;
        let secs = shotline_core::ledger::money(r.duration);
        Ok((out, format!("{secs}s · {}x{}", r.width, r.height)))
    }

    fn rates(&self, film: &Path) -> Result<Vec<shotline_core::ledger::RateLine>> {
        cost::rate_lines(&Project::open(film)?, &plugins::load())
    }

    fn reconcile(&self, film: &Path) -> Result<Vec<shotline_core::ledger::reconcile::Check>> {
        cost::checks(&Project::open(film)?, &plugins::load(), None)
    }

    fn fetch_image(&self, film: &Path, url: &str) -> Result<PathBuf> {
        fetch::image(&Project::open(film)?, url)
    }

    fn say_quote(&self, film: &Path, shot: ShotRef, line: usize) -> Result<String> {
        shoot::say_quote(&Project::open(film)?, shot, line)
    }

    fn say(&self, film: &Path, shot: ShotRef, line: usize, at: &str) -> Result<PathBuf> {
        let p = Project::open(film)?;
        match shoot::say(&p, shot, line, shoot::Go::At(at))? {
            Some(j) => Ok(p.job_dir(j.id).join(&j.outputs[0])),
            None => bail!("nothing was recorded"),
        }
    }

    fn take_quote(&self, film: &Path, shot: ShotRef, translate: bool) -> Result<String> {
        shoot::take_quote(&Project::open(film)?, shot, translate)
    }

    fn take(
        &self,
        film: &Path,
        shot: ShotRef,
        at: &str,
        translate: bool,
        on: &mut dyn FnMut(String),
    ) -> Result<String> {
        shoot::take_at(&Project::open(film)?, shot, at, translate, on)
    }

    fn make_anchor(&self, film: &Path, ask: &shotline_ui::AnchorAsk, at: &str) -> Result<PathBuf> {
        let p = Project::open(film)?;
        match image::image(&p, target(ask), &anchor_make(ask, Some(at)))? {
            Some((_, out)) => Ok(out),
            None => bail!("nothing was made"),
        }
    }
}

#[cfg(feature = "ui")]
fn target(ask: &shotline_ui::AnchorAsk) -> image::Target<'_> {
    match ask.shot {
        Some(r) if ask.frame => image::Target::Frame(r),
        Some(r) => image::Target::Shot(r),
        None => image::Target::Anchor(&ask.handle),
    }
}

#[cfg(feature = "ui")]
fn anchor_make<'a>(ask: &'a shotline_ui::AnchorAsk, at: Option<&'a str>) -> image::Make<'a> {
    image::Make {
        prompt: &ask.prompt,
        plugin: Some(&ask.plugin),
        model: Some(&ask.model),
        yes: false,
        at,
        refs: Some(ask.refs.clone()),
        aspect: ask.aspect.as_deref(),
        resolution: ask.resolution.as_deref(),
        translate: ask.translate,
        outfit: ask.outfit,
    }
}

#[cfg(not(feature = "ui"))]
fn ui(_: Option<Project>) -> Result<()> {
    bail!("this shotline was built without the ui feature")
}

fn run(cli: Cli) -> Result<()> {
    let open = || -> Result<Project> {
        Ok(match &cli.project {
            Some(dir) => Project::open(dir)?,
            None => Project::discover(&std::env::current_dir()?)?,
        })
    };

    match cli.cmd {
        Cmd::Init {
            ref dir,
            ref name,
            no_git,
            ref aspect,
            ref resolution,
        } => {
            let mut p = Project::init(dir, name.as_deref())?;
            if aspect.is_some() || resolution.is_some() {
                let c = format::Change {
                    aspect: aspect.as_deref(),
                    resolution: resolution.as_deref(),
                    ..Default::default()
                };
                format::apply(&mut p, &c).map_err(|e| {
                    e.context(format!(
                        "made {} with the default format, but couldn't set this one",
                        dir.display()
                    ))
                })?;
            }
            println!("created {} in {}", p.config.name, dir.display());
            // Inside a repository already, the film is just another folder of it.
            if let Some(git) = Git::find().filter(|g| !no_git && !g.is_repo(dir)) {
                match git.init(dir, &format!("start {}", p.config.name)) {
                    Ok(h) => println!("started its git history at {h}"),
                    Err(e) => eprintln!("no git history: {e}"),
                }
            }
        }

        Cmd::Format {
            ref aspect,
            ref resolution,
            duration,
            ref plugin,
            ref model,
        } => format::run(
            &mut open()?,
            &format::Change {
                aspect: aspect.as_deref(),
                resolution: resolution.as_deref(),
                duration,
                plugin: plugin.as_deref(),
                model: model.as_deref(),
            },
        )?,
        Cmd::Story { ref set } => doc(&open()?, "story.md", set.as_deref())?,
        Cmd::Style { ref set } => doc(&open()?, "style.md", set.as_deref())?,

        Cmd::Element(ElementCmd::Add {
            kind,
            ref name,
            ref handle,
            ref desc,
        }) => {
            let el = open()?.add_element(kind, name, handle.as_deref(), desc)?;
            println!("added {} @{} ({})", el.name, el.handle, el.kind);
        }
        Cmd::Element(ElementCmd::Anchor {
            ref handle,
            ref file,
            generate,
            ref prompt,
            ref refs,
            no_anchors,
            ref aspect,
            ref resolution,
            translate,
            ref image,
        }) => {
            let p = open()?;
            if generate {
                let picked = image::picked(&p, handle, refs, no_anchors)?;
                let mut m = image.make(prompt);
                m.refs = picked;
                m.aspect = aspect.as_deref();
                m.resolution = resolution.as_deref();
                m.translate = translate;
                image::anchor(&p, handle, &m)?;
            } else {
                let dest = p.add_anchor(handle, file.as_ref().unwrap())?;
                println!("{}", p.rel(&dest));
            }
        }
        Cmd::Element(ElementCmd::Draft {
            ref handle,
            ref text,
        }) => draft::element(&open()?, handle, &text.ask())?,
        Cmd::Element(ElementCmd::Voice {
            ref handle,
            ref plugin,
            ref model,
            ref voice_id,
            ref settings,
        }) => {
            let p = open()?;
            let mut set = toml::Table::new();
            for (k, v) in settings {
                let v = v
                    .parse::<toml::Value>()
                    .unwrap_or_else(|_| toml::Value::String(v.clone()));
                set.insert(k.clone(), v);
            }
            let el = p.set_voice(
                handle,
                Voice {
                    plugin: plugin.clone(),
                    model: model.clone(),
                    voice_id: voice_id.clone(),
                    settings: set,
                },
            )?;
            println!("@{} speaks with {plugin}", el.handle);
        }
        Cmd::Element(ElementCmd::Ls) => {
            let p = open()?;
            for el in p.elements()? {
                let n = p.anchors(&el)?.len();
                let s = if n == 1 { "" } else { "s" };
                println!(
                    "{:<5} @{:<16} {} ({n} anchor{s})",
                    el.kind, el.handle, el.name
                );
            }
        }

        Cmd::Chapter(ChapterCmd::Add { ref title }) => {
            let p = open()?;
            let c = p.add_chapter(title)?;
            println!("{} {}", c.id(), p.rel(&c.dir));
        }
        Cmd::Chapter(ChapterCmd::Rename {
            ref chapter,
            ref title,
        }) => {
            let c = open()?.rename_chapter(chapter, title)?;
            println!("{}  {}", c.id(), c.title);
        }
        Cmd::Chapter(ChapterCmd::Ls) => {
            for c in open()?.chapters()? {
                println!("{}  {}", c.id(), c.title);
            }
        }

        Cmd::Shot(ShotCmd::Add {
            ref chapter,
            ref after,
            ref title,
            ref plot,
            ref prompt,
            duration,
            transition,
            ref plugin,
            ref model,
            ref recipe,
        }) => {
            let after = after.as_deref().map(parse_shot_number).transpose()?;
            let model = (plugin.is_some() || model.is_some()).then(|| ModelOverride {
                plugin: plugin.clone(),
                id: model.clone(),
                ..ModelOverride::default()
            });
            let shot = Shot {
                title: title.clone(),
                plot: plot.clone(),
                prompt: prompt.clone(),
                duration,
                transition,
                model,
                recipe: recipe.clone(),
                ..Shot::default()
            };
            println!("{}", open()?.add_shot(chapter, after, shot)?);
        }
        Cmd::Shot(ShotCmd::Set {
            shot,
            ref title,
            ref plot,
            ref prompt,
            ref frame_prompt,
            duration,
            transition,
            ref recipe,
            ref model,
            ref plugin,
            ref resolution,
            ref leave_out,
            ref put_back,
        }) => {
            let p = open()?;
            let mut s = p.shot(shot)?;
            for a in leave_out {
                let a = a.trim_start_matches("./").to_string();
                if !p.root.join(&a).is_file() {
                    bail!("{a} doesn't exist");
                }
                if !s.leave_out.contains(&a) {
                    s.leave_out.push(a);
                }
            }
            for a in put_back {
                s.leave_out.retain(|x| x != a.trim_start_matches("./"));
            }
            if let Some(m) = model {
                let m = (!m.is_empty()).then(|| {
                    let plugin = plugin.clone().unwrap_or_else(|| p.config.plugin.clone());
                    (plugin, m.clone())
                });
                if let Some((plugin, m)) = &m {
                    format::video_model(&p, plugin, m)?;
                }
                s.set_video_model(m);
            }
            if let Some(r) = resolution {
                s.set_resolution((!r.is_empty()).then(|| r.clone()));
            }
            if let Some(t) = frame_prompt {
                s.frame_prompt = (!t.is_empty()).then(|| t.clone());
            }
            if let Some(t) = title {
                s.title = t.clone();
            }
            if let Some(t) = plot {
                s.plot = t.clone();
            }
            if let Some(t) = prompt {
                s.prompt = t.clone();
            }
            if let Some(r) = recipe {
                s.recipe = (!r.is_empty()).then(|| r.clone());
            }
            s.duration = duration.or(s.duration);
            s.transition = transition.unwrap_or(s.transition);
            p.save_shot(shot, &s)?;
            println!("{shot} saved");
        }
        Cmd::Shot(ShotCmd::Image {
            shot,
            ref file,
            generate,
            ref prompt,
            ref note,
            ref refs,
            ref aspect,
            ref resolution,
            translate,
            remove,
            edit,
            ref image,
        }) => {
            let p = open()?;
            let n = |n: usize| anyhow::Context::context(n.checked_sub(1), "images count from 1");
            if let Some(f) = file {
                let dest = p.add_shot_image(shot, f, note)?;
                println!("{}", p.rel(&dest));
            } else if generate {
                let mut m = image.make(prompt);
                m.refs = image::given(&p, refs)?;
                m.aspect = aspect.as_deref();
                m.resolution = resolution.as_deref();
                m.translate = translate;
                image::shot_image(&p, shot, &m, note)?;
            } else if let Some(i) = remove {
                let img = p.remove_shot_image(shot, n(i)?)?;
                println!("{} taken off {shot}", img.file);
            } else if let Some(i) = edit {
                p.set_shot_image_note(shot, n(i)?, note)?;
                println!("{shot} saved");
            } else {
                for (i, img) in p.shot(shot)?.images.iter().enumerate() {
                    match img.note.as_str() {
                        "" => println!("{}  {}", i + 1, img.file),
                        note => println!("{}  {}  {note}", i + 1, img.file),
                    }
                }
            }
        }
        Cmd::Shot(ShotCmd::Line {
            shot,
            ref who,
            ref text,
            at,
        }) => {
            let s = open()?.add_line(shot, who, text, at)?;
            println!(
                "{shot} has {} line{}",
                s.lines.len(),
                if s.lines.len() == 1 { "" } else { "s" }
            );
        }
        Cmd::Shot(ShotCmd::Say {
            shot,
            line,
            ls,
            pick,
            fresh,
            yes,
        }) => {
            let p = open()?;
            let i = line.checked_sub(1).context("lines count from 1")?;
            let who = p
                .shot(shot)?
                .lines
                .get(i)
                .map(|l| l.who.clone())
                .with_context(|| format!("{shot} has no line {line}"))?;
            let all = p.recordings_by(&who, Some(shot))?;
            let sent = p.line_audio(shot, i)?.map(|j| j.id);
            if ls {
                if all.is_empty() {
                    println!("@{who} has no recordings yet");
                }
                for (n, j) in all.iter().enumerate() {
                    let mark = if Some(j.id) == sent { "*" } else { " " };
                    let from = j.shot.map(|s| s.to_string()).unwrap_or_default();
                    let words = j.prompt.as_deref().unwrap_or("");
                    println!("{mark} {}  {from}  {words}", n + 1);
                }
            } else if let Some(n) = pick {
                let j = n
                    .checked_sub(1)
                    .and_then(|k| all.get(k))
                    .with_context(|| format!("@{who} has {} recordings", all.len()))?;
                p.set_line_audio(shot, i, Some(j))?;
                println!("line {line} of {shot} sends recording {n}");
            } else if fresh {
                p.set_line_audio(shot, i, None)?;
                println!("line {line} of {shot} is recorded fresh with each take");
            } else if let Some(j) = shoot::say(&p, shot, i, yes.into())? {
                let file = p.job_dir(j.id).join(&j.outputs[0]);
                println!("recorded line {line} of {shot}: {}", p.rel(&file));
            }
        }
        Cmd::Shot(ShotCmd::Draft {
            shot,
            how,
            ref text,
        }) => draft::shot(&open()?, shot, how, &text.ask())?,
        Cmd::Shot(ShotCmd::Ls { ref chapter }) => {
            let p = open()?;
            for (r, s) in p.shots(chapter.as_deref())? {
                let secs = s.duration.unwrap_or(p.config.duration);
                let label = [&s.title, &s.plot, &s.prompt]
                    .into_iter()
                    .find(|t| !t.is_empty())
                    .map_or("", |t| t.as_str());
                println!("{r}  {:<8} {secs:>4}s  {label}", s.transition);
            }
        }
        Cmd::Shot(ShotCmd::Show { shot }) => {
            let p = open()?;
            p.shot(shot)?;
            println!("# {shot}");
            print!("{}", std::fs::read_to_string(p.shot_path(shot)?)?);
        }

        Cmd::Prompt { shot } => {
            let p = open()?;
            let installed = plugins::load();
            let b = build(&p, &plugins::catalog(&p, &installed)?, shot)?;
            for w in &b.warnings {
                eprintln!("warning: {w}");
            }
            for c in &b.cautions {
                eprintln!("note: {c}");
            }
            println!(
                "shot {} · {} {} · {}s · {} · {}",
                b.shot, b.plugin, b.model, b.duration, b.resolution, b.aspect
            );
            if let Some(sf) = &b.start_frame {
                match sf.from {
                    Some(r) => println!("start frame: {} (from {r})", sf.path),
                    None => println!("start frame: {} (storyboard)", sf.path),
                }
            }
            if !b.references.is_empty() {
                println!("references:");
                for r in &b.references {
                    let whose = match r.handle.as_str() {
                        "" => String::new(),
                        h => format!("@{h}"),
                    };
                    println!("  {:<12} {whose:<17} {}", r.label, r.path);
                }
            }
            println!("\n{}", b.text);
        }

        Cmd::Frame {
            shot,
            ref file,
            clear,
            ref refs,
            no_refs,
            ref aspect,
            ref resolution,
            translate,
            ref image,
        } => {
            let p = open()?;
            let s = p.shot(shot)?;
            // Only read when it's to be translated; otherwise the shot's own is used.
            let said = s.frame_prompt.unwrap_or(s.prompt);
            let how = match (file, clear) {
                (Some(f), _) => image::Frame::File(f),
                (None, true) => image::Frame::Clear,
                (None, false) => {
                    let mut m = image.make(&said);
                    m.translate = translate;
                    m.refs = if no_refs {
                        Some(vec![])
                    } else {
                        image::given(&p, refs)?
                    };
                    m.aspect = aspect.as_deref();
                    m.resolution = resolution.as_deref();
                    image::Frame::Make(m)
                }
            };
            image::frame(&p, shot, how)?;
        }
        Cmd::Quote { ref target } => shoot::quote(&open()?, target)?,
        Cmd::Take {
            shot,
            yes,
            from_step,
            ref of,
            translate,
        } => shoot::take(&open()?, shot, yes, from_step, of.as_deref(), translate)?,
        Cmd::Run {
            ref op,
            ref plugin,
            ref model,
            ref prompt,
            duration,
            ref inputs,
            ref refs,
            ref params,
            yes,
        } => shoot::run(
            &open()?,
            shoot::RunArgs {
                op: op.clone(),
                plugin: plugin.clone(),
                model: model.clone(),
                prompt: prompt.clone(),
                duration,
                inputs: inputs.clone(),
                refs: refs.clone(),
                params: params.clone(),
                yes,
            },
        )?,

        Cmd::Takes { shot } => {
            let p = open()?;
            let selected = p.shot(shot)?.selected_take;
            let takes = p.takes(shot)?;
            for t in &takes {
                let mark = if selected.as_deref() == Some(&t.id.to_string()) {
                    "*"
                } else {
                    " "
                };
                let cost = t
                    .costs()
                    .iter()
                    .map(|c| format!("{} {}", shotline_core::ledger::money(c.amount), c.unit))
                    .collect::<Vec<_>>()
                    .join(" + ");
                let steps = t.jobs.len();
                let s = if steps == 1 { "" } else { "s" };
                let made = shotline_core::made_from(t, &takes)
                    .map(|m| format!("  {m}"))
                    .unwrap_or_default();
                println!(
                    "{mark} {t}  {:<9} {steps} step{s}  {cost}  {}{made}",
                    t.status(),
                    t.id
                );
            }
        }
        Cmd::Select { ref shot, ref take } => {
            let (shot, take) = match (shot.split_once('#'), take) {
                (Some((s, t)), None) => (s.parse::<ShotRef>()?, t.to_string()),
                (None, Some(t)) => (shot.parse::<ShotRef>()?, t.clone()),
                _ => bail!("give the take as `01-010 3` or `01-010#3`"),
            };
            let p = open()?;
            let t = p.select_take(shot, &take)?;
            println!("{t} selected");
            // Kept to pick from in the app, and for a shot continuing from this one.
            let video = t
                .output()
                .and_then(|j| j.outputs.first().map(|f| p.job_dir(j.id).join(f)));
            if let (Ok(ff), Some(v), Some(png)) = (
                shotline_media::Ffmpeg::find(),
                video,
                p.last_frame(&t).filter(|f| !f.is_file()),
            ) && let Err(e) = ff.last_frame(&v, &png)
            {
                eprintln!("warning: can't take its last frame: {e}");
            }
        }

        Cmd::Splice {
            shot,
            ref pieces,
            fade,
            no_colormatch,
        } => {
            let p = open()?;
            let pieces = pieces
                .iter()
                .map(|s| shotline_core::Piece::parse(&p, shot, s))
                .collect::<shotline_core::Result<Vec<_>>>()?;
            let ff = shotline_media::Ffmpeg::find()?;
            let opts = shotline_media::splice::Options {
                fade,
                colormatch: !no_colormatch,
                ..Default::default()
            };
            let work = p.root.join(".shotline/cache/splice");
            let t = p.splice_take(shot, &pieces, |sources, out| {
                shotline_media::splice::splice(&ff, &media_pieces(sources), &opts, &work, out)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })?;
            println!("{t} made from {} pieces", pieces.len());
        }

        Cmd::Import { shot, ref file } => {
            let t = open()?.import_take(shot, file)?;
            println!("{t} brought in from {}", file.display());
        }

        Cmd::Cost { ref cmd, by, csv } => {
            let p = open()?;
            match cmd {
                None => cost::show(&p, by, csv)?,
                Some(CostCmd::Verify) => cost::verify(&p)?,
                Some(CostCmd::Rates) => cost::rates(&p, &plugins::load())?,
                Some(CostCmd::Reconcile { plugin }) => {
                    cost::reconcile_cmd(&p, &plugins::load(), plugin.as_deref())?
                }
                Some(CostCmd::Adjust { amount, unit, note }) => {
                    cost::adjust(&p, *amount, unit, note)?
                }
                Some(CostCmd::Release { job, note, sure }) => {
                    cost::release(&p, job.as_deref(), note, *sure)?
                }
                Some(CostCmd::Refund {
                    job,
                    amount,
                    unit,
                    note,
                }) => cost::refund(&p, job, *amount, unit, note)?,
            }
        }
        Cmd::Plugin(PluginCmd::Ls) => plugins::ls(&plugins::load()),
        Cmd::Plugin(PluginCmd::Doctor { ref name }) => plugins::doctor(&plugins::load(), name)?,
        Cmd::Plugin(PluginCmd::Balance { ref name }) => {
            plugins::balance(&plugins::load(), name.as_deref())?
        }
        Cmd::Plugin(PluginCmd::Voices { ref name }) => {
            plugins::print_voices(&plugins::load(), name)?
        }
        Cmd::Plugin(PluginCmd::Pin { ref name, ref path }) => {
            println!("{}", plugins::pin(&plugins::load(), name, path.as_deref())?)
        }
        Cmd::Plugin(PluginCmd::Unpin { ref name }) => println!("{}", plugins::unpin(name)?),
        Cmd::Plugin(PluginCmd::Option {
            ref plugin,
            ref model,
            ref key,
            ref value,
        }) => {
            let mut p = open()?;
            println!(
                "{}",
                plugins::set_option(&mut p, plugin, model, key, value.as_deref())?
            )
        }
        Cmd::Timeline { ref cmd } => {
            let p = open()?;
            match cmd {
                None => timeline::show(&p)?,
                Some(TimelineCmd::Render {
                    output,
                    no_colormatch,
                    xfade,
                }) => timeline::render(&p, output.as_deref(), !no_colormatch, *xfade)?,
            }
        }
        Cmd::Mcp => mcp::serve(cli.project.clone())?,
        Cmd::Ui { ref dir } => {
            // Outside a film the app asks for one.
            let p = match (dir, &cli.project) {
                (Some(d), _) => Project::discover(d),
                (None, Some(d)) => Project::open(d),
                (None, None) => Project::discover(&std::env::current_dir()?),
            };
            match p {
                Ok(p) => ui(Some(p))?,
                Err(shotline_core::Error::NotAProject(_)) => ui(None)?,
                Err(e) => return Err(e.into()),
            }
        }
        Cmd::FakePlugin => {
            plugins::runtime().block_on(shotline_plugin::serve(
                shotline_plugin::fake::FakePlugin::default(),
            ))?;
        }
    }
    Ok(())
}

fn doc(p: &Project, name: &str, set: Option<&str>) -> Result<()> {
    let path = p.root.join(name);
    match set {
        Some(text) => {
            let mut text = text.to_string();
            if !text.ends_with('\n') {
                text.push('\n');
            }
            std::fs::write(&path, text)?;
            println!("{name} saved");
        }
        None => match std::fs::read_to_string(&path) {
            Ok(t) => print!("{t}"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("{name} is empty"),
            Err(e) => return Err(e.into()),
        },
    }
    Ok(())
}

fn key_value(s: &str) -> Result<(String, String), String> {
    s.split_once('=')
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .ok_or_else(|| format!("expected key=value, got {s:?}"))
}

fn key_path(s: &str) -> Result<(String, PathBuf), String> {
    key_value(s).map(|(k, v)| (k, PathBuf::from(v)))
}

/// "010" or "01-010" -> 10
fn parse_shot_number(s: &str) -> Result<u32> {
    let n = s.rsplit(['-', '/']).next().unwrap_or(s);
    match n.parse() {
        Ok(n) => Ok(n),
        Err(_) => bail!("bad shot number {s:?}"),
    }
}

fn media_pieces(sources: &[(PathBuf, shotline_core::Piece)]) -> Vec<shotline_media::splice::Piece> {
    use shotline_media::splice::Join;
    sources
        .iter()
        .map(|(path, p)| shotline_media::splice::Piece {
            path: path.clone(),
            from: p.from,
            to: p.to,
            join: match p.join {
                shotline_core::Join::Cut => Join::Cut,
                shotline_core::Join::Fade => Join::Fade,
                shotline_core::Join::Smooth => Join::Smooth,
            },
        })
        .collect()
}
