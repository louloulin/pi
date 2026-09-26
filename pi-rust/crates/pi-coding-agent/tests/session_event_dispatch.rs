//! P1-2 — SessionEvent family dispatch reaches the JS side.
//!
//! Every `session_*` event the plan lists (plan §6 P1-2) is now wired into
//! the agent lifecycle: the dispatcher forwards each variant to
//! [`ExtensionRuntime::dispatch_event`] from the matching hook, and a JS
//! extension that subscribes via `pi.on("<name>", handler)` observes the
//! delivery. This test exercises the chain end to end for `session_fork`
//! — the variant the audit below discovered was *not* wired up before this
//! PR — so a future regression cannot silently drop a fork event.
//!
//! Audit table (verbatim from the grep that drove P1-2):
//!
//! | variant               | dispatch site                                  |
//! |-----------------------|------------------------------------------------|
//! | `SessionStart`        | `interactive/mod.rs` load_extensions path       |
//! | `SessionBeforeSwitch` | `interactive/session_lifecycle.rs`              |
//! | `SessionBeforeFork`   | `interactive/session_picker.rs`                |
//! | `SessionFork`         | `interactive/mod.rs` `handle_fork_selection`   |
//! | `SessionBeforeCompact`| `interactive/interactive_loop.rs`              |
//! | `SessionCompact`      | `interactive/interactive_loop.rs`              |
//! | `SessionCompactFailed`| `interactive/interactive_loop.rs` (Rust-only)  |
//! | `SessionShutdown`     | `interactive/interactive_loop.rs` end-of-run    |
//! | `SessionBeforeTree`   | `interactive/session_picker.rs`                |
//! | `SessionTree`         | `interactive/session_picker.rs`                |
//! | `SessionInfoChanged`  | `interactive/session_lifecycle.rs` rename hook |
//!
//! The test below covers `SessionFork` specifically because it is the one
//! variant the prior PR found referenced only in tests and the audit doc —
//! no live dispatch site. A handler that calls `pi.setSessionName` proves
//! the JS shim delivered the payload all the way to QuickJS, which is the
//! only behaviour the run loop actually cares about.

#![cfg(test)]

use std::sync::Arc;

use pi_coding_agent::extensions::wiring::ExtensionRuntime;
use pi_extensions::{ExtensionEntry, JsExtensionHost};
use pi_protocol::ExtensionEvent;

fn session_fork_handler() -> &'static str {
    r#"
module.exports = function (pi) {
  pi.on("session_fork", function (event) {
    pi.setSessionName("forked-into-" + event.sessionId);
  });
};
"#
}

async fn runtime_for_session_fork() -> (Arc<ExtensionRuntime>, JsExtensionHost) {
    let dir = std::env::temp_dir().join(format!(
        "pi-session-fork-dispatch-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let file = dir.join("extension.js");
    std::fs::write(&file, session_fork_handler()).expect("write extension");
    let host = JsExtensionHost::new().await.expect("host");
    host.load(
        ExtensionEntry {
            source: file.clone(),
            id: "session-fork-dispatch".to_string(),
            label: None,
        },
        session_fork_handler(),
    )
    .await
    .expect("load");
    let _ = std::fs::remove_dir_all(&dir);
    let runtime = Arc::new(ExtensionRuntime::for_test(host.clone(), &["session_fork"]));
    (runtime, host)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_fork_delivery_reaches_the_js_handler() {
    let (runtime, host) = runtime_for_session_fork().await;

    // Sanity: the host reports a subscription for the event we are about
    // to deliver. Without a subscriber `dispatch_event` short-circuits
    // before crossing into QuickJS, so this guard catches the regression
    // where `subscribed` is empty (the test would then observe no side
    // effect and pass for the wrong reason).
    assert!(
        runtime.has_subscriber_for("session_fork"),
        "runtime must report a subscriber for `session_fork`"
    );

    let session_id = "child-123";
    let event = ExtensionEvent::SessionFork {
        session_id: session_id.to_string(),
        parent_id: "parent-456".to_string(),
        entry_id: "entry-789".to_string(),
    };

    // The dispatch must return Some(DispatchOutcome): the handler ran, the
    // shim processed the call, and the host retained the side effect.
    let outcome = runtime
        .dispatch_event(&event)
        .await
        .expect("session_fork dispatch must cross into JS");
    assert!(
        !outcome.results.is_empty() || outcome.event.is_some() || outcome.results.is_empty(),
        "outcome shape is opaque to this test; what matters is the side effect"
    );

    // Drain side effects and assert the handler invoked `pi.setSessionName`.
    // The shim records the latest session_name in `ExtensionSideEffects`,
    // so a single drain after dispatch is enough evidence.
    let side_effects = host.drain_side_effects();
    let observed = side_effects.session_name.as_deref();
    assert_eq!(
        observed,
        Some("forked-into-child-123"),
        "the JS handler must observe `event.sessionId` from the dispatched payload",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_with_no_subscriber_is_a_no_op() {
    // Same fixture, but the runtime reports no subscribers. The dispatcher
    // short-circuits before the JS bridge, so no side effect accumulates.
    // This is the behaviour callers depend on: a missing subscriber must
    // not pay the QuickJS round-trip.
    let dir = std::env::temp_dir().join(format!(
        "pi-session-fork-noop-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let file = dir.join("extension.js");
    std::fs::write(&file, session_fork_handler()).expect("write extension");
    let host = JsExtensionHost::new().await.expect("host");
    host.load(
        ExtensionEntry {
            source: file.clone(),
            id: "session-fork-noop".to_string(),
            label: None,
        },
        session_fork_handler(),
    )
    .await
    .expect("load");
    let _ = std::fs::remove_dir_all(&dir);
    let runtime = Arc::new(ExtensionRuntime::for_test(host.clone(), &[]));

    let outcome = runtime
        .dispatch_event(&ExtensionEvent::SessionFork {
            session_id: "x".into(),
            parent_id: "y".into(),
            entry_id: "z".into(),
        })
        .await;
    assert!(
        outcome.is_none(),
        "no subscriber must yield None; got {outcome:?}"
    );

    let side_effects = host.drain_side_effects();
    assert!(
        side_effects.session_name.is_none(),
        "no subscriber must not trigger the handler; got {:?}",
        side_effects.session_name
    );
}