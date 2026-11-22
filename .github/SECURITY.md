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
  filesystem that holds no
  Unix modes kb runs anyway, and `kb doctor` says the folder isn't private.
  Whatever connects can drive the browser, including running JavaScript in
  pages you are logged in to in kb. Any program running as your user
  already has that much access to your account, but treat the socket
  accordingly.
- **The MCP server** (`kb mcp serve`) forwards an agent's tool calls to that
  socket. It offers no tools until you run `kb mcp on`. `kb mcp serve -i
  NAME` reaches only the kb with that name.
- **Uploads.** `kb ctl upload` (and the MCP tool) reads files from your
  computer and gives them to the page, which can send them anywhere. An
  agent that can drive kb can send any file you can read.
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
  sequences. The address and find fields and the inspect box drop them.
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
