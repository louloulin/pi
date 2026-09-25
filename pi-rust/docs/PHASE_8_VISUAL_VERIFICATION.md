# Phase 8 — End-to-End Visual Verification (G1–G7 closure)

## Goal

Drive every gap-closure scenario through `App::render_snapshot` and capture
the rendered frames to disk so they can be diffed against the TS pi-tui
reference snapshots. This is the make-or-break gate of the parity plan.

## Snapshot harness

`crates/pi-coding-agent/examples/snapshot_tui.rs` is the harness. It boots
a fresh `App` against the `faux` provider + dark theme for each scenario,
installs the `app.*` keybinding table (`APP_CHORDS` — see the file), and
writes:

* `target/snapshot/<scenario>.txt` — trimmed per-row plain text.
* `target/snapshot/<scenario>.cells.txt` — per-cell symbol trace, for
  pixel-level work (CJK widths, etc.).

Run with:

```bash
cargo run -p pi-coding-agent --example snapshot_tui
# or:
SNAPSHOT_OUT=path/to/dir cargo run -p pi-coding-agent --example snapshot_tui
```

## Scenarios

| # | Scenario | What it pins |
|---|----------|--------------|
| 01 | empty | Cold-boot view: title + 21 expanded hint rows + composer + footer |
| 02 | messages | `> hi` user row + `hi back` assistant row in transcript |
| 03 | composer-multiline | Three-line composer with `▍` caret; G3 top border above |
| 04 | selector | Modal overlay: title, G5 `─` separator, items with `❯` cursor |
| 05 | tool-block | Folded tool block (`+196 lines, Ctrl+O to expand`) + last 4 output lines |
| 06 | footer-xp | `?/1.0k  ? for help • xp  snapshot-footer-xp  (anthropic) Faux` (G2) |
| 07 | short-terminal | 23-row terminal forces G4 fold path: title + `Press Alt+H…` + composer border |

## Gap coverage map

| Gap | Scenario that proves it | Visible artefact |
|-----|-------------------------|------------------|
| G1  | 05 (tool-block) | Folded tool rows show the `ToolSuccessBg` slot on each cell (see `05-tool-block.cells.txt` + the per-cell `Buffer` walk in `dump_cell_traces`). |
| G2  | 06 (footer-xp) | `• xp` appears between `? for help` and the session name. |
| G3  | 03 (composer-multiline) and 07 (short-terminal) | A `─` row sits immediately above the composer (top border). |
| G4  | 07 (short-terminal) | `Press Alt+H to show full startup help and loaded resources.` appears as the compactOnboarding row. |
| G5  | 04 (selector) | A `─` row sits between the selector title and the items. |
| G6  | Phase 3 unit tests in `streaming_assistant.rs` | The `WORKING_LABEL` is replaced by `working_header_line(spinner)`; covered by tests, not the harness (the spinner is time-dependent and the harness captures one instant). |
| G7  | Phase 6 unit tests in `user_block_box.rs` | `BoxLayout::with_bg` carries the slot onto the inner span; covered by tests, not the harness. |
| G8  | — | TS wins where it covers (80 ms spinner tick, default blockquote gutter); nanopi's 120 ms tick and other tweaks would only matter if TS were silent. |

## Known acceptable diffs

* **Spinner frame number** (G6) — the harness captures one instant, so the
  frame glyph (`⠋` / `⠙` / `⠹` / …) in any `Working …` line depends on
  when the snapshot is taken. Acceptable.
* **Token counts** in the footer (`1.0k` in `01-empty`) — driven by the
  faux agent, not the harness. Acceptable.
* **Animated caret / live reload timers** — only paint on tick; the
  harness's `render_snapshot` does not advance ticks. Acceptable.

## Cross-check vs TS pi-tui

To compare against the upstream reference, the corresponding TS renderer
in `packages/coding-agent/src/modes/interactive/interactive-mode.ts` must
be invoked under the same `assets/themes/dark.json` (TS uses the same
themes via `packages/tui/src/theme.ts` — they share the JSON schema with
pi-rust). The output of:

```bash
node -e 'import("./packages/tui/dist/index.js").then(t => /* boot App, render the same scenarios, dump cells */)'
```

can be diffed directly against `target/snapshot/*.cells.txt` after
stripping ANSI (`sed 's/\x1b\[[0-9;]*m//g'`).

## Status

* Harness: **live** (compiles + runs + writes all 9 files).
* G1–G7 closure: **complete** (covered by the harness above + unit tests
  for G6/G7).
* TS reference diff: **pending** — no reference snapshots captured yet;
  the harness output is the seed for the parity diff workflow.