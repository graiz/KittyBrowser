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

Every frame reuses image id 1, so it replaces the last, and the draw is
wrapped in a synchronized update. A frame whose size doesn't match the
current layout (one rendered before a resize, or for a new pane's first guess
at its size) is dropped rather than stretched to fit, and a frame identical
to the one on screen is not drawn again: some pages repaint without changing,
30 times a second.

There are two ways to send a frame:

- **By file.** The PNG is decoded here, the raw pixels are written to a
  temporary file, and the terminal is given its name (`t=t,f=24`). The
  terminal reads the file and deletes it. At startup kb asks whether the
  terminal will do this (`a=q` with a one-pixel file); a terminal on another
  machine can't see the file and says no.
- **Through the pty.** The data goes inside the escapes, in 4096-byte
  chunks. This works everywhere, over ssh too, and is what kb uses until the
  terminal has said yes to files. Only what moved or changed is sent (see
  "Over a network" below); the base64 PNG as Chrome sent it (`f=100`) is
  the fallback when a frame can't be drawn that way.

What Rex accepts, found by querying it: raw pixels directly, raw pixels with
zlib, and raw pixels from a file in a temporary directory. It refuses PNG
from a file, files outside temporary directories, and shared memory.

Measured with `kb bench` in Rex on a 1372x1624 pixel pane, scrolling a
Wikipedia article, median milliseconds per step:

| Frames sent as | Chrome | Decode and write | Terminal | Total |
| --- | --- | --- | --- | --- |
| PNG through the pty | 54 | 0 | 28 | 84 |
| PNG, pixels by file | 51 | 11 | 7 | 68 |
| JPEG 90, pixels by file | 46 | 10 | 7 | 64 |

So the file path is used, and JPEG isn't: it saves little and smears text.

**Only what changed** (`Tiles`). When frames go by file and line up with the
cells, the page is one image (id 1) and the screen is treated as a grid of
tiles, 12 cells by 6. Each new frame is compared with the one on screen,
tile by tile (under a millisecond). If a third of the tiles or fewer differ,
each is sent as a small image (ids from 10, `z=1`) laid over the page at its
cells. If more differ, the whole page is sent again and the tiles are
deleted, since ninety small files cost more than one large one: sending a
whole page as tiles took 89 ms a step against 72. A viewport is rounded up
to cover the cells, and the extra pixel is cropped, so tiles and page line
up exactly. A fixed viewport (`kb ctl viewport`) is scaled to fit and so is
always sent whole.

**Over a network** (`term::layers`). When the terminal can't read files,
which is always the case over ssh, every byte crosses the link: a whole
2736x1666 frame is about 1.1 MB, and resending it for each scroll step or
keystroke cost 50 to 70 ms on a LAN. So each frame is decoded and compared
with the one on screen:

- **What moved.** The frame is cut into columns a cell wide, and rows that
  occur once in a column vote on how far it moved. Neighbouring columns that
  moved alike become one region, so a sidebar that stays put and an article
  that scrolls are each found. Images already in the terminal are placed
  again, shifted, with a source rectangle and a pixel offset inside the cell,
  and only the newly exposed rows are sent.
- **What changed.** Whatever still differs (a caret, a typed word, a sticky
  header) is sent as small rectangles of raw pixels compressed with zlib
  (`f=24,o=z`, level 3, on a few threads), laid on top with a higher z.
- **What came back.** Images that scroll off are kept for a while, two pages'
  worth, and kb keeps their rows' hashes; scrolling back places them again
  instead of sending them.
- **Starting again.** When most of the page changed, or placements pile up,
  the whole frame is sent and the layers are dropped. Re-placements ask the
  terminal to report errors (`q=1`); if it says it no longer has an image,
  everything is drawn again.

This needs more of the protocol than a whole frame does: placements of
part of an image (`x,y,w,h`), offsets within a cell (`X,Y`), stacking (`z`)
and zlib-compressed pixels (`o=z`). For a terminal that has only the basics,
`KB_TRANSFER=png` sends every frame whole, as a PNG scaled to the cells
(`a=T,f=100,c,r`), as kb did before.

Which of these drew what is on screen is noted (`term::page::PageImage`):
each knows only its own images, so when another takes over, all images are
deleted first. A frame that doesn't fit the cells (one rendered before a
resize) goes whole, as image 1, above the layers; left there, it would hide
every frame they draw after it.

Measured with `kb bench` over ssh on a LAN (Ghostty, 171x50 cells, frames
2736x1666, Wikipedia), median milliseconds per step:

| Frames sent as | Chrome | kb | Terminal | Total | KB sent |
| --- | --- | --- | --- | --- | --- |
| Scroll, PNG through the pty | 78 | 0 | 77 | 155 | 1154 |
| Scroll, moved layers | 75 | 12 | 22 | 109 | 5 |
| Typing, PNG through the pty | 44 | 0 | 72 | 110 | 1169 |
| Typing, moved layers | 47 | 9 | 17 | 72 | 1 to 3 |

What's left is Chrome's capture, and ssh: OpenSSH 10 holds back small
packets on a 20 ms clock (`ObscureKeystrokeTiming`), which the terminal
column includes. Also tried and left out: lossy frames while scrolling
(Chrome sending a third of the size, 256 colours, sharpened when still: 76
ms to show, but soft text for a third of a second), JPEG from Chrome,
`captureScreenshot`, and `--disable-frame-rate-limit` (halves Chrome's time,
but animated pages then take three cores).

**Headless window.** On a Mac whose display is asleep, Chrome makes
screencast frames the size of its headless window less a margin, whatever
viewport `Emulation.setDeviceMetricsOverride` sets: a 500x256 window gave
frames of 500x113 CSS pixels, 143 short. kb compares each frame's size in
its metadata with the viewport. A frame of another size that can't be a
late one (the size of an earlier viewport, or of an earlier window less the
margin) was made in the window as it is, so gives the margin, and the
window is set to the viewport plus the margin (`Browser.setWindowBounds`).
Every resize sets it so again. The size is absolute, so a late frame
can't make it drift, and a wrong margin (Chrome's very first frame can be
off by a few pixels) is put right by the next frame (`Window` in
`engine/chrome.rs`). Where frames match, nothing is ever sent. macOS keeps
windows at least 500 wide, so there a narrower viewport comes in frames
500 wide.

**Pacing** (`App::frame_gap`). Chrome sends a frame, then waits for it to be
acknowledged; kb decides when. While the page is in use (a key, the mouse,
or a command in the last half second) every frame is acknowledged at once.
After that one is acknowledged every 100 ms, or every 500 ms when the
terminal reports the pane has lost focus (mode 1004). Chrome keeps count of
unacknowledged frames and stops for good if one is never acknowledged, so
they are queued and each is answered.

Measured on a Wikipedia article at rest, where a 36-pixel dot pulses for
ever in the toolbar: before, 49 frames a second, each redrawn whole, and
Chrome at a full core. After, 8 frames a second of 2 tiles each and Chrome
at about a third of a core; 2 frames a second and a quarter of a core when
the pane isn't focused. What is left is Chrome drawing the animation, which
it does whether or not anyone takes the frames.

Chrome's 50 ms is now most of a frame, and it is frame pacing, not encoding.
`--disable-frame-rate-limit` cuts it to about 20 ms (try it with
`KB_CHROME_ARGS`), but Chrome then draws as fast as it can: a page that
repaints continuously took over three cores at rest, so it is not on by
default. Things to try next:

1. Find a way to cut Chrome's latency without the unlimited frame rate.
2. A scrolled copy of the previous frame, so a scroll sends only the strip
   that came into view.
3. Stop Chrome drawing at all when the pane is hidden.
4. Photographs: a 3 MB PNG takes 60 ms to decode, where JPEG takes 25.

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

The kitty keyboard protocol's first level is switched on, so in terminals
that have it Esc, and keys held with Ctrl, Alt or Command, arrive as
`CSI code ; modifiers u` and can't be mistaken for anything else. That is
how Command+L and Command+F reach kb. Elsewhere keys come in the legacy
encoding, where some combinations are indistinguishable (Ctrl+I is Tab) and
Command never arrives.

## UI (`src/ui.rs`)

Row 1 is ordinary terminal text: three buttons and the address field. The
field is a line editor (cursor, word moves, select-all on focus, horizontal
scroll) with no terminal dependencies, so it is unit tested directly. Hit
testing maps a column to a button or a position in the field.
Zoom (Ctrl+= and Ctrl+-) divides the CSS viewport by the zoom, so Chrome
lays the page out narrower or wider, as a desktop browser does. Chrome's
screencast renders at the launch scale whatever scale is asked for (both a
different `deviceScaleFactor` and the override's `scale` were tried), so a
zoomed frame has fewer or more pixels than the pane and is stretched or
shrunk to fill it: zoomed in, the page is a little soft. Such frames are
drawn whole rather than as changed blocks. Ctrl+0 goes back to 100% and then asks the page
for `scrollWidth`; if that is wider than the viewport it zooms out to fit.

The page and its tiles are placed with a z-index below -1,073,741,824, which
the protocol defines as under text and under cells with a background colour
of their own. So the help box (Ctrl+/) is ordinary coloured text drawn over
the page, and blanking its cells shows the page again. The startup logo
(`splash.rs`) is a separate image above the page, animated by sending a new
frame every 60 ms for 12 seconds; the light's path is the ring's ellipse,
measured from the logo, dimmed where it passes behind the sphere.

Find (Command+F) puts a second such field in the address bar's place; each
change runs `window.find` in the page, which selects the match and scrolls
to it.
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

`control.rs` gives each running kb a number, the lowest not in use, and a
socket to match: `~/.kittybrowser/kb.sock` for the first, `kb-2.sock` and so
on after it (mode 0600). The number is taken before anything is written to
the terminal, under a file lock so two kbs starting together can't take the
same one. Clients go to the lowest-numbered kb unless `kb ctl -i N` or
`KB_INSTANCE` names another. Each client gets a thread that turns a request
line into a `Request` carrying a reply channel, hands it to the app loop, and
writes back whatever arrives.

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

Page scripts share a helper object, `__kb` (`LIB` in `devtools.rs`), defined
afresh with each script: it resolves selectors (CSS across shadow roots,
`text=`, `@N` from `links`, `:focus`), hit-tests through shadow roots, finds
the main content, and reads text including shadow content. `outline`,
`links`, `find`, `text --section`, `tables` and `images` are scripts built
on it.

**Clicks that don't arrive.** For about half an hour during testing, after a
successful login form submission on one site, Chrome stopped delivering
synthetic mouse events to that site's pages; keys and scripts still worked,
and a cross-site navigation (a new renderer process) cleared it. It
reproduced in a bare headless Chrome driven over DevTools, so it is
Chrome's, not kb's. What was ruled out: PaintHolding, BackForwardCache,
RenderDocument and site-isolation flags; the password manager and autofill
(prefs and flags); re-attaching the session; a held third-party script; a
Chrome update. A failed login (the same POST, back to a page with a password
form) and a login by `fetch` followed by a navigation did not trigger it.
Then it stopped reproducing entirely, in kb and in the bare script, with
nothing changed on our side, which points at something outside Chrome's
flags (a system prompt, network or the site). So every `click` listens for
its own mousedown, and if none arrived on a page that didn't change, it
clicks the element by script and says so. If it comes back, the bare
reproduction is the place to start: log in on the-internet.herokuapp.com by
clicking the submit button, then click Logout.

**Decisions on the remaining requests.** Iframes are not entered: selectors
and reading stop at the top document, and a selector that matches nothing
on a page with iframes says so (open the frame's address instead). There is
no JSON output: answers are written for agents to read, and scripts use exit
status, `check`, `wait` and exact `text`. `style --why` uses the CSS domain
(enabled on first use) to explain the cascade; `throttle` uses
`Network.emulateNetworkConditions`; `click --at` clicks a point inside an
element. Screenshots default to one image pixel per CSS pixel, a quarter of
a retina capture, with `--size` for smaller, sharper or a set width.

All of this was shaped by rounds of agents using kb with only `kb ctl --help`
and the skill to go on: three building pages, three browsing real sites, and
three judging the skill for development, research and QA. Still open: the
cause of the clicks that don't arrive.

## Open questions

- Which faster graphics transmissions do kitty, Ghostty and WezTerm accept?
  Only Rex has been measured.
- Should instances share one Chrome, so they share logins?
- Does replacing the image by id flicker in any terminal?
- Is a persistent background Chrome acceptable, to hide its startup cost?
- Over ssh, layers have been checked in Ghostty and Rex. Do kitty and
  WezTerm keep images after their last placement is deleted, as layers
  expect?
- What does "super fast" mean in numbers? Proposed: under 300 ms to first
  paint with a warm engine, under 30 ms from scroll key to new frame.
