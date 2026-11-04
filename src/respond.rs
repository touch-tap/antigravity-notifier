//! Turns what the phone sent into what Codex's hook contract expects on stdout.

use serde_json::{Value, json};

fn normalise(response: &str) -> String {
    response.trim().to_ascii_lowercase()
}

/// PermissionRequest: the phone sends `y` / `a` / `n` or free text. Free text is a deny with the
/// text as the reason, so Codex reads it. `a` (allow for the session) is an allow: Codex's
/// decision has no "remember" form, so the next request asks again.
pub fn permission_output(response: &str) -> Value {
    let decision = match normalise(response).as_str() {
        "y" | "yes" | "allow" | "approve" | "ok" | "a" | "always" => {
            json!({ "behavior": "allow" })
        }
        "n" | "no" | "deny" | "reject" => {
            json!({ "behavior": "deny", "message": "Denied from the phone" })
        }
        _ => {
            json!({ "behavior": "deny", "message": format!("From the phone: {}", response.trim()) })
        }
    };
    json!({ "hookSpecificOutput": { "hookEventName": "PermissionRequest", "decision": decision } })
}

/// `request_user_input`: the phone sends one option label, free text, or `[[MULTI_ANSWER]]`
/// followed by a JSON array with one entry per question: a 1-based option number, or
/// `OTHER:N:text`.
///
/// Codex's tool reads its answers from the terminal, not from its input, so the hook cannot hand
/// them over the way Claude Code's `updatedInput` does. Instead the call is blocked with the
/// answers as the reason: the model reads them as the tool result and carries on. Returns None
/// when nothing usable could be mapped, so the terminal asks as normal.
pub fn question_output(response: &str, questions: &[Value]) -> Option<Value> {
    let mut answers: Vec<(String, String)> = Vec::new();
    let trimmed = response.trim();

    if let Some(rest) = trimmed.strip_prefix("[[MULTI_ANSWER]]") {
        let list: Vec<String> = serde_json::from_str(rest).ok()?;
        for (i, raw) in list.iter().enumerate() {
            let Some(q) = questions.get(i) else { break };
            let Some(answer) = map_answer(raw, q) else {
                continue;
            };
            answers.push((label_of(q), answer));
        }
    } else {
        let q = questions.first()?;
        let answer = map_answer(trimmed, q)?;
        answers.push((label_of(q), answer));
    }
    if answers.is_empty() {
        return None;
    }
    let mut reason = String::from(
        "The user has already answered this on their phone, so do not ask again. Their answers:\n",
    );
    for (label, answer) in &answers {
        reason.push_str(&format!("- {label}: {answer}\n"));
    }
    reason.push_str("Continue with these answers.");
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    }))
}

fn label_of(question: &Value) -> String {
    question["question"]
        .as_str()
        .or_else(|| question["header"].as_str())
        .unwrap_or("Answer")
        .to_string()
}

fn map_answer(raw: &str, question: &Value) -> Option<String> {
    let labels: Vec<&str> = question["options"]
        .as_array()
        .map(|opts| opts.iter().filter_map(|o| o["label"].as_str()).collect())
        .unwrap_or_default();
    if let Some(rest) = raw.strip_prefix("OTHER:") {
        let text = rest.split_once(':').map(|(_, t)| t).unwrap_or(rest).trim();
        return if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        };
    }
    if let Ok(n) = raw.trim().parse::<usize>() {
        if n >= 1 {
            if let Some(label) = labels.get(n - 1) {
                return Some(label.to_string());
            }
        }
    }
    if let Some(label) = labels.iter().find(|l| l.eq_ignore_ascii_case(raw.trim())) {
        return Some(label.to_string());
    }
    // Free text typed on the phone becomes the answer as written
    let text = raw.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Stop: free text from the phone becomes Codex's next instruction.
pub fn stop_output(response: &str) -> Option<Value> {
    let text = response.trim();
    if text.is_empty()
        || matches!(
            normalise(text).as_str(),
            "ok" | "done" | "thanks" | "y" | "n"
        )
    {
        return None;
    }
    Some(json!({ "decision": "block", "reason": text }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn questions() -> Vec<Value> {
        vec![
            json!({ "question": "Which framework?", "header": "Framework", "multiSelect": false,
            "options": [{ "label": "React", "value": "React" }, { "label": "Vue", "value": "Vue" }] }),
            json!({ "question": "What is it called?", "header": "Name", "options": [], "freeText": true }),
        ]
    }

    #[test]
    fn maps_a_numbered_multi_answer_into_a_deny_reason() {
        let out = question_output("[[MULTI_ANSWER]][\"2\",\"OTHER:1:Pinloop\"]", &questions()).unwrap();
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
        let reason = out["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap();
        assert!(reason.contains("- Which framework?: Vue"), "{reason}");
        assert!(reason.contains("- What is it called?: Pinloop"), "{reason}");
    }

    #[test]
    fn maps_a_single_label_or_free_text() {
        let out = question_output("vue", &questions()).unwrap();
        assert!(out["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().contains(": Vue"));
        let out = question_output("Svelte please", &questions()[1..]).unwrap();
        assert!(out["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().contains("called?: Svelte please"));
        assert!(question_output("   ", &questions()).is_none());
    }

    #[test]
    fn permission_values() {
        assert_eq!(permission_output("y")["hookSpecificOutput"]["decision"]["behavior"], "allow");
        assert_eq!(permission_output("a")["hookSpecificOutput"]["decision"]["behavior"], "allow");
        assert_eq!(permission_output("n")["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert_eq!(
            permission_output("use pnpm instead")["hookSpecificOutput"]["decision"]["message"],
            "From the phone: use pnpm instead"
        );
    }

    #[test]
    fn stop_block() {
        assert!(stop_output("ok").is_none());
        assert_eq!(stop_output("now write tests").unwrap()["reason"], "now write tests");
    }
}
