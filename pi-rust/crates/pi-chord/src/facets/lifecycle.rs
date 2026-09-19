//! Per-facet lifecycle: resource ownership, service access gating and disposal order.
//!
//! Port of `FacetLifecycle` in `packages/chord/src/facets/host.ts`. Every error message is kept
//! verbatim so upstream tests continue to describe the behaviour.
//!
//! Three invariants drive the design:
//!
//! * service handles are unusable until the facet is active and become unusable again the moment a
//!   generation starts disposing ([`FacetLifecycle::assert_service_access`]);
//! * effects are disposed in reverse ownership order and a failure in one effect never prevents the
//!   others from being disposed (all failures are aggregated);
//! * observations materialise at activation, so an observer installed by one facet sees the
//!   instances that providers created during *their* activation.

use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;

use crate::types::{FacetError, FacetFuture};

/// A disposal effect owned by a facet: a deferred fallible future.
pub type Effect = Box<dyn FnOnce() -> FacetFuture + Send>;

/// An observation registered during setup that produces its effect at activation.
pub type ObservationStart = Box<dyn FnOnce() -> Effect + Send>;

/// A registration for an activation callback.
pub type ActivationCallback = Box<dyn FnOnce() -> FacetFuture + Send>;

/// The lifecycle phase of one facet generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecyclePhase {
    /// `setup` is running; only registration operations are legal.
    SettingUp,
    /// `setup` finished; the facet waits for the generation to activate.
    Prepared,
    /// Service access is granted and activation callbacks have run.
    Active,
    /// Disposal is running; service access is revoked.
    Disposing,
    /// Disposal finished; the facet is inert.
    Dead,
}

impl LifecyclePhase {
    /// The upstream spelling of the phase, used in error messages.
    pub fn as_str(self) -> &'static str {
        match self {
            LifecyclePhase::SettingUp => "setting_up",
            LifecyclePhase::Prepared => "prepared",
            LifecyclePhase::Active => "active",
            LifecyclePhase::Disposing => "disposing",
            LifecyclePhase::Dead => "dead",
        }
    }
}

impl std::fmt::Display for LifecyclePhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Tracks one facet's phase, effects and callbacks.
pub struct FacetLifecycle {
    id: String,
    phase: Mutex<LifecyclePhase>,
    service_access: AtomicBool,
    effects: Mutex<Vec<Effect>>,
    observations: Mutex<Vec<ObservationStart>>,
    activations: Mutex<Vec<ActivationCallback>>,
}

impl FacetLifecycle {
    /// Creates a lifecycle for `id` in the `setting_up` phase.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            phase: Mutex::new(LifecyclePhase::SettingUp),
            service_access: AtomicBool::new(false),
            effects: Mutex::new(Vec::new()),
            observations: Mutex::new(Vec::new()),
            activations: Mutex::new(Vec::new()),
        }
    }

    /// The facet id this lifecycle belongs to.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The current phase.
    pub fn phase(&self) -> LifecyclePhase {
        *self.phase.lock()
    }

    /// Fails unless the facet is still setting up.
    pub fn assert_setting_up(&self, operation: &str) -> Result<(), FacetError> {
        if self.phase() != LifecyclePhase::SettingUp {
            return Err(FacetError::new(format!(
                "Facet {} can {operation} only during setup",
                self.id
            )));
        }
        Ok(())
    }

    /// Fails unless the facet is setting up or active.
    pub fn assert_running(&self, operation: &str) -> Result<(), FacetError> {
        let phase = self.phase();
        if phase != LifecyclePhase::SettingUp && phase != LifecyclePhase::Active {
            return Err(FacetError::new(format!(
                "Facet {} cannot {operation} while {phase}",
                self.id
            )));
        }
        Ok(())
    }

    /// Fails unless the facet is active.
    pub fn assert_active(&self, operation: &str) -> Result<(), FacetError> {
        let phase = self.phase();
        if phase != LifecyclePhase::Active {
            return Err(FacetError::new(format!(
                "Facet {} can {operation} only while active",
                self.id
            )));
        }
        Ok(())
    }

    /// Fails unless service handles held by this facet may be used.
    pub fn assert_service_access(&self) -> Result<(), FacetError> {
        if !self.service_access.load(Ordering::Acquire) {
            return Err(FacetError::new(format!(
                "Facet {} service handles cannot be used while {}",
                self.id,
                self.phase()
            )));
        }
        Ok(())
    }

    /// Revokes service access without disposing; the host calls this before a cutover.
    pub fn revoke(&self) {
        self.service_access.store(false, Ordering::Release);
    }

    /// Registers a disposal effect. Only legal while setting up or active.
    pub fn own(&self, effect: Effect) -> Result<(), FacetError> {
        self.assert_running("own resources")?;
        self.effects.lock().push(effect);
        Ok(())
    }

    /// Registers an observation started at activation. Only legal while setting up.
    pub fn observe(&self, start: ObservationStart) -> Result<(), FacetError> {
        self.assert_setting_up("observe services")?;
        self.observations.lock().push(start);
        Ok(())
    }

    /// Registers an activation callback. Only legal while setting up.
    pub fn on_activate(&self, callback: ActivationCallback) -> Result<(), FacetError> {
        self.assert_setting_up("register activation callbacks")?;
        self.activations.lock().push(callback);
        Ok(())
    }

    /// Moves the facet from `setting_up` to `prepared`.
    pub fn prepared(&self) -> Result<(), FacetError> {
        self.assert_setting_up("finish setup")?;
        *self.phase.lock() = LifecyclePhase::Prepared;
        Ok(())
    }

    /// Registers an effect without the phase check, for framework-owned cleanups.
    pub(crate) fn own_unchecked(&self, effect: Effect) {
        self.effects.lock().push(effect);
    }

    /// Materialises observations, grants service access and runs activation callbacks.
    pub async fn activate(&self) -> Result<(), FacetError> {
        if self.phase() != LifecyclePhase::Prepared {
            return Err(FacetError::new(format!(
                "Facet {} is not prepared",
                self.id
            )));
        }
        *self.phase.lock() = LifecyclePhase::Active;
        self.service_access.store(true, Ordering::Release);
        let observations: Vec<ObservationStart> = std::mem::take(&mut *self.observations.lock());
        for start in observations {
            self.effects.lock().push(start());
        }
        let activations: Vec<ActivationCallback> = std::mem::take(&mut *self.activations.lock());
        for callback in activations {
            callback().await?;
        }
        Ok(())
    }

    /// Disposes owned effects in reverse order and marks the facet dead.
    ///
    /// Idempotent. Failures are aggregated into `Failed to dispose facet <id>`; a panicking effect
    /// propagates and leaves the facet in the `disposing` phase.
    pub async fn dispose(&self) -> Result<(), FacetError> {
        if self.phase() == LifecyclePhase::Dead {
            return Ok(());
        }
        *self.phase.lock() = LifecyclePhase::Disposing;
        let mut effects: Vec<Effect> = std::mem::take(&mut *self.effects.lock());
        effects.reverse();
        // Service access is revoked for the whole disposal window, not just at the end: an effect
        // must not be able to reach a service the generation is tearing down.
        self.service_access.store(false, Ordering::Release);
        let mut failures = Vec::new();
        for effect in effects {
            if let Err(error) = effect().await {
                failures.push(error);
            }
        }
        self.observations.lock().clear();
        self.activations.lock().clear();
        *self.phase.lock() = LifecyclePhase::Dead;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(FacetError::aggregate(
                format!("Failed to dispose facet {}", self.id),
                failures,
            ))
        }
    }
}

impl std::fmt::Debug for FacetLifecycle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FacetLifecycle")
            .field("id", &self.id)
            .field("phase", &self.phase())
            .finish()
    }
}
