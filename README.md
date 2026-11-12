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
| Ctrl+L | Edit the address (it starts selected, so typing replaces it) |
| Enter / Esc | Go to the address / cancel the edit |
| Right, End | Accept the suggested completion (Backspace rejects it) |
| Alt+Left, Alt+Right | Back, forward |
| Ctrl+R | Reload |
| Ctrl+Q | Quit |

Every other key goes to the page. While a page loads, the reload button turns
into a stop button.

As you type an address, the field completes it from pages you have visited,
most visited first, with the suggested part highlighted. Visits are kept in
`~/.kittybrowser/history`; nothing you type is sent to a suggestion service.

Cookies and logins are kept in `~/.kittybrowser/profile`, separate from your
everyday Chrome. Only one `kb` can use the profile at a time.

## For agents: `kb ctl` and MCP

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

`kb ctl --help` lists all the commands, and `kb ctl <command> --help`
describes one. They cover:

- **Navigating:** open, reload, back, forward, status, check, viewport.
- **Reading:** outline (title and headings), text (the main content, one
  element, or `--section HEADING`), find (where some words appear, with their
  context and section), links (numbered links and buttons), tables, images,
  html, screenshot.
- **Acting:** click, hover, type, select, press, scroll, wait, dialog.
- **Debugging:** console, network, eval, style, box.

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
- The control socket is `~/.kittybrowser/kb.sock`, readable only by you.
  Anything that can reach it can run JavaScript in your logged-in pages.

Options and environment:

- `--scale N`: device pixels per CSS pixel. Defaults to 2 on macOS and 1
  elsewhere; pass `--scale 1` on a non-retina display.
- `KB_CHROME`: path to the browser binary, if it isn't found automatically.
- `KB_LOG`: a file to append diagnostics to (detected sizes, frames, clicks).

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
2. **Speed.** Faster image transfer, redraw only changed regions, keep Chrome
   warm between runs. Add a benchmark so "fast" is a number.
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
