//! Faux provider — emits scripted events for tests.
//!
//! Stage 6 extends the Stage 0 stub with a scriptable constructor and
//! shared `provider_id()` / `models()` accessors so the WASM-bindgen
//! surface can register a faux provider in the global model catalog.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;
use pi_protocol::{Api, AssistantMessageEvent, Context, Model, ProviderId, StopReason, Usage};

use crate::stream::AssistantMessageEventStream;
use crate::types::SimpleStreamOptions;
use crate::StreamFn;

/// Faux provider — returns scripted text replies for tests and WASM
/// hosts.
///
/// `scripts` is the queue of replies; each call to `stream_simple`
/// pops the front element. When the queue is empty the last script is
/// reused (matching the behaviour of the TS faux provider so tests do
/// not have to pre-load an unbounded script list).
#[derive(Debug, Clone)]
pub struct FauxProvider {
    inner: Arc<Mutex<FauxInner>>,
}

#[derive(Debug)]
struct FauxInner {
    provider: ProviderId,
    models: Vec<Model>,
    scripts: Vec<String>,
}

impl Default for FauxProvider {
    fn default() -> Self {
        Self::with_scripts(Vec::new())
    }
}

impl FauxProvider {
    /// Construct a faux provider with the given script list. When the
    /// list is empty the provider falls back to a single
    /// `"(faux) hello"` reply.
    pub fn with_scripts(scripts: Vec<String>) -> Self {
        let models = vec![Model {
            provider: ProviderId::new("faux"),
            id: "faux-model".into(),
            api: Api::Faux,
            label: Some("Faux model".into()),
            context_window: 8192,
            max_output_tokens: 1024,
        }];
        let scripts = if scripts.is_empty() {
            vec!["(faux) hello".to_string()]
        } else {
            scripts
        };
        Self {
            inner: Arc::new(Mutex::new(FauxInner {
                provider: ProviderId::new("faux"),
                models,
                scripts,
            })),
        }
    }

    /// The provider identifier — always `"faux"`.
    pub fn provider_id(&self) -> ProviderId {
        self.inner
            .lock()
            .expect("faux mutex poisoned")
            .provider
            .clone()
    }

    /// Models this provider advertises.
    pub fn models(&self) -> Vec<Model> {
        self.inner
            .lock()
            .expect("faux mutex poisoned")
            .models
            .clone()
    }

    /// Append additional scripted replies.
    pub fn extend_scripts<I: IntoIterator<Item = String>>(&self, scripts: I) {
        self.inner
            .lock()
            .expect("faux mutex poisoned")
            .scripts
            .extend(scripts);
    }

    /// Pop the next reply off the script queue. When the queue has
    /// run out, the last entry is reused so tests do not have to seed
    /// an infinite list.
    fn next_script(&self) -> String {
        let mut inner = self.inner.lock().expect("faux mutex poisoned");
        if inner.scripts.is_empty() {
            return "(faux) hello".to_string();
        }
        let next = inner.scripts.remove(0);
        // Keep at least one script around for reuse.
        if inner.scripts.is_empty() {
            inner.scripts.push(next.clone());
        }
        next
    }
}

#[async_trait]
impl StreamFn for FauxProvider {
    async fn stream_simple(
        &self,
        model: &Model,
        _ctx: &Context,
        _options: &SimpleStreamOptions,
    ) -> Result<AssistantMessageEventStream, crate::types::StreamError> {
        let model_id = model.id.clone();
        let script = self.next_script();
        let start = AssistantMessageEvent::Start { model: model_id };
        let done = AssistantMessageEvent::Done {
            content: vec![pi_protocol::Content::text(script)],
            stop_reason: StopReason::Stop,
            usage: Usage::default(),
        };
        Ok(Box::pin(stream::iter(vec![Ok(start), Ok(done)])))
    }
}
