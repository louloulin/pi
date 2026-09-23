# pi-telemetry

Vendor-neutral telemetry contracts for the Pi Rust port — the Rust counterpart
of `packages/telemetry` in the TypeScript repository.

Pi core never depends on an observability SDK. It emits through a small
callback-based contract and the host installs the adapter (OpenTelemetry,
Langfuse, a test recorder, ...). The default adapter stores nothing.

## Contract

A span is opened around a callback. The callback receives the new span and is
also the explicit parent context for child spans. There is no public `end()`:
the span settles when the callback's future settles.

```rust
use pi_telemetry::{SpanError, SpanOptions, TelemetryContextExt, NOOP_TELEMETRY_CONTEXT};
use futures::executor::block_on;

let result = block_on(NOOP_TELEMETRY_CONTEXT.start_span_with(
    SpanOptions::new("pi.ai.request").with_attribute("pi.ai.model", "gpt-4o"),
    |span| async move {
        span.add_event("pi.ai.retry", Default::default());
        Ok::<_, SpanError>("ok")
    },
));
assert_eq!(result.unwrap(), "ok");
```

- `TelemetryContext::start_span` — erased, adapter-facing API. Invokes the
  callback exactly once with the new span and resolves when the callback's
  future resolves.
- `TelemetryContextExt::start_span_with` — typed convenience layer that keeps
  the callback's `Result` and forwards `Err` to the adapter as the automatic
  span status unless the callback set an explicit one.
- `TelemetrySpan` — records attributes (merge, last write wins), ordered
  events and the final status. Recording is passive: these methods cannot fail
  and calls made after settlement are ignored, so telemetry can never change
  the outcome of the instrumented operation.
- `AttributeValue` / `SpanAttributes` — scalars and flat arrays of scalars
  only, in insertion order. Never record prompts, completions, tool arguments
  or output, file contents, provider payloads, headers or credentials unless a
  schema and data policy explicitly allow it.

## Adapters

| Adapter | Purpose |
| --- | --- |
| `NoopTelemetry` / `NOOP_TELEMETRY_CONTEXT` | Default. Zero-sized, reuses one inert span, admits every callback and retains nothing. |
| `MemoryTelemetry` | Reference adapter for tests and local debugging. Keeps spans in process memory and returns detached snapshots from `spans()`. |

`MemoryTelemetry` tracks the same facts the upstream adapter does: monotonic
span ids, parent ids, ordered `end_sequence`, merged attributes, ordered
events, and the automatic error status when a callback fails without setting an
explicit one. Starting a child from an already settled span is a no-op.

## Conformance suite

`pi_telemetry::testing::create_telemetry_adapter_conformance` returns the shared
cases adapter authors run against their own implementation. Each case is
runner-independent: it returns a future plus a `Result<(), String>`
description, so tests work under `futures`, `tokio` and wasm hosts.

Ported groups: `callback lifecycle`, `status`, `recording`, `parentage`.

Three upstream cases are intentionally not ported because they exercise
JavaScript-only throw/`Proxy` semantics (`ignores failed attribute calls
atomically`, `ignores failed status calls atomically`, `suppresses unreadable
telemetry payload failures`): Rust recording methods cannot throw and attribute
payloads cannot be unreadable, so there is nothing to observe.

```rust
use pi_telemetry::testing::{create_telemetry_adapter_conformance, TelemetryAdapterFixture};
use pi_telemetry::MemoryTelemetry;
use std::sync::Arc;

let telemetry = MemoryTelemetry::new();
let snapshots = telemetry.clone();
let factory = Arc::new(move || {
    TelemetryAdapterFixture::new(Arc::new(telemetry.clone()), Arc::new({
        let snapshots = snapshots.clone();
        move || snapshots.spans()
    }))
});
for case in create_telemetry_adapter_conformance(factory) {
    assert!(case.run().await.is_ok(), "{} / {}", case.group(), case.name());
}
```

## Schema data

`TelemetrySchemaDefinition` and friends mirror the schema types from
`index.ts`: spans, parents, start/end attributes, events, status rules. In
TypeScript the schema is a compile-time contract; in Rust it is plain serde
data so a host can load, ship and snapshot it (for example
`pi.telemetry.schema.json`) without a validation runtime.

## WASM

Runtime-agnostic: no ambient context, clock, filesystem, thread runtime or
vendor SDK. The crate compiles unchanged for `wasm32-unknown-unknown` and needs
no `wasm` feature gate. `std::sync::Mutex` is used only for the in-memory
adapter's bookkeeping and never held across a callback.

## Upstream mapping

| Rust | `packages/telemetry/src` |
| --- | --- |
| `src/context.rs`, `src/types.rs` | `index.ts` |
| `src/noop.rs` | `noop.ts` |
| `src/memory.rs` | `memory.ts` |
| `src/testing.rs` | `testing/{index,types,conformance}.ts` |
| `src/schema.rs` | `index.ts` schema types |
