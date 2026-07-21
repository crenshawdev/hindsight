//! Three-tier grain assignment (D-07).
//!
//! `indexed` keeps full text (user prompts, assistant text, tool_use
//! invocations, artifacts). `skeleton` keeps only structural signal - the body
//! is blanked, only `is_error` + the answering `tool_name` survive - for
//! assistant thinking and for tool_result bodies of local-content tools
//! (Read/Bash/Grep/Glob and any other non-web tool); WebFetch/WebSearch results
//! stay `indexed` because their bodies are external content worth searching.
//! `archive-only` machine-noise lines produce no Event at all.

use serde_json::Value;

use super::model::{Event, Grain};

/// Grain for an event, from its kind and (for tool_result) its resolved
/// answering-tool name. Pure.
pub fn assign_grain(event: &Event) -> Grain {
    match event.kind.as_str() {
        "text" | "tool_use" => Grain::Indexed,
        "thinking" => Grain::Skeleton,
        "tool_result" => match event.tool_name.as_deref() {
            // Web tools return external content; keep it indexed.
            Some("WebFetch") | Some("WebSearch") => Grain::Indexed,
            // Local-content tools (Read/Bash/Grep/Glob/...) skeleton.
            _ => Grain::Skeleton,
        },
        // No other kind is produced by expansion; be conservative.
        _ => Grain::Skeleton,
    }
}

/// Set the event's grain and, for skeleton events, blank the body text so no
/// tool-result body or thinking text reaches the indexed output. `is_error` and
/// `tool_name` are kept as the retained structural signal.
pub fn apply_grain(event: &mut Event) {
    let grain = assign_grain(event);
    if grain == Grain::Skeleton {
        event.text = None;
    }
    event.grain = grain;
}

/// True for machine-noise lines that get no Event record: `isMeta` lines, hook
/// chatter, and known noise line types (`system`, `attachment`, `mode`,
/// `file-history-snapshot`, `last-prompt`, `queue-operation`, `ai-title`). The
/// `{user, assistant}` whitelist already drops most; this additionally drops an
/// `isMeta:true` user/assistant line.
pub fn is_archive_only_line(line: &Value) -> bool {
    if line.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    if line.get("hookInfos").is_some() || line.get("hookErrors").is_some() {
        return true;
    }
    matches!(
        line.get("type").and_then(Value::as_str),
        Some("system")
            | Some("attachment")
            | Some("mode")
            | Some("file-history-snapshot")
            | Some("last-prompt")
            | Some("queue-operation")
            | Some("ai-title")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(kind: &str, tool_name: Option<&str>) -> Event {
        Event {
            uuid: "u".into(),
            parent_uuid: None,
            session_id: "s".into(),
            role: "assistant".into(),
            kind: kind.into(),
            timestamp: None,
            text: Some("BODY".into()),
            tool_name: tool_name.map(str::to_string),
            is_error: None,
            attribution: None,
            is_sidechain: false,
            agent_id: None,
            agent_type: None,
            grain: Grain::Indexed,
        }
    }

    #[test]
    fn user_text_and_tool_use_are_indexed() {
        assert_eq!(assign_grain(&event("text", None)), Grain::Indexed);
        assert_eq!(assign_grain(&event("tool_use", Some("Read"))), Grain::Indexed);
    }

    #[test]
    fn read_tool_result_is_skeleton_with_blanked_body() {
        let mut e = event("tool_result", Some("Read"));
        e.is_error = Some(false);
        apply_grain(&mut e);
        assert_eq!(e.grain, Grain::Skeleton);
        assert!(e.text.is_none(), "skeleton body blanked");
        assert_eq!(e.is_error, Some(false), "is_error preserved");
        assert_eq!(e.tool_name.as_deref(), Some("Read"), "tool_name preserved");
    }

    #[test]
    fn webfetch_tool_result_is_indexed_with_body_kept() {
        let mut e = event("tool_result", Some("WebFetch"));
        apply_grain(&mut e);
        assert_eq!(e.grain, Grain::Indexed);
        assert_eq!(e.text.as_deref(), Some("BODY"), "web body kept");
    }

    #[test]
    fn thinking_is_skeleton_with_blanked_text() {
        let mut e = event("thinking", None);
        apply_grain(&mut e);
        assert_eq!(e.grain, Grain::Skeleton);
        assert!(e.text.is_none());
    }

    #[test]
    fn every_grain_is_one_of_three_literals() {
        for e in [
            event("text", None),
            event("thinking", None),
            event("tool_use", Some("Bash")),
            event("tool_result", Some("Read")),
            event("tool_result", Some("WebSearch")),
        ] {
            assert!(matches!(
                assign_grain(&e),
                Grain::Indexed | Grain::Skeleton | Grain::ArchiveOnly
            ));
        }
    }

    #[test]
    fn machine_noise_lines_are_archive_only() {
        assert!(is_archive_only_line(&json!({"type": "system"})));
        assert!(is_archive_only_line(&json!({"type": "user", "isMeta": true})));
        assert!(is_archive_only_line(&json!({"type": "assistant", "hookInfos": []})));
        assert!(!is_archive_only_line(
            &json!({"type": "user", "message": {"role": "user", "content": "hi"}})
        ));
    }
}
