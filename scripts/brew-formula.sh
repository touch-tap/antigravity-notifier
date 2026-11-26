#!/bin/sh
# Prints the Homebrew formula for a release: scripts/brew-formula.sh 0.2.0 dist/SHA256SUMS
# Copy the output to touch-tap/homebrew-tap as Formula/antigravity-notifier.rb once the release is published.
# The formula is named after the command, as `antigravity-notifier upgrade` expects
# (`brew upgrade touch-tap/tap/antigravity-notifier`, and Homebrew installs under Cellar/antigravity-notifier/).
set -eu
version=$1
sums=$2
[ -r "$sums" ] || { echo "brew-formula: cannot read $sums" >&2; exit 1; }
# `<hash>  <name>` or `<hash> *<name>`. Each is read here, not inside the heredoc, where a missing
# one would print an empty sha256 and still exit 0
sha() {
  s=$(awk -v f="antigravity-notifier-$1.tar.gz" '{ n = $2; sub(/^\*/, "", n) } n == f { print $1; exit }' "$sums")
  case "$s" in
    [0-9a-fA-F]*) [ ${#s} -eq 64 ] && { echo "$s"; return; } ;;
  esac
  echo "brew-formula: no sha256 for antigravity-notifier-$1.tar.gz in $sums" >&2
  exit 1
}
base="https://github.com/touch-tap/antigravity-notifier/releases/download/v$version"
mac_arm=$(sha aarch64-apple-darwin)
mac_intel=$(sha x86_64-apple-darwin)
linux_arm=$(sha aarch64-unknown-linux-musl)
linux_intel=$(sha x86_64-unknown-linux-musl)

cat <<RUBY
class AntigravityNotifier < Formula
  desc "Phone notifications and approvals for OpenAI Codex, from any terminal"
  homepage "https://antigravitynotifier.com/cli"
  version "$version"

  on_macos do
    on_arm do
      url "$base/antigravity-notifier-aarch64-apple-darwin.tar.gz"
      sha256 "$mac_arm"
    end
    on_intel do
      url "$base/antigravity-notifier-x86_64-apple-darwin.tar.gz"
      sha256 "$mac_intel"
    end
  end

  on_linux do
    on_arm do
      url "$base/antigravity-notifier-aarch64-unknown-linux-musl.tar.gz"
      sha256 "$linux_arm"
    end
    on_intel do
      url "$base/antigravity-notifier-x86_64-unknown-linux-musl.tar.gz"
      sha256 "$linux_intel"
    end
  end

  def install
    bin.install "antigravity-notifier"
  end

  def caveats
    <<~EOS
      Run \`antigravity-notifier\` to finish setting up: it installs the Codex hooks (desktop
      notifications from then on) and shows a QR code to pair your phone, or run
        antigravity-notifier pair <the code on your phone>
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/antigravity-notifier --version")
  end
end
RUBY
