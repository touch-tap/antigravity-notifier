//! The walk-away watcher. A hook only waits for the phone when the user is away at the moment a
//! tool call starts. If they are at the keyboard, the hook answers `allow`, Antigravity may then
//! show its own approval prompt, and nothing would reach the phone if the user walks off while
//! it sits there. Antigravity has no "waiting for approval" event, so the hook cannot know that a
//! prompt is showing: all it sees is that the call has not finished (measured on agy 1.5.18:
//! PostToolUse for `run_command` fires when the command finishes, not when it is approved).
//!
//! So when a tool call that would have waited for the phone is allowed because the user was
//! present, the hook leaves a note (`watch/<conversation>.json`) and starts this watcher as a
//! detached process. The watcher ends without a word as soon as the note is gone or replaced:
//! PostToolUse for that call, the model being invoked again (the call was answered one way or the
//! other), a Stop, or a newer watched call. If instead the user goes away (input idle past the
//! presence threshold, or the screen locked) while the note still stands, it waits a short grace
//! and sends ONE informational push that is honest about not knowing: the command has not
//! finished, and if it needs approval, that is at the computer. It cannot be approved from the
//! phone, because the hook has long since answered, so the card offers no buttons.
//!
//! The watcher can never delay or fail the hook: the hook prints its answer first, and every
//! error on the way to starting the watcher is swallowed (hook.rs, `start`).

use rust_i18n::t;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::api::Api;
use crate::config::{self, Config, Unpaired};
use crate::logger::log;
use crate::payload;
use crate::presence;
use crate::session::SessionState;

/// The watcher gives up after this long whatever happens
const MAX_WATCH: Duration = Duration::from_secs(15 * 60);
/// How long the user must have been away before the push goes out, so stepping away from the
/// keyboard for a moment (or reading the prompt) sends nothing
const GRACE: Duration = Duration::from_secs(20);
/// How often the watcher looks
const TICK: Duration = Duration::from_secs(1);
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// The note a hook leaves for the watcher: one per conversation, replaced by each newer call.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Watch {
    /// Made for this watch alone, so a watcher can tell its note from a newer one
    pub token: String,
    pub conversation: String,
    /// The tool call's id, which its PostToolUse repeats
    pub call_id: String,
    pub tool_name: String,
    /// What the call does, as the phone shows it ("cargo test")
    pub summary: String,
    pub project: Option<String>,
    pub cwd: String,
    pub started_at: i64,
}

fn dir() -> PathBuf {
    config::app_dir().join("watch")
}

fn path_for(conversation: &str) -> PathBuf {
    let safe: String = conversation
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    dir().join(format!("{safe}.json"))
}

fn read(conversation: &str) -> Option<Watch> {
    let text = std::fs::read_to_string(path_for(conversation)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Whether a watcher makes sense on this computer right now: a phone to tell, and a user who
/// wants telling.
pub fn wanted(cfg: &Config) -> bool {
    cfg.walkaway_alerts
        && cfg.settings.notify_on_permission()
        && cfg.is_paired()
        && !cfg.local_mode()
        && Unpaired::load().is_none()
}

/// Leaves the note for a tool call that was allowed with the user present, and starts its
/// watcher. Called after the hook has printed its answer. Nothing here can fail the hook: an
/// error is logged and that is all.
pub fn start(input: &Value) {
    let input = payload::Input(input);
    let conversation = input.session_id();
    let tool_name = input.tool_name().unwrap_or_default();
    let cwd = input.cwd();
    let summary = payload::tool_summary(&tool_name, &input.tool_input(), &cwd)
        .unwrap_or_else(|| payload::display_name(&tool_name));
    let watch = Watch {
        token: uuid::Uuid::new_v4().to_string(),
        conversation: conversation.clone(),
        call_id: input.0["toolCall"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        tool_name,
        summary,
        project: input.project_name(),
        cwd,
        started_at: config::now(),
    };
    let result = (|| -> anyhow::Result<()> {
        config::ensure_dir(&dir())?;
        config::write_atomic(&path_for(&conversation), serde_json::to_string(&watch)?.as_bytes())?;
        crate::update::spawn_detached(&["watch", &conversation, &watch.token])?;
        Ok(())
    })();
    if let Err(err) = result {
        log(&format!("walk-away watcher not started: {err:#}"));
        clear(&conversation);
    }
}

/// The call has been answered, has finished, or the turn has moved on: nothing to watch.
pub fn clear(conversation: &str) {
    let _ = std::fs::remove_file(path_for(conversation));
}

/// PostToolUse: clears the note when it is about this call. A note for another call (a newer
/// one, when several run at once) stays.
pub fn finished(conversation: &str, call_id: &str) {
    if read(conversation).is_some_and(|watch| watch.call_id == call_id) {
        clear(conversation);
    }
}

/// Deletes every note (and any temp file a crash left), then the folder. Uninstall.
pub fn remove_all() {
    let Ok(entries) = std::fs::read_dir(dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".json") || name.ends_with(".tmp") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let _ = std::fs::remove_dir(dir());
}

/// What the watcher does next.
#[derive(Debug, PartialEq)]
pub enum Step {
    /// Look again in a moment
    Wait,
    /// Nothing more to watch: say nothing
    End(&'static str),
    /// The user has been away long enough with the call still open
    Alert,
}

/// One look by the watcher. `note_stands`: the note is still there and still ours. `away`: the
/// user is known to be away (None: can't tell, which never counts as away). `away_for`: how long
/// they have been away without a break, kept by the caller.
pub fn step(
    note_stands: bool,
    away: Option<bool>,
    away_for: Option<Duration>,
    watched: Duration,
    grace: Duration,
    max: Duration,
) -> Step {
    if !note_stands {
        return Step::End("the call finished or the turn moved on");
    }
    if watched >= max {
        return Step::End("watched long enough");
    }
    match (away, away_for) {
        (Some(true), Some(gone)) if gone >= grace => Step::Alert,
        _ => Step::Wait,
    }
}

/// The informational card: no options, no remote input, so the phone shows it as a note.
pub fn card(watch: &Watch) -> Value {
    json!({
        "type": "notification",
        "sessionId": watch.conversation,
        "notificationType": "permission_prompt",
        "title": payload::title(watch.project.as_deref()),
        "message": t!("walkaway.title"),
        "promptText": t!("walkaway.body", summary = SessionState::truncate(&watch.summary, 120)),
        "terminalContext": payload::terminal_context(&watch.conversation, &watch.cwd, false),
        "projectName": watch.project,
        "projectPath": watch.cwd,
        "toolName": payload::display_name(&watch.tool_name),
        "toolInput": watch.summary,
    })
}

/// (tick, grace, longest watch). Debug builds let a test shorten them:
/// `ANTIGRAVITY_NOTIFIER_TEST_WATCH_MS=<tick>,<grace>,<max>`.
fn timings() -> (Duration, Duration, Duration) {
    #[cfg(debug_assertions)]
    if let Ok(value) = std::env::var("ANTIGRAVITY_NOTIFIER_TEST_WATCH_MS") {
        let ms: Vec<u64> = value.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        if let [tick, grace, max] = ms[..] {
            let d = Duration::from_millis;
            return (d(tick), d(grace), d(max));
        }
    }
    (TICK, GRACE, MAX_WATCH)
}

/// `antigravity-notifier watch <conversation> <token>`: the detached watcher itself.
pub fn run(conversation: &str, token: &str) {
    let (tick, grace, max) = timings();
    let cfg = Config::load();
    if !wanted(&cfg) {
        return;
    }
    let threshold = cfg.settings.presence_threshold();
    let started = Instant::now();
    let mut away_since: Option<Instant> = None;
    log(&format!("watching a tool call in {conversation} in case the user walks away"));
    loop {
        std::thread::sleep(tick);
        let watch = read(conversation).filter(|watch| watch.token == token);
        // Unknown presence is not "away": where idle time can't be read, nothing is ever sent
        let away = presence::idle_seconds().map(|idle| idle >= threshold);
        match away {
            Some(true) => {
                away_since.get_or_insert_with(Instant::now);
            }
            _ => away_since = None,
        }
        let next = step(
            watch.is_some(),
            away,
            away_since.map(|since| since.elapsed()),
            started.elapsed(),
            grace,
            max,
        );
        match (next, watch) {
            (Step::Wait, _) => {}
            (Step::Alert, Some(watch)) => {
                // The note goes first, so nothing can send for this call a second time
                clear(conversation);
                alert(&cfg, &watch);
                return;
            }
            (Step::End(why), watch) => {
                if watch.is_some() {
                    clear(conversation);
                }
                log(&format!("walk-away watch over: {why}"));
                return;
            }
            (Step::Alert, None) => return,
        }
    }
}

fn alert(cfg: &Config, watch: &Watch) {
    // Settings may have changed on the phone while this watched
    let cfg_now = Config::load();
    let settings = &cfg_now.settings;
    if !wanted(&cfg_now) || settings.in_quiet_hours(chrono::Local::now().time()) {
        log("walk-away alert not sent: quiet hours, or turned off");
        return;
    }
    match Api::new(cfg).post_event(&card(watch), SEND_TIMEOUT) {
        Ok(_) => log(&format!(
            "walk-away alert sent: {} has not finished and the user is away",
            watch.tool_name
        )),
        Err(err) => log(&format!("walk-away alert not sent: {err:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRACE: Duration = Duration::from_secs(20);
    const MAX: Duration = Duration::from_secs(900);
    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn ends_quietly_when_the_note_is_gone() {
        // PostToolUse, a new prompt, a Stop or a newer call all remove or replace the note
        for away in [Some(true), Some(false), None] {
            assert!(matches!(
                step(false, away, Some(secs(60)), secs(5), GRACE, MAX),
                Step::End(_)
            ));
        }
    }

    #[test]
    fn waits_while_the_user_is_there_or_cannot_be_seen() {
        assert_eq!(step(true, Some(false), None, secs(100), GRACE, MAX), Step::Wait);
        // Unknown presence never becomes an alert, however long it lasts
        assert_eq!(step(true, None, None, secs(800), GRACE, MAX), Step::Wait);
        assert_eq!(step(true, None, Some(secs(800)), secs(800), GRACE, MAX), Step::Wait);
    }

    #[test]
    fn alerts_only_after_the_grace_and_gives_up_after_fifteen_minutes() {
        assert_eq!(step(true, Some(true), Some(secs(0)), secs(30), GRACE, MAX), Step::Wait);
        assert_eq!(step(true, Some(true), Some(secs(19)), secs(49), GRACE, MAX), Step::Wait);
        assert_eq!(step(true, Some(true), Some(secs(20)), secs(50), GRACE, MAX), Step::Alert);
        // They came back before the grace ended: the caller forgets how long they were away
        assert_eq!(step(true, Some(false), None, secs(45), GRACE, MAX), Step::Wait);
        // Fifteen minutes in, nothing more is sent even if they are away
        assert!(matches!(
            step(true, Some(true), Some(secs(300)), secs(900), GRACE, MAX),
            Step::End(_)
        ));
    }

    #[test]
    fn the_card_is_a_note_with_nothing_to_press() {
        let watch = Watch {
            token: "t".into(),
            conversation: "c1".into(),
            call_id: "call_1".into(),
            tool_name: "run_command".into(),
            summary: "npm publish".into(),
            project: Some("demo".into()),
            cwd: "/Users/dev/projects/demo".into(),
            started_at: 0,
        };
        let card = card(&watch);
        assert_eq!(card["title"], "Antigravity - demo");
        assert_eq!(card["message"], "Antigravity may be waiting for you");
        assert_eq!(
            card["promptText"],
            "npm publish hasn't finished. If it needs approval, that's at your computer."
        );
        assert_eq!(card["terminalContext"]["supportsRemoteInput"], false);
        assert!(card.get("options").is_none() && card.get("promptType").is_none());
        assert_eq!(card["sessionId"], "c1");
        // A long command is cut, not sent whole
        let long = Watch { summary: "x".repeat(500), ..watch };
        assert!(card_text(&long).chars().count() < 220);
    }

    fn card_text(watch: &Watch) -> String {
        card(watch)["promptText"].as_str().unwrap().to_string()
    }

    #[test]
    fn the_real_timings_are_the_ones_promised() {
        // No test override in this process
        if std::env::var("ANTIGRAVITY_NOTIFIER_TEST_WATCH_MS").is_err() {
            assert_eq!(timings(), (secs(1), secs(20), secs(900)));
        }
    }
}
