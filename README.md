# shotline

A CLI for making AI films one shot at a time.

You write a story bible, define your cast/sets/props with anchor images, break the story into
chapters and shots, then generate takes and pick the good ones. Shots marked as "continue" start
from the last frame of the previous take, so scenes flow instead of jumping. At the end it stitches
the selected takes into one video.

There's no built-in chat. If you want a co-director, run `shotline mcp` and let any MCP client
drive it. It always asks before spending credits.

Very early: only the design exists so far, see [docs/DESIGN.md](docs/DESIGN.md).

Needs ffmpeg. API keys for fal and/or Venice go in your environment.

MIT licensed.
