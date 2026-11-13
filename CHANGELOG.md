# Changelog

Notable changes to kb. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[semantic versioning](https://semver.org).

## Unreleased

- A ✕ at the right of the bar quits, after asking "Quit KittyBrowser?"
  (Yes or Enter, No or Esc).
- Copy: Cmd+C or Ctrl+C copies the selection, or the address while editing
  it, to the system clipboard through the terminal; what pages copy
  themselves (copy buttons) goes there too.
- The bar follows the terminal's colours, and pages are told you prefer
  dark mode when the terminal is dark (`--dark`, `--light`, and
  `kb ctl scheme` override it).
- The mouse pointer changes shape over links, text and controls.
- The mouse's back and forward buttons go back and forward.
- Key bindings can be changed in `~/.kittybrowser/keys`; `kb keys` lists them.
- Inspect mode (Ctrl+Shift+C): outline elements under the mouse, click one
  for its selector, size and styles; the selector is copied.
- Chrome DevTools on F12, with `kb --devtools`.
- Function keys reach the page, and `kb ctl press F5` works.
- In a terminal without the kitty graphics protocol (Terminal, iTerm2,
  tmux), kb says so and exits, instead of drawing nothing.
- Much faster over ssh: when the terminal can't read kb's files, only what
  moved or changed is sent. A scroll step went from 1.1 MB to about 5 KB and
  155 to 109 ms, a keystroke from 1.1 MB to a few KB and 110 to 72 ms.
- Pages no longer come out cropped and stretched on a Mac whose display is
  asleep, where Chrome cut frames to its headless window.
- Frames are decoded faster (base64), which helps locally too.
- `kb bench` has a typing scene (`--scene type`), `--modes`, the bytes sent
  to the terminal, and `KB_BENCH_DUMP` and `KB_BENCH_HOLD` for looking at
  frames.
- `kb list`, `kb ctl` and the other subcommands exit quietly when their
  output is cut short (`kb list | head`), rather than panicking.

## 0.1.0

The first version.

- A browser in a terminal pane: headless Chrome drawn with the kitty graphics
  protocol, with the mouse and keyboard, an address bar with completion from
  history, back and forward, find, zoom, and a persistent profile.
- Several kbs at once, one per pane, listed by `kb list`.
- Fast drawing: frames handed to the terminal by file where it's accepted,
  only changed blocks redrawn, and frames paced when the page is idle;
  `kb bench` to measure it.
- `kb ctl`, to drive a running kb from another program: navigation, reading
  (`outline`, `text --section`, `find`, `links`, `tables`, `images`),
  actions (`click`, `type`, `select`, `press`, `wait`, ...), and debugging
  (`console`, `network`, `check`, `style --why`, `box`, `listeners`,
  `storage`, `throttle`, `viewport`, `screenshot`).
- The same commands as an MCP server, off until `kb mcp on`.
- An agent skill, installed with `kb skill install`.
