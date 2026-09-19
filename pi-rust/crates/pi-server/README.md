# `pi-server` — host agent sessions over a wire protocol

`pi-server` is the Rust port of [`packages/server`](../../packages/server) in the
TypeScript monorepo: it exposes an in-process agent host as a *server* that clients reach over a
byte-oriented transport. One connection carries a handshake, then framed request/response traffic
for either the **server** scope or one attached **session** scope.

The crate owns no protocol vocabulary of its own. The wire types, framing and CBOR codec come from
`pi-protocol`'s `rpc` module (a port of `packages/protocol/src/cbor/*`), and service dispatch goes
through `pi-chord`'s remote-service layer (`RemoteServiceEndpoint`, `ServiceStateEncoder`,
`ServiceControlCall`). The server is the glue: connections, routing, attachments and transports.

## Upstream mapping

| Rust module | Upstream file(s) | Contents |
| --- | --- | --- |
| `errors` | `src/errors.ts` | `ServerErrorCode`, `ServerError`, protocol-error conversion |
| `connection` | `src/connection.ts` | `ByteConnection`, `ByteConnectionHandler`, acceptor alias, `ConnectionStage` |
| `listener` | `src/listener.ts` | `ServerListener` trait (`start` / `close`) |
| `types` | `src/types.ts` | host/presentation/attachment contracts, `ServerOptions`, observers |
| `session_router` | `src/session-router.ts` | per-client attachment routing, session acquire/release, termination |
| `server` | `src/server.ts` | `Server`: handshake, request dispatch, cancellation, subscriptions, lifecycle |
| `transports::memory` | `src/testing/server.ts` (+ loopback used by `src/testing/client.ts`) | in-process/loopback transport for tests and embedded use |
| `transports::unix` | `src/transports/unix/**` | Unix-domain socket listener |
| `transports::tcp` | — | TCP listener (added; upstream has no socket-of-record transport) |
| `transports::stdio` | — | stdin/stdout listener (added) |
| `transports::stream` | `src/transports/unix/listener.ts` (shared plumbing) | generic chunked-stream connection + read pump |
| `testing::host` | `src/testing/host.ts` | `TestServerHost`, `TestHarness`, gates and fault injection |
| `testing::client` | `src/testing/client.ts` | `ProtocolTestClient` over the in-memory transport |

The wire layer this crate consumes lives in `pi-protocol`:

| Rust module (`pi-protocol`) | Upstream `packages/protocol` |
| --- | --- |
| `rpc::protocol` | `src/rpc.ts` (message vocabulary, `PROTOCOL_VERSION`) |
| `rpc::framing` | `src/cbor/framing.ts` (4-byte length prefix) |
| `rpc::cbor` | `src/cbor/{encode,decode}.ts` (RFC 8949 subset) |
| `rpc::codec` | `src/cbor/*` strict key validation + message decoders |

## Design

- **Sync dispatch, async transport.** Fixtures come from `pi-chord`, whose service graph is fully
  synchronous. The server therefore runs service invocations on a blocking pool
  (`spawn_blocking`) while the socket read/write loops stay async Tokio tasks. Each connection has
  one outbound queue and one writer task, so `send` never blocks a reader.
- **Targets are `ServerTarget` or `SessionTarget`.** A request without an `attachment_id` routes
  to the server's own services; a request with one routes through the `SessionRouter` to a
  specific attached session. Attaching a client is itself a server-scoped service call
  (`pi.session-management.attach` / `.detach`), and the server answers it with an
  `Attachment` message carrying the session target.
- **Cancellation cannot interrupt a blocking call.** A `Cancel` still flips the request's
  `AbortSignal` and frees the id immediately; when the invocation returns, the server observes
  the abort and answers `cancelled` instead of the result.
- **Subscriptions** are installed as `pi-chord` provider listeners feeding
  `ServiceUpdate` frames; a subscription whose state is not serialisable is published via its
  `ServiceStateEncoder` (`to_json()`).

## Transports

| Transport | Constructor | Use |
| --- | --- | --- |
| In-memory | `transports::MemoryListener::new()` then `connect()` | tests, embedded hosts, same-process clients |
| TCP | `transports::TcpServerListener::new("127.0.0.1:0")` | network/loopback sockets |
| Unix | `transports::UnixServerListener::new(path)` (unix only) | local IPC |
| stdio | `transports::StdioListener::new()` | pipes (one server per process) |

All four are `ServerListener` implementations, so `ServerOptions::new(server_id, listeners)`
accepts any mix.

## Example

```rust
use std::sync::Arc;

use pi_server::transports::MemoryListener;
use pi_server::{Server, ServerHost, ServerListener, ServerOptions, SessionId};
use pi_server::testing::TestServerHost;

# async fn run() -> Result<(), pi_server::ServerError> {
let listener = MemoryListener::new();
let host = Arc::new(TestServerHost::new());
host.seed("session-1");

let options = ServerOptions::new(
    "3f1d2c9a-7b4e-4a1f-8c2d-9e5b6a7c8d90",
    vec![Arc::clone(&listener) as Arc<dyn ServerListener>],
);
let server: Arc<Server<SessionId>> =
    Server::new(Arc::clone(&host) as Arc<dyn ServerHost<SessionId>>, options)?;
server.start().await?;

let _client = listener.connect()?;
// ... drive the handshake and service calls through the client transport ...
server.close().await?;
# Ok(())
# }
```

## Tests

- `tests/conformance.rs` — protocol conformance over the in-memory transport: handshake accept /
  version mismatch / timeout, request round-trip, cancel, error mapping, session routing,
  attachment lifecycle, observers, fragmentation.
- `tests/transports.rs` — the same flow over a real `127.0.0.1:0` TCP socket and a temporary Unix
  socket. No external processes and no off-host traffic.

## Deliberate deviations from upstream

1. **Three extra transports.** Upstream ships only the Unix listener; this port adds in-memory
   (the analogue of upstream's `testing/` loopback), TCP and stdio, as the task requires.
2. **Handshake is not parked behind a promise.** `finish_handshake` runs synchronously; the
   `Handshaking` stage and timeout are still enforced.
3. **Simplified Unix lifecycle.** Stale-socket handling removes the path only when the filesystem
   entry is a socket (upstream's hash/link/inode protocol is not reproduced).
4. **`ProtocolError` is a plain data struct** shared with `pi-protocol`, so the server builds
   failures through `ServerError::to_protocol_error` instead of a `ProtocolError` constructor.
5. **`SessionMetadata` lives here** (`id()` plus an optional parent), because `pi-agent-core` has
   no equivalent trait; the test host implements it for `SessionId`.
