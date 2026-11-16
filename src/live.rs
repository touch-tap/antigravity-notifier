//! What the phone's lock-screen card needs to know while Codex works: the current step, and a
//! receipt when it stops. Built from what hooks see, with nothing guessed: a number we cannot
//! derive is left out of the message.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::payload;
use crate::session::SessionState;

/// At most one working update per session in this many seconds
pub const WORKING_EVERY: i64 = 10;

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
    /// A Write replaced a file whose old length is unknown, so added/removed would be a guess
    pub diff_unknown: bool,
    /// The latest step and step count from Codex's plan (`update_plan`)
    pub progress: Option<(u32, u32)>,
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

    /// Counts the files and lines a tool call is about to change. Codex edits files with
    /// `apply_patch`, whose patch text names every file and marks added and removed lines.
    pub fn record_tool(&mut self, tool_name: &str, input: &Value) {
        if let Some(progress) = todo_progress(tool_name, input) {
            self.progress = Some(progress);
        }
        // Codex's `exec` tool wraps the real calls in a JavaScript snippet
        if tool_name == payload::EXEC_TOOL {
            for (name, input) in payload::tool_calls(tool_name, input) {
                if name != payload::EXEC_TOOL {
                    self.record_tool(&name, &input);
                }
            }
            return;
        }
        if tool_name != "apply_patch" {
            return;
        }
        for path in payload::patch_files(input) {
            if !self.files.contains(&path) {
                self.files.push(path);
            }
        }
        match payload::patch_text(input) {
            Some(text) => self.count_patch(&text),
            None => self.diff_unknown = true,
        }
    }

    fn count_patch(&mut self, patch: &str) {
        for line in patch.lines() {
            if line.starts_with("*** ") {
                continue;
            }
            if line.starts_with('+') {
                self.added += 1;
            } else if line.starts_with('-') {
                self.removed += 1;
            }
        }
    }
}

/// (current step, total steps) from an `update_plan` call: the first step in progress, else the
/// first not yet done. None for any other tool or an empty plan.
pub fn todo_progress(tool_name: &str, input: &Value) -> Option<(u32, u32)> {
    if tool_name != "update_plan" {
        return None;
    }
    let steps = input["plan"].as_array().filter(|t| !t.is_empty())?;
    let current = steps
        .iter()
        .position(|t| t["status"] == "in_progress")
        .or_else(|| steps.iter().position(|t| t["status"] != "completed"))
        .unwrap_or(steps.len() - 1);
    Some((current as u32 + 1, steps.len() as u32))
}

/// The working update. `detail` is the tool being run, `title` the user's request.
pub fn working(
    session_id: &str,
    project: Option<&str>,
    title: Option<&str>,
    tool: Option<(&str, &Value)>,
    progress: Option<(u32, u32)>,
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
        let shown = payload::display_name(name, input);
        let detail = match payload::tool_summary(name, input) {
            Some(summary) => format!("{shown}({})", SessionState::truncate(&summary, 60)),
            None => shown,
        };
        update["detail"] = json!(detail);
    }
    if let Some((step, steps)) = progress {
        update["step"] = json!(step);
        update["steps"] = json!(steps);
    }
    update
}

/// The done receipt. `added` and `removed` are left out when a Write made them unknowable.
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

    #[test]
    fn throttles_working_updates() {
        let mut turn = Turn::begin(100);
        assert!(turn.working_due(100));
        assert!(!turn.working_due(105));
        assert!(!turn.working_due(109));
        assert!(turn.working_due(110));
    }

    const PATCH: &str = "*** Begin Patch\n*** Update File: a.ts\n@@\n-x\n+x\n+y\n*** Add File: b.ts\n+one\n+two\n*** End Patch";

    #[test]
    fn counts_distinct_files_and_patch_lines() {
        let mut turn = Turn::begin(0);
        turn.record_tool("apply_patch", &json!({ "patch": PATCH }));
        turn.record_tool(
            "apply_patch",
            &json!({ "patch": "*** Begin Patch\n*** Update File: a.ts\n-q\n+r\n*** End Patch" }),
        );
        turn.record_tool("Bash", &json!({ "command": "cat c.ts" }));
        assert_eq!(turn.files, vec!["a.ts", "b.ts"]);
        assert_eq!((turn.added, turn.removed), (5, 2));
        assert!(!turn.diff_unknown);
    }

    #[test]
    fn a_patch_without_text_makes_the_diff_unknown_and_done_omits_it() {
        let mut turn = Turn::begin(10);
        turn.record_tool("apply_patch", &json!({ "files": ["n.ts"] }));
        turn.record_approval();
        let update = done("s1", Some("proj"), Some("  All good. "), &turn, 70);
        assert_eq!(update["status"], "done");
        assert_eq!(update["approvals"], 1);
        assert_eq!(update["durationSeconds"], 60);
        assert_eq!(update["summary"], "All good.");
        assert!(update.get("added").is_none() && update.get("removed").is_none());
    }

    #[test]
    fn done_carries_the_diff_when_known() {
        let mut turn = Turn::begin(0);
        turn.record_tool("apply_patch", &json!({ "patch": PATCH }));
        let update = done("s1", None, None, &turn, 5);
        assert_eq!(
            (update["added"].as_u64(), update["removed"].as_u64()),
            (Some(4), Some(1))
        );
        assert_eq!(update["files"], 2);
        assert!(update.get("summary").is_none() && update.get("projectName").is_none());
    }

    #[test]
    fn counts_patches_inside_exec_snippets_and_the_0_169_shape() {
        let mut turn = Turn::begin(0);
        let js = r#"text(await tools.apply_patch("*** Begin Patch\n*** Add File: /Users/jack/codex-test-proj/colour.txt\n+Green\n*** End Patch"));
text(await tools.exec_command({cmd:"cat colour.txt"}));
text(await tools.apply_patch({input:`*** Begin Patch
*** Update File: a.ts
-x
+y
*** End Patch`}));"#;
        turn.record_tool("exec", &json!(js));
        // Codex 0.169's PreToolUse: apply_patch with the patch under "command"
        turn.record_tool(
            "apply_patch",
            &json!({ "command": "*** Begin Patch\n*** Add File: notes.md\n+one\n+two\n*** End Patch" }),
        );
        assert_eq!(
            turn.files,
            vec!["/Users/jack/codex-test-proj/colour.txt", "a.ts", "notes.md"]
        );
        assert_eq!((turn.added, turn.removed), (4, 1));
        assert!(!turn.diff_unknown);
    }

    #[test]
    fn working_update_names_the_tool_inside_exec() {
        let input = json!(
            r#"text(await tools.exec_command({cmd:"echo hi > hello.txt",max_output_tokens:1000}));"#
        );
        let update = working("s1", None, None, Some(("exec", &input)), None, 0);
        assert_eq!(update["detail"], "Bash(echo hi > hello.txt)");
    }

    #[test]
    fn reads_progress_from_the_plan() {
        let plan = json!({ "plan": [
            { "step": "a", "status": "completed" }, { "step": "b", "status": "completed" },
            { "step": "c", "status": "in_progress" }, { "step": "d", "status": "pending" }
        ] });
        assert_eq!(todo_progress("update_plan", &plan), Some((3, 4)));
        let none_started = json!({ "plan": [{ "status": "completed" }, { "status": "pending" }] });
        assert_eq!(todo_progress("update_plan", &none_started), Some((2, 2)));
        let all_done = json!({ "plan": [{ "status": "completed" }] });
        assert_eq!(todo_progress("update_plan", &all_done), Some((1, 1)));
        assert_eq!(todo_progress("apply_patch", &plan), None);
        assert_eq!(todo_progress("update_plan", &json!({ "plan": [] })), None);
    }

    #[test]
    fn remembers_the_latest_plan_progress() {
        let mut turn = Turn::begin(0);
        turn.record_tool(
            "update_plan",
            &json!({ "plan": [{ "status": "in_progress" }, { "status": "pending" }] }),
        );
        turn.record_tool("Bash", &json!({ "command": "ls" }));
        assert_eq!(turn.progress, Some((1, 2)));
    }

    #[test]
    fn working_update_shape() {
        let input = json!({ "command": "cargo test" });
        let update = working(
            "s1",
            Some("my-project"),
            Some("Refactor auth\nmore"),
            Some(("Bash", &input)),
            Some((4, 6)),
            100,
        );
        assert_eq!(update["status"], "working");
        assert_eq!(update["title"], "Refactor auth");
        assert_eq!(update["detail"], "Bash(cargo test)");
        assert_eq!(
            (update["step"].as_u64(), update["steps"].as_u64()),
            (Some(4), Some(6))
        );
        assert_eq!(update["projectName"], "my-project");
    }
}
