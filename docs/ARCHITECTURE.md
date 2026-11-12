# Architecture

Design notes for each part, and the questions still open. Nothing here is
built yet.

## Data flow

```
keys, mouse, resize          frames (RGBA)
terminal ──────────► app ◄────────────── engine
   ▲                  │   input, resize, navigate
   │                  └──────────────────► engine
   └── kitty graphics + text (ui) ◄── app
```

The app owns one event loop. It waits on three sources: terminal input, resize
signals, and new frames from the engine. It draws only when something changed.

## Engine (`src/engine/`)

The `Engine` trait is the seam: navigate, resize, input, and fetch the newest
frame. Backends:

- **chrome** (first): start headless Chrome, talk to it over the DevTools
  protocol on a pipe or websocket. `Page.startScreencast` pushes frames as the
  page changes; `Input.dispatchMouseEvent` and `Input.dispatchKeyEvent` send
  input back. Keep one Chrome process alive between runs to hide its startup
  cost.
- **native** (later): a Rust engine in-process. Candidates to evaluate: Servo
  (full engine, embeddable) and Blitz (HTML and CSS renderer built on Servo's
  style engine, no JavaScript engine of its own).

## Terminal

### Graphics (`src/term/graphics.rs`)

The prototype sends a PNG as base64 in 4096-byte chunks
(`ESC _G a=T,f=100,m=1;<data> ESC \`). That is the slowest path. Faster ones,
in the order to try them:

1. Shared memory (`t=s`): no copy through the pty. Local terminals only.
2. Temp file (`t=t`): the terminal reads the file itself. Local only.
3. Raw RGBA with zlib (`f=32,o=z`) over the pty: skips PNG encode and decode.
4. PNG as base64: the fallback that works everywhere, including over SSH.

Also planned:

- Query support at startup (`a=q`) and fall back cleanly.
- Give the page image a fixed id and re-transmit to that id, so a new frame
  replaces the old one without flicker.
- Send only the changed rectangle when the engine reports one.

### Size (`src/term/size.rs`)

Need columns, rows, and cell size in pixels. Sources: `TIOCGWINSZ` (pixel
fields are often zero), the `CSI 14 t` and `CSI 16 t` queries, and in Rex,
`rex block call size`. A new Rex pane reports 80x24 briefly before layout, so
wait for the first resize or re-read before the first paint. Device pixel
ratio is not reported by the terminal; assume 2 on macOS and allow an override.

### Input (`src/term/input.rs`)

Raw mode. Mouse through SGR reporting (mode 1006), with pixel coordinates
(mode 1016) where supported, otherwise cell coordinates mapped to the cell
centre. The kitty keyboard protocol for unambiguous key events.

## UI (`src/ui.rs`)

Ordinary terminal text on the rows the image doesn't use: an address bar on
top, a status line at the bottom. Link hints (letters over each link, typed to
follow it) need link positions from the engine.

## Modes

- **Interactive:** `kb URL`. Full screen in the pane until quit.
- **One-shot:** `kb --print URL` or an HTML file. Draw one frame inline and
  exit. This is the mode agents call to show something in a split.

## Candidate crates

Not chosen yet: `clap` (arguments), `crossterm` or `rustix` + hand-written
escape handling (terminal), `tokio` (async, if the Chrome backend wants it),
`chromiumoxide` or a small hand-written DevTools client, `flate2` (zlib),
`png`, `shared_memory` or `rustix` for shm.

## Open questions

- Which graphics transmissions does Rex accept? Only base64 PNG is confirmed.
- Does Rex report mouse events in pixels?
- How much latency does Chrome's screencast add per frame, and is it JPEG or
  PNG only?
- Is a persistent background Chrome acceptable, or should startup cost be paid
  each run?
- Over SSH or a remote Rex host, only the pty path works. Is that fast enough
  to be usable?
- What does "super fast" mean in numbers? Proposed: under 300 ms to first
  paint with a warm engine, under 30 ms from scroll key to new frame.
