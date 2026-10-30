# shotline

A CLI for making AI films one shot at a time.

You write a story bible, define your cast/sets/props with anchor images and voices, break the
story into chapters and shots, then generate takes and pick the good ones. Shots marked as "continue" start
from the last frame of the previous take, so scenes flow instead of jumping. At the end it stitches
the selected takes into one video.

There's no built-in chat. If you want a co-director, add `shotline mcp` to any MCP client as a
stdio server, e.g. for Claude Code:

```
claude mcp add shotline -- shotline mcp -C ~/films/my-film
```

The tools are the CLI's commands. Taking a shot only shows the price until the agent calls again
with `confirm: true`, and budgets in `shotline.toml` are enforced either way. What the agent is told
about the workflow is in [agent/CO_DIRECTOR.md](agent/CO_DIRECTOR.md).

Very early. So far you can set up a project, add cast/sets/props with anchor images, break it
into chapters and shots, preview the exact prompt each shot would send, and install and check
plugins, quote and generate takes (fal, Venice, ElevenLabs voices, WaveSpeed lip sync) with
every cost on record, chain continue shots, and render the cut with colour matching and music. The plan is in [docs/DESIGN.md](docs/DESIGN.md).

```
cargo install --path crates/shotline-cli
shotline init my-film && cd my-film
shotline element add cast "Paka" --desc "team lead, 30s"
shotline element anchor paka --file paka.png
shotline chapter add "Into the forest"
shotline shot add 1 --prompt "@paka walks into the trees"
shotline prompt 01-010
```

Video models, TTS, lip sync, upscalers and anything else are plugins: small programs in any
language that speak a JSON protocol over stdio ([docs/PLUGINS.md](docs/PLUGINS.md)). fal is built in. Every paid step is quoted first
and recorded in an append-only ledger per film.

The plugins in `plugins/` install with `cargo install --path plugins/venice` (and so on) and are
found on PATH. `shotline plugin ls` shows what's there and `shotline plugin doctor <name>` checks
one without spending anything.

Needs ffmpeg. API keys go in your environment or `~/.config/shotline/.env`.

MIT licensed.
