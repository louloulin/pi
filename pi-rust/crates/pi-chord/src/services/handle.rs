//! Host-owned mutable target with consumer-owned guarded views, ported from
//! `packages/chord/src/services/handle.ts`.
//!
//! Upstream's `ServiceSlot` exists so a host can rebind what a consumer sees *without* handing the
//! consumer a raw reference: the consumer holds a `ServiceView` (a `Proxy`), and every access
//! resolves through the slot after running an access assertion. That is how a released observation
//! can keep a stale handle around but still fail every use with `service_stale_instance`.
//!
//! Rust has no `Proxy`, so the dynamic property lookup cannot be reproduced literally — this port
//! keeps the part that is observable: [`ServiceSlot`] is a rebindable cell whose every read goes
//! through an assertion and fails with the upstream wording when unbound. The consumer layer
//! ([`super::consumer::KeyedServiceProxy`]) builds its name-addressed methods on top of it.

use parking_lot::RwLock;

use super::errors::ServiceError;

/// A host-owned cell that a consumer reads through an access assertion.
pub struct ServiceSlot<T> {
    service_id: String,
    inner: RwLock<Option<T>>,
}

impl<T> std::fmt::Debug for ServiceSlot<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceSlot")
            .field("service_id", &self.service_id)
            .field("bound", &self.is_bound())
            .finish()
    }
}

impl<T> ServiceSlot<T> {
    /// Creates an unbound slot for `service_id`.
    pub fn new(service_id: impl Into<String>) -> Self {
        Self {
            service_id: service_id.into(),
            inner: RwLock::new(None),
        }
    }

    /// The service this slot addresses.
    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    /// Binds the current implementation.
    pub fn bind(&self, implementation: T) {
        *self.inner.write() = Some(implementation);
    }

    /// Unbinds the implementation, as upstream's `unbind` does.
    pub fn unbind(&self) {
        *self.inner.write() = None;
    }

    /// Whether an implementation is currently bound.
    pub fn is_bound(&self) -> bool {
        self.inner.read().is_some()
    }

    /// Runs `assert_access`, then projects the bound implementation.
    ///
    /// Fails with `Service <id> is disconnected` when the slot is unbound, matching upstream's
    /// `ServiceSlot#resolve`.
    pub fn resolve<R>(
        &self,
        assert_access: impl FnOnce() -> Result<(), ServiceError>,
        project: impl FnOnce(&T) -> R,
    ) -> Result<R, ServiceError> {
        assert_access()?;
        let guard = self.inner.read();
        match guard.as_ref() {
            Some(value) => Ok(project(value)),
            None => Err(ServiceError::message(format!(
                "Service {} is disconnected",
                self.service_id
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_asserts_before_it_reads() {
        let slot: ServiceSlot<u32> = ServiceSlot::new("pi.models");
        assert!(!slot.is_bound());
        let disconnected = slot
            .resolve(|| Ok(()), |value| *value)
            .unwrap_err()
            .to_string();
        assert_eq!(disconnected, "Service pi.models is disconnected");

        slot.bind(7);
        assert!(slot.is_bound());
        assert_eq!(slot.resolve(|| Ok(()), |value| *value).unwrap(), 7);

        let denied = slot
            .resolve(
                || Err(ServiceError::remote(
                    super::super::errors::RemoteServiceErrorCode::ServiceStaleInstance,
                    "observation is closed",
                )),
                |value| *value,
            )
            .unwrap_err();
        assert!(denied.is_code(super::super::errors::RemoteServiceErrorCode::ServiceStaleInstance));

        slot.unbind();
        assert!(!slot.is_bound());
    }
}
