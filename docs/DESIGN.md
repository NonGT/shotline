# Design notes

Draft, Oct 2026. Only the planning part (M0) is built, expect the rest to change.

## Idea

Most AI video tools give you single clips. Making something watchable out of them means keeping
faces consistent across shots, giving characters voices, and hiding the seams between clips.
shotline is my attempt at handling that as a pipeline instead of by hand:

- characters, sets and props get anchor images that are attached to every shot they appear in
- characters get a voice, and dialogue lines become audio that drives lip sync
- a shot can either cut (new scene) or continue from the last frame of the previous shot's take
- takes get colour matched to the previous one and stitched with ffmpeg

shotline itself doesn't know any AI service. Everything that generates or processes media is a
plugin: video models, TTS, lip sync, upscalers, image generators, or someone's own tool. fal ships
built in, the rest are separate programs.

Everything is files in a project folder. No database, no server, no accounts. I don't want to
build a chat agent either. The CLI, an MCP server and the UI expose the same operations and
whatever MCP client you use plays co-director.

## Project folder

A film is its own folder, wherever you like. It has nothing to do with the shotline repo.

```
my-film/
  shotline.toml        defaults, recipes, budget, currency and rates
  story.md             logline, world, synopsis
  style.md             look of the film, camera/lens notes; goes into every prompt
  elements/
    cast/paka.toml     name, @handle, description, voice
    cast/paka/*.png    anchor images
    sets/...  props/...
  chapters/01-into-the-forest/
    chapter.toml
    shots/010.toml     ids go 010, 020, ... so there's room to insert
  jobs/<ulid>/         one folder per generation: job.toml + output files
  timeline.toml        optional: shot order, crossfades, colour match, music/sfx tracks
  renders/             rendered films
  prompts/             optional overrides for the prompt templates
  models.toml          optional overrides for model capabilities
  .shotline/ledger.jsonl
  .gitignore
```

If you keep the film folder in git, the `.gitignore` that `init` writes leaves out media under
`jobs/` (it gets big fast) but keeps job.toml files, anchors and the ledger. So the history of what
was made, from what, and for how much survives even if the media lives elsewhere.

Keys are never stored in the project, only read from the environment or `~/.config/shotline/.env`.

## Shots

```toml
title = "Rest stop, the team argues"
plot = "พักเท้ากลางทาง มะลิถ่ายคลิปเช็กอิน ดร.กันขัดผกาเรื่องแผน"
prompt = "Handheld medium shot, @mali films a check-in while @kan argues with @paka ..."
duration = 8
transition = "continue"    # or "cut"
recipe = "talking"         # optional, project default otherwise
elements = ["paka", "kan", "mali", "deep-forest"]
selected_take = "01JBX9Q3T7K2M8V5N4R6P1W0YZ"

[[lines]]
who = "kan"
text = "เราต้องไปต่อก่อนมืด"

[model]   # optional, overrides project defaults
plugin = "fal"
id = "minimax/h3-max/reference-to-video"
resolution = "768P"

[model.params]   # passed through to the plugin as is
prompt_expansion_mode = "disabled"
```

`elements` gets filled from the @handles in the prompt but can be edited by hand.

A cast member can have a voice:

```toml
[voice]
plugin = "elevenlabs"
voice_id = "..."
# or sample = "paka/voice.wav" for plugins that clone
```

Audio length matters: a shot's lines have to fit its duration, and models limit audio input (h3 takes
2-15s per clip, 15s total). `shotline prompt` warns before anything is paid for.

## Prompts

The prompt that actually gets sent is put together from a few parts:

1. the shot prompt
2. @handles swapped for whatever the model expects (`Image 1` on fal, `<Picture 1>` on Venice),
   with the anchor images attached in the same order
3. style.md
4. a short identity note (keep faces/outfits from the references)
5. for continue shots, a note that it starts on the given frame
6. per-model tweaks

Each part can be turned off in shotline.toml. The templates for 3-6 are plain files in `prompts/`
so tuning them doesn't need a rebuild. `shotline prompt <shot>` prints the result without
spending anything.

## Operations

shotline core works with a fixed set of operations it understands, so it can chain them and check
that an output fits the next input before anything is paid for:

```
image.generate     prompt (+ refs) -> image            anchors, storyboard frames
image.edit         image + prompt -> image             fix an anchor, remove a background
video.generate     prompt + refs/start/end/audio -> video   t2v, i2v, r2v depending on inputs
video.lipsync      video + audio -> video
video.upscale      video -> video
video.interpolate  video -> video
audio.tts          text + voice -> audio
audio.music        prompt -> audio
audio.sfx          prompt -> audio
audio.transcribe   audio -> text with timings
x.<name>           files + params -> files and/or JSON, for anything else
```

`x.*` is the escape hatch: core doesn't know what it means, but it still runs it, records it and
counts its cost. That's how someone's own tool gets in without changes to shotline.

Colour match, last frame, concat and the like are built-in operations backed by ffmpeg, and go
through the same path.

## Plugins

A plugin is an executable that talks JSON-RPC 2.0 over stdin/stdout (spec in PLUGINS.md). It can be written in anything;
my Python experiments can be wrapped as they are. shotline starts it on demand and keeps it running
for the session.

Found in `~/.config/shotline/plugins/<name>/` (with a `shotline-plugin.toml` naming the executable
and protocol version) or as `shotline-plugin-<name>` on PATH.

```
describe  -> name, version, protocol version, env vars it needs, and for each model:
             operation, accepted inputs (JSON Schema), limits (refs, durations, resolutions,
             start frame with refs, audio input), how refs are named in the prompt
quote     -> price for a request: amount + unit (USD, or the provider's own credits)
run       -> job handle (or the result right away for quick jobs)
poll      -> queued | running | done | failed, progress
fetch     -> writes outputs into a folder shotline gives it
cancel
rates     -> optional: what one of its credit units costs
balance   -> optional: what's left on the account, free to read
usage     -> optional: what the provider actually charged, for reconciling
```

Rules:

- Plugins are stateless about the project. shotline passes absolute input paths and an output
  folder; plugins never read or write the project themselves.
- Only the env vars a plugin declares in `describe` are passed to it.
- Errors come back in one shape: `{kind: validation | content_policy | auth | quota | other,
  message, field}`. `field` points into the request (`references[2]`, `audio`), which core maps
  back to the shot, element or file it came from.
- Quoting, confirmation, budget and the ledger live in core. A plugin can only answer what
  something costs; it can't spend on its own.

Built-in plugins (fal for now) implement the same Rust trait that wraps external ones, and are
tested with the same conformance kit:

```rust
trait Plugin {
    fn describe(&self) -> Result<Description>;
    async fn quote(&self, req: &Request) -> Result<Quote>;
    async fn run(&self, req: &Request) -> Result<Handle>;
    async fn poll(&self, h: &Handle) -> Result<Status>;
    async fn fetch(&self, h: &Handle, dest: &Path) -> Result<Vec<Output>>;
    async fn cancel(&self, h: &Handle) -> Result<()>;
}
```

`Request` is generic: operation, model, prompt, duration, resolution, aspect, references (image,
audio, video), start/end frame, seed, and `params` passed through from the shot.

What a plugin says about its models can be overridden in the project's `models.toml`.

First plugins: fal (built in, video), and in `plugins/` Venice (video), ElevenLabs (TTS) and
WaveSpeed (`sync/lipsync-3` for lip sync), installed with `cargo install --path plugins/<name>`.
ElevenLabs quotes in `elevenlabs-credit` (one per character) and works out what a credit costs
from the subscription (next invoice over the month's credits; the key needs `user_read`).
WaveSpeed quotes in USD from the `base_price` its model list gives per second of video.

## Jobs and takes

Every generation is a job with a ULID, in `jobs/<ulid>/`:

```toml
op = "video.lipsync"
plugin = "wavespeed"
model = "sync/lipsync-3"
shot = "01-020"
take = "01JBX9Q3T7K2M8V5N4R6P1W0YZ"
inputs = ["01JBX9Q1...", "01JBX9Q2..."]   # other jobs, or project files
params = { sync_mode = "silence" }
status = "done"
cost = { amount = 0.38, unit = "USD" }
```

A take is one run of a shot's recipe, so it's a chain of jobs, for example tts -> video -> lipsync
-> upscale. The take's output is the last job's output. Jobs are never edited after they finish.

That gives you:

- where any file came from and what it was made from
- the full cost of a take, not just the video step
- redoing only the last step (say a different upscaler) without paying for the rest again

ULIDs are long, so the CLI and UI also show takes by order within the shot (`01-020#3`). That number
is worked out from the ULIDs and never stored, so it can't clash between two copies of a project.

## Recipes

Named in shotline.toml, picked per shot with `recipe = "talking"`. A shot without one runs a single
video.generate.

```toml
[[recipe.talking]]
op = "audio.tts"              # one job per line, with the speaker's voice

[[recipe.talking]]
op = "video.generate"         # always the shot's own model
audio = true                  # send the lines' audio along

[[recipe.talking]]
op = "video.lipsync"
plugin = "wavespeed"
model = "sync/lipsync-3"
params = { sync_mode = "silence" }
```

Before a take runs, every step is quoted and listed with the total, then it waits for a yes. All
steps are reserved up front, so a take that wouldn't fit the budget never starts. Each step's
output feeds the next; the first failure ends the take and releases what the rest held.
`shotline take 01-010 --from-step 2` reuses the earlier steps of the latest take (or `--of 3`) and
only pays for the rest.

Lip sync takes one audio clip, so for now it needs a shot with one line; joining several comes
with the media work.

## Continuity

For a continue shot, the last frame of the previous shot's selected take is sent as the start frame
along with the references. Not every model accepts a start frame and references together (Venice's
h3 doesn't); `shotline prompt` says so before you pay for anything.

The frame is taken with ffmpeg the first time a take of the continue shot is quoted, and kept as
`jobs/<job>/last.png` next to the video it came from.

Colour matching happens when the film is rendered, not when a take comes back, since the take
before can still change. Each clip gets one fixed MKL colour transform (Pitie 2007), from the
stats of its first frames to those of the previous clip's last frames after their own correction,
so a whole chain lines up and nothing flickers. By default only continue shots are matched; a cut
is usually a new look on purpose.

```toml
# timeline.toml, all optional
shots = ["01-010", "01-030", "01-020"]   # play order; default is every shot in order
xfade = 0.0                              # crossfade at joins, seconds

[colormatch]
at = "continue"     # continue | all | none
frames = 5          # frames each side of a join used for the stats
strength = 1.0

[[track]]
file = "audio/theme.mp3"
at = 0.0            # seconds into the film
volume = 0.5
```

`shotline timeline` lists the cut with the selected takes. `shotline timeline render` decodes each
clip to the first one's size and frame rate, colour matches, writes lossless intermediates under
`.shotline/cache/`, then encodes once to H.264/AAC in `renders/` (ignored by git), with each
clip's own sound kept in sync and the tracks mixed over it.

## Money

Anything that costs anything is quoted first and waits for a yes (`--yes` to skip).

The ledger, `.shotline/ledger.jsonl`, is append-only. Lines are never changed; corrections are new
lines. Each line has the hash of the one before it, and `.shotline/ledger.head` holds the last seq
and hash, so `shotline cost verify` can tell if anything was edited, removed, reordered or cut off.
Writes hold a lock, and the budget check happens inside it.

```json
{"seq":57,"time":"2026-10-30T10:12:03Z","type":"charge","job":"01JBX9Q3...","take":"01JBX9Q0...",
 "shot":"01-020","op":"video.lipsync","plugin":"wavespeed","model":"sync/lipsync-3",
 "units":8,"unit_name":"s","amount":0.38,"unit":"USD","rate":36.5,"rate_source":"config",
 "base":13.87,"base_currency":"THB","prev":"<hash>"}
```

Entry types:

- `reserve` before a job is sent, for the quoted amount, so parallel jobs can't overshoot the budget
- `charge` when it finishes, for what it actually cost
- `release` when it fails and nothing was charged; failed jobs that were still charged (content
  policy hits often are) get a `charge`
- `refund` and `adjust` for anything fixed afterwards

### Currencies and credits

Every amount is stored in the unit the provider used (USD, Venice credits, whatever) along with
what it came to in the project's currency and the rate used at the time (a charge uses the rate of
the day it's charged). Old lines are never recalculated when rates change. If the film's currency
is changed later, reports convert older lines at today's rates.

```toml
[money]
currency = "USD"          # the project's currency; budgets and totals use it

[rates]                   # what one unit is worth, in any currency
venice-credit = "0.004 USD"
USD = "36.5 THB"
```

Rates come from the plugin's `rates`, then `~/.config/shotline/config.toml`, then shotline.toml;
later ones win, since what you actually
paid for credits can differ from list price. Rates chain (credit -> USD -> THB). If a cost can't be
converted to the project currency, the job doesn't run until a rate is set, so nothing gets spent
uncounted.

### Measured from balances

Plugins that implement `balance` (Venice, WaveSpeed, ElevenLabs) have it read before a job and
after it finishes, re-read a few times if billing is slow. When nothing else of that plugin is
running, the drop is written on the charge line as `measured`, next to the booked amount and never
instead of it. Someone using the provider's website at the same time still muddles it, so it's a
check, not the truth.

When a job booked in a provider unit (credits, seconds) and the balance is money, the drop gives a
rate for that unit. Those go to `~/.config/shotline/measured-rates.toml`, apart from everything
else, averaged over samples (one far off the average is dropped). They're only used for a unit no
plugin and no config has a rate for; quotes then say "at a measured rate", and `shotline cost`
splits what was spent by where its price came from: the provider's, your rates, measured.
`shotline cost rates` lists every rate, where it's from and which one wins. `shotline plugin
balance` shows what's left with each provider, and takes show it before asking to spend.

### Budgets

Hard stops, in the project currency: `per_job`, `per_take`, `per_chapter`, `per_day` and `total`
under `[budget]`. Money held by running jobs counts. Every line is also written to
`~/.config/shotline/spend.jsonl`, and `[budget] per_day` in `~/.config/shotline/config.toml` (in
that file's `[money] currency`) stops spending across all films, since it's all the same wallet.

### Reports

`shotline cost` shows totals by chapter, shot, operation, plugin and day, in the project currency
and per provider unit (to compare with the provider's dashboard). It also splits what's in the cut
(selected takes) from what was thrown away, and shows what each second of the final film cost.
`--csv` exports it.

`shotline cost reconcile` lists jobs whose balance dropped by a different amount than was booked,
then pulls `usage` from plugins that support it and lists any differences from what the ledger
says.

## Commands

```
shotline init <dir>
shotline doctor
shotline plugin ls | doctor <name> | balance [name]
shotline element add <kind> <name> [--handle h] [--desc ...]
shotline element anchor <handle> (--file img | --generate)
shotline element voice <handle> --plugin p [--model m] [--voice-id id] [--sample f] [--set k=v]
shotline element ls
shotline chapter add <title>
shotline chapter ls
shotline shot add <chapter> [--after id] --plot ... --prompt ... [--duration n] [--transition cut|continue]
shotline shot add ... [--plugin p] [--model m] [--recipe r]
shotline shot line <shot> <who> <text>
shotline shot ls [chapter]
shotline shot show <id>
shotline prompt <shot>
shotline quote <shot|chapter>
shotline take <shot> [--from-step n [--of take]] [--yes]
shotline takes <shot>
shotline select <shot> <take>
shotline run <op> --plugin p --model m [--prompt ..] [--duration s] [--input name=file]
             [--ref file] [--param k=v] [--yes]          one-off operation outside a shot
shotline timeline [render [-o file] [--no-colormatch] [--xfade s]]
shotline cost [--by chapter|shot|op|plugin|day] [--csv]
shotline cost verify | rates | reconcile [--plugin p]
shotline cost adjust <amount> <unit> --note ...
shotline cost refund <job> <amount> <unit> [--note ...]
shotline mcp
shotline ui
```

The MCP tools mirror these. Anything that spends refuses to run without an explicit confirm flag or
when it would go over budget. There'll be a short `agent/CO_DIRECTOR.md` describing the workflow for
the agent: story, elements, anchors, voices, shot breakdown, preview and quote, takes, pick, render.

## UI

A native egui/eframe app, opened with `shotline ui [dir]`. No webview: it should start instantly
and stay light. It browses story, elements, shots and takes, compares takes side by side, plays
the timeline and shows costs. Plugin operations show up in it without plugins shipping any UI.

egui can't play video on its own. Instead of linking libav, the player runs ffmpeg as a process like
everything else: `ffmpeg -ss <t> -i take.mp4 -f rawvideo -pix_fmt rgba -s <preview size> -` feeds
frames into a texture, a second ffmpeg pipes PCM to `cpal`, and the audio clock drives the frames.
Seeking restarts ffmpeg at the new time. Takes are 5-15s at preview size, so this is cheap. Until the
player exists, "play" opens the file in mpv or ffplay.

The UI only goes through shotline-core, same as the CLI and MCP, so all three always agree on what's
in the project folder. Paid actions still quote and ask first.

## Code layout

```
Cargo.toml             workspace, shared dependency versions
assets/
  models.toml          default model capabilities, embedded in the binary
  prompts/             default prompt templates, embedded; a project's prompts/ overrides them
crates/
  shotline-core        project folder, elements, shots, recipes, jobs, prompt building
  shotline-ledger      ledger, budgets, reserve/charge, rates, reports, verify, reconcile
  shotline-protocol    plugin protocol types only (serde), shared by core and plugins
  shotline-plugin      Plugin trait, external process plugins, discovery, conformance kit,
                       fake plugin for tests
  shotline-builtin     fal (reqwest + rustls, tokio)
  shotline-engine      plan, quote, reserve, run, poll, fetch, book; shared by CLI, MCP and UI
  shotline-media       ffmpeg/ffprobe: last frame, colour match, concat, frame/PCM pipes
  shotline-ui          egui app
  shotline-cli         the binary, clap; `mcp` via rmcp, `ui` behind the `ui` cargo feature
plugins/
  venice/  elevenlabs/  wavespeed/   external plugins, built and installed separately
```

shotline-core knows nothing about HTTP or ffmpeg. The CLI, MCP server and UI are thin layers on top
of it.

Release binaries for mac/linux/windows from CI. ffmpeg has to be installed separately,
`shotline doctor` checks for it.

## Testing

- Unit tests next to the code (`#[cfg(test)]`): handle parsing, shot ids, TOML loading, model limits,
  rate conversion, ledger hashing.
- Snapshot tests with `insta` for built prompts, plugin requests and error messages, so a template
  change shows up as a diff.
- CLI end-to-end tests with `assert_cmd` + `assert_fs`: run the real binary in a temp dir through
  init, element, shot, prompt, take (against the fake plugin) and so on.
- Plugin conformance kit: runs any plugin, built-in or external, through describe/quote/run/poll/
  fetch/errors against expected behaviour. External plugins run it in their own CI.
- HTTP plugins against `wiremock`, using real responses and errors saved as fixtures. Base URLs are
  configurable for this. CI never calls a paid API; live checks are `#[ignore]`d, need
  `SHOTLINE_LIVE=1` and only hit free endpoints (quotes, pricing).
- Ledger: budgets hold under parallel jobs, failed jobs release, verify catches an edited line.
- Media tests make tiny clips with ffmpeg `testsrc` at test time and skip when ffmpeg is missing.
- MCP tests spawn `shotline mcp` and talk JSON-RPC over stdio. Spending must refuse without
  confirmation or over budget.
- UI tests with `egui_kittest`.
- Time and ids are injectable so ledgers and job ids are deterministic in tests.

CI runs `cargo fmt --check`, `cargo clippy -- -D warnings` and `cargo test` on mac, linux and
windows.

## Order of work

1. workspace, data model, init/element/shot/prompt (no network) - done
2. plugin protocol, Plugin trait, fake plugin, conformance kit, jobs and takes - done
3. ledger: reserve/charge, budgets, rates and currencies, `cost`, `verify` - done
4. fal built in: quote, take, select, errors - done
5. external plugins: Venice, ElevenLabs (voices, lines), WaveSpeed lip sync; recipes - done
6. continuity: last frame chaining, colour match, timeline render with audio tracks - done
7. mcp server and the co-director notes
8. anchor image generation, storyboard frames, upscale/interpolate
9. egui UI: browse the project, costs, open takes in mpv/ffplay
10. built-in player over ffmpeg pipes, side by side takes, timeline playback

## Not sure yet

- Should shotline translate Thai plot text into English prompts, or leave that to the agent?
- Are storyboard frames worth paying for before every video, or just an option?
- Colour matching gets harder when the previous take came from a different model.
- Hooks (run something after every take) or are recipes enough?
- Should plugins be able to add their own CLI commands, or only operations?
