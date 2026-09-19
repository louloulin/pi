# Vendored `rquickjs-core` 0.9.0 (upstream double-free fix only)

This directory is a verbatim copy of the `rquickjs-core` 0.9.0 crates.io source
(with the normalized manifest trimmed of the crate's own `dev-dependencies`),
plus **one** upstream bug fix. `pi-rust/Cargo.toml` redirects the registry crate
here:

```toml
[patch.crates-io]
rquickjs-core = { path = "vendor/rquickjs-core" }
```

## Why

`pi --rpc` subprocesses were randomly killed by glibc's heap checker
(`free(): double free detected in tcache 2`, `malloc(): unaligned tcache chunk
detected`) or by SIGSEGV, which the RPC client observes as a dropped stdout
stream. The crash originates inside `rquickjs-core`, not in this repository.

`WithFuture::poll` acquires the runtime mutex and then frees the pinned lock
future **twice**:

```rust
// rquickjs-core 0.9.0, src/context/async/future.rs
let lock = ready!(pin.poll(cx));
unsafe { ManuallyDrop::drop(fut) };   // first drop
this.lock_state = LockState::Initial; // assignment runs `Drop for LockState` ->
                                      // `ManuallyDrop::drop(x)` a second time
```

`Drop for LockState` unconditionally calls `ManuallyDrop::drop(x)` for the
`Pending` variant, so the assignment double-drops `async_lock::futures::Lock`.
That lock future owns an `AcquireSlow` with an `event_listener::EventListener`
(`Pin<Box<InnerListener>>`); the second drop re-drops the already-freed listener,
unlinking it from the event-listener intrusive list a second time and using the
freed node's stale `prev`/`next` pointers. Whether that corrupts the heap, trips
the tcache checks, or segfaults depends on the tcache state — hence the
nondeterministic failures.

The `AcquireSlow` future is only created when `Mutex::try_lock` fails, so the
bug only fires when the QuickJS runtime mutex is actually contended. That is
normal in `pi`: the extension host spawns `runtime.drive()` and then races it
with `AsyncContext::async_with` calls. It is why the failures cluster under
load / high test parallelism and vanish with `--no-extensions`.

## The fix

Copied verbatim from upstream rquickjs-core 0.12.0 (which fixed this bug):

```diff
--- a/src/context/async/future.rs
+++ b/src/context/async/future.rs
@@
                 let lock = ready!(pin.poll(cx));
-                // at this point we have acquired a lock, so we will now drop the future allowing
-                // us to reused the memory space.
-                unsafe { ManuallyDrop::drop(fut) };
-                // The pinned memory is dropped so now we can freely move into it.
-                this.lock_state = LockState::Initial;
+                // Replace lock_state with Initial first, so the old Pending variant
+                // is moved out and its Drop runs exactly once.
+                let old = mem::replace(&mut this.lock_state, LockState::Initial);
+                drop(old);
                 break lock;
```

Affected upstream versions: 0.8.1, 0.9.0, 0.10.0, 0.11.0. Fixed in 0.12.0,
0.12.1, 0.12.2, 0.13.0, 0.14.0.

## Removing this vendor copy

The patch becomes unnecessary as soon as `crates/pi-extensions/Cargo.toml` can
depend on `rquickjs-core >= 0.12`. That is not a drop-in bump: 0.12 replaces the
`async_with!` closure signature (`FnOnce(Ctx) -> Pin<Box<dyn Future + Send>>`)
with `AsyncFnOnce(Ctx) -> R`, which needs Rust 1.85, while this workspace
declares `rust-version = "1.75"`. When the workspace MSRV is lifted, drop the
`[patch.crates-io]` entry, delete this directory, update the dependency and port
the `async_with!` call sites in `crates/pi-extensions/src/host.rs`.
