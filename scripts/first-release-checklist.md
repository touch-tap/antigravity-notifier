# First public release of codex-notifier (v0.1.0)

Run on the release Mac (this one: it has the Developer ID certificate, the `ccn-notary` notarytool
profile and the `codexnotifier-release-signing` key), from the engine repo, in this order.
Prepared 8 Nov 2026; the dry run, signing and upgrade checks in RELEASING.md all passed.

```sh
cd ~/Documents/Development/touchtap/codexnotifier

# 0. Back up the update signing key to the company password manager (once). Without it no later
#    release can be signed and every installed codex-notifier stops updating itself.
security find-generic-password -s codexnotifier-release-signing -a touchtap -w | pbcopy
#    ...paste into the password manager, then clear the clipboard:
pbcopy < /dev/null

# 1. Create the repository (it must be PUBLIC: install scripts, Homebrew, Scoop, winget and npm
#    all download the release assets anonymously) and push main.
gh repo create touch-tap/codex-notifier --public \
  --description "Notifier for Codex in the terminal: phone notifications and approvals for OpenAI Codex" \
  --homepage https://notifierforcodex.com/cli --source . --remote origin --push

# 2. No CI secrets are needed: both workflows use the built-in GITHUB_TOKEN. Check that Actions may
#    write to releases (Settings > Actions > General > Workflow permissions: "Read and write", or
#    leave it, since release.yml asks for contents: write itself unless the org forbids it):
gh api repos/touch-tap/codex-notifier/actions/permissions/workflow
#    Wait for the first CI run on main to go green on all three OSes:
gh run watch -R touch-tap/codex-notifier "$(gh run list -R touch-tap/codex-notifier --workflow ci.yml --limit 1 --json databaseId --jq '.[0].databaseId')" --exit-status

# 3. Release. Cargo.toml is already at 0.1.0. This notarizes the notifier app, creates the DRAFT,
#    pushes tag v0.1.0, waits for the Release workflow, then signs + notarizes the macOS binaries
#    and uploads SHA256SUMS, SHA256SUMS.sig and codex-notifier.rb. Takes ~15-25 minutes.
scripts/release.sh 0.1.0
#    If the wait fails: scripts/release.sh 0.1.0 --resume

# 4. Check the draft: 6 archives + codex-notifier-macos-app.zip + SHA256SUMS + SHA256SUMS.sig + codex-notifier.rb
gh release view v0.1.0 -R touch-tap/codex-notifier --json assets --jq '.assets[].name'
#    Then publish it (it becomes "latest"; install.sh/ps1 and upgrade see it from now on):
gh release edit v0.1.0 -R touch-tap/codex-notifier --draft=false --latest

# 5. Smoke test the public install path on this Mac, in a scratch folder (in a terminal it ends by
#    showing a pairing QR code: Ctrl-C there):
tmp=$(mktemp -d); CODEX_NOTIFIER_INSTALL_DIR=$tmp/bin CODEX_NOTIFIER_HOME=$tmp/h CODEX_HOME=$tmp/c \
  sh -c 'curl -fsSL https://notifierforcodex.com/install.sh | sh' </dev/null
"$tmp/bin/codex-notifier" --version          # codex-notifier 0.1.0
codesign -dv "$tmp/bin/codex-notifier" 2>&1 | grep TeamIdentifier   # 4ABJZNGY79

# 6. Homebrew (touch-tap/homebrew-tap is checked out next to this repo)
gh release download v0.1.0 -R touch-tap/codex-notifier -p codex-notifier.rb -D ../homebrew-tap/Formula --clobber
git -C ../homebrew-tap add Formula/codex-notifier.rb
git -C ../homebrew-tap commit -m "codex-notifier 0.1.0"
git -C ../homebrew-tap push
brew install touch-tap/tap/codex-notifier && codex-notifier --version && brew uninstall codex-notifier

# 7. Scoop (writes packaging/scoop/codex-notifier.json here and commits in ../scoop-bucket)
scripts/scoop-bucket.sh 0.1.0
git -C ../scoop-bucket push

# 8. winget (manifests here; first submission is reviewed by a person)
scripts/winget-manifests.sh 0.1.0
#    On a Windows machine:  wingetcreate submit --token <github token> packaging/winget/manifests/t/TouchTap/CodexNotifier/0.1.0
#    or fork microsoft/winget-pkgs, copy that folder to manifests/t/TouchTap/CodexNotifier/0.1.0,
#    and open a PR titled "New package: TouchTap.CodexNotifier version 0.1.0".

# 9. Commit the generated packaging files and push
git add packaging && git commit -m "Scoop and winget manifests for 0.1.0" && git push

# 10. npm (@touchtap/codex-notifier and its six platform packages; npm refused an unscoped alias)
npm whoami                                   # must be an owner of the touchtap org; npm login if 401
rm -rf dist/release && gh release download v0.1.0 -R touch-tap/codex-notifier -D dist/release \
  -p 'codex-notifier-*.tar.gz' -p 'codex-notifier-*.zip'
scripts/npm-packages.sh 0.1.0 dist/release
scripts/npm-publish.sh                       # in a real terminal: passkey in the browser
#    npm can take ~5 minutes to show new versions; don't re-publish. Then:
npx --prefer-online @touchtap/codex-notifier@0.1.0 --version
npx --prefer-online codexnotifier@0.1.0 --version

# 11. Website: the install scripts must still match the engine's copies byte for byte
cmp install/install.sh ../CodexNotifier_Website/install.sh && cmp install/install.ps1 ../CodexNotifier_Website/install.ps1
curl -fsSL https://notifierforcodex.com/install.sh | cmp - install/install.sh

# 12. Log it in the change log (admin token as in the CCN memory notes)
curl -fsS -X POST https://ccnotifier-api.touchtap.workers.dev/api/admin/changes \
  -H "Authorization: Bearer $ADMIN_TOKEN" -H 'Content-Type: application/json' \
  -d "{\"occurredAt\": $(date +%s), \"category\": \"release\", \"platform\": \"cli\",
       \"title\": \"codex-notifier 0.1.0: first public release\",
       \"details\": \"Notifier for Codex CLI on GitHub (touch-tap/codex-notifier), install.sh/ps1, Homebrew, Scoop and npm; winget submitted\",
       \"version\": \"0.1.0\"}"
```

Afterwards: `codex-notifier upgrade --check` on an installed copy says it is the latest. The server's
`/api/cli/latest` still answers with ccn's release, which codex-notifier ignores in favour of GitHub's
releases API until the server answers per app.
