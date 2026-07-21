//! Mechanical line-to-Event mapping (D-06), generation union, and session
//! assembly (D-05).
//!
//! Events come ONLY from message lines - a positive `{user, assistant}` type
//! whitelist, so unlisted/future line types (`system`, `permission-mode`, hook
//! chatter, ...) never leak an event. `message.content` is EITHER a bare string
//! (common on real user prompts) or a block list, and both shapes expand here.
//! Generations are unioned and deduped by line `uuid` so a `precompact`
//! snapshot's pre-compaction turns survive alongside a later `sweep` (the
//! flagged reconciliation assumption; latest-only would drop them and defeat
//! CAP-03).

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::model::{Event, Grain};

/// Expand every generation's lines into ordered Events.
///
/// `generations` is one `Vec<Value>` per generation, already in read order
/// (parent generations first, then each nested subagent's). `session_id` is the
/// logical session's id, used when a line omits its own `sessionId`.
pub fn assemble_events(generations: &[Vec<Value>], session_id: &str) -> Vec<Event> {
    // Union + dedup by uuid, preserving generation/line order (keep first seen).
    let mut seen: HashSet<String> = HashSet::new();
    let mut ordered: Vec<&Value> = Vec::new();
    for generation in generations {
        for line in generation {
            if let Some(uuid) = line.get("uuid").and_then(Value::as_str) {
                if !uuid.is_empty() && !seen.insert(uuid.to_string()) {
                    continue;
                }
            }
            ordered.push(line);
        }
    }

    // Pass 1: tool_use_id -> answering tool name, and the set of subagent_type
    // values seen across the session's Agent tool_use calls.
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut subagent_types: HashSet<String> = HashSet::new();
    for line in &ordered {
        if !is_message_line(line) {
            continue;
        }
        if let Some(blocks) = content_blocks(line) {
            for block in blocks {
                if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                    continue;
                }
                if let (Some(id), Some(name)) = (
                    block.get("id").and_then(Value::as_str),
                    block.get("name").and_then(Value::as_str),
                ) {
                    tool_names.insert(id.to_string(), name.to_string());
                    if name == "Agent" || name == "Task" {
                        if let Some(t) = block
                            .get("input")
                            .and_then(|i| i.get("subagent_type"))
                            .and_then(Value::as_str)
                        {
                            if !t.is_empty() {
                                subagent_types.insert(t.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    // Best-effort, session-scoped: an unambiguous single subagent_type is
    // attributed to every sidechain event; multiple distinct types are
    // ambiguous -> None (same posture as end_reason).
    let session_agent_type: Option<String> = if subagent_types.len() == 1 {
        subagent_types.into_iter().next()
    } else {
        None
    };

    // Pass 2: expand each whitelisted message line into Events.
    let mut events = Vec::new();
    for line in &ordered {
        if !is_message_line(line) {
            continue;
        }
        expand_line(line, session_id, &tool_names, &session_agent_type, &mut events);
    }
    events
}

/// Positive whitelist: only `user` / `assistant` lines produce events.
fn is_message_line(line: &Value) -> bool {
    matches!(
        line.get("type").and_then(Value::as_str),
        Some("user") | Some("assistant")
    )
}

/// The `message.content` array, when content is a block list (not a bare string).
fn content_blocks(line: &Value) -> Option<&Vec<Value>> {
    line.get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
}

fn expand_line(
    line: &Value,
    session_id: &str,
    tool_names: &HashMap<String, String>,
    session_agent_type: &Option<String>,
    events: &mut Vec<Event>,
) {
    let uuid = line
        .get("uuid")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let parent_uuid = line
        .get("parentUuid")
        .and_then(Value::as_str)
        .map(str::to_string);
    let sess = line
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(session_id)
        .to_string();
    let role = line
        .get("message")
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str)
        .or_else(|| line.get("type").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    let timestamp = line
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_string);
    let is_sidechain = line
        .get("isSidechain")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let agent_id = line.get("agentId").and_then(Value::as_str).map(str::to_string);
    let attribution = attribution(line);
    let agent_type = if is_sidechain {
        session_agent_type.clone()
    } else {
        None
    };

    let mut push = |kind: &str,
                    text: Option<String>,
                    tool_name: Option<String>,
                    is_error: Option<bool>| {
        // Grain is a placeholder here; Task 3 assigns the real three-tier grain
        // and drops archive-only events during this expansion.
        events.push(Event {
            uuid: uuid.clone(),
            parent_uuid: parent_uuid.clone(),
            session_id: sess.clone(),
            role: role.clone(),
            kind: kind.to_string(),
            timestamp: timestamp.clone(),
            text,
            tool_name,
            is_error,
            attribution: attribution.clone(),
            is_sidechain,
            agent_id: agent_id.clone(),
            agent_type: agent_type.clone(),
            grain: Grain::Indexed,
        });
    };

    let content = line.get("message").and_then(|m| m.get("content"));
    match content {
        // Bare string (common on real user prompts): one text Event.
        Some(Value::String(s)) => push("text", Some(s.clone()), None, None),
        // Block list: one Event per block.
        Some(Value::Array(blocks)) => {
            for block in blocks {
                expand_block(block, tool_names, &mut push);
            }
        }
        _ => {}
    }
}

fn expand_block<F>(block: &Value, tool_names: &HashMap<String, String>, push: &mut F)
where
    F: FnMut(&str, Option<String>, Option<String>, Option<bool>),
{
    let btype = block.get("type").and_then(Value::as_str).unwrap_or("");
    match btype {
        "text" => push("text", block.get("text").and_then(Value::as_str).map(str::to_string), None, None),
        "thinking" => {
            let text = block
                .get("thinking")
                .and_then(Value::as_str)
                .or_else(|| block.get("text").and_then(Value::as_str))
                .map(str::to_string);
            push("thinking", text, None, None);
        }
        "tool_use" => {
            let name = block.get("name").and_then(Value::as_str).map(str::to_string);
            let summary = block.get("input").and_then(tool_use_summary);
            push("tool_use", summary, name, None);
        }
        "tool_result" => {
            let tool_name = block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .and_then(|id| tool_names.get(id).cloned());
            let is_error = block.get("is_error").and_then(Value::as_bool);
            let text = block.get("content").and_then(result_body);
            push("tool_result", text, tool_name, is_error);
        }
        _ => {}
    }
}

/// A compact one-line summary of a tool_use `input`: the salient identifying arg
/// (file_path / command / pattern / ...), never a body like `content`.
fn tool_use_summary(input: &Value) -> Option<String> {
    for key in [
        "file_path",
        "command",
        "pattern",
        "query",
        "url",
        "path",
        "notebook_path",
    ] {
        if let Some(s) = input.get(key).and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }
    None
}

/// A tool_result `content` body: a bare string, or the joined text parts of a
/// block list.
fn result_body(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = content.as_array() {
        let mut parts = Vec::new();
        for item in arr {
            if let Some(s) = item.get("text").and_then(Value::as_str) {
                parts.push(s.to_string());
            } else if let Some(s) = item.as_str() {
                parts.push(s.to_string());
            }
        }
        if !parts.is_empty() {
            return Some(parts.join("\n"));
        }
    }
    None
}

/// Whichever of the attribution fields is present.
fn attribution(line: &Value) -> Option<String> {
    for key in ["attributionSkill", "attributionAgent", "attributionPlugin"] {
        match line.get(key) {
            None => continue,
            Some(Value::Null) => continue,
            Some(Value::String(s)) if !s.is_empty() => return Some(s.clone()),
            Some(Value::String(_)) => continue,
            Some(other) => {
                if let Some(name) = other.get("name").and_then(Value::as_str) {
                    return Some(name.to_string());
                }
                return Some(other.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shared_uuid_across_generations_emits_once() {
        let gen_a = vec![json!({
            "type": "user", "uuid": "u1", "sessionId": "s",
            "message": {"role": "user", "content": "hello"}
        })];
        let gen_b = vec![json!({
            "type": "user", "uuid": "u1", "sessionId": "s",
            "message": {"role": "user", "content": "hello"}
        })];
        let events = assemble_events(&[gen_a, gen_b], "s");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn assistant_blocks_expand_to_three_events() {
        let line = json!({
            "type": "assistant", "uuid": "a1", "sessionId": "s",
            "message": {"role": "assistant", "content": [
                {"type": "text", "text": "answer"},
                {"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "/x"}},
                {"type": "thinking", "thinking": "hmm"}
            ]}
        });
        let events = assemble_events(&[vec![line]], "s");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].kind, "text");
        assert_eq!(events[1].kind, "tool_use");
        assert_eq!(events[1].tool_name.as_deref(), Some("Read"));
        assert_eq!(events[2].kind, "thinking");
    }

    #[test]
    fn tool_result_resolves_answering_tool_name() {
        let line = json!({
            "type": "assistant", "uuid": "a1", "sessionId": "s",
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "tid", "name": "Bash", "input": {"command": "ls"}},
                {"type": "tool_result", "tool_use_id": "tid", "content": "file listing", "is_error": false}
            ]}
        });
        let events = assemble_events(&[vec![line]], "s");
        let result = events.iter().find(|e| e.kind == "tool_result").unwrap();
        assert_eq!(result.tool_name.as_deref(), Some("Bash"));
        assert_eq!(result.is_error, Some(false));
    }

    #[test]
    fn unmatched_tool_use_id_yields_none_tool_name() {
        let line = json!({
            "type": "user", "uuid": "u1", "sessionId": "s",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "missing", "content": "x"}
            ]}
        });
        let events = assemble_events(&[vec![line]], "s");
        let result = events.iter().find(|e| e.kind == "tool_result").unwrap();
        assert!(result.tool_name.is_none());
    }

    #[test]
    fn sidechain_events_carry_flag_and_agent_id() {
        let parent = vec![json!({
            "type": "assistant", "uuid": "p1", "sessionId": "s",
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "Agent", "input": {"subagent_type": "reviewer"}}
            ]}
        })];
        let nested = vec![json!({
            "type": "assistant", "uuid": "n1", "sessionId": "s",
            "isSidechain": true, "agentId": "agent-xyz",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "sub work"}]}
        })];
        let events = assemble_events(&[parent, nested], "s");
        let sub = events.iter().find(|e| e.uuid == "n1").unwrap();
        assert!(sub.is_sidechain);
        assert_eq!(sub.agent_id.as_deref(), Some("agent-xyz"));
        assert_eq!(sub.agent_type.as_deref(), Some("reviewer"));
    }

    #[test]
    fn bare_string_content_emits_one_text_event() {
        let line = json!({
            "type": "user", "uuid": "u1", "sessionId": "s",
            "message": {"role": "user", "content": "yes"}
        });
        let events = assemble_events(&[vec![line]], "s");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "text");
        assert_eq!(events[0].text.as_deref(), Some("yes"));
    }

    #[test]
    fn non_message_line_types_emit_no_event() {
        let system = json!({"type": "system", "uuid": "x1", "content": "boot"});
        let mode = json!({"type": "permission-mode", "uuid": "x2", "mode": "plan"});
        let events = assemble_events(&[vec![system, mode]], "s");
        assert!(events.is_empty());
    }
}
