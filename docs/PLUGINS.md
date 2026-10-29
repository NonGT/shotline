# Writing a plugin

Protocol version 1.

A plugin is a program that shotline starts and talks to over stdin/stdout. It can be written in any
language. shotline handles everything around it: building prompts, quoting and asking before
spending, budgets, the ledger, where files go. The plugin only turns requests into API calls (or
local work) and reports back.

## Installing

Either put an executable called `shotline-plugin-<name>` on PATH, or make a folder
`~/.config/shotline/plugins/<name>/` (or `$SHOTLINE_HOME/plugins/<name>/`) with a
`shotline-plugin.toml`:

```toml
name = "venice"
exec = "venice.py"          # relative to this folder, or absolute
args = []                   # optional
protocol = 1
```

A folder wins over PATH when both have the same name. `shotline plugin ls` shows what was found and
`shotline plugin doctor <name>` checks it.

## Wire format

JSON-RPC 2.0, one JSON object per line, UTF-8. shotline writes requests to the plugin's stdin and
reads responses from its stdout, one request at a time. Anything the plugin wants to log goes to
stderr; shotline keeps the last lines and shows them if the plugin crashes.

```
→ {"jsonrpc":"2.0","id":1,"method":"describe","params":{}}
← {"jsonrpc":"2.0","id":1,"result":{"name":"venice","version":"0.1.0","protocol":1,...}}
```

The plugin should exit when stdin closes. shotline may start it once for a whole session or once
per call, so don't rely on either.

Timeouts on the shotline side: 5 minutes for `run`, 10 for `fetch`, 1 for everything else. A plugin
that doesn't answer in time is killed and restarted, so long work belongs behind `run` + `poll`.

## Environment

The plugin gets a clean environment: PATH, HOME, USERPROFILE, SYSTEMROOT, TEMP, TMP, TMPDIR, LANG
and LC_ALL if set, plus the variables it lists in `describe.env`. Values come from shotline's own
environment or from `~/.config/shotline/.env`. Nothing else leaks through, so ask for every key you
need.

`describe` is called with no keys at all and must work without them.

## Methods

### describe

No params. Returns:

```json
{
  "name": "venice",
  "version": "0.1.0",
  "protocol": 1,
  "env": ["VENICE_API_KEY"],
  "models": [
    {
      "id": "minimax-h3-max-reference-to-video",
      "op": "video.generate",
      "ref_image": "<Picture {n}>",
      "max_images": 30,
      "min_duration": 5,
      "max_duration": 15,
      "whole_seconds": true,
      "resolutions": ["480P", "768P", "1080P"],
      "aspects": ["16:9", "9:16"],
      "start_frame": true,
      "start_frame_with_refs": false,
      "audio_input": true
    }
  ]
}
```

Model fields other than `id` and `op` are optional; leave out what doesn't apply.

- `ref_image`: how a reference image is named in the prompt, `{n}` counting from 1. Without it
  shotline assumes the model takes no reference images.
- `max_images`, `max_audio`: reference limits.
- `min_duration`, `max_duration` in seconds; `whole_seconds` if fractions aren't allowed.
- `resolutions`, `aspects`: allowed values, as the API spells them.
- `start_frame`, `start_frame_with_refs`, `audio_input`: what the model accepts.
- `notes`: name of a prompt template (`prompts/models/<notes>.md`) with per-model tweaks.
- `params_schema`: JSON Schema for anything the model takes in `params`.

Operations:

```
image.generate     prompt (+ references) -> image
image.edit         inputs.image + prompt -> image
video.generate     prompt + references / start_frame / end_frame / audio refs -> video
video.lipsync      inputs.video + inputs.audio -> video
video.upscale      inputs.video -> video
video.interpolate  inputs.video -> video
audio.tts          prompt (the text) + voice -> audio
audio.music        prompt -> audio
audio.sfx          prompt -> audio
audio.transcribe   inputs.audio -> data (text with timings)
x.<name>           anything else
```

### quote

Params: a request (below). Returns the price:

```json
{"amount": 0.40, "unit": "USD", "units": 8, "unit_name": "s"}
```

`unit` is a currency code or the provider's own credit name (`venice-credit`); shotline converts
with the rates the user sets. `units`/`unit_name` say what was counted and are optional.

quote must never cost anything. `shotline plugin doctor` calls it. If the API has no price
endpoint, work it out locally.

### run

Params: a request plus `output_dir`, an absolute folder the outputs go into. Returns
`{"job": "<your id>"}`. Start the work and return quickly.

### poll

Params: `{"job": "<id>"}`. Returns:

```json
{"state": "running", "progress": 0.4}
{"state": "done", "cost": {"amount": 0.40, "unit": "USD"}}
{"state": "failed", "error": {"kind": "content_policy", "message": "..."}, "cost": {"amount": 0.40, "unit": "USD"}}
```

`state` is one of `queued`, `running`, `done`, `failed`, `cancelled`. Include `cost` when you know
what was actually charged, including for failed jobs that still cost money. Without it shotline
books the quoted amount.

### fetch

Params: `{"job": "<id>", "output_dir": "<folder>"}`. Write the results into `output_dir` and return:

```json
{"outputs": [{"file": "out.mp4", "kind": "video"}], "data": null}
```

`file` is a name inside `output_dir`. `kind` is `image`, `audio`, `video` or `data`. `data` is any
JSON the job produced, e.g. a transcript. Only write inside `output_dir`.

### cancel

Params: `{"job": "<id>"}`. Returns `{}`.

### rates (optional)

What one of your credit units costs:

```json
{"rates": [{"unit": "venice-credit", "amount": 0.004, "currency": "USD"}]}
```

User settings override this.

### usage (optional)

Params: `{"since": "2026-10-01T00:00:00Z"}`. What the provider actually charged, for
`shotline cost reconcile`:

```json
{"charges": [{"job": "abc", "time": "2026-10-30T10:12:03Z", "amount": 0.40, "unit": "USD"}]}
```

Leave out optional methods; answer with error code -32601 (method not found).

## Requests

```json
{
  "op": "video.generate",
  "model": "minimax-h3-max-reference-to-video",
  "prompt": "<Picture 1> walks into the trees ...",
  "duration": 8,
  "resolution": "768P",
  "aspect": "9:16",
  "seed": 42,
  "references": [{"kind": "image", "path": "/abs/elements/cast/paka/01.png"},
                 {"kind": "audio", "path": "/abs/jobs/01JB.../line.wav"}],
  "start_frame": "/abs/jobs/01JA.../last.png",
  "end_frame": null,
  "inputs": {"video": "/abs/...", "audio": "/abs/..."},
  "voice": {"voice_id": "..."},
  "params": {"prompt_expansion_mode": "disabled"}
}
```

Only `op` and `model` are always there. Paths are absolute and local; uploading them, or turning
them into data URLs, is the plugin's job. References are in the order the prompt numbers them.
`params` comes straight from the user's shot or command line; validate it.

## Errors

Plugin errors use JSON-RPC error code `1` with the details in `data`:

```json
{"jsonrpc":"2.0","id":3,"error":{"code":1,"message":"too many images",
  "data":{"kind":"validation","message":"too many images","field":"references[9]"}}}
```

`kind` is one of `validation`, `content_policy`, `auth`, `quota`, `not_found`, `unsupported`,
`other`. `field` points into the request (`prompt`, `model`, `references[2]`, `inputs.audio`,
`params.seed`) so shotline can tell the user which shot, element or file is the problem. Map the
provider's own errors onto this; for example fal's
`{"detail":[{"loc":["body","reference_image_urls",2],...}]}` becomes `references[2]`.

An unknown model is `validation` with field `model`. An unknown job id is `not_found`.

## Checking a plugin

`shotline plugin doctor <name>` checks `describe`, the environment, `quote` for every model and the
error shapes. It never calls `run`.

Rust plugins can use the `shotline-plugin` crate: implement the `Plugin` trait, call
`shotline_plugin::serve(plugin)` from `main`, and run `shotline_plugin::conformance::check` with
`run_jobs: true` in tests against a mocked API.

## A minimal plugin in Python

```python
#!/usr/bin/env python3
import json, os, sys

MODELS = [{"id": "echo/tts", "op": "audio.tts"}]
jobs = {}

def handle(method, p):
    if method == "describe":
        return {"name": "echo", "version": "0.1.0", "protocol": 1, "env": [], "models": MODELS}
    if method == "quote":
        if p["model"] != "echo/tts":
            raise PluginError("validation", "no such model", "model")
        return {"amount": 0, "unit": "USD"}
    if method == "run":
        jobs[str(len(jobs) + 1)] = p
        return {"job": str(len(jobs))}
    if method == "poll":
        if p["job"] not in jobs:
            raise PluginError("not_found", "no such job")
        return {"state": "done", "cost": {"amount": 0, "unit": "USD"}}
    if method == "fetch":
        os.makedirs(p["output_dir"], exist_ok=True)
        with open(f'{p["output_dir"]}/out.txt', "w") as f:
            f.write(jobs[p["job"]].get("prompt", ""))
        return {"outputs": [{"file": "out.txt", "kind": "data"}]}
    if method == "cancel":
        return {}
    raise NotImplementedError

class PluginError(Exception):
    def __init__(self, kind, message, field=None):
        self.data = {"kind": kind, "message": message, "field": field}

for line in sys.stdin:
    req = json.loads(line)
    resp = {"jsonrpc": "2.0", "id": req["id"]}
    try:
        resp["result"] = handle(req["method"], req.get("params") or {})
    except PluginError as e:
        resp["error"] = {"code": 1, "message": e.data["message"], "data": e.data}
    except NotImplementedError:
        resp["error"] = {"code": -32601, "message": "method not found"}
    print(json.dumps(resp), flush=True)
```
