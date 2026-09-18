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
//   - `_pi_known_event_names()`   — list event names with at least one
//     subscriber.
//   - `_pi_load_extension(source)` — evaluate an extension source and
//     call its default export with `pi`. The source is wrapped so a
//     CommonJS-style `module.exports = function (pi) { ... }` works,
//     matching how upstream pi extensions are wired (the TS source
//     compiles to that shape).

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
    _pi.tools.set(name, {
      name,
      label,
      description,
      parameters: paramsJson,
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

globalThis._pi_known_event_names = function _pi_known_event_names() {
  return JSON.stringify({ events: Object.keys(_pi.handlers) });
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
  const ctx = buildCtx({ mode: "rpc", hasUI: false });
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
 * The source is treated as a CommonJS-shaped module: we wrap it so
 * `module.exports = function (pi) { ... }` is the contract. This
 * matches the shape the upstream pi extensions compile to.
 *
 * Stage 3 ships only this minimal module wrapper — supporting the
 * full ESM pipeline lands in a later stage alongside TypeBox schema
 * validation.
 *
 * @param {string} source
 */
globalThis._pi_load_extension = function _pi_load_extension(source) {
  if (typeof source !== "string") {
    throw new TypeError("_pi_load_extension: source must be a string");
  }
  const factoryFn = new Function(
    "module",
    "exports",
    "pi",
    source +
      "\n;return (typeof module.exports === 'function') ? module.exports : ((typeof exports === 'function') ? exports : undefined);",
  );
  const module = { exports: undefined };
  const exports = {};
  const factory = factoryFn(module, exports, pi);
  if (typeof factory !== "function") {
    throw new Error("extension source did not export a factory function");
  }
  factory(pi);
  _pi.loadedCount += 1;
  return JSON.stringify({ loaded: true });
};