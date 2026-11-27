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

The input thread polls the terminal and reads its descriptor directly,
with no buffer in between, so it can be paused: View source (in the ⋮
menu) hands the terminal to `$EDITOR` the way git does for a commit
message, and while the editor runs the input thread reads nothing, or it
would take keys typed to the editor. The main thread goes on meanwhile,
answering Chrome and agents' commands, with the terminal lent (`term::lend`):
what it would draw is dropped, and once the editor exits it takes the
terminal back and draws everything again. The source file stays until kb
quits, as an editor that opens a window of its own exits at once.

Each pass of the loop drains everything waiting on the channel. If several
frames arrived, only the newest is drawn, a burst of mouse moves collapses
to the last one, and a burst of wheel ticks to one scroll by all of them.
Writing a frame to the terminal blocks until the terminal has taken it, which
is what keeps a slow terminal from falling behind.

## Startup

Before Chrome starts, kb asks the terminal whether it draws kitty graphics
(`term::draws_graphics`): a query about a one-pixel image, then a request
for its attributes (DA1). Every terminal answers DA1, and answers in order,
so a DA1 reply with no graphics reply before it means no. kb then says
why (naming tmux, screen or Zellij when it is inside one) and asks whether
to draw pages in characters instead (see "Characters" below); Enter means
yes, and no exits as before. It waits 1.5 s at most (`kb doctor` waits 5). With
no answer by then, as over a slow link, kb goes ahead as if the answer were
yes. It asks last, just before entering raw mode, so replies that come
later are read and ignored by the input thread instead of being left for
the shell when a later step fails. `KB_FORCE=1` skips the question.

## Engine (`src/engine/`)

`cdp.rs` is the transport: Chrome is started with `--remote-debugging-pipe`
and speaks NUL-terminated JSON on file descriptors 3 and 4. No port is opened
and no websocket library is needed.

On Apple Silicon, an Intel build of kb runs under Rosetta, and a program it
starts would too: Chrome's JavaScript, compiled for Intel and then
translated, runs several times slower. So when kb is translated
(`sysctl.proc_translated`), it starts Chrome through `arch -arm64 -x86_64`,
and the client hints give the machine's architecture, not kb's.

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
- **New windows.** A window a page opens (a link to a new tab, a sign-in
  popup) is attached to and shown in place of the page that opened it, which
  waits, as a browser's tab would behind it; ‹ › in the bar switch between
  them. Loading it in the one page
  instead would break sign-in: the popup hands its result to the page that
  opened it, through `window.opener`, and then closes. Each window gets the
  same settings (the domains kb listens to, the user agent, the viewport,
  the colour scheme) and its own headless window, fitted like the first.
  Only the window shown sends events the app looks at. When it closes, by
  itself, with Ctrl+W, or with Back on its first page, the one before it is
  shown again.
- **Looking like a browser.** Sites such as Google answer what looks like a
  robot with captchas, and headless Chrome looks like one in four ways, each
  put right: `navigator.webdriver` is true (Chrome starts with
  `--disable-blink-features=AutomationControlled`); the user agent says
  "HeadlessChrome" (replaced with "Chrome"); replacing it empties the client
  hints, `Sec-CH-UA` and `navigator.userAgentData` (so kb gives them: the
  browser it runs, its version, the system's); and the screen is 400x300 and
  the window 500x256, smaller than the page (the screen is given as a
  laptop's, or the page's if larger, and the window is kept a browser's frame
  bigger than the page, below).
- **Exit.** `Browser.close` first, so cookies reach the profile, then a kill
  if Chrome hasn't gone within two seconds.

Back and forward are `history.back()` and `history.forward()` evaluated in the
page, which avoids tracking command replies after startup.

## Terminal (`src/term/`)

### Graphics (`graphics.rs`)

Each draw is wrapped in a synchronized update. A frame whose size doesn't
match the current layout (one rendered before a resize, or for a new pane's
first guess at its size) is dropped rather than stretched to fit, and a
frame identical to the one on screen is not drawn again: some pages repaint
without changing, 30 times a second.

There are two ways to send a frame:

- **By file.** The PNG is decoded here, the raw pixels are written to
  temporary files, and the terminal is given their names (`t=t,f=24`). The
  terminal reads each file and deletes it. The files go in a folder made once
  per run under the temporary directory (`tty-graphics-protocol-kb-PID-XXXXXX`,
  by `mkdtemp`), which only the user can open, so on a machine with a shared
  `/tmp` no one else can read a frame of a signed-in page or plant a file or
  link where kb will write one. kb removes it as it leaves the terminal; a
  kb that was killed can't, so before making its own, kb removes the user's
  folders whose process has gone and that have sat unchanged for ten
  minutes (a kb in a container sharing `/tmp` has process ids of its own).
  At startup kb asks whether the
  terminal will do this (`a=q` with a one-pixel file); a terminal on another
  machine can't see the file and says no.
- **Through the pty.** The data goes inside the escapes, in 4096-byte
  chunks. This works everywhere, over ssh too, and is what kb uses until the
  terminal has said yes to files.

Either way only what moved or changed is sent (see "Only what moved or
changed" below); the base64 PNG as Chrome sent it (`f=100`) is the fallback
when a frame can't be drawn that way.

What Rex accepts, found by querying it: raw pixels directly, raw pixels with
zlib, and raw pixels from a file in a temporary directory or a folder
under it. It refuses PNG
from a file, files outside temporary directories, and shared memory.

Measured with `kb bench` in Rex on a 1372x1624 pixel pane, scrolling a
Wikipedia article, median milliseconds per step:

| Frames sent as | Chrome | Decode and write | Terminal | Total |
| --- | --- | --- | --- | --- |
| PNG through the pty | 54 | 0 | 28 | 84 |
| PNG, pixels by file | 51 | 11 | 7 | 68 |
| JPEG 90, pixels by file | 46 | 10 | 7 | 64 |

So the file path is used, and JPEG isn't: it saves little and smears text.

**Only what moved or changed** (`term::layers`). Resending the whole frame
for each scroll step or keystroke costs the terminal a 2736x1666 image each
time (about 1.1 MB through the pty, 50 to 70 ms on a LAN; a 13 MB file
locally). So each frame is decoded and compared with the one on screen:

- **What moved.** The frame is cut into columns a cell wide, and rows that
  occur once in a column vote on how far it moved. Neighbouring columns that
  moved alike become one region, so a sidebar that stays put and an article
  that scrolls are each found. Images already in the terminal are placed
  again, shifted, with a source rectangle and a pixel offset inside the cell,
  and only the newly exposed rows are sent.
- **What changed.** Whatever still differs (a caret, a typed word, a sticky
  header, the rows a scroll uncovered) is sent as small rectangles of raw
  pixels, laid on top with a higher z: by file where the terminal reads
  files, otherwise through the pty compressed with zlib (`f=24,o=z`,
  level 3, on a few threads). Levels 1 and 6 measured worse: 22% more bytes,
  or twice the time for 0.2% fewer.
- **What came back.** Images that scroll off are kept for a while, two pages'
  worth, and kb keeps their rows' hashes; scrolling back places them again
  instead of sending them.
- **Files.** By file, the terminal deletes each file once it has read it.
  One it never read (an escape it refused, or one dropped while an editor
  had the pane) kb removes 64 draws later.
- **Crowding.** A page that scrolls a few rows at a time, as the glide
  scrolls it (see "Scrolling"), leaves a thin strip per step, each its own
  placement. Past 96 placements, the shortest band of the page that holds
  enough of them is sent again as one strip, which covers them (sent as
  it is: looked for in the held images, it would be found there, in the
  very strips it is to replace); before
  this, the whole frame went again, and over ssh each of those 1.3 MB
  frames held up the ones behind it (frames reached the far end in 102 to
  133 ms at the 90th percentile; after, 43 to 51).
- **Starting again.** When half the page or more changed (an animation,
  a video), or crowding can't be relieved, the whole frame is sent and the
  layers are dropped. Re-placements ask the terminal to report errors
  (`q=1`); if it says it no longer has an image, everything is drawn
  again.

This needs more of the protocol than a whole frame does: placements of
part of an image (`x,y,w,h`), offsets within a cell (`X,Y`), stacking (`z`)
and zlib-compressed pixels (`o=z`). For a terminal that has only the basics,
`KB_TRANSFER=png` sends every frame whole, as a PNG scaled to the cells
(`a=T,f=100,c,r`), as kb did before.

Which drew what is on screen is noted (`term::page::PageImage`): layers
know only their own images, so when a whole frame takes over, or layers
take over from one, all images are deleted first. A frame that doesn't fit
the cells (one rendered before a resize, or a fixed viewport scaled to fit)
goes whole, as image 1; left there, it would hide every frame the layers
draw after it.

Locally, in Rex, layers by file against the same layers through the pty,
replaying a trackpad scroll on a news site: the page followed the finger
72 to 76 ms behind against 80 to 92, and kb's own time per frame went from
10 ms at the 90th percentile (compressing strips) to under 1. `kb bench`
had found layers through the pty already faster there than the changed
tiles by file kb used before (14.5 ms a scroll step against 19.1, 18.6 a
typed key against 20.1), so tiles were removed.

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

**Headless window.** On a Mac, Chrome makes screencast frames the size of
its headless window less a margin for the browser's frame, whatever
viewport `Emulation.setDeviceMetricsOverride` sets: a 500x256 window gave
frames of 500x113 CSS pixels, 143 short. So kb sets the window to the
viewport plus that margin (`Browser.setWindowBounds`) on every resize, which
also gives pages a window as big as they are. A frame of another size that
can't be a late one (the size of an earlier viewport, or of an earlier
window less the margin) was made in the window as it is, so corrects the
margin. The size is absolute, so a late frame can't make it drift (`Window`
in `engine/chrome.rs`). Two things follow on a Mac. Frames stop after the
window changes until the screencast is started again, so kb restarts it
when Chrome answers each change. And macOS keeps windows at least 500
wide, so a narrower viewport comes in frames 500 wide, holding the page in
their top left; kb cuts those to the viewport (`Window::crop`).

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
2. Stop Chrome drawing at all when the pane is hidden.
3. Photographs: a 3 MB PNG takes 60 ms to decode, where JPEG takes 25.

### Characters (`ascii.rs`)

In a terminal that can't draw images, with `KB_ASCII` set, or after ASCII
mode is chosen in the ⋮ menu (after Select element; once on, the same item
offers Exit ASCII mode) or `about:ascii` is typed in the address field,
each frame is drawn in
characters instead. This is a branch in `App::draw` beside `PageImage`,
which it shares nothing with but the decoder.

Before the text is read, `terminal_type` sets the page's type to the
terminal's: a monospace face sized so that a letter is one cell wide and a
line one row high, wide letters (CJK) in an installed face scaled to two
cells, and icon fonts left in their own face. Laid out so, the page breaks
its lines where the terminal would and puts a letter in each cell. Blocks'
margins and padding, table rows and the space between them are rounded to
whole rows, and pictures and tables made whole rows by their bottom
margins, so that every line of text sits the same way in its row and
scrolling by whole rows keeps it on the page under it. A mouse over a
drawn letter is then over that letter in the page, so a drag selects what
is shown. `about:ascii-type` (or `KB_ASCII_TYPE=off`) keeps the page's own
type instead.

The page's text is printed as text. `READ_TEXT`, run in the page at most
every 100 ms while frames come, returns each line of each text node in view,
open shadow roots included, as the page wrapped it: its letters, its
colour, its box, in document coordinates (viewport ones in fixed boxes),
and which of its letters are selected, which are drawn the other way
round. A line's end is found by halving, as its letters share a top, so a
line costs a few boxes rather than one a letter. Collapsed white space
(a newline in the source) is a space. Text under something painted (a
background, a picture) is left out, but not text under a transparent
element: pages stretch links over whole cards to make them clickable. The
page's text under a floating box (a cookie banner, a sticky header) is
left out line by line, and letter by letter at the box's edge; the boxes
are found from the text in them. Text kept for screen readers (clipped to
a pixel) and icon fonts' letters are left out too. A page the script
can't read in 40 ms is drawn from its pixels alone, its text shown again,
until a try 5 seconds later.
Each frame carries the scroll it was drawn at (`Chrome::frame_scroll`), so
the lines stay on the page as it scrolls between readings. A line starts in
the cell where the page starts it, on the row through its middle, with one
letter per cell: a terminal's letters are wider or narrower than the
page's, but lines break where the page breaks them. A line that runs long
stops where the next text on its row starts, and one that follows another
straight on (a link in a sentence) goes on after it. Text scrolls by whole
rows, the scroll rounded to the nearest cell, so that lines at different
heights in their cells move together instead of one after another. Only
characters one or two cells wide are printed (no controls, emoji or text
written right to left), so page text can't move the cursor or drive the
terminal; a wide letter is sent once for its two cells. The script also hides the page's own text
(`-webkit-text-fill-color: transparent`, from a constructed sheet a
content security policy can't refuse) so frames show what is behind it;
`SHOW_TEXT` puts it back when the page goes back to pixels.

Everything else is drawn from the pixels. Each cell gets the character
and the two colours closest to the frame's pixels under it: a block covers
part of its cell (a half, a quarter, an eighth, or with sextants a sixth),
with the foreground colour on that part and the background on the rest.
The best colours for a shape are the averages of the pixels inside and
outside it, and the shape chosen is the one whose split takes the most off
the pixels' spread about their average. Running sums over the cell's
pixels give each part's total from four lookups, so trying every shape
costs about as much as reading the pixels once: about a millisecond for
1,800 cells. A shape that takes off less than a few steps of colour per
pixel leaves the cell a space, so noise doesn't flicker.

Only cells whose pixels changed are worked out again, and only cells that
look different are sent, with the cursor moved between runs and colours
set only when they change. Colours are truecolor when `COLORTERM` says so,
otherwise the nearest of the 256. Sextants (Unicode 13) are used where the
terminal draws images, as the terminals that do draw them themselves; a
terminal that can't may lack them in its font, so there they need
`KB_ASCII=sextants`.

The page is in the cells themselves, not an image under them, so a box or
menu that closes can't leave blanks for the page to show through:
`App::uncover` draws the page's cells there again from what was last sent.
Hovering a picture shows what the page says of it in a box beside the
mouse (its alt text, an SVG's title, a `title`, an icon button's label),
read by the same script that finds the pointer's shape.
The agent's pointer and the logo are images, and aren't drawn.

### Size (`size.rs`)

`TIOCGWINSZ` gives columns, rows and, in Rex, pixel dimensions, so the cell
size is pixels divided by cells. When the pixel fields are zero the app falls
back to the terminal's reply to `CSI 16 t`, then a guess. The app polls the
window size every 100 ms rather than handling SIGWINCH; a new Rex pane
reports 80x24 for a moment before it is laid out, and the poll picks up the
real size.

The device pixel ratio is not reported by the terminal: `--scale`, default 2
on macOS.

### Input (`input.rs`)

Raw mode. A small incremental parser turns bytes into keys, mouse events,
pastes (bracketed paste) and replies to queries. Mouse reporting is SGR
(mode 1006) with any-motion tracking. Pixel coordinates (mode 1016) are
requested and then confirmed with a mode query; without confirmation, reports
are taken as cells and mapped to the cell's centre. Rex confirms pixel mode.

A wheel report carries no distance, only a direction. Terminals turn a
trackpad's movement into one report per row the fingers move (per column
across), and a mouse wheel's notch into a few (three by default in Ghostty,
five in kitty). So each tick scrolls the page by one row of the pane,
in CSS pixels for the zoom and fit in effect: the page stays under the
fingers, and a notch moves it as far as the terminal's own scrollback would.
How those rows reach the page is in "Scrolling" below.

The kitty keyboard protocol's first level is switched on, so in terminals
that have it Esc, and keys held with Ctrl, Alt or Command, arrive as
`CSI code ; modifiers u` and can't be mistaken for anything else. That is
how Command+L and Command+F reach kb. Elsewhere keys come in the legacy
encoding, where some combinations are indistinguishable (Ctrl+I is Tab) and
Command never arrives.

### Scrolling (`src/scroll.rs`, `App::glide`)

A trackpad drawn slowly down gives a tick every 100 to 140 ms, and sent as
they came, each moved the page a whole row (14 CSS pixels) in the next
frame: the page jumped a row at a time, seven to ten times a second, while
the finger moved smoothly. It wasn't latency (Chrome showed each scroll
within 20 ms) but resolution. Three things make it follow the finger:

- **The glide** (`Glide`). Each tick is taken as a point the finger
  passed. The page goes to it, and on from it at the speed the ticks have
  been coming, for at most one more row: at a steady speed it reaches each
  row just as the next tick arrives, so it never jumps. The main loop wakes
  when the page is next owed half a pixel (`Glide::next_due`), but no
  sooner than 8 ms after the last step, and sends Chrome what the glide
  says it is owed. Waking every 8 ms instead found nothing to send on a
  slow drag, where half a pixel comes due only every 10 to 20 ms, and the
  loop spun until it did: about 40% of a core, against 5% now. Stepping at
  every wake (Chrome's
  answer to each step is one) sent three times as many scrolls for 4 to 8
  ms less lag, and every 16 ms added 4 to 8. When the finger stops, the page goes on to the end
  of that row, up to 14 pixels; holding it back half a row (to rest where
  the finger did, on average) doubled the lag. A scroll's first ticks have
  no speed yet: the page goes to those and no further, so a mouse wheel's
  notch, several ticks at once, scrolls as far as it always did (a wheel
  spun notch after notch may end a row further). Gliding only on single
  ticks, to keep spins exact too, cost 20 ms of lag: a fast trackpad's ticks
  also reach kb several at once. Turning back starts from where the page is, which may be past
  the last tick, so it doesn't leap back. With Ctrl held the wheel zooms
  on pages that zoom (maps, viewers), and some zoom a step for each
  event, however small; those ticks go as they came, not glided, which
  would have made six ticks 45 events.
- **Rails** (`Rails`). Fingers drawn down a trackpad drift sideways, and
  terminals report the drift as ticks left and right among the ticks down
  (about a third of them in a recorded scroll). Sent on, they shake
  anything that scrolls across. A scroll goes across only once it is
  mostly across.
- **Writing in the background** (`term::write_in_background`). A terminal
  that is slow to read (busy, or another client attached to it is behind)
  fills the pty, and a write then waits. On the app thread that held up
  scrolls on their way to Chrome by up to 760 ms at the 90th percentile.
  A thread of its own writes now; the app holds back a new frame until the
  last one has been written (`term::mark`, `term::reached`), and the writer
  wakes it (`Msg::Written`) to draw the latest, not each that came
  meanwhile. Leaving waits for the writer at most a second, so a terminal
  that has stopped reading can't keep kb from quitting.

Replaying the same recorded trackpad scroll on a news site, in Rex on the
same Mac, slow drags only:

| | Before | After |
| --- | --- | --- |
| How far behind the finger | 84 to 88 ms | 40 to 60 ms |
| Frames in which the page stood still while the finger moved | 86% | 39 to 49% |
| Each step | 14 px | about 3 px |

Over ssh (kb and Chrome on one Mac, the terminal on another on the same
LAN), 68 to 120 ms became 60 to 84, and 72 to 79% standing still became 54
to 58%. Tried and left out: `--disable-smooth-scrolling` (no difference),
the finger's speed over the last few ticks rather than two (more lag, and
no help with ticks that cross ssh two at once), stepping once per frame
Chrome sends rather than on a clock (more lag), and
`--disable-frame-rate-limit` (smoother, 45 frames a second, but kb and
Chrome at idle went from half a core to 1.8).

To measure: `KB_TRACE=FILE` writes a timeline of ticks, scrolls sent,
frames and their bytes, one line per event, and `tools/scroll/` has the
scripts that replay a recorded scroll into kb and score it.

## UI (`src/ui.rs`)

Row 1 is ordinary terminal text: three buttons, the address field, then
the window switcher (‹ 2/3 › ✕, only while a page has opened windows, so
✕ never quits kb), the badge (a note, the spinner, a
download's progress, the zoom) and ⋮ at the end, the field giving up room to the
rest down to a few columns. The field is a line editor (cursor, word
moves, select-all on focus, horizontal scroll) with no terminal
dependencies, so it is unit tested directly. Hit testing maps a column to a
button or a position in the field, from the same layout the bar is drawn
with. While an agent sends commands, the field is orange, until 10 s
after the last one: the user sees whenever an agent is reading or working
the page. With it, an orange pointer (`term/cursor.rs`) drifts to where
the agent clicks, hovers or types: an image kb places above the page and
below text, moved by placing it again with pixel offsets about 30 times a
second while it shows, never part of the page. ⋮ opens the menu (`menu.rs`), grouped as Chrome's is: the page (Find in page,
Close window, Zoom), tools for developers (Select element, View
source, Network performance, Developer tools), then Keyboard shortcuts and
Quit browser. Items that can't do anything at the moment it opens (closing
kb's only window, DevTools without `--devtools`) aren't shown, rather than
shown greyed. Zoom is a row of − and + around the percentage, as in
Chrome's: they zoom and leave the menu open, so it can be stepped, and the
menu draws the zoom as it is at each draw, so Ctrl+0's fit to a wide page,
which comes later, shows too. Network performance reads the page's
own Performance API (navigation and resource timings), not the DevTools
network events, which carry no timings in kb's record.
Zoom (Ctrl+= and Ctrl+-) divides the CSS viewport by the zoom, so Chrome
lays the page out narrower or wider, as a desktop browser does. Chrome's
screencast renders at the launch scale whatever scale is asked for (both a
different `deviceScaleFactor` and the override's `scale` were tried), so a
zoomed frame has fewer or more pixels than the pane and is stretched or
shrunk to fill it: zoomed in, the page is a little soft. Such frames are
drawn whole rather than as changed blocks. Ctrl+0 goes back to 100% and then asks the page
for `scrollWidth`; if that is wider than the viewport it zooms out to fit.

The page and what is laid over it are placed with a z-index below -1,073,741,824, which
the protocol defines as under text and under cells with a background colour
of their own. So the help box (Ctrl+/) is ordinary coloured text drawn over
the page, and blanking its cells shows the page again. The startup logo
(`splash.rs`) is a separate image above the page. While a page loads it is
animated by sending a new frame every 60 ms; otherwise it is drawn once, as
the plain logo, and costs nothing. The light's path is the ring's ellipse,
measured from the logo, dimmed where it passes behind the sphere. Drawing a
frame moves the cursor, and the bar isn't drawn with it, so each frame is
drawn between a save and a restore of the cursor.

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
`~/.kittybrowser/mcp-enabled` exists (`kb mcp on`). `kb mcp serve -i NAME`
sets `KB_INSTANCE`, so every call goes to that kb and none other. Each
command's `Effect` in the table says whether it only reads, changes
something that can be undone, or can destroy something (it sends the page
input, clears storage, or quits); it becomes the tool's `readOnlyHint` and
`destructiveHint`, since a client takes a tool with no annotations to be
destructive. `kb mcp serve --read-only` lists only the tools that read and
refuses calls to the rest, and `--tools` narrows the list to the names
given, failing at startup on one it doesn't know. Parameters
are typed in the table (text, number, or a list of words), which becomes the
tool schema; the control thread turns numbers back into the words `kb ctl`
would send, so commands read one form.

Answers carry whatever the page put in them, escape sequences included.
`kb ctl`, `kb list` and kb's errors print them through `term::printable`,
which shows every control character but newline and tab as �, so a page
can't retitle the caller's terminal or write its clipboard. MCP answers are
JSON and go out as they are. A screenshot is saved by `kb ctl` itself, not
the browser (`cli.rs`). With `--out`, it is written as `cp` would write
it: a file replaced, a link followed, a pipe or a device written into; the
path is printed only for a file other than `kb ctl`'s own output, so
`--out /dev/stdout > shot.png` holds just the image. Without it, it goes in `kb-screenshots-UID` in the temporary
directory, where the system clears old ones, or in a new folder no one can
guess if another user made that first. Over MCP it comes back as an image.

`headless.rs` runs the same Chrome, profile and `Devtools` without a
terminal, for `kb ctl` alone: no screencast, a desktop viewport, and a
small loop of its own rather than the app's, which is built around the
pane. `kb --headless` starts a copy of itself in a session of its own, so
it outlives the shell that started it, and prints its id once the copy
answers. Nobody sees a headless kb that is forgotten, so it quits after ten
minutes without a command (`kb list` asking its status doesn't count), unless
`KB_HEADLESS_IDLE=0`. If Chrome never starts to load the first page, it
stops and says why rather than leave every command to time out; and
`Chrome::launch` stops a Chrome that hasn't answered within 20 s of starting,
as one on macOS can when it waits for the keychain.

Numbers are reused as soon as a kb closes, so each run also gets an id, its
number and four hex digits (`3-a7f2`). `kb ctl -i ID` asks that socket's kb
for its id before sending the command, and fails if it differs: an agent
holding an old id is told its kb has gone rather than driving another.

`control.rs` gives each running kb a number, the lowest not in use, and a
socket to match: `~/.kittybrowser/kb.sock` for the first, `kb-2.sock` and so
on after it (mode 0600). `~/.kittybrowser` itself, and each profile in it,
is made 0700 (`private_dir` in `files.rs`), and an earlier kb's 0755 one is
tightened. A link in its place is followed, as ssh follows `~/.ssh`, but
a folder another user owns is refused rather than used (run as root with
HOME kept, the folders of HOME's owner are used). In the shared temporary
directory, where refusing links is the convention, `private_shared_dir`
doesn't follow them. On a filesystem that holds no Unix modes the chmods fail and kb
runs anyway, as Chrome does with its profile; `kb doctor` says so. The
number is taken before anything is written to
the terminal, under a file lock so two kbs starting together can't take the
same one. A kb started with `--name NAME` takes no number: its socket is
`kb-NAME.sock`, its profile `profiles/NAME` whatever else is running, and
its name is its id. Names start with a letter, so they can't be mistaken
for numbers or ids. As a kb quits it removes its socket at once, before
Chrome has closed, so the same name can start again straight away; and only
if the socket is still its own, by inode. Clients go to the kb that `kb ctl
-i N|ID|NAME` or `KB_INSTANCE` names; else, run in Rex, to the kb beside
them; else to the lowest-numbered. Each client gets a thread that turns a
request line into a `Request` carrying a reply channel, hands it to the app
loop, and writes back whatever arrives.

"Beside" means drawn in a pane with the caller's `REX_SESSION`, so an
agent reaches the kb it split, not the user's kb in another session. The
client asks each kb `where` (id, pane, session); the socket's thread
answers, not the app, so a page stuck in a script can't stall other kbs'
commands. Several kbs there is an error that lists them.

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
- **Stuck pages.** A script that never yields holds the page's main thread,
  and Chrome then answers nothing that needs the page: not a title, not
  even a navigation, since Chrome asks the page before it lets it go, and
  once it is going holds back what it is sent until the next page is
  there. So `status`, `open`, `reload`, `back` and `forward` first ask the
  page something it answers at once (`Runtime.evaluate` of `0`, the probe).
  `status` answers without the title, saying the page isn't answering, if
  the probe has gone 5 s unanswered. The others wait for the probe before
  they leave the page, and if it goes 5 s unanswered, stop the script
  (`Runtime.terminateExecution`, which V8 carries out off the main thread)
  and leave it, saying so. Only then: told to stop with no script running,
  V8 stops the next one instead. The probe is dropped when it is answered,
  when the stop is sent, and when the page gets a new document, whose
  answer to a question sent to the old one may never come; one left waiting
  would make a healthy page look stuck. On a free page the probe costs one
  round trip, about a millisecond, before every `open`, `reload`, `back` and
  `forward`, in a pane as well as headless.

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

**Uploads.** Chrome runs headless, so a file chooser has nowhere to show:
every window intercepts them (`Page.setInterceptFileChooserDialog`). `upload`
sets a file field's files with `DOM.setFileInputFiles`, over the pipe like
everything else, so no port is opened. Given anything other than a file
field, it first looks for the field the element belongs to (a label's
`control`, the one field inside it, or the one field in an ancestor no
further up than three levels, its form or the body) and sets that, since a
click on a wrong selector could do anything. Another kind of control (a
text field, a list, a button that submits its form, or a label for one)
fails instead: a file field near it is no more likely to be the one meant. Only when there is none does it
click through the same path as `click` and answer the
`Page.fileChooserOpened` that follows, by its `backendNodeId`; a chooser
that opens with no upload waiting is reported like a dialog. The answer
lists the files sent, as they are on disk, and then reads the field back
only to say whether the page has emptied or changed it: Chrome runs the
page's change handlers while setting the files, and some pages take the
files there and clear the field.

**Downloads.** At startup kb sends `Browser.setDownloadBehavior` with
`behavior: "default"` and `eventsEnabled: true`. "default" leaves the folder
and the naming to Chrome, so files land where the user would look (usually
`~/Downloads`) and a repeated name is numbered; "allow" with a folder would
overwrite a file of the same name. The events (`Browser.downloadWillBegin`,
`Browser.downloadProgress`, the last carrying `filePath`) come on the
browser's own session, so downloads from any window are seen.
`src/downloads.rs` keeps them; each download takes a place in the stream of
page messages when it begins, so the answer of the command that set it off,
and only that one, names the file and where it was saved, or how far it has
come if it is still running when the command answers. `status` lists those
running, the console keeps a line for each that finished (shown whatever
page has loaded since), and the bar shows the progress. `Page.navigate`
answers a download with `isDownload`, which `open` takes as a success.
Quitting cancels downloads still running, and Chrome deletes the part saved.

**Decisions on the remaining requests.** Iframes are entered only when a
selector asks, with `iframe >>> sel`, the syntax that already crosses shadow
roots; reading commands stop at the top document. Only frames from the same
site can be entered (their `contentDocument`); one from another site runs in
another process, and reaching it would mean attaching to its target. A
click's position adds the offsets of the frames the element is in, and the
check that a click landed listens in the element's frame, and in the frame
itself when the element is one. There is
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
