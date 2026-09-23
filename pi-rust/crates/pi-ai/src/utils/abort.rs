//! Operation-local signals and abort racing — port of
//! `packages/ai/src/utils/abort.ts`.
//!
//! Upstream uses these two helpers in the auth / credential-store path: the
//! public API's signal is optional, so [`operation_signal`] supplies a never
//! aborted stand-in, and [`race_with_abort_signal`] stops waiting as soon as
//! that signal fires while still letting the abandoned promise settle.
//!
//! # Deliberate differences from the TypeScript implementation
//!
//! * **No `AbortError` payload and no cancellation reason.** Upstream's
//!   `abortReason(signal)` reads `signal.reason` (or fabricates a named
//!   `AbortError`). Rust [`AbortSignal`] carries no reason — its cancellation
//!   is a bare `CancellationToken` / WASM flag — so every cancellation
//!   surfaces as [`StreamError::Aborted`]. Callers must not expect a message
//!   from the signal.
//! * **The abandoned operation keeps running, and the native port pays for
//!   that with `tokio::spawn`.** Upstream attaches a no-op `catch` to the
//!   abandoned promise so a late rejection is never an unhandled rejection.
//!   Rust has no unhandled-rejection concept, but dropping a future *does*
//!   cancel it, so the faithful translation is to spawn the operation on the
//!   tokio runtime and drop the `JoinHandle` when the signal wins: dropping a
//!   `JoinHandle` detaches the task instead of aborting it, so the operation
//!   runs to completion and its output (or error) is dropped at that point.
//!   [`Future`] values must therefore be `Send + 'static` on native. A future
//!   that panics after being abandoned panics inside its own task and cannot
//!   unwind into the caller, which is the closest Rust equivalent of the
//!   no-op `catch`.
//! * **WASM cannot spawn, so it polls instead.** The WASM branch checks
//!   `is_cancelled` before and after the await (the crate's existing WASM
//!   cancellation model) and carries no `Send + 'static` bounds, because
//!   `wasm-bindgen-futures` values are not `Send`. A signal that fires while
//!   the operation is awaiting is only observed once that await returns, and
//!   the abandoned operation is not kept alive — there is no task to keep.
//!   This is the same degradation the crate documents for
//!   `AbortSignal::cancelled`, which does not exist on WASM.

use std::future::Future;

use crate::types::{AbortSignal, StreamError};

/// `operationSignal(signal)` — return `signal`, or a never-cancelled signal
/// when the caller did not pass one.
pub fn operation_signal(signal: Option<AbortSignal>) -> AbortSignal {
    signal.unwrap_or_default()
}

/// `raceWithAbortSignal(operation, signal)` — await `operation`, but give up
/// with [`StreamError::Aborted`] as soon as `signal` fires.
///
/// `Err` is reserved for cancellation (and for a panic, which is resumed):
/// the operation's *output* is `T`, so a fallible operation reports its own
/// error as `Ok(Err(..))` and `race(..).await?` composes as usual.
///
/// An already-cancelled signal returns immediately; the operation is never
/// polled in that case (upstream starts it and swallows its rejection, which
/// is unobservable to the caller either way).
#[cfg(not(target_arch = "wasm32"))]
pub async fn race_with_abort_signal<F, T>(
    operation: F,
    signal: &AbortSignal,
) -> Result<T, StreamError>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    if signal.is_cancelled() {
        return Err(StreamError::Aborted);
    }

    let handle = tokio::spawn(operation);
    tokio::select! {
        joined = handle => joined.map_err(join_error_to_stream_error),
        _ = signal.cancelled() => Err(StreamError::Aborted),
    }
}

/// Turn a `JoinError` back into what the operation did: a panicking operation
/// propagates its panic (the task boundary would otherwise swallow it), and a
/// cancelled task — which this module never aborts — maps to
/// [`StreamError::Aborted`].
#[cfg(not(target_arch = "wasm32"))]
fn join_error_to_stream_error(error: tokio::task::JoinError) -> StreamError {
    if error.is_panic() {
        std::panic::resume_unwind(error.into_panic());
    }
    StreamError::Aborted
}

/// `raceWithAbortSignal(operation, signal)` — WASM degradation: check the
/// signal before awaiting and again once the operation has finished. See the
/// module docs for why the abandoned future is not kept alive here.
#[cfg(target_arch = "wasm32")]
pub async fn race_with_abort_signal<F, T>(
    operation: F,
    signal: &AbortSignal,
) -> Result<T, StreamError>
where
    F: Future<Output = T>,
{
    if signal.is_cancelled() {
        return Err(StreamError::Aborted);
    }
    let output = operation.await;
    if signal.is_cancelled() {
        return Err(StreamError::Aborted);
    }
    Ok(output)
}
