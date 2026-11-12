# Security

## Reporting a problem

Please don't open a public issue for a security problem. Report it privately
through GitHub: on the repository's **Security** tab, choose **Report a
vulnerability**. Expect an answer within a week. Fixes go into the next
release, with credit if you'd like it.

## What kb trusts

- **The control socket.** A running kb listens on a Unix socket in
  `~/.kittybrowser` (`kb.sock`, `kb-2.sock`, ...), created with mode 0600 so
  only your user can connect. Whatever connects can drive the browser,
  including running JavaScript in pages you are logged in to in kb. Any
  program running as your user already has that much access to your
  account, but treat the socket accordingly.
- **The MCP server** (`kb mcp serve`) forwards an agent's tool calls to that
  socket. It offers no tools until you run `kb mcp on`.
- **Profiles.** Cookies and site data are kept in `~/.kittybrowser/profile`
  (and `profile-N` for further kbs), separate from your everyday Chrome. kb
  turns off Chrome's password manager and autofill in them.
- **Web content** is handled by Chrome, with Chrome's sandbox. kb doesn't
  open any network port.

Reports about any of these are in scope, as are ways for a web page to reach
the control socket or the terminal.

## Supported versions

Only the latest release gets fixes.
