# `node:*` builtin virtual modules

Upstream pi runs extensions on Node (or Bun), so its examples and the
extensions in the wild import Node builtins directly:

```
node:path (43)  node:fs (28)  node:fs/promises (18)  node:os (16)
node:crypto (15)  node:child_process (14)  node:url (8)  node:buffer (1)
node:util (1)  node:zlib (1)  node:module (1)  node:readline (1)
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
  `node:crypto` / `node:zlib` / `node:process` / `node:child_process`.
  Native-only, like every other host import in `src/host.rs` (a `wasm32` binding would need
  to reimplement the op table). `node:child_process`'s `spawn` additionally
  uses two async host imports — `host_child_read(handle, stream)` and
  `host_child_wait(handle)` — because a live child outlives the call that
  created it.
- **Some modules are pure JS.** `node:util`, `node:module` and
  `node:readline` add no op at all: they are implemented entirely inside the
  shim, over the same map (and, for `question`, the existing `host_ui_input`
  dialog). A module only earns a host op when it needs something the JS
  side cannot do — the filesystem, entropy, a child process.
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
| `createReadStream(path[, options])` | `fs.readFile` | `Readable`-shaped stream (also exported as `fs.ReadStream`): `on("open"/"data"/"end"/"close"/"error")`, `setEncoding`, `pause`/`resume`, `read`, `pipe`, `[Symbol.asyncIterator]`, `path`, `bytesRead`, `readableEnded`. The file is read up front and replayed, so a `data` listener attached a tick later cannot lose output. `encoding` (a string, or `{encoding}`) decodes the remainder in one pass, so a multi-byte character split across chunks is not corrupted; `start`/`end` are byte offsets with `end` inclusive; only read flags are accepted. `fd` is `null` (there is no descriptor) and `options.fd` is ignored. |
| `createWriteStream(path[, options])` | `fs.writeFile` (`w`) / `fs.appendFile` (`a`) | `Writable`-shaped stream (also exported as `fs.WriteStream`): `write(chunk[, encoding][, cb])`, `end([chunk][, encoding][, cb])`, `destroy([err])`, `cork`/`uncork` (no-ops), `on("open"/"ready"/"finish"/"close"/"error")`, `path`, `bytesWritten`, `writableEnded`, `writableFinished`, `destroyed`. Chunks are accepted into memory and flushed in **one** write on `end()`, so `write()` (and its callback) means "buffered" and only `finish` / the `end()` callback / `close` mean "on disk". Only the `w` (truncate) and `a` (append) flags are accepted; `encoding` defaults to `utf8` and only applies to string chunks. `fd` is `null`. |
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

Hashing is bridged for SHA-1 and SHA-256 through the `crypto.digest` op,
and HMAC through `crypto.hmac` (`crate::digest`, hand-rolled because the
offline registry has no `digest` / `hmac` backend):

- `createHash(algorithm)` with `update(data[, encoding])` (chaining) and
  `digest([encoding])` — a `Buffer` by default, a string for `hex` /
  `base64` / …; `sha256` / `sha-256` / `SHA-256` all parse.
- `createHmac(algorithm, key[, {encoding}])` with the same `update` /
  `digest` shape (LUM-1181). `key` is a string (UTF-8 unless `options.encoding`
  says otherwise) or any `BufferSource`; RFC 2104 is applied in Rust over the
  same SHA-1 / SHA-256 primitives, so keys longer than the 64-byte block are
  hashed first and no new dependency is needed. `tests/web_globals.rs` checks
  the RFC 4231 and RFC 2202 vectors (including the block-size-key cases) and
  `crate::digest`'s own tests cover the same vectors against Python.
- Both hash objects refuse `update` / `digest` after `digest` has run (Node's
  `ERR_CRYPTO_HASH_FINALIZED`), so a reused object fails loudly instead of
  hashing stale state.
- `getRandomValues(typedArray)` (fills in place, 64 KiB quota per call,
  rejects float and `DataView` views) and `randomUUID()`.
- `subtle.digest(name | {name}, data)` — resolves to an `ArrayBuffer`.
- `webcrypto` — `{ getRandomValues, randomUUID, subtle }`.

The key-based WebCrypto operations (`subtle.importKey`, `sign`, `encrypt`,
…) still throw, and names other than SHA-1/SHA-256 (`md5`, `sha512`, …)
surface the host's "unsupported digest algorithm" error rather than a wrong
digest. HMAC is available through the Node `createHmac` surface above, not
through `subtle`, because the host bundles no key-object model.

### `node:zlib`

The whole compression surface the repo uses is bridged. zstd comes from the
`zstd = 0.13` crate (it also compresses `pi-session`'s payload column); the
gzip/deflate family (LUM-1131) is a pure-Rust codec in
`crates/pi-extensions/src/deflate.rs` — RFC 1951 (DEFLATE) / 1950 (zlib) /
1952 (gzip) — because the offline registry has no `flate2` / `miniz_oxide`.
Nothing is faked: the invariants are checked against Python `zlib` fixtures
in `tests/zlib_deflate.rs`.

| Node API | Bridge op | Notes |
|---|---|---|
| `zstdCompressSync(data[, options])` | `zlib.zstdCompress` | `data` may be a string (utf8), `Buffer`, typed array or `ArrayBuffer`; returns a `Buffer`. `options.params[constants.ZSTD_c_compressionLevel]` and `options.level` set the level; the default is zstd's 3, like Node. |
| `zstdDecompressSync(data)` | `zlib.zstdDecompress` | Returns a `Buffer`. A non-zstd input throws an `Error` whose `code` is Node's `ZSTD_error_*` name (`ZSTD_error_prefix_unknown` for a frame with an unknown descriptor) instead of taking the host down. |
| `deflateSync(data[, options])` | `zlib.deflate` | zlib container (RFC 1950: 2-byte header + DEFLATE + Adler-32). Output of level 6 on ASCII is byte-identical to Node's/Python's fixed-Huffman choice for short inputs; see the divergence table for what differs. |
| `inflateSync(data)` | `zlib.inflate` | Accepts stored, fixed-Huffman and dynamic-Huffman blocks, concatenated members and `Z_SYNC_FLUSH` padding. A preset-dictionary header (`FDICT`) throws `Z_STREAM_ERROR`. |
| `deflateRawSync(data[, options])` | `zlib.deflateRaw` | Raw DEFLATE, no header/trailer. |
| `inflateRawSync(data)` | `zlib.inflateRaw` | Raw DEFLATE. |
| `gzipSync(data[, options])` | `zlib.gzip` | gzip container (RFC 1952). The header is reproducible: `MTIME = 0`, `XFL = 0`, `OS = 0xFF`, no name/comment fields. |
| `gunzipSync(data)` | `zlib.gunzip` | Tolerates `FEXTRA` / `FNAME` / `FCOMMENT` / `FHCRC` (it skips and verifies them), then checks the CRC-32 and `ISIZE` trailer — this is what `doom-overlay/wad-finder.ts` calls. |
| `crc32(data[, value])` | `zlib.crc32` | CRC-32/ISO-HDLC. Returns an unsigned 32-bit integer (`crc32("") === 0`, `crc32("123456789") === 3421780262`); the second argument continues a previous checksum, so `crc32(b, crc32(a)) === crc32(a ++ b)`. The same table now backs the gzip trailer. |
| `constants.ZSTD_c_compressionLevel` | — | `100`, the `zstd.h` parameter id `openai-codex-responses.ts` passes through. |
| `constants.Z_NO_COMPRESSION` / `Z_BEST_SPEED` / `Z_BEST_COMPRESSION` / `Z_DEFAULT_COMPRESSION` | — | `0` / `1` / `9` / `-1`, matching Node. `options.level` accepts `-1..=9` and throws `RangeError` with `code: "ERR_OUT_OF_RANGE"` outside it. |

The async/callback forms (`zstdCompress` / `zstdDecompress` / `gzip` / `unzip`),
the stream constructors (`createGzip` / `createDeflate` / …), `unzipSync`'s
header sniffing and every option other than `level` (`windowBits`,
`memLevel`, `strategy`, `dictionary`, `finishFlush`) are not provided — the
bridge is synchronous and nothing in the repo calls them.

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

### `node:module`

Pure JS — no host op is involved. Upstream uses every shape of it:
`doom-overlay/doom-engine.ts:6,64` and `core/extensions/loader.ts:76` read
`createRequire(import.meta.url)`, `chord/src/node/bundle-loader.ts:177` passes a
*path* and then gates package names on `isBuiltin(specifier)` (`:180`, `:206`),
`tui/src/native-module-path.ts:5,15` calls `require.resolve(…)` inside a
`try`/`catch`, and `tui/test/native-platform.test.ts:59,85` mocks a native addon
by writing `new Module(path)` into `require.cache`.

| Node API | Notes |
|---|---|
| `createRequire(from)` / `Module.createRequire` | `from` may be a string or an object with an `href` (`import.meta.url` is a `file://` URL string). The returned `require` resolves **the bridged virtual modules** — the builtins and the `@earendil-works/*` / `typebox` SDK modules — and consults the shared `Module._cache`; anything else throws an `Error` with `code: "MODULE_NOT_FOUND"` and `requireStack: [from]`. |
| `require.resolve(specifier)` | Returns the specifier itself for a resolvable module (a virtual module has no disk path to canonicalize) and throws the same `MODULE_NOT_FOUND` otherwise. |
| `require.cache` | *Is* `Module._cache` — one store, so a mocked module is visible from either handle. |
| `builtinModules` / `isBuiltin(specifier)` | Derived from the virtual module map, i.e. they answer "what can this host resolve?": `node:fs`, `node:module`, `node:readline` … are `true`, `node:stream` is `false` where Node says `true`. `isBuiltin` accepts the bare and `node:` forms. |
| `Module` | `new Module(id[, parent])` with `id` / `path` / `filename` / `exports` / `loaded` / `children` / `paths` / `require(specifier)`; statics `createRequire` / `isBuiltin` / `builtinModules` / `_cache`. |

Not covered: `register` / `registerHooks` / `syncBuiltinESMExports`
(loader-level hooks), `runMain`, `wrap`, `findSourceMap` / `SourceMap`, and the
internal `_load` / `_resolveFilename` / `_extensions` machinery. Requiring a
path that exists on disk does **not** work: the virtual-module sandbox is the
point, and widening it is a separate decision.

### `node:readline`

The sync subset: `createInterface` over a stream-like `input` — any object with
`on("data")` / `on("end")`, which is exactly the `child.stdout` shape
`node:child_process` returns. That is the shape the TUI's own tools use:
`core/tools/grep.ts:169` and `core/tools/find.ts:217` do
`createInterface({ input: child.stdout })` +
`rl.on("line", …)` + `rl.close()`, and the `rpc-extension-ui.ts:521` example
does the same over a spawned agent's stdout with `terminal: false`.
`git-merge-and-resolve.ts:37` and `core/session-manager.ts:698` iterate a file
stream with `for await (const line of rl)` — the latter passing
`crlfDelay: Infinity`, which the pending-CR handling below matches without a
timer. The `process.stdin` callers (`packages/ai/src/cli.ts:48`,
`core/main.ts:283`, `rpc-example.ts:46`) are the ones that take the UI-dialog
path.

| Node API | Notes |
|---|---|
| `createInterface({ input, output?, terminal? })` | Also accepts the deprecated positional form `(input, output, terminal)`. Lines split on `\n`, `\r\n` and a lone `\r`; a CRLF pair straddling two chunks is still one break (the pending CR is remembered instead of using Node's 100 ms `crlfDelay` timer). `close` fires on `rl.close()` and on input `end`. |
| `rl.on("line", …)` / `rl.on("close", …)` / `rl.pause()` / `rl.resume()` / `rl.write(data)` | `Interface` is an `EventEmitter`; `write` feeds `data` through the same splitter (how a caller simulates typing). |
| `for await (const line of rl)` | Reads the internal queue, then waits; `close()` (or input `end`) settles every pending consumer with `done: true` instead of leaving the promise hanging. |
| `rl.question(query[, options][, callback])` | Resolves (or calls back) with the next line, `null` when the input ends first. With no `input`, the query goes to the session UI (`host_ui_input`, the channel `ctx.ui.input` uses). |
| `rl.setPrompt` / `getPrompt` / `prompt()` | The prompt is written to `output` when there is one, else to `host_log`. |

Branches that **fail instead of pretending**: `terminal: true` →
`ERR_READLINE_TTY_UNSUPPORTED` (there is no TTY in the embedded engine, and the
host owns stdin); no `input` and no session UI → `ERR_READLINE_NO_INPUT`; a
non-stream `input` → `ERR_INVALID_ARG_TYPE`; `question` after `close()` →
`ERR_USE_AFTER_CLOSE`.

Not covered: `emitKeypressEvents`, `cursorTo` / `moveCursor` / `clearLine` /
`clearScreenDown`, `getCursorPos`, and the `readline/promises` entry point.

## Globals

`process` and `Buffer` are installed globally (they are Node globals, not just
module exports), plus the web-platform names below. Every install is guarded
with `typeof globalThis.X === "undefined"`, so an engine that grows its own
implementation keeps it, and re-evaluating the shim is idempotent.

| Global | Coverage |
|---|---|
| `process`, `Buffer` | See the module sections above. |
| `TextEncoder` / `TextDecoder` | From `node:util`, installed when the engine lacks them. |
| `fetch` / `Headers` / `Request` / `Response` | Backed by `host_fetch`; see [`EXTENSIONS.md`](EXTENSIONS.md#fetch-global). |
| `atob(string)` / `btoa(string)` | Strict Latin-1 binary-string codec: one code unit per byte, `InvalidCharacterError` above `0xFF`, and `atob` rejects a length that is not a multiple of four or a character outside the base64 alphabet (whitespace is stripped first, as the spec requires). |
| `crypto` | `webcrypto` from `node:crypto`: `getRandomValues` (in place, 64 KiB quota, integer views only), `randomUUID`, `subtle.digest`. |
| `URLSearchParams` | `application/x-www-form-urlencoded` codec: `append` / `delete` / `get` / `getAll` / `has` / `set` / `sort` / `toString` / `forEach` / `keys` / `values` / `entries` / `size` / iterator, constructible from a query string, a record, a sequence of pairs or another `URLSearchParams`. |
| `URL` | WHATWG parser / serializer / resolver over the special schemes (`http` / `https` / `ws` / `wss` / `ftp` / `file`) plus opaque ones (`mailto:`, `urn:`, custom): `href` / `origin` / `protocol` / `username` / `password` / `host` / `hostname` / `port` / `pathname` / `search` / `hash` read **and** written, a live `searchParams`, `toString` / `toJSON`, and the `URL.canParse` / `URL.parse` statics. Divergences below. |

The `URL` polyfill (LUM-1177) closes the gap `URLSearchParams` could not:
the repo's `packages/coding-agent/examples/extensions/custom-provider-gitlab-duo/index.ts`
reads its OAuth redirect as `new URL(callbackUrl).searchParams.get("code")` —
the only global that example was missing. Deliberate divergences
from Node: no IDNA / punycode, so a non-ASCII host is byte-percent-encoded
instead of `xn--`-transcoded (ASCII hosts lower-case exactly as Node does);
no `blob:` unwrapping (`new URL("blob:https://h/x").origin` is `"null"`);
resolving a relative reference against a non-special *opaque* base
(`mailto:`) is not supported; and an emptied live `searchParams` clears the
query instead of leaving a bare `?`. The legacy OAuth extension
`packages/coding-agent/examples/extensions/custom-provider-anthropic/index.ts`
is the clearest consumer of the rest, its PKCE step being
`crypto.getRandomValues` → `btoa` →
`crypto.subtle.digest("SHA-256", …)` → `new URLSearchParams({ … })`; the
`base64_globals_…` / `crypto_globals_…` / `url_search_params_…` tests in
`tests/web_globals.rs` replay it, including the RFC 7636 appendix B vector, and
`node_url_global_…` in `tests/node_builtins.rs` replays the `URL` half.

## Coverage against the repo's own extensions

| Extension | Builtins it imports | Status |
|---|---|---|
| `claude-rules.ts`, `file-trigger.ts`, `preset.ts`, `provider-payload.ts`, `titlebar-spinner.ts`, `truncated-tool.ts`, `subagent/agents.ts`, `dynamic-resources/index.ts`, `gondolin/index.ts` | `node:fs`, `node:fs/promises`, `node:path`, `node:url` | Builtins **fully covered**. |
| `.pi/extensions/import-repro.ts` | `node:buffer`, `node:fs`, `node:path` | **Unblocked**: builtins plus the `fetch` / `Headers` / `Request` / `Response` globals (LUM-1135) are covered, and `response.text()` / `response.json()` / `response.ok` are all it uses. |
| `custom-provider-gitlab-duo/index.ts` | globals `URL` / `URLSearchParams` / `crypto.subtle.digest`, plus `@earendil-works/pi-ai/compat` | **Builtins/globals unblocked** (LUM-1177): the OAuth redirect check is `new URL(callbackUrl).searchParams.get("code")`, which needed the `URL` global's parser and was the only global this example was missing. Its remaining blocker is not a builtin: `anthropicMessagesApi` / `openAIResponsesApi` are documented `pi-ai/compat` gaps (see [`SDK_MODULES.md`](SDK_MODULES.md#earendil-workspi-aicompat--provider-registry--builtin-gaps)). |
| `.pi/extensions/prompt-url-widget.ts` | `node:fs/promises`, `node:os`, `node:path` | Covered; the `@earendil-works/pi-tui` module it needs is bridged as well (see [`SDK_MODULES.md`](SDK_MODULES.md)). |
| `.pi/extensions/redraws.ts`, `.pi/extensions/tps.ts` | — | No builtins; the `@earendil-works/*` modules they need are bridged (see [`SDK_MODULES.md`](SDK_MODULES.md)). |
| `git-merge-and-resolve.ts`, `doom-overlay/doom-engine.ts`, `doom-overlay/wad-finder.ts` | covered set + `node:readline` / `node:module` / `node:zlib` | `node:readline` / `node:module` (LUM-1129), the whole `node:zlib` surface (LUM-1125, LUM-1131) and `fs.createReadStream` (LUM-1172) are bridged, so **`git-merge-and-resolve.ts` reads its input through `createReadStream` + `readline.createInterface` and `wad-finder.ts`'s `gunzipSync` works**. The remaining blocker is non-builtin: `doom-engine.ts` needs to `require` the local `doom.js` off disk (the sandbox refuses). |
| `rpc-extension-ui.ts` (example) | `node:child_process`, `node:path`, `node:readline`, `node:url`, `@earendil-works/pi-tui` | **Unblocked** as far as builtins go: `spawn` + `readline.createInterface({ input: agent.stdout, terminal: false })` + `on("line")` is exactly the subset above. Its remaining dependency is the TUI SDK, not a builtin. |
| `core/tools/grep.ts`, `core/tools/find.ts`, `core/session-manager.ts` (host-side, not extensions) | `node:readline` (+ `node:child_process`) | The readline patterns these rely on (`child.stdout` + `on("line")` + `close()`; file stream + `for await` + `crlfDelay: Infinity`) all work; they are not loaded through the extension host, so this is a completeness note rather than a coverage claim. |
| `interactive-shell.ts`, `ssh.ts`, `mac-system-theme.ts`, `truncated-tool.ts` | `node:child_process` (+ `node:util`) | **Unblocked**: all four now have the builtins they import. `ssh.ts`'s timed/abortable path additionally needs the `setTimeout` global (engine-level, not builtin); `AbortSignal` is available since LUM-1116. |
| `auto-commit-on-exit.ts`, `border-status-editor.ts`, `dirty-repo-guard.ts`, `git-checkpoint.ts`, `github-issue-autocomplete.ts`, `inline-bash.ts`, `input-transform-streaming.ts`, `shutdown-command.ts` | — (shell out through the extension API) | **Unblocked**: these use `pi.exec`, which is now bridged to the Rust host (see [`EXTENSIONS.md`](EXTENSIONS.md#host-imports-rust--js)); they never import `node:child_process` themselves. |

The `@earendil-works/*` specifiers are pi's own APIs rather than Node
ones, so they have their own document: [`SDK_MODULES.md`](SDK_MODULES.md)
lists what each virtual module implements, which names are documented
gaps, and every divergence.

## Deliberate divergences from Node

| Divergence | Why |
|---|---|
| Error `message` text comes from Rust (`"ENOENT: No such file or directory (os error 2), open '/x'"`); `code` / `syscall` / `path` match Node. | Extensions branch on `code`/`syscall`; reproducing Node's exact `strerror` prose buys nothing. |
| `mkdirSync(path, {recursive: true})` returns `undefined`, Node returns the first created directory. | The bridge would need a second syscall per ancestor. |
| `accessSync` only checks existence. | See the table above. |
| `readdirSync` returns name-sorted entries. | Deterministic tests; Node's order is unspecified. |
| `copyFileSync` ignores `COPYFILE_EXCL` / `COPYFILE_FICLONE`. | Always a plain overwrite copy. |
| `fs.createReadStream` reads the whole file up front (`fs.readFile`) and replays it, instead of opening a descriptor and reading lazily: `fd` is `null`, `options.fd` is ignored, `highWaterMark` only sizes the byte-mode chunks, and `destroy()` emits `close` synchronously. | The bridge is blocking and hands back no descriptor; buffering is what lets a listener attached a tick later still see the data, which is the same contract `child.stdout` / `child.stderr` already document. |
| `fs.createWriteStream` buffers every chunk in memory and flushes once on `end()` (a single `fs.writeFile` / `fs.appendFile`), where Node opens a descriptor and writes incrementally: `write()` and its callback only mean "the bytes are buffered", `bytesWritten` counts accepted bytes, `write()` always returns `true` (so `drain` never fires), `open`/`ready` are announced optimistically (a flush failure arrives later as `error`), `finish` / the `end()` callback / `close` are the only "on disk" signals, only the `w` and `a` flags exist, and `cork`/`uncork` are no-ops. A failed flush reports through both the `error` event and the `end()` callback's argument. | The bridge takes the whole payload in one base64 hop and hands back no descriptor, so there is no incremental write to acknowledge and no backpressure to signal; buffering until `end()` is the write-side mirror of `ReadStream`'s replay. |
| `process.env` is a read-only snapshot; assigning to it stays in JS. | The bridge cannot mutate the host process environment. |
| `process.version` is `"v0.0.0-pi-rust"`, not a Node version. | There is no Node here; version-gated feature checks take the conservative branch. |
| `process.stdout.write` / `stderr.write` go to `host_log`, not the real streams. | The `pi` process owns stdout (`--rpc` speaks JSON-RPC on it); an extension must never write raw bytes there. `isTTY` is `false`. |
| Reading a file without an encoding returns a `Buffer` (correct), but `Buffer` lacks the numeric `readUInt32BE` / `writeUInt32LE` / `fill`-with-pattern families. | Out of the subset the ecosystem uses so far. |
| `util.inspect` renders everything on one line — `breakLength` / `compact` are accepted but ignored — and boxed primitives / Promises print as `Boolean {}` / `Promise { <pending> }` instead of Node's resolved-state form. | Extensions log the output; line wrapping buys nothing here. |
| `util.styleText` always emits ANSI codes; it does not consult `process.stdout.hasColors` (there is no TTY in the embedded engine). | Pass `{validateStream: false}` for Node-identical bytes; the codes are what the evals reporters consume. |
| `util.TextDecoder` ignores `{stream: true}` and `fatal: true`; decoding never throws on malformed input. | Streaming would need a per-instance byte buffer; non-fatal decoding matches Node's default. |
| `node:zlib`'s gzip/deflate encoder emits either stored blocks (`level: 0`) or a single final fixed-Huffman LZ77 block, instead of choosing per block between fixed and dynamic Huffman; the decoder handles all three block types. Levels 1–9 only change the greedy hash-chain search depth, so the ratio trails zlib's, and `gzipSync` writes fixed header fields (`MTIME = 0`). Async/callback forms, `unzipSync` header sniffing, stream constructors and options other than `level` are absent. The `ZSTD_error_*` code is recovered from zstd's error prose (a name the table does not know falls back to `ZSTD_error_GENERIC`). | The offline registry has no `flate2`/`miniz_oxide`, so a bundled dynamic-Huffman matcher was not worth the code when every decoder accepts fixed blocks; `zstd` additionally surfaces `ZSTD_getErrorName()` rather than the `ZSTD_ErrorCode` enum Node reports. Error *codes* still match Node (`Z_DATA_ERROR` / `Z_BUF_ERROR` / `Z_STREAM_ERROR`). |
| `kill()` on a `spawn`ed child always sends SIGKILL and ignores its signal argument; a `timeout` also reports `signal: "SIGKILL"`, where Node uses SIGTERM. The error still carries `code: "ETIMEDOUT"` / `status: null` / `killed: true`, like Node. | The host owns one kill primitive; a portable per-signal path would need `libc::kill` on unix and a Windows equivalent. |
| The buffered forms (`exec*` / `*Sync`) stop draining a pipe shortly after the direct child exits instead of waiting for EOF, so output written by a *grandchild* that inherited the pipe (`sh -c "sleep 5 & echo hi"`) is not waited for, and may be truncated. Node blocks until the pipe closes. | The reader threads are bounded so a grandchild cannot pin the host past its deadline. The direct child's own output is always captured: the reader only gives up after 250 ms of no EOF. |
| Every `node:child_process` child is bounded by the host per-call timeout (5 s normally, 300 s in interactive mode); a child that outlives it is killed and reported as `killed` / `signal: "SIGKILL"`. | This is what stops a runaway extension from hanging `pi` — the same ceiling `pi.exec` / `host_exec` enforce. Nothing in the shim can raise it: an `options.timeout` on the buffered forms only kills the child *earlier*, where `pi.exec`'s explicit `options.timeout` raises the host ceiling for that call (LUM-1116). |
| `spawn`'s `options.signal` and the `AbortSignal` families of the buffered forms are ignored. | `pi.exec` honours `options.signal` (LUM-1116) through a dedicated host cancel op; the `node:child_process` bridge has no per-call cancel channel yet. `AbortController` / `AbortSignal` themselves *are* available (polyfilled by the shim), so an extension can still `kill()` on abort itself. |
| `spawn` pipes are fully buffered host-side; `maxBuffer` does not apply to `spawn` (only to the buffered forms), and there is no implicit 1 MiB default anywhere. | The reader tasks must always drain so the child never blocks on a full pipe; enforcing a cap on a live stream would mean dropping bytes an extension can still observe. |
| `stdio: "inherit"` replays the captured output through the log-backed `process.stdout` / `stderr` after the child exits, instead of handing the real terminal to the child. | The `pi` process owns stdout (`--rpc`); a raw passthrough would corrupt the protocol. `spawnSync(…, {stdio: "inherit"})` therefore returns `stdout: null`, like Node. |
| Writing to `child.stdin` is not supported (`spawn`'s stdin is `/dev/null`) and `detached` is ignored. | The host bridges no stdin channel; `detached` would need a process-group model the host does not have. |
| `readline` splits lines immediately instead of waiting out Node's 100 ms `crlfDelay`, so a bare `\r` is a break at once. | The host has no timer surface for the shim to schedule the delay on, and immediate splitting is what every reader actually wants. |
| `readline.createInterface()` with no `input` asks through the session UI dialog instead of Node's `process.stdin`, and `terminal: true` throws `ERR_READLINE_TTY_UNSUPPORTED`. | There is no TTY in the embedded engine and the host owns stdin; a raw-mode interface that can never read a keystroke would be a lie, and the UI dialog is the real prompt channel (`ctx.ui.input`). |
| `module.createRequire` resolves only the bridged virtual modules — a path that exists on disk still throws `MODULE_NOT_FOUND` — and `builtinModules` / `isBuiltin` report the bridged set (`isBuiltin("node:stream")` is `false`). | The virtual-module boundary is the sandbox; reporting Node's full builtin list would promise resolutions that fail. |

## Not bridged (frontier)

Importing these modules fails with the readable error
`unsupported import "node:x" in pi extension: available virtual modules are …`.
A row that names a *family* (`crypto.createHmac`, `crypto.createHash`)
means the module imports fine but those names are absent, so the failure is a
plain "undefined is not a function":

| Builtin | Why it is missing | What it would take |
|---|---|---|
| `node:module`'s disk resolution (`require` of a real path, `registerHooks`, `findSourceMap`) | The virtual-module sandbox deliberately stops at the bridged set; `createRequire` / `Module` / `builtinModules` themselves **are** bridged (LUM-1129). | A deliberate decision to widen the sandbox (e.g. require-from-`node_modules`-only); `doom-overlay/doom-engine.ts` would then load its local CJS blob. |
| `node:stream` / `node:http` / `node:net` / `node:worker_threads` | No event loop integration for streams. | Substantial; probably out of scope for the QuickJS host. |
| `crypto.createHmac` / key-based WebCrypto (`importKey`, `sign`, `encrypt`, …) / algorithms other than SHA-1 + SHA-256 | Only one-shot digests **and HMAC** are bridged (`crypto.digest` / `crypto.hmac`); `createHmac` was closed by LUM-1181. The remaining key-based WebCrypto surface needs a key-object model and a cipher backend the workspace does not bundle, and a wrong result would be worse than a clear failure. | Add the key-object model plus a cipher backend (or a crate that provides one) when an extension actually imports/signs with keys. |
| `fs.watch` | The two `fs` stream halves are bridged — `createReadStream` (LUM-1172) as a buffered replay of `fs.readFile`, `createWriteStream` (LUM-1179) as a buffer-until-`end` `Writable` over `fs.writeFile`/`fs.appendFile` — so this row is down to watching alone: the host has no filesystem watcher. | A `notify`-based bridge op, or a polling watcher over the existing `fs.stat`. |
| `os.cpus()`, `os.totalmem()`, `os.networkInterfaces()` | Machine topology has no consumer yet; inventing numbers would be worse than failing. | Straightforward `sysinfo`-style additions when needed. |
| `process.argv`, `process.execPath`, `process.stdin`, `process.kill` | The host owns the process; extensions must not steer it. | Probably never. |
| `node:test`, `node:assert` global | Engine-level globals QuickJS does not ship (`TextEncoder` / `TextDecoder` are now polyfilled from `node:util` and installed globally, `AbortController` / `AbortSignal` by the extension shim for `pi.exec` cancellation — see [`EXTENSIONS.md`](EXTENSIONS.md#host-imports-rust--js)), and `atob` / `btoa` / `crypto` / `URLSearchParams` / `URL` are polyfilled by the shim now (LUM-1159, LUM-1177) — see [Globals](#globals). | `node:test` is a whole runner and `assert` is a deep-equality library; neither is on an extension's critical path. `fetch` **is** bridged (LUM-1135): `fetch` / `Headers` / `Request` / `Response` are backed by the `host_fetch` import over the host's `reqwest` stack, so the repo's own `.pi/extensions/import-repro.ts` runs (see [`EXTENSIONS.md`](EXTENSIONS.md#fetch-global)). |

The upstream examples are the compatibility yardstick: the test
`upstream_node_imports_are_all_bridged_or_documented`
(`tests/node_builtins.rs`) scans `.pi/extensions/` and
`packages/coding-agent/examples/extensions/` for `node:*` imports and
fails unless every specifier is either bridged or listed above — so this
table and the shim cannot drift apart silently.

Two rows this table used to carry are now closed: `node:zlib`'s
gzip/deflate family (LUM-1131, see the `node:zlib` section) and the zstd
family (LUM-1125). `crypto.createHmac` (LUM-1181) is closed too — only the
key-based WebCrypto half of that row remains, and it is now listed on its
own.

## Adding a new op

1. `crates/pi-extensions/src/host.rs` — for a module that needs the
   operating system, add an arm to `node_call(op, args)`, returning a JSON
   value or `NodeError::io(..)`/`NodeError::invalid(..)`. A module that is
   implementable in JS (`node:util`, `node:module`, `node:readline`) needs no
   arm at all — do not add one just to have one.
2. `crates/pi-extensions/runtime/pi-ext-shim.mjs` — wrap it in the matching
   module object (`__pi_fs_module`, `__pi_os_module`, …) and add the
   specifier to `globalThis.__pi_virtual_modules` if it is a new module.
3. `crates/pi-extensions/tests/node_builtins.rs` — extend the probe tool in
   the relevant test; assert on the returned `details` object and, where it
   matters, on the real filesystem afterwards.
4. Update the tables in this file.
