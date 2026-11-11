#!/bin/sh
# Downloads a pinned litekeys release binary and checks it against a pinned SHA-256.
# Meant to be copied into a project (e.g. for Cloudflare Pages builds), so the checksum is in
# that project's git, not fetched from the same place as the binary.
#
#   install.sh VERSION SHA256 [DEST]      DEST defaults to .cache/litekeys-VERSION
#
# SHA256 is the line for this platform in the release's SHA256SUMS (litekeys-linux-amd64 on
# Cloudflare Pages). While the repository is private, set LITEKEYS_GITHUB_TOKEN to a token that
# can read it.
set -eu

version="${1:?usage: install.sh VERSION SHA256 [DEST]}"
sum="${2:?usage: install.sh VERSION SHA256 [DEST]}"
dest="${3:-.cache/litekeys-$version}"
repo="${LITEKEYS_REPO:-pokedotdev/litekeys}"

if [ -x "$dest" ]; then
  exit 0
fi
case "$(uname -s)" in
  Linux) os=linux ;;
  Darwin) os=darwin ;;
  *) echo "install.sh: no litekeys release for $(uname -s)" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch=amd64 ;;
  aarch64 | arm64) arch=arm64 ;;
  *) echo "install.sh: no litekeys release for $(uname -m)" >&2; exit 1 ;;
esac
asset="litekeys-$os-$arch"
mkdir -p "$(dirname "$dest")"
tmp="$dest.download"
if [ -n "${LITEKEYS_GITHUB_TOKEN:-}" ]; then
  # A private repository's assets only download through the API, by asset id. The token goes in
  # a header read from stdin, so it never shows in the process list.
  id="$(printf 'header = "Authorization: Bearer %s"\n' "$LITEKEYS_GITHUB_TOKEN" | curl -fsSL -K - \
    "https://api.github.com/repos/$repo/releases/tags/v$version" \
    | tr ',{' '\n\n' | grep -B3 "\"name\": *\"$asset\"" | sed -n 's/.*"id": *\([0-9]*\).*/\1/p' | tail -1)"
  [ -n "$id" ] || { echo "install.sh: v$version has no $asset" >&2; exit 1; }
  printf 'header = "Authorization: Bearer %s"\n' "$LITEKEYS_GITHUB_TOKEN" | curl -fsSL -K - \
    -H 'Accept: application/octet-stream' -o "$tmp" "https://api.github.com/repos/$repo/releases/assets/$id"
else
  curl -fsSL -o "$tmp" "https://github.com/$repo/releases/download/v$version/$asset"
fi
got="$( (sha256sum "$tmp" 2>/dev/null || shasum -a 256 "$tmp") | cut -d' ' -f1)"
if [ "$got" != "$sum" ]; then
  rm -f "$tmp"
  echo "install.sh: the downloaded $asset does not match its pinned checksum" >&2
  exit 1
fi
chmod +x "$tmp"
mv "$tmp" "$dest"
