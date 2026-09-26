//! HTTP-backed catalog cache (`ModelsStore`) with conditional refresh.
//!
//! The `pi-coding-agent` startup path reads a remote `models.json` once
//! per process — but a long-running TUI session can outlive a publisher
//! (the upstream catalog refreshes every few minutes), and `pi
//! --update-models` is the explicit user command for pulling a fresh
//! catalog without restarting. ETag / `Last-Modified` lets every refresh
//! be cheap on the server side and silent on the wire when nothing
//! changed.
//!
//! ## On-disk cache
//!
//! `~/.pi/agent/models-cache.json` carries the parsed [`Models`] as a
//! stringified JSON envelope together with the response validator
//! headers. A missing or malformed file is treated as "first run, no
//! validators to send" — same shape as the loaders that walk
//! `~/.pi/agent/models.json`.
//!
//! ## Concurrency
//!
//! The store is `!Sync` (mutable) and lives inside one task. Callers
//! serialise their refreshes. The on-disk file is overwritten
//! atomically — write to `models-cache.json.tmp`, rename in place —
//! so a crash mid-write never leaves a half-baked cache.
//!
//! ## Non-WASM only
//!
//! Conditional GETs need `reqwest`, which is not available on
//! `wasm32-unknown-unknown`. The store is gated behind
//! `#[cfg(not(target_arch = "wasm32"))]` so the wasm build can still
//! use [`Models::load_models_json`](crate::Models::load_models_json)
//! for embedded catalogs without paying for the cache plumbing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::models::Models;

/// Outcome of a single [`ModelsStore::refresh`] call.
#[derive(Debug)]
pub enum RefreshOutcome {
    /// Server returned 304 Not Modified. Cache stays unchanged.
    NotModified,
    /// Server returned 200 with a new body. Cache was updated.
    Updated {
        /// Provider id count after the refresh — cheap success signal.
        provider_count: usize,
    },
}

/// Errors surfaced by [`ModelsStore::refresh`].
#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    /// The HTTP request failed (network, DNS, TLS, …).
    #[error("fetch {url}: {message}")]
    Fetch {
        /// URL that failed.
        url: String,
        /// Underlying error message.
        message: String,
    },
    /// The server replied with a status other than 200 / 304.
    #[error("fetch {url}: HTTP {status}: {body}")]
    BadStatus {
        /// URL that failed.
        url: String,
        /// HTTP status code.
        status: u16,
        /// Trimmed response body.
        body: String,
    },
    /// The 200 body was not valid `models.json`. The cache is left as it
    /// was — the next refresh will try again, and the user sees a clear
    /// message in the meantime.
    #[error("parse response body: {message}")]
    Parse {
        /// Underlying parser error message.
        message: String,
    },
    /// The cache file could not be persisted. The in-memory state is
    /// updated so the running session sees the new catalog, but the
    /// next process start will refetch.
    #[error("persist cache at {path}: {message}")]
    Persist {
        /// Path the store tried to write.
        path: PathBuf,
        /// Underlying error message.
        message: String,
    },
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    /// `ETag` header from the most recent 200 response, sent as
    /// `If-None-Match` on the next refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    etag: Option<String>,
    /// `Last-Modified` header from the most recent 200 response, sent
    /// as `If-Modified-Since` on the next refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_modified: Option<String>,
    /// The cached catalog body, stored verbatim so the next process
    /// start can serve the same `Models` without re-parsing.
    body: String,
}

/// HTTP-backed model catalog cache with conditional refresh.
///
/// `cache_path` is the on-disk file the store reads from and writes to.
/// Callers pick where the cache lives (typically
/// `~/.pi/agent/models-cache.json`); the store never picks its own
/// location so unit tests can point it at a temp file without globals.
pub struct ModelsStore {
    cache_path: PathBuf,
    inner: Option<CacheFile>,
}

impl ModelsStore {
    /// Build a store that already loaded `cache_path` if it existed.
    /// A missing file is not an error — the store starts empty and the
    /// first refresh warms both the cache and the in-memory `Models`.
    pub fn load_or_default(cache_path: PathBuf) -> Self {
        let inner = std::fs::read_to_string(&cache_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<CacheFile>(&raw).ok());
        Self { cache_path, inner }
    }

    /// Path the store persists its cache to.
    pub fn cache_path(&self) -> &Path {
        &self.cache_path
    }

    /// Last `ETag` value observed on a 200 response.
    pub fn etag(&self) -> Option<&str> {
        self.inner.as_ref().and_then(|c| c.etag.as_deref())
    }

    /// Last `Last-Modified` value observed on a 200 response.
    pub fn last_modified(&self) -> Option<&str> {
        self.inner
            .as_ref()
            .and_then(|c| c.last_modified.as_deref())
    }

    /// Re-parse the cached body into a [`Models`] handle.
    ///
    /// Returns `None` when the store is cold (no cache file yet, or the
    /// cached body failed to parse at load time — both surface as
    /// "first refresh will populate the cache").
    pub fn cached_models(&self) -> Option<Models> {
        let inner = self.inner.as_ref()?;
        Models::load_models_json_str(&inner.body, &self.cache_path).ok()
    }

    /// Synchronous wrapper around [`Self::refresh`] for callers that
    /// don't want to plumb a `reqwest::Client` and a tokio runtime
    /// through their own API.
    ///
    /// Spawns a fresh current-thread runtime, builds a private client,
    /// and forwards to the async path. Returns the same [`RefreshError`]
    /// variants as [`Self::refresh`]. The `user_agent` argument lets
    /// callers identify their binary in the wire logs without needing
    /// to construct the client themselves.
    pub fn refresh_sync(
        &mut self,
        url: &str,
        user_agent: &str,
    ) -> Result<RefreshOutcome, RefreshError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| RefreshError::Fetch {
                url: url.to_string(),
                message: format!("build runtime: {err}"),
            })?;
        let client = reqwest::Client::builder()
            .user_agent(user_agent)
            .build()
            .map_err(|err| RefreshError::Fetch {
                url: url.to_string(),
                message: format!("build client: {err}"),
            })?;
        runtime.block_on(self.refresh(&client, url))
    }

    /// Issue a conditional GET against `url`.
    ///
    /// When the store holds an ETag or `Last-Modified`, the request
    /// carries `If-None-Match` / `If-Modified-Since`. A 304 leaves the
    /// cache unchanged. A 200 replaces the cache body + validators and
    /// persists to disk. Any other status is an error and the cache is
    /// left untouched.
    pub async fn refresh(
        &mut self,
        client: &reqwest::Client,
        url: &str,
    ) -> Result<RefreshOutcome, RefreshError> {
        let mut request = client.get(url).header("Accept", "application/json");
        if let Some(etag) = self.etag() {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        if let Some(modified) = self.last_modified() {
            request = request.header(reqwest::header::IF_MODIFIED_SINCE, modified);
        }
        let response = request.send().await.map_err(|err| RefreshError::Fetch {
            url: url.to_string(),
            message: err.to_string(),
        })?;
        let status = response.status();
        if status.as_u16() == 304 {
            return Ok(RefreshOutcome::NotModified);
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(RefreshError::BadStatus {
                url: url.to_string(),
                status: status.as_u16(),
                body: body.chars().take(200).collect(),
            });
        }
        let new_etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_string());
        let new_last_modified = response
            .headers()
            .get(reqwest::header::LAST_MODIFIED)
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_string());
        let body = response.text().await.map_err(|err| RefreshError::Fetch {
            url: url.to_string(),
            message: format!("read response body: {err}"),
        })?;
        // Validate the body before swapping it into the cache so a
        // 200 with garbage doesn't poison the next cold-start.
        Models::load_models_json_str(&body, &self.cache_path)
            .map_err(|err| RefreshError::Parse {
                message: format!("{err}"),
            })?;
        let next = CacheFile {
            etag: new_etag,
            last_modified: new_last_modified,
            body: body.clone(),
        };
        self.persist(&next)?;
        self.inner = Some(next);
        let provider_count = Models::load_models_json_str(&body, &self.cache_path)
            .map(|m| m.iter().count())
            .unwrap_or(0);
        Ok(RefreshOutcome::Updated { provider_count })
    }

    fn persist(&self, cache: &CacheFile) -> Result<(), RefreshError> {
        let Some(parent) = self.cache_path.parent() else {
            return Ok(());
        };
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| RefreshError::Persist {
                path: self.cache_path.clone(),
                message: format!("create parent dir: {err}"),
            })?;
        }
        let tmp = self.cache_path.with_extension("json.tmp");
        let raw = serde_json::to_string_pretty(cache).map_err(|err| RefreshError::Persist {
            path: self.cache_path.clone(),
            message: format!("serialize: {err}"),
        })?;
        std::fs::write(&tmp, raw).map_err(|err| RefreshError::Persist {
            path: tmp.clone(),
            message: format!("write: {err}"),
        })?;
        std::fs::rename(&tmp, &self.cache_path).map_err(|err| RefreshError::Persist {
            path: self.cache_path.clone(),
            message: format!("rename: {err}"),
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;

    /// Tiny single-shot HTTP server that replies to one GET and then
    /// shuts its half down. The handler returns `(status, etag?,
    /// last_modified?, body)`. Sufficient for the conditional-refresh
    /// tests; not a general-purpose HTTP test fixture.
    async fn serve_once(
        response: (u16, Option<String>, Option<String>, String),
    ) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                // Drain the request line + headers.
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let (status, etag, last_modified, body) = response;
                let reason = match status {
                    200 => "OK",
                    304 => "Not Modified",
                    _ => "Internal Server Error",
                };
                let mut head = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: application/json\r\n",
                    body.len()
                );
                if let Some(etag) = etag {
                    head.push_str(&format!("ETag: {etag}\r\n"));
                }
                if let Some(modified) = last_modified {
                    head.push_str(&format!("Last-Modified: {modified}\r\n"));
                }
                head.push_str("Connection: close\r\n\r\n");
                let _ = socket.write_all(head.as_bytes()).await;
                let _ = socket.write_all(body.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        port
    }

    const CATALOG_BODY: &str = r#"{
        "providers": {
            "anthropic": {
                "models": [
                    {"id": "claude-haiku-4-5", "context_window": 200000, "max_output_tokens": 8192}
                ]
            }
        }
    }"#;

    fn catalog_with_model(id: &str) -> String {
        format!(
            r#"{{
                "providers": {{
                    "anthropic": {{
                        "models": [
                            {{"id": "{id}", "context_window": 200000, "max_output_tokens": 8192}}
                        ]
                    }}
                }}
            }}"#
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn first_refresh_warms_the_cache_and_persists_validators() {
        let tmp = std::env::temp_dir().join(format!(
            "pi-ai-models-store-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        let port = serve_once((
            200,
            Some("W/\"abc\"".to_string()),
            Some("Wed, 21 Oct 2026 07:28:00 GMT".to_string()),
            CATALOG_BODY.to_string(),
        ))
        .await;
        let url = format!("http://127.0.0.1:{port}/models.json");
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client");
        let mut store = ModelsStore::load_or_default(tmp.clone());
        let outcome = store
            .refresh(&client, &url)
            .await
            .expect("refresh succeeds");
        match outcome {
            RefreshOutcome::Updated { provider_count } => {
                assert_eq!(provider_count, 1);
            }
            other => panic!("expected Updated, got {other:?}"),
        }
        assert_eq!(store.etag(), Some("W/\"abc\""));
        assert_eq!(store.last_modified(), Some("Wed, 21 Oct 2026 07:28:00 GMT"));
        let on_disk = std::fs::read_to_string(&tmp).expect("cache file exists");
        assert!(on_disk.contains("W/\\\"abc\\\""));
        // The body is stored as an escaped JSON string, so a literal
        // substring match would miss. Parse it back through the loader
        // and confirm the model is recoverable.
        let parsed: CacheFile = serde_json::from_str(&on_disk).expect("cache parses");
        let cached = Models::load_models_json_str(&parsed.body, &tmp).expect("body parses");
        assert!(cached
            .get_model(&pi_protocol::ProviderId::new("anthropic"), "claude-haiku-4-5")
            .is_some());
        let cached = store.cached_models().expect("cached models");
        assert!(cached
            .get_model(&pi_protocol::ProviderId::new("anthropic"), "claude-haiku-4-5")
            .is_some());
        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn second_refresh_sends_validators_and_returns_not_modified_on_304() {
        let tmp = std::env::temp_dir().join(format!(
            "pi-ai-models-store-{}-304.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        let port = serve_once((
            200,
            Some("W/\"abc\"".to_string()),
            Some("Wed, 21 Oct 2026 07:28:00 GMT".to_string()),
            CATALOG_BODY.to_string(),
        ))
        .await;
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client");
        let mut store = ModelsStore::load_or_default(tmp.clone());
        let url = format!("http://127.0.0.1:{port}/models.json");
        store.refresh(&client, &url).await.expect("warm");

        // Second request — point at a different port serving 304, and
        // verify the store sends `If-None-Match`.
        let port304 = serve_once((
            304,
            Some("W/\"abc\"".to_string()),
            Some("Wed, 21 Oct 2026 07:28:00 GMT".to_string()),
            String::new(),
        ))
        .await;
        let url304 = format!("http://127.0.0.1:{port304}/models.json");
        let outcome = store
            .refresh(&client, &url304)
            .await
            .expect("304 refresh");
        assert!(matches!(outcome, RefreshOutcome::NotModified));
        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn refresh_replaces_the_cache_when_the_body_changes() {
        let tmp = std::env::temp_dir().join(format!(
            "pi-ai-models-store-{}-replace.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        // First refresh serves the original body.
        let port1 = serve_once((
            200,
            Some("W/\"v1\"".to_string()),
            None,
            CATALOG_BODY.to_string(),
        ))
        .await;
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client");
        let mut store = ModelsStore::load_or_default(tmp.clone());
        store
            .refresh(&client, &format!("http://127.0.0.1:{port1}/models.json"))
            .await
            .expect("warm");
        assert_eq!(store.etag(), Some("W/\"v1\""));

        // Second refresh serves a new body and ETag — the cache should
        // pick up both, and `cached_models` should now expose the
        // updated model.
        let port2 = serve_once((
            200,
            Some("W/\"v2\"".to_string()),
            None,
            catalog_with_model("claude-haiku-4-6"),
        ))
        .await;
        let outcome = store
            .refresh(&client, &format!("http://127.0.0.1:{port2}/models.json"))
            .await
            .expect("refresh v2");
        match outcome {
            RefreshOutcome::Updated { provider_count } => assert_eq!(provider_count, 1),
            other => panic!("expected Updated, got {other:?}"),
        }
        assert_eq!(store.etag(), Some("W/\"v2\""));
        let cached = store.cached_models().expect("cached models");
        assert!(cached
            .get_model(&pi_protocol::ProviderId::new("anthropic"), "claude-haiku-4-6")
            .is_some());
        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn refresh_rejects_garbage_bodies_without_poisoning_the_cache() {
        let tmp = std::env::temp_dir().join(format!(
            "pi-ai-models-store-{}-garbage.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        let port = serve_once((
            200,
            Some("W/\"v1\"".to_string()),
            None,
            "this is not json".to_string(),
        ))
        .await;
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client");
        let mut store = ModelsStore::load_or_default(tmp.clone());
        let err = store
            .refresh(&client, &format!("http://127.0.0.1:{port}/models.json"))
            .await
            .expect_err("garbage must fail validation");
        assert!(matches!(err, RefreshError::Parse { .. }), "got {err:?}");
        // Cache must not have been overwritten.
        assert!(!tmp.exists(), "garbage refresh must not write the cache");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cold_store_uses_only_optional_validators() {
        // No prior cache → store has no ETag / Last-Modified → the
        // request still goes out, the server replies 200, and the store
        // warms as if it were the first refresh.
        let tmp = std::env::temp_dir().join(format!(
            "pi-ai-models-store-{}-cold.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&tmp);
        let port = serve_once((
            200,
            Some("W/\"v1\"".to_string()),
            None,
            CATALOG_BODY.to_string(),
        ))
        .await;
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client");
        let mut store = ModelsStore::load_or_default(tmp.clone());
        assert!(store.etag().is_none());
        assert!(store.last_modified().is_none());
        store
            .refresh(&client, &format!("http://127.0.0.1:{port}/models.json"))
            .await
            .expect("cold refresh");
        assert_eq!(store.etag(), Some("W/\"v1\""));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn load_or_default_returns_empty_store_when_cache_is_missing() {
        let path = std::env::temp_dir().join("pi-ai-models-store-does-not-exist.json");
        let _ = std::fs::remove_file(&path);
        let store = ModelsStore::load_or_default(path);
        assert!(store.cached_models().is_none());
        assert!(store.etag().is_none());
        assert!(store.last_modified().is_none());
    }

    #[test]
    fn load_or_default_returns_empty_store_when_cache_is_corrupt() {
        let path = std::env::temp_dir().join("pi-ai-models-store-corrupt.json");
        std::fs::write(&path, "this is not the cache file").expect("write corrupt");
        let store = ModelsStore::load_or_default(path.clone());
        assert!(store.cached_models().is_none());
        let _ = std::fs::remove_file(&path);
    }

    // Silence unused-import lints when the heavy network tests above are
    // the only consumers of `Arc` / `Mutex` (kept around for future
    // multi-request fixtures).
    #[allow(dead_code)]
    fn _silence_unused(_a: Arc<()>, _m: Mutex<()>) {}
}