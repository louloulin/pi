# `node:*` builtin virtual modules

Upstream pi runs extensions on Node (or Bun), so its examples and the
extensions in the wild import Node builtins directly:

```
node:path (43)  node:fs (28)  node:fs/promises (18)  node:os (16)
node:crypto (15)  node:child_process (14)  node:url (8)  node:buffer (1)
node:util (1)
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
  `node:crypto` / `node:process` / `node:child_process`. Native-only, like
  every other host import in `src/host.rs` (a `wasm32` binding would need
  to reimplement the op table). `node:child_process`'s `spawn` additionally
  uses two async host imports — `host_child_read(handle, stream)` and
  `host_child_wait(handle)` — because a live child outlives the call that
  created it.
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

### `node:child_process`

The six upstream examples that hold a `ChildProcess` handle import this
directly, so the buffered `pi.exec` bridge is not enough: `spawn` returns
an object whose `stdout` / `stderr` emit `data`, whose `close` fires once
the child is reaped, and whose `kill()` terminates it.

| Node API | Bridge op | Notes |
|---|---|---|
| `execSync(command[, options])` | `child_process.runSync` | Shell command (`/bin/sh -c` on unix, `cmd.exe /C` on Windows). Returns a `Buffer`, or a string when `encoding` is set. Throws on a non-zero exit with `status` / `signal` / `stdout` / `stderr` / `pid` / `output` (`truncated-tool.ts` branches on `err.status === 1`). |
| `execFileSync(file[, args][, options])` | `child_process.runSync` | No shell. Same return shape as `execSync`. |
| `spawnSync(command[, args][, options])` | `child_process.runSync` | No shell by default (`{shell: true}` opts in). Never throws: a spawn failure becomes `{error, status: null, signal: null, pid: 0}`, and `output` is `[null, stdout, stderr]`. |
| `exec(command[, options], callback)` | `child_process.runSync` | Runs through the shell; callback is `(error, stdout, stderr)` on the microtask queue. Default encoding `utf8`. |
| `execFile(file[, args][, options], callback)` | `child_process.runSync` | No shell. |
| `promisify(exec)` / `promisify(execFile)` | via `child_process.runSync` | The `util.promisify.custom` symbol is installed on both, so it resolves `{stdout, stderr}` and rejects with `error.stdout` / `error.stderr` — `mac-system-theme.ts` does `const { stdout } = await promisify(exec)(…)`. |
| `spawn(command[, args][, options])` | `child_process.spawn` + `host_child_read` / `host_child_wait` | Returns a `ChildProcess`: `pid`, `killed`, `exitCode`, `signalCode`, `stdin` (`null` when ignored), `stdout` / `stderr` (`on("data")` / `on("end")` / `setEncoding`), `on("exit")` / `on("close")` / `on("error")`, `kill()`, `ref()` / `unref()`. |
| `fork(modulePath)` | — | Not supported: needs an IPC channel the host does not have. Throws a named error. |

Options: `cwd`, `env` (replaces the environment, like Node), `shell`,
`encoding`, `maxBuffer`, `timeout`, `input`, `stdio`. `stdio` accepts
`"pipe"` / `"ignore"` / `"inherit"` plus the positional array form; a
stream or `"ipc"` entry throws. `detached` is accepted and ignored.

The child is polled by a host task and each pipe drained by its own reader
task, so a command that writes more than a pipe buffer cannot deadlock
(`child_process_honours_cwd_env_and_drains_large_output`).

### `node:util`

Pure JS — no host op is involved, so the whole module is shim-side. The
upstream examples import `promisify` (`mac-system-theme.ts`), and the
evals reporters use `stripVTControlCharacters` / `styleText`.

| Node API | Notes |
|---|---|
| `format` / `formatWithOptions` | `%s %d %i %f %j %o %O %c %%`; extra arguments are appended (strings verbatim, everything else inspected). Output matches Node v22 for the cases covered by `node_util_surface_matches_node`. |
| `inspect(value[, options])` | `depth`, `colors`, `showHidden`, `maxArrayLength`, `maxStringLength`, `sorted`, `customInspect`; `Map` / `Set` / `Date` / `RegExp` / `Error` (stack) / typed arrays / `Buffer` / class instances / `Object.create(null)` / circular references (`<ref *N>` + `[Circular *N]`). `inspect.custom` and `inspect.defaultOptions` are wired. |
| `promisify(fn)` | Honours `fn[util.promisify.custom]`. |
| `callbackify(fn)` | Honours `fn[util.callbackify.custom]`; callbacks fire on the host microtask queue. |
| `inherits(ctor, superCtor)` | Sets `super_`, the prototype chain and `Object.setPrototypeOf(ctor, superCtor)`. |
| `deprecate(fn, msg[, code])` | Reports the first use through `host_log("warn", …)` (`[CODE] DeprecationWarning: …`). |
| `stripVTControlCharacters(str)` | Node's own VT/OSC pattern. |
| `styleText(format, text[, options])` | Node's `[open, close]` palette, one escape per style, closes reversed. |
| `types` | The full Node 22 predicate set. `isProxy` / `isModuleNamespaceObject` / `isCryptoKey` / `isKeyObject` are `false`-only because no such objects exist in this runtime. |
| `isDeepStrictEqual` | Prototype-aware for `Object.create(null)` and class instances, unordered for `Map` / `Set`, `NaN` equal, `0 !== -0`, own enumerable string **and** symbol keys. |
| `debuglog` / `debug` | Enabled by `NODE_DEBUG` (section name or `*`); output goes to `host_log("debug", …)`. |
| `isArray` / `isBoolean` / `isBuffer` / `isDate` / `isError` / `isFunction` / `isNull` / `isNullOrUndefined` / `isNumber` / `isObject` / `isPrimitive` / `isRegExp` / `isString` / `isSymbol` / `isUndefined`, `toUSVString`, `_extend`, `log` | The legacy helpers. |
| `TextEncoder` / `TextDecoder` | `encode` / `encodeInto`; decoding labels `utf-8`, `utf-16le` and `windows-1252` (canonical for `latin1` / `ascii`), BOM stripping. Also installed as globals when the engine lacks them (QuickJS does). |

Not covered: `parseArgs`, `parseEnv`, `diff`, `aborted`,
`transferableAbortSignal` / `transferableAbortController`,
`getSystemErrorName` / `getSystemErrorMessage` / `getSystemErrorMap`,
`getCallSite` / `getCallSites`, `MIMEType` / `MIMEParams` and
`setTraceSigInt` — importing the module still works, but those names are
absent (the failure is a plain "undefined is not a function").

## Coverage against the repo's own extensions

| Extension | Builtins it imports | Status |
|---|---|---|
| `claude-rules.ts`, `file-trigger.ts`, `preset.ts`, `provider-payload.ts`, `titlebar-spinner.ts`, `truncated-tool.ts`, `subagent/agents.ts`, `dynamic-resources/index.ts`, `gondolin/index.ts` | `node:fs`, `node:fs/promises`, `node:path`, `node:url` | Builtins **fully covered**. |
| `.pi/extensions/import-repro.ts` | `node:buffer`, `node:fs`, `node:path` | Covered; additionally needs the `fetch` global. |
| `.pi/extensions/prompt-url-widget.ts` | `node:fs/promises`, `node:os`, `node:path` | Covered; additionally needs the `@earendil-works/pi-tui` module. |
| `.pi/extensions/redraws.ts`, `.pi/extensions/tps.ts` | — | No builtins; need the `@earendil-works/*` modules only. |
| `git-merge-and-resolve.ts`, `subagent/index.ts`, `sandbox/index.ts`, `doom-overlay/doom-engine.ts`, `doom-overlay/wad-finder.ts` | covered set + `node:readline` / `node:child_process` / `node:module` / `node:zlib` | `node:child_process` is bridged (LUM-1110); still blocked on `node:readline` / `node:module` / `node:zlib`. `sandbox/index.ts` additionally needs `setTimeout` / `process.kill`, which are engine/process-contract gaps rather than builtin ones. |
| `interactive-shell.ts`, `ssh.ts`, `mac-system-theme.ts`, `truncated-tool.ts` | `node:child_process` (+ `node:util`) | **Unblocked**: all four now have the builtins they import. `ssh.ts`'s timed/abortable path additionally needs the `setTimeout` global (engine-level, not builtin); `AbortSignal` is available since LUM-1116. |
| `auto-commit-on-exit.ts`, `border-status-editor.ts`, `dirty-repo-guard.ts`, `git-checkpoint.ts`, `github-issue-autocomplete.ts`, `inline-bash.ts`, `input-transform-streaming.ts`, `shutdown-command.ts` | — (shell out through the extension API) | **Unblocked**: these use `pi.exec`, which is now bridged to the Rust host (see [`EXTENSIONS.md`](EXTENSIONS.md#host-imports-rust--js)); they never import `node:child_process` themselves. |

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
| `util.inspect` renders everything on one line — `breakLength` / `compact` are accepted but ignored — and boxed primitives / Promises print as `Boolean {}` / `Promise { <pending> }` instead of Node's resolved-state form. | Extensions log the output; line wrapping buys nothing here. |
| `util.styleText` always emits ANSI codes; it does not consult `process.stdout.hasColors` (there is no TTY in the embedded engine). | Pass `{validateStream: false}` for Node-identical bytes; the codes are what the evals reporters consume. |
| `util.TextDecoder` ignores `{stream: true}` and `fatal: true`; decoding never throws on malformed input. | Streaming would need a per-instance byte buffer; non-fatal decoding matches Node's default. |
| `kill()` on a `spawn`ed child always sends SIGKILL and ignores its signal argument; a `timeout` also reports `signal: "SIGKILL"`, where Node uses SIGTERM. The error still carries `code: "ETIMEDOUT"` / `status: null` / `killed: true`, like Node. | The host owns one kill primitive; a portable per-signal path would need `libc::kill` on unix and a Windows equivalent. |
| The buffered forms (`exec*` / `*Sync`) stop draining a pipe shortly after the direct child exits instead of waiting for EOF, so output written by a *grandchild* that inherited the pipe (`sh -c "sleep 5 & echo hi"`) is not waited for, and may be truncated. Node blocks until the pipe closes. | The reader threads are bounded so a grandchild cannot pin the host past its deadline. The direct child's own output is always captured: the reader only gives up after 250 ms of no EOF. |
| Every `node:child_process` child is bounded by the host per-call timeout (5 s normally, 300 s in interactive mode); a child that outlives it is killed and reported as `killed` / `signal: "SIGKILL"`. | This is what stops a runaway extension from hanging `pi` — the same ceiling `pi.exec` / `host_exec` enforce. Nothing in the shim can raise it: an `options.timeout` on the buffered forms only kills the child *earlier*, where `pi.exec`'s explicit `options.timeout` raises the host ceiling for that call (LUM-1116). |
| `spawn`'s `options.signal` and the `AbortSignal` families of the buffered forms are ignored. | `pi.exec` honours `options.signal` (LUM-1116) through a dedicated host cancel op; the `node:child_process` bridge has no per-call cancel channel yet. `AbortController` / `AbortSignal` themselves *are* available (polyfilled by the shim), so an extension can still `kill()` on abort itself. |
| `spawn` pipes are fully buffered host-side; `maxBuffer` does not apply to `spawn` (only to the buffered forms), and there is no implicit 1 MiB default anywhere. | The reader tasks must always drain so the child never blocks on a full pipe; enforcing a cap on a live stream would mean dropping bytes an extension can still observe. |
| `stdio: "inherit"` replays the captured output through the log-backed `process.stdout` / `stderr` after the child exits, instead of handing the real terminal to the child. | The `pi` process owns stdout (`--rpc`); a raw passthrough would corrupt the protocol. `spawnSync(…, {stdio: "inherit"})` therefore returns `stdout: null`, like Node. |
| Writing to `child.stdin` is not supported (`spawn`'s stdin is `/dev/null`) and `detached` is ignored. | The host bridges no stdin channel; `detached` would need a process-group model the host does not have. |

## Not bridged (frontier)

Importing these fails with the readable error
`unsupported import "node:x" in pi extension: available virtual modules are …`:

| Builtin | Why it is missing | What it would take |
|---|---|---|
| `node:module` | `createRequire` would let an extension `require` arbitrary paths off disk, which the virtual-module sandbox exists to prevent. | Would need a deliberate decision to widen the sandbox, e.g. require-from-`node_modules`-only. Used by `doom-overlay/doom-engine.ts`. |
| `node:readline` | Interactive prompting; needs streams and stdin ownership. | `readline.createInterface` over a stream bridge; the host owns stdin. Used by `git-merge-and-resolve.ts`. |
| `node:zlib` | No compression backend in the workspace. | Add `flate2`/`miniz_oxide` and expose `gunzipSync`/`gzipSync`/`inflateRawSync`/`deflateSync`. Used by `doom-overlay/wad-finder.ts` (`gunzipSync`). |
| `node:stream` / `node:http` / `node:net` / `node:worker_threads` | No event loop integration for streams. | Substantial; probably out of scope for the QuickJS host. |
| `crypto.createHash` / `createHmac` / `webcrypto` | No digest backend is bundled in the workspace. | Add a small SHA-256 implementation (`sha2`) or vendor a JS one. |
| `fs.watch`, `fs.createReadStream/WriteStream` | Needs a filesystem watcher and stream plumbing. | `notify` crate + stream bridge. |
| `os.cpus()`, `os.totalmem()`, `os.networkInterfaces()` | Machine topology has no consumer yet; inventing numbers would be worse than failing. | Straightforward `sysinfo`-style additions when needed. |
| `process.argv`, `process.execPath`, `process.stdin`, `process.kill` | The host owns the process; extensions must not steer it. | Probably never. |
| `node:test`, `node:assert` global, `atob`/`btoa`, `fetch`, `URL` | Engine-level globals QuickJS does not ship (`TextEncoder` / `TextDecoder` are now polyfilled from `node:util` and installed globally, and `AbortController` / `AbortSignal` by the extension shim for `pi.exec` cancellation — see [`EXTENSIONS.md`](EXTENSIONS.md#host-imports-rust--js)). | Small JS polyfills; add on demand. `fetch` is the one that matters most (the repo's own `.pi/extensions/import-repro.ts` fetches gists/issue comments), and it needs a real HTTP bridge, not a polyfill. |

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
