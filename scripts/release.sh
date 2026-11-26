#!/bin/sh
# Starts a release from this Mac, which holds the Developer ID certificate and the update signing key:
#   scripts/release.sh 0.2.0
#   scripts/release.sh 0.2.0 --resume    (after the wait failed: skips to signing the macOS binaries)
#   scripts/release.sh 0.2.0 --dry-run   (this Mac's target only, into dist/dry-run/; nothing leaves the Mac)
# Builds the macOS notifier app, signs and notarizes it, creates the DRAFT release with it attached,
# then pushes the tag. The Release workflow builds every target with that app embedded and adds them
# to the draft; this script waits for it, then signs and notarizes the two macOS binaries with the
# Developer ID and replaces them (and SHA256SUMS, its signature and antigravity-notifier.rb) in the draft.
# Needs the certificate and the update signing key in the keychain, and a notarytool profile
# (NOTARY_PROFILE, default ccn-notary: the same TouchTap App Store Connect key ccn uses). See RELEASING.md.
#
# --dry-run builds the notifier app and this Mac's binary with the release profile, signs the binary
# with the Developer ID (no notarization), packages it under the release's asset name, writes
# SHA256SUMS and SHA256SUMS.sig with the real signing key. No git checks, no tag, no GitHub.
# scripts/upgrade-check.sh then tests a signed upgrade from one dry run to a newer one.
set -eu
version=$1
mode=${2:-}
tag="v$version"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
notary=${NOTARY_PROFILE:-ccn-notary}
repo=touch-tap/antigravity-notifier
key_service=antigravitynotifier-release-signing

fail() { echo "release: $*" >&2; exit 1; }

case "$mode" in ""|--resume|--dry-run) ;; *) fail "unknown option $mode" ;; esac
grep -q "^version = \"$version\"$" Cargo.toml || fail "Cargo.toml's version isn't $version"
if [ "$mode" != --dry-run ]; then
  [ "$(git branch --show-current)" = main ] || fail "not on main"
  [ -z "$(git status --porcelain)" ] || fail "the working tree has changes"
  git fetch -q origin main
  [ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || fail "main isn't the same as origin/main"
fi
if [ "$mode" = --resume ]; then
  git fetch -q origin tag "$tag"
  sha=$(git rev-parse "$tag^{commit}")
  gh release view "$tag" -R "$repo" --json isDraft --jq .isDraft | grep -q true || fail "$tag isn't a draft release"
elif [ "$mode" != --dry-run ]; then
  ! git rev-parse -q --verify "refs/tags/$tag" >/dev/null || fail "$tag already exists"
  sha=$(git rev-parse HEAD)
fi

# The update signing key (RELEASING.md, "The update signing key"): antigravity-notifier upgrade refuses a
# release whose SHA256SUMS isn't signed by a key built into the binary, so check it before anything starts
signing_key=$(security find-generic-password -s "$key_service" -a touchtap -w 2>/dev/null) \
  || fail "no update signing key in the keychain ($key_service)"
cargo build -q --release --example release-sign
public=$(ANTIGRAVITY_NOTIFIER_RELEASE_SIGNING_KEY="$signing_key" target/release/examples/release-sign public)
grep -q "\"$public\"" src/update.rs || fail "the keychain's signing key ($public) isn't in RELEASE_KEYS in src/update.rs"

identity=${CODESIGN_IDENTITY:-$(security find-identity -v -p codesigning \
  | grep -o '"Developer ID Application[^"]*"' | head -1 | tr -d '"')}
[ -n "$identity" ] || fail "no Developer ID Application certificate in the keychain"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Signs one macOS binary with the Developer ID. With a stable identity and identifier the keychain
# lets each new version read the token the last one stored, instead of prompting in the middle of a hook.
sign_binary() {
  codesign --force --options runtime --timestamp --identifier com.touchtap.antigravitynotifier --sign "$identity" "$1"
  codesign -dv "$1" 2>&1 | grep -q "TeamIdentifier=4ABJZNGY79" || fail "$1 isn't signed by TouchTap"
}

if [ "$mode" = --dry-run ]; then
  out="$root/dist/dry-run"
  rm -rf "$out"
  mkdir -p "$out"
  target=$(rustc -vV | sed -n 's/^host: //p')
  case "$target" in *-apple-darwin) ;; *) fail "--dry-run builds a macOS target; this host is $target" ;; esac
  CODESIGN_IDENTITY="$identity" scripts/build-macos-notifier.sh "$version" "$work" >/dev/null
  ANTIGRAVITY_NOTIFIER_NOTIFIER_APP="$work/Antigravity Notifier.app" cargo build -q --release --locked --target "$target"
  mkdir -p "$work/bin"
  cp "target/$target/release/antigravity-notifier" "$work/bin/antigravity-notifier"
  sign_binary "$work/bin/antigravity-notifier"
  tar czf "$out/antigravity-notifier-$target.tar.gz" -C "$work/bin" antigravity-notifier
  (cd "$out" && shasum -a 256 antigravity-notifier-*.tar.gz > SHA256SUMS)
  ANTIGRAVITY_NOTIFIER_RELEASE_SIGNING_KEY="$signing_key" target/release/examples/release-sign sign "$out/SHA256SUMS"
  echo "Dry run: $out"
  ls -l "$out"
  cat "$out/SHA256SUMS"
  echo "(The Homebrew formula needs all four Unix archives, so a one-target dry run doesn't write it.)"
  exit 0
fi

if [ "$mode" != --resume ]; then
CODESIGN_IDENTITY="$identity" scripts/build-macos-notifier.sh "$version" "$work" >/dev/null
app="$work/Antigravity Notifier.app"

echo "Notarizing the notifier app (a few minutes)..."
ditto -c -k --keepParent "$app" "$work/submit.zip"
xcrun notarytool submit "$work/submit.zip" --keychain-profile "$notary" --wait
xcrun stapler staple "$app"
spctl --assess --type exec "$app" || fail "Gatekeeper rejects the notarized app"

ditto -c -k --keepParent "$app" "$work/antigravity-notifier-macos-app.zip"
gh release create "$tag" "$work/antigravity-notifier-macos-app.zip" -R "$repo" --draft --target "$sha" \
  --title "antigravity-notifier $tag" --generate-notes
git tag "$tag"
git push origin "$tag"
fi

echo "Waiting for the Release workflow..."
run_id=""
for _ in $(seq 1 30); do
  # Matched on the commit too: a tag deleted and pushed again leaves the old tag's runs listed
  run_id=$(gh run list -R "$repo" --workflow release.yml --event push --limit 10 --json databaseId,headBranch,headSha \
    --jq ".[] | select(.headBranch == \"$tag\" and .headSha == \"$sha\") | .databaseId" | head -1)
  [ -n "$run_id" ] && break
  sleep 5
done
[ -n "$run_id" ] || fail "the Release workflow for $tag didn't start"
gh run watch "$run_id" -R "$repo" --exit-status >/dev/null || fail "the Release workflow failed: gh run view $run_id -R $repo"

echo "Signing and notarizing the macOS binaries..."
mac="$work/mac"
mkdir -p "$mac/submit"
gh release download "$tag" -R "$repo" -D "$mac" -p 'antigravity-notifier-*-apple-darwin.tar.gz' -p SHA256SUMS
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  mkdir -p "$mac/$target"
  tar xzf "$mac/antigravity-notifier-$target.tar.gz" -C "$mac/$target"
  sign_binary "$mac/$target/antigravity-notifier"
  cp "$mac/$target/antigravity-notifier" "$mac/submit/antigravity-notifier-$target"
done
ditto -c -k "$mac/submit" "$mac/submit.zip"
xcrun notarytool submit "$mac/submit.zip" --keychain-profile "$notary" --wait
for target in aarch64-apple-darwin x86_64-apple-darwin; do
  tar czf "$mac/antigravity-notifier-$target.tar.gz" -C "$mac/$target" antigravity-notifier
  sum=$(shasum -a 256 "$mac/antigravity-notifier-$target.tar.gz" | cut -d' ' -f1)
  awk -v f="antigravity-notifier-$target.tar.gz" -v s="$sum" '{ n = $2; sub(/^\*/, "", n) } n == f { print s "  " f; next } { print }' "$mac/SHA256SUMS" > "$mac/SHA256SUMS.new"
  mv "$mac/SHA256SUMS.new" "$mac/SHA256SUMS"
done
scripts/brew-formula.sh "$version" "$mac/SHA256SUMS" > "$mac/antigravity-notifier.rb"
# Signed last, once SHA256SUMS has its final macOS lines: this is what antigravity-notifier upgrade checks
ANTIGRAVITY_NOTIFIER_RELEASE_SIGNING_KEY="$signing_key" target/release/examples/release-sign sign "$mac/SHA256SUMS"
gh release upload "$tag" -R "$repo" --clobber \
  "$mac/antigravity-notifier-aarch64-apple-darwin.tar.gz" "$mac/antigravity-notifier-x86_64-apple-darwin.tar.gz" \
  "$mac/SHA256SUMS" "$mac/SHA256SUMS.sig" "$mac/antigravity-notifier.rb"
echo "Done: $tag is a draft with signed macOS binaries. Check it, then publish it."
