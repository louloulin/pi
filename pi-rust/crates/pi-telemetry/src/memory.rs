//! In-memory reference adapter used by tests and local debugging.

use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;

use crate::context::{SpanCallback, SpanRef, TelemetryContext, TelemetrySpan};
use crate::noop::NOOP_TELEMETRY_CONTEXT;
use crate::sync::lock;
use crate::types::{SpanAttributes, SpanError, SpanOptions, SpanStatus};

/// A recorded event, as returned by [`MemoryTelemetry::spans`].
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedTelemetryEvent {
    /// Event name.
    pub name: String,
    /// Attributes attached to the event.
    pub attributes: SpanAttributes,
}

/// A detached snapshot of one recorded span.
///
/// Snapshots are deep copies: mutating an earlier snapshot, or recording more
/// telemetry afterwards, cannot change it.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedTelemetrySpan {
    /// Monotonic span id, starting at 1.
    pub id: u64,
    /// Parent span id when the span was started from another span.
    pub parent_id: Option<u64>,
    /// Span name.
    pub name: String,
    /// Merged start and post-start attributes.
    pub attributes: SpanAttributes,
    /// Events in the order they were recorded.
    pub events: Vec<RecordedTelemetryEvent>,
    /// Final status; [`SpanStatus::Ok`] until settled or marked explicitly.
    pub status: SpanStatus,
    /// True once the span's callback has settled.
    pub settled: bool,
    /// Settlement order across all recorded spans, starting at 1.
    pub end_sequence: Option<u64>,
}

#[derive(Debug, Clone)]
struct MutableSpan {
    id: u64,
    parent_id: Option<u64>,
    name: String,
    attributes: SpanAttributes,
    events: Vec<RecordedTelemetryEvent>,
    status: SpanStatus,
    explicit_status: bool,
    settled: bool,
    end_sequence: Option<u64>,
}

#[derive(Debug)]
struct MemoryState {
    spans: Vec<MutableSpan>,
    next_span_id: u64,
    next_end_sequence: u64,
}

/// Reference adapter that records spans in process memory.
///
/// Mirrors `MemoryTelemetry` from `packages/telemetry/src/memory.ts`. The
/// snapshot API returns owned copies, so it is safe to read from tests while
/// other spans are still open.
#[derive(Debug, Clone)]
pub struct MemoryTelemetry {
    state: Arc<Mutex<MemoryState>>,
}

impl MemoryTelemetry {
    /// Create an empty recorder.
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(MemoryState {
                spans: Vec::new(),
                next_span_id: 1,
                next_end_sequence: 1,
            })),
        }
    }

    /// Snapshot every recorded span in start order.
    pub fn spans(&self) -> Vec<RecordedTelemetrySpan> {
        let guard = lock(&self.state);
        guard
            .spans
            .iter()
            .map(|span| RecordedTelemetrySpan {
                id: span.id,
                parent_id: span.parent_id,
                name: span.name.clone(),
                attributes: span.attributes.clone(),
                events: span.events.clone(),
                status: span.status.clone(),
                settled: span.settled,
                end_sequence: span.end_sequence,
            })
            .collect()
    }

    /// Number of recorded spans, settled or not.
    pub fn len(&self) -> usize {
        lock(&self.state).spans.len()
    }

    /// True when no span has been recorded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for MemoryTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetryContext for MemoryTelemetry {
    fn start_span<'a>(
        &'a self,
        options: SpanOptions,
        callback: SpanCallback<'a>,
    ) -> BoxFuture<'a, ()> {
        let (id, span) = start_recorded(&self.state, None, options);
        run_recorded(self.state.clone(), id, span, callback)
    }
}

/// A handle to one recorded span. Also the explicit parent for child spans.
#[derive(Debug)]
struct MemorySpan {
    state: Arc<Mutex<MemoryState>>,
    id: u64,
}

impl MemorySpan {
    fn is_settled(&self) -> bool {
        let guard = lock(&self.state);
        match guard.spans.iter().find(|span| span.id == self.id) {
            Some(span) => span.settled,
            None => true,
        }
    }
}

impl TelemetryContext for MemorySpan {
    fn start_span<'a>(
        &'a self,
        options: SpanOptions,
        callback: SpanCallback<'a>,
    ) -> BoxFuture<'a, ()> {
        // Starting a child from an already settled span is a no-op, exactly as
        // `createMemorySpan` delegates to the noop context in `memory.ts`.
        if self.is_settled() {
            return NOOP_TELEMETRY_CONTEXT.start_span(options, callback);
        }
        let (id, span) = start_recorded(&self.state, Some(self.id), options);
        run_recorded(self.state.clone(), id, span, callback)
    }
}

impl TelemetrySpan for MemorySpan {
    fn add_event(&self, name: &str, attributes: SpanAttributes) {
        let mut guard = lock(&self.state);
        if let Some(span) = guard.spans.iter_mut().find(|span| span.id == self.id) {
            if !span.settled {
                span.events.push(RecordedTelemetryEvent {
                    name: name.to_owned(),
                    attributes,
                });
            }
        }
    }

    fn set_attributes(&self, attributes: SpanAttributes) {
        let mut guard = lock(&self.state);
        if let Some(span) = guard.spans.iter_mut().find(|span| span.id == self.id) {
            if !span.settled {
                for (key, value) in attributes {
                    span.attributes.insert(key, value);
                }
            }
        }
    }

    fn set_status(&self, status: SpanStatus) {
        let mut guard = lock(&self.state);
        if let Some(span) = guard.spans.iter_mut().find(|span| span.id == self.id) {
            if !span.settled {
                span.status = status;
                span.explicit_status = true;
            }
        }
    }
}

fn start_recorded(
    state: &Arc<Mutex<MemoryState>>,
    parent_id: Option<u64>,
    options: SpanOptions,
) -> (u64, SpanRef) {
    let mut guard = lock(state);
    let id = guard.next_span_id;
    guard.next_span_id += 1;
    guard.spans.push(MutableSpan {
        id,
        parent_id,
        name: options.name,
        attributes: options.attributes,
        events: Vec::new(),
        status: SpanStatus::Ok,
        explicit_status: false,
        settled: false,
        end_sequence: None,
    });
    let span = Arc::new(MemorySpan {
        state: state.clone(),
        id,
    }) as SpanRef;
    (id, span)
}

fn run_recorded(
    state: Arc<Mutex<MemoryState>>,
    id: u64,
    span: SpanRef,
    callback: SpanCallback<'_>,
) -> BoxFuture<'_, ()> {
    let inner = callback(span);
    Box::pin(async move {
        match inner.await {
            Ok(()) => settle(&state, id, None),
            Err(error) => settle(&state, id, Some(error)),
        }
    })
}

/// Settle a span once, recording the automatic error status when the callback
/// failed without setting an explicit status.
fn settle(state: &Arc<Mutex<MemoryState>>, id: u64, error: Option<SpanError>) {
    let mut guard = lock(state);
    let Some(index) = guard.spans.iter().position(|span| span.id == id) else {
        return;
    };
    if guard.spans[index].settled {
        return;
    }
    if let Some(error) = error {
        if !guard.spans[index].explicit_status {
            guard.spans[index].status = SpanStatus::Error(Some(error));
        }
    }
    let end_sequence = guard.next_end_sequence;
    guard.next_end_sequence += 1;
    let span = &mut guard.spans[index];
    span.settled = true;
    span.end_sequence = Some(end_sequence);
}
