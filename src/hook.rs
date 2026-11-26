//! The hook dispatcher. Antigravity runs `antigravity-notifier hook <Event>` with the event JSON
//! on stdin; this decides what to do and prints its answer.
//!
//! THE RULE THIS FILE EXISTS TO KEEP: Antigravity is fail-closed. A PreToolUse hook that exits
//! non-zero, runs past its timeout, or prints an answer with no decision in it (`{}`) BLOCKS the
//! user's tool call (measured on agy 1.5.18, 26 Nov 2026). So whatever goes wrong here (no
//! pairing, no network, input that isn't JSON, a tool nobody has heard of, a bug that panics),
//! the process still exits 0 having printed a decision, and that decision is
//! `{"decision":"allow"}`, which leaves the call to Antigravity's own permission policy exactly
//! as if this hook were not installed. Three things make sure of it:
//!   - `main` answers from one place, after `catch_unwind`, with `fallback` for every failure;
//!   - a watchdog thread answers and exits if the work has not finished in time;
//!   - main.rs sends `hook` here before the argument parser or anything else can fail.
//!
//! Antigravity's events (there is no SessionEnd, no Notification and no PermissionRequest):
//!   SessionStart    settings sync, nudges
//!   PreInvocation   `invocationNum == 0` with a new `lastUserInput` is the user's prompt
//!   PreToolUse      questions, and approvals: the only place a tool call can wait for the phone
//!   PostToolUse     counts the edit a tool call made, for the lock-screen card
//!   Stop            the done card; a reply from the phone continues the conversation

use anyhow::Result;
use rust_i18n::t;
use serde_json::Value;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::api::{self, Api, Wait};
use crate::config::{self, Config, Unpaired};
use crate::logger::log;
use crate::payload::{self, Input, ToolClass};
use crate::session::{self, Card, SessionState};
use crate::{hookcfg, live, local, notify, presence, respond, transcript};

const EVENT_TIMEOUT: Duration = Duration::from_secs(3);
const QUICK_TIMEOUT: Duration = Duration::from_secs(2);

/// Best-effort updates (the lock-screen card, settling a card answered at the keyboard) run on
/// every prompt and tool call, so a slow server would slow every one of them. Each gets this
/// long, and all of them in one hook together get `BEST_EFFORT_BUDGET`.
const BEST_EFFORT_TIMEOUT: Duration = Duration::from_millis(1000);
const BEST_EFFORT_BUDGET: Duration = Duration::from_millis(1500);
/// After a best-effort update fails, the next ones are skipped for this long, so an unreachable
/// server costs one timeout a minute rather than one per tool call
const BEST_EFFORT_BACKOFF: i64 = 60;
const SETTINGS_MAX_AGE: i64 = 60 * 60;
const REGISTER_MAX_AGE: i64 = 24 * 60 * 60;

const DAY: i64 = 24 * 60 * 60;

/// How often a SessionStart asks the server whether to nudge
const LICENCE_CHECK_EVERY: i64 = DAY;
/// Once shown, the licence nudge waits this long before it may repeat
const LICENCE_NUDGE_REPEAT: i64 = 7 * DAY;

/// A PreToolUse hook that is still holding for the phone answers by itself this long after it
/// started, two minutes inside the timeout hooks.json gives it.
pub const PRE_TOOL_DEADLINE: Duration = Duration::from_secs(hookcfg::PRE_TOOL_TIMEOUT - 120);
/// The same for Stop (timeout 700 s, longest hold 600 s)
const STOP_DEADLINE: Duration = Duration::from_secs(660);

// ============================================
// Answering: exactly once, always
// ============================================

static ANSWERED: AtomicBool = AtomicBool::new(false);
/// Set while a hook waits for the phone, which is the only work allowed to take long
static HOLDING: AtomicBool = AtomicBool::new(false);

/// Prints the hook's answer. Only the first call prints: the watchdog and the main thread can
/// both get here. A stdout that has gone away is not an error worth dying for.
fn emit(value: &Value) {
    if ANSWERED.swap(true, Ordering::SeqCst) {
        return;
    }
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

/// The events this build knows, as Antigravity spells them.
const EVENTS: &[&str] = &[
    "SessionStart",
    "PreInvocation",
    "PostInvocation",
    "PreToolUse",
    "PostToolUse",
    "Stop",
];

fn known_event(name: &str) -> Option<&'static str> {
    EVENTS
        .iter()
        .find(|event| event.eq_ignore_ascii_case(name.trim()))
        .copied()
}

/// Which event this is: the command line says (hooks.json passes it), else the payload, else a
/// guess from the payload's shape. None when nothing tells.
fn event_of(arg: Option<&str>, input: &Value) -> Option<&'static str> {
    arg.and_then(known_event)
        .or_else(|| Input(input).event().and_then(known_event))
        .or_else(|| {
            let has = |key: &str| input.get(key).is_some();
            if has("toolCall") {
                // PostToolUse carries the tool's `error`; PreToolUse has none yet
                Some(if has("error") { "PostToolUse" } else { "PreToolUse" })
            } else if has("terminationReason") || has("fullyIdle") {
                Some("Stop")
            } else if has("invocationNum") {
                Some("PreInvocation")
            } else {
                None
            }
        })
}

/// The answer when there is nothing to say, or something went wrong. PreToolUse must carry a
/// decision; so must an event that could not be identified, since it may be one. For the rest
/// `{}` is what Antigravity was measured to accept.
fn fallback(event: Option<&str>) -> Value {
    match event {
        Some("PreToolUse") | None => respond::allow(),
        Some(_) => respond::nothing(),
    }
}

/// How long the hook may run before the watchdog answers for it: `quick` while it is doing its
/// ordinary work, `holding` once it waits for the phone.
fn deadlines(event: Option<&str>) -> (Duration, Duration) {
    let secs = Duration::from_secs;
    let (quick, holding) = match event {
        Some("PreToolUse") | None => (secs(20), PRE_TOOL_DEADLINE),
        Some("Stop") => (secs(20), STOP_DEADLINE),
        Some("SessionStart") => (secs(20), secs(20)),
        // PreInvocation and PostToolUse have a 10 s timeout in hooks.json
        Some(_) => (secs(6), secs(6)),
    };
    // Tests shorten both to see the watchdog work
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("ANTIGRAVITY_NOTIFIER_TEST_DEADLINE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return (Duration::from_millis(ms), Duration::from_millis(ms));
    }
    (quick, holding)
}

/// What the watchdog says when time is up. A tool call that was waiting for the phone goes to
/// the local prompt, as it does when the hold ends by itself; anything else is the fallback.
fn timed_out_answer(event: Option<&str>, holding: bool, is_question: bool) -> Value {
    match event {
        Some("PreToolUse") if holding && !is_question => respond::force_ask(),
        _ => fallback(event),
    }
}

static HOLDING_QUESTION: AtomicBool = AtomicBool::new(false);

fn start_watchdog(event: Option<&'static str>) {
    let started = Instant::now();
    let (quick, holding) = deadlines(event);
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(100));
            if ANSWERED.load(Ordering::SeqCst) {
                return;
            }
            let held = HOLDING.load(Ordering::SeqCst);
            let limit = if held { holding } else { quick };
            if started.elapsed() >= limit {
                let question = HOLDING_QUESTION.load(Ordering::SeqCst);
                emit(&timed_out_answer(event, held, question));
                std::process::exit(0);
            }
        }
    });
}

/// The whole hook. Never panics, never returns an error, and has always printed an answer by
/// the time it returns; the process then exits 0.
pub fn main(event_arg: Option<String>) {
    // Rust's default panic hook writes to stderr, and a panic inside a panic hook aborts the
    // process: this one does nothing, and the panic is logged below once it has been caught.
    std::panic::set_hook(Box::new(|_| {}));
    let arg_event = event_arg.as_deref().and_then(known_event);
    // Before stdin is read: a pipe that is never closed must not hang the tool call either
    start_watchdog(arg_event);

    let mut event = arg_event;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::i18n::init();
        let mut raw = Vec::new();
        let _ = std::io::stdin().lock().read_to_end(&mut raw);
        // Lossy, and without a byte order mark: whatever arrives is read as well as it can be
        let text = String::from_utf8_lossy(&raw);
        let text = text.trim_start_matches('\u{feff}').trim();
        let input = match serde_json::from_str::<Value>(text) {
            Ok(input) => input,
            Err(err) => {
                log(&format!(
                    "{}: hook input is not JSON ({err})",
                    event.unwrap_or("?")
                ));
                Value::Null
            }
        };
        event = event_of(event_arg.as_deref(), &input);
        run_inner(event, input)
    }));
    let answer = match outcome {
        Ok(Ok(Some(output))) => checked(event, output),
        Ok(Ok(None)) => fallback(event),
        Ok(Err(err)) => {
            log(&format!("{} hook error: {err:#}", event.unwrap_or("?")));
            fallback(event)
        }
        Err(_) => {
            // Logging is guarded too: nothing may stop the answer below
            let _ = std::panic::catch_unwind(|| {
                log("hook panicked; answered as if nothing was installed")
            });
            fallback(event)
        }
    };
    emit(&answer);
}

/// The last gate before stdout: a PreToolUse answer (or one for an event that could not be
/// named) without a decision would block the tool, so it becomes the fallback.
fn checked(event: Option<&str>, output: Value) -> Value {
    let needs_decision = matches!(event, Some("PreToolUse") | None);
    let has_decision = output
        .get("decision")
        .and_then(|d| d.as_str())
        .is_some_and(|d| !d.is_empty());
    if needs_decision && !has_decision {
        // A message for the user can ride along with the decision
        let mut answer = fallback(event);
        if let Some(message) = output.get("systemMessage") {
            answer["systemMessage"] = message.clone();
        }
        return answer;
    }
    output
}

// ============================================
// What the events tell
// ============================================

/// When the licence nudge may show: on the second and sixth day after pairing, and otherwise once
/// a week. Never twice in a day. `paired_at` None (an install from before it was recorded) counts
/// as long ago.
pub fn licence_nudge_allowed(now: i64, paired_at: Option<i64>, last_shown: Option<i64>) -> bool {
    if last_shown.is_some_and(|last| now - last < DAY) {
        return false;
    }
    let days = (now - paired_at.unwrap_or(0)) / DAY;
    let early = days == 2 || days == 6;
    let weekly = match last_shown {
        Some(last) => now - last >= LICENCE_NUDGE_REPEAT,
        None => days >= 7,
    };
    early || weekly
}

/// Shown once, when a SessionStart finds the first phone paired (the user paired from the phone
/// after leaving `antigravity-notifier pair`, or scanned the QR code later)
fn paired_system_message(phone: &str) -> String {
    t!("hook.paired_message", phone = phone).into_owned()
}

/// The server writes the nudge for Claude Code Notifier's `ccn` (Nov 2026: CliCopy is not yet
/// per app), so its command and any mention of Claude are put in Antigravity's words here.
/// TODO: drop once the server writes antigravity_cli copy itself.
fn antigravity_wording(mut message: api::NudgeMessage) -> api::NudgeMessage {
    let fix = |text: &str| {
        // The product name first: "Claude Code Notifier" is "Antigravity Notifier"
        text.replace("Claude Code Notifier", crate::notify::APP_NAME)
            .replace("Claude Code", "Antigravity")
            .replace("Claude", "Antigravity")
    };
    message.title = fix(&message.title);
    message.body = fix(&message.body);
    if let Some(rest) = message.command.strip_prefix("ccn ") {
        message.command = format!("antigravity-notifier {rest}");
    }
    message
}

/// Whether a PreInvocation is the user sending a prompt: the first model call of a turn
/// (`invocationNum` 0; later calls in the turn, and turns a Stop hook continued, count on), with
/// a `lastUserInput` that is new or after a turn that had stopped.
pub fn new_prompt(input: &Value, state: &SessionState) -> bool {
    if input.get("invocationNum").and_then(|n| n.as_i64()) != Some(0) {
        return false;
    }
    let prompt = Input(input).prompt().map(|p| SessionState::truncate(p, 4000));
    state.turn_stopped || state.prompt.is_none() || prompt != state.prompt
}

/// What a Stop hook's input says about how the turn ended.
#[derive(Debug, PartialEq)]
pub struct StopInfo {
    /// `executionNum`: 0 for the turn the user started, one more each time a Stop hook
    /// continued it
    pub execution: i64,
    /// False while background tasks are still running
    pub fully_idle: bool,
    /// The user stopped the turn themselves
    pub cancelled: bool,
    /// What went wrong, when the turn ended in an error
    pub error: Option<String>,
}

impl StopInfo {
    pub fn read(input: &Value) -> StopInfo {
        let text = |key: &str| {
            input
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        };
        let reason = text("terminationReason").unwrap_or("").to_ascii_uppercase();
        let has = |words: &[&str]| words.iter().any(|w| reason.contains(w));
        let cancelled = has(&["CANCEL", "INTERRUPT", "ABORT", "USER_STOP"]);
        // `error` carries the message when there is one; a reason that names a failure without
        // one is shown as it is ("MAX_STEPS_EXCEEDED" reads well enough)
        let error = text("error").map(str::to_string).or_else(|| {
            (!cancelled && has(&["ERROR", "FAIL", "EXCEEDED", "LIMIT", "QUOTA", "TIMEOUT"]))
                .then(|| reason.clone())
        });
        StopInfo {
            execution: input
                .get("executionNum")
                .and_then(|n| n.as_i64())
                .unwrap_or(0),
            // Absent means an Antigravity that doesn't say: assume it is done
            fully_idle: input
                .get("fullyIdle")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            cancelled,
            error,
        }
    }

    /// Whether this stop is worth telling the phone about: not while background tasks still
    /// run (another Stop follows when they finish), and not when the user stopped it at the
    /// keyboard.
    pub fn announce(&self) -> bool {
        self.fully_idle && !self.cancelled
    }
}

/// Antigravity's final message for a Stop, from the transcript. The Stop hook can run a moment
/// before the last line is on disk, in which case the newest message is still the one the
/// previous done card showed: wait briefly for a newer one, and report none rather than repeat
/// the old one.
pub fn final_message(input: &Value, state: &SessionState) -> Option<transcript::Message> {
    let path = transcript::path_for(input)?;
    for attempt in 0..4 {
        match transcript::last_message(&path) {
            Some(message) if Some(message.step) != state.done_step => return Some(message),
            None if state.done_step.is_some() => return None,
            _ => {}
        }
        if attempt < 3 {
            std::thread::sleep(Duration::from_millis(150));
        }
    }
    None
}

/// Whether Antigravity is running one prompt and exiting (`agy -p`), with nobody to answer a
/// local prompt and often nobody watching at all: a script, a cron job. Nothing waits for the
/// phone then. A hold that ran out would end in `force_ask`, which print mode answers with a
/// denial, and a script would lose minutes on every tool call.
/// `ANTIGRAVITY_NOTIFIER_HEADLESS=1` or `=0` says so outright.
fn headless() -> bool {
    match std::env::var("ANTIGRAVITY_NOTIFIER_HEADLESS").as_deref() {
        Ok("1") | Ok("true") => return true,
        Ok("0") | Ok("false") => return false,
        _ => {}
    }
    agy_print_mode().unwrap_or(false)
}

/// Looks up the process tree for Antigravity's CLI and reads its command line. None when it
/// can't be found (the desktop app, the IDE, Windows), which counts as interactive.
#[cfg(unix)]
fn agy_print_mode() -> Option<bool> {
    let mut pid = unsafe { libc::getppid() } as i64;
    for _ in 0..6 {
        if pid <= 1 {
            return None;
        }
        let out = std::process::Command::new("ps")
            .args(["-o", "ppid=,args=", "-p", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let line = String::from_utf8_lossy(&out.stdout);
        let (parent, args) = line.trim().split_once(char::is_whitespace)?;
        if let Some(print) = print_mode_of(args.trim()) {
            return Some(print);
        }
        pid = parent.trim().parse().ok()?;
    }
    None
}

#[cfg(not(unix))]
fn agy_print_mode() -> Option<bool> {
    None
}

/// Whether a command line is `agy` in print mode. None when it is not `agy` at all.
#[cfg_attr(not(unix), allow(dead_code))]
fn print_mode_of(args: &str) -> Option<bool> {
    let mut words = args.split_whitespace();
    let program = words.next()?;
    let base = program.rsplit(['/', '\\']).next()?;
    if !matches!(base, "agy" | "agy.exe" | "antigravity") {
        return None;
    }
    Some(words.any(|w| {
        matches!(w, "-p" | "--print" | "--prompt")
            || w.starts_with("--print=")
            || w.starts_with("--prompt=")
    }))
}

// ============================================
// The dispatcher
// ============================================

/// How a card that may wait for the phone ended.
#[derive(Debug, PartialEq)]
enum Hold {
    /// Nothing waited: the user is at the keyboard, presence is unknown, quiet hours, a
    /// headless run, or the card could not be sent
    NotHeld,
    /// The phone answered
    Answered(String),
    /// The hook waited and no answer came: time ran out, the user came back to the keyboard,
    /// or the request was settled elsewhere
    Unanswered,
}

struct Ctx {
    cfg: Config,
    api: Api,
    input: Value,
    state: SessionState,
    started: Instant,
}

fn run_inner(event: Option<&'static str>, input: Value) -> Result<Option<Value>> {
    #[cfg(debug_assertions)]
    test_knobs();
    let Some(event) = event else {
        log("hook called with no event and input that names none");
        return Ok(None);
    };
    if event == "PostInvocation" {
        // Not installed by `setup`; a hooks.json that calls it anyway gets a quick nothing
        return Ok(None);
    }
    let started = Instant::now();
    let session_id = Input(&input).session_id();

    let mut cfg = Config::load();
    let mut state = SessionState::load(&session_id);
    state.touch(&input);
    // Once a day, start a detached look for a newer antigravity-notifier; the hook itself never waits on it
    let update_message = if event == "SessionStart" {
        crate::update::daily_check(&mut cfg)
    } else {
        None
    };
    if !cfg.is_paired() {
        // One line so the log shows Antigravity is calling us even before pairing
        log(&format!(
            "{event}: not paired yet, desktop notifications only"
        ));
        // No credentials: desktop notifications only, and no network at all
        return Ok(local::run(&mut cfg, event, &input, &mut state).or(update_message));
    }
    let api = Api::new(&cfg);

    if let Some(mut flag) = Unpaired::load() {
        // No network while unpaired, except a once-a-day check at SessionStart in case the
        // rejection was a server mistake. `antigravity-notifier pair` deletes the flag.
        let recheck =
            event == "SessionStart" && config::now() - flag.checked_at > Unpaired::RECHECK_AFTER;
        if !(recheck && still_accepted(&api, &mut flag)) {
            return Ok(unpaired(&cfg, event, &input, &mut state, flag).or(update_message));
        }
        log("the server accepts this computer again");
    }

    // Registered but no phone known to be paired: one quick question at SessionStart, so a phone
    // that paired while no `antigravity-notifier pair` was watching is picked up
    let announce = if event == "SessionStart" && cfg.phones.unwrap_or(0) == 0 {
        check_phones(&mut cfg, &api)
    } else {
        None
    };
    if cfg.local_mode() {
        let message = local::run(&mut cfg, event, &input, &mut state);
        return Ok(announce.or(message).or(update_message));
    }

    let mut ctx = Ctx {
        cfg,
        api,
        input,
        state,
        started,
    };
    let result = ctx.dispatch(event);
    if ctx.api.was_rejected() {
        log(&format!(
            "{event}: the server rejected this computer's credentials; run `antigravity-notifier pair`"
        ));
        let message = unpaired(&ctx.cfg, event, &ctx.input, &mut ctx.state, Unpaired::new());
        return Ok(message.or(update_message));
    }
    let message = match announce {
        Some(message) => Some(message),
        None => result?.or(update_message),
    };
    // The ccn suggestion only on a SessionStart that says nothing else: never with a licence
    // nudge, a "paired with" or an update notice. Never through antigravity_wording, which
    // would turn its "Claude Code" into "Antigravity".
    // Then the rating request, the same way.
    if message.is_none() && event == "SessionStart" {
        let now = config::now();
        return Ok(crate::crosspromo::session_message(&mut ctx.cfg, now)
            .or_else(|| crate::rate::session_message(&mut ctx.cfg, now)));
    }
    Ok(message)
}

/// Debug builds only: ways for tests/hook.rs to make the hook fail on purpose.
#[cfg(debug_assertions)]
fn test_knobs() {
    match std::env::var("ANTIGRAVITY_NOTIFIER_TEST_FAULT").as_deref() {
        Ok("panic") => panic!("test fault"),
        Ok("stall") => loop {
            std::thread::sleep(Duration::from_secs(60));
        },
        Ok("stall_holding") => {
            HOLDING.store(true, Ordering::SeqCst);
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
        _ => {}
    }
}

/// Asks the server how many phones are paired and records the answer. Returns the one-time
/// "paired with" message when the first phone has arrived since the last look.
fn check_phones(cfg: &mut Config, api: &Api) -> Option<Value> {
    let devices = match api.pairings_within(QUICK_TIMEOUT) {
        Ok(devices) => devices,
        Err(err) => {
            log(&format!("phone check skipped: {err:#}"));
            return None;
        }
    };
    cfg.record_phones(devices.len());
    cfg.record_platforms(devices.iter().map(|d| d.platform.as_str()));
    let mut message = None;
    if !devices.is_empty() && !cfg.paired_message_shown {
        cfg.paired_message_shown = true;
        let newest = devices.iter().max_by_key(|d| d.created_at)?;
        let name = newest
            .name
            .clone()
            .unwrap_or_else(|| t!("common.your_phone").into_owned());
        message = Some(serde_json::json!({ "systemMessage": paired_system_message(&name) }));
    }
    let _ = cfg.save();
    message
}

/// Asks the server whether it accepts this computer after all. Clears the flag when it does.
fn still_accepted(api: &Api, flag: &mut Unpaired) -> bool {
    match api.get_settings(None, QUICK_TIMEOUT) {
        Ok(_) => {
            Unpaired::clear();
            true
        }
        Err(err) => {
            if !api::is_rejected(&err) {
                log(&format!("unpaired re-check failed: {err:#}"));
            }
            flag.checked_at = config::now();
            false
        }
    }
}

/// What a hook does while the server rejects this computer: a desktop notification at most once a
/// day, and a message at SessionStart and on the first prompt of each conversation. Tool calls
/// and stops get the fallback, so nothing is ever blocked for want of a pairing.
fn unpaired(
    cfg: &Config,
    event: &str,
    input: &Value,
    state: &mut SessionState,
    mut flag: Unpaired,
) -> Option<Value> {
    let now = config::now();
    if cfg.desktop_notifications && now - flag.notified_at > Unpaired::NOTIFY_EVERY {
        notify::show(
            notify::APP_NAME,
            &t!("hook.unpaired_notification"),
            cfg.settings.sound(),
        );
        flag.notified_at = now;
    }
    flag.save();
    let first_prompt = event == "PreInvocation" && new_prompt(input, state);
    if event == "SessionStart" || (first_prompt && !state.unpaired_warned) {
        state.unpaired_warned = true;
        let _ = state.save();
        return Some(serde_json::json!({ "systemMessage": t!("hook.unpaired_message") }));
    }
    None
}

impl Ctx {
    fn dispatch(&mut self, event: &str) -> Result<Option<Value>> {
        match event {
            "SessionStart" => self.session_start(),
            "PreInvocation" => self.pre_invocation(),
            "PreToolUse" => self.pre_tool_use().map(Some),
            "PostToolUse" => self.post_tool_use(),
            "Stop" => self.stop(),
            other => {
                log(&format!("ignoring event {other}"));
                Ok(None)
            }
        }
    }

    fn input(&self) -> Input<'_> {
        Input(&self.input)
    }

    fn quiet(&self) -> bool {
        self.cfg
            .settings
            .in_quiet_hours(chrono::Local::now().time())
    }

    /// The user is at the terminal Antigravity runs in, and asked for nothing to be sent then
    fn quiet_at_terminal(&self) -> bool {
        self.cfg.settings.quiet_when_present()
            && presence::at_terminal(self.cfg.settings.presence_threshold())
    }

    // ---- events ----

    fn session_start(&mut self) -> Result<Option<Value>> {
        self.state.started_at = config::now();
        self.state.save()?;
        session::prune_old(7);

        let now = config::now();
        // Registration is an unauthenticated upsert, so it would bring a deleted computer back
        // with no phones. Only re-register once an authenticated call has just been accepted.
        let register_due = now - self.cfg.registered_at.unwrap_or(0) > REGISTER_MAX_AGE;
        let mut accepted = false;
        if register_due || now - self.cfg.settings_fetched_at.unwrap_or(0) > SETTINGS_MAX_AGE {
            match self
                .api
                .get_settings(self.cfg.settings_etag.as_deref(), QUICK_TIMEOUT)
            {
                Ok(Some(result)) => {
                    accepted = true;
                    self.cfg.settings = result.settings;
                    self.cfg.settings_updated_at = result.updated_at;
                    self.cfg.settings_etag = result.etag;
                    self.cfg.settings_fetched_at = Some(now);
                    let _ = self.cfg.save();
                }
                Ok(None) => {
                    accepted = true;
                    self.cfg.settings_fetched_at = Some(now);
                    let _ = self.cfg.save();
                }
                Err(err) if api::is_rejected(&err) => return Err(err),
                Err(err) => log(&format!("settings sync skipped: {err:#}")),
            }
        }
        if register_due
            && accepted
            && let Some(identity) = crate::commands::identity(&self.cfg)
        {
            match self.api.register(&identity) {
                Ok(()) => {
                    self.cfg.registered_at = Some(now);
                    let _ = self.cfg.save();
                }
                Err(err) => log(&format!("re-registration skipped: {err:#}")),
            }
        }
        Ok(self.licence_nudge(now))
    }

    /// Once a day, asks whether this computer should be told about licences, and tells it when the
    /// cadence allows (`licence_nudge_allowed`). The server writes the message and picks the
    /// variant; `ANTIGRAVITY_NOTIFIER_NO_NUDGE=1` turns it off.
    fn licence_nudge(&mut self, now: i64) -> Option<Value> {
        if local::no_nudge_env()
            || now - self.cfg.licence_checked_at.unwrap_or(0) < LICENCE_CHECK_EVERY
        {
            return None;
        }
        self.cfg.licence_checked_at = Some(now);
        let _ = self.cfg.save();
        let status = match self.api.licence_status(QUICK_TIMEOUT) {
            Ok(status) => status,
            Err(err) => {
                log(&format!("licence check skipped: {err:#}"));
                return None;
            }
        };
        if let Some(platforms) = &status.phone_platforms {
            self.cfg
                .record_platforms(platforms.iter().map(String::as_str));
            let _ = self.cfg.save();
        }
        if !status.nudge {
            return None;
        }
        let since = self.cfg.paired_at.or(self.cfg.registered_at);
        if !licence_nudge_allowed(now, since, self.cfg.licence_nudge_last_at) {
            return None;
        }
        self.cfg.licence_nudge_last_at = Some(now);
        let _ = self.cfg.save();
        let Some(message) = status.nudge_message.map(antigravity_wording) else {
            // A server that says `nudge: true` without writing the message itself (one from
            // before Oct 2026). The line describes this computer, never anyone's store purchase.
            return Some(serde_json::json!({ "systemMessage": t!("hook.licence_message") }));
        };
        if self.cfg.desktop_notifications {
            notify::show(&message.title, &message.body, false);
        }
        crate::telemetry::event(
            "cli_nudge_shown",
            &[
                ("id", &message.id),
                ("kind", &message.kind),
                ("variant", &message.variant),
            ],
        );
        Some(serde_json::json!({
            "systemMessage": t!(
                "hook.nudge_message",
                title = message.title,
                body = message.body,
                command = message.command
            )
        }))
    }

    /// Every model call starts with one of these; only the first of a turn the user started is
    /// their prompt.
    fn pre_invocation(&mut self) -> Result<Option<Value>> {
        if !new_prompt(&self.input, &self.state) {
            return Ok(None);
        }
        self.settle_last_card();
        let now = config::now();
        if let Some(prompt) = self.input().prompt() {
            self.state.prompt = Some(SessionState::truncate(prompt, 4000));
            self.state.prompt_at = Some(now);
        }
        self.state.turn = live::Turn::begin(now);
        self.state.turn_stopped = false;
        // `executionNum` may start again at 0 with each prompt
        self.state.done_execution = None;
        self.state.continued.clear();
        self.send_working(now, None);
        self.state.save()?;
        Ok(None)
    }

    /// The one place a tool call can wait for the phone. Always returns a decision.
    fn pre_tool_use(&mut self) -> Result<Value> {
        self.settle_last_card();
        let tool_name = self.input().tool_name().unwrap_or_default();
        let tool_input = self.input().tool_input();
        let call_id = self.input["toolCall"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let class = payload::classify(&tool_name);
        self.state.tool_name = Some(tool_name.clone()).filter(|n| !n.is_empty());
        self.state.tool_input = Some(tool_input.clone());
        self.state.tool_at = Some(config::now());
        self.state
            .turn
            .tool_starting(&tool_name, &tool_input, &call_id);
        if class != ToolClass::Question && !tool_name.is_empty() {
            self.send_working(config::now(), Some(&tool_name));
        }
        // A state file that can't be written is no reason to stop a tool call
        if let Err(err) = self.state.save() {
            log(&format!("could not save the session state: {err:#}"));
        }

        if class == ToolClass::Question {
            return Ok(self.question(&tool_input));
        }
        if !self.cfg.approval_wanted(class) || !self.cfg.settings.notify_on_permission() {
            return Ok(respond::allow());
        }
        // "Allow for session" on the phone earlier in this conversation
        if self.state.allowed_tools.contains(&tool_name) {
            return Ok(respond::auto_approve());
        }
        // Antigravity has no event for "a prompt is about to show", and most calls run without
        // one. So a card goes out only when the hook is going to wait for the answer: with the
        // user at the keyboard nothing is sent, and Antigravity asks them itself if it wants to.
        let (card, kind, key) = payload::permission_card(&self.input(), false);
        let ceiling = self
            .cfg
            .settings
            .hold_ceiling()
            .min(hookcfg::MAX_TOOL_HOLD);
        match self.blocking(kind, &key, card, ceiling, false, false) {
            Ok(Hold::Answered(text)) => {
                let approval = respond::permission_output(&text);
                if approval.approved {
                    self.state.turn.record_approval();
                }
                if approval.for_session && !self.state.allowed_tools.contains(&tool_name) {
                    self.state.allowed_tools.push(tool_name);
                }
                let _ = self.state.save();
                Ok(approval.output)
            }
            // Nobody answered: Antigravity's own prompt takes over, saying why
            Ok(Hold::Unanswered) => Ok(respond::force_ask()),
            Ok(Hold::NotHeld) => Ok(respond::allow()),
            Err(err) => {
                log(&format!("approval card not sent: {err:#}"));
                Ok(respond::allow())
            }
        }
    }

    /// `ask_question`: the phone's answer goes back as a deny whose reason holds it. With no
    /// answer the call is allowed, so the question shows in the terminal as usual.
    fn question(&mut self, tool_input: &Value) -> Value {
        if !self.cfg.settings.notify_on_question() {
            return respond::allow();
        }
        let questions = payload::questions_from_input(tool_input);
        if questions.is_empty() {
            return respond::allow();
        }
        let (card, key) = payload::question_card(&self.input(), &questions, false);
        let ceiling = self
            .cfg
            .settings
            .hold_ceiling()
            .min(hookcfg::MAX_TOOL_HOLD);
        match self.blocking("question", &key, card, ceiling, true, true) {
            Ok(Hold::Answered(text)) => {
                respond::question_output(&text, &questions).unwrap_or_else(respond::allow)
            }
            Ok(_) => respond::allow(),
            Err(err) => {
                log(&format!("question card not sent: {err:#}"));
                respond::allow()
            }
        }
    }

    /// The tool call ran (or failed): its edit now counts towards the lock-screen card.
    fn post_tool_use(&mut self) -> Result<Option<Value>> {
        let tool_name = self.input().tool_name().unwrap_or_default();
        let call_id = self.input["toolCall"]["id"].as_str().unwrap_or_default();
        let failed = self.input["error"]
            .as_str()
            .is_some_and(|e| !e.trim().is_empty());
        let before = self.state.turn.clone();
        self.state
            .turn
            .tool_finished(&tool_name, &self.input().tool_input(), call_id, failed);
        if self.state.turn != before {
            self.state.save()?;
        }
        Ok(None)
    }

    fn stop(&mut self) -> Result<Option<Value>> {
        let stop = StopInfo::read(&self.input);
        self.state.turn_stopped = true;
        let _ = self.state.save();
        if !self.cfg.settings.notify_on_done() {
            return Ok(None);
        }
        if !stop.announce() {
            log(&format!(
                "stop not announced (fully idle: {}, stopped by the user: {})",
                stop.fully_idle, stop.cancelled
            ));
            return Ok(None);
        }
        // `continuationActive` stays false after a continue, so it can't tell a second Stop
        // for the same execution (two hook groups, a retry) from a new one. One card, one hold
        // and at most one continue per execution, by our own record.
        if self.state.done_execution == Some(stop.execution) {
            log(&format!(
                "execution {} was already announced",
                stop.execution
            ));
            return Ok(None);
        }
        let message = final_message(&self.input, &self.state);
        if let Some(message) = &message {
            self.state.done_step = Some(message.step);
        }
        self.state.done_execution = Some(stop.execution);
        // Saved before anything waits, so a second Stop finds it
        self.state.save()?;
        let text = message.as_ref().map(|m| m.text.as_str());

        if self.live_updates_allowed() {
            // Before the done card, which may hold for the phone's answer
            let update = live::done(
                &self.state.session_id,
                self.input().project_name().as_deref(),
                text,
                &self.state.turn,
                config::now(),
            );
            if let Err(err) = self.best_effort(&update) {
                log(&format!("could not send the done update: {err:#}"));
            }
        }
        self.settle_last_card();
        let card = payload::done_card(
            &self.input(),
            &self.state,
            text,
            stop.error.as_deref(),
            false,
        );
        let hold = self.cfg.settings.stop_hold();
        // A reply typed on the phone becomes the next turn. That turn's own Stop (the next
        // `executionNum`) holds again, so a conversation from the phone can go on.
        let Hold::Answered(reply) = self.blocking("done", "done", card, hold, true, true)? else {
            return Ok(None);
        };
        let Some(output) = respond::stop_output(&reply) else {
            return Ok(None);
        };
        self.state.continued.push(stop.execution);
        self.state.turn_stopped = false;
        let _ = self.state.save();
        log(&format!(
            "execution {} continued by a reply from the phone",
            stop.execution
        ));
        Ok(Some(output))
    }

    // ---- helpers ----

    /// The phone's lock-screen card follows the session unless the user asked for quiet
    fn live_updates_allowed(&self) -> bool {
        self.cfg.settings.notify_on_done() && !self.quiet() && !self.quiet_at_terminal()
    }

    /// A throttled "Antigravity is working" update for the lock-screen card. Never fails the hook.
    fn send_working(&mut self, now: i64, tool_name: Option<&str>) {
        if !self.live_updates_allowed() || !self.state.turn.working_due(now) {
            return;
        }
        let tool_input = self.input().tool_input();
        let update = live::working(
            &self.state.session_id,
            self.input().project_name().as_deref(),
            self.state.prompt.as_deref(),
            tool_name.map(|name| (name, &tool_input)),
            &self.input().cwd(),
            self.state.turn.started_at,
        );
        if let Err(err) = self.best_effort(&update) {
            log(&format!("could not send the working update: {err:#}"));
        }
    }

    /// Tells the server the previous request in this session was answered at the keyboard.
    fn settle_last_card(&mut self) {
        if self
            .state
            .last_card
            .as_ref()
            .is_none_or(|card| card.settled)
        {
            return;
        }
        let update = payload::session_update(&self.state.session_id, "responded");
        let result = best_effort_post(&self.api, &update, self.started);
        let Some(card) = &mut self.state.last_card else {
            return;
        };
        match result {
            Ok(_) => card.settled = true,
            Err(err) => log(&format!("could not settle card: {err:#}")),
        }
        let _ = self.state.save();
    }

    fn best_effort(&self, update: &Value) -> Result<Option<String>> {
        best_effort_post(&self.api, update, self.started)
    }

    fn send_card(
        &mut self,
        kind: &str,
        key: &str,
        mut card: Value,
        held: bool,
    ) -> Result<Option<String>> {
        card["terminalContext"]["supportsRemoteInput"] = Value::Bool(held);
        let id = self.api.post_event(&card, EVENT_TIMEOUT)?;
        if let Some(id) = &id {
            self.state.last_card = Some(Card {
                notification_id: id.clone(),
                kind: kind.to_string(),
                key: key.to_string(),
                sent_at: config::now(),
                held,
                settled: false,
            });
            let _ = self.state.save();
        }
        if self.cfg.desktop_notifications {
            let title = card["title"].as_str().unwrap_or("Antigravity");
            let attention = t!("hook.needs_attention");
            let body = card["promptText"]
                .as_str()
                .or(card["message"].as_str())
                .unwrap_or(&attention);
            notify::show(title, body, self.cfg.settings.sound());
        }
        Ok(id)
    }

    /// The presence rule. Holds only when the user is away from the keyboard (while a hook
    /// holds, Antigravity shows no prompt of its own, so someone sitting there could not
    /// answer), and never in a headless run. `present_sends_info` sends the card as a plain
    /// note when nothing waits.
    fn blocking(
        &mut self,
        kind: &str,
        key: &str,
        card: Value,
        ceiling: u64,
        present_sends_info: bool,
        is_question: bool,
    ) -> Result<Hold> {
        if self.quiet() {
            return Ok(Hold::NotHeld);
        }
        let threshold = self.cfg.settings.presence_threshold();
        let idle = presence::idle_seconds();
        if ceiling == 0
            || !presence::hold_allowed(idle, threshold, self.cfg.hold_without_presence())
            || headless()
        {
            // At the keyboard, no way to tell (ssh, WSL, a desktop without an idle API), or
            // `agy -p`: nothing waits. The phone gets a note, unless the user is looking at
            // this very terminal and has asked for quiet then
            if present_sends_info && !self.quiet_at_terminal() {
                self.send_card(kind, key, card, false)?;
            }
            return Ok(Hold::NotHeld);
        }

        let Some(id) = self.send_card(kind, key, card, true)? else {
            return Ok(Hold::NotHeld);
        };
        log(&format!("holding {kind} {id} for up to {ceiling}s"));
        HOLDING_QUESTION.store(is_question, Ordering::SeqCst);
        HOLDING.store(true, Ordering::SeqCst);

        let deadline = Instant::now() + Duration::from_secs(ceiling);
        let (tx, rx) = mpsc::channel::<Option<String>>();
        let api = self.api.clone();
        let poll_id = id.clone();
        std::thread::spawn(move || {
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now()).as_secs();
                if remaining == 0 {
                    let _ = tx.send(None);
                    return;
                }
                match api.wait_response(&poll_id, remaining.clamp(1, 25)) {
                    Ok(Wait::Responded(text)) => {
                        let _ = tx.send(Some(text));
                        return;
                    }
                    Ok(Wait::Settled(status)) => {
                        log(&format!("{poll_id} settled as {status}"));
                        let _ = tx.send(None);
                        return;
                    }
                    Ok(Wait::Pending) => {}
                    Err(err) if api::is_rejected(&err) => {
                        let _ = tx.send(None);
                        return;
                    }
                    Err(err) => {
                        log(&format!("wait error: {err:#}"));
                        std::thread::sleep(Duration::from_secs(2));
                    }
                }
            }
        });

        loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(Some(text)) => {
                    if let Some(card) = &mut self.state.last_card {
                        card.settled = true;
                    }
                    let _ = self.state.save();
                    log(&format!("{id} answered from the phone"));
                    // Counted for the rating request (rate.rs), on a fresh read so concurrent
                    // hooks don't undo each other's count
                    let mut fresh = Config::load();
                    fresh.phone_answers += 1;
                    let _ = fresh.save();
                    return Ok(Hold::Answered(text));
                }
                Ok(None) => return Ok(Hold::Unanswered),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if presence::user_present(threshold) {
                        log(&format!("{id}: user returned to the keyboard"));
                        return Ok(Hold::Unanswered);
                    }
                    if Instant::now() >= deadline {
                        log(&format!("{id}: hold ceiling reached"));
                        return Ok(Hold::Unanswered);
                    }
                }
                // The polling thread died: the same as no answer
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(Hold::Unanswered),
            }
        }
    }
}

/// Where the time of the last failed best-effort update is kept. One file for every session,
/// since a server that is down is down for all of them.
fn backoff_path() -> std::path::PathBuf {
    config::app_dir().join("offline_at")
}

/// How long a best-effort update may take, given when the hook started and when the last one
/// failed. None skips it.
fn best_effort_timeout(elapsed: Duration, failed_at: Option<i64>, now: i64) -> Option<Duration> {
    if failed_at.is_some_and(|at| (0..BEST_EFFORT_BACKOFF).contains(&(now - at))) {
        return None;
    }
    let left = BEST_EFFORT_BUDGET.saturating_sub(elapsed);
    (left >= Duration::from_millis(100)).then(|| left.min(BEST_EFFORT_TIMEOUT))
}

/// Posts an update nothing waits on, within the budget, and remembers a failure so the next
/// hooks skip theirs for a while. A rejection still reaches `Api::was_rejected`.
fn best_effort_post(api: &Api, update: &Value, started: Instant) -> Result<Option<String>> {
    let path = backoff_path();
    let failed_at = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok());
    let Some(timeout) = best_effort_timeout(started.elapsed(), failed_at, config::now()) else {
        anyhow::bail!("skipped: the server was slow or unreachable a moment ago");
    };
    let result = api.post_event(update, timeout);
    match &result {
        Ok(_) if failed_at.is_some() => {
            let _ = std::fs::remove_file(&path);
        }
        Err(err) if !api::is_rejected(err) => {
            let _ = std::fs::write(&path, config::now().to_string());
        }
        _ => {}
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::fixtures;
    use serde_json::json;

    #[test]
    fn the_event_comes_from_the_command_line_then_the_payload_then_its_shape() {
        let run = fixtures::run_command();
        assert_eq!(event_of(Some("PreToolUse"), &Value::Null), Some("PreToolUse"));
        assert_eq!(event_of(Some("pretooluse"), &Value::Null), Some("PreToolUse"));
        assert_eq!(event_of(None, &run), Some("PreToolUse"));
        assert_eq!(event_of(Some("nonsense"), &run), Some("PreToolUse"));
        // The command line wins: hooks.json decides which hook this is
        assert_eq!(event_of(Some("Stop"), &run), Some("Stop"));
        // No name anywhere: the shape
        let strip = |mut v: Value| {
            v.as_object_mut().unwrap().remove("hookEventName");
            v
        };
        assert_eq!(event_of(None, &strip(run)), Some("PreToolUse"));
        assert_eq!(event_of(None, &strip(fixtures::post_tool_use())), Some("PostToolUse"));
        assert_eq!(event_of(None, &strip(fixtures::stop())), Some("Stop"));
        assert_eq!(event_of(None, &strip(fixtures::pre_invocation())), Some("PreInvocation"));
        assert_eq!(event_of(None, &strip(fixtures::session_start())), None);
        assert_eq!(event_of(None, &Value::Null), None);
        assert_eq!(event_of(None, &json!([1])), None);
    }

    #[test]
    fn nothing_to_say_is_allow_for_a_tool_call_and_empty_otherwise() {
        assert_eq!(fallback(Some("PreToolUse")), json!({ "decision": "allow" }));
        // An event nobody could name may be a tool call
        assert_eq!(fallback(None), json!({ "decision": "allow" }));
        for event in ["SessionStart", "PreInvocation", "PostToolUse", "Stop"] {
            assert_eq!(fallback(Some(event)), json!({}), "{event}");
        }
    }

    #[test]
    fn a_tool_call_answer_without_a_decision_never_gets_out() {
        let allow = json!({ "decision": "allow" });
        assert_eq!(checked(Some("PreToolUse"), json!({})), allow);
        assert_eq!(checked(Some("PreToolUse"), json!({ "decision": "" })), allow);
        assert_eq!(checked(Some("PreToolUse"), json!({ "decision": 3 })), allow);
        assert_eq!(checked(Some("PreToolUse"), json!(null)), allow);
        assert_eq!(checked(None, json!({})), allow);
        assert_eq!(
            checked(Some("PreToolUse"), json!({ "systemMessage": "hi" })),
            json!({ "decision": "allow", "systemMessage": "hi" })
        );
        let deny = json!({ "decision": "deny", "reason": "no" });
        assert_eq!(checked(Some("PreToolUse"), deny.clone()), deny);
        // Other events say what they like
        assert_eq!(checked(Some("Stop"), json!({})), json!({}));
        assert_eq!(
            checked(Some("SessionStart"), json!({ "systemMessage": "hi" })),
            json!({ "systemMessage": "hi" })
        );
    }

    #[test]
    fn the_watchdog_answers_inside_every_timeout() {
        // hooks.json gives each event its timeout; the watchdog's limits sit inside them
        for (event, _, timeout) in hookcfg::EVENTS {
            let (quick, holding) = deadlines(Some(event));
            let timeout = Duration::from_secs(*timeout);
            assert!(quick < timeout, "{event}");
            assert!(holding < timeout, "{event}");
        }
        // The longest hold fits before the watchdog would cut it short
        assert!(Duration::from_secs(hookcfg::MAX_TOOL_HOLD) + Duration::from_secs(60) <= PRE_TOOL_DEADLINE);
        assert!(Duration::from_secs(600) < STOP_DEADLINE);
        // What it says: the local prompt for an approval that was waiting, allow otherwise
        assert_eq!(timed_out_answer(Some("PreToolUse"), true, false)["decision"], "force_ask");
        assert_eq!(timed_out_answer(Some("PreToolUse"), true, true)["decision"], "allow");
        assert_eq!(timed_out_answer(Some("PreToolUse"), false, false)["decision"], "allow");
        assert_eq!(timed_out_answer(None, false, false)["decision"], "allow");
        assert_eq!(timed_out_answer(Some("Stop"), true, false), json!({}));
    }

    #[test]
    fn the_first_invocation_with_a_new_input_is_the_users_prompt() {
        let first = fixtures::pre_invocation();
        assert_eq!(first["invocationNum"], 0);
        let mut state = SessionState::default();
        assert!(new_prompt(&first, &state));
        // The same turn's later invocations, and a turn a Stop hook continued
        state.prompt = first["lastUserInput"].as_str().map(str::to_string);
        assert!(!new_prompt(&first, &state), "already seen");
        let mut later = first.clone();
        later["invocationNum"] = json!(2);
        later["lastUserInput"] = json!("something else");
        assert!(!new_prompt(&later, &state));
        // A different prompt
        let mut next = first.clone();
        next["lastUserInput"] = json!("And now the tests");
        assert!(new_prompt(&next, &state));
        // The same words again, after the turn stopped
        state.turn_stopped = true;
        assert!(new_prompt(&first, &state));
        // No invocation number: not a prompt we can vouch for
        assert!(!new_prompt(&json!({ "lastUserInput": "x" }), &SessionState::default()));
        assert!(!new_prompt(&Value::Null, &SessionState::default()));
    }

    #[test]
    fn reads_how_a_turn_stopped() {
        let stop = StopInfo::read(&fixtures::stop());
        assert_eq!(
            stop,
            StopInfo { execution: 0, fully_idle: true, cancelled: false, error: None }
        );
        assert!(stop.announce());
        // Background tasks still running: not yet
        let mut busy = fixtures::stop();
        busy["fullyIdle"] = json!(false);
        assert!(!StopInfo::read(&busy).announce());
        // An error with a message, and one with only a reason
        let mut failed = fixtures::stop();
        failed["error"] = json!("  RESOURCE_EXHAUSTED: quota exceeded  ");
        failed["terminationReason"] = json!("ERROR");
        failed["executionNum"] = json!(3);
        let info = StopInfo::read(&failed);
        assert_eq!(info.error.as_deref(), Some("RESOURCE_EXHAUSTED: quota exceeded"));
        assert_eq!(info.execution, 3);
        assert!(info.announce());
        let mut reason_only = fixtures::stop();
        reason_only["terminationReason"] = json!("max_steps_exceeded");
        assert_eq!(StopInfo::read(&reason_only).error.as_deref(), Some("MAX_STEPS_EXCEEDED"));
        // The user stopped it: nothing to announce, and not an error
        let mut cancelled = fixtures::stop();
        cancelled["terminationReason"] = json!("USER_CANCELLED");
        let info = StopInfo::read(&cancelled);
        assert!(info.cancelled && !info.announce() && info.error.is_none());
        // Input with nothing in it reads as an ordinary stop
        let empty = StopInfo::read(&Value::Null);
        assert!(empty.announce() && empty.error.is_none());
        assert_eq!(empty.execution, 0);
    }

    #[test]
    fn the_final_message_is_never_the_previous_turns() {
        let dir = std::env::temp_dir().join(format!("agn-final-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("transcript_full.jsonl");
        std::fs::write(&path, include_str!("../tests/fixtures/transcript_full.jsonl")).unwrap();
        let input = json!({ "transcriptPath": path.to_string_lossy() });
        let mut state = SessionState::default();
        let message = final_message(&input, &state).unwrap();
        assert_eq!(message.step, 9);
        // The next Stop finds nothing newer (a turn that ended in an error, say): no message
        state.done_step = Some(9);
        assert_eq!(final_message(&input, &state), None);
        // No transcript at all
        assert_eq!(final_message(&json!({}), &SessionState::default()), None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn recognises_agy_in_print_mode() {
        assert_eq!(print_mode_of("/opt/agy/agy -p hello --output-format json"), Some(true));
        assert_eq!(print_mode_of("agy --print=hello"), Some(true));
        assert_eq!(print_mode_of("agy --prompt hello"), Some(true));
        assert_eq!(print_mode_of("agy"), Some(false));
        assert_eq!(print_mode_of("agy --prompt-interactive hello"), Some(false));
        assert_eq!(print_mode_of("/usr/local/bin/agy -i fix --mode auto"), Some(false));
        assert_eq!(print_mode_of("/bin/sh -c /x/antigravity-notifier hook PreToolUse"), None);
        assert_eq!(print_mode_of("-zsh"), None);
        assert_eq!(print_mode_of(""), None);
    }

    #[test]
    fn the_servers_ccn_nudge_reads_as_antigravity() {
        let message = antigravity_wording(api::NudgeMessage {
            id: "none-b".into(),
            kind: "none".into(),
            variant: "b".into(),
            title: "Answer Claude from your phone, no subscription".into(),
            body: "Approvals from Claude Code reach you. Claude Code Notifier unlocks once.".into(),
            command: "ccn unlock --lifetime".into(),
        });
        assert_eq!(
            message.title,
            "Answer Antigravity from your phone, no subscription"
        );
        assert_eq!(
            message.body,
            "Approvals from Antigravity reach you. Antigravity Notifier unlocks once."
        );
        assert_eq!(message.command, "antigravity-notifier unlock --lifetime");
    }

    #[test]
    fn antigravity_wording_never_touches_the_ccn_suggestion() {
        // antigravity_wording would turn the line into nonsense about Antigravity...
        let line = crate::crosspromo::line();
        let rewritten = antigravity_wording(api::NudgeMessage {
            id: String::new(),
            kind: String::new(),
            variant: String::new(),
            title: String::new(),
            body: line.clone(),
            command: crate::crosspromo::command().into(),
        });
        assert_ne!(rewritten.body, line);
        // ...so the SessionStart message is the line as written, naming Claude Code and ccn
        let dir =
            std::env::temp_dir().join(format!("agn-hook-crosspromo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        let places = crate::crosspromo::Places {
            claude_dir: dir.join(".claude"),
            ccn_dir: dir.join("ccn"),
            path: None,
        };
        let mut cfg = Config::default();
        cfg.paired_at = Some(0);
        let shown = crate::crosspromo::session_line(&mut cfg, 10 * DAY, &places).unwrap();
        assert_eq!(shown, line);
        assert!(shown.contains("Claude Code approvals"), "{shown}");
        assert!(shown.contains(crate::crosspromo::command()), "{shown}");
        assert!(!shown.contains("Antigravity"), "{shown}");
    }

    #[test]
    fn best_effort_updates_share_a_budget_and_back_off() {
        let now = 1_700_000_000;
        let ms = Duration::from_millis;
        // Fresh hook, no recent failure: the per-update timeout
        assert_eq!(
            best_effort_timeout(ms(0), None, now),
            Some(BEST_EFFORT_TIMEOUT)
        );
        // A second update gets what is left of the budget
        assert_eq!(best_effort_timeout(ms(1100), None, now), Some(ms(400)));
        // Too little left: skip
        assert_eq!(best_effort_timeout(ms(1450), None, now), None);
        // A failure a moment ago skips, one over a minute ago doesn't
        assert_eq!(best_effort_timeout(ms(0), Some(now - 10), now), None);
        assert_eq!(
            best_effort_timeout(ms(0), Some(now - BEST_EFFORT_BACKOFF), now),
            Some(BEST_EFFORT_TIMEOUT)
        );
        // A clock that went backwards doesn't block updates for good
        assert_eq!(
            best_effort_timeout(ms(0), Some(now + 3600), now),
            Some(BEST_EFFORT_TIMEOUT)
        );
    }

    #[test]
    fn licence_nudges_on_day_two_and_six_then_weekly() {
        let paired = 1_700_000_000;
        let at = |day: i64| paired + day * DAY + 3600;
        // Days 0 and 1: nothing
        assert!(!licence_nudge_allowed(at(0), Some(paired), None));
        assert!(!licence_nudge_allowed(at(1), Some(paired), None));
        // Day 2 yes, day 3 no (even though never shown), day 6 yes
        assert!(licence_nudge_allowed(at(2), Some(paired), None));
        assert!(!licence_nudge_allowed(at(3), Some(paired), Some(at(2))));
        assert!(!licence_nudge_allowed(at(3), Some(paired), None));
        assert!(licence_nudge_allowed(at(6), Some(paired), Some(at(2))));
        // Weekly after that: day 7 is too soon after day 6, day 13 is a week on
        assert!(!licence_nudge_allowed(at(7), Some(paired), Some(at(6))));
        assert!(licence_nudge_allowed(at(13), Some(paired), Some(at(6))));
        assert!(!licence_nudge_allowed(at(19), Some(paired), Some(at(13))));
        assert!(licence_nudge_allowed(at(20), Some(paired), Some(at(13))));
        // Never shown and the early days missed: from day 7 on
        assert!(licence_nudge_allowed(at(9), Some(paired), None));
        // Never twice in a day, even on a nudge day
        assert!(!licence_nudge_allowed(
            at(2) + 60,
            Some(paired),
            Some(at(2))
        ));
        // No pairing date on record counts as long ago
        assert!(licence_nudge_allowed(at(0), None, None));
        assert!(!licence_nudge_allowed(at(0), None, Some(at(0) - 3 * DAY)));
    }
}
