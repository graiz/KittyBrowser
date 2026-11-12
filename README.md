# kittybrowser

A fast web browser that runs inside a terminal pane. Pages are rendered to
pixels and drawn with the kitty graphics protocol, so you get the real layout,
fonts, and images, not a text approximation. Written in Rust. The binary is `kb`.

```sh
kb https://example.com
```

**Status:** outline only. The code compiles and does nothing yet.

## Why

Terminals such as kitty, Ghostty, WezTerm, and Rex can draw images in a pane.
That is enough to show a web page next to your editor or your agent without
leaving the terminal. The shell prototype in `prototype/webshot.sh` proves the
idea: it screenshots a page with headless Chrome at the pane's exact pixel size
and prints it. It is a one-shot still image and takes seconds. This project
makes that live, interactive, and fast.

## Goals

- **Fast.** First paint well under a second for ordinary pages; scrolling that
  feels immediate.
- **Real rendering.** Pixel output at the pane's native resolution.
- **Interactive.** Scroll, click, type, follow links by keyboard, go back and
  forward.
- **Fits the pane.** Lays the page out at the pane's true width and follows
  resizes.
- **Useful to agents.** A non-interactive mode that draws a URL or an HTML file
  and exits, so an agent can show its work in a split.

## Not goals (for now)

- Tabs, bookmarks, extensions, downloads.
- Terminals without the kitty graphics protocol.
- Being a daily-driver replacement for a desktop browser.

## The main decision: the page engine

Everything else is small next to this. The options, to be settled by a spike:

| Engine | Speed | Web compatibility | Effort |
| --- | --- | --- | --- |
| Drive headless Chrome over the DevTools protocol | Slow start, fine after | Complete, with JavaScript | Low |
| Embed a Rust engine (Servo, or Blitz) | Fast start, one process | Partial; JavaScript varies | Medium to high |
| Write our own HTML and CSS subset | Fastest | Poor | Very high |

Proposed route: define an `Engine` trait first (see `src/engine/mod.rs`), ship
the Chrome backend to get a working browser, then add a native Rust backend and
measure. The claims in the table are expectations, not measurements.

## Milestones

1. **Still image.** `kb URL` draws one frame at the pane's size and exits.
   Matches the shell prototype, in Rust.
2. **Live view.** Stay open, redraw on resize, scroll with keys and the mouse
   wheel.
3. **Interaction.** Mouse clicks, text input, keyboard link hints, back and
   forward, an address bar.
4. **Speed.** Faster image transfer, redraw only changed regions, cache frames
   while scrolling. Add a benchmark so "fast" is a number.
5. **Native engine.** A second backend that doesn't need Chrome.

## Layout

```
src/main.rs            argument parsing, starts the app
src/app.rs             event loop
src/engine/            the Engine trait and its backends
src/term/graphics.rs   kitty graphics protocol
src/term/input.rs      keyboard and mouse
src/term/size.rs       pane size and resizes
src/ui.rs              address bar, status line, link hints
docs/ARCHITECTURE.md   design notes and open questions
prototype/webshot.sh   the shell proof of concept
```

## Build

```sh
cargo build --release
./target/release/kb https://example.com
```
