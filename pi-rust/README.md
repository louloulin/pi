# Pi Rust — Rust rewrite of the Pi coding agent

This workspace is the Rust port of the Pi monorepo (`packages/agent`, `packages/ai`,
`packages/coding-agent`, `packages/tui`, `packages/protocol`, …). The aim is functional
parity with the existing TypeScript implementation **and** a WASM-hosted extension API
that can load the existing pi extension ecosystem (or close-enough facades of it).

> Status: scaffolding only. See `docs/PLAN.md` for the staged implementation plan and
> `docs/ARCHITECTURE.md` for the cross-crate design.

## Crate map

| Crate | Mirrors | Status | Notes |
|---|---|---|---|
| `pi-ai` | `packages/ai` | scaffolded | Multi-provider LLM types + streaming clients |
| `pi-agent-core` | `packages/agent` | scaffolded | Stateful agent loop, tool execution, events |
| `pi-protocol` | `packages/protocol` | scaffolded | Wire types shared between crates and WASM |
| `pi-extensions` | new (WASM host) | scaffolded | Loads pi extensions; targets `pi-coding-agent` extensions JS shape |
| `pi-tui` | `packages/tui` | scaffolded | Terminal UI primitives |
| `pi-coding-agent` | `packages/coding-agent` | scaffolded | Interactive CLI binary |
| `pi-mono` | root monorepo meta | scaffolded | Workspace glue, CLI entry point |
| `pi-evals` | `packages/evals` | active | Offline-first eval harness + regression suites |

## Building

Requires Rust 1.75+ (edition 2021). Once the toolchain is installed locally:

```bash
cd pi-rust
cargo build --workspace
cargo test  --workspace
```

## Layout

```
pi-rust/
├── Cargo.toml                  # workspace manifest
├── crates/                     # one crate per pi package
│   ├── pi-ai/
│   ├── pi-agent-core/
│   ├── pi-coding-agent/
│   ├── pi-tui/
│   ├── pi-protocol/
│   ├── pi-extensions/
│   ├── pi-mono/
│   └── pi-evals/
└── docs/
    ├── PLAN.md                 # staged delivery plan
    └── ARCHITECTURE.md         # cross-crate design notes
```

## Relationship to the upstream TS repo

This directory is **not** a fork — it is an independent workspace kept next to the
existing `pi/` checkout (TypeScript source) so we can reference and cross-check
behaviour during the port. The Rust crates will eventually publish under the same
family of names with a `-rs` suffix to avoid registry collisions (e.g.
`pi-agent-core-rs`).
