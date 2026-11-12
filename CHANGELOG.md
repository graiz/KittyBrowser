# Changelog

Notable changes to kb. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[semantic versioning](https://semver.org).

## Unreleased

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
