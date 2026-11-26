//! Reads Antigravity's hook input, and builds the messages the server expects: the same shapes
//! the Claude Code and Codex clients send, so the server's push code and the phone apps need no
//! change. The vocabulary (`permission_prompt`, `idle_prompt`, `user_question`) is ours, not
//! Antigravity's.
//!
//! The input shapes are the ones agy 1.5.18 sent on 26 Nov 2026 (tests/fixtures): camelCase,
//! `conversationId`, `cwd`, `toolCall {name, args, id}`. Its tool names differ from Google's
//! docs: `write_file` and `edit_file`, not `write_to_file` and `replace_file_content`. The
//! documented names are accepted as well, since the desktop app and IDE were not measured.

use rust_i18n::t;
use serde_json::{Value, json};

use crate::session::SessionState;

pub const PERMISSION_OPTIONS: &[(&str, &str)] = &[
    ("Allow once", "y"),
    ("Allow for session", "a"),
    ("Deny", "n"),
];

/// The tool Antigravity asks the user something with
pub const QUESTION_TOOL: &str = "ask_question";

pub fn is_question_tool(tool_name: &str) -> bool {
    tool_name == QUESTION_TOOL
}

/// What a tool call can do, which decides whether it may wait for the phone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolClass {
    /// Runs a shell command
    Command,
    /// Changes files
    Edit,
    /// Changes something else (an MCP tool, or a tool whose name says so)
    Other,
    /// Asks the user a question
    Question,
    /// Only reads, or nothing is known about it: never waits for the phone
    Passive,
}

impl ToolClass {
    /// Whether the call changes something, so an approval from the phone makes sense
    pub fn side_effects(self) -> bool {
        matches!(
            self,
            ToolClass::Command | ToolClass::Edit | ToolClass::Other
        )
    }
}

const COMMAND_TOOLS: &[&str] = &["run_command", "run_terminal_command", "send_command_input"];
const EDIT_TOOLS: &[&str] = &[
    "write_file",
    "edit_file",
    "multi_edit_file",
    "delete_file",
    "move_file",
    "rename_file",
    // The names in Google's docs
    "write_to_file",
    "replace_file_content",
    "multi_replace_file_content",
];
/// Words in a tool's name that mean it only looks
const READ_WORDS: &[&str] = &[
    "read", "view", "list", "ls", "search", "grep", "find", "glob", "get", "fetch", "query",
    "show", "describe", "status", "check", "inspect", "browse", "lookup", "count", "diff",
    "outline", "open", "cat", "stat", "tree", "peek", "explain", "help",
];
/// Words in a tool's name that mean it changes something
const WRITE_WORDS: &[&str] = &[
    "write",
    "edit",
    "delete",
    "remove",
    "create",
    "update",
    "replace",
    "move",
    "rename",
    "run",
    "exec",
    "execute",
    "install",
    "deploy",
    "send",
    "post",
    "put",
    "patch",
    "kill",
    "commit",
    "push",
    "merge",
    "apply",
    "set",
    "add",
    "insert",
    "drop",
    "publish",
    "upload",
    "save",
    "start",
    "stop",
    "restart",
    "terminate",
    "click",
    "type",
    "submit",
];

/// Sorts a tool by name. A read-only tool, and a tool nothing is known about, is Passive: holding
/// those for the phone would stall the agent for no reason, and Antigravity's own policy still
/// applies to them. An MCP tool (`mcp_<server>_<tool>`) can do anything, so it counts as changing
/// something unless its name says it only looks.
pub fn classify(tool_name: &str) -> ToolClass {
    if is_question_tool(tool_name) {
        return ToolClass::Question;
    }
    if COMMAND_TOOLS.contains(&tool_name) {
        return ToolClass::Command;
    }
    if EDIT_TOOLS.contains(&tool_name) {
        return ToolClass::Edit;
    }
    let lower = tool_name.to_ascii_lowercase();
    let is_mcp = lower.starts_with("mcp_") || lower.starts_with("mcp:");
    let mut words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    if is_mcp && !words.is_empty() {
        // "mcp" itself and the server's name say nothing about the tool
        words.remove(0);
        if words.len() > 1 {
            words.remove(0);
        }
    }
    if words.iter().any(|w| WRITE_WORDS.contains(w)) {
        return ToolClass::Other;
    }
    if words.iter().any(|w| READ_WORDS.contains(w)) {
        return ToolClass::Passive;
    }
    if is_mcp {
        ToolClass::Other
    } else {
        ToolClass::Passive
    }
}

pub struct Input<'a>(pub &'a Value);

impl Input<'_> {
    pub fn str(&self, key: &str) -> Option<&str> {
        self.0
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    }
    /// The event, as the payload names it
    pub fn event(&self) -> Option<&str> {
        self.str("hookEventName")
    }
    /// A conversation is our session
    pub fn session_id(&self) -> String {
        self.str("conversationId").unwrap_or("unknown").to_string()
    }
    /// The working directory: `cwd`, else the first workspace folder
    pub fn cwd(&self) -> String {
        self.str("cwd")
            .or_else(|| {
                self.0
                    .get("workspacePaths")
                    .and_then(|paths| paths.as_array())
                    .and_then(|paths| paths.iter().find_map(|p| p.as_str()))
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or("")
            .to_string()
    }
    fn tool_call(&self) -> Option<&Value> {
        self.0.get("toolCall")
    }
    pub fn tool_name(&self) -> Option<String> {
        self.tool_call()?
            .get("name")?
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }
    pub fn tool_input(&self) -> Value {
        self.tool_call()
            .and_then(|call| call.get("args"))
            .cloned()
            .unwrap_or(Value::Null)
    }
    /// What the user last typed
    pub fn prompt(&self) -> Option<&str> {
        self.str("lastUserInput")
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
        Some(p) => format!("Antigravity - {p}"),
        None => "Antigravity".to_string(),
    }
}

fn options(list: &[(&str, &str)]) -> Value {
    Value::Array(
        list.iter()
            .map(|(label, value)| json!({ "label": label, "value": value }))
            .collect(),
    )
}

fn field(input: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        input
            .get(*key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string())
    })
}

/// The file a write or edit names
pub fn file_path(input: &Value) -> Option<String> {
    field(
        input,
        &["Path", "TargetFile", "AbsolutePath", "path", "file_path"],
    )
}

/// A path as the phone shows it: inside the working directory, without the directory.
pub fn short_path(path: &str, cwd: &str) -> String {
    if !cwd.is_empty()
        && let Some(rest) = path.strip_prefix(cwd)
        && let Some(rest) = rest.strip_prefix(['/', '\\'])
        && !rest.is_empty()
    {
        return rest.to_string();
    }
    path.to_string()
}

/// The name the phone shows for a tool: Antigravity's own, except that an MCP tool loses its
/// `mcp_` prefix.
pub fn display_name(tool_name: &str) -> String {
    tool_name
        .strip_prefix("mcp_")
        .filter(|rest| !rest.is_empty())
        .unwrap_or(tool_name)
        .to_string()
}

/// A one-line summary of a tool call, the way the phone shows it.
pub fn tool_summary(tool_name: &str, input: &Value, cwd: &str) -> Option<String> {
    let summary = match classify(tool_name) {
        ToolClass::Command => field(input, &["CommandLine", "Command", "command", "Input"]),
        ToolClass::Edit => file_path(input)
            .map(|path| t!("payload.edit_file", file = short_path(&path, cwd)).into_owned()),
        ToolClass::Question => first_question(input),
        ToolClass::Other | ToolClass::Passive => match tool_name {
            "read_file" => file_path(input).map(|path| short_path(&path, cwd)),
            // Antigravity's own short description when the call has one, else the first text
            // argument, else the arguments as JSON
            _ => field(input, &["toolSummary"]).or_else(|| generic_summary(input)),
        },
    };
    summary.map(|s| SessionState::truncate(s.trim(), 200))
}

fn generic_summary(input: &Value) -> Option<String> {
    if let Some(text) = input.as_str() {
        return Some(text.to_string()).filter(|s| !s.trim().is_empty());
    }
    if let Some(first) = input.as_object().and_then(|o| {
        o.values()
            .find_map(|v| v.as_str().filter(|s| !s.trim().is_empty()))
    }) {
        return Some(first.to_string());
    }
    let empty = input.is_null() || input.as_object().is_some_and(|o| o.is_empty());
    (!empty).then(|| input.to_string())
}

pub fn first_question(input: &Value) -> Option<String> {
    input.get("questions")?.as_array()?.first()?.get("question")?
        .as_str()
        .map(|s| s.to_string())
}

/// Normalises `ask_question` input (`questions: [{question, options: ["a", "b"],
/// is_multi_select}]`) into the question shape the phone renders. An option may also be an object
/// with a `label`. A question without options is asked as free text.
pub fn questions_from_input(input: &Value) -> Vec<Value> {
    let Some(list) = input.get("questions").and_then(|q| q.as_array()) else {
        return vec![];
    };
    list.iter()
        .filter_map(|q| {
            let question = q
                .get("question")?
                .as_str()
                .filter(|s| !s.trim().is_empty())?;
            let options: Vec<Value> = q
                .get("options")
                .and_then(|o| o.as_array())
                .map(|opts| opts.iter().filter_map(question_option).collect())
                .unwrap_or_default();
            let multi = ["is_multi_select", "isMultiSelect", "multiSelect"]
                .iter()
                .find_map(|key| q.get(*key).and_then(|v| v.as_bool()))
                .unwrap_or(false);
            let header = q
                .get("header")
                .and_then(|h| h.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| t!("payload.question_header").into_owned());
            Some(json!({
                "id": q.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                "question": question,
                "header": header,
                "options": options,
                "multiSelect": multi,
                "freeText": options.is_empty(),
            }))
        })
        .collect()
}

fn question_option(option: &Value) -> Option<Value> {
    let label = option
        .as_str()
        .or_else(|| option.get("label").and_then(|l| l.as_str()))
        .filter(|s| !s.trim().is_empty())?;
    let mut out = json!({ "label": label, "value": label });
    if let Some(description) = option.get("description").and_then(|d| d.as_str()) {
        out["description"] = json!(description);
    }
    Some(out)
}

pub fn terminal_context(session_id: &str, cwd: &str, remote: bool) -> Value {
    json!({
        "terminalApp": "antigravity-notifier",
        "sessionIdentifier": session_id,
        "workingDirectory": cwd,
        "supportsRemoteInput": remote,
    })
}

/// The card for a tool call that waits for approval. Returns the message, its kind and a key
/// that identifies the request.
pub fn permission_card(input: &Input, remote: bool) -> (Value, &'static str, String) {
    let raw_name = input.tool_name().unwrap_or_else(|| "Tool".to_string());
    let tool_input = input.tool_input();
    let cwd = input.cwd();
    let summary = tool_summary(&raw_name, &tool_input, &cwd);
    let tool_name = display_name(&raw_name);
    let project = input.project_name();
    let session_id = input.session_id();

    // Antigravity's own few words about a command ("Run the tests"), under the command itself
    let description = match classify(&raw_name) {
        ToolClass::Command => field(&tool_input, &["toolSummary"]).unwrap_or_default(),
        _ => String::new(),
    };
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
        "terminalContext": terminal_context(&session_id, &cwd, remote),
        "projectName": project,
        "projectPath": cwd,
        "toolName": tool_name,
        "toolInput": summary,
    });
    (card, "permission", key)
}

/// The card for an `ask_question` call. Questions with options render as a question card; a
/// first question without options is asked as free text, the way a Stop follow-up is typed.
pub fn question_card(input: &Input, questions: &[Value], remote: bool) -> (Value, String) {
    let project = input.project_name();
    let session_id = input.session_id();
    let fallback = crate::local::question_title();
    let first = questions
        .first()
        .and_then(|q| q["question"].as_str())
        .unwrap_or(&fallback);
    let key = format!("{QUESTION_TOOL}:{first}");
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
        "toolName": QUESTION_TOOL,
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

/// What the done card says. `last_message` is Antigravity's final message from the transcript;
/// `error` is set when the turn ended because something went wrong.
pub fn done_text(last_message: Option<&str>, error: Option<&str>, state: &SessionState) -> String {
    let last = last_message
        .map(|s| SessionState::truncate(s.trim(), 1500))
        .filter(|s| !s.is_empty());
    if let Some(error) = error {
        let line = t!(
            "payload.stopped_error",
            error = SessionState::truncate(error.trim(), 300)
        )
        .into_owned();
        return match last {
            Some(text) => format!("{line}\n\n{text}"),
            None => line,
        };
    }
    last.or_else(|| {
        state.prompt.as_ref().map(|p| {
            t!(
                "payload.finished_prompt",
                prompt = SessionState::truncate(p, 300)
            )
            .into_owned()
        })
    })
    .unwrap_or_else(crate::local::finished_body)
}

/// The card for Stop: Antigravity has finished a turn.
pub fn done_card(
    input: &Input,
    state: &SessionState,
    last_message: Option<&str>,
    error: Option<&str>,
    remote: bool,
) -> Value {
    let project = input.project_name();
    let session_id = input.session_id();
    let message = match error {
        Some(_) => crate::local::error_title(),
        None => crate::local::finished_title(),
    };
    json!({
        "type": "notification",
        "sessionId": session_id,
        "notificationType": "idle_prompt",
        "title": title(project.as_deref()),
        "message": message,
        "promptText": done_text(last_message, error, state),
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
pub mod fixtures {
    //! What agy 1.5.18 sent on 26 Nov 2026 (tests/fixtures/*.json), with the home folder renamed.
    use serde_json::Value;

    fn parse(text: &str) -> Value {
        serde_json::from_str(text).expect("fixture is JSON")
    }
    pub fn session_start() -> Value {
        parse(include_str!("../tests/fixtures/session_start.json"))
    }
    pub fn pre_invocation() -> Value {
        parse(include_str!("../tests/fixtures/pre_invocation.json"))
    }
    pub fn run_command() -> Value {
        parse(include_str!(
            "../tests/fixtures/pre_tool_use_run_command.json"
        ))
    }
    pub fn write_file() -> Value {
        parse(include_str!(
            "../tests/fixtures/pre_tool_use_write_file.json"
        ))
    }
    pub fn edit_file() -> Value {
        parse(include_str!(
            "../tests/fixtures/pre_tool_use_edit_file.json"
        ))
    }
    pub fn read_file() -> Value {
        parse(include_str!(
            "../tests/fixtures/pre_tool_use_read_file.json"
        ))
    }
    pub fn ask_question() -> Value {
        parse(include_str!(
            "../tests/fixtures/pre_tool_use_ask_question.json"
        ))
    }
    pub fn post_tool_use() -> Value {
        parse(include_str!(
            "../tests/fixtures/post_tool_use_write_file.json"
        ))
    }
    pub fn stop() -> Value {
        parse(include_str!("../tests/fixtures/stop.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WS: &str = "/Users/dev/projects/demo";

    #[test]
    fn reads_the_real_payloads() {
        let hook = fixtures::run_command();
        let input = Input(&hook);
        assert_eq!(input.event(), Some("PreToolUse"));
        assert_eq!(input.session_id(), "32fc0aec-d58e-47c7-b08c-f489e3a4644a");
        assert_eq!(input.cwd(), WS);
        assert_eq!(input.project_name().as_deref(), Some("demo"));
        assert_eq!(input.tool_name().as_deref(), Some("run_command"));
        assert_eq!(input.tool_input()["CommandLine"], "echo first-step");
        assert_eq!(
            input.prompt(),
            Some("Run the shell command 'echo first-step' and tell me what it printed.")
        );
        // Nothing of Codex's or Claude Code's snake_case shape is there
        assert!(hook.get("session_id").is_none() && hook.get("tool_name").is_none());
    }

    #[test]
    fn the_working_directory_falls_back_to_the_first_workspace() {
        let hook = json!({ "conversationId": "c", "workspacePaths": ["/w/one", "/w/two"] });
        assert_eq!(Input(&hook).cwd(), "/w/one");
        assert_eq!(Input(&hook).project_name().as_deref(), Some("one"));
        let hook = json!({ "cwd": "", "workspacePaths": [] });
        assert_eq!(Input(&hook).cwd(), "");
        assert_eq!(Input(&hook).project_name(), None);
        assert_eq!(Input(&hook).session_id(), "unknown");
        // Input that is not an object at all reads as empty, without a panic
        for odd in [json!(null), json!("text"), json!([1, 2]), json!(7)] {
            assert_eq!(Input(&odd).tool_name(), None);
            assert_eq!(Input(&odd).tool_input(), Value::Null);
            assert_eq!(Input(&odd).cwd(), "");
            assert_eq!(Input(&odd).prompt(), None);
        }
        let odd = json!({ "toolCall": "x", "workspacePaths": "y", "cwd": 3 });
        assert_eq!(Input(&odd).tool_name(), None);
        assert_eq!(Input(&odd).cwd(), "");
    }

    #[test]
    fn sorts_tools_by_what_they_can_do() {
        assert_eq!(classify("run_command"), ToolClass::Command);
        for name in [
            "write_file",
            "edit_file",
            "write_to_file",
            "replace_file_content",
            "multi_replace_file_content",
        ] {
            assert_eq!(classify(name), ToolClass::Edit, "{name}");
        }
        assert_eq!(classify("ask_question"), ToolClass::Question);
        // Read-only tools never wait for the phone
        for name in [
            "read_file",
            "list_dir",
            "list_directory",
            "grep_search",
            "find_by_name",
            "view_file",
            "view_file_outline",
            "codebase_search",
            "search_web",
            "read_url_content",
            "command_status",
            "mcp_github_list_issues",
            "mcp_docs_search",
            "mcp_linear_get_issue",
        ] {
            assert_eq!(classify(name), ToolClass::Passive, "{name}");
            assert!(!classify(name).side_effects(), "{name}");
        }
        // Tools that say they change something, and MCP tools that don't say they only look
        for name in [
            "mcp_github_create_issue",
            "mcp_slack_send_message",
            "mcp_db_run",
            "mcp_thing",
            "delete_branch",
            "browser_click",
        ] {
            assert_eq!(classify(name), ToolClass::Other, "{name}");
        }
        // Nothing known: left to Antigravity's own policy
        for name in ["", "Tool", "task_boundary", "notify_user", "think"] {
            assert_eq!(classify(name), ToolClass::Passive, "{name}");
        }
    }

    #[test]
    fn summarises_antigravity_tools() {
        let run = fixtures::run_command();
        assert_eq!(
            tool_summary("run_command", &Input(&run).tool_input(), WS).as_deref(),
            Some("echo first-step")
        );
        let write = fixtures::write_file();
        assert_eq!(
            tool_summary("write_file", &Input(&write).tool_input(), WS).as_deref(),
            Some("Edit notes.txt")
        );
        let edit = fixtures::edit_file();
        assert_eq!(
            tool_summary("edit_file", &Input(&edit).tool_input(), WS).as_deref(),
            Some("Edit notes.txt")
        );
        let read = fixtures::read_file();
        assert_eq!(
            tool_summary("read_file", &Input(&read).tool_input(), WS).as_deref(),
            Some("notes.txt")
        );
        // A file outside the working directory keeps its whole path
        assert_eq!(
            tool_summary("write_file", &json!({ "Path": "/etc/hosts" }), WS).as_deref(),
            Some("Edit /etc/hosts")
        );
        assert_eq!(
            short_path("/Users/dev/projects/demo-two/a", WS),
            "/Users/dev/projects/demo-two/a"
        );
        // Unknown and MCP tools: the tool's own summary, the first text argument, or JSON
        assert_eq!(
            tool_summary(
                "mcp_github_create_issue",
                &json!({ "toolSummary": "Open an issue", "title": "x" }),
                WS
            )
            .as_deref(),
            Some("Open an issue")
        );
        assert_eq!(
            tool_summary(
                "mcp_github_create_issue",
                &json!({ "n": 1, "title": "Crash on start" }),
                WS
            )
            .as_deref(),
            Some("Crash on start")
        );
        assert_eq!(
            tool_summary("mystery", &json!({ "n": 1 }), WS).as_deref(),
            Some("{\"n\":1}")
        );
        assert_eq!(tool_summary("mystery", &json!({}), WS), None);
        assert_eq!(tool_summary("mystery", &Value::Null, WS), None);
        assert_eq!(
            tool_summary("run_command", &json!({ "CommandLine": 7 }), WS),
            None
        );
        assert_eq!(
            display_name("mcp_github_create_issue"),
            "github_create_issue"
        );
        assert_eq!(display_name("run_command"), "run_command");
    }

    #[test]
    fn permission_card_for_a_command() {
        let hook = fixtures::run_command();
        let (card, kind, key) = permission_card(&Input(&hook), true);
        assert_eq!(kind, "permission");
        assert_eq!(card["title"], "Antigravity - demo");
        assert_eq!(card["toolName"], "run_command");
        assert_eq!(card["toolInput"], "echo first-step");
        assert_eq!(
            card["promptText"],
            "run_command: echo first-step\n\nEcho first-step"
        );
        assert_eq!(card["promptType"], "permission");
        assert_eq!(card["options"].as_array().unwrap().len(), 3);
        assert_eq!(card["sessionId"], "32fc0aec-d58e-47c7-b08c-f489e3a4644a");
        assert_eq!(card["projectPath"], WS);
        assert_eq!(card["terminalContext"]["supportsRemoteInput"], true);
        assert_eq!(key, "run_command:echo first-step");

        let hook = fixtures::edit_file();
        let (card, _, key) = permission_card(&Input(&hook), false);
        assert_eq!(card["promptText"], "edit_file: Edit notes.txt");
        assert_eq!(key, "edit_file:Edit notes.txt");

        // A tool call with nothing readable still makes a card
        let hook = json!({ "conversationId": "c", "toolCall": { "name": "mcp_x_do" } });
        let (card, _, _) = permission_card(&Input(&hook), false);
        assert_eq!(card["promptText"], "x_do needs permission");
        assert_eq!(card["title"], "Antigravity");
    }

    #[test]
    fn questions_keep_options_multi_select_and_free_text() {
        let hook = fixtures::ask_question();
        let qs = questions_from_input(&Input(&hook).tool_input());
        assert_eq!(qs.len(), 1);
        assert_eq!(qs[0]["question"], "Which colour do you prefer?");
        assert_eq!(qs[0]["multiSelect"], false);
        assert_eq!(qs[0]["freeText"], false);
        assert_eq!(
            qs[0]["options"],
            json!([
                { "label": "red", "value": "red" },
                { "label": "green", "value": "green" },
                { "label": "blue", "value": "blue" }
            ])
        );
        let (card, key) = question_card(&Input(&hook), &qs, true);
        assert_eq!(card["promptType"], "user_question");
        assert_eq!(card["questions"].as_array().unwrap().len(), 1);
        assert_eq!(card["toolName"], "ask_question");
        assert_eq!(card["title"], "Antigravity - demo");
        assert_eq!(key, "ask_question:Which colour do you prefer?");

        let input = json!({ "questions": [
            { "question": "Which toppings?", "options": ["ham", "olives", ""], "is_multi_select": true },
            { "question": "What is it called?" },
            { "question": "  " },
            { "options": ["a"] },
            "not a question",
            { "question": "Pick", "options": [{ "label": "One", "description": "the first" }, 4] }
        ]});
        let qs = questions_from_input(&input);
        assert_eq!(qs.len(), 3);
        assert_eq!(qs[0]["multiSelect"], true);
        assert_eq!(qs[0]["options"].as_array().unwrap().len(), 2);
        assert_eq!(qs[1]["freeText"], true);
        assert_eq!(qs[2]["options"].as_array().unwrap().len(), 1);
        assert_eq!(qs[2]["options"][0]["description"], "the first");
        // A first question without options is typed
        let (card, _) = question_card(&Input(&hook), &qs[1..], true);
        assert_eq!(card["promptType"], "freeform");
        assert_eq!(card["promptText"], "What is it called?");
        // Nothing usable
        assert!(questions_from_input(&json!({ "questions": "x" })).is_empty());
        assert!(questions_from_input(&Value::Null).is_empty());
        assert!(questions_from_input(&json!("x")).is_empty());
        assert_eq!(first_question(&json!("x")), None);
    }

    #[test]
    fn done_card_prefers_the_final_message() {
        let hook = fixtures::stop();
        let mut state = SessionState::default();
        let card = done_card(&Input(&hook), &state, Some("  All done.  "), None, false);
        assert_eq!(card["notificationType"], "idle_prompt");
        assert_eq!(card["promptText"], "All done.");
        assert_eq!(card["title"], "Antigravity - demo");
        assert_eq!(card["message"], "Antigravity has finished");
        // No final message: the prompt, then the stock line
        state.prompt = Some("Tidy the imports".into());
        let card = done_card(&Input(&hook), &state, None, None, false);
        assert_eq!(card["promptText"], "Finished: Tidy the imports");
        state.prompt = None;
        let card = done_card(&Input(&hook), &state, Some("   "), None, false);
        assert_eq!(
            card["promptText"],
            "Antigravity has finished and is waiting for you."
        );
        // An error says so first
        let card = done_card(
            &Input(&hook),
            &state,
            Some("Half done."),
            Some("quota exceeded"),
            false,
        );
        assert_eq!(card["message"], "Antigravity stopped with an error");
        assert_eq!(
            card["promptText"],
            "Antigravity stopped because of an error: quota exceeded\n\nHalf done."
        );
    }
}
