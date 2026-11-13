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
    cast/paka.toml     name, @handle, description, voice, [labels] for anchors
    cast/paka/*.png    anchor images
    sets/...  props/...
  chapters/01-into-the-forest/
    chapter.toml
    shots/010.toml     ids go 010, 020, ... so there's room to insert
  jobs/<ulid>/         one folder per generation: job.toml + output files
  timeline.toml        optional: shot order, crossfades, colour match, music/sfx tracks
  bridges/<ulid>.toml  clips made to join two clips of an editor's sequence (see Editors)
  renders/             rendered films
  shared/              files shared with other apps, e.g. a video editor's project (see Editors)
  prompts/             optional overrides for the prompt templates
  models.toml          optional overrides for model capabilities
  .shotline/ledger.jsonl
  .gitignore
```

If you keep the film folder in git, the `.gitignore` that `init` writes leaves out media under
`jobs/` (it gets big fast) but keeps job.toml files, anchors and the ledger. So the history of what
was made, from what, and for how much survives even if the media lives elsewhere.

Keys are never stored in the project, only read from the environment or `~/.config/shotline/.env`.
The app's Preferences (Cmd+,) has a page per plugin, listed on the left with a dot on one missing
a key or not answering. A page has the keys it needs, written to that file (mode 600) and checked
by asking the plugin for its balance, which is free; where its program is, with the copies on PATH
that aren't used and a button to use one instead; and each model's options for the open film.
Nothing is written until OK, and Cancel or Escape throws the changes away (Escape asks first). The
film's options go into shotline.toml as one step that can be undone. Asking for a plugin's keys
elsewhere, e.g. Keys… under a voice that couldn't be listed, opens Preferences at that plugin.

## Shots

```toml
title = "Rest stop, the team argues"
plot = "พักเท้ากลางทาง มะลิถ่ายคลิปเช็กอิน ดร.กันขัดผกาเรื่องแผน"
prompt = "Handheld medium shot, @mali films a check-in while @kan argues with @paka ..."
duration = 8
transition = "continue"    # or "cut"
continue_from = "01JBX9Q4..."   # continue shots: the take of the shot before it starts on
recipe = "talking"         # optional, project default otherwise
elements = ["paka", "kan", "mali", "deep-forest"]
selected_take = "01JBX9Q3T7K2M8V5N4R6P1W0YZ"
frame = "chapters/01-forest/shots/010.png"   # optional storyboard frame, cut shots only

[[lines]]
who = "kan"
text = "เราต้องไปต่อก่อนมืด"
audio = "01JBX9Q1..."   # optional, the recording takes send (shot say); text is what it says

leave_out = ["elements/cast/paka/02.png"]   # optional, anchors this shot doesn't send

[anchor_notes]   # optional, what an anchor shows in this shot; the prompt gets "Image 1: from behind."
"elements/cast/paka/01.png" = "from behind"

[english]   # written by take --translate; sent instead while prompt is still `from`
from = "@paka เดินเข้าป่า"
text = "@paka walks into the forest"

[[image]]   # optional, the shot's own pictures, kept in shots/010/
file = "chapters/01-forest/shots/010/01.png"
note = "the rest stop by the stream"

[[sound]]   # optional, laid over a take in the cut; the model never hears it
take = "01JBX9Q3T7K2M8V5N4R6P1W0YZ"
job = "01JBXA2..."   # the file, copied into the job's folder
at = 1.5             # seconds into the take

[model]   # optional, overrides project defaults
plugin = "fal"
id = "minimax/h3-max/reference-to-video"
resolution = "768P"

[model.params]   # passed through to the plugin as is
prompt_expansion_mode = "disabled"
```

`elements` gets filled from the @handles in the prompt but can be edited by hand.

`frame` is a still of the shot's first frame, made with `shotline frame` or given as a file. A cut
shot starts on it, which is a cheap way to agree on framing before paying for video. Continue shots
ignore it, they start on the previous take's last frame.

A cast member can have a voice:

```toml
[voice]
plugin = "elevenlabs"
voice_id = "..."
```

Audio length matters: a shot's lines have to fit its duration, and models limit audio input (h3 takes
2-15s per clip, 15s total). `shotline prompt` warns before anything is paid for.

## Prompts

The prompt that actually gets sent is put together from a few parts:

1. the shot prompt
2. @handles swapped for whatever the model expects (`Image 1` on fal, `<Picture 1>` on Venice),
   with the anchor images attached in the same order. An element with several anchors becomes
   "Paka (shown in Image 1 and Image 2)"; "Image 1 and Image 2 walks" gets you two people.
   The shot's own pictures come after, each with its note: "Image 5: the rest stop by the
   stream." (`shot-image.md`)
3. style.md
4. a short identity note (keep faces/outfits from the references). Images can leave the outfit
   out (`--any-outfit`, or untick it in Make image) to try other clothes on the same person
   (`identity-look.md`)
5. for continue shots and storyboard frames, a note that it starts on the given frame
6. per-model tweaks

Each part can be turned off in shotline.toml. The templates for 3-6 are plain files in `prompts/`
so tuning them doesn't need a rebuild. `shotline prompt <shot>` prints the result without
spending anything.

Images use the same parts with their own templates: `anchor.md` for an element's reference image
(plain background, even light, the element's description) and `storyboard.md` for a shot's first
frame. Which plugin and model make them is set in shotline.toml:

```toml
[image]
plugin = "fal"                 # or "venice"
model = "fal-ai/nano-banana"
aspect = "9:16"                # defaults to the project's
```

With references attached, fal goes to the model's `/edit` endpoint and Venice to `multi-edit`.

Plot can be written in Thai and the prompt drafted from it by a text model (`text.generate`).
`shot draft` fills its template with the plot, the elements and their @handles, who speaks, the
current prompt and story.md cut down (no title or empty sections, at most 1500 characters).
`draft-video.md` asks for a video prompt, `draft-image.md` for a still (saved as the shot's
`frame_prompt`, which storyboard frames use instead of the prompt) and `translate.md` for a plain
translation, which gets the plot alone. `element draft` translates a description. Lines are never sent, they
stay Thai for TTS and lip sync. A draft that uses a @handle that isn't an element is refused. It
shows the draft and asks before saving, unless `--write`.

```toml
[text]
plugin = "venice"
model = "claude-haiku-5-5"
```

The agent can still write prompts itself over MCP; this is for working without one.

Each job can have its own default model, picked in the app under Preferences, Default Models, which
shows how each model is charged (the plugin's `price`, a guide; the quote is still what counts):

```toml
plugin = "fal"                 # video: a shot's takes, unless the shot picks its own
model = "minimax/h3-max/reference-to-video"
[image]                        # anchors, shot pictures, storyboard frames
[text]                         # drafting a shot's prompt from its plot
[translate]                    # putting what's typed into English; [text] when not set
[tts]                          # a voice on this plugin that names no model, and new voices
[lipsync]                      # the lip-synced recipe; wavespeed sync/lipsync-3 when not set
[assistant]                    # kept for an assistant in the app; nothing uses it yet
```

Each table has `plugin` and `model`.

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
text.generate      prompt -> text                      draft English prompts from Thai plot
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
and protocol version) or as `shotline-plugin-<name>` on PATH. The first one on PATH wins; the rest
are listed as not used, and `plugin pin` keeps one in the plugins folder so it wins over PATH.

A model's options (`params_schema`) can be set for the whole film in shotline.toml, under the
plugin and model. A shot's `[model.params]` or a recipe step's `params` win over them:

```toml
[params.fal."minimax/h3-max/reference-to-video"]
enable_safety_checker = false
```

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
sequences -> editor plugins: the film's projects in that editor, their sequences and clips
open      -> editor plugins: open the film's project in the editor, making it if there's none
import    -> editor plugins: a finished take, to have at hand in the editor
```

Rules:

- Plugins are stateless about the project. shotline passes absolute input paths and an output
  folder; plugins never read or write the project themselves. Editor plugins are the exception:
  they read and write under `shared/`, and keep their own state in `.shotline/cache/`.
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

First plugins: fal (built in, video), and in `plugins/` Venice (video), ElevenLabs (TTS, sound effects) and
WaveSpeed (`sync/lipsync-3` for lip sync), installed with `cargo install --path plugins/<name>`.
ElevenLabs quotes in `elevenlabs-credit` (one per character) and works out what a credit costs
from the subscription (next invoice over the month's credits; the key needs `user_read`).
Its sound effects (`eleven_text_to_sound_v2` and `_v3`, 0.5-30 s) are quoted at 40 credits a
second with a length set and 200 without, as ElevenLabs' docs give it; the balance read after the
run books what it really took.
WaveSpeed quotes in USD from the `base_price` its model list gives per second of video.
fal and Venice also make images (nano-banana, seedream and others). `plugins/local` is free and
runs on the machine: `ffmpeg/minterpolate` and `ffmpeg/lanczos` for interpolation and upscaling,
and `rife` when `rife-ncnn-vulkan` is on PATH.

## Jobs and takes

Every generation is a job with a ULID, in `jobs/<ulid>/`:

```toml
op = "video.lipsync"
plugin = "wavespeed"
model = "sync/lipsync-3"
shot = "01-020"
take = "01JBX9Q3T7K2M8V5N4R6P1W0YZ"
inputs = ["01JBX9Q1...", "01JBX9Q2..."]   # other jobs, or project files
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
A removed take has its jobs marked `discarded`: its files stay because the ledger points at them,
it drops off the list, and the others keep their numbers.

Two kinds of take are made here, for nothing, as a single job by plugin `shotline`. A take from
pieces (`x.splice`) keeps its pieces in the job, so it can be opened again and changed into a new
take:

```toml
op = "x.splice"
plugin = "shotline"
model = "pieces"

[[params.pieces]]
take = "01JBX9Q3T7K2M8V5N4R6P1W0YZ"
from = 0.0
to = 3.2

[[params.pieces]]
take = "01JBXA0R2C8N6M4V1K7P3W9QZT"
from = 2.1
to = 6.0
join = "fade"      # cut (default) | fade | smooth
```

Each piece is colour matched to the end of the one before (MKL, as at render time). A fade
overlaps the two by 0.3s; smooth puts 0.25s of frames made by ffmpeg's minterpolate between them,
for two pieces that almost line up. Sound follows the pictures. A take brought in (`x.import`) is a
copy of a file made somewhere else, e.g. cut in an editor; `brief` keeps the file's name. Either
is selected, continued from and rendered like any other take.

## Recipes

Named in shotline.toml, picked per shot with `recipe = "talking"`. A shot without one runs a single
video.generate, so its lines aren't spoken.

Two come with every film, and a recipe of the same name in shotline.toml takes their place:
`spoken` (tts, then video.generate with `audio = true`, the video model lip syncs) and
`lip-synced` (tts, video.generate, then video.lipsync on the film's `[lipsync]`, wavespeed
sync/lipsync-3 unless set). The app's
"Lines spoken" dropdown above a shot's lines picks between them, none, and the film's own.

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
```

Before a take runs, every step is quoted and listed with the total, then it waits for a yes. All
steps are reserved up front, so a take that wouldn't fit the budget never starts. Each step's
output feeds the next; the first failure ends the take and releases what the rest held.
`shotline take 01-010 --from-step 2` reuses the earlier steps of the latest take (or `--of 3`) and
only pays for the rest.

Free steps fit on the end, for example `op = "video.upscale"`, `plugin = "local"`,
`model = "ffmpeg/lanczos"`, `params = { scale = 2 }`, or `video.interpolate` with `rife`.

A line can be recorded on its own first to hear how it comes out (`shotline shot say 01-010 1`).
Recordings belong to the speaker (`who` in the tts job), whether made alone or by a take, so
every version of the words, tags and all, stays there to go back to, from any shot. The one
picked goes in the line's `audio` and the line's text becomes what it says; a take reuses it
instead of paying for tts again. A recording that's thrown out goes, unless a take used it; then
it's only marked `discarded`.

Lip sync takes one audio clip. A shot with several lines (or a line with `at`) gets them mixed
into `lines.wav` in the lip sync job's folder first: each line starts at its `at` seconds, or 0.3s
after the line before. Redoing lip sync with `--from-step` joins the same lines again for free.

When the video model gets the lines itself ("by the video model"), its plugin says how. A model
with `audio_mix` (Venice's h3) gets the same kind of mix, made in the video job's folder, as its
one soundtrack. Any other model gets each line as its own clip, so a shot can have no more lines
than its `max_audio` (3 for fal's h3); the app stops offering Add line there and a take is refused.
Either way the plugin is told each line's speaker, words, clip, start and the pictures the speaker
is in, and writes them into the prompt its own way: fal's says whose voice and when each starts,
Venice's says whose lips move from when.

Venice's h3 takes the lines' audio only along with at least one reference image
(`audio_needs_image` in its model info), so a take that would send audio alone is refused before
anything is spent. It also won't take a start frame along with references or audio, so a cut
shot's storyboard frame goes there as `<Picture 1>` instead, the anchors are numbered from 2, and
the prompt asks it to open on that picture (`prompts/frame-ref.md`). It usually does, though not
to the pixel, and the app says so in amber rather than refusing. Models that declare
`frame_as_reference` get this; with nothing else to send, the frame stays a real start frame.

## Continuity

For a continue shot, the last frame of a take of the previous shot is sent as the start frame along
with the references. The shot keeps which take in `continue_from`: the one in the cut when continue
was turned on, or when no take is named, the one in the cut now. Putting another take of the shot
before in the cut asks first and leaves the continue shot on the old frame; the shot's page then
offers the new one ("Start on #3's last frame instead", or `shot set --continue-from`). A take a
continue shot starts on can't be removed. Venice's h3 won't take a start frame beside references, so
there the frame goes as one more reference picture that the prompt calls the first frame: the shot
opens close to it rather than exactly on it. fal's h3 takes both.

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

## Editors

A film can be cut in a real video editor next to shotline, from the same folder. The editor is
linked by a plugin that says `"editor": true` (FilmCraft for now, `plugins/filmcraft`), so the core
and the app don't know about any editor, and Resolve or Kdenlive could be linked the same way
through OTIO or FCPXML.

The editor's project lives under `shared/`, `shared/<plugin>/` for one made from shotline. shotline
never writes it: an editor writes the whole file on save and doesn't merge, so anything written
into it would be lost on the next save. Changes go through the editor's own commands instead. Files
handed to the editor (`cut.otio`) sit next to it.

What's read: `sequences` gives each project's sequences, tracks and clips in seconds. Once there's
an editor project, its sequence is the film's cut: the Cut page shows it in place of shotline's own
(the project shotline made first, with a picker when there's more than one sequence). shotline's
cut, with its joins, music and render, only shows while there's no editor project, and is what
starts one. `timeline.toml` stays as it was, so taking the project away brings it back. A clip from a take's
job folder is named by its take. The Cut page's player shows a preview ffmpeg makes into
`.shotline/cache/sequences/` whenever the sequence changes: the top enabled clip at each moment
with its LUT files and dissolves, the audio tracks mixed. Other effects, Lumetri's sliders and
built-in LUTs are left out. Above the tracks the page says when the editor hasn't rendered the
sequence since it changed, with an Update preview button: the editor plugin's `export` then
renders the saved file at half size into the same folder (`<key>-full.mp4`), through
`filmcraft-cli` so an open FilmCraft is left alone, and the player swaps to that, keeping its place.
It never renders by itself, since a render keeps the editor busy. The tracks pinch to zoom in and scroll
sideways, or along the bar under them, turning a page when the playhead plays past the edge.
The folder keeps the 16 newest quick previews and the 3 newest full ones. Render makes one into `renders/` to keep, through the open FilmCraft
when there is one, so what isn't saved yet is in it. The page
watches `shared/` and reads it again when the editor saves.

What's sent: "Open in FilmCraft" on the Cut page opens the film's project, or makes it when
there's none. shotline sends its cut (the selected takes back to back on V1, their sound on A1,
the music on A2 and up) and every finished take; the plugin writes the cut as OTIO, starts
FilmCraft with a control port and a new token, has it import the cut and save the project itself,
so the file is always one FilmCraft wrote. After that, each take that's done goes into a
`shotline/<shot>` bin named as the take. FilmCraft doesn't watch folders, so takes made while it's
closed, or showing another project, wait in `.shotline/cache/filmcraft-waiting.json` and go in the
next time it's opened from shotline. The port and token are kept in
`.shotline/cache/filmcraft-session.json` (mode 600), never in `shared/` or on a command line.

Both edit the same timeline. On the Cut page a sound clip can be picked to see what it is (a
take's own sound, a bridge's, or a sound laid over one) and open that take's Sounds window or its
shot. A clip of an editor's sequence can be picked to open
its shot, swap in another take (all of it from the same point in, with what follows moved
when it runs longer or shorter), match its colour to the clip before, give it a LUT, or run a video
tool on it (interpolation, upscaling): the part in use plus half a second either side is cut out,
the tool runs on it as a job with the usual quote, and the result is swapped in. A join between two
clips can get a dissolve or lose one, or a colour match: the editor's own (Apply Match), or
shotline's MKL match, the one its renders use, written as a 33-point `.cube` and set as the clip's
input LUT. The plugin's `edit` makes each change with
FilmCraft's commands: through the control port when shotline opened it, through `filmcraft-cli`
when it's closed (after copying the project into `.shotline/cache/filmcraft-backups/`, the last
20), and not at all while a FilmCraft shotline didn't start is running or the CLI's version isn't
the app's (on a Mac the Cut page's warning has a button that downloads the app's version of the
CLI from FilmCraft's release and puts it in place of the old one). Each change comes back with
steps that undo just it, kept in
`.shotline/cache/editor-undo.json`, since FilmCraft keeps no undo history in the file.

When the sequence uses a take that isn't the shot's selected one, the page says so and offers to
select it. A shot with a take but none in the sequence, like one added after the project was made,
can be put in with its sound: after the last clip of the nearest shot before it in the film, or
before the first of the nearest one after, moving what follows later. Its take (the selected one,
or else the newest) gets selected too. The other way round, "Use in the cut" on a shot's page
puts that take in the sequence as well: in place of the shot's clip (as Swap take does), or where
a missing shot would go. A shot cut from several clips is left for the Cut page. Render on the
sequence asks the editor to render it into `renders/`.

A take can have sounds over it, effects or a voice off: Sounds on the Cut page's clip tools, or
on the shot page for the take in the cut. A bridge's take can too, from its clip; those are listed
in the bridge's file, and its speech goes by the voice alone, with no shot's recipe to follow. A file is copied into a job of its own. Speech is one of
the cast with a voice saying what's typed, priced first and made like a line's recording (the same
voice and model), but not one of the shot's lines, so no take sends it. A sound effect is made from
a description by any installed `audio.sfx` model, as long as asked or as long as the model makes
it. Remake on a made one fills in its words (and for an effect its model and the length it came
out), prices it, and puts the new one in its place, in the editor too. Each is listed in the
shot's `[[sound]]` with where in the take it starts. With an editor project, it goes
in the sequence too, on the first audio track with room at that point (a new one if none has),
and Remove lifts it out of there as well. It stays where it was put: swapping the take later
leaves it in the editor, and on the old take in the shot file. Without an editor, the film's own
cut shows the sounds on a lane of their own and a render mixes them in.

Two clips that don't quite meet can be joined by a bridge: a short clip from a model that takes
a start and an end frame (`end_frame` in its model info; h3 on fal does, and
Venice's minimax-h3-max-image-to-video from 5 s), made from the frame shown
last before the join and the one shown first after it. A bridge isn't a shot. It belongs to the
join, so it follows the trims there, not the shots' takes:

```toml
# bridges/01K....toml
sequence = "fan"
start = "bridges/01K.../start-1762650000000.png"
end = "bridges/01K.../end-1762650000000.png"
prompt = "she turns to the window"
length = 2.0
plugin = "fal"
model = "minimax/h3-max/reference-to-video"
resolution = "768P"
used = "01K..."            # the take put in the cut last

[from]                     # inside the last frame shown before the join
file = "jobs/01K.../out.mp4"
at = 4.979

[to]                       # inside the first one after it
file = "jobs/01K.../out.mp4"
at = 0.021
```

Bridge… on a join opens its window: the two frames, the prompt (put into English like a shot's,
kept as `english`), the model, length and resolution, the price, Go, and the takes to play and
put in. A take is a job with `bridge` set instead of a shot, quoted first and in the ledger like
any other. The prompt goes with the note in `prompts/bridge.md` about keeping the motion going.
Use in the cut asks the editor to `insert` it before the clip after the join, so what follows
moves later; another take later goes in its place with `replace`. Bridges are silent: the clips'
own sound stays where it was. When a trim moves either clip's end, the window says so and offers
to take the frames again for the next takes. A join with a bridge not yet in is marked on the
timeline, a bridge in the sequence is named `bridge #n`, and picking either opens the window
again. One closed with no takes is thrown away. `shotline bridge ls` lists them and
`shotline bridge take <id>` makes another take.

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
Preferences has a Budget page for both. New films start with `per_take = 2` and `per_day = 10`.

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
shotline init <dir> [--no-git] [--aspect a] [--resolution r]
shotline format [--aspect a] [--resolution r] [--duration s] [--plugin p] [--model m]
shotline story | style [--set text]
shotline doctor
shotline plugin ls | doctor <name> | balance [name] | voices <name>
shotline plugin pin <name> [path] | unpin <name>
shotline plugin option <plugin> <model> <key> [value]   # film-wide; no value takes it out
shotline element add <kind> <name> [--handle h] [--desc ...]
shotline element anchor <handle> (--file img | --generate [--prompt ..] [--ref file|url].. [--no-anchors] [--aspect a] [--resolution r] [--translate] [--plugin p] [--model m] [--yes])
shotline element draft <handle> [--write] [--plugin p] [--model m] [--yes]
shotline element voice <handle> --plugin p [--model m] [--voice-id id] [--set k=v]
shotline element ls
shotline chapter add <title>
shotline chapter rename <chapter> <title>   # the folder keeps its name
shotline chapter ls
shotline shot add <chapter> [--after id] --plot ... [--prompt ...] [--duration n] [--transition cut|continue]
shotline shot add ... [--plugin p] [--model m] [--recipe r]
shotline shot set <shot> [--title] [--plot] [--prompt] [--frame-prompt] [--duration] [--transition] [--recipe]
                  [--model m [--plugin p]] [--resolution r]   "" goes back to the film's
                  [--leave-out anchor].. [--put-back anchor]..
shotline shot draft <shot> [--as video|image|translate] [--write] [--plugin p] [--model m] [--yes]
shotline shot line <shot> <who> <text> [--at s]
shotline shot say <shot> <line> [--yes | --ls | --use n | --fresh]   n from --ls: the speaker's
shotline shot image <shot> [--file img | --generate --prompt .. [--ref f|url].. [--aspect a]
                    [--resolution r] [--translate] | --remove n | --edit n] [--note ..]
shotline shot ls [chapter]
shotline shot show <id>
shotline prompt <shot>
shotline frame <shot> [--file img | --clear] [--plugin p] [--model m] [--yes]
               [--ref file|url].. [--no-refs] [--aspect a] [--resolution r] [--translate]
shotline quote <shot|chapter>
shotline take <shot> [--from-step n [--of take]] [--translate] [--yes]
shotline takes <shot>
shotline select <shot> <take>
shotline splice <shot> <[fade:|smooth:]take:from-to>... [--fade s] [--no-colormatch]
shotline import <shot> <file>
shotline run <op> --plugin p --model m [--prompt ..] [--duration s] [--input name=file]
             [--ref file] [--param k=v] [--yes]          one-off operation outside a shot
shotline timeline [render [-o file] [--no-colormatch] [--xfade s]]
shotline bridge ls | take <id> [--no-translate] [--yes]
shotline cost [--by chapter|shot|op|plugin|day] [--csv]
shotline cost verify | rates | reconcile [--plugin p]
shotline cost adjust <amount> <unit> --note ...
shotline cost refund <job> <amount> <unit> [--note ...]
shotline cost release [<job> [--note ...] [--sure]]       money held for jobs that never ended
shotline mcp [-C dir]
shotline ui [dir]
```

The MCP tools mirror these (`shot_add`, `take`, `timeline_render`, ...), over stdio with rmcp. Each
call runs the shotline binary itself as a child process in the project folder and returns what it
printed, so tools and commands can't drift apart and nothing reads the MCP stream from stdin. Lines
go out as progress notifications while a take runs. `cost adjust`, `cost refund` and
`cost release` aren't tools:
corrections to the ledger are for people.

`take` and `run` without `confirm: true` answer no at the prompt, so the agent gets the price and
balances and nothing is spent. Budgets are checked when money is reserved, before any job starts,
whether or not it was confirmed. The server starts in `-C dir` or the project around its working
directory; `init` and `open_project` switch to another. Relative paths are from the project folder.

`agent/CO_DIRECTOR.md` is sent as the server's instructions: the workflow (story, style, elements
and anchors, voices, shot breakdown, prompt check, quote, take, pick, render) and the rule that
agreeing to one take isn't agreeing to the next.

## UI

A native egui/eframe app, opened with `shotline ui [dir]`. No webview: it should start instantly
and stay light. It browses story, elements, shots and takes, compares takes side by side, plays
the timeline and shows costs. Plugin operations show up in it without plugins shipping any UI.

egui can't play video on its own. Instead of linking libav, the player runs ffmpeg as a process like
everything else: `ffmpeg -ss <t> -i take.mp4 -f rawvideo -pix_fmt rgba -s <preview size> -` feeds
frames into a texture, a second ffmpeg pipes PCM to `cpal`, and the audio clock drives the frames.
Seeking restarts ffmpeg at the new time. Takes are 5-15s at preview size, so this is cheap. When
the sound stops moving (a gap, or the audio is shorter than the picture) the wall clock takes over
after a quarter second. Two takes can play side by side, the second one silent. The cut plays the
selected takes back to back as they are; colour matching and music only happen in a render. In the
Cut tab "Open" still hands a take to mpv or ffplay to see it full size.

Anchors and storyboard frames are shrunk to 512px thumbnails when first shown: generated images
are often 2-4K, past the texture size some GPUs allow. egui ships no Thai font, so Noto Sans
Thai (`assets/fonts`, OFL) is built in; system ones looked different everywhere and Ayuthaya left
a gap after every leading vowel. Latin text is Inter and icons are the Phosphor font; icons are
drawn in their own font family, since Inter has glyphs of its own at many of Phosphor's code
points.

Colours mean one thing each: amber is in the cut and buttons that spend, paper white the main
button that doesn't spend, steel blue something running (and the playhead), green done, red
failed or removes. Pages are cards on the bay colour; popups size themselves from the window.
`cargo test -p shotline-ui --test ui -- --ignored shots pages` renders the main screens to
target/shots for a look after a visual change.

Shots are edited in place and saved once a field is left; every save is one undo step (Cmd+Z,
Cmd+Shift+Z, or the buttons in the bottom bar). Undo puts the shot file back only if nothing else
changed it since, so it never writes over the co-director's work. The bottom bar also commits the
film with the user's own git, with a message naming the shots and elements that changed; a commit
clears the undo steps, and anything older is git's job. Outside a film the app opens on a page to
make one (with `git init`, like `shotline init` unless `--no-git`) or pick a recent one. File and Edit
live in the system menu bar on macOS and Windows (muda) and in a menu bar inside the window on
Linux, where muda would need GTK; all three send the same commands.

The film's format is the shape, resolution, shot length and video model in shotline.toml. A new
film picks its shape (9:16 unless chosen); `shotline format` and the Format section at the top of
the Story tab change it later, as an undo step like any other edit. The resolutions on offer are
the model's own names for them. Takes already made keep the shape they were made at, so it says
how many shots have one. A shot can use another model or resolution (`shot set --model`, or the
Model and Resolution rows next to the shot), kept in its `[model]` table, which goes away once
both are back to the film's. Its shape can only be changed by hand there, and is warned about:
the render scales every clip to the first one's size, so a shot of another shape comes out
stretched.

The Cut tab lists the shots in play order with their takes, edits timeline.toml (crossfade,
colour matching, music tracks with start and volume; a track from outside the film is copied into
`music/`) and renders in the background with the same code as `shotline timeline render`, into
`renders/`. Past renders play in the app. With an editor plugin, the top of the Cut tab switches
to the editor's sequences and opens the film in the editor (see Editors).

The Cost tab charts spending per day as stacked bars, one colour per model (last 7 or 30 days,
or all of it), with a table of models under it (usage, jobs, money) and the spend per shot. Under
that, the Ledger row does what `cost verify`, `cost rates`, `cost reconcile`, `cost adjust`,
`cost refund` and `cost release` do; rates and reconcile ask the plugins on another thread.

A reservation stays open when the process running its job dies (the app was closed mid-take, say).
`cost release` closes it with nothing spent and marks the job cancelled. It refuses a job whose
job.toml still says pending or running unless `--sure`, since a job that really is running would
then find no reservation to charge against. When a film opens, the app shows a strip under the tabs for jobs
that look like that: still not sent after five minutes, not done after an hour, or done by their
job.toml yet never closed in the ledger. Review opens the release form on the first one.

The UI only goes through shotline-core, same as the CLI and MCP, so all three always agree on what's
in the project folder. Every paid action looks the same: "Make…" (or Shoot a take…, Record…)
asks the price for free, then "Make for $0.04" spends it and Cancel or Esc backs out. Nothing is
priced on its own, a price goes away when what it was for is edited, and a plugin missing a key
says so with a Set key… button instead. The CLI refuses the job if the price has changed since.

Images are made in a popup ("Make an image of @paka", "Make a frame for 01-010", "Remake picture
2 of 01-010"). Once made, the image shows there with Keep and Try again; nothing changes until
Keep, and Keep is one undo step; Discard drops it. Make starts a `shotline __run` process of its
own (its own process group, detached on Windows), so closing the popup or the whole app doesn't
stop it. Each run has a record in `.shotline/cache/running/<id>.json` with the popup's settings,
which the run fills in with the image or the error, and a `<id>.beat` file it touches every 5s; a
run with no beat for 30s is shown as stopped. While any are listed, an icon beside Reload turns
while something is being made and has the count on a badge; its list opens a run's popup again, as
it was. An image made with its popup closed (or the app) is put in place by itself, as one undo
step; the popup watching one shows it with Keep, Try again and Discard instead. A remade picture goes in
place of the file it was made again for, wherever it is by then (added as a new one if it's gone);
a frame made out of sight isn't put in if the shot's frame changed meanwhile. A failed one stays
on the list until its popup has said why.
A new anchor starts with no references, so a new look isn't pulled toward the old anchors; they
can be added from any element, a shot's storyboard frame or own pictures, a file, or a web
address (Add a reference…, downloaded into `.shotline/cache/refs`). Browse existing lists them under headers
like the recordings: the shot it's for first, then the elements, then images made before, then
other shots behind Show all. The prompt names them Image 1, Image 2… in that order. Size picks the
shape (and the resolution, for models that offer a choice) when it shouldn't be the film's: a
face square, a set wide. Every job.toml keeps the prompt exactly as the model got it, and for an
anchor what was typed into the box (`brief`). Details under a zoomed picture shows them, finding
the job by matching the picture to a job's output, so it works for anchors made from the CLI too.
The zoomed picture can be pinched in (up to 8x) and dragged or scrolled around; a double-click
shows it whole.
What's typed for an anchor can be put into English with the film's translating model first
(`element anchor --generate --translate`, or the tick box under the prompt in the app, shown only
when the text isn't English and that model's plugin and keys are there). It's a second small
job, priced and agreed together with the image; the English goes into the prompt and is kept as
`english` in the image's job.toml.

A shot's page lists what goes to the video model under References: the anchors of the elements
it names, then its own pictures (Add image… or Make image…, the same panel as for anchors), one a
row, each with a note of what it shows here; its tooltip shows the sentence the prompt gets. Browse
existing sends one anchor of any element (the element goes into the shot's `elements` with its
other anchors left out), including one left out before, or adds an image made before as one of the
shot's pictures. Leave out beside an element's anchor leaves it out of this shot only; those
are listed under Left out (n), each with Put back. The red ✕ is only for the shot's own pictures,
which it deletes.
The Start card, in the right column above References, holds Continue from previous shot (only
once the shot before has a selected take) or, left off, the storyboard frame. With no frame it
offers Browse existing, Add image… and Make frame…; with one, Replace… (a file or a new one),
Remake… and Remove. Make frame… starts from the frame prompt and what the video gets. What's
typed for a frame stays in the popup and becomes the shot's frame prompt on Keep.
Remake… shows only on pictures a job here made (found by matching the file to a job's output);
others say where they came from ("from a file", "last frame of 01-010#3"). It opens the popup
filled from that job: what was typed, the pictures sent along that are still there, the model
(or a note that it isn't installed and which one is used instead), and the translate tick. Kept,
the new picture takes the old one's place, note and all. Image buttons with no image model
installed are shown greyed out, saying so.

An element's page shows its anchors as cards of one size, the picture letterboxed on black with
a label beside it (up to 40 letters, kept in the element's `[labels]` by file name, e.g.
`"02.png" = "side view"`). The label follows the anchor's name wherever one is picked from a
list ("@paka 2  side view") and goes when the anchor is removed.

Every image a model makes stays in its job folder, so making a frame or an anchor again doesn't
lose the one before. Each job records what it was for (`shot`, `frame = true` for a storyboard
frame, or the element in `who`), and every Browse existing (frame, References, an element's
anchors, the image panel) lists them, the ones made for what's open first, newest first. The
frame's lists frames and the previous shot's last frames; References and anchors list pictures;
the rest wait behind Show all. Under Takes is the last frame of each take that has
been selected (taken once, when it's first selected, into its job folder as `last.png`, the file
a shot continuing from it sends). Picking one copies it in. Remove beside one throws it
out (its job is marked `discarded`, the file and the ledger line stay) and can be undone. Every box typed for a model (Prompt, Frame
prompt, Make image) offers "Translate to English first" when what's in it isn't English and the
film's translating model has its key; the price then includes the translation. Lines start with Add line; a line is its start (dragged like the
length, or left as "after" to follow the line before), who says it, its words, whether it's
recorded, Record… and ⋯, the same shape whatever state it's in. The words stay editable; once
they differ from what was recorded the line is marked out of date and still sends the old
recording until it's recorded again. Record again… keeps the old one among the speaker's
recordings. Another speaker drops the recording (undoable). A new line is priced from its words
and only added once it's paid for. ⋯ has Recordings by @x… (a popup of everything that speaker
has recorded: this shot's first, other shots behind Show all, each with play and delete, one
another line sends showing a way to that line's shot instead of being deleted), Recording from a
file…, None: record with each take, and Delete line. Voice-overs laid over a take aren't listed
there and can't be deleted while they're laid. A speaker with no voice, or a voice without a
model, says so under the line with a link to the Elements page; what went wrong recording shows
whole under the line until it's dismissed. The Lines panel shows even when the shot isn't
spoken, with Record… off. A voice in the Elements page can be heard before it's picked (▶, the
provider's sample, cached in `.shotline/cache/voices/`), and changing a voice says how many lines
were recorded with the old one, with links to them. Lines are dragged into order by their grip. Play the scene puts the picked recordings at
their start times. A Shoot section above Takes says what might go wrong, shows the prompt as the
model gets it, and has Shoot a take…, which asks the price first and spends only on a
second click, like `take`.
While it runs, Cancel asks the provider to stop it. fal stops a job only while it's queued (once
started, its cancel answers ALREADY_COMPLETED and the job runs on); Venice and WaveSpeed can't
cancel. When it won't stop, the row says so, and the job finishes and is charged as usual.
Each take shows a still from its video (kept in `.shotline/cache/posters/`); clicking it opens the
take in a popup, stopped on its first frame, with every step's model, cost and prompt, and a
Compare with dropdown that plays another take beside it in the popup. Each player also decodes a
small strip of its clip (8 frames a second) in the background, so the picture follows the slider
while it's dragged; the sharp frame is decoded when it's let go. Its video also sits under the shot's title, stopped on the first frame until it's played (Space
plays it when nothing's being typed), with its size, length, model and prompt beside it. A take's
video job keeps the shot's prompt as written (`brief`) and its English (`english`) when that's what
went; older takes show the prompt the model got. The take in the cut has a tungsten border; the
others have Use in the cut, and finished ones Remove (undoable). References sit in the right column
under the shot's model. A shot row's right-click menu inserts a shot after it, duplicates it
(pictures copied, no takes), moves it up, down or to another chapter (its takes, pictures and
place in the cut go with it) and deletes it (its takes are thrown out, kept for the ledger); all
undoable. A long
message in the bottom bar is cut short with … and has a copy button.

It reopens on the tab and shot it was left on (`.shotline/cache/ui-place`) and where the window
was. When working on the UI, keep it open with
`watchexec -r -w target/ui-restart --ignore-nothing -- target/debug/shotline ui <film>` and
`touch target/ui-restart` after a build to restart it on the new code.

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
  venice/  elevenlabs/  wavespeed/  local/   external plugins, built and installed separately
  filmcraft/           editor plugin: reads .fcproj, writes OTIO, drives FilmCraft's control port
```

shotline-core knows nothing about HTTP or ffmpeg. The CLI, MCP server and UI are thin layers on top
of it.

Release binaries for mac/linux/windows from CI (`.github/workflows/release.yml`): a `v*` tag that
matches the crate version builds all targets and makes a draft release; a manual run only builds.
ffmpeg has to be installed separately, `shotline doctor` checks for it.

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
7. mcp server and the co-director notes - done
8. anchor image generation, storyboard frames, upscale/interpolate - done
9. egui UI: browse the project, costs, open takes in mpv/ffplay - done
10. built-in player over ffmpeg pipes, side by side takes, timeline playback - done
11. editor plugins: FilmCraft's sequences on the Cut page, open the cut there, send takes - done

## Not sure yet

- Colour matching gets harder when the previous take came from a different model.
- Hooks (run something after every take) or are recipes enough?
- Should plugins be able to add their own CLI commands, or only operations?
