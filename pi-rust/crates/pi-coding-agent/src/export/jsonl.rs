//! JSONL session export.
//!
//! Rust port of `packages/coding-agent/src/core/session-export.ts`
//! (`exportSessionToJsonl`): a fresh `session` header followed by one
//! upstream session entry per line, with `parentId` rewritten into a linear
//! chain, and a trailing newline.

use chrono::{SecondsFormat, Utc};
use serde_json::{json, Value};

use super::{SessionData, CURRENT_SESSION_VERSION};

/// Serialise `data` to the upstream JSONL shape.
///
/// The header's `timestamp` is regenerated at export time (upstream builds a
/// new `SessionHeader` rather than reusing the stored one), while `id` and
/// `cwd` come from [`SessionData::header`].
pub fn generate_jsonl(data: &SessionData) -> String {
    let header = data.header.clone().unwrap_or(Value::Null);
    let session_id = header
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("session");
    let cwd = header.get("cwd").and_then(Value::as_str).unwrap_or("");

    let session_header = json!({
        "type": "session",
        "version": CURRENT_SESSION_VERSION,
        "id": session_id,
        "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        "cwd": cwd,
    });

    let mut lines =
        vec![serde_json::to_string(&session_header).unwrap_or_else(|_| "{}".to_string())];

    let mut parent_id: Option<String> = None;
    for (index, entry) in data.entries.iter().enumerate() {
        let mut entry = entry.clone();
        let Some(object) = entry.as_object_mut() else {
            continue;
        };
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{:08x}", index + 1));
        object.insert("id".to_string(), Value::String(id.clone()));
        object.insert(
            "parentId".to_string(),
            parent_id.clone().map(Value::String).unwrap_or(Value::Null),
        );
        lines.push(serde_json::to_string(&entry).unwrap_or_else(|_| "{}".to_string()));
        parent_id = Some(id);
    }

    format!("{}\n", lines.join("\n"))
}

/// Build a timestamped default file name, `session-<ISO>.<extension>`
/// (upstream `session-${new Date().toISOString().replace(/[:.]/g, "-")}.jsonl`).
pub fn timestamped_session_file_name(extension: &str) -> String {
    let stamp = Utc::now()
        .to_rfc3339_opts(SecondsFormat::Millis, true)
        .replace([':', '.'], "-");
    format!("session-{stamp}.{extension}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_header_and_a_linear_parent_chain() {
        let mut data = crate::export::session_file::fixture_session_data();
        // Deliberately corrupt the chain to prove the exporter rewrites it.
        data.entries[2]["parentId"] = json!("bogus");
        let jsonl = generate_jsonl(&data);

        assert!(jsonl.ends_with('\n'), "missing trailing newline");
        let lines: Vec<&str> = jsonl.trim_end_matches('\n').split('\n').collect();
        assert_eq!(lines.len(), 4);

        let header: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(header["type"], "session");
        assert_eq!(header["version"], CURRENT_SESSION_VERSION);
        assert_eq!(header["id"], "fixture-session");
        assert!(header["timestamp"].as_str().unwrap().ends_with('Z'));

        let first: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(first["parentId"], Value::Null);
        let second: Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(second["parentId"], first["id"]);
        let third: Value = serde_json::from_str(lines[3]).unwrap();
        assert_eq!(third["parentId"], second["id"]);
        assert_eq!(third["message"]["role"], "toolResult");
    }

    #[test]
    fn default_file_name_follows_the_upstream_pattern() {
        let name = timestamped_session_file_name("jsonl");
        assert!(name.starts_with("session-"), "{name}");
        assert!(name.ends_with(".jsonl"), "{name}");
        // ISO ':' and '.' separators are replaced by '-'.
        assert!(!name[8..name.len() - 6].contains(':'), "{name}");
    }
}
