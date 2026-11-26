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
fn a_server_that_cannot_be_reached_never_blocks_an_away_user() {
    let sandbox =
        Sandbox::paired("away-offline", DEAD_SERVER).with("ANTIGRAVITY_NOTIFIER_PRESENCE", "away");
    // The approval card can't be sent, so nothing waits: Antigravity decides by itself
    let started = Instant::now();
    assert_eq!(answer(&sandbox.hook(&["PreToolUse"], RUN_COMMAND.as_bytes())), allow());
    assert_eq!(answer(&sandbox.hook(&["PreToolUse"], WRITE_FILE.as_bytes())), allow());
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
