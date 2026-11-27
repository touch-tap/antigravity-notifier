# First public release of antigravity-notifier (v0.1.0)

Run on the release Mac (this one: it has the Developer ID certificate, the `ccn-notary` notarytool
profile and the `antigravitynotifier-release-signing` key), from the engine repo, in this order.
Prepared 27 Nov 2026: the update signing key exists (RELEASING.md), the website
(antigravitynotifier.com) and api.antigravitynotifier.com are live, the dry run passed. Not yet
created: the GitHub repository, the npm packages, the Homebrew formula, the Scoop manifest.

```sh
cd ~/Documents/Development/touchtap/antigravitynotifier
# The engine branch becomes main (main is still codex-notifier 0.1.6 here)
git checkout main && git merge --ff-only feature/antigravity-engine

# 0. Back up the update signing key to the company password manager (once). Without it no later
#    release can be signed and every installed antigravity-notifier stops updating itself.
security find-generic-password -s antigravitynotifier-release-signing -a touchtap -w | pbcopy
#    ...paste into the password manager, then clear the clipboard:
pbcopy < /dev/null

# 1. Create the repository (it must be PUBLIC: install scripts, Homebrew, Scoop, winget and npm
#    all download the release assets anonymously) and push main.
gh repo create touch-tap/antigravity-notifier --public \
  --description "Antigravity Notifier in the terminal: phone notifications and approvals for Google Antigravity" \
  --homepage https://antigravitynotifier.com/cli --source . --remote origin --push

# 2. No CI secrets are needed: both workflows use the built-in GITHUB_TOKEN. Check that Actions may
#    write to releases (Settings > Actions > General > Workflow permissions: "Read and write", or
#    leave it, since release.yml asks for contents: write itself unless the org forbids it):
gh api repos/touch-tap/antigravity-notifier/actions/permissions/workflow
#    Wait for the first CI run on main to go green on all three OSes:
gh run watch -R touch-tap/antigravity-notifier "$(gh run list -R touch-tap/antigravity-notifier --workflow ci.yml --limit 1 --json databaseId --jq '.[0].databaseId')" --exit-status

# 3. Release. Cargo.toml is already at 0.1.0. This notarizes the notifier app, creates the DRAFT,
#    pushes tag v0.1.0, waits for the Release workflow, then signs + notarizes the macOS binaries
#    and uploads SHA256SUMS, SHA256SUMS.sig and antigravity-notifier.rb. Takes ~15-25 minutes.
scripts/release.sh 0.1.0
#    If the wait fails: scripts/release.sh 0.1.0 --resume

#    Release notes: the 0.1.0 section of CHANGELOG.md
awk '/^## 0.1.0/{p=1;next} /^## /{p=0} p' CHANGELOG.md > /tmp/agn-notes.md
gh release edit v0.1.0 -R touch-tap/antigravity-notifier --notes-file /tmp/agn-notes.md

# 4. Check the draft: 6 archives + antigravity-notifier-macos-app.zip + SHA256SUMS + SHA256SUMS.sig + antigravity-notifier.rb
gh release view v0.1.0 -R touch-tap/antigravity-notifier --json assets --jq '.assets[].name'
#    Then publish it (it becomes "latest"; install.sh/ps1 and upgrade see it from now on):
gh release edit v0.1.0 -R touch-tap/antigravity-notifier --draft=false --latest

# 5. Smoke test the public install path on this Mac, in a scratch folder (in a terminal it ends by
#    showing a pairing QR code: Ctrl-C there):
tmp=$(mktemp -d); ANTIGRAVITY_NOTIFIER_INSTALL_DIR=$tmp/bin ANTIGRAVITY_NOTIFIER_HOME=$tmp/h ANTIGRAVITY_NOTIFIER_GEMINI_HOME=$tmp/c \
  sh -c 'curl -fsSL https://antigravitynotifier.com/install.sh | sh' </dev/null
"$tmp/bin/antigravity-notifier" --version          # antigravity-notifier 0.1.0
codesign -dv "$tmp/bin/antigravity-notifier" 2>&1 | grep TeamIdentifier   # 4ABJZNGY79

# 6. Homebrew (touch-tap/homebrew-tap is checked out next to this repo)
gh release download v0.1.0 -R touch-tap/antigravity-notifier -p antigravity-notifier.rb -D ../homebrew-tap/Formula --clobber
git -C ../homebrew-tap add Formula/antigravity-notifier.rb
git -C ../homebrew-tap commit -m "antigravity-notifier 0.1.0"
git -C ../homebrew-tap push
brew install touch-tap/tap/antigravity-notifier && antigravity-notifier --version && brew uninstall antigravity-notifier

# 7. Scoop (writes packaging/scoop/antigravity-notifier.json here and commits in ../scoop-bucket)
scripts/scoop-bucket.sh 0.1.0
git -C ../scoop-bucket push

# 8. winget: SKIP for now, as for codex-notifier (never submitted; ccn's first PR was closed
#    because the Windows binary needs the VC runtime: link the CRT statically first). To prepare:
scripts/winget-manifests.sh 0.1.0
#    On a Windows machine:  wingetcreate submit --token <github token> packaging/winget/manifests/t/TouchTap/AntigravityNotifier/0.1.0
#    or fork microsoft/winget-pkgs, copy that folder to manifests/t/TouchTap/AntigravityNotifier/0.1.0,
#    and open a PR titled "New package: TouchTap.AntigravityNotifier version 0.1.0".

# 9. Commit the generated packaging files and push
git add packaging && git commit -m "Scoop and winget manifests for 0.1.0" && git push

# 10. npm (@touchtap/antigravity-notifier and its six platform packages; npm refused an unscoped alias)
npm whoami                                   # must be an owner of the touchtap org; npm login if 401
rm -rf dist/release && gh release download v0.1.0 -R touch-tap/antigravity-notifier -D dist/release \
  -p 'antigravity-notifier-*.tar.gz' -p 'antigravity-notifier-*.zip'
scripts/npm-packages.sh 0.1.0 dist/release
scripts/npm-publish.sh                       # in a real terminal: passkey in the browser
#    npm can take ~5 minutes to show new versions; don't re-publish. Then:
npx --prefer-online @touchtap/antigravity-notifier@0.1.0 --version

# 11. Website: the install scripts must still match the engine's copies byte for byte
cmp install/install.sh ../AntigravityNotifier_Website/install.sh && cmp install/install.ps1 ../AntigravityNotifier_Website/install.ps1
curl -fsSL https://antigravitynotifier.com/install.sh | cmp - install/install.sh

# 12. Log it in the change log (admin token as in the CCN memory notes)
curl -fsS -X POST https://ccnotifier-api.touchtap.workers.dev/api/admin/changes \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H 'Content-Type: application/json' \
  -d "{\"occurredAt\": $(date +%s), \"category\": \"release\", \"platform\": \"cli\", \"app\": \"antigravity\",
       \"title\": \"antigravity-notifier 0.1.0: first public release\",
       \"details\": \"Antigravity Notifier CLI on GitHub (touch-tap/antigravity-notifier), install.sh/ps1, Homebrew, Scoop and npm (winget not submitted). Default server api.antigravitynotifier.com; cross-promotes Claude Code Notifier and Notifier for Codex.\",
       \"version\": \"0.1.0\"}"
```

Afterwards: `antigravity-notifier upgrade --check` on an installed copy says it is the latest. The
server's `/api/cli/latest` answers per app (X-App antigravity: `NO_RELEASE` until this release is
published; it may cache for up to an hour), and the engine falls back to GitHub's releases API.
