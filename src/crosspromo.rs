//! The one place antigravity-notifier mentions its sister app: Claude Code Notifier (its computer side is
//! ccn), which does the same for Claude Code. Shown only to someone who has Claude Code and not ccn
//! already, and only where the user is looking at antigravity-notifier anyway: the end of every
//! `antigravity-notifier pair` (with a QR code for the app: the phone is in their hand), a hint in
//! `status` and `doctor`, and at most twice ever in Antigravity at SessionStart. Never a desktop
//! notification. ccn's own copy of this module suggests antigravity-notifier the same way.
//!
//! Detection is a few `stat` calls and at most one small file read: no network.

use rust_i18n::t;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::config::{self, Config};
use crate::qr;

const DAY: i64 = 24 * 60 * 60;
/// The first SessionStart mention waits this long after pairing
const FIRST_AFTER: i64 = 3 * DAY;
/// The second waits this long after the first
const SECOND_AFTER: i64 = 30 * DAY;
/// SessionStart mentions it no more often than this, ever
const SESSION_MAX: u32 = 2;

/// One line installs ccn, sets up Claude Code's hooks and pairs: the script ends in `ccn pair`,
/// whose QR code also offers the app to a phone without it. No Node needed, unlike npm.
/// The addresses themselves are in brand.rs. /get sends a phone straight to the App Store or
/// Google Play (and shows a computer both store buttons).
use crate::brand::claude_code::{UNIX_COMMAND, WINDOWS_COMMAND};
pub use crate::brand::claude_code::{NOTIFICATION_URL, PAIR_URL, URL};

/// Where Claude Code and ccn would live on this computer.
#[derive(Debug, Clone)]
pub struct Places {
    /// Claude Code's config folder: `$CLAUDE_CONFIG_DIR`, else `~/.claude`
    pub claude_dir: PathBuf,
    /// ccn's own folder, found the way ccn finds it (and antigravity-notifier finds its own)
    pub ccn_dir: PathBuf,
    /// `$PATH`
    pub path: Option<OsString>,
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

impl Places {
    pub fn from_env() -> Places {
        let home = config::home_dir();
        let claude_dir = env_dir("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude"));
        Places {
            claude_dir,
            ccn_dir: ccn_dir(&home),
            path: std::env::var_os("PATH"),
        }
    }
}

/// ccn's folder: `$CCN_HOME`, else `%APPDATA%\ccn` on Windows, else `$XDG_CONFIG_HOME/ccn`, else
/// `~/.config/ccn`. The same order as `config::app_dir`.
fn ccn_dir(home: &Path) -> PathBuf {
    if let Some(dir) = env_dir("CCN_HOME") {
        return dir;
    }
    #[cfg(windows)]
    {
        if let Some(appdata) = env_dir("APPDATA") {
            return appdata.join("ccn");
        }
    }
    if let Some(xdg) = env_dir("XDG_CONFIG_HOME") {
        return xdg.join("ccn");
    }
    home.join(".config").join("ccn")
}

/// An executable called `name` in one of `path`'s folders.
fn on_path(name: &str, path: Option<&OsString>) -> bool {
    let Some(path) = path else {
        return false;
    };
    let names: Vec<String> = if cfg!(windows) {
        // npm puts a .cmd (and a .ps1) shim on Windows
        ["", ".exe", ".cmd", ".bat", ".ps1"]
            .iter()
            .map(|ext| format!("{name}{ext}"))
            .collect()
    } else {
        vec![name.to_string()]
    };
    std::env::split_paths(path).any(|dir| names.iter().any(|n| dir.join(n).is_file()))
}

/// Claude Code's settings.json has a hook that runs ccn (`ccn hook`, `/path/to/ccn hook`,
/// `npx @touchtap/ccn hook`, `C:\...\ccn.exe hook`).
fn hooks_run_ccn(settings: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(settings) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    let mut commands = Vec::new();
    collect_commands(&json["hooks"], &mut commands);
    commands.iter().any(|command| {
        command.split_whitespace().any(|word| {
            let word = word.trim_matches(|c| c == '"' || c == '\'');
            let file = word.rsplit(['/', '\\']).next().unwrap_or(word);
            file.eq_ignore_ascii_case("ccn") || file.eq_ignore_ascii_case("ccn.exe")
        })
    })
}

fn collect_commands<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::Object(map) => {
            for (key, v) in map {
                match (key.as_str(), v) {
                    ("command", Value::String(s)) => out.push(s),
                    _ => collect_commands(v, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect_commands(v, out)),
        _ => {}
    }
}

/// Claude Code is on this computer: its config folder exists, or `claude` is on the PATH.
pub fn claude_present(places: &Places) -> bool {
    places.claude_dir.exists() || on_path("claude", places.path.as_ref())
}

/// ccn is already here: its folder exists, `ccn` is on the PATH, or Claude Code's hooks run it.
pub fn ccn_present(places: &Places) -> bool {
    places.ccn_dir.exists()
        || on_path("ccn", places.path.as_ref())
        || hooks_run_ccn(&places.claude_dir.join("settings.json"))
}

/// Someone with Claude Code who doesn't have ccn yet.
pub fn worth_suggesting(places: &Places) -> bool {
    claude_present(places) && !ccn_present(places)
}

/// The setting is on and `ANTIGRAVITY_NOTIFIER_NO_NUDGE` isn't set.
pub fn enabled(cfg: &Config) -> bool {
    cfg.cross_promo && !crate::local::no_nudge_env()
}

/// Whether to show the line anywhere other than SessionStart: enabled, and worth suggesting here.
pub fn eligible(cfg: &Config) -> bool {
    enabled(cfg) && worth_suggesting(&Places::from_env())
}

/// The one line that sets ccn up on this computer
pub fn command() -> &'static str {
    if cfg!(windows) {
        WINDOWS_COMMAND
    } else {
        UNIX_COMMAND
    }
}

/// The line itself, the same everywhere it is shown.
pub fn line() -> String {
    t!("cross_promo.line", command = command(), url = URL).into_owned()
}

/// The SessionStart desktop notification's body (its title is the app's name).
pub fn notification_body() -> String {
    t!("cross_promo.notification").into_owned()
}

/// Counts a showing; `place` is pair, session, status or doctor.
fn shown(place: &str) {
    crate::telemetry::event_detached("cli_cross_promo_shown", &[("where", place)]);
}

/// Prints the line (after a blank one) when eligible. For `status` and `doctor`.
pub fn print_hint(cfg: &Config, place: &str) {
    if eligible(cfg) {
        println!();
        println!("{}", line());
        shown(place);
    }
}

/// After every successful `antigravity-notifier pair`, when eligible: the phone is in their hand, so a
/// QR code for the app, its link, and the one command for this computer.
pub fn print_after_pair(cfg: &Config) {
    if !eligible(cfg) {
        return;
    }
    let code = qr::render(PAIR_URL);
    println!();
    println!("{}", t!("cross_promo.pair_title"));
    match &code {
        Some(code) => {
            println!("{}", t!("cross_promo.pair_scan"));
            println!();
            print!("{code}");
        }
        None => println!("{}", t!("cross_promo.pair_steps")),
    }
    println!("  {PAIR_URL}");
    println!("  {}", command());
    let qr = if code.is_some() { "yes" } else { "no" };
    crate::telemetry::event_detached("cli_cross_promo_shown", &[("where", "pair"), ("qr", qr)]);
}

/// Whether SessionStart may show the line: the first time no earlier than 3 days after `since`
/// (pairing), the second 30 days after the first, then never.
pub fn session_due(now: i64, since: i64, last_at: Option<i64>, count: u32) -> bool {
    match count {
        0 => now - since >= FIRST_AFTER,
        c if c < SESSION_MAX => last_at.is_none_or(|last| now - last >= SECOND_AFTER),
        _ => false,
    }
}

/// The `systemMessage` for a paired SessionStart that shows nothing else, when due and eligible,
/// with a desktop notification as well (most people never read Antigravity's start-up lines) unless
/// desktop notifications are off. Neither goes through `antigravity_wording`.
pub fn session_message(cfg: &mut Config, now: i64) -> Option<Value> {
    if !enabled(cfg) {
        return None;
    }
    let first_seen = cfg.cross_promo_first_seen_at;
    let text = session_line(cfg, now, &Places::from_env());
    if text.is_some() || cfg.cross_promo_first_seen_at != first_seen {
        let _ = cfg.save();
    }
    let text = text?;
    if cfg.desktop_notifications {
        crate::notify::show_link(
            crate::notify::APP_NAME,
            &notification_body(),
            NOTIFICATION_URL,
        );
    }
    crate::logger::log(&format!(
        "ccn suggestion shown at SessionStart (desktop notification: {})",
        cfg.desktop_notifications
    ));
    shown("session");
    Some(json!({ "systemMessage": text }))
}

/// The cadence and detection behind `session_message`, without saving: the line when it is due
/// here, with the showing counted in `cfg`.
pub fn session_line(cfg: &mut Config, now: i64, places: &Places) -> Option<String> {
    // Never the same day as the rating request
    if cfg.cross_promo_count >= SESSION_MAX
        || cfg.rate_prompt_shown_at.is_some_and(|at| now - at < DAY)
    {
        return None;
    }
    let since = cfg
        .paired_at
        .or(cfg.registered_at)
        .unwrap_or_else(|| *cfg.cross_promo_first_seen_at.get_or_insert(now));
    if !session_due(now, since, cfg.cross_promo_last_at, cfg.cross_promo_count)
        || !worth_suggesting(places)
    {
        return None;
    }
    cfg.cross_promo_count += 1;
    cfg.cross_promo_last_at = Some(now);
    Some(line())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "antigravity-notifier-crosspromo-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn places(root: &Path) -> Places {
        Places {
            claude_dir: root.join(".claude"),
            ccn_dir: root.join(".config").join("ccn"),
            path: Some(std::env::join_paths([root.join("bin")]).unwrap()),
        }
    }

    fn exe(root: &Path, name: &str) {
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let file = if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_string()
        };
        fs::write(bin.join(file), "").unwrap();
    }

    #[test]
    fn nothing_installed_suggests_nothing() {
        let root = temp("none");
        let p = places(&root);
        assert!(!claude_present(&p));
        assert!(!ccn_present(&p));
        assert!(!worth_suggesting(&p));
        let none = Places {
            path: None,
            ..p.clone()
        };
        assert!(!worth_suggesting(&none));
    }

    #[test]
    fn claude_code_without_ccn_is_worth_suggesting() {
        let root = temp("claude-dir");
        fs::create_dir_all(root.join(".claude")).unwrap();
        assert!(worth_suggesting(&places(&root)));

        let root = temp("claude-bin");
        exe(&root, "claude");
        assert!(claude_present(&places(&root)));
        assert!(worth_suggesting(&places(&root)));
    }

    #[test]
    fn ccn_already_there_suggests_nothing() {
        // ccn's folder
        let root = temp("ccn-dir");
        fs::create_dir_all(root.join(".claude")).unwrap();
        fs::create_dir_all(root.join(".config").join("ccn")).unwrap();
        assert!(ccn_present(&places(&root)));
        assert!(!worth_suggesting(&places(&root)));

        // ccn on the PATH
        let root = temp("ccn-bin");
        fs::create_dir_all(root.join(".claude")).unwrap();
        exe(&root, "ccn");
        assert!(!worth_suggesting(&places(&root)));

        // Claude Code's hooks run ccn
        for command in [
            "ccn hook",
            "/usr/local/bin/ccn hook",
            "\"C:\\Users\\me\\AppData\\Local\\ccn\\ccn.exe\" hook",
            "npx -y @touchtap/ccn hook",
        ] {
            let root = temp("ccn-hooks");
            fs::create_dir_all(root.join(".claude")).unwrap();
            let settings = json!({ "hooks": { "Stop": [ { "hooks": [
                { "type": "command", "command": command }
            ] } ] } });
            fs::write(
                root.join(".claude").join("settings.json"),
                settings.to_string(),
            )
            .unwrap();
            assert!(!worth_suggesting(&places(&root)), "{command}");
        }
    }

    #[test]
    fn other_hooks_and_bad_settings_do_not_count_as_ccn() {
        let root = temp("other-hooks");
        fs::create_dir_all(root.join(".claude")).unwrap();
        let settings = json!({
            "env": { "command": "ccn" },
            "hooks": { "Stop": [ { "hooks": [
                { "type": "command", "command": "/opt/ccnotes/bin/ccnx --hook" }
            ] } ] }
        });
        let file = root.join(".claude").join("settings.json");
        fs::write(&file, settings.to_string()).unwrap();
        assert!(worth_suggesting(&places(&root)));
        fs::write(&file, "{ not json").unwrap();
        assert!(worth_suggesting(&places(&root)));
    }

    #[test]
    fn session_shows_it_after_three_days_then_thirty_then_never() {
        let paired = 1_700_000_000;
        let at = |days: i64| paired + days * DAY;
        assert!(!session_due(at(0), paired, None, 0));
        assert!(!session_due(at(2), paired, None, 0));
        assert!(session_due(at(3), paired, None, 0));
        assert!(session_due(at(40), paired, None, 0));
        // Second: 30 days after the first, not after pairing
        assert!(!session_due(at(32), paired, Some(at(3)), 1));
        assert!(session_due(at(33), paired, Some(at(3)), 1));
        // Never a third
        assert!(!session_due(at(400), paired, Some(at(33)), 2));
        assert!(!session_due(at(400), paired, Some(at(33)), 9));
    }

    #[test]
    fn session_counts_showings_and_stops_at_two() {
        let root = temp("session");
        fs::create_dir_all(root.join(".claude")).unwrap();
        let p = places(&root);
        let paired = 1_700_000_000;
        let mut cfg = Config::default();
        cfg.paired_at = Some(paired);
        assert_eq!(session_line(&mut cfg, paired + DAY, &p), None);
        assert_eq!(session_line(&mut cfg, paired + 3 * DAY, &p), Some(line()));
        assert_eq!(cfg.cross_promo_count, 1);
        assert_eq!(cfg.cross_promo_last_at, Some(paired + 3 * DAY));
        assert_eq!(session_line(&mut cfg, paired + 4 * DAY, &p), None);
        assert_eq!(session_line(&mut cfg, paired + 33 * DAY, &p), Some(line()));
        assert_eq!(session_line(&mut cfg, paired + 400 * DAY, &p), None);
        assert_eq!(cfg.cross_promo_count, 2);
        assert_eq!(cfg.cross_promo_first_seen_at, None);

        // No pairing time: counted from the first SessionStart that looks
        let mut cfg = Config::default();
        assert_eq!(session_line(&mut cfg, paired, &p), None);
        assert_eq!(cfg.cross_promo_first_seen_at, Some(paired));
        assert_eq!(session_line(&mut cfg, paired + 2 * DAY, &p), None);
        assert_eq!(session_line(&mut cfg, paired + 3 * DAY, &p), Some(line()));

        // Not worth suggesting: nothing shown, nothing counted
        let bare = temp("session-bare");
        let mut cfg = Config::default();
        cfg.paired_at = Some(paired);
        assert_eq!(
            session_line(&mut cfg, paired + 9 * DAY, &places(&bare)),
            None
        );
        assert_eq!(cfg.cross_promo_count, 0);
    }

    #[test]
    fn the_line_keeps_its_command_url_and_claude_code() {
        let line = line();
        assert_eq!(
            line,
            format!(
                "Claude Code detected — get Claude Code approvals and questions on your phone too, with the Claude Code Notifier app (iPhone and Android). Set up this computer with one command: {}  ·  https://claudecodenotifier.com/?ref=antigravity-cli",
                command()
            )
        );
        assert!(command().contains("claudecodenotifier.com/install."));
        assert!(!command().contains("npm"));
        assert_eq!(PAIR_URL, "https://claudecodenotifier.com/get?ref=antigravity-pair");
        assert!(qr::render(PAIR_URL).is_some());
    }

    #[test]
    fn every_translation_names_claude_code_and_no_one_platform() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        for lang in config::LOCALES {
            let text = rust_i18n::t!(
                "cross_promo.line",
                locale = lang,
                command = command(),
                url = URL
            );
            assert!(
                text.contains(command()) && text.contains(URL),
                "{lang}: {text}"
            );
            let naming_the_app = [
                text.to_string(),
                rust_i18n::t!("cross_promo.notification", locale = lang).into_owned(),
                rust_i18n::t!("cross_promo.pair_title", locale = lang).into_owned(),
            ];
            for text in &naming_the_app {
                assert!(text.contains("Claude Code Notifier"), "{lang}: {text}");
            }
            let steps = [
                rust_i18n::t!("cross_promo.pair_scan", locale = lang).into_owned(),
                rust_i18n::t!("cross_promo.pair_steps", locale = lang).into_owned(),
            ];
            for text in naming_the_app.iter().chain(&steps) {
                for mac_only in ["Mac", "macOS"] {
                    assert!(!text.contains(mac_only), "{lang}: {text}");
                }
            }
            assert!(dir.join(format!("{lang}.yml")).exists());
        }
    }
}
