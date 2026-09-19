# `pi-chord` — application composition core

`pi-chord` is the Rust port of the **core layer** of
[`packages/chord`](../../packages/chord) in the TypeScript monorepo: a typed service graph
("facets") plus replicated state that travels as JSON deltas, both threaded through a `Context`
that carries cancellation.

This crate deliberately contains no filesystem, clock, socket or thread-pool dependency. It is
the groundwork for Stage 19 (`pi-server` / `pi-client`), which own the concrete transports: the
remote-service protocol here is transport-agnostic and is exercised in-process by the loopback
transport.

## Upstream mapping

| Rust module | Upstream file | Contents |
| --- | --- | --- |
| `json` | `src/json.ts` | `JsonValue`, kind inspection, structural validation, normalisation |
| `delta::path` | `src/delta/index.ts` (paths) | `Seg`, `Path`, `NonEmptyPath`, `assertSafePath`, path JSON encoding |
| `delta::op` | `src/delta/index.ts` (grammar) | `Op` (decoded) and `WireOp` (wire), validators, JSON codec |
| `delta::apply` | `src/delta/index.ts` (`apply`, `applyImmutable`) | in-place and persistent application of a batch |
| `delta::diff` | `src/delta/index.ts` (`diff`) | value and string diff, overlap search |
| `delta::codec` | `src/delta/index.ts` (`encodeBatch`, `decodeBatch`) | path interning (`#`), short forms, stream state |
| `delta::tracker` | `src/delta/index.ts` (`track`) | baseline/target tracking, flush, rebase, discard, sync |
| `context` | `src/context/index.ts` | `Context`, `ContextKey`, `AbortSignal`/`AbortController`, `awaitWithContext` |
| `types` | `src/types.ts` | facet/service vocabulary, `FacetError`, replicated-state catalogue types |
| `state` | `src/services/state.ts` | replicated state producer, live view, cold replica |
| `facets::lifecycle` | `src/facets/host.ts` (`FacetLifecycle`) | phases, effect ownership, observations, activation callbacks |
| `facets::registry` | `src/services/{handle,instances}.ts` (local half) | singleton slots, keyed instances, observers, service handles |
| `facets::host` | `src/facets/host.ts` (`FacetKernel`, `FacetHostImpl`) | setup, validation, assembly, activation, reload, disposal |
| `facets::loader` | `src/facets/loader.ts`, `src/api.ts` | static loaders, `combineFacetLoaders`, generation disposal |
| `services::errors` | `src/services/errors.ts` | `RemoteServiceErrorCode`, `RemoteServiceError`, `ServiceError` |
| `services::wire` | `src/services/wire.ts` | calls, control calls, snapshots, provider updates, strict JSON parsers |
| `services::state_codec` | `src/services/state-codec.ts` | per-subscription `Encoder`/`Decoder` registries |
| `services::state` | `src/services/state.ts` (`ReplicatedStateMember`) | provider source listener for replicated state |
| `services::provider` | `src/services/provider.ts` | `RemoteServiceProvider`, subscriptions, `RemoteServiceEndpoint` |
| `services::consumer` | `src/services/consumer.ts` | bindings, facades, proxies, retained handles, keyed observations |
| `services::handle` / `services::instances` / `services::loopback` | same names | member slots, instance directory, loopback transport |
| `api` | `src/api.ts` | the public entry points (`createFacetHost`, `defineFacet`, `createRemoteServiceBinding`, ...) |

## When to use

- Run several independent features as facets over one shared service graph, with deterministic
  setup/activation/disposal order (the eventual `pi-server` and `pi-client` compositions).
- Publish a piece of state to a live view or a remote replica without sending the whole value
  again (the eventual `pi-server` session/state plumbing).
- Reuse the JSON delta grammar standalone, for example in a custom wire protocol.

## Delta grammar

A batch is a list of operations. Paths are `(key | index)` sequences; `["a", 0]` is an object key
followed by an array index.

| Verb | Decoded | Wire | Meaning |
| --- | --- | --- | --- |
| `r` | `Op::Replace(value)` | same | replace the root; clears the path dictionary |
| `s` | `Op::Set(path, value)` | `["s", ref, value]` / short `["s", value]` | set a leaf |
| `d` | `Op::Delete(path)` | `["d", ref]` / short `["d"]` | delete a key / splice an array element |
| `a` | `Op::Append(path, text)` | `["a", ref, text]` / short `["a", text]` | append UTF-16 text |
| `t` | `Op::Truncate(path, count)` | `["t", ref, count]` / short `["t", count]` | drop the first `count` UTF-16 units |
| `p` | `Op::Patch { path, index, remove, items }` | always inline | splice `remove` array items at `index` |
| `#` | — | `["#", id, path]` | define a reusable path id |

```rust
use pi_chord::delta::{apply, diff, Op, Tracker};
use serde_json::json;

// Diff two values and replay the batch.
let before = json!({"title": "draft", "tags": ["a"]});
let after = json!({"title": "draft!", "tags": ["a", "b"]});
let ops = diff(&before, &after);
assert_eq!(apply(Some(before), &ops).unwrap(), after);

// Or track a value across publishes: the first flush is a base batch.
let mut tracker = Tracker::new(json!({"n": 0}));
assert!(matches!(tracker.flush().unwrap().first(), Some(Op::Replace(_))));
```

Wire encoding interns paths: a path is spelled inline on first use, defined once with `#` on
second use, and omitted entirely when it repeats the previous operation's path. The dictionary
survives across batches and is cleared by `r`; the "same as previous" short form is per batch.

## Facets

```rust
# use pi_chord::context::block_on;
use pi_chord::facets::{create_facet_host, HostPhase};
use pi_chord::types::{define_facet, define_local_service, FacetOptions, Service};

let clock: Service<u64> = define_local_service("test.clock").unwrap();

let provider = {
    let clock = clock.clone();
    define_facet("clock", move |env| {
        env.provide(&clock, 42u64)?;
        Ok(())
    })
};
let consumer = {
    let clock = clock.clone();
    define_facet("consumer", move |env| {
        let handle = env.use_service(&clock)?;          // resolved during setup, usable when active
        env.on_activate(move || {
            let handle = handle.clone();
            async move {
                assert_eq!(*handle.get()?, 42);
                Ok(())
            }
        })?;
        Ok(())
    })
};

let mut host = block_on(create_facet_host(FacetOptions::new(vec![provider, consumer])))?;
assert_eq!(host.phase(), HostPhase::Active);
assert_eq!(host.activation_order(), ["clock", "consumer"]);
block_on(host.dispose())?;
# Ok::<(), pi_chord::FacetError>(())
```

The lifecycle is deterministic:

1. `setup` runs in **declaration order**. Every facet registers its requirements and provisions.
2. The generation is validated: unique ids, no duplicate or mode-conflicting provider, every
   requirement satisfied, no dependency cycle.
3. Singleton provisions are bound and keyed provisions are attached.
4. Activation callbacks run in **dependency order** (a stable topological sort).
5. Disposal runs effects in **reverse activation order**; service access is revoked for the whole
   disposal window, so a cleanup can no longer read a service it had acquired.

`FacetHost::reload` swaps in a replacement generation: candidates are set up and activated before
the cutover, singletons are rebound at the cutover, and the outgoing generation is retired last.
A reload must preserve each facet's service requirements and provisions.

## Replicated state

`replicated_state(initial)` returns a producer; `publish` diffs against the last published value
and bumps the sequence only when the diff is non-empty. `MutableReplicatedState::replica()` is a
live in-process view; `StateReplica` is a cold replica fed by a transport and it rejects a
non-base snapshot, an update before hydration, and any sequence gap (a duplicate or reordered
batch) by clearing itself until a fresh snapshot arrives.

## Remote services

A provider publishes a service implementation (methods and replicated-state members); a binding
consumes it through a transport. The transport is a two-method trait, so a host can drive the
protocol over anything from an in-process loopback to a socket:

```rust
use std::sync::Arc;
use pi_chord::context::background_context;
use pi_chord::{
    create_loopback_service_transport, create_remote_service_binding, define_service,
    replicated_state, RemoteServiceBindingOptions, RemoteServiceProvider, ServiceImplementation,
    ServiceProviderEntry,
};
use serde_json::{json, Value};

let models: pi_chord::Service<Value> = define_service("test.models").unwrap();
let state = Arc::new(replicated_state(json!({ "revision": 0 })).unwrap());
let provider = Arc::new(RemoteServiceProvider::new([
    ServiceProviderEntry::singleton(&models),
])?);
provider.provide(&models, ServiceImplementation::new().state("state", Arc::clone(&state)))?;

let transport = create_loopback_service_transport(Arc::clone(&provider));
let binding = create_remote_service_binding(
    RemoteServiceBindingOptions::new(transport).service(&models),
)?;
let proxy = binding.use_service(&models)?;
assert_eq!(proxy.state("state")?.value()?, Some(json!({ "revision": 0 })));
# Ok::<(), pi_chord::ServiceError>(())
```

The wire protocol is factored so a server can reuse it without the consumer:

- `ServiceCall` / `parse_service_call` and the control calls (`create_service_catalogue_call`,
  `create_service_subscribe_call`, `create_service_unsubscribe_call`,
  `decode_service_control_call`) travel over the reserved `$chord.service` id.
- `ServiceSubscriptionSnapshot` / `ServiceProviderUpdate<Op>` are the decoded messages;
  `ServiceStateEncoder` / `ServiceStateDecoder` (or one-shot `encode_subscription_snapshot` /
  `decode_wire_subscription_snapshot` / `decode_wire_provider_update`) convert them to and from
  the interned `WireOp` vocabulary.
- `RemoteServiceEndpoint` turns provider subscriptions into published updates and owns their
  cleanup.

## Semantics kept exactly

- **Error wording.** Messages and their classes (`unresolvable path: ...`, `unsafe path segment:
  ...`, `op is not a tuple`, `unknown op verb: ...`, `r arity`, `p arity`, `a shape`, `t shape`,
  `# shape`, the `Facet ...` messages, the aggregate messages) match upstream, so assertions
  written against the TypeScript implementation still hold.
- **Apply semantics.** Arrays reject non-numeric segments and out-of-range indices
  (`index > length` is unsafe, `index == length` appends); `a`/`t` require a string leaf and count
  in UTF-16 units; `t` clamps like `String.prototype.slice`; `p` splices like
  `Array.prototype.splice`.
- **Wire semantics.** `previous` is per batch; ids survive across batches and are cleared by `r`;
  the short form is detected by arity; an unresolved id or a short form without `previous` is a
  path error.
- **Tracked state sequences.** The published value is the construction value at sequence `0`;
  `subscribe` publishes first (usually a no-op) and then hydrates; publish is a no-op when nothing
  changed.

## Documented deviations

| Area | Upstream | Here | Why |
| --- | --- | --- | --- |
| Transport | transport methods are `async` | `RemoteServiceTransport` / `ServiceSubscription` are synchronous | `pi-chord` has no async runtime; a host that has one wraps the calls |
| Service facade members | a `Proxy` resolves a member lazily by name | `RemoteServiceProxy::{method,state}(name)` returns a retained handle | Rust has no property-access proxy; the observable semantics (stable facade, stale-instance guard) are preserved |
| `track` | `Proxy` records which paths changed | `Tracker::target_mut()` + `flush()` recomputes a diff | Rust has no property-access proxy; the API is the same, the cost is not |
| `r` op | adopts the payload by reference | clones the payload | Rust values have no aliasing |
| Handle access | `handle` is a deref proxy that throws when inactive | `ServiceHandle::get() -> Result<Arc<T>, FacetError>` | `Deref` cannot fail |
| "implementation must be an object" | rejected at runtime | not checked | The trait bound on `provide` already enforces a typed value; the JS check exists only because JS objects are untyped |
| "setup must be synchronous" | checked after the fact | structurally impossible | `Facet::setup` is not `async` |
| String ops | UTF-16 code units, lone surrogates possible | surrogate-splitting `t` returns `DeltaError::NotCharAligned` | Rust `String` cannot hold lone surrogates; refusing beats corrupting |
| Object key order | JS insertion order | `serde_json` map order (BTreeMap by default) | `serde_json` does not preserve insertion order unless the `preserve_order` feature is on |
| JSON values | `isJsonValue` accepts any acyclic JSON, including sparse arrays | `assert_json_value` also enforces a depth bound (512) | `serde_json::Value` cannot represent sparse arrays, accessors or cycles |
| Unset root | a batch may leave the root `undefined` | `apply` returns `Err(DeltaError::Path("[]"))` | A `None` root would push the check into every caller |
| `FacetEnvironment` | an object literal capturing mutable arrays | a lifetime-free `Arc`-based value | Keeps `Fn(&mut FacetEnvironment)` closures simple (no HRTB) |

`FacetHost::reload` simplification: because a reload requires an identical facet shape, a failure
after the singleton rebind leaves the rebind in place and the host is torn down (`abort`) rather
than rolled back to the previous generation.

## Out of scope

- `packages/chord/src/node/{bundle,package,manifest,bundle-loader}.ts` and `bundler.ts` — Node
  bundling has no Rust equivalent, so only the abstract parts (catalogue and manifest shapes) are
  represented by the service catalogue and provider entries.
- The concrete network/thread transports for `pi-server` / `pi-client` (Stage 19); this crate
  ships the protocol and the loopback implementation only.

## Dependencies

No new third-party dependencies. The crate uses only crates already in the workspace:

- `serde` / `serde_json` — the shared JSON representation (`JsonValue = serde_json::Value`).
- `thiserror` — `DeltaError` / `FacetError` `Display`/`Error` impls.
- `parking_lot` (`=0.12`, pinned in the workspace root) — the `Mutex` behind the mutable
  replicated state and the facet registries, matching the workspace lock choice.

No async runtime is required. `context::block_on` is a minimal park/unpark executor used by the
tests and available to synchronous hosts; embedders with a runtime should poll the futures
themselves.

## Testing

```bash
cargo test -p pi-chord
cargo clippy -p pi-chord --all-targets -- -D warnings
cargo check --workspace --all-targets
```

Unit tests live next to each module; the `tests/` directory holds the contract suites:
`delta_contract.rs` (apply order, purity, rebase, round trips, conflict errors),
`replicated_state.rs` (hydration, ordered updates, gap detection),
`facets.rs` (ordering, failure isolation, gating, reload, disposal),
`facet_loader.rs` (load order, static loader, cleanup aggregation),
`services.rs` (request/response, error mapping, subscription lifecycle, provider/consumer
recovery, keyed observations, loopback), and
`service_wire.rs` (control calls, strict parsers, per-subscription state codecs, endpoint
publish/cleanup).
