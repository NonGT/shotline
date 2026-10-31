//! `shotline mcp`: the CLI's commands as MCP tools over stdio. Each call runs this same binary as a
//! child process in the project folder, so tools print exactly what the commands print and nothing
//! can read the MCP stream from stdin.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, ProgressNotificationParam, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use shotline_core::Project;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

const CO_DIRECTOR: &str = include_str!("../../../agent/CO_DIRECTOR.md");

type Reply = Result<CallToolResult, ErrorData>;

#[derive(Clone)]
pub struct Server {
    project: Arc<Mutex<Option<PathBuf>>>,
    exe: PathBuf,
    tool_router: ToolRouter<Self>,
}

pub fn serve(project: Option<PathBuf>) -> Result<()> {
    let start = match project {
        Some(dir) => Some(Project::open(&dir)?.root),
        None => Project::discover(&std::env::current_dir()?)
            .ok()
            .map(|p| p.root),
    };
    let server = Server {
        project: Arc::new(Mutex::new(start)),
        exe: std::env::current_exe()?,
        tool_router: Server::tool_router(),
    };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let running = server.serve(rmcp::transport::stdio()).await?;
            running.waiting().await?;
            Ok(())
        })
}

fn text(s: impl Into<String>) -> Reply {
    Ok(CallToolResult::success(vec![ContentBlock::text(s)]))
}

fn refuse(s: impl Into<String>) -> Reply {
    Ok(CallToolResult::error(vec![ContentBlock::text(s)]))
}

const NOT_CONFIRMED: &str = "Nothing was spent. Show the user this price and call again with \
                             confirm: true only once they agree to it.";

impl Server {
    fn dir(&self) -> Option<PathBuf> {
        self.project.lock().unwrap().clone()
    }

    /// Runs `shotline <args>` in the open project. Lines it prints go out as progress if the
    /// client asked for it. `answer` is what to type at a confirmation prompt.
    async fn cli(
        &self,
        args: Vec<String>,
        answer: Option<&str>,
        ctx: &RequestContext<RoleServer>,
    ) -> Reply {
        let Some(dir) = self.dir() else {
            return refuse("no project is open; call open_project or init first");
        };
        self.exec(&dir, args, answer, ctx).await
    }

    async fn exec(
        &self,
        dir: &std::path::Path,
        args: Vec<String>,
        answer: Option<&str>,
        ctx: &RequestContext<RoleServer>,
    ) -> Reply {
        let fail =
            |e: std::io::Error| ErrorData::internal_error(format!("running shotline: {e}"), None);
        let mut child = tokio::process::Command::new(&self.exe)
            .args(&args)
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(fail)?;
        let mut stdin = child.stdin.take().unwrap();
        if let Some(a) = answer {
            stdin
                .write_all(format!("{a}\n").as_bytes())
                .await
                .map_err(fail)?;
        }
        drop(stdin);
        let mut err = String::new();
        let mut stderr = child.stderr.take().unwrap();
        let err_task = async { stderr.read_to_string(&mut err).await };
        let token = ctx.meta.get_progress_token();
        let out_task = async {
            let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
            let mut out = String::new();
            let mut n = 0.0;
            while let Some(line) = lines.next_line().await? {
                if let Some(t) = &token {
                    n += 1.0;
                    let _ = ctx
                        .peer
                        .notify_progress(
                            ProgressNotificationParam::new(t.clone(), n).with_message(line.clone()),
                        )
                        .await;
                }
                out.push_str(&line);
                out.push('\n');
            }
            Ok::<_, std::io::Error>(out)
        };
        let (out, e) = tokio::join!(out_task, err_task);
        let out = out.map_err(fail)?;
        e.map_err(fail)?;
        let status = child.wait().await.map_err(fail)?;
        let mut all = out;
        if !err.trim().is_empty() {
            all.push_str(&err);
        }
        if status.success() {
            text(all.trim_end().to_string())
        } else {
            refuse(all.trim_end().to_string())
        }
    }
}

fn opt(args: &mut Vec<String>, flag: &str, v: Option<impl ToString>) {
    if let Some(v) = v {
        args.push(flag.into());
        args.push(v.to_string());
    }
}

fn a<const N: usize>(xs: [&str; N]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

#[derive(Deserialize, JsonSchema)]
pub struct Dir {
    /// Folder path; relative paths are from the folder the server was started in
    dir: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct Init {
    dir: String,
    /// Film name (default: the folder name)
    name: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct Doc {
    /// New contents, replacing the whole file. Leave out to read it.
    text: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ElementAdd {
    /// cast, set or prop
    kind: String,
    name: String,
    /// @handle used in prompts (default: from the name)
    handle: Option<String>,
    description: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct Anchor {
    handle: String,
    /// Image file, relative to the project folder
    file: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct MakeAnchor {
    handle: String,
    /// Added to the element's description, e.g. "side view, full body"
    prompt: Option<String>,
    /// Image plugin and model instead of [image] in shotline.toml
    plugin: Option<String>,
    model: Option<String>,
    /// Spend the money. Only after the user agreed to the price.
    #[serde(default)]
    confirm: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct Frame {
    /// e.g. 01-010
    shot: String,
    /// Use this image (relative to the project folder) instead of making one
    file: Option<String>,
    /// Remove the shot's frame
    #[serde(default)]
    clear: bool,
    plugin: Option<String>,
    model: Option<String>,
    /// Spend the money. Only after the user agreed to the price.
    #[serde(default)]
    confirm: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct Voice {
    handle: String,
    plugin: String,
    model: Option<String>,
    voice_id: Option<String>,
    /// Recording to clone from, relative to the project folder
    sample: Option<String>,
    /// Voice settings passed to the plugin, e.g. {"stability": 0.5}
    #[serde(default)]
    settings: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize, JsonSchema)]
pub struct Title {
    title: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct ShotAdd {
    /// Chapter number, e.g. 1
    chapter: String,
    /// Insert after this shot number (e.g. 010) instead of at the end
    after: Option<String>,
    title: Option<String>,
    /// What happens, for people
    plot: Option<String>,
    /// What the model is asked for; mention elements as @handle
    prompt: String,
    /// Seconds (default: the project's)
    duration: Option<f64>,
    /// cut, or continue to start on the previous shot's last frame
    transition: Option<String>,
    plugin: Option<String>,
    model: Option<String>,
    /// Recipe from shotline.toml, e.g. talking
    recipe: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ShotSet {
    /// e.g. 01-010
    shot: String,
    title: Option<String>,
    plot: Option<String>,
    prompt: Option<String>,
    /// What the storyboard frame is made from instead of the prompt; "" clears it
    frame_prompt: Option<String>,
    duration: Option<f64>,
    transition: Option<String>,
    recipe: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct Line {
    shot: String,
    /// @handle of the cast member
    who: String,
    text: String,
    /// Seconds into the shot it starts (default: after the line before)
    at: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct Chapter {
    /// Only this chapter
    chapter: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ShotId {
    /// e.g. 01-010
    shot: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct Target {
    /// A shot (01-010) or a whole chapter (1)
    target: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct Take {
    shot: String,
    /// Reuse the steps before this one from an earlier take
    from_step: Option<usize>,
    /// Which take to reuse from (default: the latest)
    of: Option<String>,
    /// Spend the money. Only after the user agreed to the price.
    #[serde(default)]
    confirm: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct Run {
    /// Operation, e.g. video.upscale
    op: String,
    plugin: String,
    model: String,
    prompt: Option<String>,
    duration: Option<f64>,
    /// Named input files relative to the project folder, e.g. {"video": "jobs/x/out.mp4"}
    #[serde(default)]
    inputs: BTreeMap<String, String>,
    /// Reference files relative to the project folder
    #[serde(default)]
    refs: Vec<String>,
    /// Plugin parameters, e.g. {"seed": 7}
    #[serde(default)]
    params: BTreeMap<String, serde_json::Value>,
    /// Spend the money. Only after the user agreed to the price.
    #[serde(default)]
    confirm: bool,
}

#[derive(Deserialize, JsonSchema)]
pub struct Select {
    shot: String,
    /// Take number, e.g. 3
    take: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct Render {
    /// Output file relative to the project folder (default: renders/<time>.mp4)
    output: Option<String>,
    /// Set false to skip colour matching
    colormatch: Option<bool>,
    /// Crossfade at each join in seconds
    xfade: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct Cost {
    /// chapter, shot, take, op, plugin or day
    by: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PluginName {
    name: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct MaybePlugin {
    name: Option<String>,
}

fn settings(map: &BTreeMap<String, serde_json::Value>, flag: &str, args: &mut Vec<String>) {
    for (k, v) in map {
        let v = match v {
            serde_json::Value::String(s) => s.clone(),
            v => v.to_string(),
        };
        args.push(flag.into());
        args.push(format!("{k}={v}"));
    }
}

#[tool_router]
impl Server {
    /// Create a new film folder and work in it from now on.
    #[tool]
    async fn init(
        &self,
        Parameters(p): Parameters<Init>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        let here =
            std::env::current_dir().map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let mut args = a(["init", &p.dir]);
        opt(&mut args, "--name", p.name);
        let r = self.exec(&here, args, None, &ctx).await?;
        if r.is_error != Some(true) {
            *self.project.lock().unwrap() = Some(here.join(&p.dir));
        }
        Ok(r)
    }

    /// Work in this existing film folder from now on.
    #[tool]
    async fn open_project(&self, Parameters(p): Parameters<Dir>) -> Reply {
        match Project::open(std::path::Path::new(&p.dir)) {
            Ok(proj) => {
                let msg = format!("{} in {}", proj.config.name, proj.root.display());
                *self.project.lock().unwrap() = Some(proj.root);
                text(msg)
            }
            Err(e) => refuse(e.to_string()),
        }
    }

    /// Read story.md (logline, world, synopsis), or replace it with `text`.
    #[tool]
    async fn story(
        &self,
        Parameters(p): Parameters<Doc>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        let mut args = a(["story"]);
        opt(&mut args, "--set", p.text);
        self.cli(args, None, &ctx).await
    }

    /// Read style.md (the film's look, camera and lens notes; goes into every prompt), or replace it with `text`.
    #[tool]
    async fn style(
        &self,
        Parameters(p): Parameters<Doc>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        let mut args = a(["style"]);
        opt(&mut args, "--set", p.text);
        self.cli(args, None, &ctx).await
    }

    /// Add a cast member, set or prop.
    #[tool]
    async fn element_add(
        &self,
        Parameters(p): Parameters<ElementAdd>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        let mut args = a(["element", "add", &p.kind, &p.name]);
        opt(&mut args, "--handle", p.handle);
        opt(&mut args, "--desc", p.description);
        self.cli(args, None, &ctx).await
    }

    /// Add an anchor image to an element, so the model keeps its look.
    #[tool]
    async fn element_anchor(
        &self,
        Parameters(p): Parameters<Anchor>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        self.cli(
            a(["element", "anchor", &p.handle, "--file", &p.file]),
            None,
            &ctx,
        )
        .await
    }

    /// Make a new anchor image of an element with the image model, from its description and the
    /// anchors it already has. SPENDS MONEY. Without confirm it only shows the price.
    #[tool]
    async fn element_anchor_generate(
        &self,
        Parameters(p): Parameters<MakeAnchor>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        let mut args = a(["element", "anchor", &p.handle, "--generate"]);
        opt(&mut args, "--prompt", p.prompt);
        opt(&mut args, "--plugin", p.plugin);
        opt(&mut args, "--model", p.model);
        self.spend(args, p.confirm, &ctx).await
    }

    /// Give a shot a storyboard frame for its video to start on: made by the image model (SPENDS
    /// MONEY; without confirm it only shows the price), or from a file, or cleared. Continue shots
    /// start on the shot before instead.
    #[tool]
    async fn frame(
        &self,
        Parameters(p): Parameters<Frame>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        let mut args = a(["frame", &p.shot]);
        if p.clear {
            args.push("--clear".into());
            return self.cli(args, None, &ctx).await;
        }
        if let Some(f) = p.file {
            args.extend(["--file".into(), f]);
            return self.cli(args, None, &ctx).await;
        }
        opt(&mut args, "--plugin", p.plugin);
        opt(&mut args, "--model", p.model);
        self.spend(args, p.confirm, &ctx).await
    }

    /// Give a cast member a voice for their lines.
    #[tool]
    async fn element_voice(
        &self,
        Parameters(p): Parameters<Voice>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        let mut args = a(["element", "voice", &p.handle, "--plugin", &p.plugin]);
        opt(&mut args, "--model", p.model);
        opt(&mut args, "--voice-id", p.voice_id);
        opt(&mut args, "--sample", p.sample);
        settings(&p.settings, "--set", &mut args);
        self.cli(args, None, &ctx).await
    }

    /// List cast, sets and props with their anchors.
    #[tool]
    async fn element_ls(&self, ctx: RequestContext<RoleServer>) -> Reply {
        self.cli(a(["element", "ls"]), None, &ctx).await
    }

    /// Add a chapter.
    #[tool]
    async fn chapter_add(
        &self,
        Parameters(p): Parameters<Title>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        self.cli(a(["chapter", "add", &p.title]), None, &ctx).await
    }

    /// List chapters.
    #[tool]
    async fn chapter_ls(&self, ctx: RequestContext<RoleServer>) -> Reply {
        self.cli(a(["chapter", "ls"]), None, &ctx).await
    }

    /// Add a shot to a chapter. Shots are numbered 010, 020, ... so there's room to insert.
    #[tool]
    async fn shot_add(
        &self,
        Parameters(p): Parameters<ShotAdd>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        let mut args = a(["shot", "add", &p.chapter, "--prompt", &p.prompt]);
        opt(&mut args, "--after", p.after);
        opt(&mut args, "--title", p.title);
        opt(&mut args, "--plot", p.plot);
        opt(&mut args, "--duration", p.duration);
        opt(&mut args, "--transition", p.transition);
        opt(&mut args, "--plugin", p.plugin);
        opt(&mut args, "--model", p.model);
        opt(&mut args, "--recipe", p.recipe);
        self.cli(args, None, &ctx).await
    }

    /// Change a shot's title, plot, prompt, frame prompt, duration, transition or recipe.
    #[tool]
    async fn shot_set(
        &self,
        Parameters(p): Parameters<ShotSet>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        let mut args = a(["shot", "set", &p.shot]);
        opt(&mut args, "--title", p.title);
        opt(&mut args, "--plot", p.plot);
        opt(&mut args, "--prompt", p.prompt);
        opt(&mut args, "--frame-prompt", p.frame_prompt);
        opt(&mut args, "--duration", p.duration);
        opt(&mut args, "--transition", p.transition);
        opt(&mut args, "--recipe", p.recipe);
        self.cli(args, None, &ctx).await
    }

    /// Add a line of dialogue to a shot.
    #[tool]
    async fn shot_line(
        &self,
        Parameters(p): Parameters<Line>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        let mut args = a(["shot", "line", &p.shot, &p.who, &p.text]);
        opt(&mut args, "--at", p.at);
        self.cli(args, None, &ctx).await
    }

    /// List shots, all or one chapter's.
    #[tool]
    async fn shot_ls(
        &self,
        Parameters(p): Parameters<Chapter>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        let mut args = a(["shot", "ls"]);
        args.extend(p.chapter);
        self.cli(args, None, &ctx).await
    }

    /// Show a shot's file.
    #[tool]
    async fn shot_show(
        &self,
        Parameters(p): Parameters<ShotId>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        self.cli(a(["shot", "show", &p.shot]), None, &ctx).await
    }

    /// The exact prompt and references a shot would send. Free.
    #[tool]
    async fn prompt(
        &self,
        Parameters(p): Parameters<ShotId>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        self.cli(a(["prompt", &p.shot]), None, &ctx).await
    }

    /// Price a shot or a whole chapter. Free.
    #[tool]
    async fn quote(
        &self,
        Parameters(p): Parameters<Target>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        self.cli(a(["quote", &p.target]), None, &ctx).await
    }

    /// Generate a take of a shot. SPENDS MONEY. Without confirm it only shows the price.
    #[tool]
    async fn take(
        &self,
        Parameters(p): Parameters<Take>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        let mut args = a(["take", &p.shot]);
        opt(&mut args, "--from-step", p.from_step);
        opt(&mut args, "--of", p.of);
        self.spend(args, p.confirm, &ctx).await
    }

    /// Run one operation outside any shot, e.g. an upscale. SPENDS MONEY. Without confirm it only shows the price.
    #[tool]
    async fn run(&self, Parameters(p): Parameters<Run>, ctx: RequestContext<RoleServer>) -> Reply {
        let mut args = a(["run", &p.op, "--plugin", &p.plugin, "--model", &p.model]);
        opt(&mut args, "--prompt", p.prompt);
        opt(&mut args, "--duration", p.duration);
        for (k, v) in &p.inputs {
            args.push("--input".into());
            args.push(format!("{k}={v}"));
        }
        for r in &p.refs {
            args.push("--ref".into());
            args.push(r.clone());
        }
        settings(&p.params, "--param", &mut args);
        self.spend(args, p.confirm, &ctx).await
    }

    /// List a shot's takes; * marks the selected one.
    #[tool]
    async fn takes(
        &self,
        Parameters(p): Parameters<ShotId>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        self.cli(a(["takes", &p.shot]), None, &ctx).await
    }

    /// Pick the take a shot uses in the cut.
    #[tool]
    async fn select(
        &self,
        Parameters(p): Parameters<Select>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        self.cli(a(["select", &p.shot, &p.take]), None, &ctx).await
    }

    /// The cut: shots in order with their selected takes, and the music tracks.
    #[tool]
    async fn timeline(&self, ctx: RequestContext<RoleServer>) -> Reply {
        self.cli(a(["timeline"]), None, &ctx).await
    }

    /// Join the selected takes into one video with ffmpeg. Free.
    #[tool]
    async fn timeline_render(
        &self,
        Parameters(p): Parameters<Render>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        let mut args = a(["timeline", "render"]);
        opt(&mut args, "--output", p.output);
        opt(&mut args, "--xfade", p.xfade);
        if p.colormatch == Some(false) {
            args.push("--no-colormatch".into());
        }
        self.cli(args, None, &ctx).await
    }

    /// What the film has cost, and what's left of the budgets.
    #[tool]
    async fn cost(
        &self,
        Parameters(p): Parameters<Cost>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        let mut args = a(["cost"]);
        opt(&mut args, "--by", p.by);
        self.cli(args, None, &ctx).await
    }

    /// Every rate in use and where it comes from.
    #[tool]
    async fn cost_rates(&self, ctx: RequestContext<RoleServer>) -> Reply {
        self.cli(a(["cost", "rates"]), None, &ctx).await
    }

    /// Check nobody edited the ledger.
    #[tool]
    async fn cost_verify(&self, ctx: RequestContext<RoleServer>) -> Reply {
        self.cli(a(["cost", "verify"]), None, &ctx).await
    }

    /// Compare the ledger with what providers say they charged.
    #[tool]
    async fn cost_reconcile(
        &self,
        Parameters(p): Parameters<MaybePlugin>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        let mut args = a(["cost", "reconcile"]);
        opt(&mut args, "--plugin", p.name);
        self.cli(args, None, &ctx).await
    }

    /// Installed plugins and their operations and models.
    #[tool]
    async fn plugin_ls(&self, ctx: RequestContext<RoleServer>) -> Reply {
        self.cli(a(["plugin", "ls"]), None, &ctx).await
    }

    /// Check a plugin talks the protocol properly. Free.
    #[tool]
    async fn plugin_doctor(
        &self,
        Parameters(p): Parameters<PluginName>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        self.cli(a(["plugin", "doctor", &p.name]), None, &ctx).await
    }

    /// What's left on each provider account. Free.
    #[tool]
    async fn plugin_balance(
        &self,
        Parameters(p): Parameters<MaybePlugin>,
        ctx: RequestContext<RoleServer>,
    ) -> Reply {
        let mut args = a(["plugin", "balance"]);
        args.extend(p.name);
        self.cli(args, None, &ctx).await
    }
}

impl Server {
    /// Without `confirm`, answers no at the prompt so the reply is the price and nothing else.
    /// Budgets are checked when the money is reserved, before anything runs.
    async fn spend(
        &self,
        mut args: Vec<String>,
        confirm: bool,
        ctx: &RequestContext<RoleServer>,
    ) -> Reply {
        if confirm {
            args.push("--yes".into());
            return self.cli(args, None, ctx).await;
        }
        let r = self.cli(args, Some("n"), ctx).await?;
        if r.is_error == Some(true) {
            return Ok(r);
        }
        let mut content = r.content;
        content.push(ContentBlock::text(NOT_CONFIRMED));
        Ok(CallToolResult::success(content))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(rmcp::model::Implementation::new(
                "shotline",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(CO_DIRECTOR)
    }
}
