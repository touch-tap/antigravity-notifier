//! The hook as Antigravity runs it: a process with the event on its command line and JSON on
//! stdin. Antigravity blocks the user's tool call when a PreToolUse hook exits non-zero, times
//! out or prints no decision, so the one thing every test here checks is that the hook exits 0
//! and, for a tool call, prints `{"decision": ...}`, whatever is wrong with its input, its
//! pairing, the network or the hook itself.

use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const RUN_COMMAND: &str = include_str!("fixtures/pre_tool_use_run_command.json");
const WRITE_FILE: &str = include_str!("fixtures/pre_tool_use_write_file.json");
const READ_FILE: &str = include_str!("fixtures/pre_tool_use_read_file.json");
const ASK_QUESTION: &str = include_str!("fixtures/pre_tool_use_ask_question.json");
const POST_TOOL_USE: &str = include_str!("fixtures/post_tool_use_write_file.json");
const PRE_INVOCATION: &str = include_str!("fixtures/pre_invocation.json");
const SESSION_START: &str = include_str!("fixtures/session_start.json");
const STOP: &str = include_str!("fixtures/stop.json");
const TRANSCRIPT: &str = include_str!("fixtures/transcript_full.jsonl");

struct Sandbox {
    root: PathBuf,
    env: Vec<(String, String)>,
}

impl Sandbox {
    /// A home with nothing in it: not paired, which is how most installs start
    fn new(name: &str) -> Sandbox {
        let root = std::env::temp_dir().join(format!(
            "antigravity-notifier-hook-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("home")).unwrap();
        Sandbox { root, env: vec![] }
    }

    /// Credentials for a server at `url`, with a phone paired, so the hooks use the network
    fn paired(name: &str, url: &str) -> Sandbox {
        let sandbox = Sandbox::new(name);
        std::fs::write(
            sandbox.root.join("home/config.toml"),
            format!("client_id = \"test-computer\"\nserver_url = \"{url}\"\nphones = 1\n"),
        )
        .unwrap();
        std::fs::write(sandbox.root.join("home/credentials"), "test-token\n").unwrap();
        sandbox
    }

    fn with(mut self, key: &str, value: &str) -> Sandbox {
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    fn hook(&self, args: &[&str], stdin: &[u8]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_antigravity-notifier"));
        command
            .arg("hook")
            .args(args)
            .env("HOME", &self.root)
            .env("ANTIGRAVITY_NOTIFIER_HOME", self.root.join("home"))
            .env("ANTIGRAVITY_NOTIFIER_GEMINI_HOME", self.root.join("gemini"))
            .env("ANTIGRAVITY_NOTIFIER_CREDENTIAL_STORE", "file")
            .env("ANTIGRAVITY_NOTIFIER_NOTIFY", "off")
            .env("ANTIGRAVITY_NOTIFIER_NO_ANALYTICS", "1")
            .env("ANTIGRAVITY_NOTIFIER_NO_AUTO_UPDATE", "1")
            .env("ANTIGRAVITY_NOTIFIER_PRESENCE", "present")
            .env("ANTIGRAVITY_NOTIFIER_HEADLESS", "0")
            .env("ANTIGRAVITY_NOTIFIER_LANG", "en")
            .env_remove("ANTIGRAVITY_NOTIFIER_SERVER_URL")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &self.env {
            command.env(key, value);
        }
        let mut child = command.spawn().unwrap();
        // The hook may answer and exit before it has read everything
        let _ = child.stdin.take().unwrap().write_all(stdin);
        child.wait_with_output().unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The hook's answer: exit 0, exactly one line of JSON on stdout.
fn answer(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "exit {:?}, stderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "one line of output, got: {stdout:?}");
    serde_json::from_str(lines[0]).unwrap_or_else(|err| panic!("{err}: {stdout:?}"))
}

fn allow() -> Value {
    json!({ "decision": "allow" })
}

/// A port nothing listens on
const DEAD_SERVER: &str = "http://127.0.0.1:9";

#[test]
fn an_unpaired_computer_never_blocks_a_tool_call() {
    let sandbox = Sandbox::new("unpaired");
    for input in [RUN_COMMAND, WRITE_FILE, READ_FILE, ASK_QUESTION] {
        assert_eq!(answer(&sandbox.hook(&["PreToolUse"], input.as_bytes())), allow());
    }
    // Away from the keyboard changes nothing without a pairing
    let away = Sandbox::new("unpaired-away").with("ANTIGRAVITY_NOTIFIER_PRESENCE", "away");
    assert_eq!(answer(&away.hook(&["PreToolUse"], RUN_COMMAND.as_bytes())), allow());
}

#[test]
fn input_that_cannot_be_read_is_still_answered_with_allow() {
    let sandbox = Sandbox::new("malformed");
    let huge = format!("{{\"toolCall\":{{\"name\":\"run_command\",\"args\":{{\"CommandLine\":\"{}\"}}}}}}", "x".repeat(2_000_000));
    let nested = "[".repeat(5000);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", vec![]),
        ("whitespace", b"  \n".to_vec()),
        ("not json", b"hello there".to_vec()),
        ("cut short", RUN_COMMAND.as_bytes()[..RUN_COMMAND.len() / 2].to_vec()),
        ("not utf-8", vec![0xff, 0xfe, 0x00, 0x80, b'{']),
        ("null", b"null".to_vec()),
        ("an array", b"[1,2,3]".to_vec()),
        ("a string", b"\"PreToolUse\"".to_vec()),
        ("empty object", b"{}".to_vec()),
        ("wrong types", br#"{"toolCall":7,"conversationId":[],"cwd":{},"workspacePaths":"x","lastUserInput":null}"#.to_vec()),
        ("tool call without a name", br#"{"toolCall":{"args":{"CommandLine":"ls"}}}"#.to_vec()),
        ("args not an object", br#"{"toolCall":{"name":"run_command","args":"ls"}}"#.to_vec()),
        ("questions not a list", br#"{"toolCall":{"name":"ask_question","args":{"questions":"what?"}}}"#.to_vec()),
        ("unknown tool", br#"{"conversationId":"c1","toolCall":{"name":"teleport_user","args":{"to":"mars"}}}"#.to_vec()),
        ("mcp tool", br#"{"conversationId":"c1","toolCall":{"name":"mcp_github_create_issue","args":{"title":"x"}}}"#.to_vec()),
        ("odd conversation id", br#"{"conversationId":"../../etc/passwd","toolCall":{"name":"run_command","args":{}}}"#.to_vec()),
        ("byte order mark", format!("\u{feff}{RUN_COMMAND}").into_bytes()),
        ("huge", huge.into_bytes()),
        ("deeply nested", nested.into_bytes()),
    ];
    for (name, input) in &cases {
        let output = sandbox.hook(&["PreToolUse"], input);
        assert_eq!(answer(&output), allow(), "{name}");
    }
}

#[test]
fn the_event_can_come_from_the_payload_and_an_unknown_one_is_allowed() {
    let sandbox = Sandbox::new("event");
    // No argument: the payload's hookEventName
    assert_eq!(answer(&sandbox.hook(&[], RUN_COMMAND.as_bytes())), allow());
    // Nothing names the event, and the input doesn't look like one: allow, in case it is a tool call
    assert_eq!(answer(&sandbox.hook(&[], b"")), allow());
    assert_eq!(answer(&sandbox.hook(&["SomethingNew"], b"{}")), allow());
    // Extra arguments and option-like ones must not reach an argument parser that exits 2
    assert_eq!(
        answer(&sandbox.hook(&["PreToolUse", "--bogus", "-x", "extra"], RUN_COMMAND.as_bytes())),
        allow()
    );
    assert_eq!(answer(&sandbox.hook(&["--help"], RUN_COMMAND.as_bytes())), allow());
}

#[test]
fn other_events_answer_with_an_empty_object() {
    let sandbox = Sandbox::new("others");
    for (event, input) in [
        ("SessionStart", SESSION_START),
        ("PreInvocation", PRE_INVOCATION),
        ("PostToolUse", POST_TOOL_USE),
        ("PostInvocation", PRE_INVOCATION),
        ("Stop", STOP),
    ] {
        assert_eq!(answer(&sandbox.hook(&[event], input.as_bytes())), json!({}), "{event}");
        assert_eq!(answer(&sandbox.hook(&[event], b"garbage")), json!({}), "{event} garbage");
        assert_eq!(answer(&sandbox.hook(&[event], b"")), json!({}), "{event} empty");
    }
}

#[test]
fn a_panic_in_the_hook_still_answers_allow_and_exits_zero() {
    let sandbox = Sandbox::new("panic").with("ANTIGRAVITY_NOTIFIER_TEST_FAULT", "panic");
    let output = sandbox.hook(&["PreToolUse"], RUN_COMMAND.as_bytes());
    assert_eq!(answer(&output), allow());
    assert!(output.stderr.is_empty(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(answer(&sandbox.hook(&["Stop"], STOP.as_bytes())), json!({}));
    assert_eq!(answer(&sandbox.hook(&[], b"")), allow());
}

#[test]
fn a_hook_that_hangs_is_answered_by_the_watchdog() {
    let stalled = Sandbox::new("stall")
        .with("ANTIGRAVITY_NOTIFIER_TEST_FAULT", "stall")
        .with("ANTIGRAVITY_NOTIFIER_TEST_DEADLINE_MS", "300");
    let started = Instant::now();
    assert_eq!(answer(&stalled.hook(&["PreToolUse"], RUN_COMMAND.as_bytes())), allow());
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(answer(&stalled.hook(&["Stop"], STOP.as_bytes())), json!({}));

    // Stuck while waiting for the phone: the local prompt takes over
    let holding = Sandbox::new("stall-holding")
        .with("ANTIGRAVITY_NOTIFIER_TEST_FAULT", "stall_holding")
        .with("ANTIGRAVITY_NOTIFIER_TEST_DEADLINE_MS", "300");
    let output = answer(&holding.hook(&["PreToolUse"], RUN_COMMAND.as_bytes()));
    assert_eq!(output["decision"], "force_ask");
    assert_eq!(output["reason"], "No answer from your phone");
}

#[test]
fn stdin_that_never_closes_does_not_hang_the_tool_call() {
    let sandbox = Sandbox::new("open-stdin");
    let mut child = Command::new(env!("CARGO_BIN_EXE_antigravity-notifier"))
        .args(["hook", "PreToolUse"])
        .env("HOME", &sandbox.root)
        .env("ANTIGRAVITY_NOTIFIER_HOME", sandbox.root.join("home"))
        .env("ANTIGRAVITY_NOTIFIER_NOTIFY", "off")
        .env("ANTIGRAVITY_NOTIFIER_TEST_DEADLINE_MS", "300")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Held open, with nothing written, until the hook has answered by itself
    let stdin = child.stdin.take().unwrap();
    let started = Instant::now();
    let output = child.wait_with_output().unwrap();
    drop(stdin);
    assert_eq!(answer(&output), allow());
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn a_paired_computer_with_the_user_present_allows_at_once_without_the_network() {
    // The server is unreachable; with the user at the keyboard nothing needs it
    let sandbox = Sandbox::paired("present", DEAD_SERVER);
    let started = Instant::now();
    for input in [RUN_COMMAND, WRITE_FILE, READ_FILE] {
        assert_eq!(answer(&sandbox.hook(&["PreToolUse"], input.as_bytes())), allow());
    }
    assert!(started.elapsed() < Duration::from_secs(15), "{:?}", started.elapsed());
}

#[test]
fn a_server_that_cannot_be_reached_hands_the_call_back_to_the_terminal() {
    let sandbox =
        Sandbox::paired("away-offline", DEAD_SERVER).with("ANTIGRAVITY_NOTIFIER_PRESENCE", "away");
    // The approval card can't be sent. The engine never answers for the user: the command goes to
    // Antigravity's own prompt, which says why, and nothing waits
    let started = Instant::now();
    let command = answer(&sandbox.hook(&["PreToolUse"], RUN_COMMAND.as_bytes()));
    assert_eq!(command["decision"], "force_ask");
    assert_eq!(command["reason"], "Couldn't reach your phone");
    // A file write isn't sent to the phone by default (phone_approvals = commands): Antigravity decides
    assert_eq!(answer(&sandbox.hook(&["PreToolUse"], WRITE_FILE.as_bytes())), allow());
    // With phone_approvals = all it is, so it is handed back too
    let all = Sandbox::paired("away-offline-all", DEAD_SERVER).with("ANTIGRAVITY_NOTIFIER_PRESENCE", "away");
    let mut config = std::fs::read_to_string(all.root.join("home/config.toml")).unwrap();
    config.push_str("phone_approvals = \"all\"\n");
    std::fs::write(all.root.join("home/config.toml"), config).unwrap();
    assert_eq!(answer(&all.hook(&["PreToolUse"], WRITE_FILE.as_bytes()))["decision"], "force_ask");
    // A question is never answered for the user either: it shows in the terminal
    assert_eq!(answer(&sandbox.hook(&["PreToolUse"], ASK_QUESTION.as_bytes())), allow());
    assert_eq!(answer(&sandbox.hook(&["PreToolUse"], READ_FILE.as_bytes())), allow());
    assert_eq!(answer(&sandbox.hook(&["Stop"], STOP.as_bytes())), json!({}));
    assert_eq!(answer(&sandbox.hook(&["SessionStart"], SESSION_START.as_bytes())), json!({}));
    assert!(started.elapsed() < Duration::from_secs(40), "{:?}", started.elapsed());
}

#[test]
fn a_headless_run_is_never_held_even_when_the_user_is_away() {
    let sandbox = Sandbox::paired("headless", DEAD_SERVER)
        .with("ANTIGRAVITY_NOTIFIER_PRESENCE", "away")
        .with("ANTIGRAVITY_NOTIFIER_HEADLESS", "1");
    assert_eq!(answer(&sandbox.hook(&["PreToolUse"], RUN_COMMAND.as_bytes())), allow());
    assert_eq!(answer(&sandbox.hook(&["Stop"], STOP.as_bytes())), json!({}));
}

#[test]
fn a_whole_turn_keeps_its_state_and_stop_reads_the_transcript() {
    let sandbox = Sandbox::new("turn");
    // The transcript where the Stop payload says it is
    let transcript = sandbox.root.join("transcript_full.jsonl");
    std::fs::write(&transcript, TRANSCRIPT).unwrap();
    let with_transcript = |text: &str| {
        let mut input: Value = serde_json::from_str(text).unwrap();
        input["transcriptPath"] = json!(transcript.to_string_lossy());
        input["conversationId"] = json!("turn-1");
        input.to_string()
    };
    for (event, input) in [
        ("SessionStart", SESSION_START),
        ("PreInvocation", PRE_INVOCATION),
        ("PreToolUse", WRITE_FILE),
        ("PostToolUse", POST_TOOL_USE),
        ("Stop", STOP),
    ] {
        let output = answer(&sandbox.hook(&[event], with_transcript(input).as_bytes()));
        let expected = if event == "PreToolUse" { allow() } else { json!({}) };
        assert_eq!(output, expected, "{event}");
    }
    let state: Value = serde_json::from_str(
        &std::fs::read_to_string(sandbox.root.join("home/sessions/turn-1.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(state["cwd"], "/Users/dev/projects/demo");
    assert_eq!(state["tool_name"], "write_file");
    assert_eq!(state["turn_stopped"], true);
    assert_eq!(state["done_execution"], 0);
    assert!(state["prompt"].as_str().unwrap().starts_with("Run the shell command"));
    // The same Stop again (a second hook group, a retry) is not announced twice
    let again = answer(&sandbox.hook(&["Stop"], with_transcript(STOP).as_bytes()));
    assert_eq!(again, json!({}));
    let log = std::fs::read_to_string(sandbox.root.join("home/antigravity-notifier.log")).unwrap();
    assert!(log.contains("not paired yet"), "{log}");
}

// ============================================
// The walk-away watcher
// ============================================

use std::io::Read;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// A server that accepts every event and keeps what it was sent.
struct Recorder {
    url: String,
    bodies: Arc<Mutex<Vec<String>>>,
}

impl Recorder {
    fn start() -> Recorder {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let seen = bodies.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let request = read_request(&mut stream);
                seen.lock().unwrap().push(request);
                let body = r#"{"notificationId":"n1"}"#;
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Write);
            }
        });
        Recorder { url, bodies }
    }

    /// The walk-away pushes received so far
    fn alerts(&self) -> Vec<String> {
        self.bodies
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.contains("may be waiting for you"))
            .cloned()
            .collect()
    }

    /// Waits until `count` alerts have arrived, or `within` has passed
    fn wait_for_alerts(&self, count: usize, within: Duration) -> usize {
        let deadline = Instant::now() + within;
        while self.alerts().len() < count && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        self.alerts().len()
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let text = String::from_utf8_lossy(&data).to_string();
        if let Some(end) = text.find("\r\n\r\n") {
            let length = text[..end]
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            if data.len() >= end + 4 + length {
                return text;
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return String::from_utf8_lossy(&data).to_string(),
            Ok(n) => data.extend_from_slice(&chunk[..n]),
        }
    }
}

/// A paired computer whose presence a test can change while the watcher runs, with the
/// watcher's clock shortened: it looks every 40 ms and alerts after 400 ms away.
struct Watched {
    sandbox: Sandbox,
    server: Recorder,
    presence: PathBuf,
}

impl Watched {
    fn new(name: &str) -> Watched {
        let server = Recorder::start();
        let sandbox = Sandbox::paired(name, &server.url);
        let presence = sandbox.root.join("presence");
        std::fs::write(&presence, "present").unwrap();
        let sandbox = sandbox
            .with("ANTIGRAVITY_NOTIFIER_PRESENCE", &format!("file:{}", presence.display()))
            .with("ANTIGRAVITY_NOTIFIER_TEST_WATCH_MS", "40,400,20000");
        Watched { sandbox, server, presence }
    }

    fn user_is(&self, state: &str) {
        std::fs::write(&self.presence, state).unwrap();
    }

    fn note(&self) -> PathBuf {
        self.sandbox
            .root
            .join("home/watch/32fc0aec-d58e-47c7-b08c-f489e3a4644a.json")
    }

    /// PreToolUse for `run_command` with this command and call id: allowed at once
    fn tool_call(&self, command: &str, id: &str) {
        let mut input: Value = serde_json::from_str(RUN_COMMAND).unwrap();
        input["toolCall"]["args"]["CommandLine"] = json!(command);
        input["toolCall"]["id"] = json!(id);
        let started = Instant::now();
        let output = self.sandbox.hook(&["PreToolUse"], input.to_string().as_bytes());
        assert_eq!(answer(&output), allow());
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    }

    fn tool_result(&self, id: &str) {
        let mut input: Value = serde_json::from_str(RUN_COMMAND).unwrap();
        input["hookEventName"] = json!("PostToolUse");
        input["error"] = json!("");
        input["toolCall"]["id"] = json!(id);
        assert_eq!(
            answer(&self.sandbox.hook(&["PostToolUse"], input.to_string().as_bytes())),
            json!({})
        );
    }

    /// Long enough for the watcher to have alerted several times over, had it meant to
    fn settle(&self) {
        std::thread::sleep(Duration::from_millis(1200));
    }
}

#[test]
fn the_watcher_ends_when_the_tool_call_finishes() {
    let w = Watched::new("watch-finished");
    w.tool_call("npm publish", "call_a");
    assert!(w.note().exists(), "the hook left a note for the watcher");
    // A result for some other call changes nothing; this call's result ends the watch
    w.tool_result("call_other");
    assert!(w.note().exists());
    w.tool_result("call_a");
    assert!(!w.note().exists());
    w.user_is("away");
    w.settle();
    assert_eq!(w.server.alerts().len(), 0);
}

#[test]
fn the_watcher_ends_on_the_next_invocation_and_on_stop() {
    for (event, input) in [("PreInvocation", PRE_INVOCATION), ("Stop", STOP)] {
        let w = Watched::new(&format!("watch-{event}"));
        w.tool_call("npm publish", "call_a");
        assert!(w.note().exists());
        assert_eq!(answer(&w.sandbox.hook(&[event], input.as_bytes())), json!({}));
        assert!(!w.note().exists(), "{event}");
        w.user_is("away");
        w.settle();
        assert_eq!(w.server.alerts().len(), 0, "{event}");
    }
}

#[test]
fn walking_away_from_an_open_call_sends_one_push_after_the_grace() {
    let w = Watched::new("watch-alert");
    w.tool_call("npm publish", "call_a");
    // Still there: nothing, however long the command takes
    w.settle();
    assert_eq!(w.server.alerts().len(), 0);
    let left_at = Instant::now();
    w.user_is("away");
    assert_eq!(w.server.wait_for_alerts(1, Duration::from_secs(10)), 1);
    assert!(left_at.elapsed() >= Duration::from_millis(400), "not before the grace");
    let alert = &w.server.alerts()[0];
    assert!(alert.contains("npm publish hasn't finished. If it needs approval, that's at your computer."), "{alert}");
    let body: Value = serde_json::from_str(alert.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["terminalContext"]["supportsRemoteInput"], false);
    assert_eq!(body["message"], "Antigravity may be waiting for you");
    assert_eq!(body["title"], "Antigravity - demo");
    assert!(body.get("options").is_none() && body.get("promptType").is_none(), "no buttons: {body}");
    assert!(alert.to_ascii_lowercase().contains("x-app: antigravity"), "{alert}");
    // Never a second one for the same call, and the note is gone
    w.settle();
    assert_eq!(w.server.alerts().len(), 1);
    assert!(!w.note().exists());
    // Its late result is harmless
    w.tool_result("call_a");
    assert_eq!(w.server.alerts().len(), 1);
}

#[test]
fn a_locked_screen_counts_as_walking_away() {
    let w = Watched::new("watch-locked");
    w.tool_call("npm publish", "call_a");
    w.user_is("locked");
    assert_eq!(w.server.wait_for_alerts(1, Duration::from_secs(10)), 1);
}

#[test]
fn coming_back_within_the_grace_sends_nothing() {
    let w = Watched::new("watch-back");
    w.tool_call("npm publish", "call_a");
    w.user_is("away");
    std::thread::sleep(Duration::from_millis(150));
    w.user_is("present");
    w.settle();
    assert_eq!(w.server.alerts().len(), 0);
    assert!(w.note().exists(), "still watching");
    // Presence that can't be read is not "away" either
    w.user_is("unknown");
    w.settle();
    assert_eq!(w.server.alerts().len(), 0);
    // Away for good: the grace starts again from here
    w.user_is("away");
    assert_eq!(w.server.wait_for_alerts(1, Duration::from_secs(10)), 1);
}

#[test]
fn a_newer_call_replaces_the_watch_and_only_it_can_alert() {
    let w = Watched::new("watch-replaced");
    w.tool_call("npm test", "call_a");
    w.tool_call("npm publish", "call_b");
    // The first call's result no longer matters
    w.tool_result("call_a");
    assert!(w.note().exists());
    w.user_is("away");
    assert_eq!(w.server.wait_for_alerts(1, Duration::from_secs(10)), 1);
    w.settle();
    let alerts = w.server.alerts();
    assert_eq!(alerts.len(), 1, "one watcher, one push");
    assert!(alerts[0].contains("npm publish hasn't finished"), "{}", alerts[0]);
}

#[test]
fn nothing_is_watched_when_it_should_not_be() {
    // Turned off
    let off = Watched::new("watch-off");
    let config = off.sandbox.root.join("home/config.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    std::fs::write(&config, format!("walkaway_alerts = false\n{text}")).unwrap();
    off.tool_call("npm publish", "call_a");
    assert!(!off.note().exists());

    // A file edit doesn't wait for the phone by default, so it isn't watched either
    let edit = Watched::new("watch-edit");
    let mut input: Value = serde_json::from_str(WRITE_FILE).unwrap();
    input["conversationId"] = json!("32fc0aec-d58e-47c7-b08c-f489e3a4644a");
    assert_eq!(answer(&edit.sandbox.hook(&["PreToolUse"], input.to_string().as_bytes())), allow());
    assert!(!edit.note().exists());

    // Presence unknown: there would be no telling when they left
    let unknown = Watched::new("watch-unknown");
    unknown.user_is("unknown");
    unknown.tool_call("npm publish", "call_a");
    assert!(!unknown.note().exists());

    // `agy -p`: nobody is shown a prompt
    let headless = Watched::new("watch-headless");
    let headless = Watched {
        sandbox: headless.sandbox.with("ANTIGRAVITY_NOTIFIER_HEADLESS", "1"),
        server: headless.server,
        presence: headless.presence,
    };
    headless.tool_call("npm publish", "call_a");
    assert!(!headless.note().exists());

    // Not paired
    let unpaired = Sandbox::new("watch-unpaired");
    assert_eq!(answer(&unpaired.hook(&["PreToolUse"], RUN_COMMAND.as_bytes())), allow());
    assert!(!unpaired.root.join("home/watch").exists());

    for w in [&off, &edit, &unknown, &headless] {
        w.user_is("away");
    }
    off.settle();
    for w in [&off, &edit, &unknown, &headless] {
        assert_eq!(w.server.alerts().len(), 0);
    }
}
