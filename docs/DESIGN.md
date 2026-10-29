# Design notes

Draft, Oct 2026. Nothing is built yet, expect this to change.

## Idea

Most AI video tools give you single clips. Making something watchable out of them means keeping
faces consistent across shots and hiding the seams between clips. shotline is my attempt at
handling that as a pipeline instead of by hand:

- characters, sets and props get anchor images that are attached to every shot they appear in
- a shot can either cut (new scene) or continue from the last frame of the previous shot's take
- takes get colour matched to the previous one and stitched with ffmpeg

Everything is files in a project folder so it can live in git. No database, no server, no accounts.
I don't want to build a chat agent either. The CLI and an MCP server expose the same operations and
whatever MCP client you use plays co-director.

## Project folder

```
my-film/
  shotline.toml        project defaults: aspect, provider, model, resolution, budget
  story.md             logline, world, synopsis
  style.md             look of the film, camera/lens notes; goes into every prompt
  elements/
    cast/paka.toml     name, @handle, description
    cast/paka/*.png    anchor images
    sets/...  props/...
  chapters/01-into-the-forest/
    chapter.toml
    shots/010.toml     ids go 010, 020, ... so there's room to insert
  takes/01-010/t001/   request.json (exact prompt, params, cost), video.mp4, last.png
  timeline.toml        shot order + selected takes
  prompts/             optional overrides for the prompt templates
  .shotline/ledger.jsonl
```

Keys are never stored in the project, only read from the environment or a user-level .env.

A shot looks like this:

```toml
title = "Rest stop, the team argues"
plot = "พักเท้ากลางทาง มะลิถ่ายคลิปเช็กอิน ดร.กันขัดผกาเรื่องแผน"
prompt = "Handheld medium shot, @mali films a check-in while @kan argues with @paka ..."
duration = 8
transition = "continue"    # or "cut"
elements = ["paka", "kan", "mali", "deep-forest"]
selected_take = "t002"

[model]   # optional, overrides project defaults
provider = "fal"
id = "minimax/h3-max/reference-to-video"
resolution = "768P"
```

`elements` gets filled from the @handles in the prompt but can be edited by hand.

Takes are never edited after they're made. A shot can have as many as you like and one of them is
selected.

## Prompts

The prompt that actually gets sent is put together from a few parts:

1. the shot prompt
2. @handles swapped for whatever the provider expects (`Image 1` on fal, `<Picture 1>` on Venice),
   with the anchor images attached in the same order
3. style.md
4. a short identity note (keep faces/outfits from the references)
5. for continue shots, a note that it starts on the given frame
6. per-model tweaks

Each part can be turned off in shotline.toml. The templates for 3-6 are plain files in `prompts/`
so tuning them doesn't need a rebuild. `shotline prompt <shot>` prints the result without
spending anything.

## Continuity

For a continue shot, the last frame of the previous shot's selected take is sent as the start frame
along with the references. Not every model accepts a start frame and references together; if the
chosen one doesn't, `shotline prompt` will say so before you pay for anything.

After a take comes back it's colour matched against the previous selected take (port of my
`colormatch_concat.py` experiment). `shotline timeline render` joins everything with ffmpeg.

## Providers

fal and Venice to start with. Roughly:

```rust
trait Provider {
    async fn quote(&self, req: &GenRequest) -> Result<Quote>;
    async fn submit(&self, req: &GenRequest) -> Result<JobId>;
    async fn poll(&self, job: &JobId) -> Result<JobStatus>;
    async fn fetch(&self, job: &JobId, dest: &Path) -> Result<Output>;
}
```

What each model accepts (number of refs, allowed durations, resolutions, start/end frames, audio
input, how refs are named in the prompt) lives in a `models.toml`, so adding a model shouldn't
need code. API validation errors get mapped back to the shot or file they're about.

## Money

Anything that costs credits gets quoted first and waits for a yes (`--yes` to skip). There's a
per-take and per-day budget in shotline.toml that hard-stops. Every spend goes in the ledger and
`shotline cost` shows totals plus what each second of the final cut actually cost.

## Commands

```
shotline init <dir>
shotline doctor
shotline element add <kind> <name> [--handle h] [--desc ...]
shotline element anchor <handle> (--file img | --generate)
shotline shot add <chapter> [--after id] --plot ... --prompt ... [--duration n] [--transition cut|continue]
shotline shot ls [chapter]
shotline shot show <id>
shotline prompt <shot>
shotline quote <shot|chapter>
shotline take <shot> [--yes]
shotline select <shot> <take>
shotline timeline render [--no-colormatch]
shotline cost
shotline mcp
```

The MCP tools mirror these. `generate_take` refuses to run without an explicit confirm flag or when
it would go over budget. There'll be a short `agent/CO_DIRECTOR.md` describing the workflow for
the agent: story, elements, anchors, shot breakdown, preview and quote, takes, pick, render.

## Code layout

```
crates/
  shotline-core        types, reading/writing the project folder, validation, prompt building
  shotline-providers   fal, venice (reqwest + rustls, tokio)
  shotline-media       ffmpeg/ffprobe: last frame, colour match, concat
  shotline-cli         the binary, clap; `mcp` subcommand via rmcp
```

Release binaries for mac/linux/windows from CI. ffmpeg has to be installed separately,
`shotline doctor` checks for it.

## Order of work

1. workspace, data model, init/element/shot/prompt (no network)
2. fal: quote, take, ledger, decent errors
3. continuity: last frame chaining, colour match, timeline render
4. mcp server and the co-director notes
5. Venice, anchor image generation, storyboard frames
6. maybe a small viewer (TUI or local web page) for browsing takes

## Not sure yet

- Should shotline translate Thai plot text into English prompts, or leave that to the agent?
- Are storyboard frames worth paying for before every video, or just an option?
- Colour matching gets harder when the previous take came from a different model.
