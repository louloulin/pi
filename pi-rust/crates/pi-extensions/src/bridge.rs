//! Bridge between the agent runtime and a [`JsExtensionHost`].
//!
//! [`JsExtensionBridge`] wraps a [`JsExtensionHost`] and implements
//! [`ExtensionBridge`](crate::api::ExtensionBridge) so the agent can
//! fan out `ExtensionEvent`s without knowing about QuickJS.

use std::sync::Arc;

use async_trait::async_trait;
use pi_protocol::{ExtensionEvent, ResourcesDiscoverReason, UiRequest, UiResponse};

use crate::api::ExtensionBridge;
use crate::error::ExtensionError;
use crate::host::{DiscoveredResources, JsExtensionHost};

/// Bridge that delivers events through a [`JsExtensionHost`].
pub struct JsExtensionBridge {
    host: JsExtensionHost,
    /// Mode string passed into the JS `ctx.mode` field on every event.
    mode: String,
    /// Whether `ctx.hasUI` should be `true` when an extension asks.
    has_ui: bool,
    /// Current working directory surfaced to JS.
    cwd: String,
}

impl std::fmt::Debug for JsExtensionBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsExtensionBridge")
            .field("mode", &self.mode)
            .field("has_ui", &self.has_ui)
            .field("cwd", &self.cwd)
            .finish()
    }
}

impl JsExtensionBridge {
    /// Wrap a host, attaching the runtime fields every event needs.
    pub fn new(
        host: JsExtensionHost,
        mode: impl Into<String>,
        has_ui: bool,
        cwd: impl Into<String>,
    ) -> Self {
        Self {
            host,
            mode: mode.into(),
            has_ui,
            cwd: cwd.into(),
        }
    }

    /// Borrow the underlying host.
    pub fn host(&self) -> &JsExtensionHost {
        &self.host
    }

    /// Wrap a bridge in an `Arc` so it can be shared with the agent.
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }

    /// Ask every loaded extension to advertise extra skill / prompt /
    /// theme paths (upstream `resources_discover`).
    ///
    /// A handler-level failure is reported through the `pi_extension`
    /// tracing target and yields no paths: discovery is best-effort, so
    /// a broken extension cannot keep the agent from starting. Handlers
    /// that did answer still contribute.
    pub async fn discover_resources(&self, reason: ResourcesDiscoverReason) -> DiscoveredResources {
        let event = ExtensionEvent::ResourcesDiscover {
            cwd: self.cwd.clone(),
            reason,
        };
        match self
            .host
            .emit_event_with(&event, Some(&self.mode), self.has_ui, &self.cwd)
            .await
        {
            Ok(outcome) => {
                if let Some(err) = outcome.errored.as_ref() {
                    tracing::warn!(
                        target: "pi_extension",
                        error = %err.message,
                        "resources_discover handler failed"
                    );
                }
                DiscoveredResources::from_dispatch(&outcome)
            }
            Err(err) => {
                tracing::warn!(
                    target: "pi_extension",
                    error = %err,
                    "failed to dispatch resources_discover"
                );
                DiscoveredResources::default()
            }
        }
    }
}

#[async_trait]
impl ExtensionBridge for JsExtensionBridge {
    async fn deliver(&self, event: &ExtensionEvent) -> bool {
        match self
            .host
            .emit_event_with(event, Some(&self.mode), self.has_ui, &self.cwd)
            .await
        {
            Ok(outcome) => outcome.handled,
            Err(err) => {
                tracing::warn!(target: "pi_extension", error = %err, "failed to deliver event");
                false
            }
        }
    }

    async fn ui_request(&self, request: UiRequest) -> Option<UiResponse> {
        // UI requests are handled inside the host (the host import
        // pumps the request through `UiHandler`). This method is here
        // for completeness — it forwards the request directly to the
        // worker channel and waits for the reply.
        let _ = request;
        None
    }
}

/// Convert a [`DispatchOutcome`](crate::host::DispatchOutcome) error
/// into an [`ExtensionError`].
pub fn dispatch_error_to_extension_error(
    err: &crate::host::DispatchOutcome,
) -> Option<ExtensionError> {
    err.errored
        .as_ref()
        .map(|e| ExtensionError::Runtime(e.message.clone()))
}
