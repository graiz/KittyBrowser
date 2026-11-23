# Security

## Reporting a problem

Please don't open a public issue for a security problem. Report it privately
through GitHub: on the repository's **Security** tab, choose **Report a
vulnerability**. Expect an answer within a week. Fixes go into the next
release, with credit if you'd like it.

## What kb trusts

- **The control socket.** A running kb listens on a Unix socket in
  `~/.kittybrowser` (`kb.sock`, `kb-2.sock`, ..., and `kb-NAME.sock` for a
  kb started with `--name`), created with mode 0600 so only your user can
  connect. `~/.kittybrowser` itself is kept at mode 0700; a link there is
  followed, and kb refuses the folder if it belongs to another user. On a
  filesystem that holds no Unix modes kb runs anyway, and `kb doctor` says
  the folder isn't private.
  Whatever connects can drive the browser, including running JavaScript in
  pages you are logged in to in kb. Any program running as your user
  already has that much access to your account, but treat the socket
  accordingly.
- **The MCP server** (`kb mcp serve`) forwards an agent's tool calls to that
  socket. It offers no tools until you run `kb mcp on`. `kb mcp serve -i
  NAME` reaches only the kb with that name. `--read-only` and `--tools`
  limit the MCP tools only; they are no sandbox for an agent that can also
  run `kb ctl`.
- **Uploads.** `kb ctl upload` (and the MCP tool) reads files from your
  computer and gives them to the page, which can send them anywhere. It
  refuses files that look like keys, passwords or sign-ins: those in
  `~/.ssh`, `~/.gnupg`, `~/.aws` and other credential folders, your
  keychains, browser profiles (kb's own included), `.env` files, `.netrc`
  and the like, and private keys by name, extension or contents. Any other
  file you can read, an agent that can drive kb can send.
- **The file chooser.** When a page asks for a file, kb asks you in the
  pane, and the page gets only the files you choose. Choosing one that
  looks secret, as above, asks you again before the page gets it.
- **Downloads.** A page can save files, as in Chrome, which puts them in
  your downloads folder and numbers a name that repeats rather than
  replacing a file. The bar says where each was saved, the command that
  caused it reports it, and `kb ctl console` records it. Quitting cancels
  downloads still running, and Chrome deletes the part saved.
- **Dialogs.** A page's alert, confirm, prompt or "leave site?" dialog is
  asked in the pane. One an agent's command sets off is answered at once,
  since an open dialog freezes the page, and an agent can choose with
  `kb ctl dialog accept` (or `dismiss`) how every dialog is answered,
  including one waiting for you. A headless kb, with no one to ask,
  answers them all. Each dialog is recorded in `kb ctl console`, with how
  it was answered and by whom.
- **Profiles.** Cookies and site data are kept in `~/.kittybrowser/profile`
  (`profile-N` for further kbs, `profiles/NAME` for named ones), separate
  from your everyday Chrome, and readable only by you (mode 0700). kb turns
  off Chrome's password manager and autofill in them.
- **Files kb writes.** Frames passed to the terminal by file go in a folder
  under the temporary directory with a name no one can guess, mode 0700,
  each file created new with mode 0600; the folder is removed as kb quits,
  or if kb was killed, by the next kb to start. `kb ctl screenshot` saves
  to `kb-screenshots-UID` in the temporary directory, mode 0700 (or, should
  another user have made that first, a new folder no one can guess); with
  `--out`, the file you name, written as `cp` would.
- **Page text in your terminal.** Titles, page text, console messages and
  errors printed by `kb ctl`, `kb list` and kb itself have their control
  characters shown as �, so a page can't send your terminal escape
  sequences. The address and find fields, the inspect box, and the dialogs
  and file chooser drawn in the pane drop them.
- **DevTools.** Started with `--devtools`, Chrome also listens on a port of
  127.0.0.1 so F12 can open DevTools. Any program on this machine, run by any
  user, can connect to that port and control the browser, so it is off
  unless asked for. Inspect mode (Ctrl+Shift+C) needs no port.
- **Web content** is handled by Chrome, with Chrome's sandbox. Without
  `--devtools`, kb doesn't open any network port.

Reports about any of these are in scope, as are ways for a web page to reach
the control socket or the terminal.

## Supported versions

Only the latest release gets fixes.
