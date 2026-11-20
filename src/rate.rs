//! A request, once per install, to rate the phone app. Asked only of someone it is working for:
//! after the 20th request or done card answered from the phone while Codex waited, at the next
//! SessionStart that shows nothing else, and never the same day as the ccn suggestion or a licence
//! nudge. A desktop notification whose click opens the server's /rate page (which sends the phone
//! to its store), plus a line in Codex. `codex-notifier config set rate_prompt off` or
//! `CODEX_NOTIFIER_NO_NUDGE=1` turns it off.

use rust_i18n::t;
use serde_json::{Value, json};

use crate::config::{self, Config};

const DAY: i64 = 24 * 60 * 60;
/// Answers from the phone before the request is due
pub const MIN_ANSWERS: u64 = 20;

/// The setting is on and `CODEX_NOTIFIER_NO_NUDGE` isn't set.
pub fn enabled(cfg: &Config) -> bool {
    cfg.rate_prompt && !crate::local::no_nudge_env()
}

/// The paired phones' one platform, when they share one. None (the page offers both stores) for
/// none, both, or anything else.
pub fn platform(platforms: &[String]) -> Option<&str> {
    let mut known: Vec<&str> = platforms
        .iter()
        .map(String::as_str)
        .filter(|p| *p == "ios" || *p == "android")
        .collect();
    known.sort();
    known.dedup();
    match known.as_slice() {
        [one] => Some(one),
        _ => None,
    }
}

/// The page a click opens.
pub fn url(platforms: &[String]) -> String {
    let platform = platform(platforms)
        .map(|p| format!("&platform={p}"))
        .unwrap_or_default();
    format!(
        "{}/rate?app=codex{platform}&ref=cli",
        config::DEFAULT_SERVER_URL
    )
}

/// Whether the request is due: never shown, 20 answers from the phone, and nothing else nudged
/// in the last day.
pub fn due(cfg: &Config, now: i64) -> bool {
    let recent = |at: Option<i64>| at.is_some_and(|at| now - at < DAY);
    cfg.rate_prompt_shown_at.is_none()
        && cfg.phone_answers >= MIN_ANSWERS
        && !recent(cfg.cross_promo_last_at)
        && !recent(cfg.licence_nudge_last_at)
}

/// The `systemMessage` for a paired SessionStart that shows nothing else, with the desktop
/// notification (unless those are off), when due.
pub fn session_message(cfg: &mut Config, now: i64) -> Option<Value> {
    if !enabled(cfg) || !due(cfg, now) {
        return None;
    }
    cfg.rate_prompt_shown_at = Some(now);
    let _ = cfg.save();
    let url = url(&cfg.phone_platforms);
    if cfg.desktop_notifications {
        crate::notify::show_link(crate::notify::APP_NAME, &t!("rate.notification"), &url);
    }
    crate::logger::log(&format!(
        "rating request shown at SessionStart, {url} (desktop notification: {})",
        cfg.desktop_notifications
    ));
    crate::telemetry::event_detached(
        "cli_rate_prompt_shown",
        &[("platform", platform(&cfg.phone_platforms).unwrap_or("any"))],
    );
    Some(json!({ "systemMessage": t!("rate.message", url = url) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn one_platform_goes_in_the_url_otherwise_the_page_chooses() {
        let base = "https://api.notifierforcodex.com/rate?app=codex";
        assert_eq!(url(&list(&["ios"])), format!("{base}&platform=ios&ref=cli"));
        assert_eq!(
            url(&list(&["android", "android"])),
            format!("{base}&platform=android&ref=cli")
        );
        assert_eq!(url(&list(&["ios", "android"])), format!("{base}&ref=cli"));
        assert_eq!(url(&list(&[])), format!("{base}&ref=cli"));
        assert_eq!(url(&list(&["web"])), format!("{base}&ref=cli"));
    }

    #[test]
    fn due_after_twenty_answers_once_and_not_beside_another_nudge() {
        let now = 1_700_000_000;
        let mut cfg = Config::default();
        cfg.phone_answers = 19;
        assert!(!due(&cfg, now));
        cfg.phone_answers = 20;
        assert!(due(&cfg, now));
        // Not the same day as the ccn suggestion or a licence nudge
        cfg.cross_promo_last_at = Some(now - 3600);
        assert!(!due(&cfg, now));
        cfg.cross_promo_last_at = Some(now - DAY);
        assert!(due(&cfg, now));
        cfg.licence_nudge_last_at = Some(now - 60);
        assert!(!due(&cfg, now));
        cfg.licence_nudge_last_at = None;
        // Once per install
        cfg.rate_prompt_shown_at = Some(now - 400 * DAY);
        assert!(!due(&cfg, now));
    }

    #[test]
    fn every_translation_names_the_app_and_keeps_the_url() {
        let link = url(&list(&["ios"]));
        for lang in config::LOCALES {
            let text = rust_i18n::t!("rate.notification", locale = lang);
            assert!(text.contains("Notifier for Codex"), "{lang}: {text}");
            let message = rust_i18n::t!("rate.message", locale = lang, url = link);
            assert!(
                message.contains("Notifier for Codex") && message.contains(&link),
                "{lang}: {message}"
            );
        }
    }
}
