# Architecture

How each part works, what was measured, and the questions still open.

## Data flow

```
keys, mouse, replies           DevTools messages
terminal ──────────► app ◄────────────────── Chrome
   ▲                  │   navigate, resize, input
   │                  └────────────────────► Chrome
   └── top bar (text) + page (kitty graphics) ◄── app
```

Three threads. One reads the terminal and decodes it into events; one reads
messages from Chrome; the main thread owns all state, receives both on one
channel, and is the only one that writes to the terminal or to Chrome. It
draws only when something changed. There is no async runtime.

Each pass of the loop drains everything waiting on the channel. If several
frames arrived, only the newest is drawn, and a burst of mouse moves collapses
to the last one. Writing a frame to the terminal blocks until the terminal has
taken it, which is what keeps a slow terminal from falling behind.

## Engine (`src/engine/`)

`cdp.rs` is the transport: Chrome is started with `--remote-debugging-pipe`
and speaks NUL-terminated JSON on file descriptors 3 and 4. No port is opened
and no websocket library is needed.

`chrome.rs` starts the browser, attaches to its one page, and exposes what the
app needs: navigate, reload, back, forward, resize, mouse, keys, and a
`handle` function that digests each incoming message into a `PageEvent`
(new frame, URL changed, loading started or stopped).

Things it takes care of itself:

- **Frames.** `Page.startScreencast` pushes a PNG whenever the page changes,
  and each one is acknowledged so the next is sent.
- **Resolution.** Screencast frames come out at CSS-pixel size unless the
  scale factor is set at launch with `--force-device-scale-factor`; setting it
  only through `Emulation.setDeviceMetricsOverride` gives a blurry 1x image.
  So the scale is fixed per run. Resizing the viewport later needs no restart.
- **New windows.** There are no tabs. A page opened by a link or script is
  loaded in the one page instead and its own target closed.
- **User agent.** "HeadlessChrome" is replaced with "Chrome"; some sites
  refuse the former.
- **Exit.** `Browser.close` first, so cookies reach the profile, then a kill
  if Chrome hasn't gone within two seconds.

Back and forward are `history.back()` and `history.forward()` evaluated in the
page, which avoids tracking command replies after startup.

## Terminal (`src/term/`)

### Graphics (`graphics.rs`)

The frame's base64 PNG is passed through as Chrome sent it, never decoded,
cut into 4096-byte chunks (`ESC _G a=T,f=100,i=1,p=1,…;<data> ESC \`). Every
frame reuses image id 1, so it replaces the last, and the draw is wrapped in a
synchronized update. A frame whose size doesn't match the current layout (one
rendered before a resize, or for a new pane's first guess at its size) is
dropped rather than stretched to fit.

This is the slowest transmission and the main cost today: a photographic
page at 1008x1540 is about 3 MB of base64 per frame. Faster ones to try, in
order:

1. Shared memory (`t=s`): no copy through the pty. Local terminals only.
2. Temp file (`t=t`): the terminal reads the file itself. Local only.
3. Only the changed rectangle, or a scrolled copy of the previous frame.
4. JPEG frames from Chrome, decoded and sent as raw RGB with zlib (`o=z`).

### Size (`size.rs`)

`TIOCGWINSZ` gives columns, rows and, in Rex, pixel dimensions, so the cell
size is pixels divided by cells. When the pixel fields are zero the app falls
back to the terminal's reply to `CSI 16 t`, then `rex block call size`, then a
guess. The app polls the window size every 100 ms rather than handling
SIGWINCH; a new Rex pane reports 80x24 with no pixels for a moment before it
is laid out, and the poll picks up the real size.

The device pixel ratio is not reported by the terminal: `--scale`, default 2
on macOS.

### Input (`input.rs`)

Raw mode. A small incremental parser turns bytes into keys, mouse events,
pastes (bracketed paste) and replies to queries. Mouse reporting is SGR
(mode 1006) with any-motion tracking. Pixel coordinates (mode 1016) are
requested and then confirmed with a mode query; without confirmation, reports
are taken as cells and mapped to the cell's centre. Rex confirms pixel mode.

Keys use the legacy encoding, so some combinations are indistinguishable
(Ctrl+I is Tab, Ctrl+M is Enter) and Cmd never arrives. The kitty keyboard
protocol would fix the first.

## UI (`src/ui.rs`)

Row 1 is ordinary terminal text: three buttons and the address field. The
field is a line editor (cursor, word moves, select-all on focus, horizontal
scroll) with no terminal dependencies, so it is unit tested directly. Hit
testing maps a column to a button or a position in the field.
`normalize_url` decides whether typed text is a URL, a bare host, a local
file, or a search.

## Agent tools (`control.rs`, `commands.rs`, `devtools.rs`, `cli.rs`, `mcp.rs`)

```
kb ctl ──┐
         ├─ unix socket ─► control thread ─► app loop ─► Devtools ─► Chrome
kb mcp ──┘   one JSON line each way
```

`commands.rs` holds one table of commands and their parameters. The `kb ctl`
parser, its help, and the MCP tool list are all generated from it, so the two
front ends can't drift apart. `kb mcp serve` is a thin JSON-RPC wrapper that
forwards each tool call over the same socket; it lists no tools unless
`~/.kittybrowser/mcp-enabled` exists (`kb mcp on`).

`control.rs` listens on `~/.kittybrowser/kb.sock` (mode 0600). Each client
gets a thread that turns a request line into a `Request` carrying a reply
channel, hands it to the app loop, and writes back whatever arrives.

`devtools.rs` carries commands out on the app thread, which never blocks on
Chrome. A command that needs an answer parks its reply channel in `pending`
under the DevTools request id and finishes when that reply comes past. It
also watches every event to keep what the commands report on:

- **Console**: messages, uncaught exceptions, dialogs, with a marker where
  each page begins.
- **Network**: the current page's requests. A page begins when it commits
  (`Page.frameNavigated`), keeping the requests of its own loader.
- **Honest answers.** `open` and `reload` answer once the page has loaded and
  then been quiet for 400 ms, so errors from its first fetches are counted.
  `click`, `type`, `press` and `select` answer 100 ms after Chrome confirms
  the action, with any console errors and dialogs it caused; if it started a
  page load, they wait for that instead. `click`, `hover` and `type` hit-test
  first and refuse an element that is off screen or covered.
- **Dialogs.** `alert`, `confirm` and `prompt` are answered at once, since an
  open dialog freezes the page, and recorded in the console.

`viewport` is the one command the app handles itself: it fixes the layout
size, and the image is then placed in fewer cells, shrunk to fit if need be,
with mouse positions scaled to match.

All of this was shaped by three rounds of an agent building a page with only
`kb ctl --help` to go on. Still open from those: which CSS rule won
(`CSS.getMatchedStylesForNode`), event listeners on an element, frames,
clicking at an offset, JSON output, and network throttling.

## Open questions

- Which faster graphics transmissions does Rex accept? Only base64 PNG is
  confirmed.
- Does replacing the image by id flicker in any terminal?
- Is a persistent background Chrome acceptable, to hide its startup cost?
- Over SSH or a remote Rex host only the pty path works. Is it usable?
- What does "super fast" mean in numbers? Proposed: under 300 ms to first
  paint with a warm engine, under 30 ms from scroll key to new frame.
