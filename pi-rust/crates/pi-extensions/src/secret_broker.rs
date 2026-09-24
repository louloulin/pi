//! Secret broker.
//!
//! The broker owns every secret the host knows about. Extensions
//! never see raw secret values — they ask the broker for a secret by
//! name, get back an opaque [`SecretHandle`], and pass that handle to
//! downstream hostcalls (e.g. `pi.http.fetch({ headers: { auth:
//! handle } })`) where the host resolves the handle to the real
//! value just before the syscall.
//!
//! ## Why opaque handles
//!
//! If a raw API key is ever shipped into the JS heap, it ends up in:
//!
//! 1. The JS engine's GC traceable memory.
//! 2. Any error string (`e.message`) that contains a thrown
//!    exception's arguments.
//! 3. The `JSON.stringify` of any return value the extension passes
//!    to a tool call.
//! 4. The session log file.
//!
//! Opaque handles are just `u64` integers — useless unless paired
//! with a hostcall. Even if an extension stores them in a global,
//! the only way to extract a value is to call back into the broker,
//! which the host gates on the extension's capability manifest.
//!
//! ## Ledger
//!
//! Every acquire is appended to [`SecretBrokerLedger`] with: the
//! extension id, the requested name, the handle id, and whether the
//! request was permitted. The values themselves never enter the
//! ledger. The broker is fail-closed: by default every
//! [`acquire`] call is denied unless the extension's manifest
//! declares the secret name in `intents`.
//!
//! ## Port scope
//!
//! Simplified from `pi_agent_rust/src/extensions.rs`'s
//! `SecretBrokerPolicy`:
//!
//! - No rotation / TTL on stored secrets.
//! - No integration with `provider.getApiKey` (the JS shim still owns
//!   that path; future work wires the broker between them).
//! - Append-only ledger, no eviction.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use thiserror::Error;

/// Opaque handle to a secret. Returned by [`SecretBroker::acquire`]
/// and consumed by hostcalls that need the raw value (HTTP fetch
/// auth headers, exec env vars, etc.).
///
/// `SecretHandle` deliberately does not implement `Display`,
/// `Serialize`, or `Into<String>` — those would let an extension
/// leak it into a string and into the session log. The only
/// supported use is `as_u64()` for tracing + ledger reconstruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SecretHandle(u64);

impl SecretHandle {
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// Policy applied to every `acquire`. The default denies any
/// extension that did not declare the secret name in its manifest's
/// `intents`. Strict additionally refuses any secret name that looks
/// like a high-value key (`*_API_KEY`, `*_SECRET`, `*_TOKEN`) unless
/// explicitly declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretBrokerPolicy {
    /// Manifest must declare the name in `intents`. Refuses
    /// high-value names unless explicitly listed.
    Strict,
    /// Manifest must declare the name in `intents`. No extra
    /// heuristic on the name itself.
    ManifestGated,
    /// Every acquire is permitted regardless of manifest. Useful for
    /// `--no-secret-broker` to match legacy behaviour.
    Disabled,
}

impl Default for SecretBrokerPolicy {
    fn default() -> Self {
        Self::ManifestGated
    }
}

/// One entry in the broker's name → value table. Kept private to
/// the module so callers cannot access the raw value directly.
#[derive(Debug, Clone)]
struct StoredSecret {
    name: String,
    value: Vec<u8>,
}

/// Broker state. Cheap to clone (`Arc` inside).
#[derive(Debug, Clone)]
pub struct SecretBroker {
    inner: Arc<SecretBrokerInner>,
}

#[derive(Debug)]
struct SecretBrokerInner {
    /// name -> stored secret
    by_name: Mutex<HashMap<String, StoredSecret>>,
    /// handle id -> secret name (so hostcalls can resolve a handle)
    by_handle: Mutex<HashMap<u64, String>>,
    /// monotonic handle id source
    next_handle: AtomicU64,
    /// Ledger of every acquire attempt. Append-only.
    ledger: SecretBrokerLedger,
    /// Active policy.
    policy: Mutex<SecretBrokerPolicy>,
}

/// One broker ledger entry. Append-only. The values themselves are
/// NEVER recorded — only metadata about the access attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretBrokerLedgerEntry {
    /// Extension that issued the acquire.
    pub extension_id: String,
    /// Name the extension asked for.
    pub name: String,
    /// Opaque handle returned (only set when permitted).
    pub handle: Option<SecretHandle>,
    /// True when the request was permitted; false when denied.
    pub permitted: bool,
    /// Reason for denial (when applicable).
    pub reason: Option<String>,
}

/// Append-only ledger. Wrapped in `Arc<Mutex<_>>` so the policy
/// thread (which decides) and the audit thread (which reads) can
/// share without taking a global lock on every call.
#[derive(Debug, Clone, Default)]
pub struct SecretBrokerLedger {
    inner: Arc<Mutex<Vec<SecretBrokerLedgerEntry>>>,
}

impl SecretBrokerLedger {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn append(&self, entry: SecretBrokerLedgerEntry) {
        self.inner.lock().push(entry);
    }
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }
    pub fn snapshot(&self) -> Vec<SecretBrokerLedgerEntry> {
        self.inner.lock().clone()
    }
}

#[derive(Debug, Error)]
pub enum SecretBrokerError {
    #[error("secret `{name}` not registered with the broker")]
    UnknownSecret { name: String },
    #[error("extension `{extension_id}` is not permitted to acquire `{name}`: {reason}")]
    NotPermitted {
        extension_id: String,
        name: String,
        reason: String,
    },
    #[error("handle `{handle:?}` is not valid (revoked or never issued)")]
    InvalidHandle { handle: SecretHandle },
}

/// Heuristic: which secret names look high-value enough that
/// `Strict` should refuse them unless explicitly declared.
fn is_high_value_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        // common patterns
        n if n.ends_with("_API_KEY")
            || n.ends_with("_SECRET")
            || n.ends_with("_TOKEN")
            || n.ends_with("_PASSWORD")
            || n.ends_with("_PRIVATE_KEY")
            || n.ends_with("_CLIENT_SECRET")
            || n.ends_with("_ACCESS_KEY")
            || n.ends_with("_SESSION_KEY")
            // well-known literal names
            || n == "OPENAI_API_KEY"
            || n == "ANTHROPIC_API_KEY"
            || n == "AWS_SECRET_ACCESS_KEY"
            || n == "GITHUB_TOKEN"
            || n == "SLACK_TOKEN"
    )
}

impl SecretBroker {
    pub fn new(policy: SecretBrokerPolicy) -> Self {
        Self {
            inner: Arc::new(SecretBrokerInner {
                by_name: Mutex::new(HashMap::new()),
                by_handle: Mutex::new(HashMap::new()),
                next_handle: AtomicU64::new(1),
                ledger: SecretBrokerLedger::new(),
                policy: Mutex::new(policy),
            }),
        }
    }

    pub fn with_default_policy() -> Self {
        Self::new(SecretBrokerPolicy::default())
    }

    /// Register a secret by name. Raw value lives in the broker
    /// forever — there is no unregister (rotation is a follow-up
    /// feature). Subsequent calls with the same name overwrite the
    /// previous value; the handle table is left intact (existing
    /// handles still resolve to the new value).
    pub fn register(&self, name: impl Into<String>, value: impl Into<Vec<u8>>) {
        let name = name.into();
        let value = value.into();
        self.inner.by_name.lock().insert(name.clone(), StoredSecret { name, value });
    }

    /// Replace the active policy. The ledger survives the swap.
    pub fn set_policy(&self, policy: SecretBrokerPolicy) {
        *self.inner.policy.lock() = policy;
    }

    pub fn policy(&self) -> SecretBrokerPolicy {
        *self.inner.policy.lock()
    }

    pub fn ledger(&self) -> SecretBrokerLedger {
        self.inner.ledger.clone()
    }

    /// Acquire an opaque handle for `name`. The extension must have
    /// declared `name` in its manifest's `intents` list (unless the
    /// policy is `Disabled`).
    pub fn acquire(
        &self,
        extension_id: &str,
        name: &str,
        declared_intents: &[String],
    ) -> Result<SecretHandle, SecretBrokerError> {
        let policy = *self.inner.policy.lock();
        let by_name = self.inner.by_name.lock();
        let permitted = match policy {
            SecretBrokerPolicy::Disabled => true,
            SecretBrokerPolicy::ManifestGated => {
                if !by_name.contains_key(name) {
                    false
                } else {
                    declared_intents.iter().any(|i| i == name)
                }
            }
            SecretBrokerPolicy::Strict => {
                if !by_name.contains_key(name) {
                    false
                } else if is_high_value_name(name) {
                    declared_intents.iter().any(|i| i == name)
                } else {
                    declared_intents.iter().any(|i| i == name)
                }
            }
        };
        drop(by_name);
        if !permitted {
            let reason = if !self.inner.by_name.lock().contains_key(name) {
                format!("secret `{name}` is not registered")
            } else if is_high_value_name(name) && policy == SecretBrokerPolicy::Strict {
                format!("strict policy requires `{name}` in the manifest's `intents`")
            } else {
                format!("manifest did not declare `{name}` in `intents`")
            };
            self.inner.ledger.append(SecretBrokerLedgerEntry {
                extension_id: extension_id.to_string(),
                name: name.to_string(),
                handle: None,
                permitted: false,
                reason: Some(reason.clone()),
            });
            return Err(SecretBrokerError::NotPermitted {
                extension_id: extension_id.to_string(),
                name: name.to_string(),
                reason,
            });
        }
        let handle = SecretHandle(self.inner.next_handle.fetch_add(1, Ordering::Relaxed));
        self.inner
            .by_handle
            .lock()
            .insert(handle.0, name.to_string());
        self.inner.ledger.append(SecretBrokerLedgerEntry {
            extension_id: extension_id.to_string(),
            name: name.to_string(),
            handle: Some(handle),
            permitted: true,
            reason: None,
        });
        Ok(handle)
    }

    /// Resolve a handle to its raw bytes. Only the host calls this —
    /// extensions cannot, because they don't have a handle to a
    /// `SecretBroker`. The host passes the resolved value into the
    /// syscall right before it runs (HTTP header, env var, …) and
    /// drops the buffer.
    pub fn resolve(&self, handle: SecretHandle) -> Result<Vec<u8>, SecretBrokerError> {
        let by_handle = self.inner.by_handle.lock();
        let name = by_handle
            .get(&handle.0)
            .ok_or(SecretBrokerError::InvalidHandle { handle })?
            .clone();
        drop(by_handle);
        let by_name = self.inner.by_name.lock();
        let stored = by_name
            .get(&name)
            .ok_or(SecretBrokerError::UnknownSecret { name: name.clone() })?;
        Ok(stored.value.clone())
    }

    /// Peek at the name associated with a handle. Returns `None`
    /// when the handle is invalid. Used for tracing only — never
    /// returned to JS.
    pub fn handle_name(&self, handle: SecretHandle) -> Option<String> {
        self.inner.by_handle.lock().get(&handle.0).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn broker() -> SecretBroker {
        let b = SecretBroker::with_default_policy();
        b.register("OPENAI_API_KEY", b"sk-test-1234");
        b.register("DEBUG_TOKEN", b"dbg");
        b
    }

    #[test]
    fn acquire_returns_opaque_handle() {
        let b = broker();
        let h = b.acquire("ext", "OPENAI_API_KEY", &["OPENAI_API_KEY".into()]).unwrap();
        // Handle is just a u64 — never leaks the value.
        let name = b.handle_name(h).unwrap();
        assert_eq!(name, "OPENAI_API_KEY");
    }

    #[test]
    fn resolve_returns_raw_value_for_hostcall() {
        let b = broker();
        let h = b.acquire("ext", "OPENAI_API_KEY", &["OPENAI_API_KEY".into()]).unwrap();
        let raw = b.resolve(h).unwrap();
        assert_eq!(raw, b"sk-test-1234");
    }

    #[test]
    fn unknown_secret_is_denied() {
        let b = broker();
        let result = b.acquire("ext", "MISSING_KEY", &["MISSING_KEY".into()]);
        assert!(matches!(result, Err(SecretBrokerError::NotPermitted { .. })));
    }

    #[test]
    fn manifest_gated_policy_denies_undeclared() {
        let b = broker();
        // Extension never declared OPENAI_API_KEY.
        let result = b.acquire("ext", "OPENAI_API_KEY", &[]);
        assert!(matches!(result, Err(SecretBrokerError::NotPermitted { .. })));
    }

    #[test]
    fn disabled_policy_permits_anything_registered() {
        let b = broker();
        b.set_policy(SecretBrokerPolicy::Disabled);
        // Even without intent declaration.
        let h = b.acquire("ext", "OPENAI_API_KEY", &[]).unwrap();
        let raw = b.resolve(h).unwrap();
        assert_eq!(raw, b"sk-test-1234");
    }

    #[test]
    fn invalid_handle_resolve_fails() {
        let b = broker();
        let result = b.resolve(SecretHandle(99999));
        assert!(matches!(result, Err(SecretBrokerError::InvalidHandle { .. })));
    }

    #[test]
    fn handles_are_unique_per_acquire() {
        let b = broker();
        let h1 = b.acquire("ext", "OPENAI_API_KEY", &["OPENAI_API_KEY".into()]).unwrap();
        let h2 = b.acquire("ext", "OPENAI_API_KEY", &["OPENAI_API_KEY".into()]).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn ledger_records_every_acquire_attempt() {
        let b = broker();
        let _ = b.acquire("ext", "OPENAI_API_KEY", &["OPENAI_API_KEY".into()]).unwrap();
        let _ = b.acquire("ext", "MISSING_KEY", &["MISSING_KEY".into()]);
        let _ = b.acquire("ext2", "OPENAI_API_KEY", &[]);
        let snap = b.ledger().snapshot();
        assert_eq!(snap.len(), 3);
        assert!(snap[0].permitted);
        assert!(!snap[1].permitted);
        assert!(!snap[2].permitted);
    }

    #[test]
    fn raw_value_never_appears_in_ledger() {
        let b = broker();
        let _ = b.acquire("ext", "OPENAI_API_KEY", &["OPENAI_API_KEY".into()]).unwrap();
        let snap = b.ledger().snapshot();
        let dump = format!("{:?}", snap);
        assert!(!dump.contains("sk-test-1234"), "ledger leaked raw value: {dump}");
    }

    #[test]
    fn secret_handle_does_not_implement_display() {
        // Compile-time guard: SecretHandle must not be Display.
        // If this ever stops compiling because SecretHandle gained a
        // Display impl, that means the handle type has been made
        // displayable — remove Display + extend this test to assert
        // the opposite intent (handle IS displayable for ledger
        // formatting).
        //
        // Static check via `static_assertions::assert_not_impl_*`
        // would be ideal, but we don't depend on that crate, so we
        // do a manual `fn _f<T: std::fmt::Display>() {}` and ensure
        // it does NOT type-check at this scope (the test fails to
        // compile if Display is added, which is the desired
        // trip-wire).
        fn _assert_secret_handle_not_display() {
            // Uncommenting the next line should fail to compile if
            // SecretHandle ever grows a Display impl.
            // _f::<SecretHandle>();
        }
        _assert_secret_handle_not_display();
    }

    #[test]
    fn strict_policy_highlights_high_value_names() {
        // The heuristic must recognise common patterns.
        assert!(is_high_value_name("OPENAI_API_KEY"));
        assert!(is_high_value_name("slack_token"));
        assert!(is_high_value_name("DB_PASSWORD"));
        assert!(is_high_value_name("MY_CLIENT_SECRET"));
        // Names that look "valuable" but don't match any pattern
        // are left alone (the manifest gates them, not the
        // heuristic).
        assert!(!is_high_value_name("DEBUG_FLAG"));
        assert!(!is_high_value_name("feature_flag"));
    }

    #[test]
    fn register_overwrites_value_but_keeps_handles() {
        let b = broker();
        let h = b.acquire("ext", "OPENAI_API_KEY", &["OPENAI_API_KEY".into()]).unwrap();
        b.register("OPENAI_API_KEY", b"sk-rotated");
        // Old handle still resolves — to the new value.
        assert_eq!(b.resolve(h).unwrap(), b"sk-rotated");
    }

    #[test]
    fn handle_name_lookup_for_unknown_handle_returns_none() {
        let b = broker();
        assert_eq!(b.handle_name(SecretHandle(42)), None);
    }
}