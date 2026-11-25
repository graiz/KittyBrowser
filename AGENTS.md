# AGENTS.md

Instructions for coding agents (and people) working on this repository.
For what kb is and how to use it, read [README.md](README.md); for how it
works, [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Build and check

```sh
cargo build --release
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

All four must pass before a change is done; CI runs them on macOS and Linux.
To check the Linux build from a Mac: `rustup target add
x86_64-unknown-linux-gnu`, then `cargo clippy --target
x86_64-unknown-linux-gnu -- -D warnings`.

To try a change, install it and run it in a pane: `cargo install --path .`,
then `kb URL`. A kb that is already running keeps its old code; quit it
(Ctrl+Q) and start it again. Check that `command -v kb` is
`~/.cargo/bin/kb`: a released kb from Homebrew (`/opt/homebrew/bin/kb`) or
the install script (`~/.local/bin/kb`) can come first on the PATH, and
then `kb` runs that release instead of your change. Remove it while you work
on kb (`brew uninstall kittybrowser`, or `rm ~/.local/bin/kb`).

## Where things are

```
src/main.rs            arguments; dispatches kb, kb ctl, kb mcp, kb list, kb skill, kb doctor, kb bench
src/files.rs           private folders and files: the data directory, frames, screenshots
src/app.rs             the event loop: terminal input, Chrome's messages, drawing
src/ui.rs              the top bar: buttons, address field, completion, find
src/history.rs         visited pages, for address completion
src/menu.rs            the ⋮ menu: select element, view source, network performance
src/modal.rs           boxes drawn over the page: a grid of cells, frames, buttons
src/dialog.rs          a page's alert, confirm, prompt or leave-site dialog, asked in the pane
src/picker.rs          the file chooser a page opens, asked in the pane
src/secrets.rs         which files look like keys or passwords, kept from pages
src/splash.rs          the logo shown until the first page (assets/logo.svg)
src/engine/cdp.rs      DevTools protocol over Chrome's debugging pipe
src/engine/chrome.rs   starts Chrome; commands to the page, events from it
src/term/              raw mode, input decoding, pane size, kitty graphics
src/control.rs         the socket that kb ctl and the MCP server talk to; instances
src/commands.rs        the agent commands: one table drives kb ctl, its help and MCP
src/devtools.rs        carries the agent commands out, and the page-side scripts
src/downloads.rs       the files pages download, and how commands report them
src/cli.rs             kb ctl and kb list
src/doctor.rs          kb doctor, and what kb says in a terminal it can't draw in
src/mcp.rs             kb mcp
src/skill.rs           kb skill; the skill's text is skill/SKILL.md
src/bench.rs           kb bench
src/headless.rs        kb --headless: the browser without a pane, for kb ctl alone
```

## Conventions

- **Match the code around you.** Comments explain why, not what, in full
  sentences; names are plain words. Keep the comment density you find.
- **Few dependencies.** kb depends on `libc`, `serde_json`, `png` and
  `zune-jpeg` to decode frames, and `miniz_oxide` (already under `png`) to
  compress what it sends through the terminal. Adding a crate needs
  a reason the standard library can't meet, stated in the pull request.
- **No async runtime.** Threads and channels; the app thread never blocks on
  Chrome. See "Agent tools" in docs/ARCHITECTURE.md for how commands wait.
- **Unix only.** macOS and Linux; don't add code that breaks either.
- **Unit tests** go next to the code (`#[cfg(test)] mod tests`). Anything
  that parses or decides (input bytes, URLs, selectors, command arguments,
  sizes) should have them. Behaviour that needs Chrome is checked by hand in
  a running kb; say what you checked in the pull request.

## Changing an agent command

`kb ctl` and the MCP server are both generated from the table in
`src/commands.rs`. To add or change a command:

1. Add or edit its entry in `COMMANDS`. Its help text is what agents read,
   so say what it does, what it returns, and when it fails.
2. Carry it out in `Devtools::command` in `src/devtools.rs`. Answers are read
   by models: say what happened, name the element, and when something fails,
   say why and what to try (`see: kb ctl box`).
3. Prefer failing loudly to a false success: a click that didn't land, a
   hidden element, a page that didn't load.
4. Update `skill/SKILL.md` if agents should know about it, and the command
   list in README.md.
5. Try it in a running kb on a real page, and on the failure cases.

The skill is compiled into the binary; after changing `skill/SKILL.md`, run
`cargo install --path .` and `kb skill install` to use it.

## Releasing

kb is released three ways: archives on a GitHub release (which `install.sh`
downloads), the `kittybrowser` crate on crates.io, and the `kittybrowser`
formula in the Homebrew tap at
[graiz/homebrew-tap](https://github.com/graiz/homebrew-tap). To release:

1. On a branch, bump `version` in Cargo.toml and run `cargo build` so
   Cargo.lock follows. In CHANGELOG.md, rename `## Unreleased` to
   `## X.Y.Z — YYYY-MM-DD` and start an empty `## Unreleased` above it.
   Merge it to main once CI passes.
2. From main, check the crate: `cargo publish --dry-run`.
3. Tag main and push the tag: `git tag vX.Y.Z && git push origin vX.Y.Z`.
   `.github/workflows/release.yml` builds kb for macOS and Linux, on ARM and
   Intel, and attaches the archives and their checksums to a GitHub release.
4. Publish the crate: `cargo publish`. A published version can't be
   replaced, only yanked, so do this after the release builds succeed.
5. Update the tap: in a clone of graiz/homebrew-tap, run
   `./update.sh X.Y.Z`, which puts the new version and the release's
   checksums in `Formula/kittybrowser.rb`. Commit and push it, then check
   it installs: `brew update && brew upgrade kittybrowser` (or
   `brew install graiz/tap/kittybrowser`), then `kb --version`.

Versions follow semantic versioning: before 1.0, a release that changes how
a command or flag behaves bumps the minor version, and one with only fixes
bumps the patch.

## Safety

The control socket can run JavaScript in the user's logged-in pages. Keep it
mode 0600 in the user's data directory, keep the MCP server off by default,
and don't add ways to reach kb from the network.
