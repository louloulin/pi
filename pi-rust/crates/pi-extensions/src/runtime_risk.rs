//! Runtime risk telemetry + auto-fallback.
//!
//! Each extension gets a [`RuntimeRiskState`] that counts:
//!
//! - **hostcall_rate**: hostcalls per second (rolling window).
//! - **marshalling_fallback_count**: how often a value couldn't be
//!   serialised by the fast path and had to fall back to the slow
//!   `serde_json::Value` bridge.
//! - **runtime_error_count**: how often an extension call returned an
//!   exception.
//!
//! When any counter exceeds its threshold in
//! [`RuntimeRiskConfig`], the state flips to
//! [`RolloutPhase::Fallback`] and emits an
//! [`ExtensionRepairEvent::RiskThresholdBreached`] so the host can
//! pause the extension, downgrade its capability set, or surface the
//! incident to the operator.
//!
//! ## Port scope
//!
//! Simplified from `pi_agent_rust/src/extensions.rs`'s
//! `RuntimeRiskConfig` + `RolloutPhase` + telemetry:
//!
//! - Rolling window is a single coarse-grained bucket (no
//!   sub-bucketed percentile tracking).
//! - No shadow-mode / bandit policy.
//! - Repair event is a typed enum the host can convert to its own
//!   telemetry; we don't depend on the full extension-manager
//!   surface.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Where the extension is in the rollout lifecycle.
///
/// The progression is monotonic in normal operation: `Disabled →
/// Canary -> Ramp -> Full`. `Fallback` is a side-state entered when
/// thresholds are breached; from `Fallback`, the operator can lift
/// back to `Ramp` (or further).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RolloutPhase {
    /// Extension is not loaded.
    Disabled,
    /// Single test instance; failures degrade quickly.
    Canary,
    /// Multiple nodes / load observed.
    Ramp,
    /// Fully deployed.
    Full,
    /// Threshold breach — host should refuse hostcalls.
    Fallback,
}

impl RolloutPhase {
    pub fn name(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Canary => "canary",
            Self::Ramp => "ramp",
            Self::Full => "full",
            Self::Fallback => "fallback",
        }
    }

    /// True when the phase permits hostcalls.
    pub fn permits_hostcalls(self) -> bool {
        !matches!(self, Self::Disabled | Self::Fallback)
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "disabled" => Some(Self::Disabled),
            "canary" => Some(Self::Canary),
            "ramp" => Some(Self::Ramp),
            "full" => Some(Self::Full),
            "fallback" => Some(Self::Fallback),
            _ => None,
        }
    }
}

impl Default for RolloutPhase {
    fn default() -> Self {
        Self::Canary
    }
}

/// Thresholds for the auto-fallback trigger. Counters that exceed
/// their cap cause the state to flip to `Fallback`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRiskConfig {
    /// When `false`, the risk layer records telemetry but never
    /// flips the phase.
    pub enabled: bool,
    /// Maximum sustained hostcall rate (calls/sec). The rolling
    /// window is `window` long; if the count in that window exceeds
    /// `hostcalls_per_second_max`, the state falls back.
    pub hostcalls_per_second_max: u32,
    /// Maximum marshalling fallbacks within `window`.
    pub marshalling_fallback_max: u32,
    /// Maximum runtime errors within `window`.
    pub runtime_error_max: u32,
    /// Window length used for the rolling counts.
    pub window: Duration,
}

impl Default for RuntimeRiskConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            hostcalls_per_second_max: 1000,
            marshalling_fallback_max: 50,
            runtime_error_max: 25,
            window: Duration::from_secs(60),
        }
    }
}

impl RuntimeRiskConfig {
    /// Permissive thresholds used when `--no-runtime-risk` is set on
    /// the CLI. Effectively turns the layer into a recording-only
    /// observer.
    pub fn permissive() -> Self {
        Self {
            enabled: false,
            hostcalls_per_second_max: u32::MAX,
            marshalling_fallback_max: u32::MAX,
            runtime_error_max: u32::MAX,
            window: Duration::from_secs(60),
        }
    }
}

/// Counters tracked per extension.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRiskCounters {
    /// Total hostcalls observed.
    pub hostcall_count: u64,
    /// Total marshalling fallbacks.
    pub marshalling_fallback_count: u64,
    /// Total runtime errors.
    pub runtime_error_count: u64,
    /// Hostcalls within the current window.
    pub hostcalls_in_window: u64,
    /// Marshalling fallbacks within the current window.
    pub marshalling_in_window: u64,
    /// Runtime errors within the current window.
    pub errors_in_window: u64,
    /// When the current rolling window started.
    #[serde(skip)]
    pub window_started_at: Option<Instant>,
}

/// Per-extension state. Cheap to clone (`Arc` inside).
#[derive(Debug, Clone)]
pub struct RuntimeRiskState {
    inner: Arc<RuntimeRiskStateInner>,
}

#[derive(Debug)]
struct RuntimeRiskStateInner {
    extension_id: String,
    config: Mutex<RuntimeRiskConfig>,
    counters: Mutex<RuntimeRiskCounters>,
    phase: Mutex<RolloutPhase>,
    /// Buffer of emitted repair events. Test-only; the production
    /// path forwards events into the host's telemetry channel.
    events: Mutex<Vec<ExtensionRepairEvent>>,
    /// Last breach reason (cleared when phase advances).
    last_breach: Mutex<Option<String>>,
}

/// Repair events emitted by the risk layer. The host listens for
/// these and decides whether to pause the extension, downgrade
/// capabilities, or surface the incident to the operator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExtensionRepairEvent {
    /// Counter threshold breached — phase flipped to `Fallback`.
    RiskThresholdBreached {
        extension_id: String,
        reason: String,
        counters: RuntimeRiskCounters,
        config: RuntimeRiskConfig,
    },
    /// Operator manually lifted the phase from `Fallback` back to
    /// `Ramp`.
    PhaseLifted { extension_id: String, new_phase: RolloutPhase },
}

impl RuntimeRiskState {
    pub fn new(extension_id: impl Into<String>, config: RuntimeRiskConfig) -> Self {
        Self {
            inner: Arc::new(RuntimeRiskStateInner {
                extension_id: extension_id.into(),
                config: Mutex::new(config),
                counters: Mutex::new(RuntimeRiskCounters::default()),
                phase: Mutex::new(RolloutPhase::Canary),
                events: Mutex::new(Vec::new()),
                last_breach: Mutex::new(None),
            }),
        }
    }

    /// Replace the active config. The current counters and phase are
    /// preserved; only the thresholds change.
    pub fn set_config(&self, config: RuntimeRiskConfig) {
        *self.inner.config.lock() = config;
    }

    pub fn config(&self) -> RuntimeRiskConfig {
        self.inner.config.lock().clone()
    }

    pub fn phase(&self) -> RolloutPhase {
        *self.inner.phase.lock()
    }

    pub fn counters(&self) -> RuntimeRiskCounters {
        self.inner.counters.lock().clone()
    }

    /// Record a hostcall. Returns the resulting phase.
    pub fn record_hostcall(&self) -> RolloutPhase {
        self.record(|c, _now| {
            c.hostcall_count = c.hostcall_count.saturating_add(1);
            c.hostcalls_in_window = c.hostcalls_in_window.saturating_add(1);
        })
    }

    /// Record a marshalling fallback. Returns the resulting phase.
    pub fn record_marshalling_fallback(&self) -> RolloutPhase {
        self.record(|c, _now| {
            c.marshalling_fallback_count = c.marshalling_fallback_count.saturating_add(1);
            c.marshalling_in_window = c.marshalling_in_window.saturating_add(1);
        })
    }

    /// Record a runtime error. Returns the resulting phase.
    pub fn record_runtime_error(&self) -> RolloutPhase {
        self.record(|c, _now| {
            c.runtime_error_count = c.runtime_error_count.saturating_add(1);
            c.errors_in_window = c.errors_in_window.saturating_add(1);
        })
    }

    /// Lift the phase. Used by the operator to recover from
    /// `Fallback` without restarting the extension.
    pub fn lift_phase(&self, new_phase: RolloutPhase) {
        let mut phase = self.inner.phase.lock();
        let previous = *phase;
        *phase = new_phase;
        if previous == RolloutPhase::Fallback && new_phase != RolloutPhase::Fallback {
            *self.inner.last_breach.lock() = None;
            // Reset window counts — the breach is over.
            let mut counters = self.inner.counters.lock();
            counters.hostcalls_in_window = 0;
            counters.marshalling_in_window = 0;
            counters.errors_in_window = 0;
            counters.window_started_at = Some(Instant::now());
            self.inner.events.lock().push(ExtensionRepairEvent::PhaseLifted {
                extension_id: self.extension_id().to_string(),
                new_phase,
            });
        }
    }

    /// Last breach reason (if any). Cleared on `lift_phase`.
    pub fn last_breach(&self) -> Option<String> {
        self.inner.last_breach.lock().clone()
    }

    /// Drain the repair event buffer. Production forwards each
    /// event into the host's telemetry channel; tests inspect the
    /// buffer directly via `take_events`.
    pub fn take_events(&self) -> Vec<ExtensionRepairEvent> {
        std::mem::take(&mut *self.inner.events.lock())
    }

    /// `true` when the current phase allows the extension to keep
    /// making hostcalls.
    pub fn permits_hostcalls(&self) -> bool {
        self.phase().permits_hostcalls()
    }

    fn record<F>(&self, mut apply: F) -> RolloutPhase
    where
        F: FnMut(&mut RuntimeRiskCounters, Instant),
    {
        let config = self.inner.config.lock().clone();
        let mut counters = self.inner.counters.lock();
        // Rotate the rolling window if it has expired.
        let now = Instant::now();
        let should_rotate = counters
            .window_started_at
            .map(|start| now.duration_since(start) >= config.window)
            .unwrap_or(true);
        if should_rotate {
            counters.hostcalls_in_window = 0;
            counters.marshalling_in_window = 0;
            counters.errors_in_window = 0;
            counters.window_started_at = Some(now);
        }
        apply(&mut counters, now);
        // Check thresholds. `enabled` is checked last so we always
        // record the count even when the layer is in passive mode.
        if !config.enabled {
            return *self.inner.phase.lock();
        }
        let window_secs = config.window.as_secs_f64().max(0.001);
        let hostcall_rate = counters.hostcalls_in_window as f64 / window_secs;
        let breach = if hostcall_rate > config.hostcalls_per_second_max as f64 {
            Some(format!(
                "hostcall rate {:.1}/s exceeded cap {}/s",
                hostcall_rate, config.hostcalls_per_second_max
            ))
        } else if counters.marshalling_in_window > config.marshalling_fallback_max as u64 {
            Some(format!(
                "marshalling fallbacks {} exceeded cap {}",
                counters.marshalling_in_window, config.marshalling_fallback_max
            ))
        } else if counters.errors_in_window > config.runtime_error_max as u64 {
            Some(format!(
                "runtime errors {} exceeded cap {}",
                counters.errors_in_window, config.runtime_error_max
            ))
        } else {
            None
        };
        if let Some(reason) = breach {
            let mut phase = self.inner.phase.lock();
            if *phase != RolloutPhase::Fallback {
                *phase = RolloutPhase::Fallback;
                *self.inner.last_breach.lock() = Some(reason.clone());
                self.inner.events.lock().push(ExtensionRepairEvent::RiskThresholdBreached {
                    extension_id: self.extension_id().to_string(),
                    reason,
                    counters: counters.clone(),
                    config,
                });
            }
        }
        *self.inner.phase.lock()
    }

    fn extension_id(&self) -> &str {
        &self.inner.extension_id
    }
}

/// Bookkeeping for many extensions. One per host.
#[derive(Debug, Clone, Default)]
pub struct RuntimeRiskRegistry {
    inner: Arc<Mutex<HashMap<String, RuntimeRiskState>>>,
}

impl RuntimeRiskRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get or create the state for `extension_id`. Always returns a
    /// state; the caller decides whether to configure it further.
    pub fn state_for(&self, extension_id: &str, config: RuntimeRiskConfig) -> RuntimeRiskState {
        let mut map = self.inner.lock();
        if let Some(state) = map.get(extension_id) {
            return state.clone();
        }
        let state = RuntimeRiskState::new(extension_id, config);
        map.insert(extension_id.to_string(), state.clone());
        state
    }
}

#[derive(Debug, Error)]
pub enum RuntimeRiskError {
    #[error("runtime risk layer disabled but record_* still called")]
    Disabled,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(cfg: RuntimeRiskConfig) -> RuntimeRiskState {
        RuntimeRiskState::new("test-ext", cfg)
    }

    fn strict_config() -> RuntimeRiskConfig {
        RuntimeRiskConfig {
            enabled: true,
            hostcalls_per_second_max: 5,
            marshalling_fallback_max: 2,
            runtime_error_max: 1,
            window: Duration::from_secs(60),
        }
    }

    #[test]
    fn phase_starts_at_canary_by_default() {
        let s = state(RuntimeRiskConfig::default());
        assert_eq!(s.phase(), RolloutPhase::Canary);
    }

    #[test]
    fn phase_canary_permits_hostcalls() {
        let s = state(RuntimeRiskConfig::default());
        assert!(s.permits_hostcalls());
    }

    #[test]
    fn phase_disabled_does_not_permit_hostcalls() {
        let s = state(RuntimeRiskConfig::default());
        s.lift_phase(RolloutPhase::Disabled);
        assert!(!s.permits_hostcalls());
    }

    #[test]
    fn hostcall_rate_breach_triggers_fallback() {
        let s = state(strict_config());
        // Fire 6 hostcalls in tight succession; the rolling window
        // is 60s, so the rate is 6/60 = 0.1/s. Wait — that doesn't
        // breach the cap of 5/s. The window counters only reset on
        // rotation. To trigger breach under the cap of 5/s within a
        // 60s window, we'd need > 300 calls. Reduce the window for
        // the test.
        let cfg = RuntimeRiskConfig {
            enabled: true,
            hostcalls_per_second_max: 5,
            marshalling_fallback_max: 2,
            runtime_error_max: 1,
            window: Duration::from_millis(100),
        };
        s.set_config(cfg);
        for _ in 0..10 {
            s.record_hostcall();
        }
        assert_eq!(s.phase(), RolloutPhase::Fallback);
        assert!(s.last_breach().unwrap().contains("hostcall rate"));
    }

    #[test]
    fn marshalling_fallback_breach_triggers_fallback() {
        let cfg = RuntimeRiskConfig {
            enabled: true,
            hostcalls_per_second_max: 1_000_000,
            marshalling_fallback_max: 2,
            runtime_error_max: 1_000_000,
            window: Duration::from_millis(100),
        };
        let s = state(cfg);
        s.record_marshalling_fallback();
        s.record_marshalling_fallback();
        s.record_marshalling_fallback();
        assert_eq!(s.phase(), RolloutPhase::Fallback);
        assert!(s.last_breach().unwrap().contains("marshalling"));
    }

    #[test]
    fn runtime_error_breach_triggers_fallback() {
        let cfg = RuntimeRiskConfig {
            enabled: true,
            hostcalls_per_second_max: 1_000_000,
            marshalling_fallback_max: 1_000_000,
            runtime_error_max: 1,
            window: Duration::from_millis(100),
        };
        let s = state(cfg);
        s.record_runtime_error();
        s.record_runtime_error();
        assert_eq!(s.phase(), RolloutPhase::Fallback);
        assert!(s.last_breach().unwrap().contains("runtime errors"));
    }

    #[test]
    fn disabled_config_does_not_flip_phase() {
        let cfg = RuntimeRiskConfig {
            enabled: false,
            ..strict_config()
        };
        let s = state(cfg);
        for _ in 0..10 {
            s.record_hostcall();
            s.record_runtime_error();
        }
        assert_ne!(s.phase(), RolloutPhase::Fallback);
    }

    #[test]
    fn lift_phase_resets_window_counters() {
        let cfg = RuntimeRiskConfig {
            enabled: true,
            hostcalls_per_second_max: 1_000_000,
            marshalling_fallback_max: 1,
            runtime_error_max: 1_000_000,
            window: Duration::from_millis(100),
        };
        let s = state(cfg);
        s.record_marshalling_fallback();
        s.record_marshalling_fallback();
        assert_eq!(s.phase(), RolloutPhase::Fallback);
        s.lift_phase(RolloutPhase::Ramp);
        assert_eq!(s.phase(), RolloutPhase::Ramp);
        assert!(s.last_breach().is_none());
        // Window reset: marshalling count back to zero.
        let c = s.counters();
        assert_eq!(c.marshalling_in_window, 0);
    }

    #[test]
    fn emits_repair_event_on_first_breach() {
        let cfg = RuntimeRiskConfig {
            enabled: true,
            hostcalls_per_second_max: 1_000_000,
            marshalling_fallback_max: 0,
            runtime_error_max: 1_000_000,
            window: Duration::from_millis(100),
        };
        let s = state(cfg);
        s.record_marshalling_fallback();
        let events = s.take_events();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            ExtensionRepairEvent::RiskThresholdBreached { ref extension_id, .. }
                if extension_id == "test-ext"
        ));
    }

    #[test]
    fn does_not_emit_second_event_when_already_fallback() {
        let cfg = RuntimeRiskConfig {
            enabled: true,
            hostcalls_per_second_max: 1_000_000,
            marshalling_fallback_max: 0,
            runtime_error_max: 1_000_000,
            window: Duration::from_millis(100),
        };
        let s = state(cfg);
        s.record_marshalling_fallback();
        s.record_marshalling_fallback();
        s.record_marshalling_fallback();
        let events = s.take_events();
        assert_eq!(events.len(), 1, "second breach should not re-emit: {:?}", events);
    }

    #[test]
    fn phase_lift_event_emitted_on_recovery() {
        let cfg = RuntimeRiskConfig {
            enabled: true,
            hostcalls_per_second_max: 1_000_000,
            marshalling_fallback_max: 0,
            runtime_error_max: 1_000_000,
            window: Duration::from_millis(100),
        };
        let s = state(cfg);
        s.record_marshalling_fallback();
        // Drain the breach event.
        let _ = s.take_events();
        s.lift_phase(RolloutPhase::Ramp);
        let events = s.take_events();
        assert!(matches!(
            events[0],
            ExtensionRepairEvent::PhaseLifted { new_phase: RolloutPhase::Ramp, .. }
        ));
    }

    #[test]
    fn registry_returns_same_state_per_id() {
        let reg = RuntimeRiskRegistry::new();
        let s1 = reg.state_for("ext-a", RuntimeRiskConfig::default());
        let s2 = reg.state_for("ext-a", RuntimeRiskConfig::default());
        assert!(std::sync::Arc::ptr_eq(&s1.inner, &s2.inner));
    }

    #[test]
    fn registry_creates_separate_instances_for_distinct_ids() {
        let reg = RuntimeRiskRegistry::new();
        let s1 = reg.state_for("ext-a", RuntimeRiskConfig::default());
        let s2 = reg.state_for("ext-b", RuntimeRiskConfig::default());
        assert!(!std::sync::Arc::ptr_eq(&s1.inner, &s2.inner));
    }

    #[test]
    fn rollout_phase_name_round_trips() {
        for phase in [
            RolloutPhase::Disabled,
            RolloutPhase::Canary,
            RolloutPhase::Ramp,
            RolloutPhase::Full,
            RolloutPhase::Fallback,
        ] {
            assert_eq!(RolloutPhase::from_name(phase.name()), Some(phase));
        }
        assert_eq!(RolloutPhase::from_name("unknown"), None);
    }

    #[test]
    fn counters_accumulate_totals() {
        let s = state(RuntimeRiskConfig::permissive());
        for _ in 0..5 {
            s.record_hostcall();
        }
        s.record_marshalling_fallback();
        s.record_runtime_error();
        let c = s.counters();
        assert_eq!(c.hostcall_count, 5);
        assert_eq!(c.marshalling_fallback_count, 1);
        assert_eq!(c.runtime_error_count, 1);
    }
}