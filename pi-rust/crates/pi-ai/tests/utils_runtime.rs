//! Runtime-support util port tests: `utils/{headers,abort,abort-signals,
//! provider-env,pi-user-agent}.ts` → `pi_ai::utils::*`.
//!
//! # Adaptations
//!
//! * Upstream's `abort.ts` / `abort-signals.ts` have no dedicated test files
//!   (their behaviour is covered indirectly by the provider abort e2e suite,
//!   which needs live API keys). These vectors pin the same contracts
//!   locally: already-fired signals, cancellation winning the race, the
//!   abandoned operation still completing, and cleanup unsubscribing.
//! * `headersToRecord` upstream takes a web `Headers`; here the native
//!   `HeaderMap` path is `header_map_to_record` and the portable path is the
//!   generic `headers_to_record`, so both are covered.
//! * The WASM branches (`pi (browser)`, the polling `race_with_abort_signal`,
//!   the compiled-out `abort_signals`) cannot be executed without a `wasm32`
//!   toolchain; the WASM user-agent spelling is pinned through the public
//!   `WASM_USER_AGENT` constant instead.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pi_ai::utils::abort::{operation_signal, race_with_abort_signal};
use pi_ai::utils::combine_abort_signals;
use pi_ai::utils::headers::{header_map_to_record, headers_to_record, provider_headers_to_record};
use pi_ai::utils::pi_user_agent::{get_pi_user_agent, WASM_USER_AGENT};
use pi_ai::utils::provider_env::get_provider_env_value;
use pi_ai::{AbortSignal, ProviderEnv, ProviderHeaders, StreamError};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// headers
// ---------------------------------------------------------------------------

fn provider_headers(entries: &[(&str, Option<&str>)]) -> ProviderHeaders {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.map(str::to_string)))
        .collect()
}

#[test]
fn provider_headers_to_record_without_headers_is_none() {
    assert_eq!(provider_headers_to_record(None), None);
}

#[test]
fn provider_headers_to_record_with_no_entries_is_none() {
    let headers = provider_headers(&[]);
    assert_eq!(provider_headers_to_record(Some(&headers)), None);
}

#[test]
fn provider_headers_to_record_drops_none_values_and_returns_none_when_empty() {
    let all_dropped = provider_headers(&[("X-Drop", None), ("X-AlsoDrop", None)]);
    assert_eq!(provider_headers_to_record(Some(&all_dropped)), None);

    let mixed = provider_headers(&[("X-Keep", Some("1")), ("X-Drop", None)]);
    let record = provider_headers_to_record(Some(&mixed)).expect("one live header");
    assert_eq!(record.len(), 1);
    assert_eq!(record.get("X-Keep").map(String::as_str), Some("1"));
    assert!(!record.contains_key("X-Drop"));
}

#[test]
fn provider_headers_to_record_preserves_key_case() {
    let headers = provider_headers(&[("User-Agent", Some("pi (test)")), ("x-lower", Some("2"))]);
    let record = provider_headers_to_record(Some(&headers)).expect("two live headers");
    let mut expected = BTreeMap::new();
    expected.insert("User-Agent".to_string(), "pi (test)".to_string());
    expected.insert("x-lower".to_string(), "2".to_string());
    assert_eq!(record, expected);
}

#[test]
fn headers_to_record_accepts_any_string_pair_source() {
    let source = vec![("A", "1"), ("b", "2")];
    let mut expected = BTreeMap::new();
    expected.insert("A".to_string(), "1".to_string());
    expected.insert("b".to_string(), "2".to_string());
    assert_eq!(headers_to_record(source), expected);

    let map: BTreeMap<String, String> = [("x-one".to_string(), "1".to_string())]
        .into_iter()
        .collect();
    assert_eq!(
        headers_to_record(&map),
        [("x-one".to_string(), "1".to_string())]
            .into_iter()
            .collect()
    );
}

#[test]
fn header_map_to_record_lowercases_names_and_joins_repeats() {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("content-type"),
        HeaderValue::from_static("text/event-stream"),
    );
    // `HeaderName` normalises to lowercase even when the literal is mixed.
    headers.insert(
        HeaderName::from_static("x-request-id"),
        HeaderValue::from_static("req-1"),
    );
    headers.append(
        HeaderName::from_static("set-cookie"),
        HeaderValue::from_static("a=1"),
    );
    headers.append(
        HeaderName::from_static("set-cookie"),
        HeaderValue::from_static("b=2"),
    );

    let record = header_map_to_record(&headers);
    assert_eq!(
        record.get("content-type").map(String::as_str),
        Some("text/event-stream")
    );
    assert_eq!(
        record.get("x-request-id").map(String::as_str),
        Some("req-1")
    );
    assert_eq!(
        record.get("set-cookie").map(String::as_str),
        Some("a=1, b=2")
    );
    assert_eq!(record.len(), 3, "one entry per distinct name");
}

#[test]
fn header_map_to_record_of_empty_map_is_empty() {
    assert!(header_map_to_record(&HeaderMap::new()).is_empty());
}

// ---------------------------------------------------------------------------
// abort
// ---------------------------------------------------------------------------

fn triggerable_signal() -> (CancellationToken, AbortSignal) {
    let token = CancellationToken::new();
    let signal = AbortSignal::native(token.clone());
    (token, signal)
}

#[test]
fn operation_signal_without_a_signal_never_cancels() {
    let signal = operation_signal(None);
    assert!(!signal.is_cancelled());
}

#[test]
fn operation_signal_passes_a_supplied_signal_through() {
    let (_, signal) = triggerable_signal();
    assert!(!operation_signal(Some(signal)).is_cancelled());

    let (token, cancelled) = triggerable_signal();
    token.cancel();
    assert!(operation_signal(Some(cancelled)).is_cancelled());
}

#[tokio::test(flavor = "current_thread")]
async fn race_returns_the_operation_output_when_it_wins() {
    let (_, signal) = triggerable_signal();
    let result = race_with_abort_signal(async { 21 * 2 }, &signal).await;
    assert_eq!(result.ok(), Some(42));
}

#[tokio::test(flavor = "current_thread")]
async fn race_passes_a_fallible_operations_own_error_through_untouched() {
    // The operation's output is `T`, so its own error is part of a successful
    // race (`Ok(Err(..))`): `Err` from the race itself means cancellation.
    let (_, signal) = triggerable_signal();
    let result = race_with_abort_signal(
        async { Err::<u8, StreamError>(StreamError::Malformed("boom".to_string())) },
        &signal,
    )
    .await;
    assert!(matches!(result, Ok(Err(StreamError::Malformed(message))) if message == "boom"));
}

#[tokio::test(flavor = "current_thread")]
async fn race_with_an_already_cancelled_signal_never_polls_the_operation() {
    let (token, signal) = triggerable_signal();
    token.cancel();
    let polls = Arc::new(AtomicUsize::new(0));
    let polled = Arc::clone(&polls);
    let result = race_with_abort_signal(
        async move {
            polled.fetch_add(1, Ordering::SeqCst);
            1
        },
        &signal,
    )
    .await;
    assert!(matches!(result, Err(StreamError::Aborted)));
    assert_eq!(polls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn race_returns_aborted_when_the_signal_wins() {
    let (token, signal) = triggerable_signal();
    let finished = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&finished);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        token.cancel();
    });
    let result = race_with_abort_signal(
        async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            flag.store(true, Ordering::SeqCst);
            1
        },
        &signal,
    )
    .await;
    assert!(matches!(result, Err(StreamError::Aborted)));
    assert!(
        !finished.load(Ordering::SeqCst),
        "the caller must not wait for the abandoned operation"
    );

    // The abandoned future keeps running: dropping the `JoinHandle` detaches
    // the task instead of aborting it (upstream attaches a no-op `catch` to
    // the abandoned promise for the same reason).
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(
        finished.load(Ordering::SeqCst),
        "the abandoned operation still runs to completion"
    );
}

// ---------------------------------------------------------------------------
// abort_signals
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn combining_no_signals_yields_no_signal() {
    let combined = combine_abort_signals(&[]);
    assert!(combined.signal().is_none());
    combined.cleanup();
}

#[tokio::test(flavor = "current_thread")]
async fn combining_no_active_signals_yields_no_signal() {
    let combined = combine_abort_signals(&[None, None]);
    assert!(combined.signal().is_none());
    combined.cleanup();
}

#[tokio::test(flavor = "current_thread")]
async fn combining_one_signal_returns_it_unchanged() {
    let (token, signal) = triggerable_signal();
    let combined = combine_abort_signals(&[None, Some(signal)]);
    let parent = combined
        .signal()
        .expect("the single signal is passed through");
    assert!(!parent.is_cancelled());
    token.cancel();
    assert!(
        parent.is_cancelled(),
        "the pass-through signal is the child itself"
    );
    combined.cleanup();
}

#[tokio::test(flavor = "current_thread")]
async fn combining_several_signals_fires_the_parent_when_any_child_fires() {
    let (trigger_a, signal_a) = triggerable_signal();
    let (trigger_b, signal_b) = triggerable_signal();
    let combined = combine_abort_signals(&[Some(signal_a), Some(signal_b)]);
    let parent = combined.signal().expect("a parent for two children");
    assert!(!parent.is_cancelled());

    trigger_a.cancel();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(parent.is_cancelled(), "child A cancels the parent");

    // The parent stays cancelled and the second child is still observed.
    assert!(!trigger_b.is_cancelled());
    trigger_b.cancel();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(parent.is_cancelled());
    combined.cleanup();
}

#[tokio::test(flavor = "current_thread")]
async fn combining_signals_with_an_already_cancelled_child_cancels_immediately() {
    let (token_a, signal_a) = triggerable_signal();
    let (_, signal_b) = triggerable_signal();
    token_a.cancel();
    let combined = combine_abort_signals(&[Some(signal_a), Some(signal_b)]);
    assert!(
        combined.signal().expect("a parent").is_cancelled(),
        "a child that already fired cancels the parent without waiting"
    );
    combined.cleanup();
}

#[tokio::test(flavor = "current_thread")]
async fn cleanup_unsubscribes_from_the_children() {
    let (trigger_a, signal_a) = triggerable_signal();
    let (_, signal_b) = triggerable_signal();
    let combined = combine_abort_signals(&[Some(signal_a), Some(signal_b)]);
    let parent = combined.signal().expect("a parent for two children");
    combined.cleanup();

    trigger_a.cancel();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !parent.is_cancelled(),
        "after cleanup a child must no longer cancel the parent"
    );
    combined.cleanup();
}

// ---------------------------------------------------------------------------
// provider_env
// ---------------------------------------------------------------------------

// Each test owns a distinct probe name: the vectors run on parallel threads
// inside one process, so sharing a name would let one test observe another
// test's `set_var`.
const SCOPED_PROBE: &str = "PI_AI_UTILS_RUNTIME_SCOPED_PROBE";
const AMBIENT_PROBE: &str = "PI_AI_UTILS_RUNTIME_AMBIENT_PROBE";
const PRECEDENCE_PROBE: &str = "PI_AI_UTILS_RUNTIME_PRECEDENCE_PROBE";
const EMPTY_PROBE: &str = "PI_AI_UTILS_RUNTIME_EMPTY_PROBE";
const ABSENT_PROBE: &str = "PI_AI_UTILS_RUNTIME_ABSENT_PROBE";

#[test]
fn provider_env_prefers_the_scoped_map() {
    let env: ProviderEnv = [(SCOPED_PROBE.to_string(), "scoped".to_string())]
        .into_iter()
        .collect();
    assert_eq!(
        get_provider_env_value(SCOPED_PROBE, Some(&env)).as_deref(),
        Some("scoped")
    );
    assert_eq!(get_provider_env_value(SCOPED_PROBE, None), None);
}

#[test]
fn provider_env_reads_the_process_environment() {
    std::env::set_var(AMBIENT_PROBE, "ambient");
    assert_eq!(
        get_provider_env_value(AMBIENT_PROBE, None).as_deref(),
        Some("ambient")
    );
    assert_eq!(
        get_provider_env_value(AMBIENT_PROBE, Some(&BTreeMap::new())).as_deref(),
        Some("ambient")
    );
    std::env::remove_var(AMBIENT_PROBE);
}

#[test]
fn provider_env_process_value_wins_over_a_scoped_empty_string() {
    // Upstream's `env?.[name] || process.env[name]`: `""` is falsy, so the
    // scoped empty string must not shadow the real process value.
    std::env::set_var(PRECEDENCE_PROBE, "ambient");
    let env: ProviderEnv = [(PRECEDENCE_PROBE.to_string(), String::new())]
        .into_iter()
        .collect();
    assert_eq!(
        get_provider_env_value(PRECEDENCE_PROBE, Some(&env)).as_deref(),
        Some("ambient")
    );
    std::env::remove_var(PRECEDENCE_PROBE);
}

#[test]
fn provider_env_treats_empty_values_as_absent() {
    std::env::set_var(EMPTY_PROBE, "");
    let env: ProviderEnv = [(EMPTY_PROBE.to_string(), String::new())]
        .into_iter()
        .collect();
    assert_eq!(get_provider_env_value(EMPTY_PROBE, Some(&env)), None);
    assert_eq!(get_provider_env_value(EMPTY_PROBE, None), None);
    std::env::remove_var(EMPTY_PROBE);
}

#[test]
fn provider_env_missing_name_is_none() {
    std::env::remove_var(ABSENT_PROBE);
    assert_eq!(get_provider_env_value(ABSENT_PROBE, None), None);
    assert_eq!(
        get_provider_env_value(ABSENT_PROBE, Some(&BTreeMap::new())),
        None
    );
}

// ---------------------------------------------------------------------------
// pi_user_agent
// ---------------------------------------------------------------------------

#[test]
fn user_agent_has_the_upstream_native_shape() {
    let user_agent = get_pi_user_agent();
    assert!(user_agent.starts_with("pi ("), "got {user_agent}");
    assert!(user_agent.ends_with(')'), "got {user_agent}");
    assert!(!user_agent.contains('\n'), "got {user_agent}");

    let inner = user_agent
        .strip_prefix("pi (")
        .and_then(|rest| rest.strip_suffix(')'))
        .expect("the user agent is wrapped in `pi (...)`");
    let mut parts = inner.split("; ");
    let platform_and_release = parts.next().expect("a platform and release");
    let arch = parts.next().expect("an architecture");
    assert!(parts.next().is_none(), "exactly one `; ` separator");

    let expected_platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let mut platform_parts = platform_and_release.split(' ');
    assert_eq!(platform_parts.next(), Some(expected_platform));
    let release = platform_parts.next().expect("a non-empty release");
    assert!(!release.is_empty());
    assert_eq!(arch, std::env::consts::ARCH);
}

#[test]
fn user_agent_pins_the_wasm_browser_spelling() {
    assert_eq!(WASM_USER_AGENT, "pi (browser)");
    assert!(WASM_USER_AGENT.starts_with("pi ("));
}
