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
