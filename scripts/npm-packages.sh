#!/bin/sh
# Builds the npm packages for a release from its assets, ready for `npm publish` in each folder:
#   scripts/npm-packages.sh 0.2.0 dist     # dist holds the release's .tar.gz/.zip files
# Output in npm/out/: one package per platform, @touchtap/codex-notifier pinned to the same version, and the
# unscoped `codexnotifier` alias pinned to that.
set -eu
version=$1
dist=$2
root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/npm/out"
rm -rf "$out"
mkdir -p "$out"

# npm platform-arch → Rust target
for pair in darwin-arm64:aarch64-apple-darwin darwin-x64:x86_64-apple-darwin \
            linux-arm64:aarch64-unknown-linux-musl linux-x64:x86_64-unknown-linux-musl \
            win32-arm64:aarch64-pc-windows-msvc win32-x64:x86_64-pc-windows-msvc; do
  npm_name=${pair%%:*}
  target=${pair#*:}
  os=${npm_name%-*}
  cpu=${npm_name#*-}
  dir="$out/codex-notifier-$npm_name"
  mkdir -p "$dir/bin"
  if [ "$os" = win32 ]; then
    unzip -q -o "$dist/codex-notifier-$target.zip" -d "$dir/bin"
  else
    tar xzf "$dist/codex-notifier-$target.tar.gz" -C "$dir/bin"
    chmod 755 "$dir/bin/codex-notifier"
  fi
  cat > "$dir/package.json" <<JSON
{
  "name": "@touchtap/codex-notifier-$npm_name",
  "version": "$version",
  "description": "The codex-notifier binary for $npm_name. Install @touchtap/codex-notifier instead.",
  "homepage": "https://notifierforcodex.com/cli",
  "license": "SEE LICENSE IN LICENSE.md",
  "os": ["$os"],
  "cpu": ["$cpu"],
  "files": ["bin", "LICENSE.md"]
}
JSON
  cp "$root/LICENSE.md" "$dir/LICENSE.md"
done

for pkg in codex-notifier codexnotifier; do
  cp -R "$root/npm/$pkg" "$out/$pkg"
  cp "$root/LICENSE.md" "$out/$pkg/LICENSE.md"
done
node -e '
  const fs = require("fs"), v = process.argv[3];
  for (const file of process.argv.slice(1, 3)) {
    const p = JSON.parse(fs.readFileSync(file));
    p.version = v;
    for (const deps of [p.optionalDependencies, p.dependencies]) for (const k of Object.keys(deps ?? {})) deps[k] = v;
    fs.writeFileSync(file, JSON.stringify(p, null, 2) + "\n");
  }
' "$out/codex-notifier/package.json" "$out/codexnotifier/package.json" "$version"
echo "Packages in $out. Publish with scripts/npm-publish.sh (platform packages, then codex-notifier, then the alias)."
