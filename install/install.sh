#!/bin/sh
# Installs antigravity-notifier, the Antigravity Notifier, on macOS or Linux, then pairs it with your phone.
#
#   curl -fsSL https://antigravitynotifier.com/install.sh | sh              # install, then a QR code to pair
#   curl -fsSL https://antigravitynotifier.com/install.sh | sh -s 482913    # install and pair with the app's code
#   curl -fsSL https://antigravitynotifier.com/install.sh | sh -s 482913 --licence CCN-XXXX-XXXX-XXXX-XXXX
#   curl -fsSL https://antigravitynotifier.com/install.sh | sh -s -- --licence CCN-XXXX-XXXX-XXXX-XXXX
#                                                     # install, use a licence key (and pair)
#
# ANTIGRAVITY_NOTIFIER_VERSION=0.2.0 picks a release (default: the latest). ANTIGRAVITY_NOTIFIER_INSTALL_DIR overrides
# ~/.local/bin. ANTIGRAVITY_NOTIFIER_NO_ANALYTICS=1 (or DO_NOT_TRACK=1) stops the anonymous install events.
#
# Ported from ccn's install.sh (claudecodenotifier.com). Release assets are antigravity-notifier-<target>.tar.gz
# plus SHA256SUMS on the GitHub repo below, the names scripts/npm-packages.sh in the engine repo expects.
set -eu

repo="touch-tap/antigravity-notifier"
code=""
licence=""
while [ $# -gt 0 ]; do
  case "$1" in
    --licence|--license) licence="${2:-}"; [ $# -ge 2 ] && shift; shift ;;
    --licence=*|--license=*) licence="${1#*=}"; shift ;;
    --) shift ;;
    *) [ -z "$code" ] && code="$1"; shift ;;
  esac
done
dir="${ANTIGRAVITY_NOTIFIER_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
# Set reason before fail to send it with the stage (download: timeout or error)
reason=""
fail() { track cli_install_failed "\"stage\":\"$stage\"${reason:+,\"reason\":\"$reason\"}"; printf 'antigravity-notifier install: %s\n' "$*" >&2; exit 1; }

# One anonymous event per step, so we can see where installs fail: the event name, the stage, the
# OS and processor. Nothing else. ANTIGRAVITY_NOTIFIER_NO_ANALYTICS=1 or DO_NOT_TRACK=1 turns it off.
stage="start"
case "$(uname -s)" in Darwin) model="macos" ;; Linux) model="linux" ;; *) model="other" ;; esac
case "$(uname -m)" in arm64|aarch64) model="$model-aarch64" ;; x86_64|amd64) model="$model-x86_64" ;; *) model="$model-other" ;; esac
track() {
  case "${ANTIGRAVITY_NOTIFIER_NO_ANALYTICS:-}${DO_NOT_TRACK:-}" in ""|0|00) ;; *) return 0 ;; esac
  body="{\"context\":{\"platform\":\"cli\",\"deviceModel\":\"$model\",\"environment\":\"production\"},\"events\":[{\"name\":\"$1\",\"properties\":{\"agent\":\"antigravity\",\"method\":\"script\"${2:+,$2}}}]}"
  url="${ANTIGRAVITY_NOTIFIER_SERVER_URL:-https://api.antigravitynotifier.com}/api/events"
  if command -v curl >/dev/null 2>&1; then
    curl -fsS -m 3 -H 'Content-Type: application/json' -H 'X-App: antigravity' -d "$body" "$url" >/dev/null 2>&1 || true
  elif command -v wget >/dev/null 2>&1; then
    wget -q -T 3 -O /dev/null --header='Content-Type: application/json' --header='X-App: antigravity' --post-data="$body" "$url" >/dev/null 2>&1 || true
  fi
}
track cli_install_started "\"pairing\":\"$([ -n "$code" ] && echo yes || echo no)\""

stage="unsupported"
case "$(uname -s)" in
  Darwin) os="apple-darwin" ;;
  Linux) os="unknown-linux-musl" ;;
  *) fail "this script is for macOS and Linux. On Windows run: irm https://antigravitynotifier.com/install.ps1 | iex" ;;
esac
case "$(uname -m)" in
  arm64|aarch64) arch="aarch64" ;;
  x86_64|amd64) arch="x86_64" ;;
  *) fail "unsupported processor $(uname -m)" ;;
esac
# A shell running under Rosetta reports x86_64; the native build is better
if [ "$os" = apple-darwin ] && [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
  arch="aarch64"
fi
asset="antigravity-notifier-$arch-$os.tar.gz"

if [ -n "${ANTIGRAVITY_NOTIFIER_DOWNLOAD_BASE:-}" ]; then
  base="$ANTIGRAVITY_NOTIFIER_DOWNLOAD_BASE"   # for testing the script against a local build
elif [ -n "${ANTIGRAVITY_NOTIFIER_VERSION:-}" ]; then
  base="https://github.com/$repo/releases/download/v${ANTIGRAVITY_NOTIFIER_VERSION#v}"
else
  base="https://github.com/$repo/releases/latest/download"
fi

# Time limits, so a stalled GitHub download (common in mainland China) fails and says so instead
# of hanging forever. BusyBox wget has no -t, so only GNU wget gets the retry limit.
if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL --connect-timeout 20 --max-time 300 "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  if wget --version 2>/dev/null | grep -q 'GNU Wget'; then
    fetch() { wget -nv -T 30 -t 2 -O "$2" "$1"; }
  else
    fetch() { wget -T 30 -O "$2" "$1"; }
  fi
else
  fail "needs curl or wget"
fi
# Download $1 to $2, or stop with "could not download $3". A timeout is curl's exit 28, or wget's
# "timed out" message (wget's exit 4 is any network failure).
get() {
  rc=0
  fetch "$1" "$2" 2>"$tmp/fetch.err" || rc=$?
  [ "$rc" -eq 0 ] && return 0
  cat "$tmp/fetch.err" >&2
  if [ "$rc" -eq 28 ] || grep -qi 'timed out' "$tmp/fetch.err"; then
    reason="timeout"
    fail "could not download $3: the download timed out.
In mainland China you can install with: npx --registry=https://registry.npmmirror.com @touchtap/antigravity-notifier"
  fi
  reason="error"
  fail "could not download $3"
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

stage="download"
say "Downloading antigravity-notifier for $arch-$os..."
get "$base/$asset" "$tmp/$asset" "$base/$asset"
get "$base/SHA256SUMS" "$tmp/SHA256SUMS" "the checksums"

stage="checksum"
expected="$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$asset" | awk '{ print $1 }')"
else
  actual="$(shasum -a 256 "$tmp/$asset" | awk '{ print $1 }')"
fi
[ -n "$expected" ] && [ "$expected" = "$actual" ] || fail "checksum mismatch for $asset"

stage="unpack"
tar xzf "$tmp/$asset" -C "$tmp" || fail "could not unpack $asset"
mkdir -p "$dir"
# Write beside the target and rename, so a hook running the old binary is never half-overwritten
cp "$tmp/antigravity-notifier" "$dir/.antigravity-notifier.new"
chmod 755 "$dir/.antigravity-notifier.new"
mv "$dir/.antigravity-notifier.new" "$dir/antigravity-notifier"
stage="run"
version="$("$dir/antigravity-notifier" --version)" || fail "the downloaded antigravity-notifier did not run"
say "Installed $version to $dir/antigravity-notifier"

cli="antigravity-notifier"
case ":$PATH:" in
  *":$dir:"*) ;;
  *)
    cli="$dir/antigravity-notifier"
    say ""
    say "$dir is not on your PATH. The hooks don't need it, but to type antigravity-notifier yourself add this to your shell profile:"
    say "  export PATH=\"$dir:\$PATH\""
    ;;
esac

on_path=yes
[ "$cli" = antigravity-notifier ] || on_path=no
track cli_install_finished "\"pairing\":\"$([ -n "$code" ] && echo yes || echo no)\",\"on_path\":\"$on_path\""

export ANTIGRAVITY_NOTIFIER_INSTALL_METHOD=script
# The licence first, so the phone paired after it is unlocked as soon as it connects. A bad key
# doesn't stop the install; the message says what to do.
if [ -n "$licence" ]; then
  say ""
  "$dir/antigravity-notifier" activate "$licence" < /dev/null || say "The licence wasn't applied. Try again later: $cli activate $licence"
fi
# exec replaces this shell, so the EXIT trap never runs: clear the download first
rm -rf "$tmp"
trap - EXIT
# Pairing installs the Antigravity hooks. stdin is the piped script, so give pair
# the terminal for anything it asks.
say ""
if [ -n "$code" ]; then
  if (: < /dev/tty) 2>/dev/null; then
    exec "$dir/antigravity-notifier" pair "$code" < /dev/tty
  else
    exec "$dir/antigravity-notifier" pair "$code"
  fi
elif (: < /dev/tty) 2>/dev/null; then
  # No code: show a QR code to scan with the phone, which also offers the app if it isn't installed
  exec "$dir/antigravity-notifier" pair < /dev/tty
else
  say "To get Antigravity on your phone: open Antigravity Notifier, tap Machines > Link a computer, and run"
  say "  $cli pair <the code on your phone>"
  say "or run \`$cli\` in a terminal and scan the QR code."
fi
