# `pi-evals` — offline-first eval harness

`pi-evals` is the Rust analogue of
[`packages/evals`](../../../packages/evals) in the TS monorepo. It provides a
small harness (cases, suites, judgements, reports, artifacts) plus a set of
suites that measure the Rust port as it lands: provider request/response
shapes, the model catalog, the JS extension host, and documentation integrity.

The defining constraint is **offline-first**: the default `cargo test -p
pi-evals` path needs no API key and no network. Cases that talk to real
providers are declared like any other case but carry a skip reason, so they are
reported as `skipped` instead of silently disappearing. Set `PI_EVAL_LIVE=1`
(plus a credential) to run them for real.

## Quick start

```bash
cd pi-rust

# Everything offline: faux provider + loopback fixture server + files on disk.
cargo test -p pi-evals

# Same suites through the CLI runner, with a human-readable report.
cargo run -p pi-evals --example run_evals -- --suite smoke --case smoke-faux-answer

# Machine-readable report + artifacts (report.json / report.txt / runs.jsonl).
cargo run -p pi-evals --example run_evals -- --json --out .eval

# Live network probe (opt in explicitly).
PI_EVAL_LIVE=1 OPENAI_API_KEY=sk-... \
  cargo run -p pi-evals --example run_evals -- --case providers-live-openai
```

`run_evals` exits non-zero when any case fails, so it can gate CI.

## Layout

| Path | Contents |
|---|---|
| `src/harness.rs` | `Case`, `CaseBuilder`, `EvalSuite`, `CaseOutput`, `Judge` / `AssertionJudge` / `AcceptAllJudge`, `CaseStatus`, `SuiteReport`, `EvalReport`, `RunOptions`, `run_case`, `run_suites` |
| `src/fixture.rs` | Loopback HTTP fixture server (`FixtureServer`, `FixtureResponse`, `RecordedRequest`) and SSE builders (`sse_text`, `sse_tool_call`, `sse_from_chunks`) |
| `src/support.rs` | Shared helpers: env flags, faux/`Api` model builders, agent runs, transcript/usage extraction |
| `src/suites/*.rs` | The suites: `smoke`, `models`, `providers`, `extensions`, `docs` |
| `examples/run_evals.rs` | CLI runner (`--suite`, `--case`, `--repetitions`, `--live`, `--out`, `--json`) |
| `tests/evals.rs` | Harness self-tests, offline green gate, artifact round-trip, `#[ignore]` live test |

## Suites and cases

`cargo test -p pi-evals` runs 16 cases: 15 offline, 1 network case recorded as
skipped.

| Suite | Case | What it asserts |
|---|---|---|
| `smoke` | `smoke-faux-answer` | The faux provider answers the probe prompt and the transcript is non-empty. |
| `smoke` | `smoke-openai-fixture-answer` | `OpenAiProvider` hits `/v1/chat/completions`, parses streamed SSE, and reports the scripted token usage. |
| `smoke` | `smoke-openai-fixture-tool-turn` | The agent executes a fixture-issued tool call, sends the tool result back, and answers with it (2 turns, 22 tokens). |
| `models` | `models-registry-invariants` | Built-in provider specs have unique ids, non-empty defaults, and a credential env var whenever they need transport; `faux` never requires a key. |
| `models` | `models-add-model-to-existing-provider` | A catalog envelope for an existing provider resolves every entry it declares (no silent dropping of the pre-existing model). |
| `models` | `models-api-inference` | Catalog entries infer their API family from the provider id and the `api` hint. |
| `providers` | `providers-openai-compatible-request` | An OpenAI-compatible provider sends the expected path, method, auth header, model, and `stream:true`, and parses the answer. |
| `providers` | `providers-router-dispatch` | `ProviderRouter` resolves env-configured providers, dispatches to them, and reports `MissingApiKey` with the exact env vars for unconfigured ones. |
| `providers` | `providers-model-metadata-divergence` | Records which upstream `Model` fields the Rust `Model` does not carry. |
| `providers` | `providers-live-openai` | Live Chat Completions probe. **Skipped** unless `PI_EVAL_LIVE=1` and `OPENAI_API_KEY` are set. |
| `extensions` | `extensions-load-registers-tool` | A JS extension loads through `JsExtensionHost` and registers its tool with the expected name. |
| `extensions` | `extensions-execute-hello-tool` | `execute_tool("hello", {"name":"Bob"})` returns `Hello, Bob!` with structured details. |
| `extensions` | `extensions-agent-hello-round-trip` | A model-issued `hello` call is executed by `JsExtensionHost` and the extension's greeting reaches the final answer. |
| `docs` | `docs-relative-links-resolve` | Every relative Markdown link under `pi-rust/docs/**`, `pi-rust/README.md`, and the repository `README.md` resolves to an existing path. |
| `docs` | `docs-code-fences-balanced` | Every fenced code block in those pages is closed. |
| `docs` | `docs-readme-crate-map` | Every crate the `pi-rust/README.md` crate map claims ships a `crates/<name>/Cargo.toml`. |

## Upstream mapping

Upstream files and their Rust counterparts:

| Upstream (`packages/evals`) | Rust |
|---|---|
| `src/pi-harness.ts` | `src/support.rs` (`run_agent`, `AgentRun`, usage/transcript extraction) plus `src/harness.rs` (case/suite/report model) |
| `src/smoke.eval.ts` — "Answer a basic prompt" | `suites::smoke` — `smoke-faux-answer`, `smoke-openai-fixture-answer` |
| `src/models.eval.ts` — "Add model to existing provider" | `suites::models` — all three cases |
| `src/providers.eval.ts` — "Add OpenAI-compatible provider" | `suites::providers` — `providers-openai-compatible-request`, `providers-router-dispatch`, `providers-model-metadata-divergence` |
| `src/providers.eval.ts` — "Add custom streaming provider" | No direct analogue; the custom-adapter path is not exercised (see divergences) |
| `src/providers.eval.ts` — live probes | `suites::providers` — `providers-live-openai` (skipped unless opted in) |
| `src/extensions.eval.ts` — "Create and use a tool extension" | `suites::extensions` — all three cases |
| `src/docs.eval.ts` — "Audit documentation against implementation" | `suites::docs` — deterministic link/fence/crate-map audits (model-backed prose audit has no offline analogue) |
| `src/vitest-evals/harness-table.ts` | `SuiteReport` / `EvalReport::to_text` (per-suite case table) |
| `src/vitest-evals/summary.ts` | `ReportTotals` plus the `Totals:` / `Tokens:` trailer |
| `src/vitest-evals/reporter.ts`, `setup.ts` | `examples/run_evals.rs` plus `tests/evals.rs` |
| `src/vitest-evals/artifacts.ts` | `EvalReport::write` (`report.json`, `report.txt`, `runs.jsonl`) and `CaseOutput::artifacts` |
| `scripts/run-evals.mjs` | `cargo run -p pi-evals --example run_evals` |

## Harness model

```rust,no_run
use pi_evals::{Case, CaseOutput, EvalSuite, RunOptions, run_suites};

let suite = EvalSuite::new("demo").with_case(
    Case::builder("demo-greeting")
        .description("the model greets the user")
        .assertion("greeting", |output| {
            if output.as_str() == Some("hello") { Ok(()) } else { Err("expected `hello`".into()) }
        })
        .run(|| async { Ok(CaseOutput::text("hello")) })
        .build(),
);

let report = run_suites(&[suite], &RunOptions::offline()).await;
assert!(report.totals.is_success());
```

- A **case** owns a runner future and a judge. `assertion(name, f)` builds a
  pass/fail `AssertionJudge`; `judge(Arc<dyn Judge>)` accepts a custom scoring
  judge and `threshold(x)` fails any score below `x`.
- `skip_reason(Some(..))` and `skip_unless_env(flag, reason)` record a case as
  `skipped` and never call the runner.
- `RunOptions` carries `artifacts_dir`, `suite_filter`, `case_filter`, and
  `repetitions`; `RunOptions::offline()` is the no-artifact, no-filter run.
- `EvalReport::write(dir)` writes `report.json`, `report.txt`, and
  `runs.jsonl` (one JSON object per case run). Case-level `artifacts` are
  embedded in the JSON, so a failing case can carry the request body, the
  transcript, or a token count without a bespoke log.

## Live evals

`providers-live-openai` is the only networked case. It is skipped unless both
`PI_EVAL_LIVE=1` and `OPENAI_API_KEY` are present.

| Variable | Meaning | Default |
|---|---|---|
| `PI_EVAL_LIVE` | `1` (or `true`/`yes`/`on`) enables network cases | unset |
| `OPENAI_API_KEY` | Credential for the probe | required |
| `PI_EVAL_MODEL` | Model id to probe | `gpt-4o-mini` |
| `OPENAI_BASE_URL` / `PI_EVAL_BASE_URL` | Base URL override (e.g. a gateway or Azure-style endpoint) | `https://api.openai.com/v1` |

Run it through the runner:

```bash
PI_EVAL_LIVE=1 OPENAI_API_KEY=... cargo run -p pi-evals --example run_evals -- --case providers-live-openai
```

or through the ignored test:

```bash
PI_EVAL_LIVE=1 OPENAI_API_KEY=... cargo test -p pi-evals -- --ignored
```

`tests/evals.rs` keeps every async test on the default current-thread tokio
runtime on purpose: the extension cases drive QuickJS through
`pi_extensions::JsExtensionHost`, and a multi-thread flavour would poll its
`tokio::spawn`ed runtime driver on a different thread than the one evaluating
JS.

## Documented divergences

These are intentional differences from `packages/evals`, each visible as a
case or a note rather than a silent omission:

1. **No vitest.** The harness is a plain Rust library with an example runner;
   reports are JSON/text/JSONL instead of vitest attachments.
2. **No new third-party crates.** The fixture server is a
   `std::net::TcpListener` on an ephemeral port instead of `wiremock`, and the
   harness reuses dependencies the workspace already ships (`tokio`,
   `tokio-util`, `serde_json`, `chrono`, `thiserror`, `async-trait`).
3. **Faux + fixtures instead of scripted live models.** Upstream drives real
   providers for most cases; the Rust port asserts the same observable
   behaviour against the `faux` provider and a loopback SSE server.
4. **`Model` metadata gap.** The Rust `Model` carries
   `provider`/`id`/`api`/`label`/`context_window`/`max_output_tokens`; upstream
   also carries `name`, `reasoning`, `input`, `cost.*`, and `maxTokens`.
   `providers-model-metadata-divergence` records the difference instead of
   asserting parity.
5. **Catalog envelopes replace, they do not merge.**
   `Models::register_provider_json` replaces a provider's model list with the
   envelope's entries, so `models-add-model-to-existing-provider` requires the
   envelope to spell out every model that must survive. Upstream merges new
   entries into the existing catalog.
6. **`Api` wire spelling.** The serialized `Api` value for OpenAI chat
   completions is `open_ai_chat_completions` (`rename_all = "snake_case"`),
   while the accepted input hints are kebab-case (`openai-chat-completions`).
7. **No custom streaming-provider adapter.** Upstream's "Add custom streaming
   provider" eval registers an arbitrary NDJSON streaming endpoint; the Rust
   provider router only builds adapters for the four first-party API families,
   so the case has no offline analogue.
8. **No model-backed documentation audit.** Upstream reads each documentation
   page with an agent and submits a structured verdict. Offline Rust checks
   only the mechanically decidable claims: relative links resolve, fences are
   balanced, and the README crate map matches `crates/*/Cargo.toml`. Crate
   READMEs are not part of the audited page set.
9. **Extension audits measure the host, not authoring.** Upstream judges an
   agent-authored extension for canonical imports; the Rust suite verifies that
   a canonical-shape extension loads, registers, and executes through the
   QuickJS host.

See [`../../docs/FEATURE_PI_RS_STATUS.md`](../../docs/FEATURE_PI_RS_STATUS.md)
for the stage-by-stage status this crate feeds into.
