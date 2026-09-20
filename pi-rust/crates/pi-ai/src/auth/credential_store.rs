//! In-memory credential store — port of
//! `packages/ai/src/auth/credential-store.ts`.
//!
//! Writes are serialized **per provider**: a `modify` (or `delete`) for one
//! provider runs to completion before the next one starts, while different
//! providers proceed concurrently. That is the property the OAuth refresh
//! relies on — a shared lock across providers would stall unrelated requests,
//! and no lock at all would let two requests double-refresh a rotated token.
//!
//! # Deliberate differences from the TypeScript implementation
//!
//! Upstream chains promises per provider id (`chains` map) and returns
//! `raceWithAbortSignal(queued, signal)`, so an aborted caller is released
//! immediately while the queued task runs to completion. Rust mirrors the
//! mutual exclusion and ordering with a per-provider [`tokio::sync::Mutex`];
//! an abort mid-callback drops the callback future instead of continuing it in
//! the background. The lock is released either way, so a dropped refresh
//! cannot strand later writes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;

use super::types::{
    AuthOperationOptions, Credential, CredentialInfo, CredentialModifyFn, CredentialStore,
};
use super::{check_abort, AuthError};
use crate::types::AbortSignal;

/// Default in-memory credential store (`InMemoryCredentialStore`).
///
/// Keyed by provider id, one credential per provider. Apps inject persistent
/// stores; this one keeps everything in process memory.
#[derive(Default)]
pub struct InMemoryCredentialStore {
    credentials: RwLock<HashMap<String, Credential>>,
    /// Per-provider write locks. Entries are kept for the lifetime of the
    /// store (upstream releases its chain tails); the provider-id space is
    /// small and bounded in practice.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl InMemoryCredentialStore {
    /// Construct an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    fn provider_lock(&self, provider_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .locks
            .lock()
            .expect("credential-store lock registry poisoned");
        locks
            .entry(provider_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    fn credential(&self, provider_id: &str) -> Option<Credential> {
        self.credentials
            .read()
            .expect("credential store poisoned")
            .get(provider_id)
            .cloned()
    }

    fn store(&self, provider_id: &str, credential: &Credential) {
        self.credentials
            .write()
            .expect("credential store poisoned")
            .insert(provider_id.to_string(), credential.clone());
    }
}

/// Acquire `lock`, giving up if `signal` fires first.
///
/// The WASM build has no native select, so it checks cancellation around the
/// await (the crate's existing WASM cancellation model).
#[cfg(not(target_arch = "wasm32"))]
async fn acquire<'a>(
    lock: &'a tokio::sync::Mutex<()>,
    signal: &AbortSignal,
) -> Result<tokio::sync::MutexGuard<'a, ()>, AuthError> {
    check_abort(signal)?;
    tokio::select! {
        guard = lock.lock() => Ok(guard),
        _ = signal.cancelled() => Err(AuthError::Aborted),
    }
}

#[cfg(target_arch = "wasm32")]
async fn acquire<'a>(
    lock: &'a tokio::sync::Mutex<()>,
    signal: &AbortSignal,
) -> Result<tokio::sync::MutexGuard<'a, ()>, AuthError> {
    check_abort(signal)?;
    let guard = lock.lock().await;
    check_abort(signal)?;
    Ok(guard)
}

fn signal_of(options: Option<&AuthOperationOptions>) -> AbortSignal {
    options
        .and_then(|options| options.signal.clone())
        .unwrap_or_default()
}

#[async_trait]
impl CredentialStore for InMemoryCredentialStore {
    async fn read(
        &self,
        provider_id: &str,
        options: Option<AuthOperationOptions>,
    ) -> Result<Option<Credential>, AuthError> {
        check_abort(&signal_of(options.as_ref()))?;
        Ok(self.credential(provider_id))
    }

    async fn list(
        &self,
        options: Option<AuthOperationOptions>,
    ) -> Result<Vec<CredentialInfo>, AuthError> {
        check_abort(&signal_of(options.as_ref()))?;
        let credentials = self.credentials.read().expect("credential store poisoned");
        Ok(credentials
            .iter()
            .map(|(provider_id, credential)| CredentialInfo {
                provider_id: provider_id.clone(),
                r#type: credential.auth_type(),
            })
            .collect())
    }

    async fn modify(
        &self,
        provider_id: &str,
        f: CredentialModifyFn,
        options: Option<AuthOperationOptions>,
    ) -> Result<Option<Credential>, AuthError> {
        let signal = signal_of(options.as_ref());
        let lock = self.provider_lock(provider_id);
        let _guard = acquire(&lock, &signal).await?;

        let current = self.credential(provider_id);
        let fallback = current.clone();
        let next = f(current).await?;
        check_abort(&signal)?;
        if let Some(credential) = next.as_ref() {
            self.store(provider_id, credential);
        }
        // Upstream returns `next ?? current`.
        Ok(next.or(fallback))
    }

    async fn delete(
        &self,
        provider_id: &str,
        options: Option<AuthOperationOptions>,
    ) -> Result<(), AuthError> {
        let signal = signal_of(options.as_ref());
        let lock = self.provider_lock(provider_id);
        let _guard = acquire(&lock, &signal).await?;
        self.credentials
            .write()
            .expect("credential store poisoned")
            .remove(provider_id);
        Ok(())
    }
}
