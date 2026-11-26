# Releasing antigravity-notifier

Ported from ccn's RELEASING.md; the machinery is the same with the names changed. The command,
binary, archives, Homebrew formula, Scoop manifest and winget alias are all `antigravity-notifier`; on npm
it is `@touchtap/antigravity-notifier` only, as for codex-notifier (whose unscoped alias npm refused).

1. Bump `version` in `Cargo.toml` (and run `cargo build` so `Cargo.lock` follows), commit, push.
2. On the release Mac, `scripts/release.sh 0.2.0`. It signs and notarizes the macOS notifier app,
   creates a **draft** GitHub release on `touch-tap/antigravity-notifier` with it
   (`antigravity-notifier-macos-app.zip`), and pushes the tag. The Release workflow then builds six
   targets and adds `antigravity-notifier-<target>.tar.gz`/`.zip`, `SHA256SUMS` and `antigravity-notifier.rb`
   to that draft. The script waits for it, then signs the two macOS binaries with the Developer ID
   (identifier `com.touchtap.antigravitynotifier`, hardened runtime), notarizes them, replaces them,
   `SHA256SUMS` and `antigravity-notifier.rb` in the draft, and last signs `SHA256SUMS` with the update
   signing key and uploads `SHA256SUMS.sig`. If the wait fails, `scripts/release.sh 0.2.0 --resume`
   skips straight to waiting and signing. Never push a release tag by hand: the macOS builds need
   the notarized app from the draft and fail without it, and the binaries would stay unsigned.
   Notarization uses the notarytool profile `ccn-notary` (the same TouchTap App Store Connect key);
   `NOTARY_PROFILE=<name>` picks another.
3. Check the draft, then publish it. The install scripts download from
   `releases/latest/download/…`, and `antigravity-notifier upgrade` (and the daily background check) asks
   the server's `GET /api/cli/latest`, falling back to GitHub's releases API while the server only
   knows ccn's releases. It always downloads from
   `https://github.com/touch-tap/antigravity-notifier/releases/download/<tag>/` and verifies the archive
   against `SHA256SUMS` and `SHA256SUMS.sig`. The repository must be public.
4. Homebrew: copy `antigravity-notifier.rb` from the release to `touch-tap/homebrew-tap` as
   `Formula/antigravity-notifier.rb`, push. Users: `brew install touch-tap/tap/antigravity-notifier`, which is
   also what `antigravity-notifier upgrade` tells a Homebrew install to run.
5. winget: `scripts/winget-manifests.sh 0.2.0` writes the three manifests to
   `packaging/winget/manifests/t/TouchTap/AntigravityNotifier/0.2.0/` (commit them), then submit to
   microsoft/winget-pkgs (`wingetcreate submit --token <token> <that folder>`, or a PR titled
   `New package: TouchTap.AntigravityNotifier version 0.2.0` the first time).
6. Scoop: `scripts/scoop-bucket.sh 0.2.0` writes `packaging/scoop/antigravity-notifier.json` here (commit
   it) and `bucket/antigravity-notifier.json` in a checkout of `touch-tap/scoop-bucket` next to this repo
   (or at `SCOOP_BUCKET_DIR`), and commits there; push that checkout. The bucket already exists for
   ccn, so users who added it get `scoop install antigravity-notifier`.
7. npm: download the release assets into `dist/`, run `scripts/npm-packages.sh 0.2.0 dist`, then
   `scripts/npm-publish.sh` in a real terminal (signed in as an owner of the `touchtap` org).
8. Website: `install/install.sh` and `install/install.ps1` are byte-for-byte copies of the ones
   served from antigravitynotifier.com (repo AntigravityNotifier_Website). Change them in one place and copy
   to the other.
9. Log the release in the change log (`POST /api/admin/changes`, category `release`, platform `cli`).

## Dry run

`scripts/release.sh 0.2.0 --dry-run` builds the notifier app and this Mac's binary, signs the binary
with the Developer ID (not notarized), and writes `dist/dry-run/` with the archive, `SHA256SUMS` and
a real `SHA256SUMS.sig`. No git checks, tag or GitHub. To prove an upgrade, make a second dry run at
a higher version (in a scratch clone with `Cargo.toml` bumped), then
`scripts/upgrade-check.sh dist/dry-run <clone>/dist/dry-run <higher version>`: it serves both
locally (`ANTIGRAVITY_NOTIFIER_SERVER_URL`, `ANTIGRAVITY_NOTIFIER_DOWNLOAD_BASE`) and checks that the signed
release installs and that a tampered or swapped `SHA256SUMS`, or a missing signature, is refused.

## The update signing key

`antigravity-notifier upgrade` installs a release only if its `SHA256SUMS.sig` is an ed25519 signature of
`SHA256SUMS` by a key in `RELEASE_KEYS` (src/update.rs). The private key lives only in the release
Mac's login keychain as the generic password `antigravitynotifier-release-signing` (account `touchtap`),
and `scripts/release.sh` refuses to start without it or if its public half isn't in `RELEASE_KEYS`.
It was created on 26 Nov 2026 with `cargo run --release --example release-sign -- keygen`; public key
`82c67760427ba196edad705eec52fd9270d2b8694f20f2aa8041bc688dedcbaa`. It is separate from ccn's and
codex-notifier's keys: a shared key would let a release signed for one product install as another.

Keep a copy in the company password manager: without it no later release can be signed and every
installed antigravity-notifier would stop updating itself. To copy it:
`security find-generic-password -s antigravitynotifier-release-signing -a touchtap -w | pbcopy`. On a new
release Mac: `security add-generic-password -s antigravitynotifier-release-signing -a touchtap -w`.

To replace the key: ship a release with both public keys in `RELEASE_KEYS` (signed with the old
key), then sign later releases with the new key and drop the old one.

## Setting up a release Mac

The same as ccn's (its RELEASING.md): Xcode, `gh` signed in, the "Developer ID Application:
TouchTap LTD (4ABJZNGY79)" certificate in the login keychain, the `ccn-notary` notarytool profile,
and the signing key above.
