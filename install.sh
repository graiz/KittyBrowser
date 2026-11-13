#!/bin/sh
# Install kb without Rust: downloads the latest release for this machine.
#
#   curl -fsSL https://raw.githubusercontent.com/graiz/kittybrowser/main/install.sh | sh
#
# KB_INSTALL_DIR picks where kb goes (default ~/.local/bin). When there is no
# release for this machine yet, it builds from source with cargo instead.
set -eu

repo="${KB_REPO:-graiz/kittybrowser}"
dir="${KB_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf 'kb install: %s\n' "$*"; }
fail() { say "$*" >&2; exit 1; }

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
url="https://github.com/$repo/releases/latest/download/kb-$target.tar.gz"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if curl -fsSL "$url" -o "$tmp/kb.tar.gz" 2>/dev/null; then
  if curl -fsSL "$url.sha256" -o "$tmp/kb.sha256" 2>/dev/null; then
    want="$(cut -d ' ' -f 1 "$tmp/kb.sha256")"
    if command -v shasum >/dev/null; then got="$(shasum -a 256 "$tmp/kb.tar.gz" | cut -d ' ' -f 1)"
    else got="$(sha256sum "$tmp/kb.tar.gz" | cut -d ' ' -f 1)"; fi
    [ "$want" = "$got" ] || fail "the download's checksum doesn't match; try again"
  fi
  tar xzf "$tmp/kb.tar.gz" -C "$tmp"
  mkdir -p "$dir"
  install -m 755 "$tmp/kb-$target/kb" "$dir/kb"
  say "installed $dir/kb"
elif command -v cargo >/dev/null; then
  say "no release for $target yet; building from source with cargo"
  cargo install --locked --force --git "https://github.com/$repo"
  dir="$HOME/.cargo/bin"
else
  fail "no release for $target yet, and cargo isn't installed; get Rust from https://rustup.rs and run this again"
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
