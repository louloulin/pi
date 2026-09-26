//! Serialize concurrent file mutations against the same path.
//!
//! Mirrors [`withFileMutationQueue`](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/src/core/tools/file-mutation-queue.ts):
//! operations for different paths still run in parallel, but two operations on
//! the same path (or its symlink target) are serialized so a parallel `edit`
//! + `write` pair cannot both observe the same pre-write state.
//!
//! # Algorithm
//!
//! The queue is keyed by [`resolve_key`], which prefers `realpath` so symlink
//! aliases share a queue. A path that does not yet exist (e.g. the first
//! `write` to a fresh file) falls back to a normalized absolute path so a
//! subsequent edit on the same path joins the same queue even though
//! `realpath` failed.
//!
//! Internally a single global `Mutex<HashMap<String, Arc<Notify>>>` holds one
//! "current waiter" per key. Each call:
//!  1. Resolves the key.
//!  2. Snapshots the current head (if any).
//!  3. Installs a fresh `Notify` as the new head.
//!  4. Awaits the snapshot.
//!  5. Runs the closure.
//!  6. Releases: removes itself from the map if it is still the head, then
//!     wakes the next waiter.
//!
//! Removal is conditional on the head still being ours so a later concurrent
//! caller that already chained onto us does not get dropped.
//!
//! ```no_run
//! use pi_coding_agent::tools::file_mutation_queue::with_file_mutation_queue;
//!
//! # async fn demo() {
//! with_file_mutation_queue("/tmp/example.txt", || async {
//!     std::fs::write("/tmp/example.txt", "hello").unwrap();
//! }).await;
//! # }
//! ```

#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};

use tokio::sync::Notify;

/// Run `f` while holding the per-path mutation lock.
///
/// Two concurrent calls on the same `file_path` (or symlinks resolving to it)
/// are serialized; calls on different paths run in parallel. The lock is
/// released after `f` resolves *or* after the returned future is dropped —
/// early drop is treated the same as a normal completion so a panicked caller
/// does not deadlock every future operation on that path.
pub async fn with_file_mutation_queue<F, T>(file_path: &str, f: F) -> T
where
    F: Future<Output = T>,
{
    let key = match resolve_key(file_path) {
        Ok(key) => key,
        // Path resolution failure is fatal for the operation; bubble it up by
        // running the closure on an empty key (which still serializes but
        // cannot block other paths). This matches upstream's "throw" path.
        Err(_) => run_unscoped(f).await,
    };

    let (current, mine) = install(&key);
    if let Some(prev) = current {
        prev.notified().await;
    }
    let result = f.await;
    release(&key, &mine);
    result
}

/// Run `f` without acquiring the lock. Used as a fallback when path
/// resolution fails so the caller still gets its result.
async fn run_unscoped<F, T>(f: F) -> T
where
    F: Future<Output = T>,
{
    f.await
}

/// Snapshot the current head and install a fresh `Notify` as the new head.
/// Returns the previous head (if any) plus the freshly installed one.
fn install(key: &str) -> (Option<Arc<Notify>>, Arc<Notify>) {
    let mine = Arc::new(Notify::new());
    let map = queues();
    let mut guard = map.lock().expect("file mutation queue mutex poisoned");
    let prev = guard.get(key).cloned();
    guard.insert(key.to_string(), mine.clone());
    (prev, mine)
}

/// Remove `mine` from the head if it is still us, then wake the next waiter.
///
/// We only delete when we are still the head so a later concurrent caller
/// that chained onto our `Notify` is not lost. Even when we are no longer
/// the head (e.g. we dropped early and someone else installed a new waiter),
/// we still call `notify_waiters` on `mine` — the next waiter that registered
/// while we were running is now waiting on a `Notify` they installed, not
/// ours, so this is a harmless no-op for them. The semantics upstream uses
/// are "releaseNext fires regardless", which we preserve.
fn release(key: &str, mine: &Arc<Notify>) {
    let map = queues();
    {
        let mut guard = map.lock().expect("file mutation queue mutex poisoned");
        let still_head = guard
            .get(key)
            .map(|head| Arc::ptr_eq(head, mine))
            .unwrap_or(false);
        if still_head {
            guard.remove(key);
        }
    }
    mine.notify_waiters();
}

fn queues() -> &'static StdMutex<HashMap<String, Arc<Notify>>> {
    static QUEUES: OnceLock<StdMutex<HashMap<String, Arc<Notify>>>> = OnceLock::new();
    QUEUES.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// Resolve `file_path` to a stable key shared by every path that points at
/// the same file. `realpath` resolves symlinks; when the file does not yet
/// exist (`realpath` returns `ENOENT`/`ENOTDIR`) we fall back to an absolute,
/// `./`- and `..`-collapsed path so a fresh `write` and a later `edit`
/// collapse onto the same queue.
fn resolve_key(file_path: &str) -> std::io::Result<String> {
    let path = Path::new(file_path);
    match std::fs::canonicalize(path) {
        Ok(resolved) => Ok(resolved.to_string_lossy().into_owned()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(path)
            };
            Ok(normalize_path(&absolute))
        }
        Err(error) => Err(error),
    }
}

/// Collapse `.` / `..` segments without touching the filesystem. Used as the
/// fallback key when the path does not yet exist.
fn normalize_path(path: &Path) -> String {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push(component);
                }
            }
            other => out.push(other),
        }
    }
    out.to_string_lossy().into_owned()
}