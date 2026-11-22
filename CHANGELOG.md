# Changelog

Notable changes to kb. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[semantic versioning](https://semver.org).

## Unreleased

- A page's `alert`, `confirm`, `prompt` and "Leave site?" dialogs are asked
  in the pane, as Chrome asks them, when the user set them off: a window
  over the page with the page's host, its message, and Cancel and OK. Enter
  accepts, Esc cancels. Dialogs an agent's command sets off are still
  answered at once; while one waits for the user, commands that need its
  page fail and say so, and `status` and `window` name it. `kb ctl dialog ask` goes back to asking after
  `dialog accept` or `dismiss`.
- A file field the user clicks opens a file chooser in the pane: places on
  the left, the folder's files as `ls -la` lists them, and a File name field
  that follows the selection, takes a typed path, and completes with Tab.
  Space or Ctrl+click marks several files. It names the computer the files
  come from, which over ssh isn't the user's own. Agents can't see or drive
  it.
- Files that look like keys, passwords or sign-ins are refused to
  `kb ctl upload`, and the file chooser asks the user a second time before
  sending one.

## 0.2.0 — 2026-11-21

- The first release with prebuilt kbs for macOS and Linux, on ARM and
  Intel: `brew install graiz/tap/kittybrowser`, the install script, or
  `cargo install kittybrowser` from crates.io. Only `cargo install` needs
  Rust.
- `kb --name NAME` starts a kb that answers to `kb ctl -i NAME` and keeps
  its cookies in a profile of its own (`~/.kittybrowser/profiles/NAME`),
  whatever other kbs are running, so a site signed in to there stays signed
  in after a restart. A second kb with the same name is refused. `kb list`
  shows named kbs. `KB_SOCKET`, which shared the first kb's profile, is gone.
- `kb mcp serve -i NAME` drives that kb only, and its instructions say so.
  MCP number parameters (`timeout`, `limit`, `count`, `number`) are typed as
  numbers and honoured; before, `{"timeout": 2}` was silently read as 10.
- `kb ctl upload SELECTOR FILE...` chooses files for a file field, or clicks
  a button and answers the file chooser it opens, and reports the files as
  the page sees them. A click that opens a chooser says so.
- `kb ctl window` lists the windows a page opened, and `window N` shows one
  without closing the others.
- Selectors reach into an iframe from the same site with `iframe >>> sel`.
  A click that lands inside a frame no longer counts as missed, which made
  kb click the frame a second time by script.
- `KB_HEADLESS_IDLE=0` keeps a headless kb running until told to quit.
- When Chrome doesn't answer within 20 s of starting, or a headless kb's
  first page never starts to load, kb stops and says why (on macOS, it can
  be waiting for the keychain) rather than leave every command to time out
  after 70 s. A
  command Chrome never answers fails after 30 s, saying so.
- A socket path too long for Unix (a long `HOME`) is refused with a clear
  message. kb's profiles are readable only by the user (0700). A quitting kb
  frees its number or name at once, and a kb that can't read the terminal
  size no longer leaves its socket behind.

- An Intel build of kb on Apple Silicon starts Chrome natively. It used to
  start it under Rosetta as well, where JavaScript ran about six times
  slower, and a page that works hard in script could hold the page's thread
  until kb's commands timed out. The client hints now give the machine's
  architecture rather than kb's build's, and `kb doctor` notes an Intel
  build.
- Scrolling follows the trackpad: each wheel tick the terminal sends moves
  the page one row of the pane, rather than 60 CSS pixels, which made a
  trackpad several times too fast. A mouse wheel moves as many rows a notch
  as the terminal's own scrollback, and a burst of ticks reaches the page
  as one scroll.
- In Rex, `kb ctl` and `kb mcp serve` without `-i` go to the kb in the
  caller's Rex session, not the lowest-numbered, which was often the user's
  own elsewhere. Several there is an error that lists them.
- `kb --headless URL` runs kb in the background without a pane, driven by
  `kb ctl` alone, with the same profile and commands. It prints its id,
  `kb ctl -i ID quit` stops it (and quits any kb), and it stops by itself
  after ten minutes without a command (`KB_HEADLESS_IDLE`). The skill has
  agents finish this way when the user closes their pane to get it out of
  the way. It answers once the first page has loaded, as `kb ctl open`
  does, or after 15 seconds if the page is slow.
- `kb ctl console --all` heads only the pages that logged something.
- `kb ctl viewport WIDTH` lays the page out at that width and as tall as
  fills the pane, rather than shrinking it into the top of the pane.
- Each kb has an id as well as a number (`3-a7f2`), shown by `kb list` and
  `status` and printed by `kb --headless`. `kb ctl -i ID` reaches that kb
  only: once it closes, its number passes to the next kb to start, but its
  id stops working, so an agent can't drive someone else's kb by mistake.
- `kb ctl` shows a command's help with `--help` anywhere; `back` on a
  popup answers once it has closed; `text --section` finds a heading by its
  words, whatever the punctuation.
- `kb ctl wait` refuses an empty `--text`, which passed at once, and
  `--visible` waits for the element to take up room; `status` says how many
  windows are open.
- Sign-in popups work ("Continue with Google" and the like): a window a
  page opens is shown over it, and when it closes the page is shown again,
  signed in. Before, kb loaded it in place of the page, which then never
  heard back. With more than one window open, the bar shows "‹ 2/3 ›" to
  switch between them, as do Ctrl+Tab and Ctrl+PageDown (Ctrl+Shift+Tab,
  Ctrl+PageUp back); ✕, Ctrl+W, or Back on a window's first page, closes
  the one shown.
- The logo no longer covers the help on a blank page, and the help's keys
  line up however long they are.
- Fewer captchas: navigator.webdriver is false, the client hints
  (Sec-CH-UA) name the browser and system, and pages see a real screen
  and a window bigger than themselves.
- A ✕ at the right of the bar quits, after asking "Quit KittyBrowser?"
  (Yes or Enter, No or Esc).
- `kb doctor` checks the terminal (asking it directly whether it draws kitty
  graphics), tmux, screen and Zellij, Chrome, the data folder, the agent
  skill and the MCP server, and says what to fix.
- kb asks the terminal when it starts, and in one without the kitty
  graphics protocol (Terminal, iTerm2, tmux, screen, Zellij), or with no
  Chrome, says why and where kb runs instead of showing a blank pane
  (`KB_FORCE=1` skips the terminal's question).
- Prebuilt kb for macOS and Linux (Intel and ARM) on each release, and
  `install.sh`, so installing doesn't need Rust once a release is out.
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
