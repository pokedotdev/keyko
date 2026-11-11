#!/usr/bin/env bash
# Creates the five npm packages with a placeholder 0.0.0, once, from a maintainer's machine.
#
# npm Trusted Publishing is configured on a package's settings page, so each package has to exist
# before the release workflow can publish it through OIDC. Publishing placeholders from your own
# login (with its two-factor prompt) avoids a long-lived publish token in CI. Afterwards:
#
#   1. On npmjs.com, for each package: Settings > Trusted Publisher > GitHub Actions, owner
#      pokedotdev, repository tierkey, workflow release.yml, and allow `npm publish`.
#   2. Push the release tag within 2 days: npm expires a trusted publisher that has not published.
#
#   npm login && npm/bootstrap.sh
set -euo pipefail

names=(@tierkey/linux-x64 @tierkey/linux-arm64 @tierkey/darwin-x64 @tierkey/darwin-arm64 tierkey)
work="$(mktemp -d)"
trap 'rm -r "$work"' EXIT

npm whoami > /dev/null || {
  echo "bootstrap.sh: log in first: npm login" >&2
  exit 1
}

for name in "${names[@]}"; do
  if npm view "$name" version > /dev/null 2>&1; then
    echo "$name already exists"
    continue
  fi
  dir="$work/${name//\//-}"
  mkdir -p "$dir"
  cat > "$dir/package.json" << EOF
{
  "name": "$name",
  "version": "0.0.0",
  "description": "Placeholder that reserves the name for tierkey; install tierkey 0.1.0 or later",
  "homepage": "https://github.com/pokedotdev/tierkey",
  "license": "MIT",
  "repository": {
    "type": "git",
    "url": "git+https://github.com/pokedotdev/tierkey.git",
    "directory": "npm"
  }
}
EOF
  printf '# %s\n\nPlaceholder; see https://github.com/pokedotdev/tierkey\n' "$name" > "$dir/README.md"
  (cd "$dir" && npm publish --access public)
done

cat << 'EOF'

Done. Now configure Trusted Publishing for each package on npmjs.com (owner pokedotdev,
repository tierkey, workflow release.yml, allow `npm publish`), then push the release tag
within 2 days.
EOF
