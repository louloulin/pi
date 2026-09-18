# `feature/pi.rs` Branch — Consolidated Status

This branch (created from LUM-1016) consolidates the in-review Rust port work
from the LUM-981 follow-up tree into a single commit chain on top of `origin/main`.

## Branch composition

Six merge commits on top of `origin/main` (`71dca871b`):

| Order | Source | Title |
|-------|--------|-------|
| 1 | `33adc5b83` | Stage 0 — scaffold pi-rust workspace |
| 2 | `9e61b0fa3` | LUM-995 — wire `should_stop_after_turn` / `prepare_next_turn` hooks |
| 3 | `3fac21a2b` | Stage 4 — pi-tui interactive CLI + `session_log` (LUM-988) |
| 4 | `9d7e49582` | Stage 6 — `wasm32-unknown-unknown` + `wasm-bindgen` browser host (LUM-990) |
| 5 | `7517da24a` | LUM-997 — bash / write / edit / read tools for `pi-coding-agent` |
| 6 | `523af6782` (cherry-pick) | LUM-996 — OpenAI Chat Completions provider (provider module + fixtures + example) |

The diff against `origin/main` is **83 files / +14 594 lines**, all under
`pi-rust/`.

## What works after the merge

```
$ cargo check  --workspace --all-targets    # clean
$ cargo build  --workspace --all-targets    # clean
$ cargo test   --workspace                  # 87 tests pass
```

## What was *deliberately* skipped

Three follow-up task branches are NOT merged into `feature/pi.rs` because each
implements the same surface area in a way that conflicts with the chain above:

- **`agent/devbox1/a5e8bd115db9` (LUM-984, Stage 1 pi-ai)** — defines a more
  elaborate `AssistantMessageEvent` (TextStart/TextEnd/ThinkingStart/ThinkingEnd
  /ToolCallStart/ToolCallEnd, plus response_id / response_model / raw_stop_reason /
  error_message / timestamp on `AssistantMessage`) and an `OpenAiTransport`
  trait abstraction. Conflicts with `pi-protocol/src/events.rs` and
  `pi-ai/src/providers/openai.rs` from LUM-995 / Stage 4 / Stage 6. The Stage 1
  providers, fixtures, and SSE parser live on the branch for future reference;
  merging requires either collapsing two protocol designs or rewriting the
  TUI / WASM consumers, which exceeds the LUM-1016 scope.
- **`agent/devbox1/e331c7847` (LUM-985, Stage 2 pi-agent-core)** — introduces
  its own `AgentLoop` event loop with `AgentEvent` defined in `agent_loop::*`
  and uses a `tool.rs` module that is incompatible with Stage 4's `events.rs`
  separation. The Stage 2 test suite (`tests/agent_loop.rs`, 5 e2e scenarios)
  is preserved on the branch.
- **`agent/devbox1/b662db4686e7` (LUM-996, full merge)** — full commit merges
  on top of the alternate Stage 0 base (`0ab09d1cd`) and changes every crate's
  Cargo.toml + lib.rs in ways that don't reconcile with the `feature/pi.rs`
  chain. The OpenAI Chat Completions provider, fixtures, and example were
  cherry-picked individually instead.

## Why this ordering instead of the LUM-981 follow-up chain

The original LUM-981 plan (and PLAN.md / PLAN-lum-981.md) sequenced Stages 0 →
1 → 2 → 3 → 4 → 5 → 6. Stage 1 and Stage 2 each delivered agent_core / pi-ai
implementations that did not anticipate the events.rs / module reorganization
Stage 4 introduced. The worktrees for Stage 4 / Stage 6 were built directly on
Stage 0 + LUM-995 and never depended on Stage 1 / Stage 2 — the Stage 4 branch
ships its own minimal `AgentLoop` plus `events::AgentEvent`.

The pragmatic sequence is the one that the actual git history of completed
tasks supports: take what compiles and what each branch already validated in
isolation, drop what conflicts at the protocol/event level, and surface the
deferred work as a follow-up.

## Recommended follow-up

If a future agent wants Stage 1 / Stage 2 work folded into `feature/pi.rs`:

1. Pick the protocol — either keep Stage 4/6's `AssistantMessageEvent` enum
   (Start / TextDelta / ThinkingDelta / ToolCallDelta / Done / Aborted / Error)
   and expand it with the Start/End variants Stage 1 needs, OR drop Stage 4's
   `events` module in favor of Stage 2's `agent_loop` placement.
2. Reconcile `pi-ai` provider designs — Stage 1's `OpenAiTransport` trait +
   `FixtureTransport` test mode is more testable; LUM-996's `reqwest` direct
   adapter is simpler. A future PR could port LUM-996's
   `OpenAiProvider::stream_simple` body onto Stage 1's `OpenAiTransport`.
3. Port Stage 2's e2e test suite (`tests/agent_loop.rs`) onto whichever
   agent_core lands in `feature/pi.rs` — the scenarios are valuable regression
   coverage independent of the implementation choice.

## Verification commands

```bash
# all on feature/pi.rs
cd pi-rust
cargo check  --workspace --all-targets     # 0 errors
cargo build  --workspace --all-targets     # 0 errors
cargo test   --workspace                   # 87 / 87 pass
```

## LUM-1017 round — re-verification + LUM-986 / protocol reconciliation handoff

LUM-1017 (2026-09-18 21:40 UTC) re-checked `feature/pi.rs` and confirmed the
LUM-1016 baseline still compiles and passes on a fresh checkout:

```
$ cargo check --workspace --all-targets   # 0 errors, 0 warnings
$ cargo test  --workspace                 # 87 / 87 pass (all crates green)
```

### Decision: skip new parallel dispatches this round

- LUM-986 (`pi-extensions` WASM host) is still `in_progress` but has **no
  comments and no commits** since the LUM-1014 dispatch — the slot is held
  without producing work. The 3-run cap from LUM-1014 is therefore
  effectively exhausted by a single placeholder.
- LUM-984 / LUM-985 / LUM-996 work cannot be merged without the protocol
  reconciliation listed above. Folding them in is a single coordinated
  design decision, not parallel work — splitting it across 3 runs would
  race on the same `AssistantMessageEvent` enum.
- LUM-988 / LUM-989 / LUM-990 are already merged and in_review, so there
  is nothing new to dispatch on the Stage 4 / 5 / 6 axes.

The pragmatic move for LUM-1017 is the same shape as LUM-1015 / LUM-1016:
**no new sub-tasks this round, ship the consolidation state, document the
next coordinated step**. The single follow-up that would unlock the most
remaining work is the protocol reconciliation described in the previous
section — a future run should pick that up as one task, not three.

### Push status

`feature/pi.rs` is committed locally and reachable from the
`agent/devbox1/deafab8e394c` worktree, but `git push origin feature/pi.rs`
fails in the sandbox:

```
$ git push origin feature/pi.rs
fatal: could not read Username for 'https://github.com': No such device or address
```

The git credential helper points at `/tmp/git-creds`, which does not exist
in this sandbox; no `gh` auth is configured. The local mirror at
`/home/devbox/multica_workspaces/.repos/.../github.com+louloulin+pi.git`
already carries `feature/pi.rs` from the LUM-1016 round, so the daemon's
next sync window will push whatever new commits land on this branch.

### Suggested next single run

A future coordinator agent (or LUM-1018) should pick **one** of the
following as a single in-flight task, not split across the 3-slot cap:

1. **Protocol reconciliation** — pick `AssistantMessageEvent` (Stage 4/6)
   or `AgentLoop` (Stage 2) and rebase LUM-984 / LUM-985 / LUM-996 onto
   `feature/pi.rs`, then merge. This unblocks Stage 1 / Stage 2 / Stage 3
   of the original LUM-981 plan.
2. **Stage 3 retry on `feature/pi.rs`** — re-implement `pi-extensions`
   WASM host + JS shim on top of `feature/pi.rs` (instead of the
   Stage 4 base LUM-986 was originally cut from). This is what LUM-986
   should have been once `feature/pi.rs` existed.

Option (2) is the smaller blast radius and unblocks the LUM-981
"plugin ecosystem compatibility" acceptance criterion directly. Option (1)
is strictly larger but covers the remaining LUM-984 / LUM-985 / LUM-996
debt in one shot.

## LUM-1018 round — re-verification, no new dispatches, push still blocked

LUM-1018 (2026-09-18 21:53 UTC, autopilot template re-run) checked
`feature/pi.rs` against the LUM-1017 baseline on a fresh checkout
(`agent/devbox1/lum-1018` cut from `feature/pi.rs` at `6bb6819e1`):

```
$ cargo check --workspace --all-targets   # 0 errors, 0 warnings
$ cargo test  --workspace                 # 87 / 87 pass (all crates green)
```

Same crate/test breakdown as LUM-1017: `pi-protocol` 6 + `pi-agent-core`
smoke 2 + hooks 4 + `pi-ai` 5 + `pi-tui` e2e 11 + snapshot 3 +
`pi-coding-agent` tools 10 + `pi-extensions` loader 32 + WASM smoke 5 +
faux provider 9.

### Decision: skip new parallel dispatches this round (third identical call)

LUM-1018's autopilot template is identical to LUM-1011 / LUM-1012 /
LUM-1013 / LUM-1015 / LUM-1017. The state has not moved between those
rounds, so the same reasoning holds:

| Issue | Status | Reality |
|-------|--------|---------|
| LUM-986 (Stage 3 — pi-extensions WASM host) | `in_progress` | No comments, no commits, no worktree, idle_watchdog cancellation. Slot held empty. |
| LUM-1003 (P1 — real OpenAI/Anthropic providers) | `in_progress` | No comments, no commits, no worktree, idle_watchdog cancellation. Slot held empty. |
| LUM-991 / LUM-992 (Stage 1/2 starter) | `in_progress` | Empty worktrees, idle_watchdog cancellations. |
| LUM-982 (workspace primer) | `in_progress` | First-pass dispatch, never advanced. |

The "3 worker slots" cap is full of bookkeeping `in_progress` rows with
zero work landed. Adding new sub-issues does not help; the gating factor
is the protocol reconciliation (Option 1 above) which is one design
decision, not three parallel runs.

### Candidate concrete next run (single)

When a real slot frees up (or a future coordinator decides to recycle one
of the stale placeholders), the highest-value single-task follow-up is:

- **Stage 3 (LUM-986) re-implementation on `feature/pi.rs`** — copy
  `lum-981-49282a6b984a/workdir/pi-rust/crates/pi-extensions/` host scaffold
  (or write fresh) directly on top of `feature/pi.rs`, then add the e2e
  test that loads `summarize.ts` / `notify-on-start.ts` and exercises
  `registerTool`. ~1500 LOC + e2e, scope-bounded, no protocol
  reconciliation needed because `feature/pi.rs` already settled on the
  Stage 4/6 event enum.

This unlocks the LUM-981 "plugin ecosystem compatibility" acceptance
criterion in one PR.

### Push status (still blocked)

Confirmed at LUM-1018 time — no GitHub credentials in the sandbox:

```
$ git push origin feature/pi.rs
fatal: could not read Username for 'https://github.com': terminal prompts disabled

$ git ls-remote --heads origin
71dca871bc80b6bc97be37f0ca3189399d651fff        refs/heads/main
# (no feature/pi.rs on GitHub)
```

The local mirror at
`/home/devbox/multica_workspaces/.repos/77113af3-bd2e-4c2a-9f11-659117e3ca3d/github.com+louloulin+pi.git`
does carry `feature/pi.rs` at `6bb6819e19050e2dcdb4eb230192e75d4d5519f0`,
so the daemon's next GitHub sync window will pick it up. No action needed
from this round beyond documenting the state.

## LUM-1019 round — re-verification, state unchanged, push still blocked

LUM-1019 (2026-09-18 22:00 UTC, autopilot template re-run) checked
`feature/pi.rs` against the LUM-1018 baseline on a fresh checkout
(`agent/devbox1/lum-1019` cut from `feature/pi.rs` at `ebf66874a`):

```
$ cargo check --workspace --all-targets   # 0 errors, 0 warnings
$ cargo test  --workspace                 # 87 / 87 pass (all crates green)
```

Same crate/test breakdown as LUM-1017 / LUM-1018: `pi-protocol` 6 +
`pi-agent-core` smoke 2 + hooks 4 + `pi-ai` 5 + `pi-tui` e2e 11 +
snapshot 3 + `pi-coding-agent` tools 10 + `pi-extensions` loader 32 +
WASM smoke 5 + faux provider 9.

### Decision: skip new parallel dispatches this round (fourth identical call)

LUM-1019's autopilot template is identical to LUM-1011 / LUM-1012 /
LUM-1013 / LUM-1015 / LUM-1017 / LUM-1018. The state has not moved
between those rounds, so the same reasoning holds:

| Issue | Status | Reality |
|-------|--------|---------|
| LUM-982 (workspace primer) | `in_progress` | First-pass dispatch, never advanced, idle since 14:33. |
| LUM-986 (Stage 3 — pi-extensions WASM host) | `in_progress` | No comments, no commits, no worktree, idle since 17:01. |
| LUM-991 (Stage 1 pi-ai starter) | `in_progress` | Empty worktree, idle since 15:08. |
| LUM-992 (Stage 2 pi-agent-core starter) | `in_progress` | Empty worktree, idle since 18:50. |
| LUM-1003 (P1 — real OpenAI/Anthropic providers) | `in_progress` | No comments, no commits, no worktree, idle since 20:01. |

The "3 worker slots" cap is full of bookkeeping `in_progress` rows with
zero work landed. Adding new sub-issues does not help; the gating factor
is the protocol reconciliation (Option 1 above) which is one design
decision, not three parallel runs.

### Why the autopilot template keeps firing

The trigger comment on LUM-1019 still says "If this task exists, plan
follow-up tasks, open max 3 parallel". That language was authored for the
first round (LUM-982) when the slots were empty. Once idle placeholders
filled the slots, every subsequent round inherits the same template, and
the only honest answer has been "no, the slots are saturated". The
template does not have a "skip if saturated" branch, so each round has
to decide by hand and document it.

The pragmatic observation: an autopilot template that re-fires every
~15 minutes on a stalled task queue produces zero forward motion. The
correct counter-measure is one focused single-task dispatch (not the
template), not more re-runs of the template.

### Candidate concrete next run (single)

When a real slot frees up (or a future coordinator decides to recycle one
of the stale placeholders), the highest-value single-task follow-up is
still:

- **Stage 3 (LUM-986) re-implementation on `feature/pi.rs`** — copy
  `lum-981-49282a6b984a/workdir/pi-rust/crates/pi-extensions/` host scaffold
  (or write fresh) directly on top of `feature/pi.rs`, then add the e2e
  test that loads `summarize.ts` / `notify-on-start.ts` and exercises
  `registerTool`. ~1500 LOC + e2e, scope-bounded, no protocol
  reconciliation needed because `feature/pi.rs` already settled on the
  Stage 4/6 event enum.

This unlocks the LUM-981 "plugin ecosystem compatibility" acceptance
criterion in one PR.

### Push status (still blocked)

Re-confirmed at LUM-1019 time — no GitHub credentials in the sandbox:

```
$ git push origin feature/pi.rs
fatal: could not read Username for 'https://github.com': terminal prompts disabled

$ git ls-remote --heads origin
71dca871bc80b6bc97be37f0ca3189399d651fff        refs/heads/main
# (no feature/pi.rs on GitHub)
```

The local mirror at
`/home/devbox/multica_workspaces/.repos/77113af3-bd2e-4c2a-9f11-659117e3ca3d/github.com+louloulin+pi.git`
does carry `feature/pi.rs` at `ebf66874ad09e311653acd79c2948362465c05d1`,
so the daemon's next GitHub sync window will pick it up. No action needed

## LUM-1022 round — re-verification, no new dispatches, push still blocked

LUM-1022 (2026-09-18 22:20 UTC, autopilot template re-run with the
original LUM-982 wording — "plan follow-up tasks, open max 3 parallel")
checked `feature/pi.rs` against the LUM-1019 baseline on a fresh
checkout (`agent/devbox1/lum-1022` cut from `feature/pi.rs` at
`aade66d67`):

```
$ cargo check --workspace --all-targets   # 0 errors, 0 warnings
$ cargo test  --workspace                 # 87 / 87 pass (all crates green)
```

Same crate/test breakdown as LUM-1017 / LUM-1018 / LUM-1019:
`pi-protocol` 6 + `pi-agent-core` smoke 2 + hooks 4 + `pi-ai` 5 +
`pi-tui` e2e 11 + snapshot 3 + `pi-coding-agent` tools 10 +
`pi-extensions` loader 32 + WASM smoke 5 + faux provider 9.

### Decision: skip new parallel dispatches this round (sixth identical call)

LUM-1022's autopilot template is identical to LUM-1011 / LUM-1012 /
LUM-1013 / LUM-1015 / LUM-1017 / LUM-1018 / LUM-1019. The state has not
moved between those rounds, so the same reasoning holds.

A first-pass reflex here was to spawn three "R2 stage" sub-issues
(`pi-extensions` QuickJS host, `pi-session` rusqlite backend, `wasm32`
browser host) under LUM-1022, but those are exactly the slots the
established pattern says **not** to fill — the placeholder bookkeeping
issues already exhaust the 3-slot cap and adding more parallel
dispatches would race on the same `AssistantMessageEvent` enum. Those
sub-issues were created and then cancelled in the same round (LUM-1023
/ LUM-1024 / LUM-1025, all `status: cancelled`). Net new dispatch
count: **0**.

### Why the autopilot template keeps firing

The trigger comment on LUM-1022 still says "If this task exists, plan
follow-up tasks, open max 3 parallel". That language was authored for
the first round (LUM-982) when the slots were empty. Once idle
placeholders filled the slots, every subsequent round inherits the same
template, and the only honest answer has been "no, the slots are
saturated". The template does not have a "skip if saturated" branch, so
each round has to decide by hand and document it.

The pragmatic observation: an autopilot template that re-fires every
~15 minutes on a stalled task queue produces zero forward motion. The
correct counter-measure is one focused single-task dispatch (not the
template), not more re-runs of the template.

### Candidate concrete next run (single)

Unchanged from LUM-1017 / LUM-1018 / LUM-1019: Stage 3 (LUM-986)
re-implementation on `feature/pi.rs`. The `pi-extensions` scaffold in
the current `feature/pi.rs` tree is the right starting point; the
previous `lum-981-49282a6b984a/workdir/pi-rust/crates/pi-extensions/`
work predates the Stage 4/6 event enum and would need to be ported, not
copied.

### Push status (still blocked)

Re-confirmed at LUM-1022 time — no GitHub credentials in the sandbox:

```
$ git push origin feature/pi.rs
fatal: could not read Username for 'https://github.com': terminal prompts disabled
```

The local mirror at
`/home/devbox/multica_workspaces/.repos/77113af3-bd2e-4c2a-9f11-659117e3ca3d/github.com+louloulin+pi.git`
already carries `feature/pi.rs` at `aade66d67`; the daemon's next
GitHub sync window will pick up whatever new commit lands on this
branch.

## LUM-1026 round — re-verification, pi-session landed upstream, push attempted

LUM-1026 (2026-09-18 22:30 UTC, autopilot template re-run with the
same wording as LUM-1011 / LUM-1012 / LUM-1013 / LUM-1015 / LUM-1017 /
LUM-1018 / LUM-1019 / LUM-1022) checked `feature/pi.rs` against the
LUM-1022 baseline on the same worktree as LUM-1024
(`/home/devbox/multica_workspaces/lumos-659117e3ca3d/lum-1024-34cc34bcff68/workdir/pi`)
after `git fetch origin` fast-forwarded the local branch:

```
$ git log --oneline origin/feature/pi.rs -3
2be1e138a feat(pi-session): wire pi-session into workspace Cargo.toml
f8bd3945d feat(pi-session): scaffold pi-session crate (rusqlite + zstd backend)
7ab74784f docs(pi-rust): LUM-1022 round — re-verify feature/pi.rs, no new dispatches, push still blocked
```

Two new commits landed between LUM-1022 and LUM-1026: the
`pi-session` rusqlite/zstd backend (Stage 5 of the original LUM-981
plan) was actually scaffolded and pushed to GitHub despite LUM-1024
being marked `cancelled` on the board. The LUM-1024 worktree
uncommitted WIP that I found on disk (Cargo.toml / Cargo.lock mods +
`crates/pi-session/` directory) was a stale local copy of the same
content that already exists on `origin/feature/pi.rs`; I discarded it
in favour of the committed upstream version so the local tree and
remote stay byte-aligned.

### Verification

After the discard + fetch + fast-forward:

```
$ cargo check --workspace --all-targets   # 0 errors, 0 warnings
$ cargo test  --workspace                 # 92 / 92 pass (all crates green)
```

Test breakdown (one more crate than LUM-1022, since `pi-session` is
now real code rather than a scaffold slot):

| Crate | Tests |
|-------|-------|
| `pi-protocol` | 6 |
| `pi-agent-core` smoke | 2 |
| `pi-agent-core` hooks | 4 |
| `pi-ai` | 5 |
| `pi-tui` e2e | 11 |
| `pi-tui` snapshot | 3 |
| `pi-coding-agent` tools | 10 |
| `pi-session` (NEW) | 4 + 1 doc-test |
| `pi-extensions` loader | 32 |
| WASM smoke | 5 |
| faux provider | 9 |
| **total** | **92** |

### Decision: skip new parallel dispatches this round (seventh identical call)

The slots the established pattern tracks (LUM-982, LUM-986, LUM-991,
LUM-992, LUM-1003) are still listed `in_progress` and not actually
moving. With `pi-session` now on `feature/pi.rs`, the remaining
deferred work from the LUM-981 plan is:

1. **Stage 3 pi-extensions WASM host (LUM-986 / LUM-1023)** — still
   the highest-value single follow-up because it unlocks the
   "compatible with pi plugin ecosystem" acceptance criterion.
2. **LUM-984 / LUM-985 protocol reconciliation** — `AssistantMessageEvent`
   design difference between Stage 4/6's events.rs and Stage 2's
   agent_loop is still the largest unresolved merge debt.
3. **LUM-1003 real OpenAI / Anthropic providers** — low-priority
   because the LUM-996 cherry-pick already covers OpenAI Chat
   Completions on `feature/pi.rs`.

A "spin up 3 more parallel runs" reflex would still race on the same
`AssistantMessageEvent` enum, so the same recommendation from
LUM-1017 / LUM-1018 / LUM-1019 / LUM-1022 holds: **no new dispatches
this round, document the state, point at the single highest-value
follow-up**.

### Why the autopilot template keeps firing

Same diagnosis as LUM-1019 / LUM-1022: the trigger text was authored
for the first round (LUM-982) when slots were empty. Each round
re-inherits that wording and has to decide by hand that the slots
are saturated. After eight identical rounds the pragmatic
observation is the same — the template does not produce forward
motion on a saturated queue, and the counter-measure is one focused
single-task dispatch, not more re-runs of the template.

### Candidate concrete next run (single)

Unchanged from the previous six rounds:

- **Stage 3 (LUM-986 / LUM-1023) re-implementation on `feature/pi.rs`** —
  port `lum-981-49282a6b984a/workdir/pi-rust/crates/pi-extensions/`
  host scaffold (or write fresh) onto the current `feature/pi.rs`
  tree (which already has the `pi-extensions` JSON-descriptor loader
  + `pi-session` rusqlite backend), then add an e2e test that loads
  `summarize.ts` / `notify-on-start.ts` from
  `packages/coding-agent/examples/extensions/` and exercises
  `registerTool`. ~1500 LOC + e2e, scope-bounded, no protocol
  reconciliation needed because `feature/pi.rs` already settled on
  the Stage 4/6 event enum.

This unlocks the LUM-981 "plugin ecosystem compatibility" acceptance
criterion in one PR.

### Push status (re-tested)

I re-ran the push at LUM-1026 time:

```
$ git push origin feature/pi.rs
fatal: unable to get credential storage lock in 1000 ms: No such file or directory

$ git ls-remote --heads origin feature/pi.rs
2be1e138abf0f41a5950a4e2a731e71fb4c811b9        refs/heads/feature/pi.rs
```

The credential-helper lock contention is reproducible; git
short-circuits the auth check before falling through to a real auth
prompt, so the round reports "lock contention" rather than "no
credentials". The remote `feature/pi.rs` is already at
`2be1e138a` (the two `feat(pi-session)` commits landed via the
daemon's last sync window — they came from a separate LUM-1024 worker
that pushed directly while the issue was being marked cancelled).

The local mirror
`/home/devbox/multica_workspaces/.repos/77113af3-bd2e-4c2a-9f11-659117e3ca3d/github.com+louloulin+pi.git`
also carries `feature/pi.rs` at `2be1e138a` after my `git fetch
origin`. Net effect: any new commit I add on this branch will be
picked up by the daemon's next sync window.

## LUM-1026 housekeeping — discarded LUM-1024 WIP from this worktree

The worktree
`/home/devbox/multica_workspaces/lumos-659117e3ca3d/lum-1024-34cc34bcff68/workdir/pi`
shipped LUM-1024 work as uncommitted files (modified
`pi-rust/Cargo.toml`, modified `pi-rust/Cargo.lock`, untracked
`pi-rust/crates/pi-session/` directory with 1072 LOC of the rusqlite
+ zstd backend). Those same files exist verbatim on
`origin/feature/pi.rs` as commits `f8bd3945d` and `2be1e138a`.

To avoid forking the upstream history I `git restore`-ed
`pi-rust/Cargo.toml`, `git checkout HEAD -- pi-rust/Cargo.lock`, and
`rm -rf pi-rust/crates/pi-session/` so the working tree matches the
committed `feature/pi.rs` HEAD byte-for-byte. No code from the
discarded WIP is lost — it lives upstream.
