#!/bin/sh
# Updates the Scoop manifest for a published release, in this repo and in a checkout of the bucket:
#   scripts/scoop-bucket.sh 0.2.0                 # downloads the release's SHA256SUMS from GitHub
#   scripts/scoop-bucket.sh 0.2.0 dist/SHA256SUMS # or uses a local copy
# Writes packaging/scoop/antigravity-notifier.json here, then bucket/antigravity-notifier.json in the touch-tap/scoop-bucket checkout
# (SCOOP_BUCKET_DIR, default ../scoop-bucket next to this repo) and commits there. Push it yourself.
# The owner creates the bucket repository once; see RELEASING.md.
set -eu
version=$1
sums=${2:-}
tag="v$version"
root="$(cd "$(dirname "$0")/.." && pwd)"
bucket="${SCOOP_BUCKET_DIR:-$root/../scoop-bucket}"
base="https://github.com/touch-tap/antigravity-notifier/releases/download/$tag"

fail() { echo "scoop-bucket: $*" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
if [ -z "$sums" ]; then
  sums="$work/SHA256SUMS"
  curl -fsSL "$base/SHA256SUMS" -o "$sums" || fail "no SHA256SUMS at $base (is $tag published?)"
fi
[ -f "$sums" ] || fail "$sums isn't a file"

sha() {
  s=$(awk -v f="antigravity-notifier-$1-pc-windows-msvc.zip" '$2 == f || $2 == "*" f { print tolower($1) }' "$sums")
  [ -n "$s" ] || fail "no antigravity-notifier-$1-pc-windows-msvc.zip in $sums"
  printf '%s' "$s"
}
x64=$(sha x86_64)
arm64=$(sha aarch64)

# `checkver: github` watches the releases feed; `autoupdate` fills the next version in from the
# same asset names and reads each hash from that release's SHA256SUMS, so the bucket's own
# `checkver -u` run (or Scoop's Excavator) can bump it without this script.
manifest() {
cat <<JSON
{
    "version": "$version",
    "description": "Antigravity Notifier in the terminal: phone notifications and approvals for OpenAI Codex",
    "homepage": "https://antigravitynotifier.com/cli",
    "license": {
        "identifier": "PolyForm-Shield-1.0.0",
        "url": "https://github.com/touch-tap/antigravity-notifier/blob/main/LICENSE.md"
    },
    "suggest": {
        "vcredist": "extras/vcredist2022"
    },
    "architecture": {
        "64bit": {
            "url": "$base/antigravity-notifier-x86_64-pc-windows-msvc.zip",
            "hash": "$x64"
        },
        "arm64": {
            "url": "$base/antigravity-notifier-aarch64-pc-windows-msvc.zip",
            "hash": "$arm64"
        }
    },
    "bin": "antigravity-notifier.exe",
    "checkver": "github",
    "autoupdate": {
        "architecture": {
            "64bit": {
                "url": "https://github.com/touch-tap/antigravity-notifier/releases/download/v\$version/antigravity-notifier-x86_64-pc-windows-msvc.zip",
                "hash": {
                    "url": "\$baseurl/SHA256SUMS"
                }
            },
            "arm64": {
                "url": "https://github.com/touch-tap/antigravity-notifier/releases/download/v\$version/antigravity-notifier-aarch64-pc-windows-msvc.zip",
                "hash": {
                    "url": "\$baseurl/SHA256SUMS"
                }
            }
        }
    },
    "notes": [
        "Connect your phone: run",
        "  antigravity-notifier",
        "and scan the QR code with the phone's camera, or run",
        "  antigravity-notifier pair <the code on your phone>"
    ]
}
JSON
}

mkdir -p "$root/packaging/scoop"
manifest > "$root/packaging/scoop/antigravity-notifier.json"
echo "Wrote packaging/scoop/antigravity-notifier.json"

[ -d "$bucket/.git" ] || fail "no checkout of touch-tap/scoop-bucket at $bucket (set SCOOP_BUCKET_DIR, or see RELEASING.md)"
mkdir -p "$bucket/bucket"
cp "$root/packaging/scoop/antigravity-notifier.json" "$bucket/bucket/antigravity-notifier.json"
if git -C "$bucket" diff --quiet -- bucket/antigravity-notifier.json && [ -z "$(git -C "$bucket" ls-files --others --exclude-standard bucket/antigravity-notifier.json)" ]; then
  echo "$bucket/bucket/antigravity-notifier.json already describes $version"
  exit 0
fi
git -C "$bucket" add bucket/antigravity-notifier.json
git -C "$bucket" commit -q -m "antigravity-notifier $version" -- bucket/antigravity-notifier.json
echo "Committed antigravity-notifier $version in $bucket; push it with: git -C $bucket push"
