//! Turns what the phone sent into what Antigravity's hook contract expects on stdout.
//!
//! Measured on agy 1.5.18 (26 Nov 2026). A PreToolUse hook answers `{"decision": ...}`:
//! `allow` defers to Antigravity's own permission policy, `auto_approve` runs the tool with no
//! local prompt, `deny` blocks it and hands `reason` to the model word for word, `force_ask`
//! shows the local prompt with `reason` on it. An answer with no decision (`{}`) BLOCKS the tool,
//! so every PreToolUse path ends in one of these. A Stop hook answers `{}` to let the turn end,
//! or `{"decision": "continue", "reason": ...}` to start another turn with `reason` as a system
//! message.

use rust_i18n::t;
use serde_json::{Value, json};

fn normalise(response: &str) -> String {
    response.trim().to_ascii_lowercase()
}

/// PreToolUse: nothing to say, so Antigravity decides as it would have without us.
pub fn allow() -> Value {
    json!({ "decision": "allow" })
}

/// PreToolUse: approved on the phone, so no local prompt.
pub fn auto_approve() -> Value {
    json!({ "decision": "auto_approve" })
}

/// PreToolUse: the hold ended with no answer, so the local prompt takes over. `reason` is shown
/// to the user on that prompt, so it is in their language.
pub fn force_ask() -> Value {
    json!({ "decision": "force_ask", "reason": t!("hook.no_answer") })
}

/// PreToolUse: the phone was going to be asked and could not be (the card could not be sent, the server
/// did not answer, something broke on the way). The engine never answers for the user, so the local
/// prompt takes over, saying why.
pub fn hand_back() -> Value {
    json!({ "decision": "force_ask", "reason": t!("hook.unreachable") })
}

/// Any other event with nothing to say
pub fn nothing() -> Value {
    json!({})
}

/// What the phone decided about a tool call.
#[derive(Debug, PartialEq)]
pub struct Approval {
    pub output: Value,
    pub approved: bool,
    /// "Allow for session": the same tool needn't ask again in this conversation
    pub for_session: bool,
}

/// A tool call's card: the phone sends `y` / `a` / `n` or free text. Free text is a deny with
/// the text as the reason, so the model reads it.
pub fn permission_output(response: &str) -> Approval {
    let deny = |reason: String| Approval {
        output: json!({ "decision": "deny", "reason": reason }),
        approved: false,
        for_session: false,
    };
    match normalise(response).as_str() {
        "y" | "yes" | "allow" | "approve" | "ok" => Approval {
            output: auto_approve(),
            approved: true,
            for_session: false,
        },
        "a" | "always" => Approval {
            output: auto_approve(),
            approved: true,
            for_session: true,
        },
        "n" | "no" | "deny" | "reject" | "" => {
            deny("The user denied this from their phone.".to_string())
        }
        _ => deny(format!(
            "The user denied this from their phone and said: {}",
            response.trim()
        )),
    }
}

/// `ask_question`: the phone sends one option label, free text, or `[[MULTI_ANSWER]]` followed
/// by a JSON array with one entry per question: a 1-based option number, `OTHER:N:text`, or for
/// a multi-select question several of those separated by commas.
///
/// The tool reads its answers from the terminal, so the hook cannot hand them over as the
/// tool's result. Instead the call is denied with the answers as the reason: the model reads
/// them and carries on (measured: it does, and does not ask again). Returns None when nothing
/// usable could be mapped, so the terminal asks as normal.
pub fn question_output(response: &str, questions: &[Value]) -> Option<Value> {
    let mut answers: Vec<(String, String)> = Vec::new();
    let trimmed = response.trim();

    if let Some(rest) = trimmed.strip_prefix("[[MULTI_ANSWER]]") {
        let list: Vec<Value> = serde_json::from_str(rest.trim()).ok()?;
        for (i, raw) in list.iter().enumerate() {
            let Some(q) = questions.get(i) else { break };
            // An entry is text, a number, or a list of either for a multi-select question
            let raw = match raw {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Array(items) => items
                    .iter()
                    .filter_map(|item| match item {
                        Value::String(s) => Some(s.clone()),
                        Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(","),
                _ => continue,
            };
            let Some(answer) = map_answer(&raw, q) else {
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
    Some(json!({ "decision": "deny", "reason": reason }))
}

fn label_of(question: &Value) -> String {
    question["question"]
        .as_str()
        .or_else(|| question["header"].as_str())
        .unwrap_or("Answer")
        .to_string()
}

fn labels_of(question: &Value) -> Vec<&str> {
    question["options"]
        .as_array()
        .map(|opts| opts.iter().filter_map(|o| o["label"].as_str()).collect())
        .unwrap_or_default()
}

/// One answer: `OTHER:N:text`, a 1-based option number, an option's label, or free text.
fn map_one(raw: &str, labels: &[&str]) -> Option<String> {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix("OTHER:") {
        let text = rest.split_once(':').map(|(_, t)| t).unwrap_or(rest).trim();
        return (!text.is_empty()).then(|| text.to_string());
    }
    if let Ok(n) = raw.parse::<usize>()
        && n >= 1
        && let Some(label) = labels.get(n - 1)
    {
        return Some(label.to_string());
    }
    if let Some(label) = labels.iter().find(|l| l.eq_ignore_ascii_case(raw)) {
        return Some(label.to_string());
    }
    // Free text typed on the phone becomes the answer as written
    (!raw.is_empty()).then(|| raw.to_string())
}

fn map_answer(raw: &str, question: &Value) -> Option<String> {
    let labels = labels_of(question);
    let raw = raw.trim();
    let multi = question["multiSelect"].as_bool().unwrap_or(false);
    // Several choices arrive as "1,3": only when every part is a number or a label, so a typed
    // sentence with a comma in it stays one answer
    if multi && raw.contains(',') && !raw.starts_with("OTHER:") {
        let parts: Vec<&str> = raw
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        let is_choice = |part: &&str| {
            part.parse::<usize>()
                .is_ok_and(|n| n >= 1 && n <= labels.len())
                || labels.iter().any(|l| l.eq_ignore_ascii_case(part))
                || part.starts_with("OTHER:")
        };
        if !parts.is_empty() && parts.iter().all(is_choice) {
            let mut chosen: Vec<String> = Vec::new();
            for part in parts {
                if let Some(answer) = map_one(part, &labels)
                    && !chosen.contains(&answer)
                {
                    chosen.push(answer);
                }
            }
            return (!chosen.is_empty()).then(|| chosen.join(", "));
        }
    }
    map_one(raw, &labels)
}

/// Stop: free text from the phone becomes Antigravity's next instruction. The reason arrives as
/// a system message, so it says whose words these are.
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
    Some(json!({
        "decision": "continue",
        "reason": format!("The user replied from their phone: {text}"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn questions() -> Vec<Value> {
        vec![
            json!({ "question": "Which framework?", "header": "Question", "multiSelect": false,
            "options": [{ "label": "React", "value": "React" }, { "label": "Vue", "value": "Vue" }] }),
            json!({ "question": "What is it called?", "header": "Question", "options": [], "freeText": true }),
            json!({ "question": "Which toppings?", "header": "Question", "multiSelect": true,
            "options": [{ "label": "ham" }, { "label": "olives" }, { "label": "onion" }] }),
        ]
    }

    #[test]
    fn every_pre_tool_answer_carries_a_decision() {
        // `{}` blocks the tool in Antigravity, so none of these may be without one
        for output in [
            allow(),
            auto_approve(),
            force_ask(),
            permission_output("y").output,
            permission_output("a").output,
            permission_output("n").output,
            permission_output("").output,
            permission_output("use pnpm").output,
            question_output("vue", &questions()).unwrap(),
        ] {
            let decision = output["decision"].as_str().unwrap_or_default();
            assert!(
                ["allow", "auto_approve", "force_ask", "deny"].contains(&decision),
                "{output}"
            );
        }
        assert_eq!(allow(), json!({ "decision": "allow" }));
        assert_eq!(force_ask()["reason"], "No answer from your phone");
    }

    #[test]
    fn maps_a_numbered_multi_answer_into_a_deny_reason() {
        let out =
            question_output("[[MULTI_ANSWER]][\"2\",\"OTHER:1:Pinloop\"]", &questions()).unwrap();
        assert_eq!(out["decision"], "deny");
        let reason = out["reason"].as_str().unwrap();
        assert!(reason.contains("do not ask again"), "{reason}");
        assert!(reason.contains("- Which framework?: Vue"), "{reason}");
        assert!(reason.contains("- What is it called?: Pinloop"), "{reason}");
        assert!(!reason.contains("toppings"), "{reason}");
    }

    #[test]
    fn maps_a_single_label_or_free_text() {
        let out = question_output("vue", &questions()).unwrap();
        assert!(out["reason"].as_str().unwrap().contains(": Vue"));
        let out = question_output("2", &questions()).unwrap();
        assert!(out["reason"].as_str().unwrap().contains(": Vue"));
        let out = question_output("Svelte please", &questions()[1..]).unwrap();
        assert!(
            out["reason"]
                .as_str()
                .unwrap()
                .contains("called?: Svelte please")
        );
        assert!(question_output("   ", &questions()).is_none());
        assert!(question_output("x", &[]).is_none());
        assert!(question_output("[[MULTI_ANSWER]]not json", &questions()).is_none());
        assert!(question_output("[[MULTI_ANSWER]][]", &questions()).is_none());
    }

    #[test]
    fn multi_select_answers_list_every_choice() {
        let toppings = &questions()[2..];
        for response in [
            "1,3",
            "1, 3",
            "ham,onion",
            "[[MULTI_ANSWER]][\"1,3\"]",
            "[[MULTI_ANSWER]][[1,3]]",
            "[[MULTI_ANSWER]][[\"ham\",\"onion\"]]",
        ] {
            let out = question_output(response, toppings).unwrap();
            assert!(
                out["reason"]
                    .as_str()
                    .unwrap()
                    .contains("- Which toppings?: ham, onion\n"),
                "{response}: {out}"
            );
        }
        // One choice, and a typed answer with a comma in it
        let out = question_output("2", toppings).unwrap();
        assert!(out["reason"].as_str().unwrap().contains(": olives\n"));
        let out = question_output("none, I'm vegan", toppings).unwrap();
        assert!(
            out["reason"]
                .as_str()
                .unwrap()
                .contains(": none, I'm vegan\n")
        );
        // A comma answer to a single-select question is free text
        let out = question_output("1,2", &questions()).unwrap();
        assert!(out["reason"].as_str().unwrap().contains(": 1,2\n"));
        // All three questions at once
        let out = question_output("[[MULTI_ANSWER]][\"1\",\"OTHER:1:Zed\",\"2,3\"]", &questions())
            .unwrap();
        let reason = out["reason"].as_str().unwrap();
        assert!(reason.contains(": React\n") && reason.contains(": Zed\n"));
        assert!(reason.contains(": olives, onion\n"), "{reason}");
    }

    #[test]
    fn permission_values() {
        let yes = permission_output("y");
        assert_eq!(yes.output, json!({ "decision": "auto_approve" }));
        assert!(yes.approved && !yes.for_session);
        let always = permission_output(" A ");
        assert_eq!(always.output["decision"], "auto_approve");
        assert!(always.approved && always.for_session);
        let no = permission_output("n");
        assert_eq!(no.output["decision"], "deny");
        assert_eq!(no.output["reason"], "The user denied this from their phone.");
        assert!(!no.approved);
        let text = permission_output("use pnpm instead");
        assert_eq!(text.output["decision"], "deny");
        assert_eq!(
            text.output["reason"],
            "The user denied this from their phone and said: use pnpm instead"
        );
    }

    #[test]
    fn stop_continue() {
        assert!(stop_output("ok").is_none());
        assert!(stop_output("  ").is_none());
        let out = stop_output(" now write tests ").unwrap();
        assert_eq!(out["decision"], "continue");
        assert_eq!(
            out["reason"],
            "The user replied from their phone: now write tests"
        );
    }
}
