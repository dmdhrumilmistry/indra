#!/bin/sh
# The body of the GitHub release v<version>: its section of CHANGELOG.md
# ("What's new"), then how to install. Used by release.yml.
#
#   sh .github/release-notes.sh 0.10.0 owner/repo > notes.md
set -eu
v="${1#indra-}"; v="${v#v}"
repo="$2"
case "$v" in
  ''|*[!0-9.]*) echo "release-notes: not a version: $1" >&2; exit 1 ;;
esac
# The lines under "## v<version>", up to the next "## ", without leading blank lines.
section=$(awk -v h="## v$v" '$0 == h { on = 1; next } on && /^## / { exit } on { print }' CHANGELOG.md | sed -e '/./,$!d')
if [ -n "$section" ]; then
  printf '## What'"'"'s new\n\n%s\n\nEvery release: [CHANGELOG.md](https://github.com/%s/blob/main/CHANGELOG.md)\n\n' "$section" "$repo"
fi
cat <<NOTES
## Install

One file, nothing else to install.

Linux and macOS:

    curl -fsSL https://raw.githubusercontent.com/$repo/main/install.sh | sh

Windows (PowerShell):

    irm https://raw.githubusercontent.com/$repo/main/install.ps1 | iex

Then: \`indra serve --open\`. Already installed? \`indra update\`.

Or download the file for your machine below; each has a .sha256, and SHA256SUMS lists them all.
NOTES
