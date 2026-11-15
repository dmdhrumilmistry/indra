#!/bin/sh
# Install indra: one file, nothing else needed.
#
#   curl -fsSL https://raw.githubusercontent.com/dmdhrumilmistry/indra/main/install.sh | sh
#
# It picks the right build for this machine (Linux or macOS, Intel or ARM),
# checks its SHA-256, puts it in ~/.local/bin (or $INDRA_INSTALL_DIR), and
# tells you the one command to run next. Run it again to update.
#
# Settings (all optional):
#   INDRA_VERSION=0.7.0            a specific release instead of the newest
#   INDRA_INSTALL_DIR=/usr/local/bin   where to put it
#   INDRA_NO_MODIFY_PATH=1         do not add the folder to your shell's PATH
#   INDRA_NO_COMPLETIONS=1         do not set up Tab completion for your shell
#   INDRA_AUTO_UPDATE=1            keep indra up to date by itself (indra update --auto on)
#   INDRA_RELEASES_URL=...         another releases API (a mirror, or tests)
set -eu

REPO="dmdhrumilmistry/indra"
API="${INDRA_RELEASES_URL:-https://api.github.com/repos/$REPO/releases}"
DIR="${INDRA_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
fail() { printf 'indra install: %s\n' "$*" >&2; exit 1; }

fetch() {
  # fetch URL OUTFILE
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 -H "User-Agent: indra-installer" -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q --header "User-Agent: indra-installer" -O "$2" "$1"
  else
    fail "needs curl or wget"
  fi
}

# Which build: the Linux ones are static (musl), so they run on any distro.
os=$(uname -s)
arch=$(uname -m)
case "$arch" in
  x86_64|amd64) arch=x86_64 ;;
  arm64|aarch64) arch=aarch64 ;;
  *) fail "no prebuilt indra for $arch yet; build it from source: cargo install --git https://github.com/$REPO indra" ;;
esac
case "$os" in
  Linux) target="$arch-unknown-linux-musl" ;;
  Darwin) target="$arch-apple-darwin" ;;
  *) fail "this script is for Linux and macOS; on Windows run: irm https://raw.githubusercontent.com/$REPO/main/install.ps1 | iex" ;;
esac
asset="indra-$target"

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t indra)
trap 'rm -rf "$tmp"' EXIT INT TERM

# Which release: the one asked for, else the newest (tags v0.7.0, or
# indra-v0.6.0 and older; GitHub lists the newest first).
fetch "$API?per_page=50" "$tmp/releases.json" || fail "cannot reach GitHub to find the release"
tags=$(tr ',' '\n' < "$tmp/releases.json" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\(\(indra-\)\{0,1\}v[0-9][^"]*\)".*/\1/p')
want="${INDRA_VERSION:-}"
if [ -z "$want" ]; then
  tag=$(printf '%s\n' "$tags" | head -n 1)
else
  want="${want#indra-v}"; want="${want#v}"
  tag=$(printf '%s\n' "$tags" | grep -x -e "v$want" -e "indra-v$want" | head -n 1)
fi
[ -n "$tag" ] || fail "no indra release ${INDRA_VERSION:-found yet}"

base="${INDRA_DOWNLOAD_URL:-https://github.com/$REPO/releases/download}/$tag"
say "Installing indra $tag for $target ..."
fetch "$base/$asset" "$tmp/indra" || fail "download failed: $base/$asset"
fetch "$base/$asset.sha256" "$tmp/indra.sha256" || fail "checksum download failed"

# Check it is exactly the file that was built.
want=$(cut -d ' ' -f 1 < "$tmp/indra.sha256")
if command -v sha256sum >/dev/null 2>&1; then
  got=$(sha256sum "$tmp/indra" | cut -d ' ' -f 1)
else
  got=$(shasum -a 256 "$tmp/indra" | cut -d ' ' -f 1)
fi
[ "$want" = "$got" ] || fail "checksum mismatch (expected $want, got $got); nothing was installed"

mkdir -p "$DIR" || fail "cannot create $DIR (set INDRA_INSTALL_DIR to a folder you can write)"
chmod +x "$tmp/indra"
mv -f "$tmp/indra" "$DIR/indra" || fail "cannot write $DIR/indra (set INDRA_INSTALL_DIR to a folder you can write)"
say "Installed $("$DIR/indra" --version 2>/dev/null || echo indra) to $DIR/indra"

# Put it on PATH for new terminals, once.
case ":$PATH:" in
  *":$DIR:"*) on_path=1 ;;
  *) on_path=0 ;;
esac
if [ "$on_path" = 0 ] && [ -z "${INDRA_NO_MODIFY_PATH:-}" ]; then
  line="export PATH=\"$DIR:\$PATH\""
  for rc in "$HOME/.profile" "$HOME/.bashrc" "$HOME/.zshrc"; do
    if [ -f "$rc" ] || [ "$rc" = "$HOME/.profile" ]; then
      grep -qs "$DIR" "$rc" || { printf '\n# indra\n%s\n' "$line" >> "$rc"; }
    fi
  done
  say "Added $DIR to your PATH (open a new terminal, or run: $line)"
fi

# Tab completion for the user's shell (bash, zsh, fish), once; rerunning
# replaces it in place. Releases before 0.4.0 have no `completions`.
if [ -z "${INDRA_NO_COMPLETIONS:-}" ] && "$DIR/indra" completions --install --quiet >/dev/null 2>&1; then
  say "Tab completion is set up for $(basename "${SHELL:-your shell}") (open a new terminal; undo: indra completions --uninstall)"
fi
if [ -n "${INDRA_AUTO_UPDATE:-}" ] && "$DIR/indra" update --auto on >/dev/null 2>&1; then
  say "Automatic updates are on (turn off: indra update --auto off)"
fi

say ""
say "Next: start the console and open it in your browser:"
say ""
say "    indra serve --open"
say ""
say "Update later with: indra update   (or let it update itself: indra update --auto on)"
