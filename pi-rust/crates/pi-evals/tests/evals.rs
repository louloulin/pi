//! End-to-end tests for the eval harness and the built-in suites.
//!
//! The default `cargo test -p pi-evals` path is fully offline: every case in
//! [`pi_evals::all_suites`] either uses the in-process faux provider, the
//! loopback fixture server, or files on disk. The single network case is
//! recorded as `skipped`; [`live_provider_eval`] is the opt-in entry point for
//! running it for real.
//!
//! Every async test stays on the default **current-thread** tokio runtime. The
//! extension cases drive QuickJS through `pi_extensions::JsExtensionHost`, whose
//! `AsyncRuntime` driver is a `tokio::spawn`ed task that touches engine state; the
//! multi-thread flavour would poll that driver on a worker thread while the case's
//! own thread evaluates JS, which is not safe.
//! `pi-extensions`' own tests build a `new_current_thread` runtime for the same
//! reason.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use pi_evals::harness::{Case, CaseOutput, CaseStatus, EvalSuite, Judgment, RunOptions};
use pi_evals::{run_suites, AcceptAllJudge, FixtureResponse, FixtureServer};
use serde_json::json;

/// Prefix for artifact output; `target/` is git-ignored.
fn artifact_dir(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("eval-tests")
        .join(name)
}

#[tokio::test]
async fn offline_suites_are_green() {
    let report = run_suites(&pi_evals::all_suites(), &RunOptions::offline()).await;
    println!("{}", report.to_text());

    let failed: Vec<&str> = report
        .cases()
        .filter(|case| case.status == CaseStatus::Failed)
        .map(|case| case.id.as_str())
        .collect();
    assert!(failed.is_empty(), "failing eval cases: {failed:?}");

    assert!(
        report.totals.passed >= 10,
        "expected the offline suites to cover at least ten cases, got {}",
        report.totals.passed
    );
    assert!(
        report.totals.is_success(),
        "report totals: {:?}",
        report.totals
    );
    // The live provider case must be reported as skipped whenever the
    // operator has not opted in.
    if std::env::var("PI_EVAL_LIVE").is_err() {
        assert!(
            report.totals.skipped >= 1,
            "the live provider case should be skipped offline"
        );
        assert!(
            report.cases().any(
                |case| case.id == "providers-live-openai" && case.status == CaseStatus::Skipped
            ),
            "providers-live-openai should be recorded as skipped"
        );
    }
    assert_eq!(
        report.totals.total,
        report.totals.passed + report.totals.failed + report.totals.skipped
    );
}

#[tokio::test]
async fn harness_writes_artifacts() {
    let dir = artifact_dir("artifacts");
    let _ = std::fs::remove_dir_all(&dir);
    let options = RunOptions {
        artifacts_dir: Some(dir.clone()),
        suite_filter: Some("docs".into()),
        ..RunOptions::default()
    };
    let report = run_suites(&pi_evals::all_suites(), &options).await;
    assert!(report.totals.is_success());
    report.write(&dir).expect("write artifacts");

    for file in ["report.json", "report.txt", "runs.jsonl"] {
        assert!(dir.join(file).exists(), "missing artifact {file}");
    }
    let parsed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("report.json")).unwrap()).unwrap();
    assert!(parsed["suites"]
        .as_array()
        .map(|s| !s.is_empty())
        .unwrap_or(false));
    let runs = std::fs::read_to_string(dir.join("runs.jsonl")).unwrap();
    assert_eq!(runs.lines().count(), report.totals.total);
}

#[tokio::test]
async fn case_and_suite_filters_select_cases() {
    let options = RunOptions {
        suite_filter: Some("smoke".into()),
        case_filter: Some("smoke-faux-answer".into()),
        ..RunOptions::default()
    };
    let report = run_suites(&pi_evals::all_suites(), &options).await;
    assert_eq!(report.totals.total, 1);
    assert_eq!(report.suites.len(), 1);
    assert!(report.totals.is_success());
}

#[tokio::test]
async fn repetitions_rerun_each_case() {
    let options = RunOptions {
        suite_filter: Some("docs".into()),
        case_filter: Some("docs-code-fences-balanced".into()),
        repetitions: 2,
        ..RunOptions::default()
    };
    let report = run_suites(&pi_evals::all_suites(), &options).await;
    assert_eq!(report.totals.total, 2);
    assert!(report.totals.is_success());
    let repetitions: Vec<u32> = report.cases().map(|case| case.repetition).collect();
    assert_eq!(repetitions, vec![0, 1]);
}

#[tokio::test]
async fn failing_judgement_marks_the_case_failed() {
    let suite = EvalSuite::new("self-check").with_case(
        Case::builder("fails")
            .description("assertion judge rejects the output")
            .assertion("reject", |_output| Err("nope".into()))
            .run(|| async { Ok(CaseOutput::text("anything")) })
            .build(),
    );
    let report = run_suites(&[suite], &RunOptions::offline()).await;
    assert_eq!(report.totals.failed, 1);
    assert!(!report.totals.is_success());
    assert_eq!(report.cases().next().unwrap().status, CaseStatus::Failed);
    assert_eq!(report.cases().next().unwrap().rationale, "nope");
}

#[tokio::test]
async fn skips_never_run_the_runner() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let suite = EvalSuite::new("self-check").with_case(
        Case::builder("skipped")
            .description("skip reason must prevent execution")
            .skip_reason(Some("needs a credential".into()))
            .judge(Arc::new(AcceptAllJudge))
            .run(move || {
                let counter = Arc::clone(&counter);
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(CaseOutput::text("ran"))
                }
            })
            .build(),
    );
    let report = run_suites(&[suite], &RunOptions::offline()).await;
    assert_eq!(report.totals.skipped, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let case = report.cases().next().unwrap();
    assert_eq!(case.status, CaseStatus::Skipped);
    assert_eq!(case.rationale, "needs a credential");
}

#[tokio::test]
async fn thresholds_gate_the_score() {
    struct HalfJudge;
    impl pi_evals::Judge for HalfJudge {
        fn name(&self) -> &str {
            "half"
        }
        fn judge(&self, _output: &CaseOutput) -> Judgment {
            Judgment {
                score: 0.5,
                rationale: "half credit".into(),
            }
        }
    }

    let build = |id: &str, threshold: f64| {
        Case::builder(id)
            .description("score is compared against the case threshold")
            .threshold(threshold)
            .judge(Arc::new(HalfJudge))
            .run(|| async { Ok(CaseOutput::text("ok")) })
            .build()
    };
    let suite = EvalSuite::new("self-check")
        .with_case(build("below", 0.75))
        .with_case(build("at", 0.5));
    let report = run_suites(&[suite], &RunOptions::offline()).await;
    let statuses: Vec<(&str, CaseStatus)> = report
        .cases()
        .map(|case| (case.id.as_str(), case.status))
        .collect();
    assert_eq!(
        statuses,
        vec![("below", CaseStatus::Failed), ("at", CaseStatus::Passed),]
    );
    assert_eq!(report.totals.failed, 1);
}

#[test]
fn fixture_server_records_and_responds() {
    let server = FixtureServer::start(|request| {
        FixtureResponse::json(json!({ "ok": request.method == "POST" }))
    })
    .expect("bind fixture server");
    let address = server.origin().trim_start_matches("http://").to_string();
    let body = br#"{"hello":"world"}"#;
    let head = format!(
        "POST /v1/chat HTTP/1.1\r\nhost: {address}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let mut stream = std::net::TcpStream::connect(&address).expect("connect fixture server");
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();

    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "response: {response}"
    );
    assert!(response.contains(r#""ok":true"#), "response: {response}");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/v1/chat");
    assert_eq!(requests[0].header("content-type"), Some("application/json"));
    assert_eq!(requests[0].json().unwrap()["hello"], "world");
}

/// Live network eval. Ignored by default; run with
/// `PI_EVAL_LIVE=1 OPENAI_API_KEY=... cargo test -p pi-evals -- --ignored`.
#[tokio::test]
#[ignore = "requires PI_EVAL_LIVE=1 and OPENAI_API_KEY"]
async fn live_provider_eval() {
    std::env::set_var("PI_EVAL_LIVE", "1");
    assert!(
        std::env::var("OPENAI_API_KEY")
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false),
        "set OPENAI_API_KEY before running the live eval"
    );
    let options = RunOptions {
        suite_filter: Some("providers".into()),
        case_filter: Some("providers-live-openai".into()),
        ..RunOptions::default()
    };
    let report = run_suites(&pi_evals::all_suites(), &options).await;
    println!("{}", report.to_text());
    assert_eq!(report.totals.total, 1);
    assert!(
        report.totals.is_success(),
        "live eval failed: {:?}",
        report.failed_cases()
    );
}
