# `node:*` builtin virtual modules

Upstream pi runs extensions on Node (or Bun), so its examples and the
extensions in the wild import Node builtins directly:

```
node:path (43)  node:fs (28)  node:fs/promises (18)  node:os (16)
node:crypto (15)  node:child_process (14)  node:url (8)  node:buffer (1)
```

The `pi` Rust port embeds QuickJS, which has no operating-system surface
of its own. This document describes the subset that is bridged, how it is
shaped, and what is deliberately missing.

## How the bridge works

```
extension ESM import                shim                          host (Rust)
──────────────────────────────────────────────────────────────────────────────
import { readFileSync }             __pi_import("node:fs")        —
  from "node:fs";            ─────▶ returns the frozen module     —
readFileSync(p, "utf8")             fs.readFileSync → __pi_node_call("fs.readFile", {path})
                             ─────────────────────────────────▶ host_node_call(op, argsJson)
                                                                    ↓
                                                              std::fs::read(..)
                            envelope as a JSON string       ◀─────────────────
  returns string / Buffer   {"ok":true,"value":{"base64":"…"}}
                            {"ok":false,"code":"ENOENT", …}
```

Key properties:

- **One host import.** `host_node_call(op, argsJson)` is the *only*
  native entry point for all of `node:fs` / `node:os` / `node:buffer` /
  `node:crypto` / `node:process`. Native-only, like every other host
  import in `src/host.rs` (a `wasm32` binding would need to reimplement
  the op table).
- **Errors are values.** The bridge never throws: it returns
  `{"ok":false,"code","message","syscall","path"}` and the shim turns
  that into a Node-shaped `Error` (`err.code === "ENOENT"`) so
  extension code can branch exactly as it does on Node.
- **Sync is the source of truth.** The bridge is a blocking call, so
  `node:fs/promises` and the callback forms are thin wrappers over the
  same sync functions; they resolve/queue on the microtask queue but do
  the I/O inline.
- **`require` works too.** Both module formats resolve through the same
  virtual module map, so CJS-compiled TS extensions
  (`require("node:fs")`) see the identical surface.
- **`Buffer` and `process` are globals.** Upstream code uses them
  without importing; the shim installs them only when the name is free.

## Supported surface

### `node:fs` (and `node:fs/promises`)

| Node API | Bridge op | Notes |
|---|---|---|
| `readFileSync` / `readFile` / `promises.readFile` | `fs.readFile` | Returns a `Buffer`; a string when `encoding` is given (or the options argument is a string). Only `r` / `rs` / `r+` flags are accepted. |
| `writeFileSync` / `writeFile` / `promises.writeFile` | `fs.writeFile` | `data` may be a string (with `encoding`), `Buffer`, typed array or array of bytes. Truncates. |
| `appendFileSync` / `appendFile` / `promises.appendFile` | `fs.appendFile` | Creates the file when missing. |
| `existsSync` | `fs.exists` | Follows symlinks, like Node. |
| `readdirSync` / `readdir` / `promises.readdir` | `fs.readdir` | `{withFileTypes: true}` yields `Dirent`-shaped objects (`name`, `isFile()`, `isDirectory()`, `isSymbolicLink()`). Sorted by name (Node leaves the order unspecified). |
| `statSync` / `lstatSync` / `promises.stat` / `promises.lstat` | `fs.stat`, `fs.lstat` | `Stats`-shaped object: `size`, `mode`, `mtimeMs`/`atimeMs`/`ctimeMs`/`birthtimeMs`, `mtime`/`atime`/`ctime`/`birthtime` `Date`s, `isFile()`/`isDirectory()`/`isSymbolicLink()`. |
| `mkdirSync` / `mkdir` / `promises.mkdir` | `fs.mkdir` | `{recursive: true}` → `create_dir_all`. |
| `rmSync` / `rm` / `promises.rm` | `fs.rm` | `{recursive, force}` honoured. |
| `unlinkSync` / `unlink` / `promises.unlink` | `fs.unlink` | Files only (like Node). |
| `rmdirSync` / `rmdir` / `promises.rmdir` | `fs.rmdir` | Empty directories only. |
| `renameSync` / `rename` / `promises.rename` | `fs.rename` | |
| `copyFileSync` / `copyFile` / `promises.copyFile` | `fs.copyFile` | `COPYFILE_*` flags are accepted but ignored (always overwrite). |
| `realpathSync` / `realpath` / `promises.realpath` | `fs.realpath` | Canonicalises, like Node. |
| `accessSync` / `access` / `promises.access` | `fs.exists` | **Only checks existence**; `R_OK`/`W_OK`/`X_OK` are not probed and `mode` is ignored. |
| `fs.constants` | — | `F_OK`/`R_OK`/`W_OK`/`X_OK` + the `COPYFILE_*` bits. |

Callback forms exist for `readFile`, `writeFile`, `appendFile`, `readdir`,
`stat`, `lstat`, `mkdir`, `rm`, `unlink`, `rmdir`, `rename`, `copyFile`,
`realpath` and `access`; callbacks fire on the microtask queue, never
synchronously.

### `node:buffer`

`Buffer` is a real `Uint8Array` subclass, so indexing, `length`,
iteration, `instanceof` and `Buffer.isBuffer` behave like Node.

- Statics: `from(value[, encoding])`, `alloc(size[, fill[, encoding]])`,
  `allocUnsafe`, `isBuffer`, `byteLength(string[, encoding])`,
  `concat(list[, totalLength])`, `compare(a, b)`.
- Prototype: `toString([encoding[, start[, end]]])`, `equals(other)`,
  `compare(other)`, `slice`, `subarray`, `toJSON()`.
- Encodings: `utf8`/`utf-8`, `latin1`/`binary`/`ascii`,
  `utf16le`/`ucs2`, `hex`, `base64`, `base64url`. Encoding/decoding is
  hand-rolled (QuickJS has no `TextEncoder` / `atob`).

### `node:os`

`homedir()`, `tmpdir()`, `platform()`, `arch()`, `type()`, `release()`,
`hostname()`, `endianness()`, `EOL`.

### `node:crypto`

`randomBytes(size[, callback])`, `randomUUID()` (v4, RFC 4122 bits set),
`randomInt([min,] max)` (rejection sampling, no modulo bias). Entropy
comes from `/dev/urandom`; there is no fallback PRNG on purpose.

### `node:process` (also the `process` global)

`env` (snapshot object), `platform`, `arch`, `pid`, `cwd()`, `nextTick`,
`version`, `versions`, `exitCode`, `stdout.write`, `stderr.write`.

## Coverage against the repo's own extensions

| Extension | Builtins it imports | Status |
|---|---|---|
| `claude-rules.ts`, `file-trigger.ts`, `preset.ts`, `provider-payload.ts`, `titlebar-spinner.ts`, `truncated-tool.ts`, `subagent/agents.ts`, `dynamic-resources/index.ts`, `gondolin/index.ts` | `node:fs`, `node:fs/promises`, `node:path`, `node:url` | Builtins **fully covered**. |
| `.pi/extensions/import-repro.ts` | `node:buffer`, `node:fs`, `node:path` | Covered; additionally needs the `fetch` global. |
| `.pi/extensions/prompt-url-widget.ts` | `node:fs/promises`, `node:os`, `node:path` | Covered; additionally needs the `@earendil-works/pi-tui` module. |
| `.pi/extensions/redraws.ts`, `.pi/extensions/tps.ts` | — | No builtins; need the `@earendil-works/*` modules only. |
| `git-merge-and-resolve.ts`, `subagent/index.ts`, `sandbox/index.ts`, `doom-overlay/doom-engine.ts`, `doom-overlay/wad-finder.ts` | covered set + `node:readline` / `node:child_process` / `node:module` / `node:zlib` | Partially covered — blocked on the frontier rows below. |
| `interactive-shell.ts`, `ssh.ts`, `mac-system-theme.ts` | `node:child_process` (+ `node:util`) | Blocked on `node:child_process`. |

The `@earendil-works/pi-coding-agent` and `@earendil-works/pi-tui`
specifiers are a separate gap: they are pi's own APIs, not Node ones, and
virtualising them is its own work item.

## Deliberate divergences from Node

| Divergence | Why |
|---|---|
| Error `message` text comes from Rust (`"ENOENT: No such file or directory (os error 2), open '/x'"`); `code` / `syscall` / `path` match Node. | Extensions branch on `code`/`syscall`; reproducing Node's exact `strerror` prose buys nothing. |
| `mkdirSync(path, {recursive: true})` returns `undefined`, Node returns the first created directory. | The bridge would need a second syscall per ancestor. |
| `accessSync` only checks existence. | See the table above. |
| `readdirSync` returns name-sorted entries. | Deterministic tests; Node's order is unspecified. |
| `copyFileSync` ignores `COPYFILE_EXCL` / `COPYFILE_FICLONE`. | Always a plain overwrite copy. |
| `process.env` is a read-only snapshot; assigning to it stays in JS. | The bridge cannot mutate the host process environment. |
| `process.version` is `"v0.0.0-pi-rust"`, not a Node version. | There is no Node here; version-gated feature checks take the conservative branch. |
| `process.stdout.write` / `stderr.write` go to `host_log`, not the real streams. | The `pi` process owns stdout (`--rpc` speaks JSON-RPC on it); an extension must never write raw bytes there. `isTTY` is `false`. |
| Reading a file without an encoding returns a `Buffer` (correct), but `Buffer` lacks the numeric `readUInt32BE` / `writeUInt32LE` / `fill`-with-pattern families. | Out of the subset the ecosystem uses so far. |

## Not bridged (frontier)

Importing these fails with the readable error
`unsupported import "node:x" in pi extension: available virtual modules are …`:

| Builtin | Why it is missing | What it would take |
|---|---|---|
| `node:child_process` | Needs streaming stdio + a process lifetime model that respects the host deadline. | `tokio::process`, an op for spawn/exec with buffered stdout/stderr, and cancellation. Highest-value next step: `examples/extensions/interactive-shell.ts`, `mac-system-theme.ts`, `sandbox/index.ts` and community extensions shell out. |
| `node:util` | Pure JS; nothing here needs a host op. | Cheapest unblock on this list: `promisify` / `callbackify` (the shim already has both internally for `node:fs`), then `format` / `inspect` / `types`. `mac-system-theme.ts` imports `promisify`, but is blocked on `node:child_process` as well. |
| `node:module` | `createRequire` would let an extension `require` arbitrary paths off disk, which the virtual-module sandbox exists to prevent. | Would need a deliberate decision to widen the sandbox, e.g. require-from-`node_modules`-only. Used by `doom-overlay/doom-engine.ts`. |
| `node:readline` | Interactive prompting; needs streams and stdin ownership. | `readline.createInterface` over a stream bridge; the host owns stdin. Used by `git-merge-and-resolve.ts`. |
| `node:zlib` | No compression backend in the workspace. | Add `flate2`/`miniz_oxide` and expose `gunzipSync`/`gzipSync`/`inflateRawSync`/`deflateSync`. Used by `doom-overlay/wad-finder.ts` (`gunzipSync`). |
| `node:stream` / `node:http` / `node:net` / `node:worker_threads` | No event loop integration for streams. | Substantial; probably out of scope for the QuickJS host. |
| `crypto.createHash` / `createHmac` / `webcrypto` | No digest backend is bundled in the workspace. | Add a small SHA-256 implementation (`sha2`) or vendor a JS one. |
| `fs.watch`, `fs.createReadStream/WriteStream` | Needs a filesystem watcher and stream plumbing. | `notify` crate + stream bridge. |
| `os.cpus()`, `os.totalmem()`, `os.networkInterfaces()` | Machine topology has no consumer yet; inventing numbers would be worse than failing. | Straightforward `sysinfo`-style additions when needed. |
| `process.argv`, `process.execPath`, `process.stdin`, `process.kill` | The host owns the process; extensions must not steer it. | Probably never. |
| `node:test`, `node:assert` global, `TextEncoder`, `atob`/`btoa`, `fetch`, `URL` | Engine-level globals QuickJS 0.8 does not ship. | Small JS polyfills; add on demand. `fetch` is the one that matters most (the repo's own `.pi/extensions/import-repro.ts` fetches gists/issue comments), and it needs a real HTTP bridge, not a polyfill. |

The upstream examples are the compatibility yardstick: the test
`upstream_node_imports_are_all_bridged_or_documented`
(`tests/node_builtins.rs`) scans `.pi/extensions/` and
`packages/coding-agent/examples/extensions/` for `node:*` imports and
fails unless every specifier is either bridged or listed above — so this
table and the shim cannot drift apart silently.

## Adding a new op

1. `crates/pi-extensions/src/host.rs` — add an arm to `node_call(op, args)`,
   returning a JSON value or `NodeError::io(..)`/`NodeError::invalid(..)`.
2. `crates/pi-extensions/runtime/pi-ext-shim.mjs` — wrap it in the matching
   module object (`__pi_fs_module`, `__pi_os_module`, …) and add the
   specifier to `globalThis.__pi_virtual_modules` if it is a new module.
3. `crates/pi-extensions/tests/node_builtins.rs` — extend the probe tool in
   the relevant test; assert on the returned `details` object and, where it
   matters, on the real filesystem afterwards.
4. Update the tables in this file.
