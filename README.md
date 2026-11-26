# antigravity-notifier — Antigravity Notifier in the terminal

Get a push on your phone when OpenAI Codex finishes or needs you, and approve, deny or answer
from there. Works on macOS, Linux and Windows, in any terminal, with no app running in the
background. Not affiliated with OpenAI.

`antigravity-notifier` is one small binary that Codex calls through its lifecycle hooks. It runs for a
few milliseconds when something happens and exits. Install it once with `antigravity-notifier pair`;
after that every `codex` session is covered, however you start it.

**Status: proof of concept (30 Oct 2026), rebased on ccn 0.4.1 (4 Nov 2026).** It pairs with the
existing Claude Code Notifier phone app against a local server, and the real Codex CLI 0.166 runs
its hooks from the trust hashes the installer writes. No release, website, store listing or brand
exists yet. See `scripts/e2e-local.sh` for what is verified.

From ccn it carries: 17 languages (`locales/`), desktop notifications before any phone is paired
(local mode), `upgrade` with a daily background check, web licences (`unlock`, `activate`,
`deactivate`, `licence`), holding only when presence can be detected, atomic config writes and
install-source tracking.

### Placeholders to settle before a release

- **Release repository.** `antigravity-notifier upgrade` downloads from `touch-tap/antigravity-notifier` GitHub
  releases, which **doesn't exist yet**, so the update check finds nothing. The release pipeline
  is ready for it (RELEASING.md); `scripts/first-release-checklist.md` is the first release, step by step.
- **Update signing key.** Antigravity Notifier's own key (made 8 Nov 2026, public `eeb6e55b…`) is in
  `RELEASE_KEYS`; the private half is in the release Mac's login keychain as
  `antigravitynotifier-release-signing`. ccn's key is deliberately not trusted.
- **API host.** The default server is `ccnotifier-api.touchtap.workers.dev`, a custom domain on the shared Worker
  (live since 8 Nov 2026; also reachable from mainland China, unlike `*.workers.dev`).
- **Server copy.** The server's `/api/cli/latest` and licence nudge copy are still ccn's; the
  engine ignores ccn releases and rewrites the nudge's `ccn` command and "Claude" until the server
  answers per app.
- **Packaging.** Homebrew formula, Scoop and winget manifests are generated from each release
  (RELEASING.md). `install/` holds byte-for-byte copies of the website's install scripts, which CI
  runs. ccn's Claude Code plugin was not carried over.

## Install (today: from source)

```sh
cargo build --release
./target/release/antigravity-notifier pair 482913     # the code the phone app shows
./target/release/antigravity-notifier                 # or show a QR code here
```

`antigravity-notifier` on its own pairs a computer that isn't paired yet. Once paired, the phone gets a
test notification it can answer, and `antigravity-notifier` shows the answer.

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

On macOS, clicking a desktop notification brings the app the session runs in (Terminal, iTerm2,
Ghostty, VS Code, Cursor and so on) to the front, when it was started from the Dock, Finder or
Spotlight and is still running. Over ssh there is nothing to bring forward, and AppleScript
notifications (before `antigravity-notifier setup` has asked for permission) do nothing on a click.

## How it hooks into Codex

`antigravity-notifier setup` (which `pair` runs) adds six entries to `$CODEX_HOME/hooks.json`
(`~/.codex/hooks.json`): `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`,
`Stop` and `SessionEnd`, each `antigravity-notifier hook`. Everything else in the file is left as it was
and a one-time backup is kept beside it.

Codex ignores a hook until it is trusted. Trust is a SHA-256 of the hook's normalised definition,
stored in `config.toml` as `[hooks.state."<hooks.json path>:<event>:<group>:<handler>"]
trusted_hash`. `setup` computes the same hash Codex does (`hook_hash` in
codex-rs/hooks/src/engine/discovery.rs: event name, matcher and the handler with its timeout, as
TOML, then JSON with sorted keys) and writes it, so the hooks run on the next session with no
visit to `/hooks`. `antigravity-notifier doctor` reports when a hash is missing or stale, and
`uninstall` removes exactly the entries it added.

Codex has no Notification event, so when you are at the keyboard the `PermissionRequest` hook
sends the informational card itself. `SessionEnd` hooks are capped at 3 s by Codex. Questions
marked secret (`isSecret`) never go to the phone.

## Commands

```
antigravity-notifier                     pair if this computer isn't paired yet, otherwise show its status
antigravity-notifier pair [CODE]         pair with a phone (installs the hooks); without a code, shows a QR code
antigravity-notifier doctor              check hooks, trust, credentials, server, presence detection
antigravity-notifier test                send a test notification
antigravity-notifier devices             list paired phones; antigravity-notifier devices revoke <name>
antigravity-notifier config show         effective settings (also editable from the phone)
antigravity-notifier config set KEY VAL  e.g. hold_ceiling_seconds 300, quiet_hours 22:00-08:00, stop_hold_seconds 0
antigravity-notifier sync                pull settings from the server now
antigravity-notifier setup               reinstall the hooks and their trust hashes (after moving the binary)
antigravity-notifier upgrade [--check]   update to the latest release (also checked once a day in the background)
antigravity-notifier unlock [--lifetime] buy a web licence for this computer; activate/deactivate/licence manage it
antigravity-notifier uninstall           remove hooks, trust entries, pairings, credentials and local state
antigravity-notifier forget [--yes]      erase this computer and its pairings from the server, then uninstall
```

Output is in the computer's language (`LC_ALL`, `LC_MESSAGES`, `LANG`, then the OS's preferred
languages), one of the 17 the phone apps speak; `ANTIGRAVITY_NOTIFIER_LANG=de` overrides it.

Settings: `notify_on_done`, `notify_on_permission`, `notify_on_question`, `quiet_hours`,
`quiet_when_present` (default on), `hold_ceiling_seconds` (30–3600, default 600),
`presence_threshold_seconds` (default 30), `stop_hold_seconds` (0–600, default 120), `sound`, and
the local-only `desktop_notifications`, `credential_store` (`keychain` or `file`), `auto_update`,
`pair_nudges`, `hold_without_presence` (hold for the phone even where presence can't be read) and
`cross_promo` (default on: someone who also uses Claude Code, without
[ccn](https://claudecodenotifier.com) installed, hears about the Claude Code Notifier app and its
one-line install after every pairing, with a QR code for the app, then as one line in `status`
and `doctor`, and at most twice ever when Codex starts, as a desktop notification and a line in Codex; `ANTIGRAVITY_NOTIFIER_NO_NUDGE=1` also turns it off), and `rate_prompt` (default on: once per install,
after 20 answers from the phone, a desktop notification asks for an app-store rating).

## Files

- `~/.config/antigravity-notifier/` (`%APPDATA%\antigravity-notifier\` on Windows, or `$ANTIGRAVITY_NOTIFIER_HOME`):
  `config.toml`, `sessions/`, `antigravity-notifier.log`
- The computer's token: the macOS keychain or Windows Credential Manager (item
  `com.touchtap.antigravitynotifier`), or `credentials` in the folder above (mode 0600) on Linux, or
  wherever `credential_store` says.
- `$CODEX_HOME/hooks.json` and `$CODEX_HOME/config.toml`: our hook entries and their trust hashes.

## Setup analytics

`antigravity-notifier pair` and the other commands people type send a few anonymous events (pairing
started, paired or failed with a short reason; how and from where it was installed; the OS and
processor; `agent: codex`). The background updater sends `cli_updated` or `cli_update_failed`, and
the one hook that sends anything is SessionStart, when it shows a licence nudge (at most once a
day) or the ccn suggestion or rating request (`cli_cross_promo_shown`, `cli_rate_prompt_shown`, sent by a detached process so nothing waits). Nothing about your sessions, prompts or computer name is included. Set
`ANTIGRAVITY_NOTIFIER_NO_ANALYTICS=1` or `DO_NOT_TRACK=1` to turn them off.

## Development

```sh
cargo build && cargo test
# end to end against the server worktree running `wrangler dev --local --port 8799`
scripts/e2e-local.sh
# one hook by hand
echo '{"hook_event_name":"Stop","session_id":"s1","cwd":"/tmp","last_assistant_message":"done"}' \
  | ANTIGRAVITY_NOTIFIER_PRESENCE=away ./target/debug/antigravity-notifier hook
```

`ANTIGRAVITY_NOTIFIER_SERVER_URL`, `ANTIGRAVITY_NOTIFIER_HOME`, `CODEX_HOME` (Codex's own folder, which the
hooks go in), `ANTIGRAVITY_NOTIFIER_PRESENCE=away|present|unknown|file:<path>`,
`ANTIGRAVITY_NOTIFIER_NOTIFY=off`, `ANTIGRAVITY_NOTIFIER_CREDENTIAL_STORE=file`, `ANTIGRAVITY_NOTIFIER_LANG`,
`ANTIGRAVITY_NOTIFIER_NO_NUDGE=1`, `ANTIGRAVITY_NOTIFIER_NO_AUTO_UPDATE=1` and
`ANTIGRAVITY_NOTIFIER_DOWNLOAD_BASE` (debug builds also trust `ANTIGRAVITY_NOTIFIER_UPDATE_PUBLIC_KEY`) are the
test knobs. To try it against a real Codex without
touching your own `~/.codex`, point `CODEX_HOME` at an empty folder, run `codex login` there,
then `antigravity-notifier pair` with the same `CODEX_HOME`.

Started from [ccn](https://github.com/touch-tap/ccn), the Claude Code engine, on 30 Oct 2026 and
rebased on its 0.4.1 (878038a) on 4 Nov 2026; the two share the server and the phone apps and
nothing else. Server:
[CCNotifier_Server](../CCNotifier_Server) (Cloudflare Worker). Research and plan: the
"Codex Notifier: research and plan" doc.

## Licence

antigravity-notifier is source-available under the [PolyForm Shield License 1.0.0](LICENSE.md). You can
use it, change it and share it, including at work, as long as you keep the copyright notice and
don't use it to build a product that competes with Antigravity Notifier.

## npm

`npm/antigravity-notifier` is `@touchtap/antigravity-notifier` (command `antigravity-notifier`), which pulls in the
binary for the platform from `@touchtap/antigravity-notifier-<os>-<cpu>`. There is no unscoped
alias: npm refused `antigravitynotifier` (17 Nov 2026) as too close to the unrelated `antigravity-notifier`.
`scripts/npm-packages.sh <version> <dist>` builds all seven from a
release's archives (`antigravity-notifier-<target>.tar.gz`/`.zip`) and `scripts/npm-publish.sh` publishes
them in order. Publishing under `@touchtap` needs an owner of the npm org. See RELEASING.md.
