//! Regression test for the rquickjs-core double free that killed `pi --rpc`
//! subprocesses at random (LUM-1083).
//!
//! `rquickjs-core <= 0.11` double-dropped the mutex lock future in
//! `WithFuture::poll`: it called `ManuallyDrop::drop(fut)` and then assigned
//! `this.lock_state = LockState::Initial`, which ran `Drop for LockState` and
//! released the very same `Pending` variant a second time.
//!
//! That double release only bites on the contended path. `async-lock`'s
//! `LockInner` keeps its `AcquireSlow` future in the struct after acquiring the
//! mutex, and `AcquireSlow` owns an `event_listener::EventListener` when the
//! lock was taken through the slow path *and* the listener was still armed
//! (notified but the lock state changed before it could be consumed). The
//! second drop then frees that listener again: `event_listener`'s
//! `InnerListener::drop` walks a list through already-freed memory and the
//! process dies with SIGSEGV — or glibc reports the heap corruption itself with
//! `free(): double free detected in tcache 2` / `attempt to subtract with
//! overflow` in `event-listener/src/intrusive.rs`.
//!
//! This test forces that contention: several tokio tasks call `async_with!`
//! concurrently on one `AsyncContext` (plus the driver the extension host
//! always runs) so that `Mutex::try_lock` frequently fails and the slow path is
//! taken. Because the crash needs the lock to change hands inside a very short
//! window, the failure is probabilistic: roughly half of the runs abort the
//! test process when the unfixed crate is linked in. A failing run therefore
//! reports a signal (`SIGSEGV`, or a glibc `double free`/`abort`) instead of an
//! assertion failure — that is the bug, not a flaky test. With
//! `pi-rust/vendor/rquickjs-core` patched (see its `PATCH.md`) the double drop
//! is gone and the test passes consistently.
//!
//! Raise the workload with `RQ_TASKS` / `RQ_ITERS` to reproduce it faster.

use rquickjs_core::{async_with, AsyncContext, AsyncRuntime};

/// Number of concurrent `async_with!` callers. Raise via `RQ_TASKS`.
const TASKS: usize = 16;
/// `async_with!` calls per task. Raise via `RQ_ITERS`.
const ITERATIONS: usize = 20_000;

fn env_usize(key: &str, fallback: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
}

#[test]
fn contended_async_with_does_not_double_free() {
    let tasks = env_usize("RQ_TASKS", TASKS);
    let iterations = env_usize("RQ_ITERS", ITERATIONS);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .expect("build tokio runtime");

    runtime.block_on(async {
        let js = AsyncRuntime::new().expect("quickjs runtime");
        let context = AsyncContext::full(&js).await.expect("async context");
        // The extension host always runs a driver next to its `async_with!`
        // calls; it is one more user of the runtime lock.
        tokio::spawn(js.drive());

        let mut handles = Vec::new();
        for _ in 0..tasks {
            let context = context.clone();
            handles.push(tokio::spawn(async move {
                for _ in 0..iterations {
                    async_with!(context => |ctx| {
                        // Short on purpose: the double free needs the lock to be
                        // handed over while a waiter is re-arming, and long
                        // critical sections make that window comparatively smaller.
                        ctx.eval::<i64, _>("1 + 1")
                            .map_err(|error| error.to_string())?;
                        Ok::<(), String>(())
                    })
                    .await
                    .expect("async_with");
                }
            }));
        }
        for handle in handles {
            handle.await.expect("async_with task");
        }
    });
}
