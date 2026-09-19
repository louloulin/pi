//! Offline-first evaluation harness.
//!
//! The TypeScript upstream (`packages/evals`) treats an eval as a
//! `vitest-evals` suite whose harness drives a real `AgentSession` against a
//! model, with a judge scoring the result. This crate keeps the same shape —
//! **case → run → judge → summary → artifact** — but makes the default path
//! *offline*: cases drive the real Rust runtime against loopback fixtures or
//! in-process scripted providers, so `cargo test -p pi-evals` is green with
//! no API key and no network.
//!
//! The harness itself is deliberately model-agnostic: a [`Case`] owns a
//! runner closure and a [`Judge`]. Suites (see [`crate::suites`]) supply the
//! concrete runners. Real-API cases are declared with
//! [`CaseBuilder::skip_unless`], so they are recorded as `skipped` unless the
//! operator opts in (see `crates/pi-evals/README.md`).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Boxed future produced by a [`Case`] runner.
pub type CaseFuture = Pin<Box<dyn Future<Output = Result<CaseOutput, EvalError>> + Send + 'static>>;

/// Errors a case runner or the artifact writer can raise.
#[derive(Debug, thiserror::Error)]
pub enum EvalError {
    /// The case itself failed to run (fixture missing, agent error, …).
    #[error("eval case failed: {0}")]
    Case(String),
    /// Filesystem failure while writing artifacts.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialisation failure.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Token / latency telemetry for one case run.
///
/// Mirrors the `usage` object `vitest-evals` records: provider + model
/// identity, input / output / total tokens, tool-call count and the cache /
/// cost fields the upstream harness attaches as metadata. `provider` and
/// `model` are `None` for cases that do not stream through a model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Provider id (`faux`, `openai`, `deepseek`, …) when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model id used for the run, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Input (prompt) tokens.
    #[serde(default)]
    pub input: u32,
    /// Output (completion) tokens.
    #[serde(default)]
    pub output: u32,
    /// Total tokens, using the provider's total when it reports one.
    #[serde(default)]
    pub total: u32,
    /// Number of tool calls executed during the run.
    #[serde(default)]
    pub tool_calls: u32,
    /// Cached input tokens.
    #[serde(default)]
    pub cache_read: u32,
    /// Cache write tokens.
    #[serde(default)]
    pub cache_write: u32,
    /// Estimated cost in USD when the model has pricing metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
}

impl TokenUsage {
    /// Accumulate `other` into `self` (used to build suite / report totals).
    pub fn add(&mut self, other: &TokenUsage) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.total = self.total.saturating_add(other.total);
        self.tool_calls = self.tool_calls.saturating_add(other.tool_calls);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        if let (Some(lhs), Some(rhs)) = (self.estimated_cost_usd, other.estimated_cost_usd) {
            self.estimated_cost_usd = Some(lhs + rhs);
        }
    }
}

/// One entry of the normalized transcript a case can expose to its judge.
///
/// This is the Rust counterpart of the `vitest-evals` transcript events the
/// upstream `pi-harness.ts` builds from an `AgentSession`: text messages,
/// tool calls and tool results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranscriptEvent {
    /// A user / assistant text message.
    Message {
        /// `user` or `assistant`.
        role: String,
        /// Concatenated text content.
        content: String,
    },
    /// A tool call emitted by the model.
    ToolCall {
        /// Provider-issued call id.
        id: String,
        /// Registered tool name.
        name: String,
        /// Arguments object.
        arguments: Value,
    },
    /// A tool result returned to the model.
    ToolResult {
        /// Id of the originating call.
        tool_call_id: String,
        /// Tool name, when the runtime carries it.
        name: String,
        /// Result content.
        content: Value,
        /// Error message when the tool failed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// What a case runner produces: a domain output plus telemetry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CaseOutput {
    /// Domain result the judge scores. Kept as JSON so suites can expose any
    /// shape without the harness knowing about it.
    pub output: Value,
    /// Normalized transcript events (empty for non-agent cases).
    #[serde(default)]
    pub events: Vec<TranscriptEvent>,
    /// Token / latency telemetry.
    #[serde(default)]
    pub usage: TokenUsage,
    /// Named artifacts the case wants recorded (extension source, session
    /// JSONL, …). Values are JSON so they round-trip through the report.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifacts: BTreeMap<String, Value>,
}

impl CaseOutput {
    /// Build an output whose domain result is a single string.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            output: Value::String(text.into()),
            ..Self::default()
        }
    }

    /// Override the telemetry attached to this output.
    pub fn with_usage(mut self, usage: TokenUsage) -> Self {
        self.usage = usage;
        self
    }

    /// Append a transcript event.
    pub fn with_event(mut self, event: TranscriptEvent) -> Self {
        self.events.push(event);
        self
    }

    /// Append several transcript events.
    pub fn with_events(mut self, events: impl IntoIterator<Item = TranscriptEvent>) -> Self {
        self.events.extend(events);
        self
    }

    /// Record a named artifact.
    pub fn with_artifact(mut self, name: impl Into<String>, value: Value) -> Self {
        self.artifacts.insert(name.into(), value);
        self
    }

    /// Convenience accessor for a string domain result.
    pub fn as_str(&self) -> Option<&str> {
        self.output.as_str()
    }
}

/// A judge's verdict for one case run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Judgment {
    /// Score in `[0.0, 1.0]`; `1.0` means fully correct.
    pub score: f64,
    /// Human-readable explanation recorded in the report.
    pub rationale: String,
}

impl Judgment {
    /// A passing verdict with the given rationale.
    pub fn pass(rationale: impl Into<String>) -> Self {
        Self {
            score: 1.0,
            rationale: rationale.into(),
        }
    }

    /// A failing verdict with the given rationale.
    pub fn fail(rationale: impl Into<String>) -> Self {
        Self {
            score: 0.0,
            rationale: rationale.into(),
        }
    }
}

/// Scores a [`CaseOutput`].
pub trait Judge: Send + Sync {
    /// Stable judge identity, recorded in the report.
    fn name(&self) -> &str;
    /// Score one case output.
    fn judge(&self, output: &CaseOutput) -> Judgment;
}

/// Assertion closure used by [`AssertionJudge`]: `Ok(())` for a full score,
/// `Err(rationale)` for a failure.
pub type AssertionFn = dyn Fn(&CaseOutput) -> Result<(), String> + Send + Sync;

/// A [`Judge`] backed by an assertion closure.
///
/// The closure returns `Ok(())` for a full score or `Err(rationale)` for a
/// failure. Unconditional cases use this directly; comparative cases can
/// wrap richer logic.
pub struct AssertionJudge {
    name: String,
    check: Box<AssertionFn>,
}

impl AssertionJudge {
    /// Build an assertion judge.
    pub fn new(
        name: impl Into<String>,
        check: impl Fn(&CaseOutput) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            check: Box::new(check),
        }
    }
}

impl Judge for AssertionJudge {
    fn name(&self) -> &str {
        &self.name
    }

    fn judge(&self, output: &CaseOutput) -> Judgment {
        match (self.check)(output) {
            Ok(()) => Judgment::pass(format!("{} satisfied", self.name)),
            Err(reason) => Judgment::fail(reason),
        }
    }
}

/// A judge that always passes. Used by cases whose runner already enforces
/// its own invariants.
pub struct AcceptAllJudge;

impl Judge for AcceptAllJudge {
    fn name(&self) -> &str {
        "accept-all"
    }

    fn judge(&self, _output: &CaseOutput) -> Judgment {
        Judgment::pass("case completed")
    }
}

type CaseRunner = Box<dyn Fn() -> CaseFuture + Send + Sync>;

/// A single evaluation case.
pub struct Case {
    id: String,
    suite: String,
    description: String,
    judge: Arc<dyn Judge>,
    threshold: f64,
    skip_reason: Option<String>,
    run: CaseRunner,
}

impl Case {
    /// Start building a case with the given stable id.
    pub fn builder(id: impl Into<String>) -> CaseBuilder {
        CaseBuilder::new(id)
    }

    /// Stable case id (unique within a suite).
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Suite this case was registered under.
    pub fn suite(&self) -> &str {
        &self.suite
    }

    /// One-line description of what the case checks.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Reason this case will be skipped, if any.
    pub fn skip_reason(&self) -> Option<&str> {
        self.skip_reason.as_deref()
    }
}

/// Builder for [`Case`].
pub struct CaseBuilder {
    id: String,
    suite: String,
    description: String,
    judge: Option<Arc<dyn Judge>>,
    threshold: f64,
    skip_reason: Option<String>,
    run: Option<CaseRunner>,
}

impl CaseBuilder {
    /// Create a builder for `id`.
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            suite: String::new(),
            description: String::new(),
            judge: None,
            threshold: 1.0,
            skip_reason: None,
            run: None,
        }
    }

    /// Set the suite name (normally filled in by
    /// [`EvalSuite::with_case`]).
    pub fn suite(mut self, suite: impl Into<String>) -> Self {
        self.suite = suite.into();
        self
    }

    /// Set the human-readable description.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Attach a judge.
    pub fn judge(mut self, judge: Arc<dyn Judge>) -> Self {
        self.judge = Some(judge);
        self
    }

    /// Attach an assertion-closure judge.
    pub fn assertion(
        mut self,
        name: impl Into<String>,
        check: impl Fn(&CaseOutput) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        self.judge = Some(Arc::new(AssertionJudge::new(name, check)));
        self
    }

    /// Minimum score for a pass. Defaults to `1.0`.
    pub fn threshold(mut self, threshold: f64) -> Self {
        self.threshold = threshold;
        self
    }

    /// Skip the case while `reason` is `Some`.
    ///
    /// Suites use this for network-backed cases, e.g.
    /// `builder.skip_unless_env("PI_EVAL_LIVE", ...)`.
    pub fn skip_when(mut self, reason: Option<String>) -> Self {
        self.skip_reason = reason;
        self
    }

    /// Skip the case unless `PI_EVAL_<name>` is set; the skip reason names
    /// the env var so the report explains how to opt in.
    pub fn skip_unless_env(self, env_var: &str, what: &str) -> Self {
        if crate::support::env_flag(env_var) {
            self
        } else {
            self.skip_reason(Some(format!("set {env_var}=1 to run {what}")))
        }
    }

    /// Set the skip reason directly.
    pub fn skip_reason(mut self, reason: Option<String>) -> Self {
        self.skip_reason = reason;
        self
    }

    /// Set the runner closure.
    pub fn run<F, Fut>(mut self, runner: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<CaseOutput, EvalError>> + Send + 'static,
    {
        self.run = Some(Box::new(move || Box::pin(runner())));
        self
    }

    /// Finish the builder. Panics when no runner was supplied — that is a
    /// programming error in the suite, not a runtime condition.
    pub fn build(self) -> Case {
        let run = self
            .run
            .expect("eval case must declare a runner via CaseBuilder::run");
        Case {
            id: self.id,
            suite: self.suite,
            description: self.description,
            judge: self.judge.unwrap_or_else(|| Arc::new(AcceptAllJudge)),
            threshold: self.threshold,
            skip_reason: self.skip_reason,
            run,
        }
    }
}

/// A named group of cases.
pub struct EvalSuite {
    name: String,
    cases: Vec<Case>,
}

impl EvalSuite {
    /// Create an empty suite.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            cases: Vec::new(),
        }
    }

    /// Append a case, stamping it with this suite's name.
    pub fn with_case(mut self, case: Case) -> Self {
        let mut case = case;
        case.suite = self.name.clone();
        self.cases.push(case);
        self
    }

    /// Suite name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Cases in registration order.
    pub fn cases(&self) -> &[Case] {
        &self.cases
    }

    /// Number of cases.
    pub fn len(&self) -> usize {
        self.cases.len()
    }

    /// Whether the suite has no cases.
    pub fn is_empty(&self) -> bool {
        self.cases.is_empty()
    }
}

/// Outcome of one case run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseStatus {
    /// Judge score met the threshold.
    Passed,
    /// Runner failed or judge score was below the threshold.
    Failed,
    /// Case was skipped (e.g. missing API key).
    Skipped,
}

/// Result of running one case (one repetition).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseResult {
    /// Suite the case belongs to.
    pub suite: String,
    /// Case id.
    pub id: String,
    /// Case description.
    #[serde(default)]
    pub description: String,
    /// Zero-based repetition index.
    pub repetition: u32,
    /// Pass / fail / skip.
    pub status: CaseStatus,
    /// Judge score.
    pub score: f64,
    /// Threshold the score was compared against.
    pub threshold: f64,
    /// Judge rationale (or the skip reason).
    pub rationale: String,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// Token telemetry for the run.
    #[serde(default)]
    pub usage: TokenUsage,
    /// Runner error, when the case failed before judging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Artifacts the runner exposed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifacts: BTreeMap<String, Value>,
}

impl CaseResult {
    /// Whether this run passed.
    pub fn passed(&self) -> bool {
        self.status == CaseStatus::Passed
    }
}

/// Per-suite results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuiteReport {
    /// Suite name.
    pub name: String,
    /// Case runs, in execution order.
    pub cases: Vec<CaseResult>,
}

/// Aggregate counts and telemetry for a whole invocation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReportTotals {
    /// Number of case runs (including repetitions and skips).
    pub total: usize,
    /// Runs that passed.
    pub passed: usize,
    /// Runs that failed.
    pub failed: usize,
    /// Runs that were skipped.
    pub skipped: usize,
    /// Wall-clock duration of the invocation in milliseconds.
    pub duration_ms: u64,
    /// Summed token telemetry across runs.
    pub usage: TokenUsage,
}

impl ReportTotals {
    /// True when nothing failed.
    pub fn is_success(&self) -> bool {
        self.failed == 0
    }
}

/// Full evaluation report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalReport {
    /// RFC 3339 UTC timestamp the report was generated.
    pub generated_at: String,
    /// Per-suite results (suites filtered to zero cases are omitted).
    pub suites: Vec<SuiteReport>,
    /// Aggregate counts / telemetry.
    pub totals: ReportTotals,
}

impl EvalReport {
    /// Iterate every case run in the report.
    pub fn cases(&self) -> impl Iterator<Item = &CaseResult> {
        self.suites.iter().flat_map(|suite| suite.cases.iter())
    }

    /// Runs that failed.
    pub fn failed_cases(&self) -> Vec<&CaseResult> {
        self.cases().filter(|case| !case.passed()).collect()
    }

    /// Pretty-printed JSON report (mirrors the upstream `report.json`).
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Human-readable text report (mirrors the upstream `report.txt`).
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "pi-evals report ({})", self.generated_at);
        for suite in &self.suites {
            let _ = writeln!(out);
            let _ = writeln!(out, "{}", suite.name);
            for case in &suite.cases {
                let status = match case.status {
                    CaseStatus::Passed => "PASS",
                    CaseStatus::Failed => "FAIL",
                    CaseStatus::Skipped => "SKIP",
                };
                let _ = writeln!(
                    out,
                    "  {status}  {}  ({}ms)  {}",
                    case.id, case.duration_ms, case.rationale
                );
            }
        }
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Totals: {} passed, {} failed, {} skipped in {}ms",
            self.totals.passed, self.totals.failed, self.totals.skipped, self.totals.duration_ms
        );
        let usage = &self.totals.usage;
        let _ = writeln!(
            out,
            "Tokens: input {}, output {}, total {}, tool calls {}",
            usage.input, usage.output, usage.total, usage.tool_calls
        );
        out
    }

    /// Write `report.json`, `report.txt` and `runs.jsonl` into `dir`.
    pub fn write(&self, dir: &Path) -> Result<(), EvalError> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join("report.json"), self.to_json()?)?;
        std::fs::write(dir.join("report.txt"), self.to_text())?;
        let mut runs = String::new();
        for case in self.cases() {
            runs.push_str(&serde_json::to_string(case)?);
            runs.push('\n');
        }
        std::fs::write(dir.join("runs.jsonl"), runs)?;
        Ok(())
    }
}

/// Invocation options.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Directory to write artifacts into. `None` runs without touching disk.
    pub artifacts_dir: Option<PathBuf>,
    /// Substring filter on suite names.
    pub suite_filter: Option<String>,
    /// Substring filter on case ids.
    pub case_filter: Option<String>,
    /// Number of repetitions per case. Defaults to 1.
    pub repetitions: u32,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            artifacts_dir: None,
            suite_filter: None,
            case_filter: None,
            repetitions: 1,
        }
    }
}

impl RunOptions {
    /// Options that run every case once without writing artifacts.
    pub fn offline() -> Self {
        Self::default()
    }
}

/// Run one case once.
async fn run_case(case: &Case, repetition: u32) -> CaseResult {
    let base = |status: CaseStatus, rationale: String, duration_ms: u64| CaseResult {
        suite: case.suite.clone(),
        id: case.id.clone(),
        description: case.description.clone(),
        repetition,
        status,
        score: if status == CaseStatus::Passed { 1.0 } else { 0.0 },
        threshold: case.threshold,
        rationale,
        duration_ms,
        usage: TokenUsage::default(),
        error: None,
        artifacts: BTreeMap::new(),
    };

    if let Some(reason) = &case.skip_reason {
        return base(CaseStatus::Skipped, reason.clone(), 0);
    }

    let started = Instant::now();
    match (case.run)().await {
        Ok(output) => {
            let judgment = case.judge.judge(&output);
            let duration_ms = started.elapsed().as_millis() as u64;
            let mut result = base(
                if judgment.score >= case.threshold {
                    CaseStatus::Passed
                } else {
                    CaseStatus::Failed
                },
                judgment.rationale,
                duration_ms,
            );
            result.score = judgment.score;
            result.usage = output.usage;
            result.artifacts = output.artifacts;
            result
        }
        Err(error) => {
            let duration_ms = started.elapsed().as_millis() as u64;
            let mut result = base(CaseStatus::Failed, error.to_string(), duration_ms);
            result.error = Some(error.to_string());
            result
        }
    }
}

/// Run every selected case in `suites`.
///
/// Cases are executed sequentially so a fixture server can serve exactly one
/// request per case without cross-talk; this matches the upstream runner,
/// which also serialises harness runs.
pub async fn run_suites(suites: &[EvalSuite], options: &RunOptions) -> EvalReport {
    let started = Instant::now();
    let repetitions = options.repetitions.max(1);
    let mut suite_reports = Vec::new();
    let mut totals = ReportTotals::default();

    for suite in suites {
        if let Some(filter) = &options.suite_filter {
            if !suite.name.contains(filter) {
                continue;
            }
        }
        let mut cases = Vec::new();
        for case in &suite.cases {
            if let Some(filter) = &options.case_filter {
                if !case.id.contains(filter) {
                    continue;
                }
            }
            for repetition in 0..repetitions {
                let result = run_case(case, repetition).await;
                match result.status {
                    CaseStatus::Passed => totals.passed += 1,
                    CaseStatus::Failed => totals.failed += 1,
                    CaseStatus::Skipped => totals.skipped += 1,
                }
                totals.total += 1;
                totals.usage.add(&result.usage);
                cases.push(result);
            }
        }
        if !cases.is_empty() {
            suite_reports.push(SuiteReport {
                name: suite.name.clone(),
                cases,
            });
        }
    }

    totals.duration_ms = started.elapsed().as_millis() as u64;
    EvalReport {
        generated_at: chrono::Utc::now().to_rfc3339(),
        suites: suite_reports,
        totals,
    }
}

/// Wall-clock duration helper used by suites that want to record timings in
/// their domain output.
pub fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis() as u64
}
