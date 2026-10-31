# codex-notifier — Codex Notifier for the terminal

Get a push on your phone when OpenAI Codex finishes or needs you, and approve, deny or answer
from there. Works on macOS, Linux and Windows, in any terminal, with no app running in the
background.

`codex-notifier` is one small binary that Codex calls through its lifecycle hooks. It runs for a
few milliseconds when something happens and exits. Install it once with `codex-notifier pair`;
after that every `codex` session is covered, however you start it.

**Status: proof of concept (30 Oct 2026).** It pairs with the existing Claude Code Notifier phone
app against a local server, and the real Codex CLI 0.166 runs its hooks from the trust hashes the
installer writes. No release, website, store listing or brand exists yet. See
`scripts/e2e-local.sh` for what is verified.

## Install (today: from source)

```sh
cargo build --release
./target/release/codex-notifier pair 482913     # the code the phone app shows
./target/release/codex-notifier                 # or show a QR code here
```

`codex-notifier` on its own pairs a computer that isn't paired yet. Once paired, the phone gets a
test notification it can answer, and `codex-notifier` shows the answer.

## What you get

| On your phone | How it works underneath |
| --- | --- |
| "Codex has finished" the moment a turn ends | the `Stop` hook |
| Approve or deny a command before the terminal even asks | the `PermissionRequest` hook returns the decision |
| Answer a question Codex asked | the `PreToolUse` hook on `request_user_input` blocks the call with your answers as the reason, which the model reads and continues from |
| Send a short follow-up right after Codex finishes | `Stop` returns it as Codex's next instruction |
| A lock-screen card while Codex works (files, lines, plan step) | `apply_patch` and `update_plan` calls seen by `PreToolUse` |

If you are at the keyboard (any input in the last 30 s) nothing waits: the terminal prompt appears
at once. While you are at the terminal Codex runs in, the phone gets nothing (on macOS that means
the terminal is also the frontmost app; elsewhere, keyboard activity alone); if you are active in
another app it gets an informational card. If you are away, the hook holds for the phone's
answer, up to a ceiling you choose, and releases the moment you touch the keyboard.

## How it hooks into Codex

`codex-notifier setup` (which `pair` runs) adds six entries to `$CODEX_HOME/hooks.json`
(`~/.codex/hooks.json`): `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`,
`Stop` and `SessionEnd`, each `codex-notifier hook`. Everything else in the file is left as it was
and a one-time backup is kept beside it.

Codex ignores a hook until it is trusted. Trust is a SHA-256 of the hook's normalised definition,
stored in `config.toml` as `[hooks.state."<hooks.json path>:<event>:<group>:<handler>"]
trusted_hash`. `setup` computes the same hash Codex does (`hook_hash` in
codex-rs/hooks/src/engine/discovery.rs: event name, matcher and the handler with its timeout, as
TOML, then JSON with sorted keys) and writes it, so the hooks run on the next session with no
visit to `/hooks`. `codex-notifier doctor` reports when a hash is missing or stale, and
`uninstall` removes exactly the entries it added.

Codex has no Notification event, so when you are at the keyboard the `PermissionRequest` hook
sends the informational card itself. `SessionEnd` hooks are capped at 3 s by Codex. Questions
marked secret (`isSecret`) never go to the phone.

## Commands

```
codex-notifier                     pair if this computer isn't paired yet, otherwise show its status
codex-notifier pair [CODE]         pair with a phone (installs the hooks); without a code, shows a QR code
codex-notifier doctor              check hooks, trust, credentials, server, presence detection
codex-notifier test                send a test notification
codex-notifier devices             list paired phones; codex-notifier devices revoke <name>
codex-notifier config show         effective settings (also editable from the phone)
codex-notifier config set KEY VAL  e.g. hold_ceiling_seconds 300, quiet_hours 22:00-08:00, stop_hold_seconds 0
codex-notifier sync                pull settings from the server now
codex-notifier setup               reinstall the hooks and their trust hashes (after moving the binary)
codex-notifier uninstall           remove hooks, trust entries, pairings, credentials and local state
codex-notifier forget [--yes]      erase this computer and its pairings from the server, then uninstall
```

Settings: `notify_on_done`, `notify_on_permission`, `notify_on_question`, `quiet_hours`,
`quiet_when_present` (default on), `hold_ceiling_seconds` (30–3600, default 600),
`presence_threshold_seconds` (default 30), `stop_hold_seconds` (0–600, default 120), `sound`, and
the local-only `desktop_notifications` and `credential_store` (`keychain` or `file`).

## Files

- `~/.config/codex-notifier/` (`%APPDATA%\codex-notifier\` on Windows, or `$CODEX_NOTIFIER_HOME`):
  `config.toml`, `sessions/`, `codex-notifier.log`
- The computer's token: the macOS keychain or Windows Credential Manager (item
  `com.touchtap.codexnotifier`), or `credentials` in the folder above (mode 0600) on Linux, or
  wherever `credential_store` says.
- `$CODEX_HOME/hooks.json` and `$CODEX_HOME/config.toml`: our hook entries and their trust hashes.

## Setup analytics

`codex-notifier pair` sends a few anonymous events (pairing started, paired or failed with a short
reason; how it was installed; the OS and processor; `agent: codex`). The hooks send none, and
nothing about your sessions, prompts or computer name is included. Set
`CODEX_NOTIFIER_NO_ANALYTICS=1` or `DO_NOT_TRACK=1` to turn them off.

## Development

```sh
cargo build && cargo test
# end to end against the server worktree running `wrangler dev --local --port 8799`
scripts/e2e-local.sh
# one hook by hand
echo '{"hook_event_name":"Stop","session_id":"s1","cwd":"/tmp","last_assistant_message":"done"}' \
  | CODEX_NOTIFIER_PRESENCE=away ./target/debug/codex-notifier hook
```

`CODEX_NOTIFIER_SERVER_URL`, `CODEX_NOTIFIER_HOME`, `CODEX_HOME` (Codex's own folder, which the
hooks go in), `CODEX_NOTIFIER_PRESENCE=away|present|file:<path>`, `CODEX_NOTIFIER_NOTIFY=off` and
`CODEX_NOTIFIER_CREDENTIAL_STORE=file` are the test knobs. To try it against a real Codex without
touching your own `~/.codex`, point `CODEX_HOME` at an empty folder, run `codex login` there,
then `codex-notifier pair` with the same `CODEX_HOME`.

Started from [ccn](https://github.com/touch-tap/ccn), the Claude Code engine, on 30 Oct 2026; the
two share the server and the phone apps and nothing else. Server:
[CCNotifier_Server](../CCNotifier_Server) (Cloudflare Worker). Research and plan: the
"Codex Notifier: research and plan" doc.

## Licence

codex-notifier is source-available under the [PolyForm Shield License 1.0.0](LICENSE.md). You can
use it, change it and share it, including at work, as long as you keep the copyright notice and
don't use it to build a product that competes with Codex Notifier.

## npm

`npm/codex-notifier` is `@touchtap/codex-notifier` (command `codex-notifier`), which pulls in the
binary for the platform from `@touchtap/codex-notifier-<os>-<cpu>`. `npm/codexnotifier` is the
unscoped `codexnotifier` alias: it depends on the real package and only forwards to it, for people
who type the name that way. `scripts/npm-packages.sh <version> <dist>` builds all eight from a
release's archives (`codex-notifier-<target>.tar.gz`/`.zip`) and `scripts/npm-publish.sh` publishes
them in order. Publishing under `@touchtap` needs an owner of the npm org. There is no release
workflow yet: port ccn's `.github/workflows/release.yml` and `scripts/release.sh` with the names changed.
