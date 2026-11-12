# Roadmap

## Why

Terminals such as kitty, Ghostty, WezTerm and Rex can draw images in a pane.
That is enough to show a web page next to your editor or your agent without
leaving the terminal. The shell prototype in `prototype/webshot.sh` proved
the idea: it screenshots a page with headless Chrome at the pane's exact pixel
size and prints it, as a one-shot still image that takes seconds. kb makes
that live, interactive and fast, and gives agents a way to drive it.

## Goals

- **Real rendering.** Pixel output at the pane's native resolution, with
  complete web compatibility.
- **Fast.** First paint well under a second for ordinary pages; scrolling that
  feels immediate.
- **Interactive.** Scroll, click, type, find, go back and forward.
- **Fits the pane.** Lays the page out at the pane's true width and follows
  resizes.
- **Useful to agents.** Everything a person can do in kb, an agent can do
  through `kb ctl`, with answers written to be read by a model: what
  happened, what went wrong, and what to try next.

## Not goals, for now

- Tabs, bookmarks, extensions and downloads.
- Terminals without the kitty graphics protocol, and tmux or screen.
- Native Windows (WSL 2 is the route there).
- Being a replacement for a desktop browser.

## The page engine

kb drives headless Chrome over the DevTools protocol: complete web
compatibility for little code, at the cost of a second or so to start and a
Chrome dependency. A native Rust engine (Servo or Blitz) would start faster
and run in one process but render fewer sites correctly; it remains a later
experiment.

## Speed

`kb bench [URL]` scrolls a page in the pane and times each way of sending
frames. In Rex, on a 1372x1624 pixel pane, a scroll step takes about 84 ms
with PNG through the pty and 68 ms with pixels handed over by file, which kb
uses whenever the terminal accepts it. Most of what is left is Chrome itself.

Two things keep a page that moves by itself (a spinner, a pulsing dot) cheap:

- **Only what changed is redrawn.** The page is cut into blocks of 12 by 6
  cells. A frame that differs in a few of them sends just those; one that
  differs in more than a third sends the whole page.
- **Frames are paced.** While you scroll, click or type, or an agent sends
  commands, frames come as fast as Chrome makes them. Half a second after
  the last input they drop to ten a second, and to two a second when the
  pane doesn't have keyboard focus.

## Milestones

1. **Done: a working browser.** Live view, resize, scrolling, mouse and
   keyboard input, back and forward, an address bar with completion, find,
   zoom, several kbs at once.
2. **Done: agent tools.** `kb ctl`, an MCP server, and the agent skill,
   shaped by rounds of agents building pages, browsing real sites and
   running QA with them.
3. **Speed, in progress.** Done: a benchmark, frames handed to the terminal
   by file, only changed blocks redrawn, pacing. Next: Chrome's 50 ms per
   frame, and keeping Chrome warm between runs.
4. **Verified on more terminals and Linux.** kitty, Ghostty and WezTerm on
   macOS and Linux, in CI where possible.
5. **Hooks.** Reload on file change, and errors fed back to the agent after
   each edit.
6. **Keyboard link hints, and a one-shot mode** (`kb --print URL`) that
   draws one frame inline and exits.
7. **A native engine.** A second backend that doesn't need Chrome.
