//! `/resume` slash command handler.
//!
//! Lists every SQLite-backed session in the configured session
//! directory, sorted by `created_at` descending (newest first), and
//! returns the user's pick as a [`SessionRef`] the TUI can attach to.
//!
//! Stage 68 (LUM-1255) moved the session picker's view state in here as
//! well: [`SessionSort`], [`SessionFilter`] and [`delete_session`] back
//! the picker's `app.session.toggleSort` / `togglePath` /
//! `toggleNamedFilter` / `rename` / `delete` chords, replacing upstream's
//! `session-selector.ts` view state (`sortMode`, `showPath`,
//! `namedOnly`).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Context;
use pi_session::SessionReader;

/// One resumable session as the TUI surfaces it.
#[derive(Debug, Clone)]
pub struct SessionRef {
    /// Path to the SQLite file that holds the session.
    pub database: PathBuf,
    /// Session identifier (the `id` field of the header row).
    pub session_id: String,
    /// Wall-clock timestamp the session was created (milliseconds since
    /// the unix epoch).
    pub created_at: i64,
    /// Optional pi version stamped on the session.
    pub version: Option<String>,
    /// Optional cwd stamped on the session header.
    pub cwd: Option<String>,
    /// Total number of entries in the session.
    pub entry_count: i64,
    /// Display name set with `/name`, when the session has one (stored in
    /// `sessions.metadata.name`).
    pub name: Option<String>,
}

impl SessionRef {
    /// Human-readable display string used by the TUI selector.
    pub fn display(&self) -> String {
        self.display_with(false)
    }

    /// [`SessionRef::display`] with the session database path optionally
    /// appended — upstream `session-selector.ts` `showPath`
    /// (`app.session.togglePath`).
    pub fn display_with(&self, show_path: bool) -> String {
        let ts = format_timestamp(self.created_at);
        let ver = self.version.as_deref().unwrap_or("?");
        let base = match self.name.as_deref() {
            Some(name) => format!(
                "{ts} · {ver} · {} entries · {name} · {}",
                self.entry_count, self.session_id
            ),
            None => format!(
                "{ts} · {ver} · {} entries · {}",
                self.entry_count, self.session_id
            ),
        };
        if !show_path {
            return base;
        }
        match self.database.to_str() {
            Some(path) => format!("{base} · {path}"),
            None => base,
        }
    }

    /// Whether the session carries a `/name`, i.e. whether it survives
    /// [`SessionFilter::NamedOnly`].
    pub fn is_named(&self) -> bool {
        self.name.as_deref().is_some_and(|name| !name.is_empty())
    }
}

/// Order the session picker lists sessions in — upstream
/// `session-selector.ts` `sortMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionSort {
    /// Newest `created_at` first (the default, and the order
    /// [`list_resumable`] returns).
    #[default]
    NewestFirst,
    /// Oldest first.
    OldestFirst,
    /// Alphabetical, by the session's `/name` when it has one and by its
    /// id otherwise. (Upstream's `sortMode` is `threaded | recent |
    /// fuzzy`; this port has no session-tree view and no separate fuzzy
    /// ranker, so the ring is `newest | oldest | name`. Documented gap,
    /// not fake parity.)
    NameAscending,
}

impl SessionSort {
    /// The next mode, as `app.session.toggleSort` cycles through them.
    pub fn next(self) -> Self {
        match self {
            SessionSort::NewestFirst => SessionSort::OldestFirst,
            SessionSort::OldestFirst => SessionSort::NameAscending,
            SessionSort::NameAscending => SessionSort::NewestFirst,
        }
    }

    /// Short name shown in the picker title / footer.
    pub fn name(self) -> &'static str {
        match self {
            SessionSort::NewestFirst => "newest",
            SessionSort::OldestFirst => "oldest",
            SessionSort::NameAscending => "name",
        }
    }

    /// Sort `refs` in place. Ties keep the input order (stable), and the
    /// session id is always the final tiebreak so the list never jitters
    /// between rebuilds.
    pub fn apply(self, refs: &mut [SessionRef]) {
        match self {
            SessionSort::NewestFirst => refs.sort_by(|a, b| {
                b.created_at
                    .cmp(&a.created_at)
                    .then(a.session_id.cmp(&b.session_id))
            }),
            SessionSort::OldestFirst => refs.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then(a.session_id.cmp(&b.session_id))
            }),
            // "name" is the session's `/name` when it has one, and its id
            // otherwise; the id breaks ties so the list never jitters.
            SessionSort::NameAscending => refs.sort_by(|a, b| {
                let key = |session: &SessionRef| {
                    session
                        .name
                        .clone()
                        .unwrap_or_else(|| session.session_id.clone())
                };
                key(a).cmp(&key(b)).then(a.session_id.cmp(&b.session_id))
            }),
        }
    }
}

/// Which sessions the picker lists — upstream `session-selector.ts`
/// `namedOnly` (`app.session.toggleNamedFilter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionFilter {
    /// Every session in the directory.
    #[default]
    All,
    /// Only sessions with a `/name`.
    NamedOnly,
}

impl SessionFilter {
    /// Flip between the two modes.
    pub fn toggled(self) -> Self {
        match self {
            SessionFilter::All => SessionFilter::NamedOnly,
            SessionFilter::NamedOnly => SessionFilter::All,
        }
    }

    /// Short name shown in the picker title.
    pub fn name(self) -> &'static str {
        match self {
            SessionFilter::All => "all",
            SessionFilter::NamedOnly => "named",
        }
    }

    /// Whether `session` is listed under this mode.
    pub fn accepts(self, session: &SessionRef) -> bool {
        match self {
            SessionFilter::All => true,
            SessionFilter::NamedOnly => session.is_named(),
        }
    }
}

/// Delete a session: drop its rows from the SQLite file and remove the
/// file when nothing else is left in it.
///
/// Upstream's `deleteSession` unlinks the session file outright; the
/// upstream v4 schema is a *container* that may hold several sessions
/// (`pi-session/src/schema.rs` module docs), so the rows go first and the
/// file only follows when it has become empty. Returns whether the file
/// itself was removed.
///
/// `keep_file` is the database the running session is attached to, if
/// any: it is never unlinked, even after its last session row is gone,
/// because the live writer still holds it open.
pub fn delete_session(
    session: &SessionRef,
    keep_file: Option<&Path>,
) -> anyhow::Result<(usize, bool)> {
    let removed =
        pi_session::delete_session(&session.database, &session.session_id).with_context(|| {
            format!(
                "deleting session {:?} from {}",
                session.session_id,
                session.database.display()
            )
        })?;
    let is_live = keep_file.is_some_and(|path| path == session.database);
    if is_live || !database_is_empty(&session.database) {
        return Ok((removed, false));
    }
    std::fs::remove_file(&session.database).with_context(|| {
        format!(
            "removing the now-empty session file {}",
            session.database.display()
        )
    })?;
    Ok((removed, true))
}

/// Whether the session database holds no session rows at all.
///
/// A database that cannot be read counts as non-empty: never delete a
/// file whose contents could not be confirmed empty.
fn database_is_empty(path: &Path) -> bool {
    match SessionReader::open(path) {
        Ok(reader) => reader
            .list_sessions()
            .map(|sessions| sessions.is_empty())
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// Rename a session that is not the running one: attach, store the name
/// in `sessions.metadata` and drop the writer.
///
/// Mirrors the `/name` write path (`interactive.rs::persist_session_name`)
/// for a session the driver is *not* attached to.
pub fn rename_session(session: &SessionRef, name: &str) -> anyhow::Result<()> {
    let writer = pi_session::SessionWriter::open(&session.database)
        .with_context(|| format!("opening {}", session.database.display()))?;
    writer
        .resume(&session.session_id)
        .with_context(|| format!("attaching to session {:?}", session.session_id))?;
    writer.set_session_name(name)?;
    writer.checkpoint()?;
    Ok(())
}

/// Discover every SQLite session in `directory` (one entry per
/// `(database, session_id)` pair). When the directory does not exist
/// the result is an empty Vec — `/resume` reports "no saved sessions".
pub fn list_resumable(directory: &Path) -> anyhow::Result<Vec<SessionRef>> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut refs = Vec::new();
    let entries = std::fs::read_dir(directory)
        .with_context(|| format!("reading session directory {}", directory.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s != "sqlite")
            .unwrap_or(true)
        {
            continue;
        }
        let reader = match SessionReader::open(&path) {
            Ok(reader) => reader,
            Err(err) => {
                // Non-blocking: skip corrupt files but keep listing the
                // rest. The TUI surfaces the error inline if the user
                // picks a damaged file.
                eprintln!("pi: skipping {}: {err}", path.display());
                continue;
            }
        };
        for session in reader
            .list_sessions()
            .with_context(|| format!("listing sessions in {}", path.display()))?
        {
            let count = reader
                .count_entries(&session.id)
                .with_context(|| format!("counting entries for {}", session.id))?;
            refs.push(SessionRef {
                database: path.clone(),
                session_id: session.id,
                created_at: session.created_at,
                version: session.version,
                cwd: session.cwd,
                entry_count: count,
                name: pi_session::session_name_from_metadata(session.metadata.as_deref()),
            });
        }
    }
    // Newest first; tiebreak on session id for stability.
    refs.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    Ok(refs)
}

/// Format an i64 millisecond timestamp as `YYYY-MM-DD HH:MM:SSZ` for the
/// TUI's selector display.
fn format_timestamp(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(ms, 0)
        .map(|t| t.format("%Y-%m-%d %H:%M:%SZ").to_string())
        .unwrap_or_else(|| format!("@{ms}"))
}

/// Resolve a `/resume <arg>` argument into a [`SessionRef`]. `arg` may be
/// either a session id (matched against every database in `directory`)
/// or a direct path to a `.sqlite` file.
pub fn resolve(directory: &Path, arg: &str) -> anyhow::Result<SessionRef> {
    let direct = Path::new(arg);
    if direct.is_file() && direct.extension().and_then(|s| s.to_str()) == Some("sqlite") {
        let reader = SessionReader::open(direct)?;
        let session = reader
            .latest_session()?
            .or_else(|| {
                reader
                    .list_sessions()
                    .ok()
                    .and_then(|s| s.into_iter().next())
            })
            .ok_or_else(|| anyhow::anyhow!("session database {arg:?} is empty"))?;
        let entry_count = reader.count_entries(&session.id)?;
        return Ok(SessionRef {
            database: direct.to_path_buf(),
            session_id: session.id,
            created_at: session.created_at,
            version: session.version,
            cwd: session.cwd,
            entry_count,
            name: pi_session::session_name_from_metadata(session.metadata.as_deref()),
        });
    }
    let candidates = list_resumable(directory)?;
    let matched = candidates
        .into_iter()
        .find(|cand| cand.session_id == arg)
        .ok_or_else(|| anyhow::anyhow!("session {arg:?} not found in {}", directory.display()))?;
    Ok(matched)
}

#[allow(dead_code)]
fn _touch_system_time(_t: SystemTime) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(id: &str, seconds: i64, name: Option<&str>) -> SessionRef {
        SessionRef {
            database: PathBuf::from(format!("/tmp/{id}.sqlite")),
            session_id: id.to_string(),
            created_at: seconds,
            version: Some("0.1.0".into()),
            cwd: None,
            entry_count: 1,
            name: name.map(str::to_string),
        }
    }

    fn ids(refs: &[SessionRef]) -> Vec<String> {
        refs.iter().map(|r| r.session_id.clone()).collect()
    }

    #[test]
    fn the_sort_ring_walks_all_three_modes_and_wraps() {
        assert_eq!(SessionSort::default(), SessionSort::NewestFirst);
        let mut mode = SessionSort::default();
        let mut seen = vec![mode.name()];
        for _ in 1..3 {
            mode = mode.next();
            seen.push(mode.name());
        }
        assert_eq!(seen, vec!["newest", "oldest", "name"]);
        assert_eq!(mode.next(), SessionSort::NewestFirst, "wraps around");
    }

    #[test]
    fn sorting_is_stable_and_never_jitters() {
        // Same `created_at` for all three: the id is the final tiebreak
        // in both directions, so the order is deterministic and the
        // reverse of the oldest-first order is not "whatever the sorter
        // felt like".
        let mut refs = vec![
            reference("c", 100, None),
            reference("a", 100, None),
            reference("b", 100, None),
        ];
        SessionSort::NewestFirst.apply(&mut refs);
        assert_eq!(ids(&refs), vec!["a", "b", "c"]);
        SessionSort::OldestFirst.apply(&mut refs);
        assert_eq!(ids(&refs), vec!["a", "b", "c"]);
        SessionSort::NewestFirst.apply(&mut refs);
        assert_eq!(
            ids(&refs),
            vec!["a", "b", "c"],
            "re-applying changes nothing"
        );

        let mut timed = vec![
            reference("old", 100, None),
            reference("new", 300, None),
            reference("middle", 200, None),
        ];
        SessionSort::NewestFirst.apply(&mut timed);
        assert_eq!(ids(&timed), vec!["new", "middle", "old"]);
        SessionSort::OldestFirst.apply(&mut timed);
        assert_eq!(ids(&timed), vec!["old", "middle", "new"]);
    }

    #[test]
    fn name_sort_uses_the_name_when_there_is_one() {
        let mut refs = vec![
            reference("zzz", 100, None),
            reference("aaa", 200, Some("middle")),
            reference("mmm", 300, None),
        ];
        SessionSort::NameAscending.apply(&mut refs);
        assert_eq!(
            ids(&refs),
            vec!["aaa", "mmm", "zzz"],
            "`middle` sorts under m, not under its id"
        );
    }

    #[test]
    fn the_named_filter_keeps_only_sessions_with_a_name() {
        assert_eq!(SessionFilter::default(), SessionFilter::All);
        assert_eq!(SessionFilter::All.toggled(), SessionFilter::NamedOnly);
        assert_eq!(SessionFilter::NamedOnly.toggled(), SessionFilter::All);

        let named = reference("a", 100, Some("hello"));
        let empty = reference("b", 100, Some(""));
        let unnamed = reference("c", 100, None);
        assert!(named.is_named() && !empty.is_named() && !unnamed.is_named());
        assert_eq!(SessionFilter::All.name(), "all");
        assert_eq!(SessionFilter::NamedOnly.name(), "named");

        let mut refs = vec![named, empty, unnamed];
        refs.retain(|session| SessionFilter::NamedOnly.accepts(session));
        assert_eq!(ids(&refs), vec!["a"], "a blank name is not a name");
    }

    #[test]
    fn the_path_toggle_appends_the_database() {
        let session = reference("a", 100, Some("hello"));
        assert!(
            !session.display().contains(".sqlite"),
            "{}",
            session.display()
        );
        let with_path = session.display_with(true);
        assert!(with_path.contains("/tmp/a.sqlite"), "{with_path}");
        assert!(with_path.starts_with(&session.display()), "{with_path}");
    }
}
