// @ts-check
//
// pi extension shim — loaded once into the embedded QuickJS context.
//
// Shape mirrors `ExtensionAPI` in
// `packages/coding-agent/src/core/extensions/types.ts`. Each method that
// would talk back to the host calls a host import (set as a global
// function). Event handlers are stored locally; the host invokes
// `_pi_dispatch(eventJson)` to fan out events.
//
// Host imports (set on the JS global by `JsExtensionHost::new`):
//   - `host_register_tool(json)`         — register a tool definition
//   - `host_register_command(json)`      — register a slash command
//   - `host_append_entry(type, dataJson)`— append a custom session entry
//   - `host_send_message(json)`          — enqueue a custom message
//   - `host_send_user_message(json)`     — enqueue a user message
//   - `host_set_session_name(name)`      — set the session display name
//   - `host_ui_notify(message, level)`   — fire-and-forget notification
//   - `host_ui_confirm(title, body)`     — return Promise<bool>
//   - `host_ui_input(title, placeholder)`— return Promise<string|null>
//   - `host_ui_select(title, optionsJson)`— return Promise<string|null>
//   - `host_log(level, message)`         — surface a log line
//
// Internal entry points exposed on `globalThis._pi_host`:
//   - `_pi_dispatch(eventJson)`   — deliver an `ExtensionEvent` payload
//     to subscribed handlers; returns a JSON string with the dispatch
//     summary so the host can inspect what ran.
//   - `_pi_registered_tools()`    — list tool names registered so far.
//   - `_pi_registered_tool_prompts()` — list each tool's `promptSnippet`
//     / `promptGuidelines` contribution for the system prompt.
//   - `_pi_registered_commands()` — list slash commands registered so far.
//   - `_pi_execute_command(name, args, ctxJson)` — run a command handler.
//   - `_pi_known_event_names()`   — list event names with at least one
//     subscriber.
//   - `_pi_load_extension(source, path)` — evaluate an extension source
//     and call its default export with `pi`. Two module formats are
//     accepted, mirroring what upstream `jiti.import` handles:
//       * CommonJS-style `module.exports = function (pi) { ... }`
//         (the shape the upstream TS source compiles to), and
//       * ESM `import { ... } from "node:path"` + `export default
//         function (pi) { ... }` (the upstream source form). `path` is
//         the extension's file path; it backs `import.meta.url` and is
//         used for readable error messages.
//   - `host_node_call(op, argsJson)` — the single bridge behind the
//     `node:*` virtual modules (fs / os / buffer / crypto / process).
//     Returns a JSON envelope, never throws; see the `node:*` section
//     near the bottom of this file.

const _pi = {
  /** @type {Record<string, Array<(event: any, ctx: any) => any>>} */
  handlers: {},
  /** @type {Map<string, {name:string,label:string,description:string,parameters:any}>} */
  tools: new Map(),
  /** @type {Map<string, {name:string,description?:string}>} */
  commands: new Map(),
  loadedCount: 0,
};

/**
 * Build the ctx object passed to event handlers. Mirrors
 * `ExtensionContext` from the TS API at minimum surface — Stage 3
 * keeps the shape minimal and grows it in later stages.
 */
function buildCtx(extra) {
  const ctxMode = (extra && extra.mode) || "print";
  const hasUI = extra && typeof extra.hasUI === "boolean" ? extra.hasUI : false;
  const cwd = (extra && extra.cwd) || "";
  return Object.freeze({
    mode: ctxMode,
    hasUI,
    cwd,
    ui: makeUiContext(hasUI),
    isIdle: () => true,
    isProjectTrusted: () => true,
    hasPendingMessages: () => false,
    getSystemPrompt: () => "",
    abort: () => {},
    shutdown: () => {},
    signal: undefined,
  });
}

/**
 * Build the `ui` sub-context. Each method either fires a host import
 * (notify) or returns a Promise that resolves when the host answers
 * (confirm / input / select). When `hasUI` is false, the Promise-based
 * methods return a sensible default (false / null / null) without
 * contacting the host.
 */
function makeUiContext(hasUI) {
  /**
   * Non-interactive modes (print / rpc / no TTY) answer a UI request
   * immediately instead of blocking the caller. Emit a warning notify
   * so the denial is visible to the user and to the extension instead
   * of being silent — an RPC client must never see a UI request hang.
   */
  function reportDenied(kind, title) {
    if (typeof globalThis.host_ui_notify !== "function") {
      return;
    }
    try {
      globalThis.host_ui_notify(
        "ctx.ui." + kind + (title ? " (\"" + String(title) + "\")" : "") +
          " denied: no interactive UI in this mode",
        "warning",
      );
    } catch (_e) {
      // Swallow — notify is fire-and-forget.
    }
  }
  return Object.freeze({
    notify(message, level) {
      if (typeof globalThis.host_ui_notify === "function") {
        try {
          globalThis.host_ui_notify(String(message), String(level || "info"));
        } catch (_e) {
          // Swallow — host notify is fire-and-forget.
        }
      }
    },
    async confirm(title, body) {
      if (!hasUI || typeof globalThis.host_ui_confirm !== "function") {
        reportDenied("confirm", title);
        return false;
      }
      try {
        const out = await globalThis.host_ui_confirm(String(title), String(body));
        return Boolean(out);
      } catch (_e) {
        return false;
      }
    },
    async input(title, placeholder) {
      if (!hasUI || typeof globalThis.host_ui_input !== "function") {
        reportDenied("input", title);
        return null;
      }
      try {
        const out = await globalThis.host_ui_input(String(title), placeholder == null ? "" : String(placeholder));
        return typeof out === "string" ? out : null;
      } catch (_e) {
        return null;
      }
    },
    async select(title, options) {
      if (!hasUI || typeof globalThis.host_ui_select !== "function") {
        reportDenied("select", title);
        return null;
      }
      const opts = Array.isArray(options) ? options.map((o) => String(o)) : [];
      try {
        const out = await globalThis.host_ui_select(String(title), JSON.stringify(opts));
        return typeof out === "string" ? out : null;
      } catch (_e) {
        return null;
      }
    },
  });
}

/** The `pi` object extensions see — mirrors `ExtensionAPI`. */
const pi = Object.freeze({
  /**
   * Subscribe to an event. `eventName` must be one of the event names
   * the host recognizes (e.g. `session_start`, `agent_start`).
   */
  on(eventName, handler) {
    if (typeof eventName !== "string") {
      throw new TypeError("pi.on: event name must be a string");
    }
    if (typeof handler !== "function") {
      throw new TypeError("pi.on: handler must be a function");
    }
    const list = _pi.handlers[eventName] || (_pi.handlers[eventName] = []);
    list.push(handler);
  },

  /** Register a tool the LLM can invoke. */
  registerTool(definition) {
    if (!definition || typeof definition !== "object") {
      throw new TypeError("pi.registerTool: definition must be an object");
    }
    const name = definition.name;
    const label = definition.label;
    const description = definition.description;
    const parameters = definition.parameters;
    if (typeof name !== "string" || !name) {
      throw new TypeError("pi.registerTool: definition.name must be a non-empty string");
    }
    if (typeof label !== "string") {
      throw new TypeError("pi.registerTool: definition.label must be a string");
    }
    if (typeof description !== "string") {
      throw new TypeError("pi.registerTool: definition.description must be a string");
    }
    if (typeof parameters !== "object" || parameters === null) {
      throw new TypeError("pi.registerTool: definition.parameters must be an object (JSON Schema)");
    }
    if (typeof definition.execute !== "function") {
      throw new TypeError("pi.registerTool: definition.execute must be a function");
    }
    const paramsJson = JSON.stringify(parameters);
    const promptSnippet =
      typeof definition.promptSnippet === "string" ? definition.promptSnippet : undefined;
    const promptGuidelines = Array.isArray(definition.promptGuidelines)
      ? definition.promptGuidelines.filter((line) => typeof line === "string")
      : [];
    _pi.tools.set(name, {
      name,
      label,
      description,
      parameters: paramsJson,
      promptSnippet,
      promptGuidelines,
      _exec: definition.execute,
    });
    if (typeof globalThis.host_register_tool === "function") {
      globalThis.host_register_tool(
        JSON.stringify({
          name,
          label,
          description,
          parameters: JSON.parse(paramsJson),
        }),
      );
    }
  },

  /** Register a slash command the user can invoke. */
  registerCommand(name, options) {
    if (typeof name !== "string" || !name) {
      throw new TypeError("pi.registerCommand: name must be a non-empty string");
    }
    const description =
      options && typeof options.description === "string" ? options.description : "";
    const handler =
      options && typeof options.handler === "function" ? options.handler : null;
    _pi.commands.set(name, { name, description, _handler: handler });
    if (typeof globalThis.host_register_command === "function") {
      globalThis.host_register_command(JSON.stringify({ name, description }));
    }
  },

  /** Append a custom entry to the session for state persistence. */
  appendEntry(customType, data) {
    if (typeof customType !== "string" || !customType) {
      throw new TypeError("pi.appendEntry: customType must be a non-empty string");
    }
    const dataJson = data === undefined ? "null" : JSON.stringify(data);
    if (typeof globalThis.host_append_entry === "function") {
      globalThis.host_append_entry(customType, dataJson);
    }
  },

  /** Send a custom message to the session. */
  sendMessage(message) {
    if (typeof globalThis.host_send_message === "function") {
      globalThis.host_send_message(JSON.stringify(message ?? null));
    }
  },

  /** Send a user message — host queues it as a turn input. */
  sendUserMessage(text) {
    if (typeof globalThis.host_send_user_message === "function") {
      globalThis.host_send_user_message(JSON.stringify(text ?? ""));
    }
  },

  /** Set the session display name. */
  setSessionName(name) {
    if (typeof globalThis.host_set_session_name === "function") {
      globalThis.host_set_session_name(String(name == null ? "" : name));
    }
  },

  /**
   * Run a command and resolve with `{ stdout, stderr, code, killed }` —
   * the upstream `ExtensionAPI.exec` contract
   * (`packages/coding-agent/src/core/exec.ts`). The command is spawned
   * directly, never through a shell, so arguments with spaces / globs are
   * passed verbatim (mirrors upstream `spawn(..., { shell: false })`).
   *
   * `options.cwd` defaults to the session working directory; `timeout` is
   * in milliseconds. `options.signal` is accepted but ignored (QuickJS has
   * no `AbortSignal`), and every call is additionally bounded by the
   * host's per-call timeout (5 s by default, 300 s in interactive mode).
   * Like upstream, the returned promise resolves for every outcome — a
   * missing binary yields `{ code: 1, stderr: "…" }` rather than a
   * rejection.
   */
  async exec(command, args, options) {
    if (typeof command !== "string" || !command) {
      throw new TypeError("pi.exec: command must be a non-empty string");
    }
    const argv = args == null ? [] : args;
    if (!Array.isArray(argv) || argv.some((arg) => typeof arg !== "string")) {
      throw new TypeError("pi.exec: args must be an array of strings");
    }
    const opts = options && typeof options === "object" ? options : {};
    const cwd = typeof opts.cwd === "string" && opts.cwd ? opts.cwd : globalThis._pi_cwd;
    const timeout =
      typeof opts.timeout === "number" && opts.timeout > 0 ? opts.timeout : undefined;
    if (typeof globalThis.host_exec !== "function") {
      throw new Error("pi.exec is not available in this host build");
    }
    const raw = await globalThis.host_exec(
      command,
      JSON.stringify({ args: argv, cwd: cwd, timeout: timeout }),
    );
    let result;
    try {
      result = typeof raw === "string" ? JSON.parse(raw) : raw;
    } catch (_e) {
      throw new Error("pi extension host returned malformed JSON for `exec`");
    }
    return {
      stdout: result && typeof result.stdout === "string" ? result.stdout : "",
      stderr: result && typeof result.stderr === "string" ? result.stderr : "",
      code: result && typeof result.code === "number" ? result.code : 0,
      killed: Boolean(result && result.killed),
    };
  },
});

// Expose `pi` globally so extension source can call it without
// importing. The TS API exposes it as the factory parameter; the shim
// also surfaces it as a global to keep extension source compatible
// with either pattern (`export default function (pi) {}`).
globalThis.pi = pi;
globalThis._pi_host = { _pi, pi };

/**
 * Deliver an event payload (JSON string) to subscribed handlers.
 * Returns a JSON string of `{ handled, subscribers, results, errored }`
 * so the host can inspect what the extension actually did.
 *
 * Async handlers are awaited; the host sees the resolved value
 * once the JS-side promise settles.
 *
 * @param {string} eventJson
 */
globalThis._pi_dispatch = async function _pi_dispatch(eventJson) {
  let parsed;
  try {
    parsed = typeof eventJson === "string" ? JSON.parse(eventJson) : eventJson;
  } catch (e) {
    return JSON.stringify({
      handled: false,
      subscribers: 0,
      results: [],
      errored: { message: "malformed event JSON: " + (e && e.message ? e.message : String(e)) },
    });
  }
  if (!parsed || typeof parsed !== "object" || typeof parsed.type !== "string") {
    return JSON.stringify({
      handled: false,
      subscribers: 0,
      results: [],
      errored: { message: "event missing `type`" },
    });
  }
  const handlers = _pi.handlers[parsed.type] || [];
  const ctx = buildCtx({
    mode: parsed._ctx_mode,
    hasUI: parsed._ctx_hasUI,
    cwd: parsed._ctx_cwd,
  });
  /** @type {any[]} */
  const results = [];
  let errored = null;
  for (const h of handlers) {
    let result;
    try {
      result = h(parsed, ctx);
    } catch (e) {
      errored = { message: e && e.message ? e.message : String(e) };
      break;
    }
    if (result && typeof result.then === "function") {
      try {
        const awaited = await result;
        results.push(awaited == null ? null : awaited);
      } catch (e) {
        errored = { message: e && e.message ? e.message : String(e) };
        break;
      }
    } else {
      results.push(result == null ? null : result);
    }
  }
  return JSON.stringify({
    handled: handlers.length > 0,
    subscribers: handlers.length,
    results,
    errored,
  });
};

globalThis._pi_registered_tools = function _pi_registered_tools() {
  const names = [];
  for (const name of _pi.tools.keys()) names.push(name);
  return JSON.stringify({ tools: names });
};

/**
 * List the prompt contributions (snippet + guidelines) of every tool
 * registered so far. Returns a JSON string with
 * `{ tools: [{ name, snippet, guidelines }] }`; `snippet` is `null` when
 * the extension did not declare `promptSnippet`. The host uses this to
 * describe extension tools in the system prompt.
 */
globalThis._pi_registered_tool_prompts = function _pi_registered_tool_prompts() {
  const tools = [];
  for (const tool of _pi.tools.values()) {
    tools.push({
      name: tool.name,
      snippet: typeof tool.promptSnippet === "string" ? tool.promptSnippet : null,
      guidelines: Array.isArray(tool.promptGuidelines) ? tool.promptGuidelines : [],
    });
  }
  return JSON.stringify({ tools });
};

globalThis._pi_known_event_names = function _pi_known_event_names() {
  return JSON.stringify({ events: Object.keys(_pi.handlers) });
};

/**
 * List the slash commands registered so far, in registration order.
 * Returns a JSON string with `{ commands: [{ name, description }] }`.
 * The host uses this as the source of truth for `/help` and for
 * deciding whether a typed `/name` belongs to an extension.
 */
globalThis._pi_registered_commands = function _pi_registered_commands() {
  const commands = [];
  for (const cmd of _pi.commands.values()) {
    commands.push({ name: cmd.name, description: cmd.description || "" });
  }
  return JSON.stringify({ commands });
};

/**
 * Invoke a registered command handler. The upstream signature is
 * `handler(args: string, ctx: ExtensionCommandContext)`, so the raw
 * argument string is forwarded verbatim and the host supplies the
 * same `ctx` shape event handlers get.
 *
 * Resolves to a JSON string with `{ handled, isError, result, error }`:
 * `handled` is false when no command of that name exists (the caller
 * can then fall through to its own "unknown command" path), and
 * `result` carries the handler's return value normalised to JSON
 * (`null` for `undefined`) so a command can echo text back.
 *
 * @param {string} name
 * @param {string} args
 * @param {string} ctxJson
 */
globalThis._pi_execute_command = function _pi_execute_command(name, args, ctxJson) {
  const entry = _pi.commands.get(String(name));
  if (!entry || typeof entry._handler !== "function") {
    return JSON.stringify({ handled: false, isError: false, result: null, error: null });
  }
  let ctxExtra = {};
  try {
    ctxExtra = ctxJson ? JSON.parse(String(ctxJson)) : {};
  } catch (_e) {
    ctxExtra = {};
  }
  const ctx = buildCtx(ctxExtra);
  const argsText = args == null ? "" : String(args);

  const stringify = (isError, result, error) => {
    let json;
    try {
      json = JSON.stringify({ handled: true, isError, result, error });
    } catch (e) {
      json = JSON.stringify({
        handled: true,
        isError: true,
        result: null,
        error: "command result is not JSON-serializable: " + (e && e.message ? e.message : String(e)),
      });
    }
    return json === undefined
      ? JSON.stringify({ handled: true, isError, result: null, error })
      : json;
  };

  const finalize = (value) => {
    if (value === undefined) return stringify(false, null, null);
    if (typeof value === "function") return stringify(false, null, null);
    return stringify(false, value, null);
  };

  let result;
  try {
    result = entry._handler(argsText, ctx);
  } catch (e) {
    return stringify(true, null, e && e.message ? e.message : String(e));
  }
  if (result && typeof result.then === "function") {
    return result
      .then(finalize)
      .catch((e) => stringify(true, null, e && e.message ? e.message : String(e)));
  }
  return finalize(result);
};

/**
 * Execute a previously registered tool by name. Used by the host when
 * the LLM dispatches a tool call. Resolves to a JSON string with
 * `{ content, details, isError }` matching the wire shape on
 * `ToolResult`.
 *
 * The JS-side tool callback receives `(args, ctx)` where `args` is the
 * parsed arguments object and `ctx` is a minimal ExecutionContext.
 *
 * @param {string} name
 * @param {string} argsJson
 */
globalThis._pi_execute_tool = function _pi_execute_tool(name, argsJson) {
  const entry = _pi.tools.get(String(name));
  if (!entry) {
    return JSON.stringify({
      isError: true,
      content: [{ type: "text", text: "unknown tool: " + String(name) }],
      details: null,
    });
  }
  let args;
  try {
    args = argsJson == null ? {} : typeof argsJson === "string" ? JSON.parse(argsJson) : argsJson;
  } catch (e) {
    return JSON.stringify({
      isError: true,
      content: [{ type: "text", text: "invalid tool args: " + (e && e.message ? e.message : String(e)) }],
      details: null,
    });
  }
  // The host installs `_pi_tool_ctx` (mode / hasUI / cwd) once at
  // startup so a tool's `execute(args, ctx)` sees the same session
  // context events and commands get — `ctx.hasUI` decides whether
  // `ctx.ui.confirm` can prompt.
  let toolCtx = { mode: "print", hasUI: false, cwd: "" };
  try {
    if (typeof globalThis._pi_tool_ctx === "string") {
      toolCtx = JSON.parse(globalThis._pi_tool_ctx);
    }
  } catch (_e) {
    // Keep the non-interactive default.
  }
  const ctx = buildCtx(toolCtx);
  let result;
  try {
    result = entry._exec(args, ctx);
  } catch (e) {
    return JSON.stringify({
      isError: true,
      content: [{ type: "text", text: e && e.message ? e.message : String(e) }],
      details: null,
    });
  }
  const finalize = (r) => {
    if (!r || typeof r !== "object") {
      return JSON.stringify({
        isError: false,
        content: [{ type: "text", text: "" }],
        details: null,
      });
    }
    const content = Array.isArray(r.content) ? r.content : [];
    const details = r.details === undefined ? null : r.details;
    const isError = r.isError === true;
    return JSON.stringify({ isError, content, details });
  };
  if (result && typeof result.then === "function") {
    return result.then(finalize).catch((e) => {
      return JSON.stringify({
        isError: true,
        content: [{ type: "text", text: e && e.message ? e.message : String(e) }],
        details: null,
      });
    });
  }
  return finalize(result);
};

/**
 * Evaluate extension source and call its default export with `pi`.
 *
 * Two module formats are accepted, matching what upstream `jiti.import`
 * handles:
 *
 *   1. CommonJS: `module.exports = function (pi) { ... }` — the shape
 *      upstream TS source compiles to.
 *   2. ESM: `import { join } from "node:path"; export default function
 *      (pi) { ... }` — the upstream source form.
 *
 * The ESM branch rewrites the `import` / `export default` statements and
 * `import.meta` references in place (see `__pi_analyze_module`) and then
 * runs the result through the same factory wrapper. No bundler and no
 * extra JS dependency is involved; the `node:path` / `node:url` /
 * `node:fs` / `node:fs/promises` / `node:os` / `node:buffer` /
 * `node:crypto` / `node:process` and `typebox` / `@sinclair/typebox`
 * slice of the upstream `VIRTUAL_MODULES` map is provided.
 *
 * @param {string} source extension source text
 * @param {string} [path] absolute path of the extension file; backs
 *   `import.meta.url` and appears in error messages
 */
globalThis._pi_load_extension = function _pi_load_extension(source, path) {
  if (typeof source !== "string") {
    throw new TypeError("_pi_load_extension: source must be a string");
  }
  const sourcePath = typeof path === "string" ? path : "";
  const label = sourcePath || "<inline extension>";
  const analysis = __pi_analyze_module(source);

  let body;
  let format;
  if (analysis.isEsm) {
    if (!analysis.hasDefaultExport) {
      throw new Error(
        label +
          ": ESM extension must `export default` a factory function, e.g. `export default function (pi) { ... }`",
      );
    }
    body =
      "let __pi_default_export;\n" +
      __pi_apply_edits(source, analysis.edits) +
      "\n;return __pi_default_export;";
    format = "esm";
  } else {
    body =
      source +
      "\n;return (typeof module.exports === 'function') ? module.exports : ((typeof exports === 'function') ? exports : undefined);";
    format = "cjs";
  }

  let factoryFn;
  try {
    factoryFn = new Function(
      "module",
      "exports",
      "pi",
      "__pi_import",
      "__pi_meta",
      "require",
      body,
    );
  } catch (e) {
    throw new Error(
      label + ": failed to compile extension: " + (e && e.message ? e.message : String(e)),
    );
  }
  const module = { exports: undefined };
  const exports = {};
  // `require` is the CJS twin of the ESM `import` rewrite: upstream TS
  // extensions compiled to CJS (`require("node:fs")`) resolve through
  // the same virtual module map, so both module formats see the same
  // builtin surface.
  const factory = factoryFn(
    module,
    exports,
    pi,
    __pi_import,
    __pi_import_meta(sourcePath),
    __pi_require,
  );
  if (typeof factory !== "function") {
    throw new Error(label + ": extension did not export a factory function");
  }
  factory(pi);
  _pi.loadedCount += 1;
  return JSON.stringify({ loaded: true, format: format });
};

// ---------------------------------------------------------------------------
// ESM support.
//
// The embedded QuickJS runtime has no Node builtins and this shim stays
// dependency-free, so the ESM contract is implemented as a *source
// rewrite*: `import`/`export default`/`import.meta` are translated into
// plain statements before the factory wrapper evaluates the module.
//
// The rewrite is driven by a token scan of a masked copy of the source
// (strings, template literals, comments and regex literals are blanked),
// so a keyword inside a string or comment is never mistaken for a
// statement. Only the forms the upstream extension contract uses are
// supported:
//
//   import "node:path";
//   import path from "node:path";
//   import * as path from "node:path";
//   import { dirname, join } from "node:path";
//   import { join as j } from "node:path";
//   import def, { a } from "node:path";
//   import type { Foo } from "...";           // erased (TS type-only)
//   export default function (pi) { ... }
//   export default (pi) => { ... }
//   import.meta.url / import.meta.dirname / import.meta.filename
//
// Anything else fails with a readable error instead of running partial
// code. The `@earendil-works/*` virtual modules stay out of scope
// (separate task); importing them names the specifier and the supported
// set.
// ---------------------------------------------------------------------------

/**
 * The virtual modules available to ESM extensions. Mirrors the
 * `node:path` / `node:url` / `typebox` slice of the upstream
 * `VIRTUAL_MODULES` map in
 * `packages/coding-agent/src/core/extensions/loader.ts`. Assigned at the
 * bottom of this file, after the module objects are built.
 */

/**
 * Resolve a rewritten `import ... from "<specifier>"`.
 *
 * Relative specifiers and anything outside the virtual module map fail
 * with a readable error: silently evaluating `undefined` would surface
 * much later as a confusing TypeError inside extension code.
 *
 * @param {string} specifier
 */
function __pi_import(specifier) {
  const key = String(specifier);
  const mod = globalThis.__pi_virtual_modules[key];
  if (mod) return mod;
  if (key.startsWith(".") || key.startsWith("/")) {
    throw new Error(
      'relative import "' +
        key +
        '" is not supported by the pi extension host: bundle the helper into the extension file',
    );
  }
  throw new Error(
    'unsupported import "' +
      key +
      '" in pi extension: available virtual modules are ' +
      Object.keys(globalThis.__pi_virtual_modules).join(", "),
  );
}

/**
 * CommonJS `require` — the same virtual module resolution as the ESM
 * `import` rewrite, so both module formats reach the same builtins.
 *
 * @param {string} specifier
 */
function __pi_require(specifier) {
  return __pi_import(specifier);
}

/**
 * Build the object exposed to the rewritten code as `import.meta`.
 *
 * `url` is `pathToFileURL(<extension path>)` so the canonical upstream
 * pattern `dirname(fileURLToPath(import.meta.url))` yields the
 * extension's own directory.
 *
 * @param {string} path
 */
function __pi_import_meta(path) {
  if (!path) {
    return Object.freeze({ url: "", dirname: "", filename: "" });
  }
  let href = "";
  try {
    href = __pi_url_module.pathToFileURL(path).href;
  } catch (_e) {
    href = "file://" + path;
  }
  let dir = "";
  try {
    dir = __pi_path_module.dirname(path);
  } catch (_e) {
    dir = "";
  }
  return Object.freeze({ url: href, dirname: dir, filename: path });
}

/** Apply the analyzer's edits back-to-front so offsets stay valid. */
function __pi_apply_edits(source, edits) {
  let code = source;
  for (let i = edits.length - 1; i >= 0; i--) {
    const edit = edits[i];
    code = code.slice(0, edit.start) + edit.text + code.slice(edit.end);
  }
  return code;
}

/**
 * True when a `/` at this point starts a regex literal rather than a
 * division. `lastCode` is the previous significant character (or "w" for
 * an identifier, with the identifier text in `lastWord`).
 */
function __pi_regex_allowed(lastCode, lastWord) {
  if (lastCode === "w") {
    return (
      [
        "return",
        "typeof",
        "instanceof",
        "in",
        "of",
        "new",
        "delete",
        "void",
        "throw",
        "case",
        "do",
        "else",
        "yield",
        "await",
      ].indexOf(lastWord) !== -1
    );
  }
  if (!lastCode) return true;
  return "([{,;=:!&|?+-*%^~<>".indexOf(lastCode) !== -1;
}

/**
 * Blank string / template-literal bodies, comments and regex literals
 * while preserving every character offset. Keywords hidden in those
 * regions therefore never reach the statement scanner.
 *
 * @param {string} source
 */
function __pi_mask_source(source) {
  const out = source.split("");
  const n = source.length;
  const blank = (start, end) => {
    for (let k = start; k < end; k++) {
      if (source[k] !== "\n") out[k] = " ";
    }
  };
  let lastCode = "";
  let lastWord = "";
  let i = 0;
  while (i < n) {
    const c = source[i];
    // Line comment.
    if (c === "/" && source[i + 1] === "/") {
      const start = i;
      while (i < n && source[i] !== "\n") i++;
      blank(start, i);
      continue;
    }
    // Block comment.
    if (c === "/" && source[i + 1] === "*") {
      const start = i;
      i += 2;
      while (i < n && !(source[i] === "*" && source[i + 1] === "/")) i++;
      i = Math.min(n, i + 2);
      blank(start, i);
      continue;
    }
    // String literal: blank the body, keep the quotes.
    if (c === "'" || c === '"') {
      const quote = c;
      const start = i;
      i++;
      while (i < n) {
        if (source[i] === "\\") {
          i += 2;
          continue;
        }
        if (source[i] === quote || source[i] === "\n") {
          i++;
          break;
        }
        i++;
      }
      blank(start + 1, Math.max(start + 1, i - 1));
      lastCode = "v";
      lastWord = "";
      continue;
    }
    // Template literal: blank it whole, backticks included.
    if (c === "`") {
      const start = i;
      i++;
      while (i < n) {
        if (source[i] === "\\") {
          i += 2;
          continue;
        }
        if (source[i] === "`") {
          i++;
          break;
        }
        i++;
      }
      blank(start, i);
      lastCode = "v";
      lastWord = "";
      continue;
    }
    // Regex literal (only where a value cannot precede it).
    if (c === "/" && __pi_regex_allowed(lastCode, lastWord)) {
      let j = i + 1;
      let inClass = false;
      let closed = false;
      while (j < n) {
        const d = source[j];
        if (d === "\\") {
          j += 2;
          continue;
        }
        if (d === "\n") break;
        if (d === "[") inClass = true;
        else if (d === "]") inClass = false;
        else if (d === "/" && !inClass) {
          closed = true;
          break;
        }
        j++;
      }
      if (closed) {
        const start = i;
        i = j + 1;
        while (i < n && /[a-z]/i.test(source[i])) i++;
        blank(start, i);
        lastCode = "v";
        lastWord = "";
        continue;
      }
    }
    if (/[A-Za-z_$]/.test(c)) {
      const start = i;
      i++;
      while (i < n && /[A-Za-z0-9_$]/.test(source[i])) i++;
      lastCode = "w";
      lastWord = source.slice(start, i);
      continue;
    }
    if (!/\s/.test(c)) {
      lastCode = c;
      lastWord = "";
    }
    i++;
  }
  return out.join("");
}

/**
 * Tokenize the masked source. Offsets point into the original source
 * too, because masking preserves length.
 */
function __pi_tokenize(masked) {
  const tokens = [];
  const n = masked.length;
  let i = 0;
  while (i < n) {
    const c = masked[i];
    if (c === "'" || c === '"') {
      const start = i;
      i++;
      while (i < n && masked[i] !== c && masked[i] !== "\n") i++;
      if (i < n && masked[i] === c) i++;
      tokens.push({ kind: "string", start: start, end: i });
      continue;
    }
    if (/[A-Za-z_$]/.test(c)) {
      const start = i;
      i++;
      while (i < n && /[A-Za-z0-9_$]/.test(masked[i])) i++;
      tokens.push({ kind: "ident", start: start, end: i });
      continue;
    }
    if (/[0-9]/.test(c)) {
      const start = i;
      i++;
      while (i < n && /[A-Za-z0-9_.$]/.test(masked[i])) i++;
      tokens.push({ kind: "number", start: start, end: i });
      continue;
    }
    if (/\s/.test(c)) {
      i++;
      continue;
    }
    tokens.push({ kind: "punct", start: i, end: i + 1 });
    i++;
  }
  return tokens;
}

/** Evaluate a JS string literal (single / double quoted, with escapes). */
function __pi_read_string_literal(literal) {
  const value = new Function("return (" + literal + ");")();
  if (typeof value !== "string") {
    throw new TypeError("module specifier must be a string literal");
  }
  return value;
}

/**
 * Translate one import clause into plain statements.
 *
 * @param {string} clause raw text between `import` and `from`
 * @param {string} specifier evaluated module specifier
 */
function __pi_import_clause(clause, specifier) {
  const spec = JSON.stringify(specifier);
  const text = clause.trim();
  // `import type { Foo } from "..."` — erased by the TS compiler upstream;
  // there is nothing to bind at runtime.
  if (text === "type" || text.startsWith("type ") || text.startsWith("type{")) {
    return "";
  }
  const entries = (inner) => {
    const out = [];
    for (const raw of inner.split(",")) {
      const part = raw.trim();
      if (!part) continue;
      const bits = part.split(/\s+as\s+/);
      const imported = bits[0].trim();
      const local = (bits[1] || bits[0]).trim();
      // `import { type Foo }` is erased by the TS compiler upstream, but a
      // *value* named `type` is legal (`node:os` exports one), so only the
      // `type Name` form is dropped.
      if (imported.startsWith("type ")) continue;
      out.push(imported === local ? imported : imported + ": " + local);
    }
    return out;
  };
  const named = (inner) => {
    const list = entries(inner);
    if (list.length === 0) return "__pi_import(" + spec + ");";
    return "const { " + list.join(", ") + " } = __pi_import(" + spec + ");";
  };
  if (text.startsWith("{")) {
    return named(text.slice(1, text.lastIndexOf("}")));
  }
  if (text.startsWith("*")) {
    const local = text.replace(/^\*\s*as\s*/, "").trim();
    return "const " + local + " = __pi_import(" + spec + ");";
  }
  const comma = text.indexOf(",");
  if (comma === -1) {
    return "const " + text + " = __pi_import(" + spec + ").default;";
  }
  const statements = [];
  const defaultLocal = text.slice(0, comma).trim();
  const rest = text.slice(comma + 1).trim();
  if (defaultLocal) {
    statements.push("const " + defaultLocal + " = __pi_import(" + spec + ").default;");
  }
  if (rest.startsWith("{")) {
    statements.push(named(rest.slice(1, rest.lastIndexOf("}"))));
  } else if (rest.startsWith("*")) {
    const local = rest.replace(/^\*\s*as\s*/, "").trim();
    statements.push("const " + local + " = __pi_import(" + spec + ");");
  }
  return statements.join(" ");
}

/**
 * Scan the source for the ESM constructs the host supports and return
 * the replacement edits plus a format verdict.
 *
 * @param {string} source
 */
function __pi_analyze_module(source) {
  const masked = __pi_mask_source(source);
  const tokens = __pi_tokenize(masked);
  const edits = [];
  let isEsm = false;
  let hasDefaultExport = false;
  let depth = 0;
  for (let index = 0; index < tokens.length; index++) {
    const token = tokens[index];
    if (token.kind === "punct") {
      const ch = masked[token.start];
      if (ch === "{" || ch === "(" || ch === "[") depth++;
      else if (ch === "}" || ch === ")" || ch === "]") depth = Math.max(0, depth - 1);
      continue;
    }
    if (token.kind !== "ident") continue;
    const word = source.slice(token.start, token.end);
    const next = tokens[index + 1];
    // `import.meta` — legal at any depth.
    if (word === "import" && next && next.kind === "punct" && masked[next.start] === ".") {
      const meta = tokens[index + 2];
      if (meta && meta.kind === "ident" && source.slice(meta.start, meta.end) === "meta") {
        edits.push({ start: token.start, end: meta.end, text: "__pi_meta" });
        continue;
      }
    }
    if (word === "export" && depth === 0) {
      if (next && next.kind === "ident" && source.slice(next.start, next.end) === "default") {
        isEsm = true;
        hasDefaultExport = true;
        edits.push({ start: token.start, end: next.end, text: "__pi_default_export =" });
      }
      continue;
    }
    if (word !== "import" || depth > 0) continue;
    // Dynamic `import(...)` stays untouched (quickjs has no loader for it).
    if (next && next.kind === "punct" && masked[next.start] === "(") continue;
    isEsm = true;
    // Bare side-effect import: `import "node:path";`
    if (next && next.kind === "string") {
      const specifier = __pi_read_string_literal(source.slice(next.start, next.end));
      let end = next.end;
      const semi = tokens[index + 2];
      if (semi && semi.kind === "punct" && masked[semi.start] === ";") end = semi.end;
      edits.push({
        start: token.start,
        end: end,
        text: "__pi_import(" + JSON.stringify(specifier) + ");",
      });
      continue;
    }
    // `import <clause> from "<specifier>";`
    let from = null;
    let spec = null;
    let j = index + 1;
    while (j < tokens.length) {
      const t = tokens[j];
      if (t.kind === "punct" && masked[t.start] === ";") break;
      if (t.kind === "ident" && source.slice(t.start, t.end) === "from") {
        const candidate = tokens[j + 1];
        if (candidate && candidate.kind === "string") {
          from = t;
          spec = candidate;
          break;
        }
      }
      j++;
    }
    if (!from || !spec) {
      throw new SyntaxError(
        "could not parse `import` statement at offset " + token.start + " (missing `from \"<specifier>\"`)",
      );
    }
    const clause = source.slice(token.end, from.start);
    const specifier = __pi_read_string_literal(source.slice(spec.start, spec.end));
    let end = spec.end;
    const semi = tokens[j + 2];
    if (semi && semi.kind === "punct" && masked[semi.start] === ";") end = semi.end;
    edits.push({
      start: token.start,
      end: end,
      text: __pi_import_clause(clause, specifier),
    });
    index = j + 1;
  }
  return { isEsm: isEsm, hasDefaultExport: hasDefaultExport, edits: edits };
}

// ---------------------------------------------------------------------------
// Virtual modules — the `node:path` / `node:url` slice of upstream
// `VIRTUAL_MODULES`. POSIX semantics; the embedded host loads extensions
// from absolute paths and the upstream examples only use `join` /
// `dirname` / `fileURLToPath`.
// ---------------------------------------------------------------------------

const __pi_path_module = (() => {
  const sep = "/";
  const delimiter = ":";

  function assertPath(p) {
    if (typeof p !== "string") {
      throw new TypeError("Path must be a string. Received " + typeof p);
    }
  }

  function normalize(p) {
    assertPath(p);
    if (p === "") return ".";
    const isAbsolute = p.charCodeAt(0) === 47; /* '/' */
    const trailingSeparator = p.length > 1 && p.charCodeAt(p.length - 1) === 47;
    const out = [];
    for (const segment of p.split("/")) {
      if (segment === "" || segment === ".") continue;
      if (segment === "..") {
        if (out.length > 0 && out[out.length - 1] !== "..") out.pop();
        else if (!isAbsolute) out.push("..");
        continue;
      }
      out.push(segment);
    }
    let result = out.join("/");
    if (result === "") result = isAbsolute ? "/" : ".";
    else if (isAbsolute) result = "/" + result;
    if (trailingSeparator && result !== "/") result += "/";
    return result;
  }

  function join(...args) {
    if (args.length === 0) return ".";
    let joined = "";
    for (const arg of args) {
      assertPath(arg);
      if (arg.length === 0) continue;
      joined = joined.length === 0 ? arg : joined + "/" + arg;
    }
    return joined.length === 0 ? "." : normalize(joined);
  }

  function resolve(...args) {
    let resolved = "";
    let isAbsolute = false;
    for (let i = args.length - 1; i >= -1 && !isAbsolute; i--) {
      const p =
        i >= 0
          ? args[i]
          : typeof globalThis._pi_cwd === "string" && globalThis._pi_cwd
            ? globalThis._pi_cwd
            : "/";
      assertPath(p);
      if (p.length === 0) continue;
      resolved = resolved.length === 0 ? p : p + "/" + resolved;
      isAbsolute = p.charCodeAt(0) === 47;
    }
    return resolved.length === 0 ? "/" : normalize(resolved);
  }

  function dirname(p) {
    assertPath(p);
    if (p === "") return ".";
    const rootEnd = p.charCodeAt(0) === 47 ? 1 : 0;
    let end = -1;
    for (let i = p.length - 1; i >= rootEnd; i--) {
      if (p[i] === "/") {
        end = i;
        break;
      }
    }
    if (end === -1) return rootEnd ? "/" : ".";
    let cut = end;
    while (cut > rootEnd && p[cut - 1] === "/") cut--;
    if (cut === rootEnd) return p.slice(0, rootEnd) || "/";
    return p.slice(0, cut);
  }

  function basename(p, ext) {
    assertPath(p);
    if (ext !== undefined && typeof ext !== "string") {
      throw new TypeError("The 'ext' argument must be of type string");
    }
    let end = p.length;
    while (end > 1 && p.charCodeAt(end - 1) === 47) end--;
    const slash = p.lastIndexOf("/", end - 1);
    let base = p.slice(slash === -1 ? 0 : slash + 1, end);
    if (ext && base.endsWith(ext)) base = base.slice(0, base.length - ext.length);
    return base;
  }

  function extname(p) {
    assertPath(p);
    const base = basename(p);
    const dot = base.lastIndexOf(".");
    return dot <= 0 ? "" : base.slice(dot);
  }

  function isAbsolute(p) {
    assertPath(p);
    return p.charCodeAt(0) === 47;
  }

  function relative(from, to) {
    assertPath(from);
    assertPath(to);
    if (isAbsolute(from) !== isAbsolute(to)) return to;
    const fromParts = normalize(from)
      .split("/")
      .filter((s) => s.length > 0);
    const toParts = normalize(to)
      .split("/")
      .filter((s) => s.length > 0);
    let i = 0;
    while (i < fromParts.length && i < toParts.length && fromParts[i] === toParts[i]) i++;
    const parts = fromParts.slice(i).map(() => "..").concat(toParts.slice(i));
    return parts.length === 0 ? "" : parts.join("/");
  }

  function parse(p) {
    assertPath(p);
    const root = p.charCodeAt(0) === 47 ? "/" : "";
    const base = basename(p);
    const ext = extname(p);
    return {
      root: root,
      dir: dirname(p),
      base: base,
      ext: ext,
      name: ext ? base.slice(0, base.length - ext.length) : base,
    };
  }

  function format(obj) {
    if (!obj || typeof obj !== "object") {
      throw new TypeError("The 'pathObject' argument must be of type object");
    }
    const dir = obj.dir || obj.root || "";
    const base = obj.base || (obj.name ? obj.name + (obj.ext || "") : "");
    if (!dir) return base;
    return dir === "/" ? "/" + base : dir + "/" + base;
  }

  const mod = {
    sep: sep,
    delimiter: delimiter,
    normalize: normalize,
    join: join,
    resolve: resolve,
    dirname: dirname,
    basename: basename,
    extname: extname,
    isAbsolute: isAbsolute,
    relative: relative,
    parse: parse,
    format: format,
  };
  mod.posix = mod;
  // `import path from "node:path"` — Node's CJS interop exposes the whole
  // module as the default export, so mirror that.
  mod.default = mod;
  return Object.freeze(mod);
})();

const __pi_url_module = (() => {
  function fileURLToPath(input) {
    let href;
    if (typeof input === "string") href = input;
    else if (input && typeof input.href === "string") href = input.href;
    else if (input && typeof input.toString === "function") href = String(input);
    else throw new TypeError("The 'url' argument must be of type string or an instance of URL");
    const match = /^file:\/\/([^/?#]*)([^?#]*)/i.exec(href);
    if (!match) throw new TypeError("The URL must be of scheme file: " + href);
    const host = match[1];
    let pathPart = match[2] === "" ? "/" : match[2];
    let decoded;
    try {
      decoded = decodeURIComponent(pathPart);
    } catch (_e) {
      decoded = pathPart;
    }
    if (/^\/[A-Za-z]:\//.test(decoded)) decoded = decoded.slice(1);
    if (host && host.toLowerCase() !== "localhost") return "//" + host + decoded;
    return decoded;
  }

  function pathToFileURL(p) {
    if (typeof p !== "string") throw new TypeError("The 'path' argument must be of type string");
    assertAbsolute(p);
    let normalized = p.replace(/\\/g, "/");
    if (/^[A-Za-z]:/.test(normalized)) normalized = "/" + normalized;
    const encoded = normalized
      .split("/")
      .map((segment) => encodeURIComponent(segment))
      .join("/");
    const href = "file://" + encoded;
    return {
      href: href,
      toString: () => href,
      toJSON: () => href,
    };
  }

  function assertAbsolute(p) {
    if (p.charCodeAt(0) !== 47 && !/^[A-Za-z]:[\\/]/.test(p)) {
      throw new TypeError("The 'path' argument must be an absolute path");
    }
  }

  const mod = { fileURLToPath: fileURLToPath, pathToFileURL: pathToFileURL };
  // `import url from "node:url"` — same CJS-interop shape as `node:path`.
  mod.default = mod;
  return Object.freeze(mod);
})();
// ---------------------------------------------------------------------------
// `typebox` virtual module — the schema builder upstream extensions use
// for `pi.registerTool({ parameters })`. TypeBox is a *virtual* module
// upstream too (`packages/coding-agent/src/core/extensions/loader.ts`
// `VIRTUAL_MODULES`), so extensions import it by bare specifier and the
// host resolves it; here that resolution is
// `globalThis.__pi_virtual_modules`.
//
// This implements the subset of the TypeBox v1 `Type` namespace the pi
// extension ecosystem actually calls (the examples under
// `packages/coding-agent/examples/extensions/*`) and emits the same
// plain JSON Schema TypeBox v1 produces, verified against
// `typebox@1.3.7`:
//
//   Type.Object({ a: Type.String(), b: Type.Optional(Type.Number()) })
//   => { type: 'object', required: ['a'], properties: { … } }
//
// `Optional` / `Readonly` are carried on non-enumerable symbol keys, so
// `JSON.stringify` (the host's `registerTool` path) sees only the JSON
// Schema — exactly like TypeBox's own `OptionalKind` symbol.
// ---------------------------------------------------------------------------

const __pi_typebox_module = (() => {
  const OPTIONAL = Symbol.for("pi.typebox.optional");
  const READONLY = Symbol.for("pi.typebox.readonly");

  function mergeOptions(schema, options) {
    if (options && typeof options === "object") {
      for (const key of Object.keys(options)) {
        const value = options[key];
        if (value !== undefined) schema[key] = value;
      }
    }
    return schema;
  }

  function withMarker(schema, marker) {
    if (!schema || typeof schema !== "object") return schema;
    const copy = Array.isArray(schema) ? schema.slice() : Object.assign({}, schema);
    Object.defineProperty(copy, marker, { value: true, enumerable: false });
    return copy;
  }

  function isOptional(schema) {
    return !!schema && typeof schema === "object" && schema[OPTIONAL] === true;
  }

  /** `Type.Any()` / `Type.Unknown()` are `{}` plus options. */
  function Any(options) {
    return mergeOptions({}, options);
  }

  function typed(name) {
    return function (options) {
      return mergeOptions({ type: name }, options);
    };
  }

  const NullType = typed("null");
  const BooleanType = typed("boolean");
  const StringType = typed("string");
  const NumberType = typed("number");
  const IntegerType = typed("integer");

  function Literal(value, options) {
    let name;
    switch (typeof value) {
      case "string":
        name = "string";
        break;
      case "number":
        name = "number";
        break;
      case "boolean":
        name = "boolean";
        break;
      case "bigint":
        name = "bigint";
        break;
      default:
        throw new TypeError(
          "Type.Literal: value must be a string, number, boolean, or bigint",
        );
    }
    return mergeOptions({ type: name, const: value }, options);
  }

  function Enum(values, options) {
    let list;
    if (Array.isArray(values)) {
      list = values.slice();
    } else if (values && typeof values === "object") {
      // A TS numeric enum carries reverse mappings (`{ 0: 'A', A: 0 }`);
      // TypeBox keeps only the declared members, so drop a string value
      // whose key it maps back to (`enum[enum[k]] === Number(k)`).
      list = [];
      for (const key of Object.keys(values)) {
        const value = values[key];
        if (typeof value === "string" && values[value] === Number(key)) continue;
        if (!list.some((entry) => entry === value)) list.push(value);
      }
    } else {
      throw new TypeError("Type.Enum: expected an enum object or array");
    }
    return mergeOptions({ enum: list }, options);
  }

  function ArrayType(items, options) {
    return mergeOptions(
      { type: "array", items: items === undefined ? {} : items },
      options,
    );
  }

  function TupleType(items, options) {
    const list = Array.isArray(items) ? items.slice() : [];
    return mergeOptions(
      { type: "array", additionalItems: false, items: list, minItems: list.length },
      options,
    );
  }

  function UnionType(schemas, options) {
    return mergeOptions({ anyOf: Array.isArray(schemas) ? schemas.slice() : [] }, options);
  }

  function IntersectType(schemas, options) {
    return mergeOptions({ allOf: Array.isArray(schemas) ? schemas.slice() : [] }, options);
  }

  function ObjectType(properties, options) {
    const props = properties && typeof properties === "object" ? properties : {};
    const schema = { type: "object" };
    const required = Object.keys(props).filter((key) => !isOptional(props[key]));
    if (required.length > 0) schema.required = required;
    schema.properties = props;
    return mergeOptions(schema, options);
  }

  /**
   * Literal string keys of a record key schema, or `null` when the key
   * is a pattern (`Type.String()` etc.) and TypeBox would emit
   * `patternProperties` instead of `properties`.
   */
  function literalKeysOf(schema) {
    if (!schema || typeof schema !== "object") return null;
    if (typeof schema.const === "string") return [schema.const];
    if (
      Array.isArray(schema.enum) &&
      schema.enum.every((value) => typeof value === "string")
    ) {
      return schema.enum.slice();
    }
    if (Array.isArray(schema.anyOf)) {
      const keys = [];
      for (const branch of schema.anyOf) {
        const branchKeys = literalKeysOf(branch);
        if (branchKeys === null) return null;
        keys.push(...branchKeys);
      }
      return keys;
    }
    return null;
  }

  function RecordType(key, value, options) {
    const schema = { type: "object" };
    const keys = literalKeysOf(key);
    if (keys !== null) {
      const props = {};
      for (const literal of keys) props[literal] = value;
      schema.required = keys.slice();
      schema.properties = props;
    } else {
      schema.patternProperties = { "^.*$": value };
    }
    return mergeOptions(schema, options);
  }

  function Optional(schema) {
    return withMarker(schema, OPTIONAL);
  }

  function Readonly(schema) {
    return withMarker(schema, READONLY);
  }

  /** `Type.Unsafe` returns the caller's schema untouched (options are ignored). */
  function Unsafe(schema) {
    return schema;
  }

  function Partial(schema) {
    if (!schema || typeof schema !== "object" || !schema.properties) return schema;
    const props = {};
    for (const key of Object.keys(schema.properties)) {
      props[key] = Optional(schema.properties[key]);
    }
    const copy = Object.assign({}, schema);
    copy.properties = props;
    delete copy.required;
    return copy;
  }

  const Type = {
    Any: Any,
    Unknown: Any,
    Null: NullType,
    Boolean: BooleanType,
    String: StringType,
    Number: NumberType,
    Integer: IntegerType,
    Literal: Literal,
    Enum: Enum,
    Array: ArrayType,
    Tuple: TupleType,
    Union: UnionType,
    Intersect: IntersectType,
    Object: ObjectType,
    Record: RecordType,
    Optional: Optional,
    Readonly: Readonly,
    Unsafe: Unsafe,
    Partial: Partial,
  };

  const mod = { Type: Type, Kind: Symbol.for("pi.typebox.Kind") };
  // `import TypeBox from "typebox"` — mirror the CJS-interop default the
  // other virtual modules use.
  mod.default = mod;
  return Object.freeze(mod);
})();

// ---------------------------------------------------------------------------
// `node:*` builtin virtual modules
//
// Upstream runs extensions on Node/Bun, so the examples under
// `packages/coding-agent/examples/extensions/` import `node:fs`,
// `node:fs/promises`, `node:os`, `node:buffer`, `node:crypto` and read
// the `process` global directly. The embedded QuickJS runtime has no
// operating-system surface of its own, so every call below goes through
// the single `host_node_call(op, argsJson)` host import installed by
// `JsExtensionHost` (native-only, like the rest of the extension host).
//
// The *sync* API of each module is the source of truth: the host bridge
// is synchronous, so `node:fs/promises` and the callback forms are thin
// wrappers over it. Deliberate divergences from Node are documented in
// `crates/pi-extensions/docs/NODE_BUILTINS.md`:
//
//   - errors carry Node's `code` / `syscall` / `path` but the message
//     text comes from Rust's `io::Error`;
//   - `fs.mkdirSync(path, {recursive: true})` returns `undefined`
//     instead of the first created directory;
//   - `node:child_process`, `node:stream`, `node:http`, … and
//     `crypto.createHash` are not provided: importing them fails with
//     the readable "unsupported import" error that lists what exists.
// ---------------------------------------------------------------------------

/**
 * Call a host bridge op and unwrap the JSON envelope.
 *
 * The Rust side returns `{"ok":true,"value":…}` or
 * `{"ok":false,"code":…,"message":…,"syscall":…,"path":…}` and never
 * throws, so the Node-shaped `Error` extensions branch on is built here,
 * in one place.
 *
 * @param {string} op
 * @param {object} [args]
 */
function __pi_node_call(op, args) {
  const raw = globalThis.host_node_call(op, JSON.stringify(args || {}));
  let envelope = null;
  try {
    envelope = JSON.parse(raw);
  } catch (_e) {
    throw new Error("pi extension host returned malformed JSON for `" + op + "`");
  }
  if (envelope && envelope.ok === true) return envelope.value;
  const err = new Error(
    envelope && typeof envelope.message === "string"
      ? envelope.message
      : "pi extension host call `" + op + "` failed",
  );
  if (envelope && typeof envelope.code === "string") err.code = envelope.code;
  if (envelope && typeof envelope.syscall === "string") err.syscall = envelope.syscall;
  if (envelope && typeof envelope.path === "string") err.path = envelope.path;
  throw err;
}

/** Run `fn` on the microtask queue when the engine has one. */
function __pi_schedule(fn) {
  if (typeof queueMicrotask === "function") queueMicrotask(fn);
  else fn();
}

// ---------------------------------------------------------------------------
// `node:buffer` — the `Buffer` subset extensions touch: `from` / `alloc` /
// `concat` / `byteLength` / `isBuffer` plus `toString(encoding)` and
// `equals`. Bytes are a real `Uint8Array` subclass, so `instanceof`,
// indexing, `length`, `slice` and iteration behave like Node's.
// ---------------------------------------------------------------------------

const __pi_buffer_module = (() => {
  const HEX_DIGITS = "0123456789abcdef";
  const BASE64_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const BASE64URL_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

  /** Map Node's encoding aliases onto the handful this subset implements. */
  function canonicalEncoding(encoding) {
    if (encoding === undefined || encoding === null) return "utf8";
    const name = String(encoding).toLowerCase();
    if (name === "utf8" || name === "utf-8") return "utf8";
    if (name === "latin1" || name === "binary" || name === "ascii") return "latin1";
    if (name === "hex") return "hex";
    if (name === "base64") return "base64";
    if (name === "base64url") return "base64url";
    if (name === "utf16le" || name === "utf-16le" || name === "ucs2" || name === "ucs-2") {
      return "utf16le";
    }
    throw new TypeError("Unknown encoding: " + encoding);
  }

  function utf8Encode(text) {
    const out = [];
    for (let i = 0; i < text.length; i++) {
      let code = text.charCodeAt(i);
      if (code >= 0xd800 && code <= 0xdbff && i + 1 < text.length) {
        const next = text.charCodeAt(i + 1);
        if (next >= 0xdc00 && next <= 0xdfff) {
          code = 0x10000 + ((code - 0xd800) << 10) + (next - 0xdc00);
          i += 1;
        }
      }
      if (code < 0x80) {
        out.push(code);
      } else if (code < 0x800) {
        out.push(0xc0 | (code >> 6), 0x80 | (code & 0x3f));
      } else if (code < 0x10000) {
        out.push(0xe0 | (code >> 12), 0x80 | ((code >> 6) & 0x3f), 0x80 | (code & 0x3f));
      } else {
        out.push(
          0xf0 | (code >> 18),
          0x80 | ((code >> 12) & 0x3f),
          0x80 | ((code >> 6) & 0x3f),
          0x80 | (code & 0x3f),
        );
      }
    }
    return out;
  }

  /** Build a JS string from UTF-16 code units / code points. */
  function fromCodeUnits(units) {
    let out = "";
    for (let i = 0; i < units.length; i++) {
      const code = units[i];
      if (code > 0xffff) {
        const rest = code - 0x10000;
        out += String.fromCharCode(0xd800 + (rest >> 10), 0xdc00 + (rest & 0x3ff));
      } else {
        out += String.fromCharCode(code);
      }
    }
    return out;
  }

  function utf8Decode(bytes) {
    const units = [];
    let i = 0;
    while (i < bytes.length) {
      const first = bytes[i];
      let code = 0xfffd;
      let size = 1;
      if (first < 0x80) {
        code = first;
      } else if ((first & 0xe0) === 0xc0) {
        size = 2;
        if (bytes.length - i >= 2 && (bytes[i + 1] & 0xc0) === 0x80) {
          code = ((first & 0x1f) << 6) | (bytes[i + 1] & 0x3f);
          if (code < 0x80) code = 0xfffd;
        }
      } else if ((first & 0xf0) === 0xe0) {
        size = 3;
        if (bytes.length - i >= 3 && (bytes[i + 1] & 0xc0) === 0x80 && (bytes[i + 2] & 0xc0) === 0x80) {
          code = ((first & 0x0f) << 12) | ((bytes[i + 1] & 0x3f) << 6) | (bytes[i + 2] & 0x3f);
          if (code < 0x800 || (code >= 0xd800 && code <= 0xdfff)) code = 0xfffd;
        }
      } else if ((first & 0xf8) === 0xf0) {
        size = 4;
        if (
          bytes.length - i >= 4 &&
          (bytes[i + 1] & 0xc0) === 0x80 &&
          (bytes[i + 2] & 0xc0) === 0x80 &&
          (bytes[i + 3] & 0xc0) === 0x80
        ) {
          code =
            ((first & 0x07) << 18) |
            ((bytes[i + 1] & 0x3f) << 12) |
            ((bytes[i + 2] & 0x3f) << 6) |
            (bytes[i + 3] & 0x3f);
          if (code < 0x10000 || code > 0x10ffff) code = 0xfffd;
        }
      }
      i += size;
      units.push(code);
    }
    return fromCodeUnits(units);
  }

  function hexEncode(bytes) {
    let out = "";
    for (let i = 0; i < bytes.length; i++) {
      out += HEX_DIGITS[bytes[i] >> 4] + HEX_DIGITS[bytes[i] & 0x0f];
    }
    return out;
  }

  function hexDecode(text) {
    const clean = String(text).replace(/\s+/g, "");
    if (clean.length % 2 !== 0) throw new TypeError("Invalid hex string");
    const out = new Uint8Array(clean.length / 2);
    for (let i = 0; i < clean.length; i += 2) {
      const byte = parseInt(clean.slice(i, i + 2), 16);
      if (byte !== byte) throw new TypeError("Invalid hex string");
      out[i / 2] = byte;
    }
    return out;
  }

  function base64Encode(bytes, urlSafe) {
    const alphabet = urlSafe ? BASE64URL_ALPHABET : BASE64_ALPHABET;
    let out = "";
    for (let i = 0; i < bytes.length; i += 3) {
      const b0 = bytes[i];
      const b1 = i + 1 < bytes.length ? bytes[i + 1] : 0;
      const b2 = i + 2 < bytes.length ? bytes[i + 2] : 0;
      const triple = (b0 << 16) | (b1 << 8) | b2;
      out += alphabet[(triple >> 18) & 0x3f];
      out += alphabet[(triple >> 12) & 0x3f];
      out += i + 1 < bytes.length ? alphabet[(triple >> 6) & 0x3f] : urlSafe ? "" : "=";
      out += i + 2 < bytes.length ? alphabet[triple & 0x3f] : urlSafe ? "" : "=";
    }
    return out;
  }

  function base64Decode(text, urlSafe) {
    const alphabet = urlSafe ? BASE64URL_ALPHABET : BASE64_ALPHABET;
    const clean = String(text).replace(/[\s=]+/g, "");
    const out = [];
    let buffer = 0;
    let bits = 0;
    for (let i = 0; i < clean.length; i++) {
      const value = alphabet.indexOf(clean[i]);
      if (value < 0) throw new TypeError("Invalid base64 string");
      buffer = (buffer << 6) | value;
      bits += 6;
      if (bits >= 8) {
        bits -= 8;
        out.push((buffer >> bits) & 0xff);
      }
    }
    return out;
  }

  function encodeString(text, encoding) {
    switch (canonicalEncoding(encoding)) {
      case "utf8":
        return utf8Encode(text);
      case "latin1": {
        const out = [];
        for (let i = 0; i < text.length; i++) out.push(text.charCodeAt(i) & 0xff);
        return out;
      }
      case "utf16le": {
        const out = [];
        for (let i = 0; i < text.length; i++) {
          const code = text.charCodeAt(i);
          out.push(code & 0xff, (code >> 8) & 0xff);
        }
        return out;
      }
      case "hex":
        return Array.prototype.slice.call(hexDecode(text));
      case "base64":
        return base64Decode(text, false);
      case "base64url":
        return base64Decode(text, true);
      default:
        return utf8Encode(text);
    }
  }

  function decodeBytes(bytes, encoding) {
    switch (canonicalEncoding(encoding)) {
      case "utf8":
        return utf8Decode(bytes);
      case "latin1": {
        let out = "";
        for (let i = 0; i < bytes.length; i++) out += String.fromCharCode(bytes[i]);
        return out;
      }
      case "utf16le": {
        let out = "";
        for (let i = 0; i + 1 < bytes.length; i += 2) {
          out += String.fromCharCode(bytes[i] | (bytes[i + 1] << 8));
        }
        return out;
      }
      case "hex":
        return hexEncode(bytes);
      case "base64":
        return base64Encode(bytes, false);
      case "base64url":
        return base64Encode(bytes, true);
      default:
        return utf8Decode(bytes);
    }
  }

  class Buffer extends Uint8Array {
    static from(value, encodingOrOffset) {
      if (typeof value === "string") {
        return new Buffer(encodeString(value, encodingOrOffset));
      }
      if (value instanceof ArrayBuffer) {
        return new Buffer(new Uint8Array(value));
      }
      if (value) {
        if (value.buffer instanceof ArrayBuffer) {
          // Any ArrayBufferView (typed array or DataView).
          const view = new Uint8Array(value.buffer, value.byteOffset || 0, value.byteLength);
          return new Buffer(view);
        }
        if (Array.isArray(value) || typeof value.length === "number") {
          const out = new Buffer(value.length >>> 0);
          for (let i = 0; i < out.length; i++) out[i] = Number(value[i]) & 0xff;
          return out;
        }
      }
      throw new TypeError(
        "The first argument must be a string, Buffer, ArrayBuffer, Array, or Array-like object",
      );
    }

    static alloc(size, fill, encoding) {
      const out = new Buffer(Number(size) >>> 0);
      if (fill !== undefined) {
        out.fill(typeof fill === "string" ? encodeString(fill, encoding)[0] : Number(fill) & 0xff);
      }
      return out;
    }

    static allocUnsafe(size) {
      return new Buffer(Number(size) >>> 0);
    }

    static isBuffer(value) {
      return value instanceof Buffer;
    }

    static byteLength(value, encoding) {
      if (typeof value !== "string") return value.length;
      return encodeString(value, encoding).length;
    }

    static concat(list, totalLength) {
      const parts = [];
      let total = totalLength === undefined ? 0 : Number(totalLength) >>> 0;
      for (let i = 0; i < list.length; i++) {
        const part = list[i] instanceof Uint8Array ? list[i] : Buffer.from(list[i]);
        parts.push(part);
        if (totalLength === undefined) total += part.length;
      }
      const out = new Buffer(total);
      let offset = 0;
      for (let i = 0; i < parts.length && offset < total; i++) {
        const part = parts[i];
        const count = Math.min(part.length, total - offset);
        Uint8Array.prototype.set.call(
          out,
          Uint8Array.prototype.subarray.call(part, 0, count),
          offset,
        );
        offset += count;
      }
      return out;
    }

    static compare(a, b) {
      const left = a instanceof Uint8Array ? a : Buffer.from(a);
      const right = b instanceof Uint8Array ? b : Buffer.from(b);
      const shared = Math.min(left.length, right.length);
      for (let i = 0; i < shared; i++) {
        if (left[i] !== right[i]) return left[i] < right[i] ? -1 : 1;
      }
      if (left.length === right.length) return 0;
      return left.length < right.length ? -1 : 1;
    }

    toString(encoding, start, end) {
      const from = start === undefined ? 0 : start;
      const to = end === undefined ? this.length : end;
      const view = Uint8Array.prototype.subarray.call(this, from, to);
      return decodeBytes(view, encoding);
    }

    equals(other) {
      return Buffer.compare(this, other) === 0;
    }

    compare(other) {
      return Buffer.compare(this, other);
    }

    slice(start, end) {
      return Buffer.from(Uint8Array.prototype.subarray.call(this, start, end));
    }

    subarray(start, end) {
      return Buffer.from(Uint8Array.prototype.subarray.call(this, start, end));
    }

    toJSON() {
      return { type: "Buffer", data: Array.prototype.slice.call(this) };
    }

    /** Not part of Node's API; keeps the base64 hop off the hot path. */
    static __toBase64(value, encoding) {
      const buffer = typeof value === "string" ? Buffer.from(value, encoding) : Buffer.from(value);
      return buffer.toString("base64");
    }
  }

  const mod = {
    Buffer: Buffer,
    SlowBuffer: Buffer,
    constants: Object.freeze({ MAX_LENGTH: 0x7fffffff, MAX_STRING_LENGTH: 0x1fffffe8 }),
  };
  mod.default = mod;
  return Object.freeze(mod);
})();

// ---------------------------------------------------------------------------
// `node:fs` (+ `node:fs/promises`) — the subset the extension ecosystem
// actually calls. Path arguments are stringified (`String(path)`), so a
// `URL` object works as well as a path string; `Buffer` is accepted as
// file *data* and returned from reads when no encoding is given.
// ---------------------------------------------------------------------------

const __pi_fs_module = (() => {
  const BufferCtor = __pi_buffer_module.Buffer;

  function optionEncoding(options) {
    if (options === undefined || options === null) return null;
    if (typeof options === "string") return options;
    if (typeof options === "object" && options.encoding) return options.encoding;
    return null;
  }

  function optionFlag(options) {
    return options && typeof options === "object" && options.flag ? String(options.flag) : "r";
  }

  function readFileSync(path, options) {
    const flag = optionFlag(options);
    if (flag !== "r" && flag !== "rs" && flag !== "r+") {
      throw new Error("pi extension host fs.readFileSync only supports read flags, got " + flag);
    }
    const result = __pi_node_call("fs.readFile", { path: String(path) });
    const buffer = BufferCtor.from(result.base64, "base64");
    const encoding = optionEncoding(options);
    return encoding ? buffer.toString(encoding) : buffer;
  }

  function writeFileSync(path, data, options) {
    __pi_node_call("fs.writeFile", {
      path: String(path),
      base64: BufferCtor.__toBase64(data, optionEncoding(options)),
    });
  }

  function appendFileSync(path, data, options) {
    __pi_node_call("fs.appendFile", {
      path: String(path),
      base64: BufferCtor.__toBase64(data, optionEncoding(options)),
    });
  }

  function existsSync(path) {
    return __pi_node_call("fs.exists", { path: String(path) }) === true;
  }

  function makeDirent(entry) {
    return Object.freeze({
      name: entry.name,
      isFile: () => entry.isFile === true,
      isDirectory: () => entry.isDirectory === true,
      isSymbolicLink: () => entry.isSymbolicLink === true,
      isBlockDevice: () => false,
      isCharacterDevice: () => false,
      isFIFO: () => false,
      isSocket: () => false,
    });
  }

  function readdirSync(path, options) {
    const entries = __pi_node_call("fs.readdir", { path: String(path) });
    const withFileTypes = options && typeof options === "object" && options.withFileTypes === true;
    return withFileTypes ? entries.map(makeDirent) : entries.map((entry) => entry.name);
  }

  function makeStats(raw) {
    return Object.freeze({
      size: raw.size,
      mode: raw.mode,
      mtimeMs: raw.mtimeMs,
      atimeMs: raw.atimeMs,
      ctimeMs: raw.ctimeMs,
      birthtimeMs: raw.birthtimeMs,
      mtime: new Date(raw.mtimeMs),
      atime: new Date(raw.atimeMs),
      ctime: new Date(raw.ctimeMs),
      birthtime: new Date(raw.birthtimeMs),
      isFile: () => raw.isFile === true,
      isDirectory: () => raw.isDirectory === true,
      isSymbolicLink: () => raw.isSymbolicLink === true,
      isBlockDevice: () => false,
      isCharacterDevice: () => false,
      isFIFO: () => false,
      isSocket: () => false,
    });
  }

  function statSync(path) {
    return makeStats(__pi_node_call("fs.stat", { path: String(path) }));
  }

  function lstatSync(path) {
    return makeStats(__pi_node_call("fs.lstat", { path: String(path) }));
  }

  function mkdirSync(path, options) {
    // Node returns the first created directory for `recursive: true`;
    // the bridge has no cheap way to know it, so this always returns
    // `undefined` (documented divergence).
    __pi_node_call("fs.mkdir", {
      path: String(path),
      recursive: !!(options && typeof options === "object" && options.recursive),
    });
  }

  function rmSync(path, options) {
    __pi_node_call("fs.rm", {
      path: String(path),
      force: !!(options && typeof options === "object" && options.force),
      recursive: !!(options && typeof options === "object" && options.recursive),
    });
  }

  function unlinkSync(path) {
    __pi_node_call("fs.unlink", { path: String(path) });
  }

  function rmdirSync(path) {
    __pi_node_call("fs.rmdir", { path: String(path) });
  }

  function renameSync(from, to) {
    __pi_node_call("fs.rename", { from: String(from), to: String(to) });
  }

  function copyFileSync(from, to) {
    __pi_node_call("fs.copyFile", { from: String(from), to: String(to) });
  }

  function realpathSync(path) {
    return __pi_node_call("fs.realpath", { path: String(path) });
  }

  function accessSync(path, mode) {
    // Only existence is checked: the bridge reports a permission error on
    // the ops that actually need it, and `R_OK`/`W_OK` probes would need a
    // dedicated syscall wrapper for little extension value.
    void mode;
    if (!existsSync(path)) {
      const err = new Error("ENOENT: no such file or directory, access '" + String(path) + "'");
      err.code = "ENOENT";
      err.syscall = "access";
      err.path = String(path);
      throw err;
    }
  }

  /** Wrap a sync function into Node's `(…, callback)` form. */
  function callbackify(fn) {
    return function () {
      const args = Array.prototype.slice.call(arguments);
      const callback = args.length > 0 ? args[args.length - 1] : undefined;
      if (typeof callback !== "function") return fn.apply(null, args);
      const rest = args.slice(0, -1);
      let value;
      let failure = null;
      try {
        value = fn.apply(null, rest);
      } catch (err) {
        failure = err;
      }
      __pi_schedule(function () {
        if (failure) callback(failure);
        else callback(null, value);
      });
      return undefined;
    };
  }

  /** Wrap a sync function into an immediately-resolved Promise. */
  function promisify(fn) {
    return function () {
      const args = arguments;
      return new Promise(function (resolve, reject) {
        try {
          resolve(fn.apply(null, args));
        } catch (err) {
          reject(err);
        }
      });
    };
  }

  const promises = Object.freeze({
    readFile: promisify(readFileSync),
    writeFile: promisify(writeFileSync),
    appendFile: promisify(appendFileSync),
    readdir: promisify(readdirSync),
    stat: promisify(statSync),
    lstat: promisify(lstatSync),
    mkdir: promisify(mkdirSync),
    rm: promisify(rmSync),
    unlink: promisify(unlinkSync),
    rmdir: promisify(rmdirSync),
    rename: promisify(renameSync),
    copyFile: promisify(copyFileSync),
    realpath: promisify(realpathSync),
    access: promisify(accessSync),
  });

  const mod = {
    readFileSync: readFileSync,
    writeFileSync: writeFileSync,
    appendFileSync: appendFileSync,
    existsSync: existsSync,
    readdirSync: readdirSync,
    statSync: statSync,
    lstatSync: lstatSync,
    mkdirSync: mkdirSync,
    rmSync: rmSync,
    unlinkSync: unlinkSync,
    rmdirSync: rmdirSync,
    renameSync: renameSync,
    copyFileSync: copyFileSync,
    realpathSync: realpathSync,
    accessSync: accessSync,
    readFile: callbackify(readFileSync),
    writeFile: callbackify(writeFileSync),
    appendFile: callbackify(appendFileSync),
    readdir: callbackify(readdirSync),
    stat: callbackify(statSync),
    lstat: callbackify(lstatSync),
    mkdir: callbackify(mkdirSync),
    rm: callbackify(rmSync),
    unlink: callbackify(unlinkSync),
    rmdir: callbackify(rmdirSync),
    rename: callbackify(renameSync),
    copyFile: callbackify(copyFileSync),
    realpath: callbackify(realpathSync),
    access: callbackify(accessSync),
    promises: promises,
    constants: Object.freeze({
      F_OK: 0,
      R_OK: 4,
      W_OK: 2,
      X_OK: 1,
      COPYFILE_EXCL: 1,
      COPYFILE_FICLONE: 2,
      COPYFILE_FICLONE_FORCE: 4,
    }),
  };
  mod.default = mod;
  return Object.freeze(mod);
})();

// ---------------------------------------------------------------------------
// `node:os` — the identity values extensions read (`homedir()` for config
// paths, `tmpdir()` for scratch files, `platform()` for shell branching).
// ---------------------------------------------------------------------------

const __pi_os_module = (() => {
  function hostValue(op) {
    return __pi_node_call(op, {});
  }

  const mod = {
    EOL: "\n",
    homedir: () => hostValue("os.homedir"),
    tmpdir: () => hostValue("os.tmpdir"),
    platform: () => hostValue("os.platform"),
    arch: () => hostValue("os.arch"),
    type: () => hostValue("os.type"),
    release: () => hostValue("os.release"),
    hostname: () => hostValue("os.hostname"),
    endianness: () => "LE",
    // `os.cpus()` / `os.totalmem()` are not bridged yet: the extension host
    // has no reason to expose machine topology, and returning made-up
    // numbers would be worse than a clear failure.
    cpus: () => {
      throw new Error("os.cpus() is not implemented in the pi extension host");
    },
  };
  mod.default = mod;
  return Object.freeze(mod);
})();

// ---------------------------------------------------------------------------
// `node:crypto` — `randomUUID` / `randomBytes` / `randomInt`, backed by the
// host's OS entropy bridge. `createHash` needs a digest backend the workspace
// does not bundle, so it fails with a readable message instead of producing
// wrong bytes.
// ---------------------------------------------------------------------------

const __pi_crypto_module = (() => {
  const BufferCtor = __pi_buffer_module.Buffer;

  function randomBytes(size, callback) {
    const buffer = BufferCtor.from(
      __pi_node_call("crypto.randomBytes", { count: Number(size) >>> 0 }).base64,
      "base64",
    );
    if (typeof callback === "function") {
      __pi_schedule(function () {
        callback(null, buffer);
      });
      return undefined;
    }
    return buffer;
  }

  function randomUint32() {
    const bytes = randomBytes(4);
    return ((bytes[0] << 24) | (bytes[1] << 16) | (bytes[2] << 8) | bytes[3]) >>> 0;
  }

  function randomUUID() {
    const bytes = randomBytes(16);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    const hex = bytes.toString("hex");
    return (
      hex.slice(0, 8) +
      "-" +
      hex.slice(8, 12) +
      "-" +
      hex.slice(12, 16) +
      "-" +
      hex.slice(16, 20) +
      "-" +
      hex.slice(20)
    );
  }

  function randomInt(min, max) {
    if (max === undefined) {
      max = min;
      min = 0;
    }
    const range = Number(max) - Number(min);
    if (!(range > 0)) throw new RangeError("randomInt: max must be greater than min");
    // Rejection sampling keeps the distribution uniform (a plain modulo
    // would favour the low end of the range).
    const limit = Math.floor(0x100000000 / range) * range;
    let value = randomUint32();
    while (value >= limit) value = randomUint32();
    return Number(min) + (value % range);
  }

  function createHash() {
    throw new Error(
      "crypto.createHash is not implemented in the pi extension host (no hashing backend is bundled)",
    );
  }

  const mod = {
    randomBytes: randomBytes,
    randomUUID: randomUUID,
    randomInt: randomInt,
    createHash: createHash,
  };
  mod.default = mod;
  return Object.freeze(mod);
})();

// ---------------------------------------------------------------------------
// `node:process` (also installed as the `process` global) — the read-only
// environment surface extensions consult. `env` is a snapshot: writes stay in
// JS because the bridge has no way to mutate the host process. `stdout` /
// `stderr` route through `host_log` instead of the real process streams: the
// `pi` process owns stdout (`--rpc` speaks JSON-RPC on it), so an extension
// must never write raw bytes there.
// ---------------------------------------------------------------------------

const __pi_process_module = (() => {
  function writeThroughHost(level, chunk) {
    if (typeof globalThis.host_log === "function") {
      globalThis.host_log(level, String(chunk));
    }
    return true;
  }

  const mod = {
    env: __pi_node_call("process.env", {}) || {},
    platform: __pi_node_call("process.platform", {}),
    arch: __pi_node_call("process.arch", {}),
    pid: __pi_node_call("process.pid", {}),
    // There is no Node runtime here; the marker makes version-gated feature
    // checks take the conservative branch.
    version: "v0.0.0-pi-rust",
    versions: Object.freeze({ pi: "0.1.0" }),
    // The host sets `_pi_cwd` from the session's `ToolContext`; the
    // bridge `process.cwd` (the daemon's own directory) is only the
    // fallback for hosts that run without a session context.
    cwd: () => {
      const contextCwd = globalThis._pi_cwd;
      if (typeof contextCwd === "string" && contextCwd.length > 0) return contextCwd;
      return __pi_node_call("process.cwd", {});
    },
    nextTick: (fn, ...args) => {
      __pi_schedule(() => fn.apply(null, args));
    },
    exitCode: 0,
    stdout: { isTTY: false, write: (chunk) => writeThroughHost("info", chunk) },
    stderr: { isTTY: false, write: (chunk) => writeThroughHost("error", chunk) },
  };
  mod.default = mod;
  return Object.freeze(mod);
})();

// ---------------------------------------------------------------------------
// `node:util` — the pure-JS helpers the ecosystem imports. Nothing here needs
// a host op (which is why it was the cheapest row on the `NODE_BUILTINS.md`
// frontier): upstream examples import `promisify` from here
// (`packages/coding-agent/examples/extensions/mac-system-theme.ts`) and the
// evals reporters use `stripVTControlCharacters` / `styleText`.
//
// Covered: `format` / `formatWithOptions`, `inspect` (+ `inspect.custom`,
// `inspect.defaultOptions`), `promisify` / `callbackify` (+ their `custom`
// symbols), `inherits`, `deprecate`, `stripVTControlCharacters`, `styleText`,
// `debuglog` / `debug`, `types`, `isDeepStrictEqual`, the legacy
// `isArray`/`isString`/… predicates, `toUSVString`, `TextEncoder` /
// `TextDecoder` (also installed as globals when the engine lacks them).
//
// Deliberately not covered, documented in `docs/NODE_BUILTINS.md`:
// `parseArgs`, `parseEnv`, `diff`, `aborted` / `transferableAbortSignal` /
// `transferableAbortController`, `getSystemErrorName` /
// `getSystemErrorMessage` / `getSystemErrorMap`, `getCallSite(s)`,
// `MIMEType` / `MIMEParams`, `setTraceSigInt`, `inspect`'s proxy / sorted /
// breakLength niceties, and the streaming `TextDecoder` form
// (`{ stream: true }`).
// ---------------------------------------------------------------------------

const __pi_util_module = (() => {
  const BufferCtor = __pi_buffer_module.Buffer;
  const inspectCustom = Symbol.for("nodejs.util.inspect.custom");
  const promisifyCustom = Symbol.for("nodejs.util.promisify.custom");
  const callbackifyCustom = Symbol.for("nodejs.util.callbackify.custom");
  const objectToString = Object.prototype.toString;
  const hasOwn = (target, key) => Object.prototype.hasOwnProperty.call(target, key);

  function typeTag(value) {
    return objectToString.call(value);
  }

  const DEFAULT_INSPECT_OPTIONS = {
    showHidden: false,
    depth: 2,
    colors: false,
    customInspect: true,
    maxArrayLength: 100,
    maxStringLength: 10000,
    breakLength: 80,
    compact: 3,
    sorted: false,
    getters: false,
  };

  const IDENTIFIER = /^[A-Za-z_$][A-Za-z0-9_$]*$/;

  // v22 palette; only the entries `inspect(..., { colors: true })` reaches.
  const ANSI_CODES = {
    bold: [1, 22],
    gray: [90, 39],
    green: [32, 39],
    yellow: [33, 39],
    magenta: [35, 39],
    cyan: [36, 39],
    red: [31, 39],
  };

  function colorize(text, name, options) {
    if (!options.colors) return text;
    const pair = ANSI_CODES[name];
    if (!pair) return text;
    return "\u001b[" + pair[0] + "m" + text + "\u001b[" + pair[1] + "m";
  }

  function invalidArgValue(name, value, reason) {
    const err = new TypeError(
      'The "' + name + '" argument must be ' + (reason || "a valid value") + ". Received " + inspect(value),
    );
    err.code = "ERR_INVALID_ARG_VALUE";
    return err;
  }

  function invalidArgType(name, expected, value) {
    const err = new TypeError(
      'The "' + name + '" argument must be of type ' + expected + ". Received type " + typeof value,
    );
    err.code = "ERR_INVALID_ARG_TYPE";
    return err;
  }

  // -------------------------------------------------------------------------
  // inspect
  // -------------------------------------------------------------------------

  function quoteString(value) {
    return (
      "'" +
      value
        .replace(/\\/g, "\\\\")
        .replace(/'/g, "\\'")
        .replace(/\n/g, "\\n")
        .replace(/\r/g, "\\r")
        .replace(/\t/g, "\\t") +
      "'"
    );
  }

  function formatPrimitive(value, options) {
    if (typeof value === "string") return colorize(quoteString(value), "green", options);
    if (typeof value === "number") {
      return colorize(Object.is(value, -0) ? "-0" : String(value), "yellow", options);
    }
    if (typeof value === "bigint") return colorize(String(value) + "n", "yellow", options);
    if (typeof value === "boolean") return colorize(String(value), "yellow", options);
    if (typeof value === "undefined") return colorize("undefined", "gray", options);
    if (value === null) return colorize("null", "bold", options);
    if (typeof value === "symbol") return colorize(value.toString(), "green", options);
    return String(value);
  }

  function functionLabel(fn) {
    const tag = typeTag(fn);
    const name = fn.name;
    if (tag === "[object AsyncFunction]") {
      return name ? "[AsyncFunction: " + name + "]" : "[AsyncFunction (anonymous)]";
    }
    if (tag === "[object GeneratorFunction]") {
      return name ? "[GeneratorFunction: " + name + "]" : "[GeneratorFunction (anonymous)]";
    }
    if (tag === "[object AsyncGeneratorFunction]") {
      return name ? "[AsyncGeneratorFunction: " + name + "]" : "[AsyncGeneratorFunction (anonymous)]";
    }
    let source = "";
    try {
      source = Function.prototype.toString.call(fn);
    } catch (_e) {
      source = "";
    }
    if (source.indexOf("class") === 0) {
      return "[class " + (name || "(anonymous)") + "]";
    }
    return name ? "[Function: " + name + "]" : "[Function (anonymous)]";
  }

  function ownKeys(value, options) {
    let keys = options.showHidden
      ? Reflect.ownKeys(value)
      : Object.keys(value);
    if (options.sorted) {
      keys = keys.slice().sort((a, b) => {
        const left = String(a);
        const right = String(b);
        return left < right ? -1 : left > right ? 1 : 0;
      });
    }
    return keys;
  }

  function formatKey(key) {
    if (typeof key === "symbol") return key.toString();
    if (IDENTIFIER.test(key)) return key;
    return quoteString(String(key));
  }

  function constructorPrefix(value) {
    const proto = Object.getPrototypeOf(value);
    if (proto === null) return "[Object: null prototype] ";
    const ctor = proto.constructor;
    if (typeof ctor === "function" && typeof ctor.name === "string" && ctor.name !== "Object") {
      return ctor.name + " ";
    }
    return "";
  }

  function inspectString(value, options) {
    const max = options.maxStringLength;
    let text;
    if (max !== null && max !== undefined && max >= 0 && value.length > max) {
      text =
        quoteString(value.slice(0, max)) +
        "... " +
        (value.length - max) +
        " more character" +
        (value.length - max === 1 ? "" : "s");
    } else {
      text = quoteString(value);
    }
    return colorize(text, "green", options);
  }

  // Pre-scan for repeated object identity so the first render can be tagged
  // `<ref *N>` and later ones `[Circular *N]`, matching Node's `inspect`.
  function collectRefs(value, visited, refs) {
    if (value === null || (typeof value !== "object" && typeof value !== "function")) return;
    if (visited.has(value)) {
      if (!refs.has(value)) refs.set(value, refs.size + 1);
      return;
    }
    visited.add(value);
    if (typeof value === "function") return;
    const tag = typeTag(value);
    if (
      tag === "[object Date]" ||
      tag === "[object RegExp]" ||
      tag === "[object Error]" ||
      tag === "[object Promise]"
    ) {
      return;
    }
    if (BufferCtor.isBuffer && BufferCtor.isBuffer(value)) return;
    if (ArrayBuffer.isView(value)) return;
    if (Array.isArray(value)) {
      for (let i = 0; i < value.length; i++) {
        if (i in value) collectRefs(value[i], visited, refs);
      }
      return;
    }
    if (tag === "[object Map]") {
      value.forEach((entryValue, entryKey) => {
        collectRefs(entryKey, visited, refs);
        collectRefs(entryValue, visited, refs);
      });
      return;
    }
    if (tag === "[object Set]") {
      value.forEach((entryValue) => collectRefs(entryValue, visited, refs));
      return;
    }
    const keys = Object.keys(value);
    for (let i = 0; i < keys.length; i++) collectRefs(value[keys[i]], visited, refs);
  }

  function inspectArray(value, depth, options, ctx) {
    const length = value.length;
    const max = options.maxArrayLength;
    const limit = max === null || max === undefined || max === Infinity ? length : Math.min(length, max);
    const parts = [];
    for (let i = 0; i < limit; i++) {
      parts.push(i in value ? inspectValue(value[i], depth + 1, options, ctx) : "<1 empty item>");
    }
    let body = parts.join(", ");
    if (limit < length) {
      const remaining = length - limit;
      body += (body.length ? ", " : "") + "... " + remaining + " more item" + (remaining === 1 ? "" : "s");
    }
    if (body.length === 0) return "[]";
    return "[ " + body + " ]";
  }

  function inspectObject(value, depth, options, ctx) {
    const tag = typeTag(value);

    if (tag === "[object Date]") {
      return isNaN(value.getTime()) ? "Invalid Date" : colorize(value.toISOString(), "magenta", options);
    }
    if (tag === "[object RegExp]") return colorize(String(value), "red", options);
    if (tag === "[object Error]") {
      const head = value.name + (value.message ? ": " + value.message : "");
      const stack = typeof value.stack === "string" ? value.stack : "";
      return colorize(stack ? stack : head, "red", options);
    }
    if (tag === "[object Promise]") return "Promise { <pending> }";
    if (tag === "[object Map]") {
      const entries = [];
      value.forEach((entryValue, entryKey) => {
        entries.push(
          inspectValue(entryKey, depth + 1, options, ctx) +
            " => " +
            inspectValue(entryValue, depth + 1, options, ctx),
        );
      });
      return "Map(" + value.size + ")" + (entries.length ? " { " + entries.join(", ") + " }" : " {}");
    }
    if (tag === "[object Set]") {
      const values = [];
      value.forEach((entryValue) => {
        values.push(inspectValue(entryValue, depth + 1, options, ctx));
      });
      return "Set(" + value.size + ")" + (values.length ? " { " + values.join(", ") + " }" : " {}");
    }
    if (tag === "[object ArrayBuffer]") return "ArrayBuffer { byteLength: " + value.byteLength + " }";
    if (tag === "[object SharedArrayBuffer]") {
      return "SharedArrayBuffer { byteLength: " + value.byteLength + " }";
    }
    if (tag === "[object DataView]") {
      return (
        "DataView { byteLength: " +
        value.byteLength +
        ", byteOffset: " +
        value.byteOffset +
        ", byteStride: undefined }"
      );
    }
    if (BufferCtor.isBuffer && BufferCtor.isBuffer(value)) {
      const bytes = [];
      for (let i = 0; i < value.length; i++) {
        const hex = value[i].toString(16);
        bytes.push(hex.length === 1 ? "0" + hex : hex);
      }
      return "<Buffer " + bytes.join(" ") + ">";
    }
    if (ArrayBuffer.isView(value)) {
      const ctor = value.constructor;
      const name = ctor && ctor.name ? ctor.name : "TypedArray";
      const parts = [];
      for (let i = 0; i < value.length; i++) {
        parts.push(inspectValue(value[i], depth + 1, options, ctx));
      }
      return name + "(" + value.length + ") [ " + parts.join(", ") + " ]";
    }
    if (Array.isArray(value)) return inspectArray(value, depth, options, ctx);

    const keys = ownKeys(value, options);
    const prefix = constructorPrefix(value);
    if (keys.length === 0) return prefix + "{}";
    const entries = [];
    for (let i = 0; i < keys.length; i++) {
      const key = keys[i];
      entries.push(formatKey(key) + ": " + inspectValue(value[key], depth + 1, options, ctx));
    }
    return prefix + "{ " + entries.join(", ") + " }";
  }

  function inspectValue(value, depth, options, ctx) {
    if (
      options.customInspect !== false &&
      value !== null &&
      (typeof value === "object" || typeof value === "function")
    ) {
      const custom = value[inspectCustom];
      if (typeof custom === "function") {
        const rendered = custom.call(value, depth, options, inspect);
        if (typeof rendered === "string") return rendered;
        return inspectValue(rendered, depth, options, ctx);
      }
    }

    const kind = typeof value;
    if (kind === "function") return functionLabel(value);
    if (kind !== "object" || value === null) {
      if (kind === "string") return inspectString(value, options);
      return formatPrimitive(value, options);
    }

    if (ctx.refs.has(value) && ctx.renderedRefs.has(value)) {
      return "[Circular *" + ctx.refs.get(value) + "]";
    }
    if (depth > options.depth) return Array.isArray(value) ? "[Array]" : "[Object]";

    let prefix = "";
    if (ctx.refs.has(value)) {
      ctx.renderedRefs.add(value);
      prefix = "<ref *" + ctx.refs.get(value) + "> ";
    }
    return prefix + inspectObject(value, depth, options, ctx);
  }

  function normalizeInspectOptions(options) {
    if (options === undefined || options === null) return DEFAULT_INSPECT_OPTIONS;
    if (typeof options === "boolean") {
      return Object.assign({}, DEFAULT_INSPECT_OPTIONS, { showHidden: options });
    }
    if (typeof options === "object") {
      return Object.assign({}, DEFAULT_INSPECT_OPTIONS, options);
    }
    return DEFAULT_INSPECT_OPTIONS;
  }

  function inspect(value, options) {
    const resolved = normalizeInspectOptions(options);
    const refs = new Map();
    collectRefs(value, new Set(), refs);
    return inspectValue(value, 0, resolved, { refs: refs, renderedRefs: new Set() });
  }
  inspect.custom = inspectCustom;
  inspect.defaultOptions = Object.assign({}, DEFAULT_INSPECT_OPTIONS, {
    showProxy: false,
    numericSeparator: false,
  });

  // -------------------------------------------------------------------------
  // format / formatWithOptions
  // -------------------------------------------------------------------------

  function formatWithOptions(inspectOptions, ...args) {
    const options = normalizeInspectOptions(inspectOptions);
    if (args.length === 0) return "";
    const first = args[0];
    if (typeof first !== "string") {
      const rendered = [];
      for (let i = 0; i < args.length; i++) {
        const arg = args[i];
        rendered.push(typeof arg === "string" ? arg : inspect(arg, options));
      }
      return rendered.join(" ");
    }

    let output = "";
    let cursor = 0;
    let next = 1;
    const pattern = /%([sdifjoOc%])/g;
    let match;
    while ((match = pattern.exec(first)) !== null) {
      output += first.slice(cursor, match.index);
      cursor = match.index + match[0].length;
      const specifier = match[1];
      if (specifier === "%") {
        output += "%";
        continue;
      }
      if (next >= args.length) {
        output += match[0];
        continue;
      }
      const arg = args[next++];
      if (specifier === "s") {
        if (typeof arg === "string") output += arg;
        else if (arg !== null && typeof arg === "object") {
          output += inspect(arg, Object.assign({}, options, { depth: 0 }));
        } else output += String(arg);
      } else if (specifier === "d") {
        output += typeof arg === "bigint" ? String(arg) : String(Number(arg));
      } else if (specifier === "i") {
        output += String(parseInt(arg, 10));
      } else if (specifier === "f") {
        output += String(parseFloat(arg));
      } else if (specifier === "j") {
        try {
          output += JSON.stringify(arg);
        } catch (_e) {
          output += "[Circular]";
        }
      } else if (specifier === "o") {
        output += inspect(arg, Object.assign({}, options, { showHidden: true, depth: 4 }));
      } else if (specifier === "O") {
        output += inspect(arg, options);
      }
      // `%c` consumes its argument and renders nothing.
    }
    output += first.slice(cursor);
    for (; next < args.length; next++) {
      const arg = args[next];
      output += " " + (typeof arg === "string" ? arg : inspect(arg, options));
    }
    return output;
  }

  function format(...args) {
    return formatWithOptions({}, ...args);
  }

  // -------------------------------------------------------------------------
  // promisify / callbackify
  // -------------------------------------------------------------------------

  function promisify(original) {
    if (typeof original !== "function") {
      throw invalidArgType("original", "function", original);
    }
    if (original[promisifyCustom] !== undefined) return original[promisifyCustom];

    function fn(...args) {
      const self = this;
      return new Promise((resolve, reject) => {
        original.call(self, ...args, (err, value) => {
          if (err) reject(err);
          else resolve(value);
        });
      });
    }
    Object.setPrototypeOf(fn, Object.getPrototypeOf(original));
    Object.defineProperty(fn, "name", { value: original.name, configurable: true });
    return fn;
  }
  promisify.custom = promisifyCustom;

  function callbackify(original) {
    if (typeof original !== "function") {
      throw invalidArgType("original", "function", original);
    }
    if (original[callbackifyCustom] !== undefined) return original[callbackifyCustom];

    function callbackified(...args) {
      const callback = args.pop();
      if (typeof callback !== "function") {
        throw invalidArgType("callback", "function", callback);
      }
      const self = this;
      original.apply(self, args).then(
        (value) => {
          __pi_schedule(() => callback(null, value));
        },
        (err) => {
          __pi_schedule(() => {
            const reason =
              err === null || err === undefined
                ? new Error("Promise rejected with no or falsy value")
                : err;
            callback(reason);
          });
        },
      );
    }
    Object.setPrototypeOf(callbackified, Object.getPrototypeOf(original));
    Object.defineProperty(callbackified, "name", { value: original.name, configurable: true });
    return callbackified;
  }
  callbackify.custom = callbackifyCustom;

  // -------------------------------------------------------------------------
  // inherits / deprecate
  // -------------------------------------------------------------------------

  function inherits(ctor, superCtor) {
    if (typeof ctor !== "function") throw invalidArgType("ctor", "function", ctor);
    if (typeof superCtor !== "function") throw invalidArgType("superCtor", "function", superCtor);
    ctor.super_ = superCtor;
    ctor.prototype = Object.create(superCtor.prototype, {
      constructor: { value: ctor, enumerable: false, writable: true, configurable: true },
    });
    Object.setPrototypeOf(ctor, superCtor);
  }

  function deprecate(fn, msg, code) {
    if (typeof fn !== "function") throw invalidArgType("fn", "function", fn);
    let warned = false;
    function deprecated(...args) {
      if (!warned) {
        warned = true;
        const text =
          (typeof code === "string" && code ? "[" + code + "] " : "") +
          "DeprecationWarning: " +
          String(msg);
        if (typeof globalThis.host_log === "function") globalThis.host_log("warn", text);
        else if (typeof console !== "undefined" && typeof console.warn === "function") console.warn(text);
      }
      return fn.apply(this, args);
    }
    Object.setPrototypeOf(deprecated, fn);
    Object.defineProperty(deprecated, "name", { value: fn.name || "deprecated", configurable: true });
    return deprecated;
  }

  // -------------------------------------------------------------------------
  // stripVTControlCharacters / styleText
  // -------------------------------------------------------------------------

  // Node's own pattern (lib/internal/util/inspect.js).
  const VT_PATTERN = new RegExp(
    "[\\u001B\\u009B][[\\]()#;?]*(?:(?:(?:(?:;[-a-zA-Z\\d\\/#&.:=?%@~_]+)*|[a-zA-Z\\d]+(?:;[-a-zA-Z\\d\\/#&.:=?%@~_]*)*)?\\u0007)|(?:(?:\\d{1,4}(?:;\\d{0,4})*)?[\\dA-PR-TZcf-nq-uy=><~]))",
    "g",
  );

  function stripVTControlCharacters(str) {
    if (typeof str !== "string") throw invalidArgType("str", "string", str);
    return str.replace(VT_PATTERN, "");
  }

  // [open, close] pairs copied from Node's `styleText` palette.
  const STYLE_CODES = {
    reset: [0, 0],
    bold: [1, 22],
    dim: [2, 22],
    italic: [3, 23],
    underline: [4, 24],
    blink: [5, 25],
    inverse: [7, 27],
    hidden: [8, 28],
    strikethrough: [9, 29],
    doubleunderline: [21, 21],
    framed: [51, 54],
    overlined: [53, 55],
    black: [30, 39],
    red: [31, 39],
    green: [32, 39],
    yellow: [33, 39],
    blue: [34, 39],
    magenta: [35, 39],
    cyan: [36, 39],
    white: [37, 39],
    gray: [90, 39],
    grey: [90, 39],
    blackbright: [90, 39],
    redbright: [91, 39],
    greenbright: [92, 39],
    yellowbright: [93, 39],
    bluebright: [94, 39],
    magentabright: [95, 39],
    cyanbright: [96, 39],
    whitebright: [97, 39],
    bgblack: [40, 49],
    bgred: [41, 49],
    bggreen: [42, 49],
    bgyellow: [43, 49],
    bgblue: [44, 49],
    bgmagenta: [45, 49],
    bgcyan: [46, 49],
    bgwhite: [47, 49],
    bggray: [100, 49],
    bggrey: [100, 49],
    bgblackbright: [100, 49],
    bgredbright: [101, 49],
    bggreenbright: [102, 49],
    bgyellowbright: [103, 49],
    bgbluebright: [104, 49],
    bgmagentabright: [105, 49],
    bgcyanbright: [106, 49],
    bgwhitebright: [107, 49],
  };

  function styleText(format, text, options) {
    if (text === undefined) {
      throw invalidArgType("text", "string", text);
    }
    const body = String(text);
    const formats = Array.isArray(format) ? format : [format];
    if (formats.length === 0) return body;

    const opens = [];
    const closes = [];
    for (let i = 0; i < formats.length; i++) {
      const label = String(formats[i]).toLowerCase();
      if (!hasOwn(STYLE_CODES, label)) {
        throw invalidArgValue(
          "format",
          formats[i],
          "one of the supported style names (e.g. 'red', 'bold', 'bgBlue')",
        );
      }
      opens.push("\u001b[" + STYLE_CODES[label][0] + "m");
    }
    for (let i = formats.length - 1; i >= 0; i--) {
      closes.push("\u001b[" + STYLE_CODES[String(formats[i]).toLowerCase()][1] + "m");
    }
    // Node emits one sequence per style, closes in reverse order.
    return opens.join("") + body + closes.join("");
  }

  // -------------------------------------------------------------------------
  // debuglog
  // -------------------------------------------------------------------------

  const debugSections = String(
    (__pi_process_module.env && __pi_process_module.env.NODE_DEBUG) || "",
  )
    .split(/[\s,]+/)
    .filter(Boolean);

  function debuglog(section) {
    const name = String(section);
    if (debugSections.indexOf(name) === -1 && debugSections.indexOf("*") === -1) {
      return function () {};
    }
    return function (...args) {
      const text = name.toUpperCase() + " " + format(...args);
      if (typeof globalThis.host_log === "function") globalThis.host_log("debug", text);
    };
  }

  function log(...args) {
    const stamp = new Date().toISOString().replace("T", " ").replace("Z", "");
    if (typeof globalThis.host_log === "function") globalThis.host_log("info", stamp + " - " + format(...args));
  }

  // -------------------------------------------------------------------------
  // types
  // -------------------------------------------------------------------------

  function isTypedArray(value) {
    return ArrayBuffer.isView(value) && !(value instanceof DataView);
  }

  const BOXED_TAGS = [
    "[object Boolean]",
    "[object Number]",
    "[object String]",
    "[object Symbol]",
    "[object BigInt]",
  ];

  const types = {
    isAnyArrayBuffer: (value) => {
      const tag = typeTag(value);
      return tag === "[object ArrayBuffer]" || tag === "[object SharedArrayBuffer]";
    },
    isArgumentsObject: (value) => typeTag(value) === "[object Arguments]",
    isArrayBuffer: (value) => typeTag(value) === "[object ArrayBuffer]",
    isArrayBufferView: (value) => ArrayBuffer.isView(value),
    isAsyncFunction: (value) =>
      typeTag(value) === "[object AsyncFunction]" ||
      (typeof value === "function" &&
        value.constructor &&
        value.constructor.name === "AsyncFunction"),
    isBigInt64Array: (value) => typeTag(value) === "[object BigInt64Array]",
    isBigIntObject: (value) => typeTag(value) === "[object BigInt]",
    isBigUint64Array: (value) => typeTag(value) === "[object BigUint64Array]",
    isBooleanObject: (value) => typeTag(value) === "[object Boolean]",
    isBoxedPrimitive: (value) =>
      typeof value === "object" && value !== null && BOXED_TAGS.indexOf(typeTag(value)) !== -1,
    isCryptoKey: () => false,
    isDataView: (value) => typeTag(value) === "[object DataView]",
    isDate: (value) => typeTag(value) === "[object Date]",
    isExternal: () => false,
    isFloat16Array: (value) => typeTag(value) === "[object Float16Array]",
    isFloat32Array: (value) => typeTag(value) === "[object Float32Array]",
    isFloat64Array: (value) => typeTag(value) === "[object Float64Array]",
    isGeneratorFunction: (value) =>
      typeTag(value) === "[object GeneratorFunction]" ||
      (typeof value === "function" &&
        value.constructor &&
        value.constructor.name === "GeneratorFunction"),
    isGeneratorObject: (value) => {
      const tag = typeTag(value);
      return tag === "[object Generator]" || tag === "[object AsyncGenerator]";
    },
    isInt8Array: (value) => typeTag(value) === "[object Int8Array]",
    isInt16Array: (value) => typeTag(value) === "[object Int16Array]",
    isInt32Array: (value) => typeTag(value) === "[object Int32Array]",
    isKeyObject: () => false,
    isMap: (value) => typeTag(value) === "[object Map]",
    isMapIterator: (value) => typeTag(value) === "[object Map Iterator]",
    isModuleNamespaceObject: () => false,
    isNativeError: (value) => {
      const tag = typeTag(value);
      return tag === "[object Error]" || value instanceof Error;
    },
    isNumberObject: (value) => typeTag(value) === "[object Number]",
    isPromise: (value) => typeTag(value) === "[object Promise]",
    isProxy: () => false,
    isRegExp: (value) => typeTag(value) === "[object RegExp]",
    isSet: (value) => typeTag(value) === "[object Set]",
    isSetIterator: (value) => typeTag(value) === "[object Set Iterator]",
    isSharedArrayBuffer: (value) => typeTag(value) === "[object SharedArrayBuffer]",
    isStringObject: (value) => typeTag(value) === "[object String]",
    isSymbolObject: (value) => typeTag(value) === "[object Symbol]",
    isTypedArray: isTypedArray,
    isUint8Array: (value) => typeTag(value) === "[object Uint8Array]",
    isUint8ClampedArray: (value) => typeTag(value) === "[object Uint8ClampedArray]",
    isUint16Array: (value) => typeTag(value) === "[object Uint16Array]",
    isUint32Array: (value) => typeTag(value) === "[object Uint32Array]",
    isWeakMap: (value) => typeTag(value) === "[object WeakMap]",
    isWeakSet: (value) => typeTag(value) === "[object WeakSet]",
  };

  // -------------------------------------------------------------------------
  // legacy predicates
  // -------------------------------------------------------------------------

  const legacy = {
    isArray: (value) => Array.isArray(value),
    isBoolean: (value) => typeof value === "boolean",
    isBuffer: (value) => !!(BufferCtor.isBuffer && BufferCtor.isBuffer(value)),
    isDate: (value) => typeTag(value) === "[object Date]",
    isError: (value) => {
      const tag = typeTag(value);
      return tag === "[object Error]" || value instanceof Error;
    },
    isFunction: (value) => typeof value === "function",
    isNull: (value) => value === null,
    isNullOrUndefined: (value) => value === null || value === undefined,
    isNumber: (value) => typeof value === "number",
    isObject: (value) => typeof value === "object" && value !== null,
    isPrimitive: (value) => value === null || (typeof value !== "object" && typeof value !== "function"),
    isRegExp: (value) => typeTag(value) === "[object RegExp]",
    isString: (value) => typeof value === "string",
    isSymbol: (value) => typeof value === "symbol",
    isUndefined: (value) => value === undefined,
  };

  // -------------------------------------------------------------------------
  // isDeepStrictEqual
  // -------------------------------------------------------------------------

  function enumerableOwnKeys(value) {
    const keys = Reflect.ownKeys(value);
    const result = [];
    for (let i = 0; i < keys.length; i++) {
      const descriptor = Object.getOwnPropertyDescriptor(value, keys[i]);
      if (descriptor && descriptor.enumerable) result.push(keys[i]);
    }
    return result;
  }

  function prototypeCtor(value) {
    const proto = Object.getPrototypeOf(value);
    return proto === null ? null : proto.constructor;
  }

  function deepEqual(a, b, seen) {
    if (Object.is(a, b)) return true;
    if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) return false;

    const tag = typeTag(a);
    if (tag !== typeTag(b)) return false;

    const memo = seen.get(a);
    if (memo !== undefined) return memo === b;
    seen.set(a, b);

    if (tag === "[object Date]") return Object.is(a.getTime(), b.getTime());
    if (tag === "[object RegExp]") return a.source === b.source && a.flags === b.flags;
    if (tag === "[object Number]" || tag === "[object String]" || tag === "[object Boolean]") {
      return Object.is(a.valueOf(), b.valueOf());
    }
    if (tag === "[object Symbol]" || tag === "[object BigInt]") {
      return Object.is(a.valueOf(), b.valueOf());
    }
    if (tag === "[object Error]") {
      if (a.name !== b.name || a.message !== b.message) return false;
    }
    if (tag === "[object Map]") {
      if (a.size !== b.size) return false;
      const entries = [];
      b.forEach((entryValue, entryKey) => entries.push([entryKey, entryValue]));
      const used = [];
      for (let i = 0; i < entries.length; i++) used.push(false);
      let matchedAll = true;
      a.forEach((entryValue, entryKey) => {
        if (!matchedAll) return;
        let matched = false;
        for (let i = 0; i < entries.length; i++) {
          if (used[i]) continue;
          if (
            deepEqual(entryKey, entries[i][0], seen) &&
            deepEqual(entryValue, entries[i][1], seen)
          ) {
            used[i] = true;
            matched = true;
            break;
          }
        }
        if (!matched) matchedAll = false;
      });
      return matchedAll;
    }
    if (tag === "[object Set]") {
      if (a.size !== b.size) return false;
      const values = [];
      b.forEach((entryValue) => values.push(entryValue));
      const used = [];
      for (let i = 0; i < values.length; i++) used.push(false);
      let matchedAll = true;
      a.forEach((entryValue) => {
        if (!matchedAll) return;
        let matched = false;
        for (let i = 0; i < values.length; i++) {
          if (used[i]) continue;
          if (deepEqual(entryValue, values[i], seen)) {
            used[i] = true;
            matched = true;
            break;
          }
        }
        if (!matched) matchedAll = false;
      });
      return matchedAll;
    }
    if (tag === "[object ArrayBuffer]" || tag === "[object SharedArrayBuffer]") {
      if (a.byteLength !== b.byteLength) return false;
      const left = new Uint8Array(a);
      const right = new Uint8Array(b);
      for (let i = 0; i < left.length; i++) {
        if (left[i] !== right[i]) return false;
      }
      return true;
    }
    if (tag === "[object DataView]") {
      if (a.byteLength !== b.byteLength) return false;
      const left = new Uint8Array(a.buffer, a.byteOffset, a.byteLength);
      const right = new Uint8Array(b.buffer, b.byteOffset, b.byteLength);
      for (let i = 0; i < left.length; i++) {
        if (left[i] !== right[i]) return false;
      }
      return true;
    }
    if (isTypedArray(a)) {
      if (a.length !== b.length) return false;
      for (let i = 0; i < a.length; i++) {
        if (!Object.is(a[i], b[i])) return false;
      }
      return true;
    }
    if (Array.isArray(a) && a.length !== b.length) return false;
    if (tag === "[object Object]" && prototypeCtor(a) !== prototypeCtor(b)) return false;

    const keysA = enumerableOwnKeys(a);
    const keysB = enumerableOwnKeys(b);
    if (keysA.length !== keysB.length) return false;
    for (let i = 0; i < keysA.length; i++) {
      const key = keysA[i];
      if (!hasOwn(b, key)) return false;
      if (!deepEqual(a[key], b[key], seen)) return false;
    }
    return true;
  }

  function isDeepStrictEqual(value1, value2) {
    return deepEqual(value1, value2, new Map());
  }

  // -------------------------------------------------------------------------
  // toUSVString
  // -------------------------------------------------------------------------

  function toUSVString(value) {
    const input = String(value);
    let output = "";
    for (let i = 0; i < input.length; i++) {
      const code = input.charCodeAt(i);
      if (code >= 0xd800 && code <= 0xdbff) {
        const next = i + 1 < input.length ? input.charCodeAt(i + 1) : 0;
        if (next >= 0xdc00 && next <= 0xdfff) {
          output += input[i] + input[i + 1];
          i += 1;
        } else {
          output += "\ufffd";
        }
      } else if (code >= 0xdc00 && code <= 0xdfff) {
        output += "\ufffd";
      } else {
        output += input[i];
      }
    }
    return output;
  }

  // -------------------------------------------------------------------------
  // TextEncoder / TextDecoder
  // -------------------------------------------------------------------------

  class TextEncoder {
    get encoding() {
      return "utf-8";
    }

    encode(input) {
      return new Uint8Array(BufferCtor.from(input === undefined ? "" : String(input), "utf8"));
    }

    encodeInto(source, destination) {
      if (!(destination instanceof Uint8Array)) {
        throw invalidArgType("destination", "Uint8Array", destination);
      }
      const input = source === undefined ? "" : String(source);
      let read = 0;
      let written = 0;
      while (read < input.length) {
        const code = input.charCodeAt(read);
        const charCode = code >= 0xd800 && code <= 0xdbff && read + 1 < input.length ? 2 : 1;
        const bytes = BufferCtor.from(input.substr(read, charCode), "utf8");
        if (written + bytes.length > destination.length) break;
        destination.set(bytes, written);
        written += bytes.length;
        read += charCode;
      }
      return { read: read, written: written };
    }
  }

  const DECODER_ENCODINGS = {
    "utf-8": "utf-8",
    "utf8": "utf-8",
    "unicode-1-1-utf-8": "utf-8",
    "utf-16le": "utf-16le",
    "utf-16": "utf-16le",
    "ucs-2": "utf-16le",
    "unicode": "utf-16le",
    "unicodefeff": "utf-16le",
    "iso-10646-ucs-2": "utf-16le",
    "csunicode": "utf-16le",
    "latin1": "windows-1252",
    "ascii": "windows-1252",
    "windows-1252": "windows-1252",
    "iso-8859-1": "windows-1252",
    "iso8859-1": "windows-1252",
    "iso88591": "windows-1252",
    "cp1252": "windows-1252",
    "cp819": "windows-1252",
    "ibm819": "windows-1252",
    "l1": "windows-1252",
    "us-ascii": "windows-1252",
    "ansi_x3.4-1968": "windows-1252",
    "csisolatin1": "windows-1252",
    "iso-ir-100": "windows-1252",
    "iso_8859-1": "windows-1252",
    "iso_8859-1:1987": "windows-1252",
    "x-cp1252": "windows-1252",
  };

  const CP1252_HIGH =
    "\u20ac\u0081\u201a\u0192\u201e\u2026\u2020\u2021\u02c6\u2030\u0160\u2039\u0152\u008d\u017d\u008f" +
    "\u0090\u2018\u2019\u201c\u201d\u2022\u2013\u2014\u02dc\u2122\u0161\u203a\u0153\u009d\u017e\u0178";

  function decodeWindows1252(bytes) {
    const parts = [];
    for (let i = 0; i < bytes.length; i++) {
      const byte = bytes[i];
      parts.push(
        byte >= 0x80 && byte <= 0x9f ? CP1252_HIGH.charAt(byte - 0x80) : String.fromCharCode(byte),
      );
    }
    return parts.join("");
  }

  class TextDecoder {
    constructor(label, options) {
      const requested = label === undefined || label === null ? "utf-8" : String(label).toLowerCase();
      const encoding = hasOwn(DECODER_ENCODINGS, requested) ? DECODER_ENCODINGS[requested] : null;
      if (!encoding) {
        const err = new RangeError(
          'The "label" argument must be a valid encoding label. Received ' + JSON.stringify(label),
        );
        err.code = "ERR_ENCODING_NOT_SUPPORTED";
        throw err;
      }
      Object.defineProperty(this, "encoding", { value: encoding, enumerable: true });
      Object.defineProperty(this, "fatal", { value: !!(options && options.fatal), enumerable: true });
      Object.defineProperty(this, "ignoreBOM", {
        value: !!(options && options.ignoreBOM),
        enumerable: true,
      });
    }

    decode(input) {
      if (input === undefined) return "";
      let bytes;
      if (input instanceof Uint8Array) bytes = input;
      else if (input instanceof ArrayBuffer) bytes = new Uint8Array(input);
      else if (ArrayBuffer.isView(input)) {
        bytes = new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
      } else if (typeof input === "string") {
        bytes = BufferCtor.from(input, "utf8");
      } else {
        throw invalidArgType("input", "ArrayBuffer, Buffer, TypedArray, or DataView", input);
      }

      let text;
      if (this.encoding === "utf-8") text = BufferCtor.from(bytes).toString("utf8");
      else if (this.encoding === "utf-16le") text = BufferCtor.from(bytes).toString("utf16le");
      else text = decodeWindows1252(bytes);
      if (!this.ignoreBOM && text.charCodeAt(0) === 0xfeff) text = text.slice(1);
      return text;
    }
  }

  // -------------------------------------------------------------------------

  const mod = Object.assign({}, legacy, {
    format: format,
    formatWithOptions: formatWithOptions,
    inspect: inspect,
    promisify: promisify,
    callbackify: callbackify,
    inherits: inherits,
    deprecate: deprecate,
    stripVTControlCharacters: stripVTControlCharacters,
    styleText: styleText,
    debuglog: debuglog,
    debug: debuglog,
    log: log,
    types: types,
    isDeepStrictEqual: isDeepStrictEqual,
    toUSVString: toUSVString,
    // `parseArgs`, `parseEnv`, `diff`, `aborted`, `transferableAbort*`,
    // `getSystemError*` and `getCallSite(s)` are intentionally absent; see
    // `docs/NODE_BUILTINS.md` for the rationale.
    _extend: (origin, add) => Object.assign(origin, add || {}),
    TextEncoder: TextEncoder,
    TextDecoder: TextDecoder,
  });
  mod.default = mod;
  return Object.freeze(mod);
})();

// `TextEncoder` / `TextDecoder` are globals on Node; expose them here when the
// engine has no native implementation (QuickJS does not).
if (typeof globalThis.TextEncoder === "undefined") globalThis.TextEncoder = __pi_util_module.TextEncoder;
if (typeof globalThis.TextDecoder === "undefined") globalThis.TextDecoder = __pi_util_module.TextDecoder;

// Built last so the module objects above are initialized before they are
// referenced (a `const` declared later in the file would otherwise throw
// a TDZ ReferenceError here).
globalThis.__pi_virtual_modules = Object.freeze({
  "node:path": __pi_path_module,
  path: __pi_path_module,
  "node:url": __pi_url_module,
  url: __pi_url_module,
  "node:fs": __pi_fs_module,
  fs: __pi_fs_module,
  "node:fs/promises": __pi_fs_module.promises,
  "fs/promises": __pi_fs_module.promises,
  "node:os": __pi_os_module,
  os: __pi_os_module,
  "node:buffer": __pi_buffer_module,
  buffer: __pi_buffer_module,
  "node:crypto": __pi_crypto_module,
  crypto: __pi_crypto_module,
  "node:process": __pi_process_module,
  process: __pi_process_module,
  "node:util": __pi_util_module,
  util: __pi_util_module,
  typebox: __pi_typebox_module,
  "@sinclair/typebox": __pi_typebox_module,
});

// `Buffer` and `process` are Node globals, not just module exports: upstream
// examples use them without importing (`notify.ts` reads `process.env`,
// `subagent/index.ts` calls `Buffer.byteLength`). Only install them when
// nothing else defined the name.
if (typeof globalThis.Buffer === "undefined") {
  globalThis.Buffer = __pi_buffer_module.Buffer;
}
if (typeof globalThis.process === "undefined") {
  globalThis.process = __pi_process_module;
}
