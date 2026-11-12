# KittyBrowser (`kb`)

A fast web browser that runs inside a terminal pane. Pages are rendered to
pixels and drawn with the kitty graphics protocol, so you get the real layout,
fonts, and images, not a text approximation. Written in Rust. The binary is `kb`.

```sh
kb https://example.com
```

**Status:** a working MVP. It opens pages, and you can scroll, click, type,
and go back and forward. It needs Chrome (or another Chromium-based browser)
installed, and has been run in Rex only.

## Use

```sh
kb                     # start with the address field focused
kb gregraiz.com        # open a page
kb rust traits         # anything that isn't an address is a web search
kb ./page.html         # a local file
```

The top row holds back, forward and reload buttons and the address field; the
page fills the rest of the pane. The mouse works on both: click the buttons,
click the address to edit it, and click, drag and scroll the page.

| Key | Does |
| --- | --- |
| Cmd+L or Ctrl+L | Edit the address (it starts selected, so typing replaces it) |
| Enter / Esc | Go to the address / cancel the edit |
| Right, End | Accept the suggested completion (Backspace rejects it) |
| Cmd+F or Ctrl+F | Find on the page. Enter and Shift+Enter (or Cmd+G, Shift+Cmd+G) go to the next and previous match; Esc closes |
| Ctrl+[ and Ctrl+], or Alt+Left and Alt+Right | Back, forward |
| Cmd+R or Ctrl+R | Reload |
| Ctrl+= and Ctrl+- | Zoom in and out; the zoom shows at the right of the bar |
| Ctrl+0 | Zoom to 100%, or out until a page too wide for the pane fits |
| Ctrl+/ | Help: the keys, and the page, zoom, viewport and pane sizes |
| Ctrl+Q | Quit |

The Command keys need a terminal with the kitty keyboard protocol, and the
terminal mustn't keep them for itself. Rex has the protocol, but its own
shortcuts take Cmd+F, Cmd+G and Cmd+R, so in Rex use Ctrl+F, Ctrl+G and
Ctrl+R; Cmd+L gets through. Ctrl+[ and Ctrl+] also need the protocol. Find doesn't yet say how many matches
there are, or that there are none.

Zoom works as in a desktop browser: the page reflows to the new width.
Zoomed in, text is a little soft, since Chrome still renders at 100% and the
image is enlarged to fill the pane.

Until the first page is ready, the logo turns in the middle of the pane;
started without an address, it stays as a home screen.

Every other key goes to the page. While a page loads, the reload button turns
into a stop button and a spinner turns at the right end of the bar.

As you type an address, the field completes it from pages you have visited,
most visited first, with the suggested part highlighted. Visits are kept in
`~/.kittybrowser/history`; nothing you type is sent to a suggestion service.

Cookies and logins are kept in `~/.kittybrowser/profile`, separate from your
everyday Chrome.

You can run several at once, one per pane. Chrome won't share a profile
between processes, so the second and later ones keep their own cookies and
logins (`profile-2`, `profile-3`, ...): a site you are signed in to in the
first is signed out in the second. History is shared. `kb list` numbers the
ones running.

## For agents: the skill, `kb ctl` and MCP

```sh
kb skill install    # puts it in ~/.claude/skills/kb; run again after upgrading kb
```

The skill teaches a coding agent to open kb in a pane beside its own (in
Rex, with `rex split`), find that kb's number, and test its work there with
`kb ctl` while you watch: reload, check for errors, screenshot, click
through, try a phone width. Its text is `skill/SKILL.md`; `kb skill show`
prints it.

A running kb can be driven from another terminal, which is how a coding agent
looks at and tests the page it is working on while you watch the same page.

```sh
kb ctl open http://localhost:3000    # waits for the load, reports errors
kb ctl screenshot --out page.png
kb ctl click '#save'                 # reports errors and dialogs it causes
kb ctl console --errors
kb ctl viewport 375x800              # test a narrow layout; reset to undo
kb ctl check                         # exit 1 if the page has errors
```

With several kbs running, commands go to the lowest-numbered one unless you
pick another: `kb ctl -i 2 status`, or `KB_INSTANCE=2` for the MCP server.

`kb ctl --help` lists all the commands, and `kb ctl <command> --help`
describes one. They cover:

- **Navigating:** open, reload, back, forward, status, check, viewport.
- **Reading:** outline (title and headings), text (the main content, one
  element, or `--section HEADING`), find (where some words appear, with their
  context and section), links (numbered links and buttons), tables, images,
  html, screenshot.
- **Acting:** click (`--double`, or `--at X,Y` for a point inside an
  element), hover, type, select, press, scroll, wait, dialog, throttle (a slow
  or offline network).
- **Debugging:** console, network, eval, style (`--why` for the rules that
  set a property and which one won), box, listeners (an element's
  event listeners), storage (localStorage, sessionStorage and cookies;
  `--clear` to test a first visit).

`screenshot` takes `--size small|normal|sharp|WIDTH`; the default is one
image pixel per CSS pixel. `kb ctl check --ignore HOST,HOST` leaves third-party noise, such as trackers
that fail to load, out of the verdict.

Anywhere a command takes a selector, it also takes `text=Some words` (the
element showing that text), `@N` (a number from `kb ctl links`) or `:focus`,
and selectors reach into shadow DOM.

```sh
kb ctl open en.wikipedia.org/wiki/Kitty_(terminal_emulator)
kb ctl outline
kb ctl text --section Features
kb ctl find "Goyal"
kb ctl links --match release
kb ctl click @12
```

The same commands are available as MCP tools. The MCP server is off until you
turn it on, and offers no tools while off:

```sh
kb mcp on        # also prints the line that registers it with Claude Code
kb mcp off
kb mcp status
```

Things to know:

- Commands act on the one page in the pane; there are no tabs.
- `reload` (and Ctrl+R) always bypass the cache, and pages on local addresses
  (files, localhost, private IPs, `.local`, `.test`, `.vibe`) are never
  cached, so an edit always shows.
- `alert`, `confirm` and `prompt` are answered at once (accepted by default;
  `kb ctl dialog dismiss` changes that) and recorded in the console.
- The control socket is `~/.kittybrowser/kb.sock` (`kb-2.sock` and so on for
  further instances), readable only by you.
  Anything that can reach it can run JavaScript in your logged-in pages.

Options and environment:

- `--scale N`: device pixels per CSS pixel. Defaults to 2 on macOS and 1
  elsewhere; pass `--scale 1` on a non-retina display.
- `KB_CHROME`: path to the browser binary, if it isn't found automatically.
- `KB_LOG`: a file to append diagnostics to (detected sizes, frames, clicks).
- `KB_TRANSFER=pty`: always send frames through the pty, never by file.
- `KB_CHROME_ARGS`: extra flags for Chrome, separated by spaces.

## Speed

`kb bench [URL]` scrolls a page in the pane and times each way of sending
frames. In Rex, on a 1372x1624 pixel pane, a scroll step takes about 84 ms
with PNG through the pty and 68 ms with pixels handed over by file, which kb
uses whenever the terminal accepts it. Most of what is left is Chrome
itself; see docs/ARCHITECTURE.md.

Two things keep a page that moves by itself (a spinner, a pulsing dot) from
costing much:

- **Only what changed is redrawn.** The page is cut into blocks of 12 by 6
  cells. A frame that differs in a few of them sends just those; one that
  differs in more than a third sends the whole page.
- **Frames are paced.** While you scroll, click or type, or an agent sends
  commands, frames come as fast as Chrome makes them. Half a second after
  the last input they drop to ten a second, and to two a second when the
  pane doesn't have keyboard focus.

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

## The page engine

The MVP drives headless Chrome over the DevTools protocol: complete web
compatibility for little code, at the cost of a second or so to start and a
Chrome dependency. A native Rust engine (Servo or Blitz) would start faster
and run in one process but render fewer sites correctly; it remains a later
experiment.

## Milestones

1. **Done: a working browser.** Live view, resize, scrolling, mouse and
   keyboard input, back and forward, an address bar.
2. **Speed, in progress.** Done: a benchmark (`kb bench`), frames handed to
   the terminal by file, only changed blocks redrawn, frames paced when the
   page isn't in use. Next: Chrome's 50 ms per frame, and keeping Chrome
   warm between runs.
3. **Done: agent tools.** `kb ctl` and an MCP server; see above.
4. **Hooks.** Reload on file change, and errors fed back to the agent after
   each edit.
5. **Keyboard link hints and a one-shot mode** (`kb --print URL`) that draws
   one frame inline and exits, for agents.
6. **Native engine.** A second backend that doesn't need Chrome.

## Layout

```
src/main.rs            argument parsing, starts the app
src/app.rs             event loop
src/engine/cdp.rs      DevTools protocol over Chrome's debugging pipe
src/engine/chrome.rs   starts Chrome; commands to the page, events from it
src/control.rs         the socket `kb ctl` and the MCP server talk to
src/bench.rs           `kb bench`
src/skill.rs           `kb skill`; the skill itself is skill/SKILL.md
src/splash.rs          the logo shown until the first page (assets/logo.svg; logo.png is rendered from it)
src/commands.rs        the agent commands: one table for CLI and MCP
src/devtools.rs        carries the commands out; console and network logs
src/cli.rs             `kb ctl`
src/mcp.rs             `kb mcp`
src/history.rs         visited pages, for address completion
src/term/graphics.rs   kitty graphics protocol
src/term/input.rs      decodes keys, mouse reports and pastes
src/term/size.rs       pane size in cells and pixels
src/ui.rs              top bar: buttons and the address field
docs/ARCHITECTURE.md   design notes and open questions
prototype/webshot.sh   the shell proof of concept
```

## Install

Needs Rust and Google Chrome (or another Chromium-based browser).

```sh
cargo install --path .     # puts kb in ~/.cargo/bin
```

## Build

```sh
cargo build --release
./target/release/kb https://example.com
cargo test
```

## License

MIT. See [LICENSE](LICENSE).
