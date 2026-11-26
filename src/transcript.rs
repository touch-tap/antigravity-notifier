//! Reads Antigravity's final message out of a conversation's transcript. A Stop hook's input has
//! no message in it, only `transcriptPath`; the message is the last line of
//! `transcript_full.jsonl` whose `type` is `PLANNER_RESPONSE` and whose `content` is not empty
//! (measured on agy 1.5.18: one JSON object per line, keys step_index, source, type, content,
//! tool_calls).
//!
//! A long session's transcript runs to megabytes and this runs inside a hook, so the file is
//! read from its end, a piece at a time. Antigravity may be appending as we read: a last line
//! that is cut short, and a first line that starts mid-way through the piece, simply don't parse
//! and are skipped.

use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const FULL_NAME: &str = "transcript_full.jsonl";
/// How much of the file's end is read: first this much, then more if no message was in it
const TAIL_SIZES: &[u64] = &[64 * 1024, 1024 * 1024, 8 * 1024 * 1024];

/// Antigravity's last message and the step it was.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub step: i64,
    pub text: String,
}

/// The transcript file for a hook's input: `transcriptPath` when it names the full transcript,
/// the full transcript beside it when it names another file (the short `transcript.jsonl`) or
/// in it when it names a folder, and otherwise the usual place under `artifactDirectoryPath`.
pub fn path_for(input: &Value) -> Option<PathBuf> {
    let text = |key: &str| {
        input
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };
    if let Some(given) = text("transcriptPath").map(PathBuf::from) {
        if given.is_dir() {
            return Some(given.join(FULL_NAME));
        }
        if given.file_name().is_some_and(|name| name == FULL_NAME) {
            return Some(given);
        }
        let beside = given.with_file_name(FULL_NAME);
        return Some(if beside.is_file() { beside } else { given });
    }
    text("artifactDirectoryPath").map(|dir| {
        Path::new(dir)
            .join(".system_generated")
            .join("logs")
            .join(FULL_NAME)
    })
}

/// The last message in the transcript at `path`, or None when there is none or the file can't
/// be read.
pub fn last_message(path: &Path) -> Option<Message> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    for size in TAIL_SIZES {
        let start = len.saturating_sub(*size);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut bytes = Vec::new();
        (&mut file).take(*size).read_to_end(&mut bytes).ok()?;
        if let Some(message) = last_in(&bytes) {
            return Some(message);
        }
        if start == 0 {
            break;
        }
    }
    None
}

fn last_in(bytes: &[u8]) -> Option<Message> {
    bytes
        .split(|b| *b == b'\n')
        .rev()
        .find_map(|line| message_of(serde_json::from_slice::<Value>(line).ok()?))
}

fn message_of(line: Value) -> Option<Message> {
    if line.get("type")?.as_str()? != "PLANNER_RESPONSE" {
        return None;
    }
    let text = line.get("content")?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    Some(Message {
        step: line.get("step_index").and_then(|v| v.as_i64()).unwrap_or(-1),
        text: text.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agn-transcript-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A real transcript from agy 1.5.18 (26 Nov 2026), with the home folder renamed
    const REAL: &str = include_str!("../tests/fixtures/transcript_full.jsonl");

    #[test]
    fn reads_the_final_message_of_a_real_transcript() {
        let dir = scratch("real");
        let path = dir.join(FULL_NAME);
        std::fs::write(&path, REAL).unwrap();
        let message = last_message(&path).unwrap();
        assert_eq!(message.step, 9);
        assert!(message.text.starts_with("Created [notes.txt]"), "{}", message.text);
        assert!(message.text.ends_with("```"), "{}", message.text);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn skips_tool_calls_other_lines_and_a_line_cut_short() {
        let dir = scratch("partial");
        let path = dir.join(FULL_NAME);
        let lines = [
            json!({ "step_index": 0, "type": "USER_INPUT", "content": "do it" }).to_string(),
            json!({ "step_index": 1, "type": "PLANNER_RESPONSE", "content": "First answer." }).to_string(),
            json!({ "step_index": 2, "type": "PLANNER_RESPONSE", "tool_calls": [{ "name": "run_command" }] }).to_string(),
            json!({ "step_index": 3, "type": "GENERIC", "content": "Command exited with code 0." }).to_string(),
            json!({ "step_index": 4, "type": "PLANNER_RESPONSE", "content": "  The real answer.\n" }).to_string(),
            json!({ "step_index": 5, "type": "PLANNER_RESPONSE", "content": "   " }).to_string(),
            json!({ "step_index": 6, "type": "SYSTEM_MESSAGE", "content": "reminder" }).to_string(),
            "not json at all".to_string(),
            "[1, 2]".to_string(),
            // Antigravity is still writing this one
            r#"{"step_index":7,"type":"PLANNER_RESPONSE","content":"Half a mess"#.to_string(),
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();
        assert_eq!(
            last_message(&path),
            Some(Message { step: 4, text: "The real answer.".into() })
        );
        // Windows line endings and a trailing newline
        std::fs::write(&path, lines[..5].join("\r\n") + "\r\n").unwrap();
        assert_eq!(last_message(&path).unwrap().step, 4);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn finds_a_message_far_from_the_end_of_a_large_file() {
        let dir = scratch("large");
        let path = dir.join(FULL_NAME);
        let mut file = File::create(&path).unwrap();
        writeln!(file, "{}", json!({ "step_index": 1, "type": "PLANNER_RESPONSE", "content": "Early." })).unwrap();
        writeln!(file, "{}", json!({ "step_index": 2, "type": "PLANNER_RESPONSE", "content": "The one." })).unwrap();
        // 3 MB of tool output after it: past the first two tail sizes
        let filler = json!({ "step_index": 3, "type": "GENERIC", "content": "x".repeat(1000) }).to_string();
        for _ in 0..3000 {
            writeln!(file, "{filler}").unwrap();
        }
        drop(file);
        assert!(std::fs::metadata(&path).unwrap().len() > 2 * 1024 * 1024);
        assert_eq!(last_message(&path).unwrap().text, "The one.");
        // And one near the end is found in the first small read
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{}", json!({ "step_index": 9, "type": "PLANNER_RESPONSE", "content": "Latest." })).unwrap();
        assert_eq!(last_message(&path).unwrap().text, "Latest.");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn nothing_to_read_is_none() {
        let dir = scratch("none");
        assert_eq!(last_message(&dir.join("missing.jsonl")), None);
        assert_eq!(last_message(&dir), None);
        let path = dir.join(FULL_NAME);
        std::fs::write(&path, "").unwrap();
        assert_eq!(last_message(&path), None);
        std::fs::write(&path, [0xff, 0xfe, b'\n', 0x00]).unwrap();
        assert_eq!(last_message(&path), None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn finds_the_full_transcript_from_the_hook_input() {
        let dir = scratch("path");
        let logs = dir.join(".system_generated/logs");
        std::fs::create_dir_all(&logs).unwrap();
        let full = logs.join(FULL_NAME);
        std::fs::write(&full, "").unwrap();
        let short = logs.join("transcript.jsonl");
        std::fs::write(&short, "").unwrap();
        let input = |path: &Path| json!({ "transcriptPath": path.to_string_lossy() });
        assert_eq!(path_for(&input(&full)), Some(full.clone()));
        assert_eq!(path_for(&input(&short)), Some(full.clone()));
        assert_eq!(path_for(&input(&logs)), Some(full.clone()));
        // A path to a full transcript that isn't there yet is kept as given
        let absent = dir.join("elsewhere").join(FULL_NAME);
        assert_eq!(path_for(&input(&absent)), Some(absent));
        assert_eq!(
            path_for(&json!({ "artifactDirectoryPath": dir.to_string_lossy() })),
            Some(full)
        );
        assert_eq!(path_for(&json!({ "transcriptPath": "" })), None);
        assert_eq!(path_for(&json!({ "transcriptPath": 4 })), None);
        assert_eq!(path_for(&json!(null)), None);
        std::fs::remove_dir_all(dir).ok();
    }
}
