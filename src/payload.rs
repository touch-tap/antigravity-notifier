//! Builds the messages the server expects: the same shapes the Claude Code clients send, so the
//! server's push code and the phone apps need no change. The vocabulary (`permission_prompt`,
//! `idle_prompt`, `user_question`) is ours, not Codex's.

use rust_i18n::t;
use serde_json::{Value, json};

use crate::session::SessionState;

pub const PERMISSION_OPTIONS: &[(&str, &str)] = &[
    ("Allow once", "y"),
    ("Allow for session", "a"),
    ("Deny", "n"),
];

/// Codex's tools that ask the user something. `request_user_input_async` lets the model carry on
/// while it waits; the answer still goes back through the same hook.
pub const QUESTION_TOOLS: &[&str] = &["request_user_input", "request_user_input_async"];

pub fn is_question_tool(tool_name: &str) -> bool {
    QUESTION_TOOLS.contains(&tool_name)
}

pub struct Input<'a>(pub &'a Value);

impl Input<'_> {
    pub fn str(&self, key: &str) -> Option<&str> {
        self.0
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    }
    pub fn session_id(&self) -> String {
        self.str("session_id").unwrap_or("unknown").to_string()
    }
    pub fn cwd(&self) -> String {
        self.str("cwd").unwrap_or("").to_string()
    }
    pub fn tool_name(&self) -> Option<String> {
        self.str("tool_name").map(|s| s.to_string())
    }
    pub fn tool_input(&self) -> Value {
        self.0.get("tool_input").cloned().unwrap_or(Value::Null)
    }
    pub fn project_name(&self) -> Option<String> {
        let cwd = self.cwd();
        std::path::Path::new(&cwd)
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
    }
}

pub fn title(project: Option<&str>) -> String {
    match project {
        Some(p) => format!("Codex - {p}"),
        None => "Codex".to_string(),
    }
}

fn options(list: &[(&str, &str)]) -> Value {
    Value::Array(
        list.iter()
            .map(|(label, value)| json!({ "label": label, "value": value }))
            .collect(),
    )
}

/// The files an `apply_patch` call touches, from its patch text (`*** Update File: path`).
pub fn patch_files(input: &Value) -> Vec<String> {
    let text = patch_text(input).unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("*** ")?;
            let path = rest
                .strip_prefix("Add File: ")
                .or_else(|| rest.strip_prefix("Update File: "))
                .or_else(|| rest.strip_prefix("Delete File: "))?;
            Some(path.trim().to_string())
        })
        .filter(|p| !p.is_empty())
        .collect()
}

/// The patch itself, whichever field Codex puts it in. Codex 0.169's PreToolUse sends
/// `apply_patch` as `{"command": "*** Begin Patch..."}`.
pub fn patch_text(input: &Value) -> Option<String> {
    for key in ["patch", "input", "content", "command"] {
        if let Some(s) = input.get(key).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    input.as_str().map(|s| s.to_string())
}

/// Codex's JavaScript tool: a snippet that calls the real tools (`tools.exec_command(...)`,
/// `tools.apply_patch(...)`). See `jsexec`.
pub const EXEC_TOOL: &str = "exec";

/// The tool calls one hook call stands for, with names and inputs in the shape the older Codex
/// tools use: an `exec` snippet becomes each call inside it (`exec_command` as `Bash {command}`,
/// `apply_patch` as `{patch}`); any other tool is itself. An `exec` snippet with no readable call
/// stays as it is.
pub fn tool_calls(tool_name: &str, input: &Value) -> Vec<(String, Value)> {
    if tool_name != EXEC_TOOL {
        return vec![(tool_name.to_string(), input.clone())];
    }
    let source = input.as_str().map(str::to_string).or_else(|| {
        ["input", "code", "source", "script", "command"]
            .iter()
            .find_map(|k| input.get(*k).and_then(|v| v.as_str()).map(str::to_string))
    });
    let calls: Vec<(String, Value)> = source
        .map(|src| crate::jsexec::tool_calls(&src))
        .unwrap_or_default()
        .into_iter()
        .map(|(name, arg)| normalise_call(name, arg))
        .collect();
    if calls.is_empty() {
        vec![(tool_name.to_string(), input.clone())]
    } else {
        calls
    }
}

fn normalise_call(name: String, arg: Value) -> (String, Value) {
    match name.as_str() {
        "exec_command" | "shell" | "shell_command" | "local_shell" => {
            let command = match &arg {
                Value::String(s) => Some(Value::String(s.clone())),
                _ => ["cmd", "command"].iter().find_map(|k| arg.get(*k).cloned()),
            };
            let mut input = json!({});
            if let Some(c) = command {
                input["command"] = c;
            }
            for key in ["justification", "description"] {
                if let Some(v) = arg.get(key) {
                    input[key] = v.clone();
                }
            }
            ("Bash".to_string(), input)
        }
        "apply_patch" => {
            let patch = match &arg {
                Value::String(s) => Some(s.clone()),
                _ => patch_text(&arg),
            };
            let input = patch.map(|p| json!({ "patch": p })).unwrap_or(json!({}));
            (name, input)
        }
        _ => (name, arg),
    }
}

/// The tool name to show for a hook call: the first tool inside an `exec` snippet.
pub fn display_name(tool_name: &str, input: &Value) -> String {
    tool_calls(tool_name, input)
        .into_iter()
        .next()
        .map(|(name, _)| name)
        .unwrap_or_else(|| tool_name.to_string())
}

/// A one-line summary of a tool call, the way the phone shows it. An `exec` snippet is the
/// summaries of the calls in it, joined.
pub fn tool_summary(tool_name: &str, input: &Value) -> Option<String> {
    if tool_name == EXEC_TOOL {
        let calls = tool_calls(tool_name, input);
        if !(calls.len() == 1 && calls[0].0 == EXEC_TOOL) {
            let parts: Vec<String> = calls
                .iter()
                .filter_map(|(name, input)| single_summary(name, input))
                .collect();
            return (!parts.is_empty()).then(|| SessionState::truncate(&parts.join("; "), 200));
        }
    }
    single_summary(tool_name, input)
}

fn single_summary(tool_name: &str, input: &Value) -> Option<String> {
    let field = |key: &str| {
        input
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    let summary = match tool_name {
        // Codex names its shell tool Bash in hook input, after Claude Code's convention
        "Bash" | "shell" | "shell_command" | "exec_command" | "local_shell" => field("command")
            .or_else(|| {
                input["command"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|p| p.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .filter(|s| !s.is_empty())
            }),
        "apply_patch" => {
            let files = patch_files(input);
            match files.len() {
                0 => Some(t!("payload.apply_patch").into_owned()),
                1 => Some(t!("payload.edit_file", file = files[0]).into_owned()),
                n => Some(t!("payload.edit_files", file = files[0], count = n - 1).into_owned()),
            }
        }
        "web_search" => field("query").map(|q| t!("payload.search", pattern = q).into_owned()),
        "view_image" => field("path"),
        "update_plan" => Some(t!("payload.update_plan").into_owned()),
        name if is_question_tool(name) => first_question(input),
        _ => {
            // The input itself when it is text (a custom tool), else the first string field,
            // else a compact JSON rendering
            input.as_str().map(str::to_string).or_else(|| {
                input
                    .as_object()
                    .and_then(|o| o.values().find_map(|v| v.as_str().map(|s| s.to_string())))
                    .or_else(|| {
                        if input.is_null() {
                            None
                        } else {
                            Some(input.to_string())
                        }
                    })
            })
        }
    };
    summary.map(|s| SessionState::truncate(&s, 200))
}

pub fn first_question(input: &Value) -> Option<String> {
    input["questions"].as_array()?.first()?["question"]
        .as_str()
        .map(|s| s.to_string())
}

/// Normalises `request_user_input` input (`questions: [{id, header, question, isOther, isSecret,
/// options?: [{label, description}]}]`) into the question shape the phone renders. A question
/// without options is asked as free text: its single option is "Type an answer", which the phone
/// shows with its text field.
pub fn questions_from_input(input: &Value) -> Vec<Value> {
    let Some(list) = input["questions"].as_array() else {
        return vec![];
    };
    list.iter()
        .filter_map(|q| {
            let question = q["question"].as_str()?;
            if q["isSecret"].as_bool().unwrap_or(false) {
                // Never put a secret through the phone
                return None;
            }
            let options: Vec<Value> = q["options"]
                .as_array()
                .map(|opts| {
                    opts.iter()
                        .filter_map(|o| {
                            let label = o["label"].as_str()?;
                            Some(json!({ "label": label, "value": label, "description": o["description"].as_str() }))
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(json!({
                "id": q["id"].as_str().unwrap_or(""),
                "question": question,
                "header": q["header"].as_str().map(str::to_string)
                    .unwrap_or_else(|| t!("payload.question_header").into_owned()),
                "options": options,
                "multiSelect": false,
                "freeText": options.is_empty(),
            }))
        })
        .collect()
}

pub fn terminal_context(session_id: &str, cwd: &str, remote: bool) -> Value {
    json!({
        "terminalApp": "codex-notifier",
        "sessionIdentifier": session_id,
        "workingDirectory": cwd,
        "supportsRemoteInput": remote,
    })
}

/// The card for a permission prompt. Returns the message, its kind and a key that identifies
/// the request.
pub fn permission_card(input: &Input, remote: bool) -> (Value, &'static str, String) {
    let raw_name = input.tool_name().unwrap_or_else(|| "Tool".to_string());
    let raw_input = input.tool_input();
    let summary = tool_summary(&raw_name, &raw_input);
    // An `exec` snippet: name the tool inside it, and read its justification
    let (tool_name, tool_input) = tool_calls(&raw_name, &raw_input)
        .into_iter()
        .next()
        .unwrap_or((raw_name.clone(), raw_input.clone()));
    let project = input.project_name();
    let session_id = input.session_id();

    let description = tool_input["description"]
        .as_str()
        .or_else(|| tool_input["justification"].as_str())
        .unwrap_or("");
    let text = match (&summary, description.is_empty()) {
        (Some(s), false) => format!("{tool_name}: {s}\n\n{description}"),
        (Some(s), true) => format!("{tool_name}: {s}"),
        (None, _) => t!("payload.needs_permission", tool = tool_name).into_owned(),
    };

    let key = format!("{tool_name}:{}", summary.clone().unwrap_or_default());
    let card = json!({
        "type": "notification",
        "sessionId": session_id,
        "notificationType": "permission_prompt",
        "title": title(project.as_deref()),
        "message": t!("payload.permission_required"),
        "promptText": text,
        "promptType": "permission",
        "options": options(PERMISSION_OPTIONS),
        "terminalContext": terminal_context(&session_id, &input.cwd(), remote),
        "projectName": project,
        "projectPath": input.cwd(),
        "toolName": tool_name,
        "toolInput": summary,
    });
    (card, "permission", key)
}

/// The card for a `request_user_input` call. Questions with options render as a question card;
/// a first question without options is asked as free text, the way a Stop follow-up is typed.
pub fn question_card(input: &Input, questions: &[Value], remote: bool) -> (Value, String) {
    let project = input.project_name();
    let session_id = input.session_id();
    let fallback = crate::local::question_title();
    let first = questions
        .first()
        .and_then(|q| q["question"].as_str())
        .unwrap_or(&fallback);
    let key = format!("request_user_input:{first}");
    let free_text = questions
        .first()
        .and_then(|q| q["freeText"].as_bool())
        .unwrap_or(false);
    let with_options: Vec<Value> = questions
        .iter()
        .filter(|q| !q["freeText"].as_bool().unwrap_or(false))
        .cloned()
        .collect();
    let mut card = json!({
        "type": "notification",
        "sessionId": session_id,
        "notificationType": "permission_prompt",
        "title": title(project.as_deref()),
        "message": t!("payload.has_questions"),
        "promptText": first,
        "terminalContext": terminal_context(&session_id, &input.cwd(), remote),
        "projectName": project,
        "projectPath": input.cwd(),
        "toolName": "request_user_input",
        "toolInput": SessionState::truncate(first, 200),
    });
    if free_text || with_options.is_empty() {
        card["promptType"] = json!("freeform");
    } else {
        card["promptType"] = json!("user_question");
        card["questions"] = Value::Array(with_options);
    }
    (card, key)
}

/// The card for Stop: Codex has finished a turn.
pub fn done_card(input: &Input, state: &SessionState, remote: bool) -> Value {
    let project = input.project_name();
    let session_id = input.session_id();
    let last = input
        .str("last_assistant_message")
        .map(|s| SessionState::truncate(s.trim(), 1500));
    let prompt_text = last
        .filter(|s| !s.is_empty())
        .or_else(|| {
            state.prompt.as_ref().map(|p| {
                t!(
                    "payload.finished_prompt",
                    prompt = SessionState::truncate(p, 300)
                )
                .into_owned()
            })
        })
        .unwrap_or_else(crate::local::finished_body);
    json!({
        "type": "notification",
        "sessionId": session_id,
        "notificationType": "idle_prompt",
        "title": title(project.as_deref()),
        "message": crate::local::finished_title(),
        "promptText": prompt_text,
        "promptType": "freeform",
        "terminalContext": terminal_context(&session_id, &input.cwd(), remote),
        "projectName": project,
        "projectPath": input.cwd(),
    })
}

pub fn session_update(session_id: &str, status: &str) -> Value {
    json!({ "type": "session_update", "sessionId": session_id, "status": status })
}

pub fn test_notification(device_id: &str) -> Value {
    json!({
        "type": "test_notification",
        "sessionId": format!("test-{}", uuid::Uuid::new_v4()),
        "targetDeviceId": device_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarises_codex_tools() {
        assert_eq!(
            tool_summary("Bash", &json!({ "command": "cargo test" })).as_deref(),
            Some("cargo test")
        );
        assert_eq!(
            tool_summary("Bash", &json!({ "command": ["git", "status"] })).as_deref(),
            Some("git status")
        );
        let patch = "*** Begin Patch\n*** Update File: src/main.rs\n@@\n-a\n+b\n*** Add File: README.md\n+hi\n*** End Patch";
        assert_eq!(
            tool_summary("apply_patch", &json!({ "patch": patch })).as_deref(),
            Some("Edit src/main.rs and 1 more")
        );
        assert_eq!(
            patch_files(&json!({ "input": patch })),
            vec!["src/main.rs", "README.md"]
        );
    }

    const EXEC_CMD: &str =
        r#"text(await tools.exec_command({cmd:"echo hi > hello.txt",max_output_tokens:1000}));"#;
    const EXEC_PATCH: &str = r#"text(await tools.apply_patch("*** Begin Patch\n*** Add File: /Users/jack/codex-test-proj/colour.txt\n+Green\n*** End Patch"));"#;

    #[test]
    fn reads_the_codex_0_169_hook_shape() {
        // What PreToolUse sent from Codex 0.169 in a real session
        let input = json!({ "command": "*** Begin Patch\n*** Add File: notes.md\n+First line.\n+Second line.\n*** End Patch" });
        assert_eq!(patch_files(&input), vec!["notes.md"]);
        assert_eq!(
            tool_summary("apply_patch", &input).as_deref(),
            Some("Edit notes.md")
        );
    }

    #[test]
    fn unwraps_the_exec_tool() {
        for input in [
            json!(EXEC_CMD),
            json!({ "input": EXEC_CMD }),
            json!({ "code": EXEC_CMD }),
        ] {
            assert_eq!(
                tool_summary("exec", &input).as_deref(),
                Some("echo hi > hello.txt")
            );
            assert_eq!(display_name("exec", &input), "Bash");
        }
        let patch = json!(EXEC_PATCH);
        assert_eq!(
            tool_summary("exec", &patch).as_deref(),
            Some("Edit /Users/jack/codex-test-proj/colour.txt")
        );
        assert_eq!(display_name("exec", &patch), "apply_patch");
        // Two calls in one snippet: both summarised, the first names the tool
        let both = json!(format!("{EXEC_CMD}\n{EXEC_PATCH}"));
        assert_eq!(
            tool_summary("exec", &both).as_deref(),
            Some("echo hi > hello.txt; Edit /Users/jack/codex-test-proj/colour.txt")
        );
        assert_eq!(display_name("exec", &both), "Bash");
        // No readable call: the snippet itself, as before
        let plain = json!("text(1 + 1)");
        assert_eq!(tool_summary("exec", &plain).as_deref(), Some("text(1 + 1)"));
        assert_eq!(display_name("exec", &plain), "exec");
        // A tool that is not exec is left alone
        assert_eq!(tool_calls("Bash", &json!({ "command": "ls" }))[0].0, "Bash");
    }

    #[test]
    fn permission_card_for_an_exec_snippet() {
        let js = r#"text(await tools.exec_command({cmd:"curl -sI https://example.com | head -1",sandbox_permissions:"require_escalated",justification:"May I run curl outside the sandbox?"}));"#;
        let hook =
            json!({ "session_id": "s", "cwd": "/tmp/p", "tool_name": "exec", "tool_input": js });
        let (card, kind, key) = permission_card(&Input(&hook), true);
        assert_eq!(kind, "permission");
        assert_eq!(card["toolName"], "Bash");
        assert_eq!(card["toolInput"], "curl -sI https://example.com | head -1");
        assert_eq!(
            card["promptText"],
            "Bash: curl -sI https://example.com | head -1\n\nMay I run curl outside the sandbox?"
        );
        assert_eq!(key, "Bash:curl -sI https://example.com | head -1");
    }

    #[test]
    fn questions_keep_options_and_mark_free_text() {
        let input = json!({ "questions": [
            { "id": "fw", "header": "Framework", "question": "Which one?", "options": [
                { "label": "React", "description": "the usual" }, { "label": "Vue", "description": "" } ] },
            { "id": "name", "header": "Name", "question": "What is it called?" },
            { "id": "pw", "header": "Secret", "question": "Password?", "isSecret": true }
        ]});
        let qs = questions_from_input(&input);
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0]["options"].as_array().unwrap().len(), 2);
        assert_eq!(qs[0]["freeText"], false);
        assert_eq!(qs[1]["freeText"], true);
        let (card, key) = question_card(
            &Input(&json!({ "session_id": "s", "cwd": "/tmp/p" })),
            &qs,
            true,
        );
        assert_eq!(card["promptType"], "user_question");
        assert_eq!(card["questions"].as_array().unwrap().len(), 1);
        assert_eq!(key, "request_user_input:Which one?");
        assert_eq!(card["title"], "Codex - p");

        let (card, _) = question_card(
            &Input(&json!({ "session_id": "s", "cwd": "/tmp/p" })),
            &qs[1..],
            true,
        );
        assert_eq!(card["promptType"], "freeform");
        assert_eq!(card["promptText"], "What is it called?");
    }
}
