# Session export (`/export`, `pi --export`)

Rust port of the upstream `packages/coding-agent` session-export surface:
the interactive `/export [path]` slash command and the standalone
`pi --export <session.jsonl> [output.html]` CLI flag.

## What it does

| Entry point | Output | Default path |
| --- | --- | --- |
| `/export` | self-contained HTML | `session-<ISO>.html` (cwd) |
| `/export <path>.jsonl` | JSONL session branch | the given path |
| `/export <other-path>` | self-contained HTML | the given path |
| `pi --export <file.jsonl>` | self-contained HTML | `pi-session-<file-basename>.html` (cwd) |
| `pi --export <file.jsonl> <out.html>` | self-contained HTML | the given path |

The HTML document is generated from the **verbatim** upstream template
assets under `crates/pi-coding-agent/assets/export-html/` (embedded with
`include_str!`, so the `pi` binary stays a single file). Rust only builds
the `SessionData` payload, base64-encodes it into the `{{SESSION_DATA}}`
placeholder, expands the theme CSS variables, and inlines the vendored
`marked` / `highlight.js` bundles. All Markdown / syntax rendering happens
in the browser, exactly as upstream.

The JSONL export follows
`packages/coding-agent/src/core/session-export.ts`: a regenerated
`{"type":"session",…}` header, then every entry with `parentId` rewritten
into a linear chain, terminated by a newline.

## Module layout

| File | Responsibility |
| --- | --- |
| `src/export/mod.rs` | `SessionData` / `ToolInfo` / `ExportError`, the four public export functions, default path helpers |
| `src/export/html.rs` | `include_str!` templates, base64, `generate_html` |
| `src/export/theme.rs` | theme → CSS bridge (`generateThemeVars` / `deriveExportColors`) on top of `pi_tui::theme` |
| `src/export/session_file.rs` | message → entry conversion, JSONL session-file reader |
| `src/export/jsonl.rs` | JSONL generator + `session-<ISO>` file naming |
| `src/commands/export.rs` | `/export` format selection and the `--export` entry point |

## Error messages

The user-facing strings match upstream so scripts keep working:

- `File not found: <path>` — `--export` target missing
- `Session file is not a valid pi session: <path>` — first JSONL line is
  not a session header
- `Nothing to export yet - start a conversation first` — `/export` before
  any message
- `Failed to export session: <message>` — `/export` failure in the TUI

## Path handling

The *input* path is resolved against the cwd and lexically normalised
before it is read, so error messages carry an absolute path exactly like
upstream's `resolvePath` (`File not found: /abs/path.jsonl`). The *output*
path is only normalised, never absolutised, so `pi --export s.jsonl out.html`
prints `Exported to: out.html` and the no-output form prints
`Exported to: pi-session-<input-basename>.html` — both byte-identical to
upstream.

## Known divergences

1. **`preRenderCustomTools` is not ported.**
   Upstream renders extension tools through their TUI (ANSI → HTML)
   renderers before injecting `renderedTools` into the payload. The Rust
   port always leaves `renderedTools` unset, so *every* tool call and
   result is rendered by `template.js` (the same fallback upstream uses for
   its built-in `bash` / `read` / `write` / `edit` / `ls` set). Extension
   tools therefore lose their bespoke TUI rendering in an export; they are
   still shown, just in the generic call/result form. This is an explicit
   gap, not a silent one: the payload simply omits the key.
2. **`/export` default HTML file name.**
   Upstream names the TUI export
   `pi-session-<session-file-basename>.html`; this port writes
   `session-<ISO>.html`, matching the LUM-1174 acceptance criteria. The
   `--export` default still follows upstream exactly
   (`pi-session-<input-basename>.html`).
3. **JSON object key order.**
   `serde_json`'s default map is ordered, so the exported JSON/JSONL (and
   the base64 payload) emits object keys alphabetically, where
   `JSON.stringify` in upstream emits insertion order. JSON object key order
   is not significant, and `template.js` / the session reader are
   order-insensitive; the CSS variable block is reordered the same way.
4. **Session-file reader is permissive.** Upstream only accepts
   `type: "session"` headers and skips unparseable lines; this port also
   accepts the Rust `type: "header"` spelling produced by
   `pi session export`, so a session can be round-tripped from either
   backend.
