#!/bin/sh
# Writes the winget manifests for a published release:
#   scripts/winget-manifests.sh 0.2.0                 # downloads the release's SHA256SUMS from GitHub
#   scripts/winget-manifests.sh 0.2.0 dist/SHA256SUMS # or uses a local copy
# Output: packaging/winget/manifests/t/TouchTap/AntigravityNotifier/<version>/TouchTap.AntigravityNotifier{,.installer,.locale.en-US}.yaml,
# the layout microsoft/winget-pkgs expects. Submit them with `wingetcreate submit` or a PR (RELEASING.md).
set -eu
version=$1
sums=${2:-}
tag="v$version"
root="$(cd "$(dirname "$0")/.." && pwd)"
base="https://github.com/touch-tap/antigravity-notifier/releases/download/$tag"
schema="1.28.0"   # the current winget manifest schema; https://github.com/microsoft/winget-pkgs/tree/master/doc/manifest/schema
id="TouchTap.AntigravityNotifier"

fail() { echo "winget-manifests: $*" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
if [ -z "$sums" ]; then
  sums="$work/SHA256SUMS"
  curl -fsSL "$base/SHA256SUMS" -o "$sums" || fail "no SHA256SUMS at $base (is $tag published?)"
fi
[ -f "$sums" ] || fail "$sums isn't a file"

# winget writes hashes in upper case; the schema accepts either
sha() {
  s=$(awk -v f="antigravity-notifier-$1-pc-windows-msvc.zip" '$2 == f || $2 == "*" f { print toupper($1) }' "$sums")
  [ -n "$s" ] || fail "no antigravity-notifier-$1-pc-windows-msvc.zip in $sums"
  printf '%s' "$s"
}
x64=$(sha x86_64)
arm64=$(sha aarch64)

# The day the release was published, when gh can tell us
date=""
if command -v gh >/dev/null 2>&1; then
  date=$(gh release view "$tag" -R touch-tap/antigravity-notifier --json publishedAt --jq '.publishedAt[:10]' 2>/dev/null || true)
fi

out="$root/packaging/winget/manifests/t/TouchTap/AntigravityNotifier/$version"
mkdir -p "$out"

cat > "$out/$id.yaml" <<YAML
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.version.$schema.schema.json
PackageIdentifier: $id
PackageVersion: $version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: $schema
YAML

cat > "$out/$id.installer.yaml" <<YAML
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.installer.$schema.schema.json
PackageIdentifier: $id
PackageVersion: $version
MinimumOSVersion: 10.0.0.0
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
  - RelativeFilePath: antigravity-notifier.exe
    PortableCommandAlias: antigravity-notifier.exe
Commands:
  - antigravity-notifier
${date:+ReleaseDate: $date
}Installers:
  - Architecture: x64
    InstallerUrl: $base/antigravity-notifier-x86_64-pc-windows-msvc.zip
    InstallerSha256: $x64
  - Architecture: arm64
    InstallerUrl: $base/antigravity-notifier-aarch64-pc-windows-msvc.zip
    InstallerSha256: $arm64
ManifestType: installer
ManifestVersion: $schema
YAML

cat > "$out/$id.locale.en-US.yaml" <<YAML
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.defaultLocale.$schema.schema.json
PackageIdentifier: $id
PackageVersion: $version
PackageLocale: en-US
Publisher: TouchTap
PublisherUrl: https://antigravitynotifier.com
PublisherSupportUrl: https://github.com/touch-tap/antigravity-notifier/issues
Author: TouchTap Ltd
PackageName: Antigravity Notifier
PackageUrl: https://antigravitynotifier.com/cli
License: PolyForm-Shield-1.0.0
LicenseUrl: https://github.com/touch-tap/antigravity-notifier/blob/$tag/LICENSE.md
Copyright: Copyright TouchTap Ltd
ShortDescription: Antigravity Notifier in the terminal, with phone notifications and approvals for OpenAI Codex
Description: |-
  Get a push on your phone when OpenAI Codex finishes or needs you, and approve, deny or answer
  from there. antigravity-notifier is one small binary that Codex calls through its hooks: it runs for a
  few milliseconds when something happens and exits, with no app running in the background. It
  also shows a Windows toast on the desktop. Pair it with the Antigravity Notifier app for iPhone or
  Android with "antigravity-notifier pair <code>", or run "antigravity-notifier" and scan the QR code.
Moniker: antigravity-notifier
Tags:
  - codex
  - openai-codex
  - notifications
  - cli
  - hooks
  - push-notifications
ReleaseNotesUrl: https://github.com/touch-tap/antigravity-notifier/releases/tag/$tag
ManifestType: defaultLocale
ManifestVersion: $schema
YAML

echo "Wrote ${out#"$root"/}/"
