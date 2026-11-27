# antigravity-notifier — Antigravity Notifier in the terminal

Get a push on your phone when Google Antigravity finishes or needs you, and approve, deny or
answer from there. Works on Mac, Windows and Linux, in any terminal, with no app running in the
background. Not affiliated with Google.

`antigravity-notifier` is one small binary that Antigravity (`agy`) calls through its lifecycle
hooks. It runs for a few milliseconds when something happens and exits. Install it once with
`antigravity-notifier pair`; after that every Antigravity session is covered, however you start it.

**Status: engine built 26 Nov 2026, not released.** Forked from
[codex-notifier](https://github.com/touch-tap/codex-notifier) 0.1.6 (itself a fork of ccn). The
hook layer is written against what agy 1.5.18 was measured to do and has run against the real
CLI with a stand-in server (`tests/fixtures/` holds its payloads). It has not yet paired with the
real server, which does not know this client yet.

From codex-notifier and ccn it carries: 17 languages (`locales/`), desktop notifications before
any phone is paired (local mode), `upgrade`, holding only when presence can be detected, atomic
config writes and install-source tracking. Purchases are made in the phone apps; nothing is sold
from the terminal.

### Before a release

Every address and identity the binary uses is in `src/brand.rs`.

- **Server.** The deployed server accepts `clientKind: "antigravity_cli"` and `X-App: antigravity`
  (since 26 Nov 2026), and answers `/api/cli/latest` per app.
- **API host.** The default server is `https://api.antigravitynotifier.com`, the shared Worker
  under this product's domain (mainland China can't reach `*.workers.dev`). A config that saved
  the workers.dev address reads as the default.
- **Website.** `antigravitynotifier.com` is live, including `/cli`, `/get`,
  `install.sh` and `install.ps1` (byte-for-byte the copies in `install/`).
- **Release repository.** `touch-tap/antigravity-notifier` is not created yet, so `upgrade` finds
  nothing; nor are the Homebrew formula, the Scoop manifest or the npm packages
  (`@touchtap/antigravity-notifier`). `scripts/first-release-checklist.md` creates them.
- **Update signing key.** Antigravity Notifier's own key (26 Nov 2026) is in `RELEASE_KEYS`; the
  private half is in the release Mac's keychain (RELEASING.md). Back it up.
- **Messages in Antigravity.** The Claude Code and Codex engines show one-line notes in the
  agent (`systemMessage`: "paired with…", an update notice). Antigravity
  accepts that output from SessionStart but was not seen to show it or pass it to the model, so
  for now those notes reach the user only where they are also desktop notifications.
- **Cross-promotion.** Someone who also has Claude Code and not ccn is told about Claude Code
  Notifier; someone with Codex and not codex-notifier about Notifier for Codex (Claude Code
  Notifier first when both qualify; two SessionStart mentions in all). Links carry
  `ref=antigravity-cli`, `-pair` and `-notification`.
- **Not measured:** Antigravity's desktop app and IDE (they share the hooks file), Windows and
  Linux, and holds longer than 600 s.

## Install

```sh
curl -fsSL https://antigravitynotifier.com/install.sh | sh          # macOS, Linux
irm https://antigravitynotifier.com/install.ps1 | iex; antigravity-notifier pair   # Windows
brew install touch-tap/tap/antigravity-notifier
scoop bucket add touch-tap https://github.com/touch-tap/scoop-bucket; scoop install antigravity-notifier
npm install -g @touchtap/antigravity-notifier
```

From source:

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
| "Antigravity has finished", with its last message, the moment a turn ends | the `Stop` hook reads the message from the conversation's `transcript_full.jsonl` |
| Approve or deny a command while you are away (file changes too, if you ask) | the `PreToolUse` hook holds the call and answers `auto_approve` or `deny` |
| Answer a question Antigravity asked | `PreToolUse` on `ask_question` denies the call with your answers as the reason, which the model reads and continues from |
| Send a short follow-up right after Antigravity finishes | `Stop` answers `continue` with it |
| A lock-screen card while Antigravity works (current step, files, lines) | `write_file` and `edit_file` calls seen by `PreToolUse`, counted once `PostToolUse` says they ran |

If you are at the keyboard (any input in the last 30 s) nothing waits and nothing is sent about
tool calls: Antigravity asks you itself when it wants to. While you are at the terminal
Antigravity runs in, the phone gets nothing at all (on macOS that means the terminal is also the
frontmost app; elsewhere, keyboard activity alone); if you are active in another app it gets the
done card and questions as notes. If you are away, the hook holds for the phone's answer, up to
a ceiling you choose (600 s at most for a tool call), and releases the moment you touch the
keyboard.

On macOS, clicking a desktop notification brings the app the session runs in (Terminal, iTerm2,
Ghostty, VS Code, Cursor and so on) to the front, when it was started from the Dock, Finder or
Spotlight and is still running. Over ssh there is nothing to bring forward, and AppleScript
notifications (before `antigravity-notifier setup` has asked for permission) do nothing on a click.

## How it hooks into Antigravity

`antigravity-notifier setup` (which `pair` runs) adds one named group, `antigravity-notifier`, to
Antigravity's global hooks file, `~/.gemini/config/hooks.json`, which the `agy` CLI, the desktop
app and the IDE all read:

```json
{
  "antigravity-notifier": {
    "SessionStart":  [ { "type": "command", "command": "/path/antigravity-notifier hook SessionStart", "timeout": 30 } ],
    "PreInvocation": [ { "type": "command", "command": "/path/antigravity-notifier hook PreInvocation", "timeout": 10 } ],
    "PreToolUse":    [ { "matcher": "*", "hooks": [ { "type": "command", "command": "/path/antigravity-notifier hook PreToolUse", "timeout": 900 } ] } ],
    "PostToolUse":   [ { "matcher": "*", "hooks": [ { "type": "command", "command": "/path/antigravity-notifier hook PostToolUse", "timeout": 10 } ] } ],
    "Stop":          [ { "type": "command", "command": "/path/antigravity-notifier hook Stop", "timeout": 700 } ]
  }
}
```

Every other group and key in the file is left as it was, and a one-time backup is kept beside
it. A file that is not valid JSON is refused, not overwritten. There is no trust step: the hooks
run from the next session. `uninstall` removes the group and nothing else.

What the hooks rely on, all measured on agy 1.5.18 (26 Nov 2026) and differing from Google's
docs in places:

- **Antigravity is fail-closed.** A `PreToolUse` hook that exits non-zero, times out or prints
  no decision (`{}`) blocks the tool call. So the hook always exits 0 and always prints a
  decision, `{"decision":"allow"}` unless it has a reason for another: not paired, no network,
  input it can't read, an unknown tool and a panic all end in `allow`, which leaves the call to
  Antigravity's own permission policy as if nothing were installed. A watchdog answers if the
  hook has not (`src/hook.rs`, `tests/hook.rs`).
- **Approvals live in `PreToolUse`.** There is no permission event. While the user is away, a
  `run_command` waits for the phone: approve is `auto_approve` ("Allow for session" then lets that tool through for the
  rest of the conversation), deny is `deny` with the note from the phone as the reason, and no
  answer is `force_ask`, which shows Antigravity's own prompt with "No answer from your phone"
  on it. Read-only tools never wait. While a hook holds, Antigravity shows no prompt of its own,
  which is why nothing is held when the user is present.
  `antigravity-notifier config set phone_approvals all` also holds file writes and edits and
  other tools that change something (Antigravity's Auto mode makes those without asking, so by
  default they don't wait); `off` turns approvals off.
- **Presence.** "At the keyboard" is input within the threshold (30 s by default) on a screen
  that is not locked: a locked screen is away at once (macOS `IOConsoleLocked`, Linux logind's
  `LockedHint`; Windows and a display that merely sleeps are not detected, and "can't tell" is
  never treated as away). A hold ends early only on real input made after it began.
- **Walking away from a prompt.** With the user present the hook answers `allow`, and
  Antigravity may then show its own approval prompt; if the user leaves, nothing would tell the
  phone, and Antigravity has no event for "waiting for approval". So for a call that would have
  waited for the phone, the hook starts a detached watcher (`src/walkaway.rs`). It ends
  silently when the call's `PostToolUse` arrives, the model is invoked again, or the turn
  stops, and after 15 minutes at most. If the user goes away while the call is still open, it
  waits 20 s and sends one note: "Antigravity may be waiting for you: <command> hasn't
  finished. If it needs approval, that's at your computer." It has no buttons, since the hook
  has already answered. `antigravity-notifier config set walkaway_alerts off` turns it off.
- **Print mode is never held.** In `agy -p` nobody can answer a local prompt, and `force_ask` is
  answered with a denial, so a script would stall and then fail. The hook finds `agy -p` among
  its parent processes and sends cards as notes instead (`ANTIGRAVITY_NOTIFIER_HEADLESS=0|1`
  overrides).
- **A prompt** is a `PreInvocation` with `invocationNum` 0 and a new `lastUserInput`.
- **Stop** carries `executionNum`, `fullyIdle` and `terminationReason`. `continuationActive`
  stays false after a `continue`, so the hook keeps its own record: one done card and at most
  one `continue` per execution. Nothing is announced while background tasks still run, or when
  the user stopped the turn; a turn that ended in an error says so.

## Commands

```
antigravity-notifier                     pair if this computer isn't paired yet, otherwise show its status
antigravity-notifier pair [CODE]         pair with a phone (installs the hooks); without a code, shows a QR code
antigravity-notifier doctor              check hooks, credentials, server, presence detection
antigravity-notifier test                send a test notification
antigravity-notifier devices             list paired phones; antigravity-notifier devices revoke <name>
antigravity-notifier config show         effective settings (also editable from the phone)
antigravity-notifier config set KEY VAL  e.g. hold_ceiling_seconds 300, quiet_hours 22:00-08:00, stop_hold_seconds 0
antigravity-notifier sync                pull settings from the server now
antigravity-notifier setup               reinstall the hooks (after moving the binary)
antigravity-notifier upgrade [--check]   update to the latest release (also checked once a day in the background)
antigravity-notifier uninstall           remove hooks, pairings, credentials and local state
antigravity-notifier forget [--yes]      erase this computer and its pairings from the server, then uninstall
```

Output is in the computer's language (`LC_ALL`, `LC_MESSAGES`, `LANG`, then the OS's preferred
languages), one of the 17 the phone apps speak; `ANTIGRAVITY_NOTIFIER_LANG=de` overrides it.

Settings: `notify_on_done`, `notify_on_permission`, `notify_on_question`, `quiet_hours`,
`quiet_when_present` (default on), `hold_ceiling_seconds` (30–3600, default 600),
`presence_threshold_seconds` (default 30), `stop_hold_seconds` (0–600, default 120), `sound`, and
the local-only `desktop_notifications`, `credential_store` (`keychain` or `file`), `auto_update`,
`pair_nudges`, `walkaway_alerts` (default on), `phone_approvals` (`commands` by default, `all` or `off`: which tool calls wait
for the phone), `hold_without_presence` (hold for the phone even where presence can't be read) and
`cross_promo` (default on: someone who also uses Claude Code, without
[ccn](https://claudecodenotifier.com) installed, hears about the Claude Code Notifier app and its
one-line install after every pairing, with a QR code for the app, then as one line in `status`
and `doctor`, and at most twice ever when Antigravity starts, as a desktop notification; `ANTIGRAVITY_NOTIFIER_NO_NUDGE=1` also turns it off), and `rate_prompt` (default on: once per install,
after 20 answers from the phone, a desktop notification asks for an app-store rating).

## Files

- `~/.config/antigravity-notifier/` (`%APPDATA%\antigravity-notifier\` on Windows, or `$ANTIGRAVITY_NOTIFIER_HOME`):
  `config.toml`, `sessions/`, `watch/`, `antigravity-notifier.log`
- The computer's token: the macOS keychain or Windows Credential Manager (item
  `com.touchtap.antigravitynotifier`), or `credentials` in the folder above (mode 0600) on Linux, or
  wherever `credential_store` says.
- `~/.gemini/config/hooks.json`: our hook group (and `hooks.json.antigravity-notifier-backup`,
  the file as it was before the first change).

## Setup analytics

`antigravity-notifier pair` and the other commands people type send a few anonymous events (pairing
started, paired or failed with a short reason; how and from where it was installed; the OS and
processor; `agent: antigravity`). The background updater sends `cli_updated` or `cli_update_failed`, and
the one hook that sends anything is SessionStart, when it shows the ccn suggestion or rating request (`cli_cross_promo_shown`, `cli_rate_prompt_shown`, sent by a detached process so nothing waits). Nothing about your sessions, prompts or computer name is included. Set
`ANTIGRAVITY_NOTIFIER_NO_ANALYTICS=1` or `DO_NOT_TRACK=1` to turn them off.

## Development

```sh
cargo build && cargo test
# end to end against the server worktree running `wrangler dev --local --port 8799`
scripts/e2e-local.sh
# one hook by hand
./target/debug/antigravity-notifier hook PreToolUse < tests/fixtures/pre_tool_use_run_command.json
```

`ANTIGRAVITY_NOTIFIER_SERVER_URL`, `ANTIGRAVITY_NOTIFIER_HOME`, `ANTIGRAVITY_NOTIFIER_GEMINI_HOME`
(used in place of `~/.gemini` for the hooks file), `ANTIGRAVITY_NOTIFIER_HEADLESS=0|1`, `ANTIGRAVITY_NOTIFIER_PRESENCE=away|present|unknown|file:<path>`,
`ANTIGRAVITY_NOTIFIER_NOTIFY=off`, `ANTIGRAVITY_NOTIFIER_CREDENTIAL_STORE=file`, `ANTIGRAVITY_NOTIFIER_LANG`,
`ANTIGRAVITY_NOTIFIER_NO_NUDGE=1`, `ANTIGRAVITY_NOTIFIER_NO_AUTO_UPDATE=1` and
`ANTIGRAVITY_NOTIFIER_DOWNLOAD_BASE` (debug builds also trust `ANTIGRAVITY_NOTIFIER_UPDATE_PUBLIC_KEY`) are the
test knobs. To try it against a real Antigravity without touching the global hooks file, put
the group above in a project's `.agents/hooks.json` (project hooks load in `agy -p` too, and run
alongside global ones) with the debug binary's path.

Forked on 26 Nov 2026 from codex-notifier 0.1.6, which started from
[ccn](https://github.com/touch-tap/ccn), the Claude Code engine; the three share the server and
the phone apps and nothing else. Server: [CCNotifier_Server](../CCNotifier_Server) (Cloudflare
Worker).

## Licence

antigravity-notifier is source-available under the [PolyForm Shield License 1.0.0](LICENSE.md). You can
use it, change it and share it, including at work, as long as you keep the copyright notice and
don't use it to build a product that competes with Antigravity Notifier.

## npm

`npm/antigravity-notifier` is `@touchtap/antigravity-notifier` (command `antigravity-notifier`), which pulls in the
binary for the platform from `@touchtap/antigravity-notifier-<os>-<cpu>`. There is no unscoped
alias. Nothing is published yet.
`scripts/npm-packages.sh <version> <dist>` builds all seven from a
release's archives (`antigravity-notifier-<target>.tar.gz`/`.zip`) and `scripts/npm-publish.sh` publishes
them in order. Publishing under `@touchtap` needs an owner of the npm org. See RELEASING.md.
