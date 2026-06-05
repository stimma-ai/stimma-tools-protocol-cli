#!/bin/sh
# stp installer for macOS / Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/stimma-ai/stimma-tools-protocol-cli/main/install.sh | sh
#
# Fetches a prebuilt binary for your platform from GitHub Releases (no Rust required). While
# the repo is private it uses the GitHub CLI (`gh`) automatically. Flags / env:
#   --from-source        build with cargo instead of downloading (needs Rust)
#   STP_VERSION=v0.1.0   install a specific tag (default: latest)
#   STP_INSTALL_DIR=...  install location (default: ~/.local/bin)
set -eu

REPO="stimma-ai/stimma-tools-protocol-cli"
INSTALL_DIR="${STP_INSTALL_DIR:-$HOME/.local/bin}"
FROM_SOURCE=0
[ "${1:-}" = "--from-source" ] && FROM_SOURCE=1

say() { printf '%s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

detect_target() {
  os=$(uname -s); arch=$(uname -m)
  case "$os" in
    Darwin) os_t=apple-darwin ;;
    Linux)  os_t=unknown-linux-gnu ;;
    *) die "unsupported OS: $os (try --from-source)" ;;
  esac
  case "$arch" in
    x86_64|amd64) arch_t=x86_64 ;;
    arm64|aarch64) arch_t=aarch64 ;;
    *) die "unsupported architecture: $arch (try --from-source)" ;;
  esac
  printf '%s-%s' "$arch_t" "$os_t"
}

build_from_source() {
  have cargo || die "cargo not found - install Rust from https://rustup.rs, or omit --from-source"
  say "building from source with cargo..."
  if [ -f "Cargo.toml" ] && grep -q 'stimma-tools-protocol-cli' Cargo.toml 2>/dev/null; then
    cargo build --release --locked -p stp
    src="target/release/stp"
  else
    tmp=$(mktemp -d)
    cargo install --git "https://github.com/$REPO" --root "$tmp" stp
    src="$tmp/bin/stp"
  fi
  install_binary "$src"
}

# Resolve latest tag (gh first for private repos, then public API).
latest_version() {
  if [ -n "${STP_VERSION:-}" ]; then printf '%s' "$STP_VERSION"; return; fi
  if have gh; then
    gh release view --repo "$REPO" --json tagName -q .tagName 2>/dev/null && return
  fi
  if have curl; then
    curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null \
      | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1
  fi
}

download() { # <tag> <asset> <out>
  tag=$1; asset=$2; out=$3
  if have gh; then
    gh release download "$tag" --repo "$REPO" -p "$asset" -O "$out" --clobber 2>/dev/null && return 0
  fi
  url="https://github.com/$REPO/releases/download/$tag/$asset"
  if have curl; then curl -fSL "$url" -o "$out" && return 0; fi
  if have wget; then wget -qO "$out" "$url" && return 0; fi
  return 1
}

install_binary() { # <src binary path>
  src=$1
  mkdir -p "$INSTALL_DIR"
  dest="$INSTALL_DIR/stp"
  cp "$src" "$dest"
  chmod +x "$dest"
  say ""
  say "installed stp -> $dest"
  "$dest" version >&2 || true
  case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *) say ""
       say "note: $INSTALL_DIR is not on your PATH. Add it, e.g.:"
       say "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.profile && . ~/.profile" ;;
  esac
}

main() {
  [ "$FROM_SOURCE" = "1" ] && { build_from_source; return; }

  target=$(detect_target)
  tag=$(latest_version)
  [ -n "$tag" ] || die "could not determine the latest release; try --from-source"
  asset="stp-${target}.tar.gz"

  say "installing stp $tag for $target..."
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  if ! download "$tag" "$asset" "$tmp/$asset"; then
    say "could not download a prebuilt binary (private repo without gh, or no asset for $target)."
    say "falling back to building from source..."
    build_from_source
    return
  fi
  tar -xzf "$tmp/$asset" -C "$tmp"
  bin=$(find "$tmp" -name stp -type f | head -1)
  [ -n "$bin" ] || die "archive did not contain a stp binary"
  install_binary "$bin"
}

main
