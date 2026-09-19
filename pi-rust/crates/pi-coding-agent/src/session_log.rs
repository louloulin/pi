//! Session log — JSONL fallback writer.
//!
//! Stage 4 stores one [`SessionEntry`](pi_protocol::SessionEntry) per
//! line in a session file under `--session-dir`. Stage 5 replaces this
//! with the SQLite backend (`pi-session`) but the JSONL path remains
//! for users who want a portable / human-readable session trail.
//!
//! The writer is intentionally fire-and-forget: callers ignore I/O
//! errors and let the interactive loop continue, matching the TS
//! behaviour where session write failures never abort the turn.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use pi_protocol::{AssistantMessage, Message, SessionEntry, ToolCall, ToolResult};

/// JSONL session log.
#[derive(Debug)]
pub struct SessionLog {
    directory: PathBuf,
    file: Mutex<Option<BufWriter<File>>>,
    session_id: String,
}

impl SessionLog {
    /// Open or create the session log under `directory`. Creates the
    /// directory tree if needed.
    pub fn open(
        directory: impl Into<PathBuf>,
        session_id: impl Into<String>,
    ) -> std::io::Result<Self> {
        let directory = directory.into();
        let session_id = session_id.into();
        std::fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{}.jsonl", session_id));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            directory,
            file: Mutex::new(Some(BufWriter::new(file))),
            session_id,
        })
    }

    /// Borrow the directory the log writes into.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Borrow the active session id.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Append a header entry to the log.
    pub fn write_header(&self, version: &str) -> std::io::Result<()> {
        let entry = SessionEntry::Header {
            id: self.session_id.clone(),
            created_at: chrono::Utc::now(),
            version: version.to_string(),
        };
        self.append(entry)
    }

    /// Append a user message.
    pub fn append_user(&self, message: Message) -> std::io::Result<()> {
        self.append(SessionEntry::UserMessage(message))
    }

    /// Append an assistant message.
    pub fn append_assistant(&self, message: AssistantMessage) -> std::io::Result<()> {
        self.append(SessionEntry::AssistantMessage(message))
    }

    /// Append a tool call + result pair.
    pub fn append_tool_call(&self, call: ToolCall) -> std::io::Result<()> {
        self.append(SessionEntry::ToolCall(call))
    }

    /// Append a tool result.
    pub fn append_tool_result(&self, result: ToolResult) -> std::io::Result<()> {
        self.append(SessionEntry::ToolResult(result))
    }

    /// Append a free-form extension entry (`pi.appendEntry`), plus the
    /// session-metadata entries the CLI synthesises from
    /// `pi.setSessionName` / `pi.sendMessage`.
    pub fn append_extension(
        &self,
        extension: impl Into<String>,
        kind: impl Into<String>,
        payload: serde_json::Value,
    ) -> std::io::Result<()> {
        self.append(SessionEntry::Extension {
            extension: extension.into(),
            kind: kind.into(),
            payload,
        })
    }

    fn append(&self, entry: SessionEntry) -> std::io::Result<()> {
        let mut guard = self.file.lock();
        if let Some(writer) = guard.as_mut() {
            let line = serde_json::to_string(&entry).map_err(std::io::Error::other)?;
            writer.write_all(line.as_bytes())?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
        Ok(())
    }

    /// Flush + close the underlying file. Idempotent.
    pub fn close(&self) -> std::io::Result<()> {
        let mut guard = self.file.lock();
        if let Some(writer) = guard.as_mut() {
            writer.flush()?;
        }
        *guard = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_protocol::{Content, Role, StopReason, TextContent, Usage};
    use std::env;

    fn user_message(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![Content::Text(TextContent { text: text.into() })],
            model: None,
        }
    }

    fn assistant_message(text: &str) -> AssistantMessage {
        AssistantMessage {
            model: "faux".into(),
            content: vec![Content::text(text)],
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
        }
    }

    #[test]
    fn appends_extension_entries() {
        let dir = env::temp_dir().join(format!("pi-session-ext-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = SessionLog::open(&dir, "ext-session").expect("open");
        log.append_extension("echo", "greeting", serde_json::json!({"n": 1}))
            .expect("extension entry");
        log.close().expect("close");

        let contents = std::fs::read_to_string(dir.join("ext-session.jsonl")).expect("read");
        assert!(contents.contains("\"type\":\"extension\""), "{contents}");
        assert!(contents.contains("\"extension\":\"echo\""), "{contents}");
        assert!(contents.contains("\"kind\":\"greeting\""), "{contents}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn writes_jsonl_with_header_and_entries() {
        let dir = env::temp_dir().join(format!("pi-session-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = SessionLog::open(&dir, "test-session").expect("open");
        log.write_header("0.1.0").expect("header");
        log.append_user(user_message("hello")).expect("user");
        log.append_assistant(assistant_message("hi"))
            .expect("assistant");
        log.close().expect("close");

        let path = dir.join("test-session.jsonl");
        let contents = std::fs::read_to_string(&path).expect("read");
        assert!(contents.starts_with('{'));
        let lines: Vec<&str> = contents.trim().split('\n').collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("\"type\":\"header\""));
        assert!(lines[0].contains("test-session"));
        assert_eq!(log.session_id(), "test-session");
        assert!(lines[1].contains("\"type\":\"user_message\""));
        assert!(lines[2].contains("\"type\":\"assistant_message\""));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
