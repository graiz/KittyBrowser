#!/bin/sh
# Install kb: downloads the latest release for this machine.
#
#   curl -fsSL https://raw.githubusercontent.com/graiz/kittybrowser/main/install.sh | sh
#
# KB_INSTALL_DIR picks where kb goes (default ~/.local/bin). When there is no
# release for this machine, it builds from source with cargo instead.
#
# Everything is in main, called on the last line, so a download cut short
# runs nothing.
set -eu

say() { printf 'kb install: %s\n' "$*"; }
fail() {
  say "$*" >&2
  exit 1
}

# Build kb from source, saying why ($1).
build() {
  command -v cargo >/dev/null || fail "$1, and cargo isn't installed; get Rust from https://rustup.rs and run this again"
  say "$1; building from source with cargo, which takes a few minutes"
  cargo install --locked --force --git "https://github.com/$repo"
  dir="${CARGO_HOME:-$HOME/.cargo}/bin"
}

# Download the release for $target and install it in $dir. Only a missing
# release falls back to building; any other failure is shown as it is.
download() {
  command -v curl >/dev/null || fail "curl isn't installed; install it and run this again"
  url="https://github.com/$repo/releases/latest/download/kb-$target.tar.gz"
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  code="$(curl -sSL -w '%{http_code}' -o "$tmp/kb.tar.gz" "$url" 2>"$tmp/error")" ||
    fail "couldn't download $url: $(cat "$tmp/error")"
  case "$code" in
    200) ;;
    404)
      build "no release for $target yet"
      return
      ;;
    *) fail "couldn't download $url: HTTP $code" ;;
  esac
  # The sum comes from the same release, so this catches a damaged
  # download, not a tampered release.
  curl -fsSL "$url.sha256" -o "$tmp/kb.sha256" || fail "couldn't download the checksum; try again"
  want="$(cut -d ' ' -f 1 "$tmp/kb.sha256")"
  if command -v shasum >/dev/null; then
    got="$(shasum -a 256 "$tmp/kb.tar.gz" | cut -d ' ' -f 1)"
  elif command -v sha256sum >/dev/null; then
    got="$(sha256sum "$tmp/kb.tar.gz" | cut -d ' ' -f 1)"
  else
    fail "needs shasum or sha256sum to check the download"
  fi
  [ "$want" = "$got" ] || fail "the download's checksum doesn't match; try again"
  tar xzf "$tmp/kb.tar.gz" -C "$tmp"
  mkdir -p "$dir"
  install -m 755 "$tmp/kb-$target/kb" "$dir/kb"
  say "installed $dir/kb"
}

main() {
  repo="${KB_REPO:-graiz/kittybrowser}"
  dir="${KB_INSTALL_DIR:-$HOME/.local/bin}"

  case "$(uname -s)" in
    Darwin) os=apple-darwin ;;
    Linux) os=unknown-linux-gnu ;;
    *) fail "kb runs on macOS and Linux (on Windows, use WSL 2)" ;;
  esac
  case "$(uname -m)" in
    arm64 | aarch64) arch=aarch64 ;;
    x86_64 | amd64) arch=x86_64 ;;
    *) fail "no build for $(uname -m)" ;;
  esac
  target="$arch-$os"

  # The Linux releases need glibc; Alpine and others use musl instead.
  if [ "$os" = unknown-linux-gnu ] && ls /lib/ld-musl-* >/dev/null 2>&1; then
    build "the prebuilt kb needs glibc, and this Linux uses musl"
  else
    download
  fi

  case ":$PATH:" in
    *":$dir:"*)
      found="$(command -v kb || true)"
      if [ -n "$found" ] && [ "$found" != "$dir/kb" ]; then
        say "another kb at $found comes first on your PATH; remove it, or run $dir/kb"
      fi
      ;;
    *) say "$dir isn't on your PATH; add this line to ~/.zshrc or ~/.bashrc: export PATH=\"$dir:\$PATH\"" ;;
  esac
  say "now run: kb doctor"
}

main "$@"
