# `pi-client` — remote client SDK for the Pi server protocol

`pi-client` is the Rust port of [`packages/client`](../../packages/client): a
connected client for a `pi-server` over any byte-oriented transport. It owns the
connection state machine, the request/response correlation, service
subscriptions and the concrete Unix-domain-socket transport, and leaves the
protocol vocabulary to `pi-protocol` and the service payloads to `pi-chord`.

The crate is *native-first*, like `pi-server`: the core is free of OS APIs and
the Unix transport lives behind `#[cfg(unix)]` as `pi_client::unix`. It is
outside the WASM CI target set (`pi-agent-core` / `pi-ai` / `pi-protocol`).

## Upstream mapping

| Rust module | Upstream file(s) | Contents |
| --- | --- | --- |
| `client` | `src/client.ts` | `Client`, `ClientOptions`, pending-request correlation, listener registries |
| `connection` | `src/connection.ts` | `Connection` state machine, handshake, decoder ownership |
| `transport` | `src/transport.ts` | `ByteTransport`, `ByteTransportHandlers`, `ByteTransportFactory` |
| `types` | `src/types.ts` | `ConnectionState`, listener aliases, `Unsubscribe` |
| `errors` | `src/errors.ts` | `ClientError` and the `Disconnected` / `Protocol` / `Server` / `Disposed` split |
| `cancel` | `AbortSignal` (implicit) | `RequestCancel` token |
| `subscription` | `client.ts` `#serviceListeners` + `types.ts` `ServiceSubscription` | hydration, ordered delivery, dispose |
| `unix` | `src/unix.ts` | `AF_UNIX` transport factory and local server discovery |
| `testing` | `packages/server/src/testing/client.ts` (inverted) | scripted in-memory server peer |
| — | `promise.ts` | replaced by `tokio::sync::oneshot` |

The wire layer this crate consumes lives in `pi-protocol` (`rpc::protocol`,
`rpc::framing`, `rpc::cbor`, `rpc::codec`), and the service payloads in
`pi-chord::services` (`ServiceCall`, catalogue/subscription wire helpers,
`ServiceMode`).

## Usage

```rust
use pi_client::{
    unix::{create_unix_transport_factory, UnixTransportOptions},
    Client, ClientOptions, RpcTarget, ServerTarget,
};

# async fn run() -> Result<(), pi_client::ClientError> {
let factory = create_unix_transport_factory(UnixTransportOptions::new("/tmp/pi.sock"))?;
let client = Client::connect_with(ClientOptions::new(
    factory,
    "3f1d2c9a-7b4e-4a1f-8c2d-9e5b6a7c8d90",
))
.await?;

let catalogue = client
    .service_catalogue(
        RpcTarget::Server(ServerTarget { server_id: client.server_id().to_owned() }),
        None,
    )
    .await?;
println!("{} services", catalogue.len());
client.dispose().await;
# Ok(())
# }
```

For tests and embedded use, `pi_client::testing::memory_transport()` returns a
connected in-memory pair whose server end is a `ScriptedServer` that decodes
whatever the client sent and encodes whatever the test wants back.

## Design

- **One transport per connection attempt.** `Connection` owns at most one
  `ByteTransport` and a monotonically increasing connection id; stale callbacks
  from a superseded attempt are ignored and the late transport is closed instead
  of installed.
- **Handshake before traffic.** The client sends `hello` immediately after the
  transport opens and answers nothing until `hello`/`hello_error` arrives. A
  server id that disagrees with the configured one fails the connection, not
  just the attempt.
- **Weak client reference.** `Connection` holds a `Weak<dyn ConnectionSink>`
  pointing at the client, so there is no `Client` ↔ `Connection` cycle; the
  client installs the connection into a `OnceLock` after it owns the `Arc`.
- **Correlation, not channels per request.** Pending requests live in one map
  keyed by `request-N`. A response for an unknown id is a protocol violation and
  fails the connection; a cancelled request keeps its entry so the server's late
  answer is absorbed rather than treated as unknown.
- **Ordered subscriptions.** One mutex guards the `pi-chord` state decoder and
  both the raw wire backlog and the decoded backlog, so updates that race the
  subscribe response cannot be reordered ahead of the snapshot. Delivery is a
  single task draining an unbounded FIFO, the Rust form of upstream's
  `deliveryTail`; a panicking listener is caught and reported through
  `ClientOptions::with_listener_error_handler`.
- **Explicit cancellation.** Rust has no `AbortSignal`, so `RequestCancel` is a
  cloneable token backed by an atomic flag and a `Notify`. Cancelling is
  idempotent, and a token cancelled before the request starts is still observed.

## Deliberate divergences from upstream

| Upstream | Here | Why |
| --- | --- | --- |
| `Client.connect(options)` | `Client::connect_with(options)` | a static `connect` cannot share a name with the instance method |
| `AbortSignal` parameter | `Option<RequestCancel>` | no `AbortSignal` in Rust |
| `createClientServiceTransport` | not ported | `pi-chord`'s `RemoteServiceTransport` is synchronous; an async client cannot implement it without blocking a runtime thread. Adapt `Client::request` / `subscribe_service` instead |
| options throw `TypeError` | `ClientError::Configuration` / `Listener` | Rust reports fallible configuration as values |
| listener registration returns a closure | `Unsubscribe` value dropped or run | Rust cannot express a droppable cancel token as a closure |
| `maxPendingBytes` counts the queue | counts bytes inside an in-flight `send` | writes are serialised by one mutex, so the in-flight byte count is the honest ceiling |
| discovery returns sorted results | same, via `stream::iter(...).buffered(16)` | concurrency is capped at 16 probes without losing input order |

## Tests

`cargo test -p pi-client` runs three layers offline:

- unit tests in the crate (state spelling, cancellation, error coercion,
  discovery validation);
- `tests/client.rs`, a scripted transcript of handshake, request, cancel,
  subscription, attachment and dispose flows;
- `tests/server_e2e.rs`, a real `pi-server` on the other end of both the
  in-memory transport and a real Unix socket (`dev-dependencies` only).
