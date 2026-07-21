//! Mechanical Artifact and Mention extraction from tool-call INPUTS and answer
//! text only, never tool_result bodies (D-06, D-10).
//!
//! Mentions are high-confidence structural references: a `file` for each
//! Read/Edit/Write `file_path`, and a `command` equal to argv[0] of each Bash
//! `command` (env-assignment prefixes skipped, the token's full path kept, so
//! the entity equals argv[0] exactly). Artifacts are the file bodies produced
//! in the run: Write/Edit content, Bash heredoc bodies, and fenced code blocks
//! in assistant text. Package/symbol/prose mentions are deferred (D-10).

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::model::{Artifact, Mention};

/// Walk the session's ordered, deduped lines and extract Mentions and Artifacts.
pub fn extract(
    generations: &[Vec<Value>],
    session_id: &str,
    project: &str,
) -> (Vec<Mention>, Vec<Artifact>) {
    // Union + dedup by uuid, preserving generation/line order (matches parse).
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

    let mut mentions: Vec<Mention> = Vec::new();
    let mut artifacts: Vec<Artifact> = Vec::new();
    // Per-source-event artifact index, so artifact_id is stable across runs.
    let mut counters: HashMap<String, usize> = HashMap::new();
    // The uuid of the nearest preceding user-prompt event (role=user, kind=text).
    let mut last_user_prompt: Option<String> = None;

    for line in &ordered {
        if !is_message_line(line) || super::grain::is_archive_only_line(line) {
            continue;
        }
        let uuid = line
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let timestamp = line
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_string);
        let role = line
            .get("message")
            .and_then(|m| m.get("role"))
            .and_then(Value::as_str)
            .or_else(|| line.get("type").and_then(Value::as_str))
            .unwrap_or("");
        let content = line.get("message").and_then(|m| m.get("content"));

        match content {
            Some(Value::String(s)) => {
                if role == "user" {
                    last_user_prompt = Some(uuid.clone());
                } else if role == "assistant" {
                    push_fenced(s, &uuid, &last_user_prompt, &mut counters, &mut artifacts);
                }
            }
            Some(Value::Array(blocks)) => {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                            if role == "user" {
                                last_user_prompt = Some(uuid.clone());
                            } else if role == "assistant" {
                                push_fenced(
                                    text,
                                    &uuid,
                                    &last_user_prompt,
                                    &mut counters,
                                    &mut artifacts,
                                );
                            }
                        }
                        Some("tool_use") => {
                            extract_tool_use(
                                block,
                                &uuid,
                                &timestamp,
                                session_id,
                                project,
                                &last_user_prompt,
                                &mut counters,
                                &mut mentions,
                                &mut artifacts,
                            );
                        }
                        // tool_result bodies are never mined.
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    (mentions, artifacts)
}

#[allow(clippy::too_many_arguments)]
fn extract_tool_use(
    block: &Value,
    event_uuid: &str,
    timestamp: &Option<String>,
    session_id: &str,
    project: &str,
    request_bundle: &Option<String>,
    counters: &mut HashMap<String, usize>,
    mentions: &mut Vec<Mention>,
    artifacts: &mut Vec<Artifact>,
) {
    let name = block.get("name").and_then(Value::as_str).unwrap_or("");
    let input = match block.get("input") {
        Some(i) => i,
        None => return,
    };

    match name {
        "Read" | "Edit" | "Write" => {
            if let Some(file_path) = input.get("file_path").and_then(Value::as_str) {
                mentions.push(Mention {
                    entity: file_path.to_string(),
                    entity_type: "file".to_string(),
                    event_uuid: event_uuid.to_string(),
                    session_id: session_id.to_string(),
                    project: project.to_string(),
                    timestamp: timestamp.clone(),
                });
            }
        }
        "Bash" => {
            if let Some(command) = input.get("command").and_then(Value::as_str) {
                if let Some(argv0) = argv0(command) {
                    mentions.push(Mention {
                        entity: argv0,
                        entity_type: "command".to_string(),
                        event_uuid: event_uuid.to_string(),
                        session_id: session_id.to_string(),
                        project: project.to_string(),
                        timestamp: timestamp.clone(),
                    });
                }
                for body in heredoc_bodies(command) {
                    push_artifact(
                        artifacts,
                        counters,
                        event_uuid,
                        "snippet",
                        None,
                        None,
                        body,
                        request_bundle,
                    );
                }
            }
        }
        _ => {}
    }

    // File-body artifacts from Write/Edit inputs.
    let (path, content) = match name {
        "Write" => (
            input.get("file_path").and_then(Value::as_str),
            input.get("content").and_then(Value::as_str),
        ),
        "Edit" => (
            input.get("file_path").and_then(Value::as_str),
            input.get("new_string").and_then(Value::as_str),
        ),
        _ => (None, None),
    };
    if let (Some(path), Some(content)) = (path, content) {
        push_artifact(
            artifacts,
            counters,
            event_uuid,
            "file",
            Some(path.to_string()),
            ext_language(path),
            content.to_string(),
            request_bundle,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn push_artifact(
    artifacts: &mut Vec<Artifact>,
    counters: &mut HashMap<String, usize>,
    source_event_uuid: &str,
    kind: &str,
    path: Option<String>,
    language: Option<String>,
    content: String,
    request_bundle: &Option<String>,
) {
    let n = counters.entry(source_event_uuid.to_string()).or_insert(0);
    let artifact_id = format!("{source_event_uuid}-{n}");
    *n += 1;
    artifacts.push(Artifact {
        artifact_id,
        kind: kind.to_string(),
        path,
        language,
        content,
        request_bundle: request_bundle.clone(),
        source_event_uuid: source_event_uuid.to_string(),
    });
}

/// Emit a snippet artifact for each fenced triple-backtick code block in `text`.
fn push_fenced(
    text: &str,
    event_uuid: &str,
    request_bundle: &Option<String>,
    counters: &mut HashMap<String, usize>,
    artifacts: &mut Vec<Artifact>,
) {
    for (language, body) in fenced_blocks(text) {
        push_artifact(
            artifacts,
            counters,
            event_uuid,
            "snippet",
            None,
            language,
            body,
            request_bundle,
        );
    }
}

fn is_message_line(line: &Value) -> bool {
    matches!(
        line.get("type").and_then(Value::as_str),
        Some("user") | Some("assistant")
    )
}

/// argv[0] of a shell command: the first whitespace-delimited token after
/// skipping leading `VAR=value` env-assignment prefixes, path kept intact.
fn argv0(command: &str) -> Option<String> {
    for token in command.split_whitespace() {
        if is_env_assignment(token) {
            continue;
        }
        return Some(token.to_string());
    }
    None
}

fn is_env_assignment(token: &str) -> bool {
    match token.find('=') {
        Some(0) | None => false,
        Some(eq) => token[..eq].chars().enumerate().all(|(i, c)| {
            if i == 0 {
                c == '_' || c.is_ascii_alphabetic()
            } else {
                c == '_' || c.is_ascii_alphanumeric()
            }
        }),
    }
}

/// The lowercased file extension of `path`, when it has one (not a dotfile).
fn ext_language(path: &str) -> Option<String> {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let dot = name.rfind('.')?;
    if dot == 0 {
        return None;
    }
    let ext = &name[dot + 1..];
    if ext.is_empty() {
        None
    } else {
        Some(ext.to_lowercase())
    }
}

/// Bodies of every `<<DELIM ... DELIM` heredoc in a shell command (quoted or
/// unquoted delimiter, `<<-` accepted).
fn heredoc_bodies(command: &str) -> Vec<String> {
    let lines: Vec<&str> = command.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some(delim) = heredoc_delim(lines[i]) {
            let mut body = Vec::new();
            let mut j = i + 1;
            let mut closed = false;
            while j < lines.len() {
                if lines[j].trim() == delim {
                    closed = true;
                    break;
                }
                body.push(lines[j]);
                j += 1;
            }
            if closed {
                out.push(body.join("\n"));
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn heredoc_delim(line: &str) -> Option<String> {
    let idx = line.find("<<")?;
    let rest = line[idx + 2..].trim_start();
    let rest = rest.strip_prefix('-').unwrap_or(rest).trim_start();
    let rest = rest.trim_start_matches(['\'', '"']);
    let word: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if word.is_empty() {
        None
    } else {
        Some(word)
    }
}

/// (language, body) for every fenced triple-backtick block in `text`.
fn fenced_blocks(text: &str) -> Vec<(Option<String>, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some(rest) = lines[i].trim_start().strip_prefix("```") {
            let tag = rest.trim();
            let language = if tag.is_empty() {
                None
            } else {
                Some(tag.to_string())
            };
            let mut body = Vec::new();
            let mut j = i + 1;
            let mut closed = false;
            while j < lines.len() {
                if lines[j].trim_start().starts_with("```") {
                    closed = true;
                    break;
                }
                body.push(lines[j]);
                j += 1;
            }
            if closed {
                out.push((language, body.join("\n")));
                i = j + 1;
                continue;
            }
            break;
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn gen(lines: Vec<Value>) -> Vec<Vec<Value>> {
        vec![lines]
    }

    #[test]
    fn write_yields_file_mention_and_artifact_with_content() {
        let line = json!({
            "type": "assistant", "uuid": "a1", "sessionId": "s",
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "Write",
                 "input": {"file_path": "/repo/src/lib.rs", "content": "fn main() {}"}}
            ]}
        });
        let (mentions, artifacts) = extract(&gen(vec![line]), "s", "proj");
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].entity, "/repo/src/lib.rs");
        assert_eq!(mentions[0].entity_type, "file");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].content, "fn main() {}");
        assert_eq!(artifacts[0].path.as_deref(), Some("/repo/src/lib.rs"));
        assert_eq!(artifacts[0].language.as_deref(), Some("rs"));
        assert_eq!(artifacts[0].artifact_id, "a1-0");
    }

    #[test]
    fn bash_command_mention_is_argv0() {
        let line = json!({
            "type": "assistant", "uuid": "a1", "sessionId": "s",
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "cargo test foo"}}
            ]}
        });
        let (mentions, _) = extract(&gen(vec![line]), "s", "proj");
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].entity, "cargo");
        assert_eq!(mentions[0].entity_type, "command");
    }

    #[test]
    fn bash_command_mention_skips_env_prefix_keeps_path() {
        let line = json!({
            "type": "assistant", "uuid": "a1", "sessionId": "s",
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "Bash",
                 "input": {"command": "FOO=1 /usr/bin/make test"}}
            ]}
        });
        let (mentions, _) = extract(&gen(vec![line]), "s", "proj");
        assert_eq!(mentions[0].entity, "/usr/bin/make");
    }

    #[test]
    fn bash_heredoc_yields_snippet_artifact() {
        let line = json!({
            "type": "assistant", "uuid": "a1", "sessionId": "s",
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "Bash",
                 "input": {"command": "cat <<'EOF' > f\nhello body\nmore\nEOF"}}
            ]}
        });
        let (_, artifacts) = extract(&gen(vec![line]), "s", "proj");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].content, "hello body\nmore");
        assert_eq!(artifacts[0].kind, "snippet");
    }

    #[test]
    fn fenced_code_in_assistant_text_yields_snippet_artifact() {
        let line = json!({
            "type": "assistant", "uuid": "a1", "sessionId": "s",
            "message": {"role": "assistant", "content": [
                {"type": "text", "text": "here:\n```rust\nlet x = 1;\n```\ndone"}
            ]}
        });
        let (_, artifacts) = extract(&gen(vec![line]), "s", "proj");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].content, "let x = 1;");
        assert_eq!(artifacts[0].language.as_deref(), Some("rust"));
    }

    #[test]
    fn tool_result_body_yields_nothing() {
        let line = json!({
            "type": "user", "uuid": "u1", "sessionId": "s",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1",
                 "content": "/etc/passwd\nrm -rf /\n```sh\nsecret\n```"}
            ]}
        });
        let (mentions, artifacts) = extract(&gen(vec![line]), "s", "proj");
        assert!(mentions.is_empty());
        assert!(artifacts.is_empty());
    }

    #[test]
    fn request_bundle_is_nearest_preceding_user_prompt() {
        let prompt = json!({
            "type": "user", "uuid": "u1", "sessionId": "s",
            "message": {"role": "user", "content": "please write it"}
        });
        let write = json!({
            "type": "assistant", "uuid": "a1", "sessionId": "s",
            "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": "Write",
                 "input": {"file_path": "/f.txt", "content": "x"}}
            ]}
        });
        let (_, artifacts) = extract(&gen(vec![prompt, write]), "s", "proj");
        assert_eq!(artifacts[0].request_bundle.as_deref(), Some("u1"));
    }
}
