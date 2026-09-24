//! Persistent disk cache for transpiled module sources.
//!
//! QuickJS only speaks plain JavaScript, so every `.ts` / `.tsx`
//! extension has to go through SWC's `strip` pass before it can be
//! handed to the host. SWC's parse + transform + emit takes ~5–20 ms
//! per extension, which adds up to a noticeable chunk of the
//! interactive-mode startup budget when a user has many extensions
//! installed.
//!
//! This cache writes the transpiled source (plus a fingerprint of the
//! inputs that produced it) under a SHA-256-named file in a
//! configurable directory. A bumped [`CACHE_VERSION`] invalidates the
//! entire cache so a binary that ships a different transpiler never
//! reuses stale output produced by an older compiler pipeline.
//!
//! ## Cache key
//!
//! The cache key is `SHA-256(VERSION ‖ source ‖ path)`. Including the
//! path ensures two extensions with the same source but different
//! identities (e.g. `foo.ts` vs `node_modules/foo/index.ts`) do not
//! collide.
//!
//! ## Eviction
//!
//! We keep at most [`MAX_CACHE_FILES`] entries; oldest first, using
//! filesystem `mtime`. This caps the on-disk footprint at roughly
//! `MAX_CACHE_FILES × typical_module_size`, which is well under a
//! megabyte for any reasonable extension set.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

/// Bump whenever the transpiler pipeline changes. A mismatch between
/// the cached version and the running binary forces a re-transpile.
pub const CACHE_VERSION: u8 = 1;

/// Soft cap on the on-disk cache. Once exceeded, eviction removes
/// the oldest entries (by mtime) until we are back under the cap.
pub const MAX_CACHE_FILES: usize = 256;

const CACHE_SUBDIR: &str = "transpiled";

/// Compute the SHA-256 cache key for `source` interpreted as the file
/// at `path`. The returned hex string is also the file name inside
/// the cache directory.
pub fn cache_key(source: &str, path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update([CACHE_VERSION]);
    hasher.update(b"\0");
    hasher.update(path.as_bytes());
    hasher.update(b"\0");
    hasher.update(source.as_bytes());
    hex_lower(&hasher.finalize())
}

/// Look up `source` in `cache_dir`. Returns the cached transpiled
/// source on hit, `None` otherwise.
pub fn lookup(cache_dir: &Path, source: &str, path: &str) -> Option<String> {
    let key = cache_key(source, path);
    let file = cache_dir.join(CACHE_SUBDIR).join(format!("{key}.js"));
    std::fs::read_to_string(&file).ok()
}

/// Persist `transpiled` under the cache key derived from `source`
/// and `path`. Errors are logged but never propagated — caching is a
/// best-effort optimisation, and a broken disk cache must not stop
/// the loader.
pub fn store(cache_dir: &Path, source: &str, path: &str, transpiled: &str) {
    let dir = cache_dir.join(CACHE_SUBDIR);
    if let Err(err) = std::fs::create_dir_all(&dir) {
        tracing::warn!(target: "pi_extension", error = %err, dir = %dir.display(), "module cache: mkdir failed; continuing without cache");
        return;
    }
    let key = cache_key(source, path);
    let file = dir.join(format!("{key}.js"));
    // Atomic write: tmp + rename, so a partial write cannot be
    // observed by a concurrent reader.
    let tmp = dir.join(format!("{key}.js.tmp"));
    if let Err(err) = std::fs::write(&tmp, transpiled) {
        tracing::warn!(target: "pi_extension", error = %err, "module cache: write failed; continuing without cache");
        return;
    }
    if let Err(err) = std::fs::rename(&tmp, &file) {
        tracing::warn!(target: "pi_extension", error = %err, "module cache: rename failed; continuing without cache");
        let _ = std::fs::remove_file(&tmp);
        return;
    }

    // Opportunistic eviction. We only sweep when a write succeeded
    // so the cost is amortised across lookups.
    evict_if_needed(&dir);
}

/// Drop the oldest entries until `dir` has at most [`MAX_CACHE_FILES`]
/// files.
fn evict_if_needed(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<(PathBuf, SystemTime)> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let path = e.path();
            // Skip our own .tmp files (transient write artifacts).
            if path.extension().and_then(|s| s.to_str()) == Some("tmp") {
                return None;
            }
            let mtime = e.metadata().and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH);
            Some((path, mtime))
        })
        .collect();
    if entries.len() <= MAX_CACHE_FILES {
        return;
    }
    // Oldest first.
    entries.sort_by_key(|(_, t)| *t);
    for (path, _) in entries.iter().take(entries.len() - MAX_CACHE_FILES) {
        let _ = std::fs::remove_file(path);
    }
}

/// Look up `source` and, on miss, compute `transpile` and store the
/// result. Always returns the transpiled source either way.
pub fn get_or_insert_with<F>(
    cache_dir: &Path,
    source: &str,
    path: &str,
    transpile: F,
) -> String
where
    F: FnOnce() -> Result<String, String>,
{
    if let Some(hit) = lookup(cache_dir, source, path) {
        return hit;
    }
    let out = match transpile() {
        Ok(s) => s,
        Err(e) => return format!("/* transpile error: {e} */\n{source}"),
    };
    store(cache_dir, source, path, &out);
    out
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pi_cache_test_{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn cache_key_changes_with_source() {
        let a = cache_key("foo", "/x.ts");
        let b = cache_key("bar", "/x.ts");
        assert_ne!(a, b, "different sources should produce different keys");
    }

    #[test]
    fn cache_key_changes_with_path() {
        let a = cache_key("foo", "/x.ts");
        let b = cache_key("foo", "/y.ts");
        assert_ne!(a, b, "different paths should produce different keys");
    }

    #[test]
    fn store_then_lookup_round_trips() {
        let dir = fresh_dir("roundtrip");
        let transpiled = "export default function (pi) {};\n";
        store(&dir, "src", "/x.ts", transpiled);
        let got = lookup(&dir, "src", "/x.ts");
        assert_eq!(got.as_deref(), Some(transpiled));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lookup_misses_after_source_change() {
        let dir = fresh_dir("miss");
        store(&dir, "src-v1", "/x.ts", "v1");
        assert!(lookup(&dir, "src-v2", "/x.ts").is_none(), "new source should miss");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_or_insert_only_transpiles_on_miss() {
        let dir = fresh_dir("insert");
        let mut call_count = 0;
        let _ = get_or_insert_with(&dir, "src", "/x.ts", || {
            call_count += 1;
            Ok("first".to_string())
        });
        let _ = get_or_insert_with(&dir, "src", "/x.ts", || {
            call_count += 1;
            panic!("transpile fn must not be called on hit");
        });
        assert_eq!(call_count, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn evict_keeps_cache_within_cap() {
        let dir = fresh_dir("evict");
        // Write more than the cap.
        for i in 0..(MAX_CACHE_FILES + 10) {
            let src = format!("src-{i}");
            store(&dir, &src, &format!("/x{i}.ts"), &format!("export default {i};"));
        }
        let count = std::fs::read_dir(dir.join(CACHE_SUBDIR))
            .map(|rd| rd.filter_map(Result::ok).count())
            .unwrap_or(0);
        assert!(count <= MAX_CACHE_FILES, "cache not evicted: {count}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
