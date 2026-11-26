//! What the phone's lock-screen card needs to know while Antigravity works: the current step,
//! and a receipt when it stops. Built from what hooks see, with nothing guessed: a number we
//! cannot derive is left out of the message.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;

use crate::payload::{self, ToolClass};
use crate::session::SessionState;

/// At most one working update per session in this many seconds
pub const WORKING_EVERY: i64 = 10;
/// A file larger than this is not read to count the lines a write replaces
const MAX_COUNTED_FILE: u64 = 2 * 1024 * 1024;
/// Edits waiting for their PostToolUse. More than this means their results never came.
const MAX_PENDING: usize = 32;

/// What this turn has done so far. Reset when the user sends a prompt.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Turn {
    pub started_at: i64,
    pub working_sent_at: i64,
    pub files: Vec<String>,
    pub approvals: u32,
    pub added: u64,
    pub removed: u64,
    /// A write replaced a file whose old contents could not be read, so added/removed would be
    /// a guess
    pub diff_unknown: bool,
    /// Edits a PreToolUse has seen and no PostToolUse has confirmed yet
    pub pending: Vec<Edit>,
}

/// What one `write_file` or `edit_file` call is about to change.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Edit {
    /// The tool call's id, which its PostToolUse repeats
    pub id: String,
    pub file: String,
    pub added: u64,
    pub removed: u64,
    pub unknown: bool,
}

/// Above this many cell comparisons the middle of a diff is counted whole instead of compared
const MAX_DIFF_CELLS: usize = 4_000_000;

/// Lines added and removed going from `old` to `new`, as a line diff would count them: the
/// lines the two share, in order, are unchanged. The shared start and end are set aside first;
/// a middle too large to compare cheaply inside a hook counts as all removed and all added.
pub fn line_delta(old: &str, new: &str) -> (u64, u64) {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let head = old
        .iter()
        .zip(new.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let tail = old[head..]
        .iter()
        .rev()
        .zip(new[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let old = &old[head..old.len() - tail];
    let new = &new[head..new.len() - tail];
    let common = if old.is_empty() || new.is_empty() {
        0
    } else if old.len().saturating_mul(new.len()) > MAX_DIFF_CELLS {
        0
    } else {
        common_lines(old, new)
    };
    ((new.len() - common) as u64, (old.len() - common) as u64)
}

/// The length of the longest run of lines, not necessarily adjacent, that both have in order.
fn common_lines(old: &[&str], new: &[&str]) -> usize {
    let mut row = vec![0usize; new.len() + 1];
    for a in old {
        let mut diagonal = 0;
        for (j, b) in new.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if a == b {
                diagonal + 1
            } else {
                above.max(row[j])
            };
            diagonal = above;
        }
    }
    row[new.len()]
}

/// The edit a tool call is about to make, from its arguments: `write_file {Path, Content}` and
/// `edit_file {Path, Target, Replacement}` (and the names Google's docs give them). Called
/// before the tool runs, so a file that `write_file` replaces can still be read. None for a
/// tool that edits nothing.
pub fn edit_of(tool_name: &str, input: &Value, id: &str) -> Option<Edit> {
    if payload::classify(tool_name) != ToolClass::Edit {
        return None;
    }
    let text = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| input.get(*key).and_then(|v| v.as_str()))
    };
    let mut edit = Edit {
        id: id.to_string(),
        file: payload::file_path(input).unwrap_or_default(),
        ..Edit::default()
    };
    let content = text(&["Content", "CodeContent", "content"]);
    let target = text(&["Target", "TargetContent", "target"]);
    let replacement = text(&["Replacement", "ReplacementContent", "replacement"]);
    if let (Some(target), Some(replacement)) = (target, replacement) {
        (edit.added, edit.removed) = line_delta(target, replacement);
    } else if let Some(content) = content {
        match existing(&edit.file) {
            Existing::Absent => edit.added = content.lines().count() as u64,
            Existing::Text(old) => (edit.added, edit.removed) = line_delta(&old, content),
            Existing::Unreadable => edit.unknown = true,
        }
    } else {
        // An edit whose arguments say nothing we can count (several chunks, a delete, a move)
        edit.unknown = true;
    }
    Some(edit)
}

enum Existing {
    Absent,
    Text(String),
    Unreadable,
}

fn existing(file: &str) -> Existing {
    if file.is_empty() {
        return Existing::Unreadable;
    }
    match std::fs::metadata(Path::new(file)) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Existing::Absent,
        Ok(meta) if meta.is_file() && meta.len() <= MAX_COUNTED_FILE => {
            match std::fs::read_to_string(file) {
                Ok(text) => Existing::Text(text),
                Err(_) => Existing::Unreadable,
            }
        }
        _ => Existing::Unreadable,
    }
}

impl Turn {
    pub fn begin(now: i64) -> Turn {
        Turn {
            started_at: now,
            ..Turn::default()
        }
    }

    /// Whether a working update is due, and records it when it is
    pub fn working_due(&mut self, now: i64) -> bool {
        if now - self.working_sent_at < WORKING_EVERY {
            return false;
        }
        self.working_sent_at = now;
        true
    }

    pub fn record_approval(&mut self) {
        self.approvals += 1;
    }

    /// PreToolUse: remembers what the call is about to change. Nothing is counted yet, since
    /// the call may be denied or fail.
    pub fn tool_starting(&mut self, tool_name: &str, input: &Value, id: &str) {
        let Some(edit) = edit_of(tool_name, input, id) else {
            return;
        };
        self.pending.retain(|p| p.id != edit.id || edit.id.is_empty());
        if self.pending.len() >= MAX_PENDING {
            self.pending.remove(0);
        }
        self.pending.push(edit);
    }

    /// PostToolUse: the call ran, so its edit counts. A call that failed changes nothing.
    pub fn tool_finished(&mut self, tool_name: &str, input: &Value, id: &str, failed: bool) {
        let at = self
            .pending
            .iter()
            .position(|p| !id.is_empty() && p.id == id);
        let edit = match at {
            Some(at) => self.pending.remove(at),
            // No PreToolUse was seen for it (the hooks were installed mid-turn): count from
            // the arguments alone, when the tool is an edit at all
            None => match edit_of(tool_name, input, id) {
                // By now a write has replaced the file, so its old length is gone
                Some(edit) if has_content(input) => Edit {
                    unknown: true,
                    ..edit
                },
                Some(edit) => edit,
                None => return,
            },
        };
        if failed {
            return;
        }
        if !edit.file.is_empty() && !self.files.contains(&edit.file) {
            self.files.push(edit.file);
        }
        if edit.unknown {
            self.diff_unknown = true;
        } else {
            self.added += edit.added;
            self.removed += edit.removed;
        }
    }
}

fn has_content(input: &Value) -> bool {
    ["Content", "CodeContent", "content"]
        .iter()
        .any(|key| input.get(*key).is_some_and(|v| v.is_string()))
        && input.get("Target").is_none()
}

/// The working update. `tool` is the tool being run, `title` the user's request.
pub fn working(
    session_id: &str,
    project: Option<&str>,
    title: Option<&str>,
    tool: Option<(&str, &Value)>,
    cwd: &str,
    started_at: i64,
) -> Value {
    let mut update = json!({ "type": "session_update", "sessionId": session_id, "status": "working", "startedAt": started_at });
    if let Some(p) = project {
        update["projectName"] = json!(p);
    }
    if let Some(t) = title.filter(|t| !t.is_empty()) {
        update["title"] = json!(SessionState::truncate(t.lines().next().unwrap_or(""), 80));
    }
    if let Some((name, input)) = tool {
        let shown = payload::display_name(name);
        let detail = match payload::tool_summary(name, input, cwd) {
            Some(summary) => format!("{shown}({})", SessionState::truncate(&summary, 60)),
            None => shown,
        };
        update["detail"] = json!(detail);
    }
    update
}

/// The done receipt. `added` and `removed` are left out when a write made them unknowable.
pub fn done(
    session_id: &str,
    project: Option<&str>,
    summary: Option<&str>,
    turn: &Turn,
    now: i64,
) -> Value {
    let mut update = json!({
        "type": "session_update", "sessionId": session_id, "status": "done",
        "files": turn.files.len(), "approvals": turn.approvals,
    });
    if let Some(p) = project {
        update["projectName"] = json!(p);
    }
    if let Some(s) = summary.map(str::trim).filter(|s| !s.is_empty()) {
        update["summary"] = json!(SessionState::truncate(s, 200));
    }
    if turn.started_at > 0 && now >= turn.started_at {
        update["durationSeconds"] = json!(now - turn.started_at);
    }
    if !turn.diff_unknown {
        update["added"] = json!(turn.added);
        update["removed"] = json!(turn.removed);
    }
    update
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::{Input, fixtures};

    #[test]
    fn throttles_working_updates() {
        let mut turn = Turn::begin(100);
        assert!(turn.working_due(100));
        assert!(!turn.working_due(105));
        assert!(!turn.working_due(109));
        assert!(turn.working_due(110));
    }

    #[test]
    fn counts_changed_lines_between_two_texts() {
        assert_eq!(line_delta("beta", "BETA"), (1, 1));
        assert_eq!(line_delta("", "a\nb\nc\n"), (3, 0));
        assert_eq!(line_delta("a\nb\nc\n", ""), (0, 3));
        assert_eq!(line_delta("a\nb\nc", "a\nB\nc"), (1, 1));
        assert_eq!(line_delta("a\nb\nc", "a\nb\nc"), (0, 0));
        assert_eq!(line_delta("a\nc", "a\nb1\nb2\nc"), (2, 0));
        assert_eq!(line_delta("a\nx\ny\nc", "a\nc"), (0, 2));
        assert_eq!(line_delta("a\na\na", "a"), (0, 2));
        assert_eq!(line_delta("fn a() {\n}\n", "fn a() {\n    b();\n}\n"), (1, 0));
        // Changes in two places with unchanged lines between them
        assert_eq!(line_delta("a\nb\nc\nd\ne", "a\nB\nc\nD\ne\nf"), (3, 2));
        assert_eq!(line_delta("a\nb\nc", "x\ny"), (2, 3));
    }

    fn call(hook: &Value) -> (String, Value, String) {
        let input = Input(hook);
        (
            input.tool_name().unwrap(),
            input.tool_input(),
            hook["toolCall"]["id"].as_str().unwrap().to_string(),
        )
    }

    #[test]
    fn counts_the_real_write_and_edit_once_they_have_run() {
        let mut turn = Turn::begin(0);
        // write_file of a new file (the fixture's path does not exist here): three lines
        let (name, input, id) = call(&fixtures::write_file());
        turn.tool_starting(&name, &input, &id);
        assert_eq!((turn.files.len(), turn.added), (0, 0), "nothing until it ran");
        assert_eq!(turn.pending.len(), 1);
        let post = fixtures::post_tool_use();
        let (name, input, id) = call(&post);
        turn.tool_finished(&name, &input, &id, false);
        assert_eq!(turn.files, vec!["/Users/dev/projects/demo/notes.txt"]);
        assert_eq!((turn.added, turn.removed), (3, 0));
        // edit_file beta -> BETA in the same file
        let (name, input, id) = call(&fixtures::edit_file());
        turn.tool_starting(&name, &input, &id);
        turn.tool_finished(&name, &input, &id, false);
        // read_file and run_command change nothing
        for hook in [fixtures::read_file(), fixtures::run_command()] {
            let (name, input, id) = call(&hook);
            turn.tool_starting(&name, &input, &id);
            turn.tool_finished(&name, &input, &id, false);
        }
        assert_eq!(turn.files.len(), 1);
        assert_eq!((turn.added, turn.removed), (4, 1));
        assert!(!turn.diff_unknown && turn.pending.is_empty());
    }

    #[test]
    fn a_denied_or_failed_edit_counts_nothing() {
        let mut turn = Turn::begin(0);
        let (name, input, id) = call(&fixtures::edit_file());
        turn.tool_starting(&name, &input, &id);
        turn.tool_finished(&name, &input, &id, true);
        assert!(turn.files.is_empty() && turn.pending.is_empty());
        assert_eq!((turn.added, turn.removed), (0, 0));
        // Denied: no PostToolUse ever comes, so it only waits, and the wait list is bounded
        for i in 0..100 {
            turn.tool_starting(&name, &input, &format!("call_{i}"));
        }
        assert_eq!(turn.pending.len(), MAX_PENDING);
        assert!(turn.files.is_empty());
    }

    #[test]
    fn overwriting_a_file_counts_against_what_was_there() {
        let dir = std::env::temp_dir().join(format!("agn-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.txt");
        std::fs::write(&file, "alpha\nbeta\ngamma\n").unwrap();
        let input = json!({ "Path": file.to_string_lossy(), "Content": "alpha\nBETA\ngamma\ndelta\n", "Overwrite": true });
        let edit = edit_of("write_file", &input, "c1").unwrap();
        assert_eq!((edit.added, edit.removed, edit.unknown), (2, 1, false));
        // A folder where the file should be: unknowable, and done leaves the numbers out
        let input = json!({ "Path": dir.to_string_lossy(), "Content": "x" });
        let mut turn = Turn::begin(10);
        turn.tool_starting("write_file", &input, "c2");
        turn.tool_finished("write_file", &input, "c2", false);
        turn.record_approval();
        assert!(turn.diff_unknown);
        let update = done("s1", Some("proj"), Some("  All good. "), &turn, 70);
        assert_eq!(update["status"], "done");
        assert_eq!(update["approvals"], 1);
        assert_eq!(update["files"], 1);
        assert_eq!(update["durationSeconds"], 60);
        assert_eq!(update["summary"], "All good.");
        assert!(update.get("added").is_none() && update.get("removed").is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn an_edit_seen_only_afterwards_still_counts() {
        let mut turn = Turn::begin(0);
        // edit_file: Target and Replacement say everything
        let (name, input, id) = call(&fixtures::edit_file());
        turn.tool_finished(&name, &input, &id, false);
        assert_eq!((turn.added, turn.removed, turn.diff_unknown), (1, 1, false));
        // write_file: the old file is gone by now
        let (name, input, id) = call(&fixtures::post_tool_use());
        turn.tool_finished(&name, &input, &id, false);
        assert!(turn.diff_unknown);
        assert_eq!(turn.files.len(), 1);
        // Arguments that say nothing countable
        let edit = edit_of("multi_replace_file_content", &json!({ "TargetFile": "/a/b.rs" }), "x").unwrap();
        assert!(edit.unknown);
        assert_eq!(edit.file, "/a/b.rs");
        assert_eq!(edit_of("run_command", &json!({ "CommandLine": "ls" }), "x"), None);
        assert_eq!(edit_of("write_file", &Value::Null, "x").map(|e| e.unknown), Some(true));
    }

    #[test]
    fn done_carries_the_diff_when_known() {
        let mut turn = Turn::begin(0);
        let (name, input, id) = call(&fixtures::edit_file());
        turn.tool_starting(&name, &input, &id);
        turn.tool_finished(&name, &input, &id, false);
        let update = done("s1", None, None, &turn, 5);
        assert_eq!(
            (update["added"].as_u64(), update["removed"].as_u64()),
            (Some(1), Some(1))
        );
        assert_eq!(update["files"], 1);
        assert!(update.get("summary").is_none() && update.get("projectName").is_none());
    }

    #[test]
    fn working_update_shape() {
        let hook = fixtures::run_command();
        let input = Input(&hook).tool_input();
        let update = working(
            "s1",
            Some("demo"),
            Some("Refactor auth\nmore"),
            Some(("run_command", &input)),
            "/Users/dev/projects/demo",
            100,
        );
        assert_eq!(update["status"], "working");
        assert_eq!(update["title"], "Refactor auth");
        assert_eq!(update["detail"], "run_command(echo first-step)");
        assert_eq!(update["projectName"], "demo");
        assert_eq!(update["startedAt"], 100);
        let hook = fixtures::edit_file();
        let input = Input(&hook).tool_input();
        let update = working("s1", None, None, Some(("edit_file", &input)), "/Users/dev/projects/demo", 0);
        assert_eq!(update["detail"], "edit_file(Edit notes.txt)");
        let update = working("s1", None, None, Some(("mcp_x_do", &Value::Null)), "", 0);
        assert_eq!(update["detail"], "x_do");
    }
}
