# Changelog

## 0.1.1

- **Windows: a tool call allowed at the keyboard could hang for up to 15 minutes.** On a paired
  PC the hook answered at once but the walk-away watcher it starts kept the hook's output open,
  so Antigravity went on waiting for the hook until the watcher gave up. The background update
  check and the anonymous setup events were started the same way and could hold a hook for as
  long as they ran. All three now start with none of the hook's handles. macOS and Linux were
  not affected. If you installed 0.1.0 on Windows, run `antigravity-notifier upgrade`.
- A Scoop install is recognised: `upgrade` says `scoop update antigravity-notifier` instead of
  replacing Scoop's copy, and `uninstall` says `scoop uninstall antigravity-notifier`.
- No web licences for Antigravity Notifier: `unlock`, `activate`, `deactivate` and `licence` are
  no longer listed in `--help`, and nothing mentions them after pairing, in `status`, `doctor`,
  `forget` or `uninstall`, or at session start. Purchases are made in the phone apps.
- The install scripts send their anonymous install event to `api.antigravitynotifier.com`, as the
  binary does (they used a `workers.dev` address that mainland China cannot reach).
- README brought up to date. The phone apps are still in App Store and Google Play review.

## 0.1.0 (first public release)

Antigravity Notifier in the terminal: phone notifications and approvals for Google Antigravity's
`agy` CLI, with the Antigravity Notifier app for iPhone and Android. Forked from codex-notifier
0.1.6.

- `antigravity-notifier` pairs this computer with the phone (a QR code, or `pair <code>` with the
  code the app shows under Agents > Link a computer) and installs a named `antigravity-notifier`
  hook group in Antigravity's `hooks.json`, leaving every other group as it was.
- Approve or deny tool calls from the phone (`phone_approvals`: `commands` by default, `all` or
  `off`), with a note on a deny, or allow the rest of the session. Questions (single choice,
  multiple choice, free text) are answered from the phone, and the done card can send a reply
  that starts Antigravity's next turn.
- The engine never answers for you: a call the phone could not be asked about, or a phone that
  cannot be reached, goes back to the prompt in the terminal.
- Only holds while you are away: at the keyboard nothing is sent; a locked screen counts as away
  (macOS, Linux logind). If a call you allowed at the keyboard is still open after you walk away,
  the phone gets one note saying so (`walkaway_alerts`).
- Desktop notifications on macOS, Windows and Linux; on macOS a click brings the terminal or IDE
  the session runs in to the front.
- Signed self-update (`antigravity-notifier upgrade`, plus a daily background check), verified
  against Antigravity Notifier's own release key.
- Talks to `api.antigravitynotifier.com`.
- Suggests Claude Code Notifier to someone who also uses Claude Code, or Notifier for Codex to
  someone who uses OpenAI's Codex CLI, at most twice at session start (`cross_promo`).
- 17 languages.

Known limits: only the `agy` CLI is supported (not Antigravity's desktop app or IDE); print mode
(`agy -p`) is never held; Windows and Linux builds have not yet been run by hand.
