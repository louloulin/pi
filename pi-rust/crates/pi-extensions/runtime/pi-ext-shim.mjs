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
//   - `host_ui_region(op, payloadJson)`  — the single synchronous bridge
//     behind `ctx.ui.setWidget` / `setHeader` / `setFooter` /
//     `setEditorComponent` / `custom`. Returns a JSON envelope
//     (`{ok:true}` / `{ok:true,session}` / `{ok:false,error}`).
//     Components are registered here under numeric ids; the host calls
//     back into `__pi_ui_render_component` / `__pi_ui_component_input` /
//     `__pi_ui_dispose_component`.
//   - `host_log(level, message)`         — surface a log line
//   - `host_child_read(handle, stream)` — return Promise<{data,done}> for a
//     `node:child_process` pipe (see `child_process.spawn`)
//   - `host_child_wait(handle)`          — return Promise<{code,signal,…}>
//   - `host_pi_ai_stream_start(requestJson)` — start one built-in pi-ai
//     provider stream; return Promise<{ok,id}> / Promise<{ok:false,error}>
//   - `host_pi_ai_stream_next(id)`       — return the next provider event as
//     Promise<{ok,done,event}>
//   - `host_pi_ai_stream_cancel(id)`     — abort and release that stream
////
// Internal entry points exposed on `globalThis._pi_host`:
//   - `_pi_dispatch(eventJson)`   — deliver an `ExtensionEvent` payload
//     to subscribed handlers; returns a JSON string with the dispatch
//     summary so the host can inspect what ran.
//   - `_pi_registered_tools()`    — list tool names registered so far.
//   - `_pi_registered_tool_prompts()` — list each tool's `promptSnippet`
//     / `promptGuidelines` contribution for the system prompt.
//   - `_pi_registered_commands()` — list slash commands registered so far.
//   - `_pi_execute_command(name, args, ctxJson)` — run a command handler.
//   - `_pi_provider_stream_simple(name, modelJson, contextJson,
//     optionsJson)` — run a provider's registered `streamSimple` handler and
//     collect its events; resolves to `{ ok, events }` / `{ ok:false, error }`.
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
//     `node:*` virtual modules (fs / os / buffer / crypto / process /
//     child_process).
//     Returns a JSON envelope, never throws; see the `node:*` section
//     near the bottom of this file.
//   - `host_exec_cancel(id)`           — cancel a running `pi.exec`; see
//     `pi.exec` below and the `AbortController` polyfill.

// ---------------------------------------------------------------------------
// `AbortController` / `AbortSignal`
//
// QuickJS ships neither, so the shim installs a minimal polyfill before any
// extension source runs. It is deliberately the subset the upstream
// extension API needs (`packages/coding-agent/src/core/extensions/types.ts`
// threads an `AbortSignal` through `pi.exec`, `ctx.ui.*` and the provider
// hooks): `aborted`, `reason`, `throwIfAborted()`, `addEventListener(
// "abort")` / `removeEventListener`, `onabort`, plus the `AbortSignal.abort()`
// and `AbortSignal.any()` statics.
//
// `AbortSignal.timeout(ms)` is **not** implemented: it needs a timer, and
// the host exposes no `setTimeout` / `node:timers` surface. It is a
// documented gap rather than a silent one — see `docs/EXTENSIONS.md`.
// ---------------------------------------------------------------------------

/** The `AbortError` a bare `controller.abort()` rejects/throws with. */
function makeAbortError(message) {
  const error = new Error(message || "This operation was aborted");
  error.name = "AbortError";
  error.code = "ABORT_ERR";
  return error;
}

class AbortSignalPolyfill {
  constructor() {
    this.aborted = false;
    this.reason = undefined;
    this.onabort = null;
    this._listeners = new Set();
  }

  addEventListener(type, listener, options) {
    if (type !== "abort" || typeof listener !== "function") return;
    // `{ once: true }` needs no bookkeeping: the list is emptied when the
    // signal fires, and a signal fires at most once.
    void options;
    this._listeners.add(listener);
  }

  removeEventListener(type, listener) {
    if (type !== "abort") return;
    this._listeners.delete(listener);
  }

  throwIfAborted() {
    if (this.aborted) {
      throw this.reason === undefined ? makeAbortError() : this.reason;
    }
  }

  /** @internal Fire the signal. No-op when already aborted. */
  _fire(reason) {
    if (this.aborted) return;
    this.aborted = true;
    this.reason = reason === undefined ? makeAbortError() : reason;
    const listeners = Array.from(this._listeners);
    this._listeners.clear();
    const event = { type: "abort", target: this };
    for (const listener of listeners) {
      try {
        listener.call(this, event);
      } catch (_e) {
        // DOM `dispatchEvent` isolates listener errors; so do we.
      }
    }
    if (typeof this.onabort === "function") {
      try {
        this.onabort.call(this, event);
      } catch (_e) {
        // See above.
      }
    }
  }
}

AbortSignalPolyfill.abort = function abort(reason) {
  const signal = new AbortSignalPolyfill();
  signal._fire(reason);
  return signal;
};

AbortSignalPolyfill.any = function any(signals) {
  const combined = new AbortSignalPolyfill();
  for (const signal of signals == null ? [] : signals) {
    if (!signal) continue;
    if (signal.aborted) {
      combined._fire(signal.reason);
      break;
    }
    if (typeof signal.addEventListener === "function") {
      signal.addEventListener("abort", () => combined._fire(signal.reason));
    }
  }
  return combined;
};

class AbortControllerPolyfill {
  constructor() {
    this.signal = new AbortSignalPolyfill();
  }

  abort(reason) {
    this.signal._fire(reason);
  }
}

// Never shadow an engine-provided implementation (QuickJS has none today,
// but a future engine bump may add one).
if (typeof globalThis.AbortSignal === "undefined") {
  globalThis.AbortSignal = AbortSignalPolyfill;
}
if (typeof globalThis.AbortController === "undefined") {
  globalThis.AbortController = AbortControllerPolyfill;
}

// Monotonic id handshake for `pi.exec` cancellation: the shim allocates,
// `host_exec` registers, `host_exec_cancel` addresses.
let __pi_next_exec_id = 1;

const _pi = {
  /** @type {Record<string, Array<(event: any, ctx: any) => any>>} */
  handlers: {},
  /** @type {Map<string, {name:string,label:string,description:string,parameters:any}>} */
  tools: new Map(),
  /** @type {Map<string, {name:string,description?:string}>} */
  commands: new Map(),
  /**
   * Providers registered with `pi.registerProvider`, keyed by provider id.
   * Holds the parts of a registration that cannot cross the host ABI:
   * `streamSimple` (and, once a login flow is wired, the `oauth` callbacks).
   * @type {Map<string, {name:string, streamSimple:Function|null, oauth:any}>}
   */
  providers: new Map(),
  loadedCount: 0,
};

/**
 * Normalise an extension `oauth` block (or a native provider's
 * `auth.oauth`) into the flag form the host ABI carries: functions stay in
 * the shim, the host only learns what the block can do.
 *
 * Upstream OAuth blocks are `{ name, isSubscription, usesCallbackServer,
 * login, refreshToken, getApiKey, modifyModels }` for provider configs and
 * `{ name, login, refresh, toAuth }` on the native `Provider` auth object;
 * both spellings are accepted.
 *
 * @returns {object|null}
 */
function normalizeOAuthBlock(oauth) {
  if (!oauth || typeof oauth !== "object") return null;
  return {
    name: typeof oauth.name === "string" ? oauth.name : "",
    isSubscription: oauth.isSubscription === true,
    usesCallbackServer: oauth.usesCallbackServer === true,
    hasLogin: typeof oauth.login === "function",
    hasRefreshToken:
      typeof oauth.refreshToken === "function" || typeof oauth.refresh === "function",
    hasGetApiKey:
      typeof oauth.getApiKey === "function" || typeof oauth.toAuth === "function",
    hasModifyModels: typeof oauth.modifyModels === "function",
  };
}

/**
 * Collect an `AssistantMessageEventStream` (or any async iterable, array or
 * promise of one) into a plain array of upstream-shaped events.
 */
async function collectProviderEvents(stream) {
  if (stream == null) return [];
  if (typeof stream.then === "function") return collectProviderEvents(await stream);
  if (Array.isArray(stream)) return stream;
  if (typeof stream[Symbol.asyncIterator] === "function") {
    const events = [];
    for await (const event of stream) events.push(event);
    return events;
  }
  if (typeof stream.collect === "function") {
    const collected = await stream.collect();
    return Array.isArray(collected) ? collected : [];
  }
  throw new TypeError(
    "streamSimple must return an AssistantMessageEventStream (async iterable)",
  );
}

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

// ---------------------------------------------------------------------------
// Session-scoped `ctx.ui` state.
//
// `buildCtx` hands every event handler and command a *fresh* `ctx` object, so
// anything that outlives one turn — the `ctx.ui.setStatus` map a custom footer
// reads, and the `footerData.onBranchChange` subscribers — has to live here and
// not inside `makeUiContext`. Upstream has exactly one `FooterDataProvider` per
// session (`footer-data-provider.ts:50-70`); a per-`ctx` copy would answer a
// footer with only the statuses written through the same handler that installed
// it, and would lose the driver's branch-change hook to whichever `buildCtx`
// ran last.
// ---------------------------------------------------------------------------

/** `ctx.ui.setStatus(key, text)` state, shared by every `ctx` of the session. */
const __pi_ui_statuses = new Map();

/** `footerData.onBranchChange(cb)` subscribers, shared by every `ctx`. */
const __pi_footer_branch_subscribers = new Set();

/** Warn through whatever notify channel the host installed; never throws. */
function __pi_ui_warn(message) {
  if (typeof globalThis.host_ui_notify !== "function") return;
  try {
    globalThis.host_ui_notify(String(message), "warning");
  } catch (_error) {
    // Swallow — notify is fire-and-forget.
  }
}

/**
 * Entry point the Rust driver calls when the branch it tracks changed
 * (`JsExtensionHost::notify_branch_change`). Each subscriber is extension code,
 * so a throw is reported and dropped instead of unwinding into the driver's
 * frame.
 */
globalThis.__pi_footer_branch_changed = function () {
  if (__pi_footer_branch_subscribers.size === 0) return;
  for (const callback of Array.from(__pi_footer_branch_subscribers)) {
    try {
      callback();
    } catch (error) {
      __pi_ui_warn(
        "ctx.ui.onBranchChange failed: " +
          String(error && error.message ? error.message : error),
      );
    }
  }
};

/**
 * Read one `footerData` field from the host. A missing bridge (no UI) or an
 * unknown field reads as `undefined`, which each getter folds into its own
 * upstream default.
 */
function __pi_footer_data_query(field) {
  if (typeof globalThis.host_ui_region !== "function") return undefined;
  try {
    const raw = globalThis.host_ui_region(
      "footerData",
      JSON.stringify({ field: String(field) }),
    );
    const reply = typeof raw === "string" ? JSON.parse(raw) : null;
    return reply && reply.ok ? reply.value : undefined;
  } catch (_error) {
    return undefined;
  }
}

/**
 * `footerData` — the read-only view of driver-owned (and session-scoped) facts a
 * custom footer gets as its third factory argument (upstream
 * `ReadonlyFooterDataProvider`, `footer-data-provider.ts:387-390`).
 *
 * `getGitBranch()` / `getAvailableProviderCount()` are the host *→* JS direction
 * of the bridge: the interactive driver pushes a snapshot
 * (`JsExtensionHost::sync_footer_data`) and these getters query it back
 * synchronously over the same `host_ui_region` bridge the mutations use, so a
 * render-time read never crosses an await. A host that never pushed one
 * (non-interactive mode) answers the untouched default — branch `null`, count
 * `0` — which is upstream's own pre-resolve answer.
 */
const __pi_footer_data_stub = Object.freeze({
  /**
   * The branch of the repository the session runs in, or `null` outside a
   * repository / on a detached HEAD (`string | null` upstream).
   */
  getGitBranch: () => {
    const branch = __pi_footer_data_query("gitBranch");
    return branch === null || branch === undefined ? null : String(branch);
  },
  /**
   * How many providers this session can route to — the count upstream derives
   * from the session's model catalog (`footer-data-provider.ts:360-370`).
   */
  getAvailableProviderCount: () => {
    const count = Number(__pi_footer_data_query("availableProviderCount"));
    return Number.isFinite(count) && count > 0 ? count : 0;
  },
  // A copy, like the `ReadonlyMap` upstream hands a custom footer.
  getExtensionStatuses: () => new Map(__pi_ui_statuses),
  /**
   * Subscribe to branch transitions; returns the unsubscribe function
   * (`footer-data-provider.ts:149-160`). Fired by the host when the driver sees
   * `HEAD` move — a checkout in another terminal included.
   */
  onBranchChange: (callback) => {
    if (typeof callback !== "function") return () => {};
    __pi_footer_branch_subscribers.add(callback);
    return () => {
      __pi_footer_branch_subscribers.delete(callback);
    };
  },
});

// ---------------------------------------------------------------------------
// Extension UI component registry (`ctx.ui.setWidget` / `setHeader` /
// `setFooter` / `setEditorComponent` / `custom`).
//
// The Rust host owns every region; a JS component is registered here under a
// numeric id and the host calls back into `__pi_ui_render_component` /
// `__pi_ui_component_input` / `__pi_ui_dispose_component` when it needs the
// component's lines, wants to deliver a key, or is done with it. Only the
// `host_ui_region` import crosses into Rust; the component objects themselves
// never leave QuickJS.
// ---------------------------------------------------------------------------

/** Registered components, keyed by the id handed to the host. */
const __pi_ui_components = new Map();
let __pi_ui_next_component_id = 1;

/**
 * Register a component object. Returns its id, or `0` when the object
 * cannot render.
 *
 * @param {unknown} component
 * @returns {number}
 */
function __pi_ui_register_component(component) {
  if (component === null || typeof component !== "object") return 0;
  if (typeof component.render !== "function") return 0;
  const id = __pi_ui_next_component_id++;
  __pi_ui_components.set(id, component);
  return id;
}

/**
 * Render component `id` for `width` columns. Returns a JSON envelope
 * (`{"ok":true,"lines":[…],"hasInput":bool}` / `{"ok":false,"error":…}`)
 * so a throwing component surfaces as an empty region instead of a host crash.
 *
 * @param {number} id
 * @param {number} width
 * @returns {string}
 */
function __pi_ui_render_component(id, width) {
  const component = __pi_ui_components.get(id);
  if (!component) {
    return JSON.stringify({ ok: false, error: "component " + id + " is not registered" });
  }
  try {
    const rendered = component.render(Number(width) || 0);
    const lines = Array.isArray(rendered) ? rendered.map((line) => String(line)) : [];
    return JSON.stringify({
      ok: true,
      lines: lines,
      hasInput: typeof component.handleInput === "function",
    });
  } catch (error) {
    return JSON.stringify({
      ok: false,
      error: String(error && error.message ? error.message : error),
    });
  }
}

/**
 * Deliver raw terminal `data` to component `id`; returns whether the component
 * handled it (upstream's `handleInput` returns `void`, so any implementation
 * counts as handled).
 *
 * @param {number} id
 * @param {string} data
 * @returns {boolean}
 */
function __pi_ui_component_input(id, data) {
  const component = __pi_ui_components.get(id);
  if (!component || typeof component.handleInput !== "function") return false;
  try {
    component.handleInput(String(data));
    return true;
  } catch (_error) {
    return false;
  }
}

/**
 * Drop component `id`, calling its `dispose` exactly once.
 *
 * @param {number} id
 * @returns {boolean}
 */
function __pi_ui_dispose_component(id) {
  const component = __pi_ui_components.get(id);
  if (!component) return false;
  __pi_ui_components.delete(id);
  try {
    if (typeof component.dispose === "function") component.dispose();
  } catch (_error) {
    // Swallow: a throwing dispose must not take the host down.
  }
  return true;
}

/**
 * Build the `ui` sub-context. Each method either fires a host import
 * (notify) or returns a Promise that resolves when the host answers
 * (confirm / input / select). When `hasUI` is false, the Promise-based
 * methods return a sensible default (false / null / null) without
// ---------------------------------------------------------------------------
// Autocomplete provider chain (`ctx.ui.addAutocompleteProvider`).
//
// Upstream stacks wrappers on top of the built-in provider
// (`packages/coding-agent/src/modes/interactive/interactive-mode.ts:734-745`):
// each factory receives the provider it wraps and returns the wrapper, and
// the resulting chain's `triggerCharacters` are deduplicated into one table.
// The chain therefore lives *here*, in JS — the host only ever sees two
// things: the deduplicated trigger list, and a call into the chain head.
//
// The innermost link is the Rust `CombinedAutocompleteProvider`, which is not
// a JS value; it is exposed as `__pi_autocomplete_base_provider` and answers
// through the synchronous `host_ui_autocomplete` import.
//
// Phase-1 restriction (documented in `docs/LUM1448_AUTOCOMPLETE_PROVIDER.md`):
// only **synchronous** provider callbacks are served. `getSuggestions` /
// `applyCompletion` / `shouldTriggerFileCompletion` that return a Promise are
// not awaited — the chain falls back to the built-in provider for that call
// and warns once. See `__pi_autocomplete_invoke`.
// ---------------------------------------------------------------------------

/** Registered `(current) => provider` factories, in registration order. */
const __pi_autocomplete_wrappers = [];
/** The current chain head, or `null` before the first rebuild. */
let __pi_autocomplete_provider = null;
/** Warn once when a callback returns a Promise (unsupported in phase 1). */
let __pi_autocomplete_warned_async = false;
/** Warn once when a factory throws or returns an unusable provider. */
let __pi_autocomplete_warned_factory = false;

/** A never-aborted signal — upstream providers read `options.signal.aborted`. */
const __pi_autocomplete_signal = (() => {
  try {
    return new AbortController().signal;
  } catch (_error) {
    return Object.freeze({
      aborted: false,
      addEventListener: () => {},
      removeEventListener: () => {},
    });
  }
})();

/** Call the `host_ui_autocomplete` import; never throws. */
function __pi_autocomplete_host(op, payload) {
  if (typeof globalThis.host_ui_autocomplete !== "function") return { ok: false };
  try {
    const raw = globalThis.host_ui_autocomplete(String(op), JSON.stringify(payload || {}));
    return typeof raw === "string" ? JSON.parse(raw) : { ok: false };
  } catch (_error) {
    return { ok: false };
  }
}

/** The built-in provider, as the object a wrapper receives as `current`. */
const __pi_autocomplete_base_provider = {
  getSuggestions(lines, cursorLine, cursorCol, options) {
    const reply = __pi_autocomplete_host("baseGetSuggestions", {
      lines: Array.isArray(lines) ? lines : [],
      cursorLine: Number(cursorLine) || 0,
      cursorCol: Number(cursorCol) || 0,
      force: !!(options && options.force),
    });
    if (!reply.ok) return null;
    return reply.suggestions == null ? null : reply.suggestions;
  },
  applyCompletion(lines, cursorLine, cursorCol, item, prefix) {
    const reply = __pi_autocomplete_host("baseApplyCompletion", {
      lines: Array.isArray(lines) ? lines : [],
      cursorLine: Number(cursorLine) || 0,
      cursorCol: Number(cursorCol) || 0,
      item: item || { value: "", label: "" },
      prefix: typeof prefix === "string" ? prefix : "",
    });
    if (!reply.ok || !reply.completion) {
      // No built-in adapter: keep the buffer as it was rather than guessing.
      return {
        lines: Array.isArray(lines) ? lines.slice() : [],
        cursorLine: Number(cursorLine) || 0,
        cursorCol: Number(cursorCol) || 0,
      };
    }
    return reply.completion;
  },
  shouldTriggerFileCompletion(lines, cursorLine, cursorCol) {
    const reply = __pi_autocomplete_host("baseShouldTriggerFileCompletion", {
      lines: Array.isArray(lines) ? lines : [],
      cursorLine: Number(cursorLine) || 0,
      cursorCol: Number(cursorCol) || 0,
    });
    return reply.ok ? reply.value !== false : true;
  },
};

/** Warn once through the host's `ctx.ui.notify` channel. */
function __pi_autocomplete_warn(message) {
  if (typeof globalThis.host_ui_notify !== "function") return;
  try {
    globalThis.host_ui_notify("ctx.ui.addAutocompleteProvider: " + message, "warning");
  } catch (_error) {
    // Notify is fire-and-forget.
  }
}

/** Whether `value` is thenable — the phase-1 unsupported shape. */
function __pi_autocomplete_is_thenable(value) {
  return (
    !!value &&
    (typeof value === "object" || typeof value === "function") &&
    typeof value.then === "function"
  );
}

/**
 * Rebuild the chain from the registered factories and return its deduplicated
 * trigger characters (upstream `provider.triggerCharacters = [...new
 * Set(triggerCharacters)]`).
 *
 * Idempotent: re-running it after the host installs the built-in provider is
 * how the first delegation starts working, and it is what a late
 * `addAutocompleteProvider` triggers.
 */
function __pi_autocomplete_rebuild() {
  let provider = __pi_autocomplete_base_provider;
  const collected = [];
  for (const factory of __pi_autocomplete_wrappers) {
    let next;
    try {
      next = factory(provider);
    } catch (error) {
      if (!__pi_autocomplete_warned_factory) {
        __pi_autocomplete_warned_factory = true;
        __pi_autocomplete_warn(
          "a provider factory threw and was skipped: " +
            String(error && error.message ? error.message : error),
        );
      }
      continue;
    }
    if (!next || typeof next.getSuggestions !== "function") {
      if (!__pi_autocomplete_warned_factory) {
        __pi_autocomplete_warned_factory = true;
        __pi_autocomplete_warn("a provider factory did not return a provider with getSuggestions()");
      }
      continue;
    }
    provider = next;
    const declared = Array.isArray(provider.triggerCharacters) ? provider.triggerCharacters : [];
    for (const character of declared) collected.push(character);
  }
  const deduped = Array.from(new Set(collected));
  // Best-effort mirror of upstream's assignment; a frozen provider object
  // makes this a no-op, and the host-side table (this return value) is the one
  // the editor actually reads.
  try {
    provider.triggerCharacters = deduped;
  } catch (_error) {
    // Ignore: the editor reads the deduplicated list from this function.
  }
  __pi_autocomplete_provider = provider;
  return deduped;
}

/**
 * Register one wrapper factory and notify the host that the chain changed.
 *
 * Upstream runs `setupAutocompleteProvider()` immediately; this port rebuilds
 * immediately *and* bumps the host's generation counter so an interactive loop
 * that is already running re-installs the provider with the new trigger
 * table.
 */
function __pi_autocomplete_register(factory) {
  if (typeof factory !== "function") {
    throw new TypeError("ctx.ui.addAutocompleteProvider: factory must be a function");
  }
  __pi_autocomplete_wrappers.push(factory);
  __pi_autocomplete_rebuild();
  __pi_autocomplete_host("register", { count: __pi_autocomplete_wrappers.length });
}

/**
 * Invoke the chain head for one autocomplete op.
 *
 * `op` is `getSuggestions` / `applyCompletion` / `shouldTriggerFileCompletion`;
 * `payload` carries the buffer snapshot plus, for `applyCompletion`, the item
 * and prefix. The return value is the *inner* result of the op (the raw
 * provider return), not yet an envelope.
 *
 * A callback that returns a Promise is not awaited (phase 1): the built-in
 * provider answers that call instead, so the user still gets the built-in
 * behaviour rather than an empty dropdown.
 */
function __pi_autocomplete_invoke(op, payload) {
  const provider = __pi_autocomplete_provider;
  const lines = Array.isArray(payload.lines) ? payload.lines : [];
  const cursorLine = Number(payload.cursorLine) || 0;
  const cursorCol = Number(payload.cursorCol) || 0;
  if (op === "shouldTriggerFileCompletion") {
    if (!provider || typeof provider.shouldTriggerFileCompletion !== "function") return true;
    let value;
    try {
      value = provider.shouldTriggerFileCompletion(lines, cursorLine, cursorCol);
    } catch (_error) {
      return __pi_autocomplete_base_provider.shouldTriggerFileCompletion(
        lines,
        cursorLine,
        cursorCol,
      );
    }
    if (__pi_autocomplete_is_thenable(value)) {
      __pi_autocomplete_warn_async();
      return __pi_autocomplete_base_provider.shouldTriggerFileCompletion(
        lines,
        cursorLine,
        cursorCol,
      );
    }
    return value !== false;
  }

  if (op === "applyCompletion") {
    const delegate = () =>
      __pi_autocomplete_base_provider.applyCompletion(
        lines,
        cursorLine,
        cursorCol,
        payload.item,
        payload.prefix,
      );
    if (!provider || typeof provider.applyCompletion !== "function") {
      return delegate();
    }
    let value;
    try {
      value = provider.applyCompletion(lines, cursorLine, cursorCol, payload.item, payload.prefix);
    } catch (_error) {
      return delegate();
    }
    if (__pi_autocomplete_is_thenable(value)) {
      __pi_autocomplete_warn_async();
      return delegate();
    }
    // A completion has to carry the rewritten buffer; anything else would
    // blank the draft, so it is treated as "the wrapper declined" and the
    // built-in rewrite is used instead.
    if (!value || typeof value !== "object" || !Array.isArray(value.lines)) return delegate();
    return value;
  }

  // `getSuggestions`
  const options = { signal: __pi_autocomplete_signal, force: !!payload.force };
  if (!provider || typeof provider.getSuggestions !== "function") {
    return __pi_autocomplete_base_provider.getSuggestions(lines, cursorLine, cursorCol, options);
  }
  let value;
  try {
    value = provider.getSuggestions(lines, cursorLine, cursorCol, options);
  } catch (_error) {
    return __pi_autocomplete_base_provider.getSuggestions(lines, cursorLine, cursorCol, options);
  }
  if (__pi_autocomplete_is_thenable(value)) {
    __pi_autocomplete_warn_async();
    return __pi_autocomplete_base_provider.getSuggestions(lines, cursorLine, cursorCol, options);
  }
  return value == null ? null : value;
}

/** Warn once that a Promise-returning callback is not supported. */
function __pi_autocomplete_warn_async() {
  if (__pi_autocomplete_warned_async) return;
  __pi_autocomplete_warned_async = true;
  __pi_autocomplete_warn(
    "an async provider callback returned a Promise; only synchronous callbacks are awaited in this port",
  );
}

/** Normalise a `getSuggestions` return value into the wire shape. */
function __pi_autocomplete_suggestions(value) {
  if (!value || typeof value !== "object") return null;
  const items = Array.isArray(value.items) ? value.items : [];
  if (items.length === 0) return null;
  return {
    items: items.map((item) => {
      const rawValue = item && item.value != null ? item.value : "";
      const rawLabel = item && item.label != null ? item.label : rawValue;
      const entry = { value: String(rawValue), label: String(rawLabel) };
      if (item && item.description != null) entry.description = String(item.description);
      return entry;
    }),
    prefix: typeof value.prefix === "string" ? value.prefix : "",
  };
}

/**
 * Rust-side entry point: `_pi_autocomplete_call(op, payloadJson)`.
 *
 * Answers in the same turn (a plain JSON string, no promise) because the host
 * reaches it from the editor's *synchronous* provider through a bounded
 * blocking bridge; a promise here would deadlock that bridge.
 *
 * @param {string} op
 * @param {string} payloadJson
 * @returns {string}
 */
globalThis._pi_autocomplete_call = function _pi_autocomplete_call(op, payloadJson) {
  let payload = {};
  try {
    payload = payloadJson == null || payloadJson === "" ? {} : JSON.parse(payloadJson);
  } catch (_error) {
    payload = {};
  }
  const name = String(op);
  if (name === "rebuild") {
    const triggerCharacters = __pi_autocomplete_rebuild();
    return JSON.stringify({
      ok: true,
      registered: __pi_autocomplete_wrappers.length > 0,
      triggerCharacters: triggerCharacters,
    });
  }
  try {
    if (name === "getSuggestions") {
      const raw = __pi_autocomplete_invoke("getSuggestions", payload);
      return JSON.stringify({ ok: true, suggestions: __pi_autocomplete_suggestions(raw) });
    }
    if (name === "applyCompletion") {
      const completion = __pi_autocomplete_invoke("applyCompletion", payload);
      const safe = completion && typeof completion === "object" ? completion : {};
      return JSON.stringify({
        ok: true,
        completion: {
          lines: Array.isArray(safe.lines) ? safe.lines.map((line) => String(line)) : [],
          cursorLine: Number(safe.cursorLine) || 0,
          cursorCol: Number(safe.cursorCol) || 0,
        },
      });
    }
    if (name === "shouldTriggerFileCompletion") {
      const value = __pi_autocomplete_invoke("shouldTriggerFileCompletion", payload);
      return JSON.stringify({ ok: true, value: value !== false });
    }
  } catch (error) {
    return JSON.stringify({
      ok: false,
      error: String(error && error.message ? error.message : error),
    });
  }
  return JSON.stringify({ ok: false, error: "unknown autocomplete op " + name });
};

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
  const warnedKinds = {};
  function reportUnsupported(kind) {
    if (warnedKinds[kind]) return;
    warnedKinds[kind] = true;
    if (typeof globalThis.host_ui_notify !== "function") return;
    try {
      globalThis.host_ui_notify(
        "ctx.ui." + kind + " is not available in the pi extension host: it has no widget or render channel",
        "warning",
      );
    } catch (_e) {
      // Swallow — notify is fire-and-forget.
    }
  }
  const ui = {
    // The host has no colour palette: every style helper is an identity
    // function so `ctx.ui.theme.fg("accent", text)` still returns text.
    theme: Object.freeze({
      fg: (color, text) => String(text),
      bg: (color, text) => String(text),
      bold: (text) => String(text),
      italic: (text) => String(text),
      underline: (text) => String(text),
      inverse: (text) => String(text),
      strikethrough: (text) => String(text),
      getFgAnsi: () => "",
      getBgAnsi: () => "",
    }),
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
    async editor(title) {
      reportDenied("editor", title);
      return null;
    },

    /**
     * Set (or clear, with `undefined`) a persistent status line owned by this
     * extension. Upstream draws every status as one extra footer row
     * (`packages/coding-agent/src/modes/interactive/components/footer.ts:243-251`),
     * sorted by key; the host keeps the canonical map and the TUI renders it.
     */
    setStatus(key, text) {
      const name = String(key);
      if (text === undefined || text === null) {
        __pi_ui_statuses.delete(name);
        regionCall("setStatus", { key: name, text: null });
        return;
      }
      const value = String(text);
      __pi_ui_statuses.set(name, value);
      regionCall("setStatus", { key: name, text: value });
    },

    /**
     * Set the terminal window/tab title (upstream's `ctx.ui.setTitle`,
     * `interactive-mode.ts:2443` → `Terminal.setTitle`, OSC 0 at
     * `terminal.ts:520`). The value is stringified like upstream's template
     * literal would, and the host sanitises control characters before the
     * sequence reaches the terminal. Upstream has no "restore" call, so a
     * later session change is what replaces the title again.
     */
    setTitle(title) {
      if (!regionsAvailable("setTitle")) return;
      regionCall("setTitle", { title: String(title) });
    },

    // --- Region / overlay surface ---------------------------------------
    // The Rust TUI owns header / footer / widget / editor regions and the
    // overlay stack; these methods register a JS component and forward the
    // mutation through `host_ui_region`. Without a UI (or without a region
    // host) they deny / warn exactly like `confirm` does.

    /**
     * Install (or, with `null` / `undefined`, clear) the widget registered
     * under `key`. `content` is either the upstream `string[]` shorthand or a
     * factory `(tui, theme) => Component`.
     */
    setWidget(key, content, options) {
      const placement =
        options && options.placement === "belowEditor" ? "belowEditor" : "aboveEditor";
      const component = normalizeRegionComponent("setWidget", content, [tuiStub, ui.theme]);
      installRegion("setWidget", "setWidget", component, {
        key: String(key),
        placement: placement,
      });
    },

    /** Install (or clear) the header region from a `(tui, theme) => Component` factory. */
    setHeader(factory) {
      const component = normalizeRegionComponent("setHeader", factory, [tuiStub, ui.theme]);
      installRegion("setHeader", "setHeader", component);
    },

    /** Install (or clear) the footer region from a `(tui, theme, data) => Component` factory. */
    setFooter(factory) {
      const component = normalizeRegionComponent("setFooter", factory, [
        tuiStub,
        ui.theme,
        __pi_footer_data_stub,
      ]);
      installRegion("setFooter", "setFooter", component);
    },

    /** Install (or clear) the editor-region component from a `(tui, theme, keys) => Component` factory. */
    setEditorComponent(factory) {
      const component = normalizeRegionComponent("setEditorComponent", factory, [
        tuiStub,
        ui.theme,
        keybindingsStub,
      ]);
      installRegion("setEditorComponent", "setEditorComponent", component);
    },

    /**
     * Stack a provider on top of the built-in one.
     *
     * The factory receives the provider it wraps (`current`) and returns the
     * wrapper; upstream `addAutocompleteProvider`
     * (`packages/coding-agent/src/extensions/types.ts`). Only *synchronous*
     * callbacks are served in this port — a callback returning a Promise is
     * not awaited and the built-in provider answers that call instead.
     */
    addAutocompleteProvider(factory) {
      __pi_autocomplete_register(factory);
    },

    /**
     * Run a full-screen / overlay component.
     *
     * Returns a thenable *handle*: `await` it for the upstream
     * `Promise<T>` behaviour, or call `handle.resolve(value)` / `close()` /
     * `isVisible()` directly, mirroring upstream's `CustomHandle`.
     */
    custom(factory, options) {
      const opts = options || {};
      const state = { closed: false, visible: true, session: 0, result: undefined };
      let settle;
      const promise = new Promise((resolve) => {
        settle = resolve;
      });
      const handle = {
        id: 0,
        isVisible() {
          return !state.closed && state.visible;
        },
        setVisible(next) {
          state.visible = Boolean(next);
          if (state.session) {
            regionCall("customSetVisible", { session: state.session, visible: state.visible });
          }
          return state.visible;
        },
        resolve(result) {
          if (state.closed) return;
          state.closed = true;
          state.result = result === undefined ? null : result;
          if (state.session) {
            regionCall("customClose", { session: state.session, result: state.result });
          }
          settle(result === undefined ? undefined : result);
        },
        close() {
          handle.resolve(undefined);
        },
        done(result) {
          handle.resolve(result);
        },
        then(onFulfilled, onRejected) {
          return promise.then(onFulfilled, onRejected);
        },
        catch(onRejected) {
          return promise.catch(onRejected);
        },
        finally(onFinally) {
          return promise.finally(onFinally);
        },
      };
      if (typeof factory !== "function" || !regionsAvailable("custom")) {
        queueMicrotask(() => handle.resolve(undefined));
        return handle;
      }
      Promise.resolve()
        .then(() => factory(tuiStub, ui.theme, keybindingsStub, (result) => handle.resolve(result)))
        .then((component) => {
          // The factory is asynchronous, so `done(...)` can win the race: a
          // session that closed before it opened never touches the host (the
          // handle already carries the result), and there is nothing to
          // dispose because the component was never registered.
          if (state.closed) return;
          if (!component || typeof component.render !== "function") {
            reportRegionError(
              "custom",
              new Error("factory did not return a component with render(width)"),
            );
            handle.resolve(undefined);
            return;
          }
          const componentId = __pi_ui_register_component(component);
          if (!componentId) {
            handle.resolve(undefined);
            return;
          }
          const overlayOptions = opts.overlayOptions || {};
          const opened = regionCall("customOpen", {
            componentId: componentId,
            overlay: opts.overlay === true,
            width: typeof overlayOptions.width === "number" ? overlayOptions.width : null,
            maxHeight: typeof overlayOptions.maxHeight === "number" ? overlayOptions.maxHeight : null,
            anchor: typeof overlayOptions.anchor === "string" ? overlayOptions.anchor : null,
            margin: typeof overlayOptions.margin === "number" ? overlayOptions.margin : 0,
          });
          if (!opened || !opened.ok || typeof opened.session !== "number") {
            __pi_ui_dispose_component(componentId);
            handle.resolve(undefined);
            return;
          }
          state.session = opened.session;
          handle.id = opened.session;
        })
        .catch((error) => {
          reportRegionError("custom", error);
          if (!state.closed) handle.resolve(undefined);
        });
      // The factory above runs on a microtask, and the host only re-polls its
      // async driver when a Rust-side task is pushed. Ask it to poll now so an
      // idle TUI opens the overlay instead of waiting for the next awaited
      // host call to drain the job queue.
      regionCall("wake", {});
      return handle;
    },
  };

  // No-op stand-ins for the upstream `tui` / `keybindings` objects a region
  // factory receives. Extensions mostly use them to request a re-render; the
  // host re-renders every frame regardless. `footerData` is *not* a no-op
  // stand-in — it is a live query channel over session-scoped state (see
  // `__pi_footer_data_stub`).
  const tuiStub = Object.freeze({
    requestRender: () => {},
    setFocus: () => {},
    terminal: Object.freeze({ columns: 80, rows: 24 }),
  });
  const keybindingsStub = Object.freeze({
    matches: () => false,
    getKeys: () => [],
  });

  /** Call the `host_ui_region` import; never throws. */
  function regionCall(op, payload) {
    if (typeof globalThis.host_ui_region !== "function") return { ok: false };
    try {
      const raw = globalThis.host_ui_region(String(op), JSON.stringify(payload || {}));
      return typeof raw === "string" ? JSON.parse(raw) : { ok: false };
    } catch (_error) {
      return { ok: false };
    }
  }

  /** Whether the real region surface exists; otherwise deny like `confirm` does. */
  function regionsAvailable(kind) {
    if (hasUI && typeof globalThis.host_ui_region === "function") return true;
    reportDenied(kind);
    return false;
  }

  /** Surface a component failure without throwing into the extension. */
  function reportRegionError(kind, error) {
    const message = String(error && error.message ? error.message : error);
    if (typeof globalThis.host_ui_notify === "function") {
      try {
        globalThis.host_ui_notify("ctx.ui." + kind + " failed: " + message, "warning");
      } catch (_error2) {
        // ignore
      }
    }
  }

  /**
   * Normalise the three shapes `content` can take — `string[]`, a factory or
   * an already-built component — into a component object (or `null` to clear).
   */
  function normalizeRegionComponent(kind, content, args) {
    if (content === undefined || content === null) return null;
    if (Array.isArray(content)) {
      return { render: () => content.map((line) => String(line)) };
    }
    if (typeof content === "function") {
      let produced;
      try {
        produced = content.apply(null, args);
      } catch (error) {
        reportRegionError(kind, error);
        return null;
      }
      if (!produced || typeof produced.render !== "function") {
        reportRegionError(kind, new Error("factory did not return a component with render(width)"));
        return null;
      }
      return produced;
    }
    if (typeof content.render === "function") return content;
    return null;
  }

  /** Register `component` and forward the region mutation to the host. */
  function installRegion(kind, op, component, extra) {
    if (!regionsAvailable(kind)) return false;
    let componentId = 0;
    if (component !== null) {
      componentId = __pi_ui_register_component(component);
      if (!componentId) {
        reportRegionError(kind, new Error("component must have render(width)"));
        return false;
      }
    }
    regionCall(op, Object.assign({ componentId: componentId }, extra || {}));
    return true;
  }

  // Theme / editor-text / working-indicator channels still have no host
  // bridge: accept the call so extensions that configure them at load time
  // still load, warn once, and keep the value inert.
  for (const kind of [
    "setEditorText",
    "setHiddenThinkingLabel",
    "setWorkingIndicator",
    "setWorkingVisible",
    "setWorkingMessage",
    "setTheme",
  ]) {
    ui[kind] = () => {
      reportUnsupported(kind);
    };
  }
  return Object.freeze(ui);
}

/**
 * Event-name aliases, for extensions written against this port's earlier
 * naming. Upstream pi matches event names exactly, so an alias is only
 * added when *this port* once shipped a different name for an upstream
 * event — never invented for a name upstream never had.
 *
 * `session_end` was this port's name for upstream's `session_shutdown`
 * (see `crates/pi-extensions/docs/EXTENSIONS.md`); the shim keeps
 * accepting it so extensions written against the older Rust docs keep
 * firing after the rename.
 */
const EVENT_ALIASES = Object.freeze({
  session_end: "session_shutdown",
});

/**
 * Every upstream event name `pi.on` may be given, in upstream declaration
 * order (`packages/coding-agent/src/core/extensions/types.ts:1257-1301`).
 *
 * The Rust host keeps the same list in
 * `crates/pi-extensions/src/events.rs`; a unit test asserts the two agree, so
 * this array is the JS-side half of the alias table rather than documentation
 * that can drift.
 *
 * `pi.on` stays permissive (an unknown name registers and simply never
 * fires), matching upstream, where the name is a compile-time type rather
 * than a runtime check.
 */
const UPSTREAM_EVENT_NAMES = Object.freeze([
  "project_trust",
  "resources_discover",
  "session_start",
  "session_info_changed",
  "session_before_switch",
  "session_before_fork",
  "session_before_compact",
  "session_compact",
  "session_compact_failed",
  "session_shutdown",
  "session_before_tree",
  "session_tree",
  "context",
  "before_provider_request",
  "before_provider_headers",
  "after_provider_response",
  "before_agent_start",
  "agent_start",
  "agent_end",
  "agent_settled",
  "ui_prompt_start",
  "ui_prompt_end",
  "turn_start",
  "turn_end",
  "message_start",
  "message_update",
  "message_end",
  "tool_execution_start",
  "tool_execution_update",
  "tool_execution_end",
  "model_select",
  "thinking_level_select",
  "tool_call",
  "tool_result",
  "user_bash",
  "input",
]);

/**
 * Resolve an event name to the canonical one the host emits.
 *
 * Both `pi.on` and `_pi_dispatch` go through this, so a subscription and a
 * delivery can never disagree about which key they use.
 *
 * @param {string} name
 * @returns {string}
 */
function canonicalEventName(name) {
  return Object.prototype.hasOwnProperty.call(EVENT_ALIASES, name)
    ? EVENT_ALIASES[name]
    : name;
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
    const name = canonicalEventName(eventName);
    const list = _pi.handlers[name] || (_pi.handlers[name] = []);
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

  /**
   * Register a custom / proxy provider the agent can stream from.
   *
   * Both upstream overloads are backed:
   *
   * * `pi.registerProvider(name, { baseUrl, apiKey, api, models,
   *   streamSimple?, oauth? })` — the declarative string overload.
   *   `streamSimple(model, context, options)` makes the provider stream
   *   through the extension's own code instead of a host adapter; `api` is
   *   required alongside it (upstream's `provider-composer` enforces the
   *   same rule) and is what the host routes the provider's models to.
   * * `pi.registerProvider(provider)` — the native `Provider` object
   *   overload. The id (`provider.id`, falling back to `provider.name`),
   *   `name`, `baseUrl`, `api` and `provider.getModels()` (or
   *   `provider.models`) are forwarded, as are `streamSimple` and
   *   `provider.oauth` / `provider.auth.oauth`.
   *
   * Validation happens host-side, so an unknown `api` or a `models`
   * array without `api` rejects with a readable `Error` that names the
   * supported set — unless the registration brings its own `streamSimple`
   * handler, which owns its wire protocol. `apiKey` is stored verbatim:
   * `$VAR` / `${VAR}` is resolved by the application layer, and a leading
   * `!command` is documented as unsupported (never executed).
   *
   * The `streamSimple` and `oauth` callbacks never leave the shim; the host
   * records that they exist and calls back by provider name.
   */
  registerProvider(nameOrProvider, config) {
    if (nameOrProvider !== null && typeof nameOrProvider === "object") {
      return registerNativeProvider(nameOrProvider);
    }
    if (typeof nameOrProvider !== "string") {
      throw new TypeError(
        "pi.registerProvider: first argument must be a provider name or a native Provider object",
      );
    }
    const name = nameOrProvider;
    if (!name) {
      throw new TypeError("pi.registerProvider: name must be a non-empty string");
    }
    if (config != null && typeof config !== "object") {
      throw new TypeError("pi.registerProvider: config must be an object");
    }
    const source = config || {};
    const payload = { name };
    for (const key of ["baseUrl", "apiKey", "api"]) {
      if (typeof source[key] === "string") payload[key] = source[key];
    }
    if (typeof source.name === "string") payload.displayName = source.name;
    else if (typeof source.displayName === "string") payload.displayName = source.displayName;
    if (Array.isArray(source.models)) payload.models = source.models;
    const streamSimple = typeof source.streamSimple === "function" ? source.streamSimple : null;
    if (streamSimple) payload.hasStreamSimple = true;
    const oauthFlags = normalizeOAuthBlock(source.oauth);
    if (oauthFlags) payload.oauth = oauthFlags;
    sendProviderRegistration(payload, {
      name,
      streamSimple,
      oauth: source.oauth || null,
    });
  },

  /**
   * Remove a provider previously registered with `registerProvider`.
   * A no-op when the name is unknown.
   */
  unregisterProvider(name) {
    if (typeof name !== "string" || !name) {
      throw new TypeError("pi.unregisterProvider: name must be a non-empty string");
    }
    if (typeof globalThis.host_unregister_provider !== "function") {
      throw new Error(
        "pi.unregisterProvider is not available in this host build",
      );
    }
    globalThis.host_unregister_provider(name);
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
   * in milliseconds.
   *
   * `options.signal` is honoured the way upstream honours it: the child is
   * killed and the promise still resolves (never rejects) with
   * `killed: true`. An already-aborted signal resolves immediately without
   * spawning anything. While the call is in flight the host's own per-call
   * timeout (5 s by default, 300 s in interactive mode) is raised to
   * `timeout + 1 s` for this call, so an explicit `options.timeout` is the
   * one that decides — a missing binary still yields
   * `{ code: 1, stderr: "…" }` rather than a rejection.
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
    const signal =
      opts.signal && typeof opts.signal === "object" && typeof opts.signal.aborted === "boolean"
        ? opts.signal
        : undefined;
    // Upstream spawns and immediately kills a pre-aborted signal; resolving
    // without spawning is equivalent and cheaper (the child never exists).
    if (signal && signal.aborted) {
      return { stdout: "", stderr: "", code: -1, killed: true };
    }
    if (typeof globalThis.host_exec !== "function") {
      throw new Error("pi.exec is not available in this host build");
    }
    // Cancellation is out of band (a signal is not JSON): the shim owns the
    // id, `host_exec` registers it, and `host_exec_cancel` kills the child.
    const execId = __pi_next_exec_id++;
    let onAbort = null;
    if (signal && typeof signal.addEventListener === "function") {
      onAbort = () => {
        if (typeof globalThis.host_exec_cancel === "function") {
          globalThis.host_exec_cancel(execId);
        }
      };
      signal.addEventListener("abort", onAbort);
    }
    let raw;
    try {
      raw = await globalThis.host_exec(
        command,
        JSON.stringify({ id: execId, args: argv, cwd: cwd, timeout: timeout }),
      );
    } finally {
      if (signal && typeof signal.removeEventListener === "function" && onAbort) {
        signal.removeEventListener("abort", onAbort);
      }
    }
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
  // Aliases resolve here as well as at `pi.on`, so a handler registered
  // under either spelling is found by the canonical name the host emits.
  const handlers = _pi.handlers[canonicalEventName(parsed.type)] || [];
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
 * Send one provider registration to the host and remember the callbacks that
 * cannot cross the ABI.
 *
 * @param {object} payload host-facing wire shape
 * @param {{name:string, streamSimple:Function|null, oauth:any}} callbacks
 */
function sendProviderRegistration(payload, callbacks) {
  if (typeof globalThis.host_register_provider !== "function") {
    throw new Error("pi.registerProvider is not available in this host build");
  }
  // The host validates and throws on a bad registration; let it propagate —
  // but only remember the handler once it was accepted, so a rejected
  // registration cannot leave a callable `streamSimple` behind.
  globalThis.host_register_provider(JSON.stringify(payload));
  _pi.providers.set(callbacks.name, callbacks);
}

/**
 * Back the native `Provider` object overload of `pi.registerProvider`.
 *
 * @param {any} provider upstream `Provider` shape
 */
function registerNativeProvider(provider) {
  const id =
    typeof provider.id === "string" && provider.id
      ? provider.id
      : typeof provider.name === "string"
        ? provider.name
        : "";
  if (!id) {
    throw new TypeError("pi.registerProvider: native provider must have a non-empty `id`");
  }
  let models = [];
  if (typeof provider.getModels === "function") {
    const listed = provider.getModels();
    if (Array.isArray(listed)) models = listed;
  } else if (Array.isArray(provider.models)) {
    models = provider.models;
  }
  const payload = { name: id, native: true };
  if (typeof provider.name === "string") payload.displayName = provider.name;
  if (typeof provider.baseUrl === "string") payload.baseUrl = provider.baseUrl;
  if (typeof provider.apiKey === "string") payload.apiKey = provider.apiKey;
  if (typeof provider.api === "string" && provider.api) {
    payload.api = provider.api;
  } else if (models[0] && typeof models[0].api === "string" && models[0].api) {
    // A native provider may only declare its api family per model; the host
    // routes the provider as a whole, so the first model's family is used.
    payload.api = models[0].api;
  }
  if (models.length > 0) payload.models = models;
  const streamSimple = typeof provider.streamSimple === "function" ? provider.streamSimple : null;
  if (streamSimple) payload.hasStreamSimple = true;
  const rawOauth =
    provider.oauth ||
    (provider.auth && typeof provider.auth === "object" ? provider.auth.oauth : null);
  const oauthFlags = normalizeOAuthBlock(rawOauth);
  if (oauthFlags) payload.oauth = oauthFlags;
  sendProviderRegistration(payload, { name: id, streamSimple, oauth: rawOauth || null });
}

/**
 * Run one `streamSimple` handler registered via `pi.registerProvider`.
 *
 * The host calls this with the upstream `Model` / `Context` /
 * `SimpleStreamOptions` JSON it built and receives
 * `{ ok: true, events: [...] }` or `{ ok: false, error }`. Events are
 * collected by draining the handler's `AssistantMessageEventStream` — the
 * host delivers them in order, but only after the handler finished.
 *
 * @param {string} name
 * @param {string} modelJson
 * @param {string} contextJson
 * @param {string} optionsJson
 */
globalThis._pi_provider_stream_simple = async function _pi_provider_stream_simple(
  name,
  modelJson,
  contextJson,
  optionsJson,
) {
  const fail = (message) => JSON.stringify({ ok: false, error: message });
  const entry = _pi.providers.get(String(name));
  if (!entry || typeof entry.streamSimple !== "function") {
    return fail("provider `" + String(name) + "` has no streamSimple handler");
  }
  let model;
  let context;
  let options;
  try {
    model = modelJson ? JSON.parse(String(modelJson)) : {};
    context = contextJson ? JSON.parse(String(contextJson)) : {};
    options = optionsJson ? JSON.parse(String(optionsJson)) : {};
  } catch (e) {
    return fail("invalid streamSimple arguments: " + (e && e.message ? e.message : String(e)));
  }
  try {
    const stream = entry.streamSimple(model, context, options);
    return JSON.stringify({ ok: true, events: await collectProviderEvents(stream) });
  } catch (e) {
    return fail(e && e.message ? e.message : String(e));
  }
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
 * `node:crypto` / `node:zlib` / `node:process` and `typebox` /
 * `@sinclair/typebox` slice of the upstream `VIRTUAL_MODULES` map is
 * provided.
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
// `node:fs/promises`, `node:os`, `node:buffer`, `node:crypto`, `node:zlib`
// and read the `process` global directly. The embedded QuickJS runtime has no
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
//   - `node:stream`, `node:http`, … are not provided: importing them
//     fails with the readable "unsupported import" error that lists what
//     exists.
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
// Shared `EventEmitter` — the `child.stdout` / `child.stderr` streams,
// `node:readline`'s `Interface` and `fs.createReadStream`'s stream are all
// EventEmitters, so the implementation lives at module scope instead of
// inside one module factory. It sits ahead of `node:buffer` because
// `node:fs`'s read stream extends it.
// ---------------------------------------------------------------------------

class Emitter {
  constructor() {
    this._listeners = Object.create(null);
  }

  on(type, listener) {
    if (typeof listener !== "function") {
      throw new TypeError('The "listener" argument must be of type function');
    }
    const key = String(type);
    (this._listeners[key] || (this._listeners[key] = [])).push(listener);
    return this;
  }

  once(type, listener) {
    const self = this;
    function wrapper(...args) {
      self.off(type, wrapper);
      listener.apply(self, args);
    }
    wrapper.listener = listener;
    return this.on(type, wrapper);
  }

  off(type, listener) {
    const key = String(type);
    const list = this._listeners[key];
    if (!list) return this;
    if (listener === undefined) {
      delete this._listeners[key];
      return this;
    }
    this._listeners[key] = list.filter(
      (item) => item !== listener && item.listener !== listener,
    );
    return this;
  }

  addListener(type, listener) {
    return this.on(type, listener);
  }

  removeListener(type, listener) {
    return this.off(type, listener);
  }

  removeAllListeners(type) {
    if (type === undefined) this._listeners = Object.create(null);
    else delete this._listeners[String(type)];
    return this;
  }

  listeners(type) {
    return (this._listeners[String(type)] || []).slice();
  }

  listenerCount(type) {
    return (this._listeners[String(type)] || []).length;
  }

  emit(type, ...args) {
    const key = String(type);
    const list = this._listeners[key];
    if (!list || list.length === 0) {
      // Node's EventEmitter rethrows an unhandled `error` event.
      if (key === "error") {
        const error = args[0];
        throw error instanceof Error ? error : new Error(String(error));
      }
      return false;
    }
    for (const listener of list.slice()) listener.apply(this, args);
    return true;
  }
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

  /** Validate an encoding the way `Buffer#toString` does, so the `encoding`
   *  option and `setEncoding` fail at the call site like Node's. */
  function streamEncoding(encoding) {
    if (encoding === undefined || encoding === null) return null;
    BufferCtor.from("").toString(encoding);
    return encoding;
  }

  /** The `Readable` subset `fs.createReadStream` needs.
   *
   *  The bridge has no live descriptor — `fs.readFile` is blocking — so the
   *  whole file is read up front and replayed to the consumer, the same shape
   *  as `child.stdout` / `child.stderr`: chunks that arrive before a `data`
   *  listener are buffered, so attaching the listener a tick after the call
   *  cannot lose output. `open` / `data` / `end` / `close` are scheduled on the
   *  microtask queue. */
  class ReadStream extends Emitter {
    constructor(path, options) {
      super();
      const opts = typeof options === "string" ? { encoding: options } : options || {};
      this.path = String(path);
      this.flags = opts.flag === undefined ? "r" : String(opts.flag);
      if (this.flags !== "r" && this.flags !== "rs" && this.flags !== "r+") {
        throw new Error(
          "pi extension host fs.createReadStream only supports read flags, got " + this.flags,
        );
      }
      this.bytesRead = 0;
      this.readable = true;
      this.readableEnded = false;
      this.destroyed = false;
      this.closed = false;
      // There is no descriptor to expose; a numeric `fd` would be a fabricated
      // handle the `fs.*` ops cannot use. `options.fd` is therefore ignored.
      this.fd = null;
      this.autoClose = opts.autoClose !== false;
      this.__encoding = streamEncoding(opts.encoding);
      this.__highWaterMark =
        opts.highWaterMark === undefined
          ? 64 * 1024
          : Math.max(1, Math.trunc(Number(opts.highWaterMark)) || 1);
      this.__offset = 0;
      this.__slice = BufferCtor.alloc(0);
      this.__ended = false;
      this.__flowing = false;
      this.__started = false;
      this.__failure = null;
      let source = null;
      try {
        source = readFileSync(this.path, null);
      } catch (err) {
        this.__failure = err;
      }
      if (source) {
        const start =
          opts.start === undefined ? 0 : Math.max(0, Math.trunc(Number(opts.start)) || 0);
        const end = opts.end === undefined ? source.length - 1 : Math.trunc(Number(opts.end));
        const stop = end < start ? start : Math.min(end + 1, source.length);
        this.__slice = source.slice(Math.min(start, source.length), Math.max(stop, start));
      }
      const self = this;
      __pi_schedule(function () {
        self.__open();
      });
    }

    __open() {
      if (this.destroyed) return;
      this.__started = true;
      if (this.__failure) {
        this.readable = false;
        this.emit("error", this.__failure);
        this.__close();
        return;
      }
      this.emit("open", this.fd);
      if (this.__flowing) this.__drain();
    }

    __nextChunk() {
      if (this.__offset >= this.__slice.length) return null;
      const end = Math.min(this.__offset + this.__highWaterMark, this.__slice.length);
      const chunk = this.__slice.slice(this.__offset, end);
      this.__offset = end;
      return chunk;
    }

    __drain() {
      this.__flowing = true;
      if (this.listenerCount("data") === 0) return;
      if (this.__encoding !== null) {
        // Decode the remainder in one pass: a multi-byte character split
        // across a chunk boundary must not become a replacement character.
        const rest = this.__slice.slice(this.__offset);
        this.__offset = this.__slice.length;
        this.bytesRead += rest.length;
        if (rest.length > 0) this.emit("data", rest.toString(this.__encoding));
        this.__finish();
        return;
      }
      while (this.listenerCount("data") > 0) {
        const chunk = this.__nextChunk();
        if (chunk === null) break;
        this.bytesRead += chunk.length;
        this.emit("data", chunk);
      }
      if (this.__offset >= this.__slice.length) this.__finish();
    }

    __finish() {
      if (this.__ended) return;
      this.__ended = true;
      this.readable = false;
      this.readableEnded = true;
      this.emit("end");
      if (this.autoClose) this.__close();
    }

    __close() {
      if (this.closed) return;
      this.closed = true;
      this.destroyed = true;
      this.emit("close");
    }

    setEncoding(encoding) {
      this.__encoding = streamEncoding(encoding);
      return this;
    }

    pause() {
      this.__flowing = false;
      return this;
    }

    resume() {
      if (this.__started) this.__drain();
      else this.__flowing = true;
      return this;
    }

    read() {
      if (this.__encoding !== null && this.__offset < this.__slice.length) {
        const rest = this.__slice.slice(this.__offset);
        this.__offset = this.__slice.length;
        this.bytesRead += rest.length;
        const value = rest.toString(this.__encoding);
        if (!this.__ended) this.__finish();
        return value;
      }
      const chunk = this.__nextChunk();
      if (chunk === null) {
        if (!this.__ended) this.__finish();
        return null;
      }
      this.bytesRead += chunk.length;
      return chunk;
    }

    pipe(destination) {
      this.on("data", function (chunk) {
        destination.write(chunk);
      });
      this.on("end", function () {
        if (destination && typeof destination.end === "function") destination.end();
      });
      return destination;
    }

    unpipe() {
      return this;
    }

    destroy(error) {
      if (error) this.emit("error", error);
      this.readable = false;
      this.__close();
      return this;
    }

    [Symbol.asyncIterator]() {
      const self = this;
      return {
        next() {
          const value = self.read();
          return Promise.resolve(
            value === null ? { done: true, value: undefined } : { done: false, value: value },
          );
        },
        return() {
          self.destroy();
          return Promise.resolve({ done: true, value: undefined });
        },
        [Symbol.asyncIterator]() {
          return this;
        },
      };
    }

    on(type, listener) {
      const result = super.on(type, listener);
      if (String(type) === "data") {
        this.__flowing = true;
        if (this.__started) this.__drain();
      }
      return result;
    }
  }

  function createReadStream(path, options) {
    return new ReadStream(path, options);
  }

  /** The `Writable` subset `fs.createWriteStream` needs.
   *
   *  The write half of the same blocking bridge: `fs.writeFile` /
   *  `fs.appendFile` take the whole payload in one base64 hop and hand back no
   *  descriptor, so a write stream accumulates its chunks in memory and flushes
   *  once, on `end()` — the mirror image of `ReadStream`'s buffered replay.
   *
   *  What each event therefore means:
   *  - `write(chunk, …)` means "the bytes are in this stream's buffer", *not*
   *    "the bytes are on disk". Its callback succeeds as soon as it runs, and
   *    `bytesWritten` counts accepted bytes (Node's `bytesWritten` is also the
   *    number handed to the stream, so the accounting matches).
   *  - `finish`, the `end()` callback and `close` only fire after the single
   *    `fs.writeFile` / `fs.appendFile` call has returned.
   *  - A failed flush surfaces both as an `error` event and as the `end()`
   *    callback's error argument; it is never swallowed, and `finish` is not
   *    emitted.
   *  - `write()` always returns `true`: there is no backpressure to signal
   *    (the buffer grows until `end()`), so `drain` is never emitted.
   *  - `cork()` / `uncork()` are no-ops — every write already waits in the
   *    buffer until the flush, so there is nothing for cork to hold back.
   */
  class WriteStream extends Emitter {
    constructor(path, options) {
      super();
      const opts = typeof options === "string" ? { encoding: options } : options || {};
      this.path = String(path);
      this.flags = opts.flags === undefined ? "w" : String(opts.flags);
      if (this.flags !== "w" && this.flags !== "a") {
        throw new Error(
          "pi extension host fs.createWriteStream only supports the `w` (truncate) and `a` (append) flags, got " +
            this.flags,
        );
      }
      this.__append = this.flags === "a";
      // Default like Node; only consulted for string chunks.
      this.__encoding = streamEncoding(opts.encoding) || "utf8";
      this.bytesWritten = 0;
      this.writable = true;
      this.writableEnded = false;
      this.writableFinished = false;
      this.destroyed = false;
      this.closed = false;
      // There is no descriptor to expose; a numeric `fd` would be a fabricated
      // handle the `fs.*` ops cannot use.
      this.fd = null;
      this.autoClose = opts.autoClose !== false;
      this.__chunks = [];
      this.__endCallbacks = [];
      this.__flushed = false;
      const self = this;
      __pi_schedule(function () {
        self.__open();
      });
    }

    __open() {
      if (this.destroyed) return;
      // The bridge cannot open a file without also writing it, so `open` /
      // `ready` are announced optimistically; a real failure arrives with the
      // flush as `error`.
      this.emit("open", this.fd);
      this.emit("ready");
    }

    __toBuffer(chunk, encoding) {
      if (typeof chunk === "string") {
        return BufferCtor.from(chunk, streamEncoding(encoding) || this.__encoding);
      }
      if (BufferCtor.isBuffer(chunk) || chunk instanceof Uint8Array) {
        return BufferCtor.from(chunk);
      }
      throw new TypeError(
        'The "chunk" argument must be of type string or an instance of Buffer or Uint8Array',
      );
    }

    write(chunk, encoding, callback) {
      if (typeof encoding === "function") {
        callback = encoding;
        encoding = undefined;
      }
      const done = typeof callback === "function" ? callback : null;
      if (this.writableEnded || this.destroyed) {
        const err = new Error("write after end");
        err.code = "ERR_STREAM_WRITE_AFTER_END";
        const self = this;
        __pi_schedule(function () {
          if (done) done(err);
          else self.emit("error", err);
        });
        return false;
      }
      let buffer;
      try {
        buffer = this.__toBuffer(chunk, encoding);
      } catch (err) {
        const self = this;
        __pi_schedule(function () {
          if (done) done(err);
          else self.emit("error", err);
        });
        return false;
      }
      this.__chunks.push(buffer);
      this.bytesWritten += buffer.length;
      if (done) {
        __pi_schedule(function () {
          done(null);
        });
      }
      return true;
    }

    end(chunk, encoding, callback) {
      if (typeof chunk === "function") {
        callback = chunk;
        chunk = undefined;
        encoding = undefined;
      } else if (typeof encoding === "function") {
        callback = encoding;
        encoding = undefined;
      }
      const done = typeof callback === "function" ? callback : null;
      if (this.writableEnded || this.destroyed) {
        if (done) {
          const err = new Error("write after end");
          err.code = "ERR_STREAM_ALREADY_FINISHED";
          __pi_schedule(function () {
            done(err);
          });
        }
        return this;
      }
      if (chunk !== undefined && chunk !== null) this.write(chunk, encoding);
      this.writableEnded = true;
      if (done) this.__endCallbacks.push(done);
      const self = this;
      // Scheduled, not inline: `write` callbacks queued before this call must
      // run before `finish`, the order Node gives them.
      __pi_schedule(function () {
        self.__flush();
      });
      return this;
    }

    __flush() {
      if (this.__flushed || this.destroyed) return;
      this.__flushed = true;
      const payload = BufferCtor.concat(this.__chunks);
      this.__chunks = [];
      const callbacks = this.__endCallbacks;
      this.__endCallbacks = [];
      let failure = null;
      try {
        __pi_node_call(this.__append ? "fs.appendFile" : "fs.writeFile", {
          path: this.path,
          base64: payload.toString("base64"),
        });
      } catch (err) {
        failure = err;
      }
      if (failure) {
        this.writable = false;
        this.emit("error", failure);
        for (const cb of callbacks) cb(failure);
        this.__close();
        return;
      }
      this.writableFinished = true;
      this.emit("finish");
      for (const cb of callbacks) cb();
      this.__close();
    }

    __close() {
      if (this.closed) return;
      this.closed = true;
      this.destroyed = true;
      this.writable = false;
      this.emit("close");
      return this;
    }

    destroy(error) {
      if (this.destroyed) return this;
      // Buffered bytes are dropped: `destroy()` abandons the stream before the
      // single flush, exactly like Node cancelling its pending writes.
      this.__chunks = [];
      this.__endCallbacks = [];
      this.writableEnded = true;
      if (error) this.emit("error", error);
      return this.__close();
    }

    cork() {
      return this;
    }

    uncork() {
      return this;
    }
  }

  function createWriteStream(path, options) {
    return new WriteStream(path, options);
  }

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
    createReadStream: createReadStream,
    ReadStream: ReadStream,
    createWriteStream: createWriteStream,
    WriteStream: WriteStream,
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
// `node:crypto` — `randomUUID` / `randomBytes` / `randomInt` / `createHash`,
// plus the Web Crypto subset extensions actually use (`crypto.getRandomValues`
// and `crypto.subtle.digest`). Entropy comes from the host's OS bridge; the
// digests come from the host's `crypto.digest` op (`crate::digest`, which
// implements SHA-1 / SHA-256 because the offline registry has no hashing
// backend). Unsupported algorithms still fail with a readable message
// instead of producing wrong bytes.
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

  /**
   * One-shot digest through the host bridge, returning a `Buffer`.
   * `algorithm` is a Node name (`sha256`) or WebCrypto name (`SHA-256`).
   */
  function digestBytes(algorithm, bytes) {
    const encoded = BufferCtor.from(bytes).toString("base64");
    const result = __pi_node_call("crypto.digest", {
      algorithm: String(algorithm),
      base64: encoded,
    });
    return BufferCtor.from(result.base64, "base64");
  }

  /**
   * Coerce a `crypto.subtle.digest` data argument.
   *
   * WebCrypto takes an `ArrayBuffer` or any `ArrayBufferView` (Buffer is a
   * `Uint8Array` subclass, so extensions can pass either).
   */
  function viewBytes(data) {
    if (typeof ArrayBuffer === "undefined") {
      throw new TypeError("ArrayBuffer is not available in this host build");
    }
    if (data instanceof ArrayBuffer) return new Uint8Array(data);
    if (ArrayBuffer.isView(data)) {
      return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    }
    throw new TypeError(
      "crypto.subtle.digest: data must be an ArrayBuffer or ArrayBufferView",
    );
  }

  function digestAlgorithmName(algorithm) {
    if (typeof algorithm === "string") return algorithm;
    if (algorithm && typeof algorithm === "object" && algorithm.name !== undefined) {
      return String(algorithm.name);
    }
    throw new TypeError("crypto.subtle.digest: algorithm must be a string or {name}");
  }

  /**
   * `crypto.subtle` — only `digest` is bridged. Key import/export, signing
   * and encryption are not: the host bundles no asymmetric crypto backend.
   * HMAC is available through the Node `createHmac` surface below, which
   * uses the same SHA-1 / SHA-256 primitives.
   */
  const subtle = Object.freeze({
    async digest(algorithm, data) {
      const bytes = viewBytes(data);
      const result = digestBytes(digestAlgorithmName(algorithm), bytes);
      // Copy out of the Buffer's pool so the caller gets a standalone
      // ArrayBuffer, exactly like the WebCrypto contract promises.
      return result.buffer.slice(
        result.byteOffset,
        result.byteOffset + result.byteLength,
      );
    },
  });

  /**
   * `crypto.getRandomValues` — fills the view in place and returns it.
   * Matches the WebCrypto quota (64 KiB per call) and rejects the float
   * views, which the spec does not allow.
   */
  function getRandomValues(array) {
    const isIntegerView =
      typeof ArrayBuffer !== "undefined" &&
      ArrayBuffer.isView(array) &&
      !(array instanceof DataView) &&
      !(array instanceof Float32Array) &&
      !(array instanceof Float64Array);
    if (!isIntegerView) {
      throw new TypeError(
        "crypto.getRandomValues: argument must be an integer TypedArray",
      );
    }
    if (array.byteLength > 65536) {
      throw new Error(
        "crypto.getRandomValues: quota exceeded (65536 bytes per call)",
      );
    }
    const bytes = randomBytes(array.byteLength);
    new Uint8Array(array.buffer, array.byteOffset, array.byteLength).set(bytes);
    return array;
  }

  /**
   * Coerce a Node-style data argument — a string (with the given encoding) or
   * any `BufferSource` — to a `Buffer`. Shared by `createHash` / `createHmac`;
   * `op` names the caller so the `TypeError` says which argument was wrong.
   */
  function sourceBytes(data, encoding, op) {
    if (typeof data === "string") {
      return BufferCtor.from(data, encoding || "utf8");
    }
    if (typeof ArrayBuffer !== "undefined" && data instanceof ArrayBuffer) {
      return BufferCtor.from(new Uint8Array(data));
    }
    if (typeof ArrayBuffer !== "undefined" && ArrayBuffer.isView(data)) {
      return BufferCtor.from(
        new Uint8Array(data.buffer, data.byteOffset, data.byteLength),
      );
    }
    throw new TypeError(`${op} must be a string or BufferSource`);
  }

  /**
   * `createHash(algorithm)` — buffering hash with Node's `update` /
   * `digest([encoding])` shape. The host digests the whole message at once,
   * so `update` only accumulates; that is invisible to callers and avoids
   * reimplementing the streaming state machine on the JS side. Reusing a hash
   * after `digest` throws instead of silently hashing stale state.
   */
  function createHash(algorithm) {
    const name = String(algorithm);
    const chunks = [];
    let finalized = false;
    const hash = {
      update(data, encoding) {
        if (finalized) {
          // Node raises ERR_CRYPTO_HASH_FINALIZED here; an extension that
          // reuses a finished hash gets a clear error instead of a silently
          // ignored update.
          throw new Error("createHash.update: the hash was already finalized by digest()");
        }
        chunks.push(sourceBytes(data, encoding, "createHash.update"));
        return hash;
      },
      digest(encoding) {
        if (finalized) {
          throw new Error("createHash.digest: the hash was already finalized");
        }
        finalized = true;
        const message = BufferCtor.concat(chunks);
        // `crypto.digest` rejects unknown names, so the failure carries the
        // host's readable message for e.g. `sha512`.
        const digest = digestBytes(name, message);
        return encoding === undefined ? digest : digest.toString(String(encoding));
      },
    };
    return hash;
  }

  /**
   * `createHmac(algorithm, key)` — Node's buffering HMAC. `update(data[,
   * encoding])` chains and `digest([encoding])` returns the MAC, exactly like
   * `createHash`. Both refuse to be reused once `digest` has run (Node's
   * `ERR_CRYPTO_HASH_FINALIZED`). The host's `crypto.hmac` op applies RFC 2104
   * on top of the same hand-rolled SHA-1 / SHA-256 primitives, so HMAC needs
   * no crypto backend of its own; `key` is a string (UTF-8, or the given
   * encoding) or a `BufferSource`, and is copied when `createHmac` is called.
   * A third `{ encoding }` argument sets the encoding of a string key (Node's
   * `createHmac(algorithm, key[, options])`).
   */
  function createHmac(algorithm, key, options) {
    const name = String(algorithm);
    if (key === undefined || key === null) {
      throw new TypeError("createHmac: the key argument is required");
    }
    const keyEncoding =
      options && typeof options === "object" ? options.encoding : undefined;
    const keyBytes = sourceBytes(key, keyEncoding, "createHmac key");
    const chunks = [];
    let finalized = false;
    const hmac = {
      update(data, encoding) {
        if (finalized) {
          throw new Error("createHmac.update: the hmac was already finalized by digest()");
        }
        chunks.push(sourceBytes(data, encoding, "createHmac.update"));
        return hmac;
      },
      digest(encoding) {
        if (finalized) {
          throw new Error("createHmac.digest: the hmac was already finalized");
        }
        finalized = true;
        const message = BufferCtor.concat(chunks);
        // `crypto.hmac` rejects unknown names with the host's readable
        // message (e.g. `md5`), never a wrong MAC.
        const result = __pi_node_call("crypto.hmac", {
          algorithm: name,
          keyBase64: keyBytes.toString("base64"),
          base64: message.toString("base64"),
        });
        const digest = BufferCtor.from(result.base64, "base64");
        return encoding === undefined ? digest : digest.toString(String(encoding));
      },
    };
    return hmac;
  }

  const webcrypto = Object.freeze({
    getRandomValues: getRandomValues,
    randomUUID: randomUUID,
    subtle: subtle,
  });

  const mod = {
    randomBytes: randomBytes,
    randomUUID: randomUUID,
    randomInt: randomInt,
    getRandomValues: getRandomValues,
    createHash: createHash,
    createHmac: createHmac,
    subtle: subtle,
    webcrypto: webcrypto,
  };
  mod.default = mod;
  return Object.freeze(mod);
})();

// ---------------------------------------------------------------------------
// `node:zlib` — the subset the upstream repo actually calls, plus the raw
// variants that sit under it. `pi-ai` reads `process.getBuiltinModule("node:zlib")`
// to zstd-compress Codex request bodies (`packages/ai/src/api/openai-codex-responses.ts`),
// `tool-result-images.test.ts` builds PNG chunks from `crc32` + `deflateSync`,
// and `doom-overlay/wad-finder.ts` gunzips a downloaded WAD with
// `gunzipSync`. Two backends stand behind those: `zstd = 0.13` (the
// workspace already bundles it for `pi-session`) and a pure-Rust
// RFC 1951/1950/1952 codec in `crates/pi-extensions/src/deflate.rs`
// (the offline registry has no `flate2` / `miniz_oxide`, so the format is
// implemented directly).
//
// `zstdCompressSync` accepts `options.params[constants.ZSTD_c_compressionLevel]`
// like Node, because that is exactly how `openai-codex-responses.ts` requests
// level 3. The gzip/deflate family accepts `options.level` (`-1..=9`) and
// ignores the rest of Node's options bag — the async/callback forms
// (`deflate` / `gzip` / …) are not provided because the bridge is
// synchronous and nothing in the repo uses them. `unzipSync` (gzip/xz auto
// detection) is likewise out: no upstream caller.
// ---------------------------------------------------------------------------

const __pi_zlib_module = (() => {
  const BufferCtor = __pi_buffer_module.Buffer;
  // `zstd.h`'s `ZSTD_c_compressionLevel`. Node re-exports the zstd parameter
  // enum through `zlib.constants`, so the upstream call site works verbatim.
  const ZSTD_c_compressionLevel = 100;

  /** Resolve the compression level from Node's options shape. */
  function zstdLevel(options) {
    if (options && typeof options === "object") {
      const params = options.params;
      if (params && typeof params === "object") {
        const fromParams = params[ZSTD_c_compressionLevel];
        if (fromParams !== undefined && fromParams !== null) return Number(fromParams);
      }
      if (options.level !== undefined && options.level !== null) return Number(options.level);
    }
    return undefined;
  }

  function zstdCompressSync(data, options) {
    const args = { base64: BufferCtor.__toBase64(data) };
    const level = zstdLevel(options);
    if (level !== undefined) args.level = level;
    return BufferCtor.from(__pi_node_call("zlib.zstdCompress", args).base64, "base64");
  }

  function zstdDecompressSync(data) {
    const result = __pi_node_call("zlib.zstdDecompress", {
      base64: BufferCtor.__toBase64(data),
    });
    return BufferCtor.from(result.base64, "base64");
  }

  function crc32(data, value) {
    const args = { base64: BufferCtor.__toBase64(data) };
    if (value !== undefined) args.value = Number(value) >>> 0;
    return __pi_node_call("zlib.crc32", args).value >>> 0;
  }

  /**
   * Resolve `options.level` the way Node's options bag does. Node rejects
   * anything outside -1..=9 with a `RangeError`; the Rust side enforces the
   * same bound as a backstop.
   */
  function compressionLevel(options) {
    if (options === undefined || options === null || typeof options !== "object") {
      return undefined;
    }
    const level = options.level;
    if (level === undefined || level === null) return undefined;
    const numeric = Number(level);
    if (!Number.isInteger(numeric) || numeric < -1 || numeric > 9) {
      const error = new RangeError(
        'The value of "options.level" is out of range. It must be >= -1 and <= 9. ' +
          `Received ${String(level)}`,
      );
      error.code = "ERR_OUT_OF_RANGE";
      throw error;
    }
    return numeric;
  }

  /** Shared shape for the six gzip/deflate entry points. */
  function codecCall(op, data, options) {
    const args = { base64: BufferCtor.__toBase64(data) };
    const level = compressionLevel(options);
    if (level !== undefined) args.level = level;
    return BufferCtor.from(__pi_node_call(op, args).base64, "base64");
  }

  function deflateSync(data, options) {
    return codecCall("zlib.deflate", data, options);
  }
  function inflateSync(data, options) {
    return codecCall("zlib.inflate", data, options);
  }
  function deflateRawSync(data, options) {
    return codecCall("zlib.deflateRaw", data, options);
  }
  function inflateRawSync(data, options) {
    return codecCall("zlib.inflateRaw", data, options);
  }
  function gzipSync(data, options) {
    return codecCall("zlib.gzip", data, options);
  }
  function gunzipSync(data, options) {
    return codecCall("zlib.gunzip", data, options);
  }

  const mod = {
    zstdCompressSync: zstdCompressSync,
    zstdDecompressSync: zstdDecompressSync,
    deflateSync: deflateSync,
    inflateSync: inflateSync,
    deflateRawSync: deflateRawSync,
    inflateRawSync: inflateRawSync,
    gzipSync: gzipSync,
    gunzipSync: gunzipSync,
    crc32: crc32,
    constants: Object.freeze({
      ZSTD_c_compressionLevel: ZSTD_c_compressionLevel,
      // Node re-exports zlib's level constants; the upstream call sites
      // only use the zstd parameter enum, but these cost nothing and make
      // `zlib.constants.Z_BEST_SPEED` style code work.
      Z_NO_COMPRESSION: 0,
      Z_BEST_SPEED: 1,
      Z_BEST_COMPRESSION: 9,
      Z_DEFAULT_COMPRESSION: -1,
    }),
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

// ---------------------------------------------------------------------------
// `node:child_process` — `exec` / `execFile` / `execSync` / `execFileSync` /
// `spawn` / `spawnSync`, backed by the `child_process.*` host ops (see
// `src/host.rs`). The buffered host op is the primitive: the callback and
// promise forms run it and schedule their result on the microtask queue,
// while `spawn` gets a live handle that a reader task per pipe keeps filled.
//
// Divergences from Node, deliberate and mirrored in `docs/NODE_BUILTINS.md`:
//   * every child is bounded by the host per-call timeout (5 s normally,
//     300 s in interactive mode). When it fires the child is killed and
//     `exit` / `close` report `signal: "SIGKILL"`; `kill()` also always
//     sends SIGKILL and ignores its signal argument.
//   * `spawn` pipes are fully buffered on the host side, so the child never
//     sees backpressure and `maxBuffer` does not apply to `spawn`.
//   * `stdio: "inherit"` is approximated by replaying captured output
//     through `process.stdout` / `process.stderr` when the child exits.
//   * writing to `child.stdin` is not supported (`spawn`'s stdin is
//     `/dev/null`); `detached` and `fork` are not supported either.
//   * there is no implicit 1 MiB `maxBuffer` default.
// ---------------------------------------------------------------------------

const __pi_child_process_module = (() => {
  const BufferCtor = __pi_buffer_module.Buffer;
  const PROMISIFY_CUSTOM = Symbol.for("nodejs.util.promisify.custom");

  /** The `child.stdout` / `child.stderr` shape: the Readable subset the
   *  examples use (`on("data")` / `on("end")` / `setEncoding`). Chunks that
   *  arrive before the first `data` listener are buffered, so attaching the
   *  listener a tick after `spawn` cannot lose output. */
  class ReadableLike extends Emitter {
    constructor() {
      super();
      this.__buffer = [];
      this.__ended = false;
      this.__encoding = null;
    }

    setEncoding(encoding) {
      this.__encoding = normalizeEncoding(encoding);
      return this;
    }

    resume() {
      this.__flush();
      return this;
    }

    pause() {
      return this;
    }

    read() {
      return this.__buffer.length === 0 ? null : this.__buffer.shift();
    }

    on(type, listener) {
      super.on(type, listener);
      if (String(type) === "data") this.__flush();
      return this;
    }

    addListener(type, listener) {
      return this.on(type, listener);
    }

    __push(bytes) {
      const value = this.__encoding == null ? bytes : bytes.toString(this.__encoding);
      if (this.listenerCount("data") > 0) this.emit("data", value);
      else this.__buffer.push(value);
    }

    __flush() {
      while (this.__buffer.length > 0) this.emit("data", this.__buffer.shift());
    }

    __end() {
      if (this.__ended) return;
      this.__ended = true;
      this.__flush();
      this.emit("end");
      this.emit("close");
    }
  }

  /** Stands in for `child.stdin`. The host does not bridge a child's stdin,
   *  so writing fails loudly instead of silently dropping bytes. */
  class WritableLike extends Emitter {
    write() {
      throw new Error(
        "pi extension host: writing to a child process stdin is not supported",
      );
    }

    end() {
      return this;
    }

    destroy() {
      return this;
    }
  }

  // -------------------------------------------------------------------------
  // Options / encodings
  // -------------------------------------------------------------------------

  function normalizeEncoding(encoding) {
    if (encoding === null) return null;
    const name = String(encoding).toLowerCase();
    if (name === "buffer") return null;
    // `Buffer.from("", enc)` validates the name exactly like `toString` does.
    try {
      BufferCtor.from("", name);
    } catch (_error) {
      const error = new TypeError("Unknown encoding: " + encoding);
      error.code = "ERR_UNKNOWN_ENCODING";
      throw error;
    }
    if (name === "utf-8") return "utf8";
    if (name === "ucs2" || name === "ucs-2" || name === "utf-16le") return "utf16le";
    if (name === "binary" || name === "ascii") return "latin1";
    return name;
  }

  /** Node lets `options` be a bare encoding string (`exec(cmd, "utf8", cb)`). */
  function normalizeOptions(options) {
    if (options === undefined || options === null) return undefined;
    if (typeof options === "string") return { encoding: options };
    if (typeof options !== "object") {
      throw new TypeError('The "options" argument must be an object or a string');
    }
    return options;
  }

  function optionEncoding(options, fallback) {
    if (options == null || options.encoding === undefined) {
      return normalizeEncoding(fallback);
    }
    return normalizeEncoding(options.encoding);
  }

  function encodeResult(bytes, encoding) {
    return encoding == null ? bytes : bytes.toString(encoding);
  }

  function normalizeArgs(args) {
    if (args === undefined || args === null) return [];
    if (!Array.isArray(args)) {
      throw new TypeError('The "args" argument must be an array of strings');
    }
    return args.map((arg) => String(arg));
  }

  function normalizeStdio(options) {
    const raw = options == null ? undefined : options.stdio;
    const defaults = { stdin: "ignore", stdout: "pipe", stderr: "pipe" };
    if (raw === undefined || raw === null || raw === "pipe") return defaults;
    if (raw === "ignore") return { stdin: "ignore", stdout: "ignore", stderr: "ignore" };
    if (raw === "inherit") {
      return { stdin: "inherit", stdout: "inherit", stderr: "inherit" };
    }
    if (Array.isArray(raw)) {
      return {
        stdin: stdioEntry(raw[0], "ignore"),
        stdout: stdioEntry(raw[1], "pipe"),
        stderr: stdioEntry(raw[2], "pipe"),
      };
    }
    throw new Error(
      "pi extension host: unsupported `stdio` option " + JSON.stringify(raw),
    );
  }

  function stdioEntry(value, fallback) {
    if (value === undefined || value === null) return fallback;
    if (value === "pipe" || value === "ignore" || value === "inherit") return value;
    throw new Error(
      'pi extension host: `stdio` entries must be "pipe", "ignore" or "inherit" (got ' +
        JSON.stringify(value) +
        '); streams and "ipc" are not supported',
    );
  }

  function buildRequest(program, args, options, defaultShell) {
    if (typeof program !== "string" || program.length === 0) {
      throw new TypeError('The "file" argument must be a non-empty string');
    }
    const opts = options == null ? {} : options;
    const request = {
      program: program,
      args: normalizeArgs(args),
      shell: opts.shell === undefined ? defaultShell : Boolean(opts.shell),
    };
    if (typeof opts.cwd === "string" && opts.cwd.length > 0) request.cwd = opts.cwd;
    if (opts.env != null) {
      if (typeof opts.env !== "object") {
        throw new TypeError('The "env" argument must be an object');
      }
      const env = {};
      for (const key of Object.keys(opts.env)) {
        const value = opts.env[key];
        if (value === undefined) continue;
        env[key] = value === null ? null : String(value);
      }
      request.env = env;
    }
    if (typeof opts.timeout === "number" && opts.timeout > 0) request.timeoutMs = opts.timeout;
    if (typeof opts.maxBuffer === "number" && opts.maxBuffer > 0) {
      request.maxBuffer = opts.maxBuffer;
    }
    if (opts.input != null) {
      const bytes =
        typeof opts.input === "string"
          ? BufferCtor.from(opts.input, "utf8")
          : BufferCtor.from(opts.input);
      request.input = bytes.toString("base64");
    }
    return request;
  }

  function runSync(request) {
    return __pi_node_call("child_process.runSync", request);
  }

  function toBytes(base64) {
    return BufferCtor.from(typeof base64 === "string" ? base64 : "", "base64");
  }

  /** `stdio: "inherit"` best effort: the captured bytes are replayed through
   *  the host's log-backed `process.stdout` / `process.stderr`. */
  function writeThrough(name, bytes) {
    if (bytes.length === 0) return;
    const stream = __pi_process_module[name];
    if (stream && typeof stream.write === "function") {
      stream.write(bytes.toString("utf8"));
    }
  }

  function describeCommand(request) {
    if (request.shell || request.args.length === 0) return request.program;
    return request.program + " " + request.args.join(" ");
  }

  function decorateSpawnError(error, request) {
    if (request) {
      error.spawnargs = [request.program].concat(request.args);
      if (error.syscall === undefined) error.syscall = "spawn " + request.program;
    }
    if (error.status === undefined) error.status = null;
    if (error.signal === undefined) error.signal = null;
    return error;
  }

  function makeMaxBufferError() {
    const error = new Error("stdout maxBuffer length exceeded");
    error.code = "ERR_CHILD_PROCESS_STDIO_MAXBUFFER";
    error.killed = false;
    return error;
  }

  function makeExecError(request, outcome, stdoutBytes, stderrBytes, encoding, fromSync) {
    const commandLine = describeCommand(request);
    const stderrText = stderrBytes.toString("utf8");
    const error = new Error(
      "Command failed: " + commandLine + (stderrText ? "\n" + stderrText : ""),
    );
    const failedBySignal = outcome.killed || outcome.timedOut || outcome.code === null;
    // Node reports a timeout as `ETIMEDOUT` (with `status: null`) for the whole
    // `exec*` family, including `execSync`.
    error.code = outcome.timedOut ? "ETIMEDOUT" : failedBySignal ? null : outcome.code;
    error.killed = Boolean(outcome.killed || outcome.timedOut);
    error.signal = outcome.signal === null ? null : outcome.signal;
    error.cmd = commandLine;
    error.stdout = encodeResult(stdoutBytes, encoding);
    error.stderr = encodeResult(stderrBytes, encoding);
    if (fromSync) {
      // `execSync` / `execFileSync` additionally expose the `spawnSync` shape.
      error.status = outcome.code === null ? null : outcome.code;
      error.output = [null, error.stdout, error.stderr];
      error.pid = outcome.pid;
    }
    return error;
  }

  // -------------------------------------------------------------------------
  // Buffered (one-shot) forms — the source of truth for `exec*` / `*Sync`
  // -------------------------------------------------------------------------

  function spawnSyncImpl(file, args, options) {
    const stdio = normalizeStdio(options);
    const encoding = optionEncoding(options, null);
    let request;
    let outcome;
    try {
      request = buildRequest(file, args, options, false);
      outcome = runSync(request);
    } catch (error) {
      // A spawn failure (ENOENT / EACCES / …) is a *value* for `spawnSync`.
      return {
        error: decorateSpawnError(error, request),
        status: null,
        signal: null,
        output: null,
        pid: 0,
        stdout: undefined,
        stderr: undefined,
      };
    }

    const stdoutBytes = toBytes(outcome.stdout);
    const stderrBytes = toBytes(outcome.stderr);
    if (stdio.stdout === "inherit") writeThrough("stdout", stdoutBytes);
    if (stdio.stderr === "inherit") writeThrough("stderr", stderrBytes);
    const stdout = stdio.stdout === "pipe" ? encodeResult(stdoutBytes, encoding) : null;
    const stderr = stdio.stderr === "pipe" ? encodeResult(stderrBytes, encoding) : null;
    const result = {
      status: outcome.code === null ? null : outcome.code,
      signal: outcome.signal === null ? null : outcome.signal,
      output: [null, stdout, stderr],
      pid: outcome.pid,
      stdout: stdout,
      stderr: stderr,
    };
    if (outcome.maxBufferExceeded) {
      // Node reports ENOBUFS for `spawnSync` and ERR_CHILD_PROCESS_STDIO_MAXBUFFER
      // for the `exec*` family; keep that split.
      const error = new Error("spawnSync " + request.program + " ENOBUFS");
      error.code = "ENOBUFS";
      error.errno = -105;
      error.syscall = "spawnSync " + request.program;
      result.error = error;
    } else if (outcome.timedOut) {
      const error = new Error("spawnSync " + request.program + " ETIMEDOUT");
      error.code = "ETIMEDOUT";
      error.syscall = "spawnSync " + request.program;
      result.error = error;
    }
    return result;
  }

  function execSyncImpl(program, args, options, defaultShell) {
    const encoding = optionEncoding(options, null);
    const request = buildRequest(program, args, options, defaultShell);
    let outcome;
    try {
      outcome = runSync(request);
    } catch (error) {
      throw decorateSpawnError(error, request);
    }
    const stdoutBytes = toBytes(outcome.stdout);
    const stderrBytes = toBytes(outcome.stderr);
    if (outcome.maxBufferExceeded) throw makeMaxBufferError();
    if (outcome.code !== 0 || outcome.killed || outcome.timedOut) {
      throw makeExecError(request, outcome, stdoutBytes, stderrBytes, encoding, true);
    }
    return encodeResult(stdoutBytes, encoding);
  }

  function execImpl(program, args, options, callback, defaultShell) {
    if (typeof options === "function") {
      callback = options;
      options = undefined;
    }
    if (typeof callback !== "function") {
      throw new TypeError('The "callback" argument must be of type function');
    }
    const stdio = normalizeStdio(options);
    const encoding = optionEncoding(options, "utf8");
    let request;
    let outcome;
    try {
      request = buildRequest(program, args, options, defaultShell);
      outcome = runSync(request);
    } catch (error) {
      const spawnError = decorateSpawnError(error, request);
      __pi_schedule(() => callback(spawnError, "", ""));
      return;
    }
    const stdoutBytes = toBytes(outcome.stdout);
    const stderrBytes = toBytes(outcome.stderr);
    if (stdio.stdout === "inherit") writeThrough("stdout", stdoutBytes);
    if (stdio.stderr === "inherit") writeThrough("stderr", stderrBytes);
    const stdout = encodeResult(stdoutBytes, encoding);
    const stderr = encodeResult(stderrBytes, encoding);
    if (outcome.maxBufferExceeded) {
      __pi_schedule(() => callback(makeMaxBufferError(), stdout, stderr));
      return;
    }
    if (outcome.code !== 0 || outcome.killed || outcome.timedOut) {
      const error = makeExecError(
        request,
        outcome,
        stdoutBytes,
        stderrBytes,
        encoding,
        false,
      );
      __pi_schedule(() => callback(error, stdout, stderr));
      return;
    }
    __pi_schedule(() => callback(null, stdout, stderr));
  }

  // -------------------------------------------------------------------------
  // `spawn` — live child
  // -------------------------------------------------------------------------

  /** Drain one host-side pipe, handing every chunk to `sink`. */
  async function pump(handle, name, sink) {
    for (;;) {
      const raw = await globalThis.host_child_read(handle, name);
      const chunk = typeof raw === "string" ? JSON.parse(raw) : raw;
      const bytes = toBytes(chunk && chunk.data);
      if (bytes.length > 0) sink(bytes);
      if (!chunk || chunk.done) return;
    }
  }

  async function pumpStream(handle, stream, name) {
    try {
      await pump(handle, name, (bytes) => stream.__push(bytes));
    } catch (_error) {
      // The handle was reaped underneath us; report EOF rather than hanging.
    }
    stream.__end();
  }

  class ChildProcess extends Emitter {
    constructor(handle, pid, stdio, spawnfile) {
      super();
      this.pid = pid;
      this.killed = false;
      this.exitCode = null;
      this.signalCode = null;
      this.spawnfile = spawnfile;
      this.spawnargs = [];
      this.__handle = handle;
      this.__stdio = stdio;
      this.__closed = false;
      this.stdout = stdio.stdout === "pipe" ? new ReadableLike() : null;
      this.stderr = stdio.stderr === "pipe" ? new ReadableLike() : null;
      this.stdin = stdio.stdin === "ignore" ? null : new WritableLike();
    }

    kill() {
      if (this.killed || this.exitCode !== null || this.signalCode !== null) return false;
      let killed = false;
      try {
        killed = Boolean(__pi_node_call("child_process.kill", { handle: this.__handle }));
      } catch (_error) {
        killed = false;
      }
      if (killed) this.killed = true;
      return killed;
    }

    ref() {
      return this;
    }

    unref() {
      return this;
    }

    __start() {
      const child = this;
      const streamTasks = [];
      if (child.stdout) {
        streamTasks.push(pumpStream(child.__handle, child.stdout, "stdout"));
      } else {
        streamTasks.push(
          pump(child.__handle, "stdout", (bytes) => {
            if (child.__stdio.stdout === "inherit") writeThrough("stdout", bytes);
          }),
        );
      }
      if (child.stderr) {
        streamTasks.push(pumpStream(child.__handle, child.stderr, "stderr"));
      } else {
        streamTasks.push(
          pump(child.__handle, "stderr", (bytes) => {
            if (child.__stdio.stderr === "inherit") writeThrough("stderr", bytes);
          }),
        );
      }

      const exitPromise = globalThis.host_child_wait(child.__handle).then((raw) =>
        typeof raw === "string" ? JSON.parse(raw) : raw,
      );
      exitPromise
        .then((exit) => {
          child.exitCode = exit && exit.code !== undefined ? exit.code : null;
          child.signalCode = exit && exit.signal !== undefined ? exit.signal : null;
          child.emit("exit", child.exitCode, child.signalCode);
        })
        .catch(() => {});

      Promise.all([exitPromise].concat(streamTasks))
        .then((results) => {
          if (child.__closed) return;
          child.__closed = true;
          try {
            __pi_node_call("child_process.reap", { handle: child.__handle });
          } catch (_error) {
            // Already reaped / handle unknown: `close` still fires.
          }
          const exit = results[0] || {};
          child.emit(
            "close",
            exit.code === undefined ? null : exit.code,
            exit.signal === undefined ? null : exit.signal,
          );
        })
        .catch(() => {
          if (child.__closed) return;
          child.__closed = true;
          child.emit("close", null, null);
        });
    }
  }

  function spawnImpl(file, args, options, defaultShell) {
    if (
      typeof globalThis.host_child_read !== "function" ||
      typeof globalThis.host_child_wait !== "function"
    ) {
      throw new Error("child_process.spawn is not available in this host build");
    }
    const stdio = normalizeStdio(options);
    const request = buildRequest(file, args, options, defaultShell);
    const started = __pi_node_call("child_process.spawn", request);
    const child = new ChildProcess(started.handle, started.pid, stdio, request.program);
    child.spawnargs = [request.program].concat(request.args);
    child.__start();
    return child;
  }

  // -------------------------------------------------------------------------
  // Public surface
  // -------------------------------------------------------------------------

  function spawn(command, args, options) {
    if (options === undefined && args !== null && typeof args === "object" && !Array.isArray(args)) {
      options = args;
      args = [];
    }
    return spawnImpl(command, args, normalizeOptions(options), false);
  }

  function spawnSync(command, args, options) {
    if (options === undefined && args !== null && typeof args === "object" && !Array.isArray(args)) {
      options = args;
      args = [];
    }
    return spawnSyncImpl(command, args, normalizeOptions(options));
  }

  function exec(command, options, callback) {
    if (typeof options === "function") {
      callback = options;
      options = undefined;
    }
    return execImpl(String(command), [], normalizeOptions(options), callback, true);
  }

  function execFile(file, args, options, callback) {
    if (typeof args === "function") {
      callback = args;
      args = [];
      options = undefined;
    } else if (typeof options === "function") {
      callback = options;
      options = undefined;
    }
    return execImpl(file, args, normalizeOptions(options), callback, false);
  }

  function execSync(command, options) {
    return execSyncImpl(String(command), [], normalizeOptions(options), true);
  }

  function execFileSync(file, args, options) {
    if (options === undefined && args !== null && typeof args === "object" && !Array.isArray(args)) {
      options = args;
      args = [];
    }
    return execSyncImpl(file, args, normalizeOptions(options), false);
  }

  /** `promisify(exec)` resolves `{stdout, stderr}` and rejects with the same
   *  error, carrying `.stdout` / `.stderr` (Node installs this hook as the
   *  `util.promisify.custom` symbol on both functions). */
  function promisifyExec(original) {
    return function (...args) {
      return new Promise((resolve, reject) => {
        original.apply(
          this,
          args.concat((error, stdout, stderr) => {
            if (error) {
              error.stdout = stdout;
              error.stderr = stderr;
              reject(error);
            } else {
              resolve({ stdout: stdout, stderr: stderr });
            }
          }),
        );
      });
    };
  }

  exec[PROMISIFY_CUSTOM] = promisifyExec(exec);
  execFile[PROMISIFY_CUSTOM] = promisifyExec(execFile);

  const mod = {
    exec: exec,
    execFile: execFile,
    execSync: execSync,
    execFileSync: execFileSync,
    spawn: spawn,
    spawnSync: spawnSync,
    ChildProcess: ChildProcess,
    // `fork` needs an IPC channel the host does not have; a named failure
    // beats `undefined is not a function`.
    fork: () => {
      throw new Error("child_process.fork is not supported in the pi extension host");
    },
  };
  mod.default = mod;
  return Object.freeze(mod);
})();

// ---------------------------------------------------------------------------
// `node:module` — the CommonJS module facade, scoped to the virtual module
// map.
//
// The consumers are real and their shapes differ: `doom-overlay/doom-engine.ts:6`
// and `core/extensions/loader.ts:76` read `createRequire(import.meta.url)`,
// `chord/src/node/bundle-loader.ts:177` passes a *path* and then gates package
// names on `isBuiltin(specifier)` (:180, :206), `tui/src/native-module-path.ts:5`
// calls `require.resolve("@earendil-works/pi-tui")` inside a `try`/`catch`, and
// `tui/test/native-platform.test.ts:59,85` mocks a native addon through
// `new Module(path)` + `require.cache[path]`. So `createRequire` resolves
// *bridged* modules — the builtins and the SDK virtual modules — and nothing
// else; `require.cache` is the real `Module._cache`; and an unresolvable
// specifier throws so the `try`/`catch` above behaves like it does on Node.
// That last part is the deliberate sandbox line: widening it to arbitrary files
// off disk is a separate decision, not a side effect of adding a module (see
// `docs/NODE_BUILTINS.md`, "Not bridged"). `registerHooks`
// (`ai/test/lazy-module-load.test.ts:19`, `experimental/source-resolver.ts:2`)
// stays a documented gap.
//
// `builtinModules` / `isBuiltin` are derived from the virtual module map
// rather than from Node's full builtin list, so they answer "what can this
// host actually resolve?": `isBuiltin("node:stream")` is `false` here where
// Node says `true`. `Module` is present for the one upstream shape that mocks
// a native addon (`packages/tui/test/native-platform.test.ts:85` constructs
// `new Module(path)` and writes `require.cache[path]`), so `require.cache` is
// a real store consulted by `require()` — not a decorative object.
// ---------------------------------------------------------------------------

const __pi_module_module = (() => {
  const hasOwn = (target, key) => Object.prototype.hasOwnProperty.call(target, key);

  function invalidArgType(name, value) {
    const received = value === null ? "null" : typeof value;
    const error = new TypeError(
      'The "' + name + '" argument must be of type string. Received type ' + received,
    );
    error.code = "ERR_INVALID_ARG_TYPE";
    return error;
  }

  function moduleNotFound(specifier, requester) {
    let message = "Cannot find module '" + specifier + "'";
    if (specifier.indexOf("node:") === 0) {
      message +=
        " (`" +
        specifier +
        "` is a Node builtin the pi extension host does not bridge; docs/NODE_BUILTINS.md lists what it does)";
    }
    if (requester) message += "\nRequire stack:\n- " + requester;
    const error = new Error(message);
    error.code = "MODULE_NOT_FOUND";
    if (requester) error.requireStack = [requester];
    return error;
  }

  function makeRequire(requester) {
    function requireFromModule(specifier) {
      const key = String(specifier);
      const virtual = globalThis.__pi_virtual_modules[key];
      if (virtual) return virtual;
      const cached = Module._cache[key];
      if (cached && cached.exports !== undefined) return cached.exports;
      throw moduleNotFound(key, requester);
    }

    requireFromModule.resolve = function resolve(specifier) {
      const key = String(specifier);
      if (globalThis.__pi_virtual_modules[key]) return key;
      if (Module._cache[key]) return key;
      throw moduleNotFound(key, requester);
    };
    // Node's `require.cache` *is* `Module._cache`; keeping one store means a
    // mocked module is visible from either handle.
    requireFromModule.cache = Module._cache;
    requireFromModule.main = undefined;
    return requireFromModule;
  }

  /** `module.createRequire(from)` / `Module.createRequire(from)`. */
  function createRequire(from) {
    let requester = "";
    if (typeof from === "string") requester = from;
    else if (from && typeof from === "object" && typeof from.href === "string") {
      requester = from.href;
    } else {
      throw invalidArgType("filename", from);
    }
    return makeRequire(requester);
  }

  function isBuiltin(specifier) {
    if (typeof specifier !== "string") throw invalidArgType("specifier", specifier);
    const key = specifier.indexOf("node:") === 0 ? specifier : "node:" + specifier;
    return hasOwn(globalThis.__pi_virtual_modules, key);
  }

  let builtinCache = null;
  function builtinModules() {
    if (builtinCache) return builtinCache;
    const names = [];
    for (const key of Object.keys(globalThis.__pi_virtual_modules)) {
      if (key.indexOf("node:") === 0) names.push(key.slice("node:".length));
    }
    builtinCache = Object.freeze(names.sort());
    return builtinCache;
  }

  class Module {
    constructor(id, parent) {
      this.id = id === undefined ? "" : String(id);
      this.path = this.id;
      this.filename = this.id;
      this.exports = {};
      this.loaded = false;
      this.children = [];
      this.paths = [];
      if (parent) this.parent = parent;
    }

    require(specifier) {
      return makeRequire(this.filename || this.id)(specifier);
    }
  }
  Module._cache = Object.create(null);
  Module.createRequire = createRequire;
  Module.isBuiltin = isBuiltin;
  Object.defineProperty(Module, "builtinModules", {
    get: builtinModules,
    enumerable: true,
  });

  const mod = {
    createRequire: createRequire,
    isBuiltin: isBuiltin,
    Module: Module,
  };
  Object.defineProperty(mod, "builtinModules", {
    get: builtinModules,
    enumerable: true,
  });
  mod.default = mod;
  return Object.freeze(mod);
})();

// ---------------------------------------------------------------------------
// `node:readline` — line reading off a stream-like input, plus UI-backed
// questions.
//
// `createInterface({ input })` accepts any object with `on("data")` /
// `on("end")` — exactly the `child.stdout` shape `node:child_process` hands
// out, which is what the TUI's own tools read (`core/tools/grep.ts:169` and
// `core/tools/find.ts:217` do `createInterface({ input: child.stdout })` +
// `on("line")`, and `examples/rpc-extension-ui.ts:521` does the same over a
// spawned agent's stdout with `terminal: false`). Lines split on `\n` /
// `\r\n` / lone `\r`; a `\r\n` pair straddling two chunks is still one break
// because a pending CR is remembered instead of using Node's 100 ms
// `crlfDelay` timer — the same thing `core/session-manager.ts:698` asks for
// with `crlfDelay: Infinity`, and `git-merge-and-resolve.ts:37` gets with
// `for await (const line of rl)` over a file stream.
//
// Terminal/raw mode is a documented gap, not a fake: there is no TTY in the
// embedded engine and the host owns stdin, so `terminal: true` throws
// `ERR_READLINE_TTY_UNSUPPORTED`. The `process.stdin` callers
// (`packages/ai/src/cli.ts:48`, `core/main.ts:283`) therefore take the
// UI-dialog path: with no `input`, `question()` goes through the session's UI
// (`host_ui_input`, the channel `ctx.ui.input` uses); a host without UI fails
// fast from `createInterface` with `ERR_READLINE_NO_INPUT` instead of handing
// back an interface whose promises never settle. `close()` (and input `end`)
// settles every pending consumer.
// ---------------------------------------------------------------------------

const __pi_readline_module = (() => {
  const BufferCtor = __pi_buffer_module.Buffer;

  function readlineError(code, message) {
    const error = new Error(message);
    error.code = code;
    return error;
  }

  function chunkToString(chunk) {
    if (typeof chunk === "string") return chunk;
    if (chunk instanceof Uint8Array) return BufferCtor.from(chunk).toString("utf8");
    return String(chunk);
  }

  function uiInputAvailable() {
    let hasUI = false;
    if (typeof globalThis._pi_tool_ctx === "string") {
      try {
        hasUI = JSON.parse(globalThis._pi_tool_ctx).hasUI === true;
      } catch (_e) {
        hasUI = false;
      }
    }
    return hasUI && typeof globalThis.host_ui_input === "function";
  }

  /** Renders `query` where Node would: the `output` stream, else the log. */
  function writeQuery(output, query) {
    if (output && typeof output.write === "function") {
      output.write(query);
      return;
    }
    if (typeof globalThis.host_log === "function") {
      globalThis.host_log("info", query.replace(/\n+$/, "") || query);
    }
  }

  class Interface extends Emitter {
    constructor(input, output) {
      super();
      this.__input = input;
      this.__output = output;
      this.__queue = [];
      this.__waiters = [];
      this.__buffer = "";
      this.__pendingCR = false;
      this.__closed = false;
      this.__ended = false;
      this.__prompt = "";
      if (input) {
        this.__onData = (chunk) => this.__push(chunkToString(chunk));
        this.__onEnd = () => this.__finish();
        input.on("data", this.__onData);
        input.on("end", this.__onEnd);
      }
    }

    __push(text) {
      if (this.__closed) return;
      for (let index = 0; index < text.length; index++) {
        const char = text[index];
        if (this.__pendingCR) {
          this.__pendingCR = false;
          // `\r\n` split across two chunks is one break, not two lines.
          if (char === "\n") continue;
        }
        if (char === "\n" || char === "\r") {
          if (char === "\r") this.__pendingCR = true;
          this.__emitLine();
          continue;
        }
        this.__buffer += char;
      }
    }

    __emitLine() {
      const line = this.__buffer;
      this.__buffer = "";
      this.emit("line", line);
      const waiter = this.__waiters.shift();
      if (waiter) waiter({ done: false, value: line });
      else this.__queue.push(line);
    }

    __next() {
      if (this.__queue.length > 0) {
        return Promise.resolve({ done: false, value: this.__queue.shift() });
      }
      if (this.__ended || this.__closed) {
        return Promise.resolve({ done: true, value: undefined });
      }
      return new Promise((resolve) => {
        this.__waiters.push(resolve);
      });
    }

    __settleWaiters() {
      const waiters = this.__waiters;
      this.__waiters = [];
      for (const resolve of waiters) resolve({ done: true, value: undefined });
    }

    __finish() {
      if (this.__ended || this.__closed) return;
      this.__ended = true;
      // Node emits a trailing line without a terminating newline.
      if (this.__buffer.length > 0) this.__emitLine();
      this.__detach();
      this.__settleWaiters();
      if (!this.__closed) {
        this.__closed = true;
        this.emit("close");
      }
    }

    __detach() {
      const input = this.__input;
      if (!input || !this.__onData) return;
      const remove =
        typeof input.off === "function"
          ? input.off
          : typeof input.removeListener === "function"
            ? input.removeListener
            : null;
      if (!remove) return;
      remove.call(input, "data", this.__onData);
      remove.call(input, "end", this.__onEnd);
    }

    /**
     * Ask a question and resolve the next line.
     *
     * With a stream `input` this is Node's behaviour (write the query, take
     * the next line; `null` when the stream ends first). Without one the
     * session UI supplies the answer, and a host that cannot prompt never
     * gets here (`createInterface` already threw).
     */
    question(query, options, callback) {
      if (typeof options === "function") {
        callback = options;
        options = undefined;
      }
      void options;
      const text = query === undefined || query === null ? "" : String(query);
      if (this.__closed) {
        throw readlineError(
          "ERR_USE_AFTER_CLOSE",
          "readline.Interface was closed: no more questions can be asked",
        );
      }
      writeQuery(this.__output, text);
      const answer = this.__input
        ? this.__next().then((result) => (result.done ? null : result.value))
        : Promise.resolve(globalThis.host_ui_input(text, "")).then(
            (value) => (typeof value === "string" ? value : null),
            () => null,
          );
      if (typeof callback === "function") {
        answer.then((value) => callback(value));
        return undefined;
      }
      return answer;
    }

    /** `rl.write(data)` feeds `data` through the same line splitter. */
    write(data) {
      this.__push(String(data));
      return this;
    }

    pause() {
      return this;
    }

    resume() {
      return this;
    }

    setPrompt(prompt) {
      this.__prompt = prompt === undefined ? "" : String(prompt);
      return this;
    }

    getPrompt() {
      return this.__prompt;
    }

    prompt(preserveCursor) {
      void preserveCursor;
      writeQuery(this.__output, this.__prompt);
      return this;
    }

    close() {
      if (this.__closed) return this;
      this.__closed = true;
      this.__detach();
      this.__settleWaiters();
      this.emit("close");
      return this;
    }

    [Symbol.asyncIterator]() {
      const self = this;
      return {
        next: () => self.__next(),
        return: () => Promise.resolve({ done: true, value: undefined }),
        [Symbol.asyncIterator]() {
          return this;
        },
      };
    }
  }

  function createInterface(input, output, terminal) {
    // Node's deprecated positional form `(input, output, terminal)` and the
    // options-object form both reach here.
    let options;
    if (
      input &&
      typeof input === "object" &&
      typeof input.on !== "function" &&
      typeof input.read !== "function"
    ) {
      options = input;
    } else {
      options = { input: input, output: output, terminal: terminal };
    }

    const stream = options.input;
    const outputStream = options.output === undefined ? null : options.output;
    const isTerminal =
      options.terminal === undefined
        ? Boolean(outputStream && outputStream.isTTY === true)
        : Boolean(options.terminal);

    if (isTerminal) {
      throw readlineError(
        "ERR_READLINE_TTY_UNSUPPORTED",
        "readline terminal (raw TTY) mode is not available in the pi extension host: the embedded engine has no TTY and the host owns stdin",
      );
    }
    if (stream !== undefined && stream !== null && typeof stream.on !== "function") {
      throw readlineError(
        "ERR_INVALID_ARG_TYPE",
        'The "input" argument must be a readable stream. Received ' + typeof stream,
      );
    }
    if ((stream === undefined || stream === null) && !uiInputAvailable()) {
      throw readlineError(
        "ERR_READLINE_NO_INPUT",
        "readline.createInterface needs an `input` stream or a session UI: the pi extension host does not expose process.stdin",
      );
    }
    return new Interface(stream === undefined ? null : stream, outputStream);
  }

  const mod = {
    createInterface: createInterface,
    Interface: Interface,
  };
  mod.default = mod;
  return Object.freeze(mod);
})();

// `TextEncoder` / `TextDecoder` are globals on Node; expose them here when the
// engine has no native implementation (QuickJS does not).
if (typeof globalThis.TextEncoder === "undefined") globalThis.TextEncoder = __pi_util_module.TextEncoder;
if (typeof globalThis.TextDecoder === "undefined") globalThis.TextDecoder = __pi_util_module.TextDecoder;

// ===========================================================================
// SDK virtual modules — `@earendil-works/*` (LUM-1120)
//
// Upstream extensions import pi's own SDK packages, not just Node builtins:
//
//   @earendil-works/pi-tui           Text / Box / Container / … components
//   @earendil-works/pi-coding-agent  defineTool / getAgentDir / DynamicBorder
//   @earendil-works/pi-ai            Type / StringEnum / uuidv7 / contentText
//   @earendil-works/pi-agent-core    (type-only in the upstream examples)
//   @earendil-works/pi-ai/compat     provider registry + event stream; the
//                                    builtin provider factories are gaps
//
// The packages used to be published under the `@mariozechner/` scope, so every
// specifier is registered under both scopes as well as the bare package name.
//
// The extension host drives the real region surface (header / footer /
// widgets / editor component / overlay) through `ctx.ui`, so the component
// classes below are the renderables that surface consumes: they implement the
// constructor surface upstream code uses and `render(width)` returns strings.
// A component an extension hands to `ctx.ui.setWidget` / `setHeader` /
// `setFooter` / `setEditorComponent` / `custom` is registered here under a
// numeric id and rendered by the Rust TUI through
// `__pi_ui_render_component`; see the component registry near
// `makeUiContext` and `docs/SDK_MODULES.md`.
//
// Hard rule: a name this file does not implement must never evaluate to
// `undefined`. Implemented names are real values; names that upstream imports
// but this shim cannot back throw an `ERR_PI_SDK_UNIMPLEMENTED` error that
// names the specifier, the export and the doc; any other name throws a
// "has no export" error. `globalThis.__pi_sdk_manifest()` returns the
// machine-readable per-specifier inventory (implemented / documented gaps) so
// `tests/sdk_modules.rs` checks it against the upstream examples.
// ===========================================================================

// --- ANSI / width helpers -------------------------------------------------

const __pi_sdk_ansi_pattern = new RegExp(
  "\\x1b\\[[0-9;?]*[ -/]*[@-~]" +
    "|\\x1b\\][^\\x07\\x1b]*(?:\\x07|\\x1b\\\\)" +
    "|\\x1b[_P^][^\\x07\\x1b]*(?:\\x07|\\x1b\\\\)" +
    "|\\x1b[@-Z\\\\-_]" +
    "|\\x9b[0-9;?]*[ -/]*[@-~]",
  "g",
);

function __pi_sdk_strip_terminal_sequences(text) {
  return String(text).replace(__pi_sdk_ansi_pattern, "");
}

// Approximate terminal cell width for one code point. This is a range table,
// not a full East-Asian-Width/grapheme implementation: combining marks and
// emoji ZWJ sequences are approximated, which `docs/SDK_MODULES.md` records as
// a known divergence from `@earendil-works/pi-tui`.
function __pi_sdk_code_point_width(codePoint) {
  if (!Number.isFinite(codePoint) || codePoint <= 0) return 0;
  if (codePoint < 32) return 0;
  if (codePoint >= 0x7f && codePoint < 0xa0) return 0;
  if (codePoint >= 0x0300 && codePoint <= 0x036f) return 0;
  if (codePoint >= 0x1ab0 && codePoint <= 0x1aff) return 0;
  if (codePoint >= 0x1dc0 && codePoint <= 0x1dff) return 0;
  if (codePoint >= 0x20d0 && codePoint <= 0x20ff) return 0;
  if (codePoint >= 0xfe00 && codePoint <= 0xfe0f) return 0;
  if (codePoint >= 0xfe20 && codePoint <= 0xfe2f) return 0;
  if (
    (codePoint >= 0x1100 && codePoint <= 0x115f) ||
    (codePoint >= 0x2e80 && codePoint <= 0x303e) ||
    (codePoint >= 0x3041 && codePoint <= 0x33ff) ||
    (codePoint >= 0x3400 && codePoint <= 0x4dbf) ||
    (codePoint >= 0x4e00 && codePoint <= 0x9fff) ||
    (codePoint >= 0xa000 && codePoint <= 0xa4cf) ||
    (codePoint >= 0xa960 && codePoint <= 0xa97f) ||
    (codePoint >= 0xac00 && codePoint <= 0xd7a3) ||
    (codePoint >= 0xf900 && codePoint <= 0xfaff) ||
    (codePoint >= 0xfe10 && codePoint <= 0xfe19) ||
    (codePoint >= 0xfe30 && codePoint <= 0xfe6f) ||
    (codePoint >= 0xff00 && codePoint <= 0xff60) ||
    (codePoint >= 0xffe0 && codePoint <= 0xffe6) ||
    (codePoint >= 0x1f300 && codePoint <= 0x1f64f) ||
    (codePoint >= 0x1f900 && codePoint <= 0x1f9ff) ||
    (codePoint >= 0x20000 && codePoint <= 0x3fffd)
  ) {
    return 2;
  }
  return 1;
}

function __pi_sdk_visible_width(text) {
  const plain = __pi_sdk_strip_terminal_sequences(String(text)).replace(/\t/g, "   ");
  let width = 0;
  for (const ch of plain) width += __pi_sdk_code_point_width(ch.codePointAt(0));
  return width;
}

function __pi_sdk_truncate_to_width(text, maxWidth, ellipsis, pad) {
  const source = text === undefined || text === null ? "" : String(text);
  const limit = Math.max(0, Math.floor(Number(maxWidth) || 0));
  const marker = ellipsis === undefined ? "..." : String(ellipsis);
  let result = source;

  if (__pi_sdk_visible_width(source) > limit) {
    const markerWidth = __pi_sdk_visible_width(marker);
    if (markerWidth > limit) {
      result = "";
    } else {
      const budget = limit - markerWidth;
      let kept = "";
      let used = 0;
      for (const ch of source) {
        const charWidth = __pi_sdk_code_point_width(ch.codePointAt(0));
        if (used + charWidth > budget) break;
        kept += ch;
        used += charWidth;
      }
      result = kept + marker;
    }
  }

  if (pad) {
    const used = __pi_sdk_visible_width(result);
    if (used < limit) result += " ".repeat(limit - used);
  }
  return result;
}

function __pi_sdk_wrap_single_line(line, width) {
  if (__pi_sdk_visible_width(line) <= width) return [line];
  const out = [];
  const tokens = line.split(/(\s+)/).filter((token) => token.length > 0);
  let current = "";
  let currentWidth = 0;

  for (const token of tokens) {
    const tokenWidth = __pi_sdk_visible_width(token);
    if (tokenWidth > width && token.trim() !== "") {
      if (current !== "") {
        out.push(current.replace(/\s+$/, ""));
        current = "";
        currentWidth = 0;
      }
      let rest = token;
      while (__pi_sdk_visible_width(rest) > width) {
        let piece = "";
        let pieceWidth = 0;
        for (const ch of rest) {
          const charWidth = __pi_sdk_code_point_width(ch.codePointAt(0));
          if (pieceWidth + charWidth > width) break;
          piece += ch;
          pieceWidth += charWidth;
        }
        if (piece === "") break;
        out.push(piece);
        rest = rest.slice(piece.length);
      }
      current = rest;
      currentWidth = __pi_sdk_visible_width(rest);
      continue;
    }
    if (currentWidth > 0 && currentWidth + tokenWidth > width) {
      out.push(current.replace(/\s+$/, ""));
      if (token.trim() === "") {
        current = "";
        currentWidth = 0;
      } else {
        current = token;
        currentWidth = tokenWidth;
      }
    } else {
      current += token;
      currentWidth += tokenWidth;
    }
  }
  if (current !== "") out.push(current.replace(/\s+$/, ""));
  return out.length > 0 ? out : [""];
}

function __pi_sdk_wrap_text(text, width) {
  const limit = Number(width) > 0 ? Math.floor(Number(width)) : 1;
  if (text === undefined || text === null) return [""];
  const source = String(text);
  if (source === "") return [""];
  const out = [];
  for (const inputLine of source.split(/\r\n|\r|\n/)) {
    if (inputLine === "") {
      out.push("");
      continue;
    }
    for (const line of __pi_sdk_wrap_single_line(inputLine, limit)) out.push(line);
  }
  return out.length > 0 ? out : [""];
}

function __pi_sdk_hyperlink(text, url) {
  return "\x1b]8;;" + String(url) + "\x1b\\" + String(text) + "\x1b]8;;\x1b\\";
}

const __pi_sdk_cursor_marker = "\x1b_pi:c\x07";

// --- keys -----------------------------------------------------------------

const __pi_sdk_legacy_keys = {
  "\x1b": "escape",
  "\x1c": "ctrl+\\",
  "\x1d": "ctrl+]",
  "\x1f": "ctrl+-",
  "\x1b\x1b": "ctrl+alt+[",
  "\x1b\x1c": "ctrl+alt+\\",
  "\x1b\x1d": "ctrl+alt+]",
  "\x1b\x1f": "ctrl+alt+-",
  "\t": "tab",
  "\r": "enter",
  "\n": "enter",
  "\x1bOM": "enter",
  "\x00": "ctrl+space",
  " ": "space",
  "\x7f": "backspace",
  "\x08": "backspace",
  "\x1b[Z": "shift+tab",
  "\x1b\r": "alt+enter",
  "\x1b ": "alt+space",
  "\x1b\x7f": "alt+backspace",
  "\x1b\b": "alt+backspace",
  "\x1bB": "alt+left",
  "\x1bF": "alt+right",
  "\x1b[A": "up",
  "\x1b[B": "down",
  "\x1b[C": "right",
  "\x1b[D": "left",
  "\x1b[H": "home",
  "\x1bOH": "home",
  "\x1b[F": "end",
  "\x1bOF": "end",
  "\x1b[3~": "delete",
  "\x1b[5~": "pageUp",
  "\x1b[6~": "pageDown",
};

function __pi_sdk_parse_key(data) {
  if (typeof data !== "string") return undefined;
  if (Object.prototype.hasOwnProperty.call(__pi_sdk_legacy_keys, data)) return __pi_sdk_legacy_keys[data];
  if (data.length === 2 && data.charCodeAt(0) === 27) {
    const code = data.charCodeAt(1);
    if (code >= 1 && code <= 26) return "ctrl+alt+" + String.fromCharCode(code + 96);
    if ((code >= 97 && code <= 122) || (code >= 48 && code <= 57)) return "alt+" + data[1];
  }
  if (data.length === 1) {
    const code = data.charCodeAt(0);
    if (code >= 1 && code <= 26) return "ctrl+" + String.fromCharCode(code + 96);
    if (code >= 32 && code <= 126) return data;
  }
  return undefined;
}

function __pi_sdk_matches_key(data, keyId) {
  if (typeof keyId !== "string") return false;
  const parsed = __pi_sdk_parse_key(data);
  if (parsed === keyId) return true;
  if (keyId === "escape" && parsed === "esc") return true;
  if (keyId === "return" && parsed === "enter") return true;
  return false;
}

function __pi_sdk_is_event(data, flag) {
  if (typeof data !== "string") return false;
  // Bracketed paste must never be mistaken for a Kitty protocol event.
  if (data.indexOf("\x1b[200~") !== -1) return false;
  const suffixes = ["u", "~", "A", "B", "C", "D", "H", "F"].map((tail) => ":" + flag + tail);
  for (const suffix of suffixes) if (data.indexOf(suffix) !== -1) return true;
  return false;
}

function __pi_sdk_make_key_object() {
  const base = {};
  const names = [
    "escape", "enter", "return", "tab", "space", "backspace", "delete", "insert",
    "up", "down", "left", "right", "home", "end", "pageUp", "pageDown",
    "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12",
  ];
  for (const name of names) base[name] = name;
  const symbols = {
    leftbracket: "[", rightbracket: "]", backslash: "\\", semicolon: ";", quote: "'",
    comma: ",", period: ".", slash: "/", exclamation: "!", at: "@", hash: "#",
    dollar: "$", percent: "%", caret: "^", ampersand: "&", asterisk: "*",
    leftparen: "(", rightparen: ")", underscore: "_", plus: "+", pipe: "|",
    tilde: "~", leftbrace: "{", rightbrace: "}", colon: ":", lessthan: "<",
    greaterthan: ">", question: "?",
  };
  for (const name of Object.keys(symbols)) base[name] = symbols[name];
  const prefixes = [
    ["ctrl", "ctrl+"], ["shift", "shift+"], ["alt", "alt+"], ["super", "super+"],
    ["ctrlShift", "ctrl+shift+"], ["shiftCtrl", "shift+ctrl+"],
    ["ctrlAlt", "ctrl+alt+"], ["altCtrl", "alt+ctrl+"],
    ["shiftAlt", "shift+alt+"], ["altShift", "alt+shift+"],
    ["ctrlSuper", "ctrl+super+"], ["superCtrl", "super+ctrl+"],
    ["shiftSuper", "shift+super+"], ["superShift", "super+shift+"],
    ["altSuper", "alt+super+"], ["superAlt", "super+alt+"],
    ["ctrlShiftAlt", "ctrl+shift+alt+"], ["ctrlShiftSuper", "ctrl+shift+super+"],
  ];
  for (const pair of prefixes) base[pair[0]] = (key) => pair[1] + key;
  return base;
}

const __pi_sdk_Key = __pi_sdk_make_key_object();

// --- fuzzy matching -------------------------------------------------------

function __pi_sdk_fuzzy_match(query, text) {
  const queryLower = String(query).toLowerCase();
  const textLower = String(text).toLowerCase();

  function matchOne(normalized) {
    if (normalized.length === 0) return { matches: true, score: 0 };
    if (normalized.length > textLower.length) return { matches: false, score: 0 };
    let queryIndex = 0;
    let score = 0;
    let lastMatchIndex = -1;
    let consecutive = 0;
    for (let i = 0; i < textLower.length && queryIndex < normalized.length; i++) {
      if (textLower[i] !== normalized[queryIndex]) continue;
      const isWordBoundary = i === 0 || /[\s\-_./:]/.test(textLower[i - 1]);
      if (lastMatchIndex === i - 1) {
        consecutive += 1;
        score -= consecutive * 5;
      } else {
        consecutive = 0;
        if (lastMatchIndex >= 0) score += (i - lastMatchIndex - 1) * 2;
      }
      if (isWordBoundary) score -= 10;
      score += i * 0.1;
      lastMatchIndex = i;
      queryIndex += 1;
    }
    if (queryIndex < normalized.length) return { matches: false, score: 0 };
    if (normalized === textLower) score -= 100;
    return { matches: true, score };
  }

  const primary = matchOne(queryLower);
  if (primary.matches) return primary;
  const alphaNumeric = /^([a-z]+)([0-9]+)$/.exec(queryLower);
  const numericAlpha = /^([0-9]+)([a-z]+)$/.exec(queryLower);
  const swapped = alphaNumeric
    ? alphaNumeric[2] + alphaNumeric[1]
    : numericAlpha
      ? numericAlpha[2] + numericAlpha[1]
      : "";
  if (!swapped) return primary;
  const swappedMatch = matchOne(swapped);
  if (!swappedMatch.matches) return primary;
  return { matches: true, score: swappedMatch.score + 5 };
}

function __pi_sdk_fuzzy_filter(items, query, getText) {
  const list = Array.isArray(items) ? items : [];
  const raw = query === undefined || query === null ? "" : String(query);
  if (raw.trim() === "") return list;
  const tokens = raw.trim().split(/[\s/]+/).filter((token) => token.length > 0);
  if (tokens.length === 0) return list;
  const results = [];
  for (const item of list) {
    const text = String(getText(item));
    let total = 0;
    let allMatch = true;
    for (const token of tokens) {
      const match = __pi_sdk_fuzzy_match(token, text);
      if (!match.matches) {
        allMatch = false;
        break;
      }
      total += match.score;
    }
    if (allMatch) results.push({ item: item, score: total });
  }
  results.sort((a, b) => a.score - b.score);
  return results.map((entry) => entry.item);
}

// --- themes ---------------------------------------------------------------
//
// The extension host has no colour palette and no TTY, so every style helper is
// either an identity function or a plain cursor string. Passing a real theme
// object is still supported — the components only call the documented
// functions.

function __pi_sdk_identity(text) {
  return String(text);
}

function __pi_sdk_identity2(text) {
  return String(text);
}

function __pi_sdk_default_markdown_theme() {
  return {
    heading: __pi_sdk_identity,
    link: __pi_sdk_identity,
    linkUrl: __pi_sdk_identity,
    code: __pi_sdk_identity,
    codeBlock: __pi_sdk_identity,
    codeBlockBorder: __pi_sdk_identity,
    quote: __pi_sdk_identity,
    quoteBorder: __pi_sdk_identity,
    hr: __pi_sdk_identity,
    listBullet: __pi_sdk_identity,
    bold: __pi_sdk_identity,
    italic: __pi_sdk_identity,
    underline: __pi_sdk_identity,
    strikethrough: __pi_sdk_identity,
  };
}

function __pi_sdk_default_select_list_theme() {
  return {
    selectedPrefix: __pi_sdk_identity,
    selectedText: __pi_sdk_identity,
    description: __pi_sdk_identity,
    scrollInfo: __pi_sdk_identity,
    noMatch: __pi_sdk_identity,
  };
}

function __pi_sdk_default_settings_list_theme() {
  return {
    label: __pi_sdk_identity2,
    value: __pi_sdk_identity2,
    description: __pi_sdk_identity,
    cursor: "> ",
    hint: __pi_sdk_identity,
  };
}

function __pi_sdk_default_editor_theme() {
  return { borderColor: __pi_sdk_identity, selectList: __pi_sdk_default_select_list_theme() };
}

// --- pi-tui components ----------------------------------------------------

class Text {
  constructor(text, paddingX, paddingY, customBgFn) {
    this.text = text === undefined || text === null ? "" : String(text);
    this.paddingX = typeof paddingX === "number" ? paddingX : 1;
    this.paddingY = typeof paddingY === "number" ? paddingY : 1;
    this.customBgFn = customBgFn;
  }
  setText(text) {
    this.text = text === undefined || text === null ? "" : String(text);
    return this;
  }
  getText() {
    return this.text;
  }
  setCustomBgFn(fn) {
    this.customBgFn = fn;
    return this;
  }
  invalidate() {}
  render(width) {
    const totalWidth = Math.max(1, Math.floor(Number(width) || 1));
    if (!this.text || this.text.trim() === "") return [];
    const paddingX = Math.max(0, Math.min(Math.floor(this.paddingX), Math.floor((totalWidth - 1) / 2)));
    const contentWidth = Math.max(1, totalWidth - paddingX * 2);
    const left = " ".repeat(paddingX);
    const lines = __pi_sdk_wrap_text(this.text.replace(/\t/g, "   "), contentWidth).map((line) => {
      let rendered = left + line + left;
      if (typeof this.customBgFn === "function") rendered = this.customBgFn(rendered);
      return rendered + " ".repeat(Math.max(0, totalWidth - __pi_sdk_visible_width(rendered)));
    });
    const emptyLines = [];
    for (let i = 0; i < Math.max(0, Math.floor(this.paddingY)); i++) emptyLines.push(" ".repeat(totalWidth));
    const result = emptyLines.concat(lines, emptyLines);
    return result.length > 0 ? result : [""];
  }
}

class Spacer {
  constructor(height) {
    this.height = Math.max(0, Math.floor(typeof height === "number" ? height : 1));
  }
  invalidate() {}
  render(width) {
    const lines = [];
    for (let i = 0; i < this.height; i++) lines.push(" ".repeat(Math.max(0, Math.floor(Number(width) || 0))));
    return lines;
  }
}

class Box {
  constructor(paddingX, paddingY, bgFn) {
    this.paddingX = typeof paddingX === "number" ? paddingX : 1;
    this.paddingY = typeof paddingY === "number" ? paddingY : 1;
    this.bgFn = bgFn;
    this.children = [];
  }
  addChild(child) {
    this.children.push(child);
    return child;
  }
  removeChild(child) {
    const index = this.children.indexOf(child);
    if (index >= 0) this.children.splice(index, 1);
  }
  clear() {
    this.children.length = 0;
  }
  setBgFn(fn) {
    this.bgFn = fn;
    return this;
  }
  invalidate() {
    for (const child of this.children) if (child && typeof child.invalidate === "function") child.invalidate();
  }
  render(width) {
    const totalWidth = Math.max(1, Math.floor(Number(width) || 1));
    const paddingX = Math.max(0, Math.min(Math.floor(this.paddingX), Math.floor((totalWidth - 1) / 2)));
    const contentWidth = Math.max(1, totalWidth - paddingX * 2);
    const left = " ".repeat(paddingX);
    const content = [];
    for (const child of this.children) {
      const childLines = child && typeof child.render === "function" ? child.render(contentWidth) : [];
      for (const line of childLines) content.push(line);
    }
    const lines = content.map((line) => {
      let rendered = left + line + left;
      if (typeof this.bgFn === "function") rendered = this.bgFn(rendered);
      return rendered + " ".repeat(Math.max(0, totalWidth - __pi_sdk_visible_width(rendered)));
    });
    const emptyLines = [];
    for (let i = 0; i < Math.max(0, Math.floor(this.paddingY)); i++) emptyLines.push(" ".repeat(totalWidth));
    return emptyLines.concat(lines, emptyLines);
  }
}

class Container {
  constructor() {
    this.children = [];
  }
  addChild(child) {
    this.children.push(child);
    return child;
  }
  removeChild(child) {
    const index = this.children.indexOf(child);
    if (index >= 0) this.children.splice(index, 1);
  }
  clear() {
    this.children.length = 0;
  }
  invalidate() {
    for (const child of this.children) if (child && typeof child.invalidate === "function") child.invalidate();
  }
  render(width) {
    const lines = [];
    for (const child of this.children) {
      const childLines = child && typeof child.render === "function" ? child.render(width) : [];
      for (const line of childLines) lines.push(line);
    }
    return lines;
  }
}

class Markdown {
  constructor(text, paddingX, paddingY, theme, defaultTextStyle, options) {
    this.text = text === undefined || text === null ? "" : String(text);
    this.paddingX = typeof paddingX === "number" ? paddingX : 1;
    this.paddingY = typeof paddingY === "number" ? paddingY : 1;
    this.theme = theme;
    this.defaultTextStyle = defaultTextStyle;
    this.options = options || {};
  }
  setText(text) {
    this.text = text === undefined || text === null ? "" : String(text);
    return this;
  }
  getText() {
    return this.text;
  }
  invalidate() {}
  render(width) {
    const theme = this.theme || __pi_sdk_default_markdown_theme();
    const totalWidth = Math.max(1, Math.floor(Number(width) || 1));
    const paddingX = Math.max(0, Math.min(Math.floor(this.paddingX), Math.floor((totalWidth - 1) / 2)));
    const contentWidth = Math.max(1, totalWidth - paddingX * 2);
    const left = " ".repeat(paddingX);
    const out = [];
    let inCode = false;
    for (const sourceLine of this.text.split(/\r\n|\r|\n/)) {
      if (/^\s*```/.test(sourceLine)) {
        inCode = !inCode;
        out.push("");
        continue;
      }
      if (inCode) {
        for (const line of __pi_sdk_wrap_text(theme.codeBlock(sourceLine), contentWidth)) out.push(line);
        continue;
      }
      let text = sourceLine;
      let prefix = "";
      const heading = /^(#{1,6})\s+(.*)$/.exec(text);
      if (heading) {
        text = theme.heading(heading[2]);
      } else {
        const bullet = /^(\s*)([-*+])\s+(.*)$/.exec(text);
        const ordered = bullet ? null : /^(\s*)(\d+)[.)]\s+(.*)$/.exec(text);
        const quote = bullet || ordered ? null : /^(\s*)>\s?(.*)$/.exec(text);
        if (bullet) {
          prefix = bullet[1] + theme.listBullet("* ");
          text = bullet[3];
        } else if (ordered) {
          prefix = ordered[1] + ordered[2] + ". ";
          text = ordered[3];
        } else if (quote) {
          prefix = quote[1] + theme.quoteBorder("| ");
          text = theme.quote(quote[2]);
        }
      }
      text = text.replace(/\[([^\]]*)\]\(([^)]+)\)/g, (match, label, url) => theme.link(label) + " (" + theme.linkUrl(url) + ")");
      text = text.replace(/\*\*([^*]+)\*\*/g, (match, bold) => theme.bold(bold));
      text = text.replace(/(^|\s)\*([^*]+)\*/g, (match, lead, italic) => lead + theme.italic(italic));
      text = text.replace(/`([^`]+)`/g, (match, code) => theme.code(code));
      for (const line of __pi_sdk_wrap_text(prefix + text, contentWidth)) {
        out.push(left + line + " ".repeat(Math.max(0, totalWidth - __pi_sdk_visible_width(line) - paddingX)));
      }
    }
    const emptyLines = [];
    for (let i = 0; i < Math.max(0, Math.floor(this.paddingY)); i++) emptyLines.push(" ".repeat(totalWidth));
    return emptyLines.concat(out, emptyLines);
  }
}

class DynamicBorder {
  constructor(color) {
    this.color = typeof color === "function" ? color : __pi_sdk_identity;
  }
  invalidate() {}
  render(width) {
    return [this.color("-".repeat(Math.max(1, Math.floor(Number(width) || 1))))];
  }
}

class Input {
  constructor(options) {
    this.options = options || {};
    this.prompt = typeof this.options.prompt === "string" ? this.options.prompt : "> ";
    this.placeholder = typeof this.options.placeholder === "string" ? this.options.placeholder : "";
    this.value = typeof this.options.value === "string" ? this.options.value : "";
    this.onSubmit = typeof this.options.onSubmit === "function" ? this.options.onSubmit : undefined;
    this.onChange = typeof this.options.onChange === "function" ? this.options.onChange : undefined;
  }
  getValue() {
    return this.value;
  }
  setValue(value) {
    this.value = value === undefined || value === null ? "" : String(value);
    if (this.onChange) this.onChange(this.value);
    return this;
  }
  invalidate() {}
  handleInput(data) {
    if (typeof data !== "string" || data.length === 0) return;
    if (data === "\r" || data === "\n") {
      if (this.onSubmit) this.onSubmit(this.value);
      return;
    }
    if (data === "\x7f" || data === "\b") {
      this.setValue(this.value.slice(0, -1));
      return;
    }
    if (data.charCodeAt(0) === 27) return;
    this.setValue(this.value + data);
  }
  render(width) {
    const totalWidth = Math.max(1, Math.floor(Number(width) || 1));
    const body = this.value !== "" ? this.value : this.placeholder;
    return [__pi_sdk_truncate_to_width(this.prompt + body, totalWidth, "", true)];
  }
}

class Editor {
  constructor(tui, theme, options) {
    this.tui = tui;
    this.theme = theme || __pi_sdk_default_editor_theme();
    this.options = options || {};
    this.text = typeof this.options.initialText === "string" ? this.options.initialText : "";
    this.onSubmit = typeof this.options.onSubmit === "function" ? this.options.onSubmit : undefined;
    this.onChange = typeof this.options.onChange === "function" ? this.options.onChange : undefined;
  }
  getText() {
    return this.text;
  }
  setText(text) {
    this.text = text === undefined || text === null ? "" : String(text);
    if (this.onChange) this.onChange(this.text);
    return this;
  }
  getCursor() {
    return this.text.length;
  }
  invalidate() {}
  handleInput(data) {
    if (typeof data !== "string" || data.length === 0) return;
    if (data === "\r" || data === "\n") {
      if (this.onSubmit) this.onSubmit(this.text);
      return;
    }
    if (data === "\x7f" || data === "\b") {
      this.setText(this.text.slice(0, -1));
      return;
    }
    if (data.charCodeAt(0) === 27) return;
    this.setText(this.text + data);
  }
  render(width) {
    const totalWidth = Math.max(1, Math.floor(Number(width) || 1));
    return this.text.split(/\r\n|\r|\n/).map((line) => __pi_sdk_truncate_to_width(line, totalWidth, "", true));
  }
}

class CustomEditor extends Editor {
  constructor(tui, theme, keybindings, options) {
    super(tui, theme, options);
    this.keybindings = keybindings;
    this.actionHandlers = new Map();
    this.embedWorkingStatus = !!(options && options.embedWorkingStatus);
  }
  setWorkingStatusIndicator(indicator) {
    this.workingStatusIndicator = indicator;
  }
}

class Loader {
  constructor(tui, spinnerColor, messageColor, message) {
    this.tui = tui;
    this.spinnerColor = typeof spinnerColor === "function" ? spinnerColor : __pi_sdk_identity;
    this.messageColor = typeof messageColor === "function" ? messageColor : __pi_sdk_identity;
    this.message = message === undefined ? "" : String(message);
    this.frame = 0;
  }
  setMessage(message) {
    this.message = message === undefined ? "" : String(message);
  }
  invalidate() {}
  render(width) {
    const totalWidth = Math.max(1, Math.floor(Number(width) || 1));
    const spinner = ["-", "\\", "|", "/"][this.frame % 4];
    const line = this.spinnerColor(spinner) + " " + this.messageColor(this.message);
    return [__pi_sdk_truncate_to_width(line, totalWidth, "", true)];
  }
}

class BorderedLoader extends Container {
  constructor(tui, theme, message, options) {
    super();
    this.cancellable = !(options && options.cancellable === false);
    const borderColor = theme && typeof theme.fg === "function" ? (text) => theme.fg("border", text) : __pi_sdk_identity;
    this.abortController = typeof AbortController === "function" ? new AbortController() : null;
    this.addChild(new DynamicBorder(borderColor));
    this.addChild(new Loader(tui, __pi_sdk_identity, __pi_sdk_identity, message));
    if (this.cancellable) this.addChild(new Text("(cancel)", 1, 0));
    this.addChild(new Spacer(1));
    this.addChild(new DynamicBorder(borderColor));
  }
  get signal() {
    return this.abortController ? this.abortController.signal : undefined;
  }
  start() {
    return this;
  }
  stop() {
    if (this.abortController) this.abortController.abort();
  }
}

class SelectList {
  constructor(items, maxVisible, theme, layout) {
    this.items = Array.isArray(items) ? items : [];
    this.filteredItems = this.items;
    this.maxVisible = Math.max(1, Math.floor(typeof maxVisible === "number" ? maxVisible : 5));
    this.theme = theme || __pi_sdk_default_select_list_theme();
    this.layout = layout || {};
    this.selectedIndex = 0;
  }
  setFilter(filter) {
    const needle = filter === undefined || filter === null ? "" : String(filter).toLowerCase();
    this.filteredItems = this.items.filter((item) => {
      const label = item && item.label !== undefined ? String(item.label) : String(item && item.value);
      return needle === "" || label.toLowerCase().indexOf(needle) !== -1;
    });
    this.selectedIndex = 0;
  }
  setSelectedIndex(index) {
    const count = this.filteredItems.length;
    if (count === 0) {
      this.selectedIndex = 0;
      return;
    }
    this.selectedIndex = Math.max(0, Math.min(count - 1, Math.floor(Number(index) || 0)));
  }
  getSelectedItem() {
    return this.filteredItems[this.selectedIndex] || null;
  }
  invalidate() {}
  handleInput(data) {
    if (__pi_sdk_matches_key(data, "up") || data === "k") this.setSelectedIndex(this.selectedIndex - 1);
    else if (__pi_sdk_matches_key(data, "down") || data === "j") this.setSelectedIndex(this.selectedIndex + 1);
  }
  render(width) {
    const totalWidth = Math.max(1, Math.floor(Number(width) || 1));
    const theme = this.theme;
    const items = this.filteredItems;
    if (items.length === 0) return [theme.noMatch("No matches")];
    const maxVisible = Math.min(this.maxVisible, items.length);
    let start = 0;
    if (items.length > maxVisible) {
      start = Math.min(Math.max(0, this.selectedIndex - Math.floor(maxVisible / 2)), items.length - maxVisible);
    }
    const lines = [];
    for (let i = start; i < start + maxVisible; i++) {
      const item = items[i];
      const selected = i === this.selectedIndex;
      const label = item && item.label !== undefined ? String(item.label) : String(item && item.value);
      let line = (selected ? theme.selectedPrefix("> ") : "  ") + (selected ? theme.selectedText(label) : label);
      if (item && item.description) line += " " + theme.description(String(item.description));
      lines.push(__pi_sdk_truncate_to_width(line, totalWidth, "...", true));
    }
    if (items.length > maxVisible) lines.push(theme.scrollInfo("(" + (this.selectedIndex + 1) + "/" + items.length + ")"));
    return lines;
  }
}

class SettingsList {
  constructor(items, maxVisible, theme, onChange, onCancel, options) {
    this.items = Array.isArray(items) ? items : [];
    this.filteredItems = this.items;
    this.maxVisible = Math.max(1, Math.floor(typeof maxVisible === "number" ? maxVisible : 8));
    this.theme = theme || __pi_sdk_default_settings_list_theme();
    this.onChange = typeof onChange === "function" ? onChange : () => {};
    this.onCancel = typeof onCancel === "function" ? onCancel : () => {};
    this.options = options || {};
    this.selectedIndex = 0;
  }
  setSelectedIndex(index) {
    const count = this.filteredItems.length;
    this.selectedIndex = count === 0 ? 0 : Math.max(0, Math.min(count - 1, Math.floor(Number(index) || 0)));
  }
  getSelectedItem() {
    return this.filteredItems[this.selectedIndex] || null;
  }
  invalidate() {}
  handleInput(data) {
    if (__pi_sdk_matches_key(data, "up") || data === "k") {
      this.setSelectedIndex(this.selectedIndex - 1);
      return;
    }
    if (__pi_sdk_matches_key(data, "down") || data === "j") {
      this.setSelectedIndex(this.selectedIndex + 1);
      return;
    }
    if (__pi_sdk_matches_key(data, "escape")) {
      this.onCancel();
      return;
    }
    if (data === "\r" || data === " ") {
      const item = this.getSelectedItem();
      if (!item || !Array.isArray(item.values) || item.values.length === 0) return;
      const current = item.values.indexOf(item.currentValue);
      const next = item.values[(current + 1) % item.values.length];
      item.currentValue = next;
      this.onChange(item.id, next);
    }
  }
  render(width) {
    const totalWidth = Math.max(1, Math.floor(Number(width) || 1));
    const theme = this.theme;
    const items = this.filteredItems;
    if (items.length === 0) return [__pi_sdk_truncate_to_width("(no settings)", totalWidth, "...", true)];
    const maxVisible = Math.min(this.maxVisible, items.length);
    let start = 0;
    if (items.length > maxVisible) {
      start = Math.min(Math.max(0, this.selectedIndex - Math.floor(maxVisible / 2)), items.length - maxVisible);
    }
    const lines = [];
    for (let i = start; i < start + maxVisible; i++) {
      const item = items[i];
      const selected = i === this.selectedIndex;
      let line = (selected ? theme.cursor : "  ") + theme.label(String(item.label), selected);
      line += "  " + theme.value(item.currentValue === undefined ? "" : String(item.currentValue), selected);
      lines.push(__pi_sdk_truncate_to_width(line, totalWidth, "...", true));
      if (selected && item.description) {
        lines.push(__pi_sdk_truncate_to_width("  " + theme.description(String(item.description)), totalWidth, "...", true));
      }
    }
    if (items.length > maxVisible) lines.push(theme.hint("(" + (this.selectedIndex + 1) + "/" + items.length + ")"));
    return lines;
  }
}
// --- pi-coding-agent helpers ----------------------------------------------

function __pi_sdk_expand_home(input) {
  const value = String(input);
  if (value === "~") return __pi_os_module.homedir();
  if (value.indexOf("~/") === 0) return __pi_path_module.join(__pi_os_module.homedir(), value.slice(2));
  return value;
}

function __pi_sdk_get_agent_dir() {
  const env = __pi_process_module.env || {};
  const fromEnv = env.PI_CODING_AGENT_DIR;
  if (typeof fromEnv === "string" && fromEnv.length > 0) return __pi_sdk_expand_home(fromEnv);
  return __pi_path_module.join(__pi_os_module.homedir(), ".pi", "agent");
}

function __pi_sdk_format_size(bytes) {
  const value = Number(bytes) || 0;
  if (value < 1024) return value + "B";
  if (value < 1024 * 1024) return (value / 1024).toFixed(1) + "KB";
  return (value / (1024 * 1024)).toFixed(1) + "MB";
}

function __pi_sdk_utf8_byte_length(value) {
  let total = 0;
  for (const ch of String(value)) {
    const code = ch.codePointAt(0);
    if (code < 0x80) total += 1;
    else if (code < 0x800) total += 2;
    else if (code < 0x10000) total += 3;
    else total += 4;
  }
  return total;
}

function __pi_sdk_split_lines_for_counting(content) {
  if (content.length === 0) return [];
  const lines = content.split("\n");
  if (content.charAt(content.length - 1) === "\n") lines.pop();
  return lines;
}

// Mirrors `truncateHead` from packages/coding-agent/src/core/tools/truncate.ts:
// options object, whole-line output, and the full TruncationResult shape.
function __pi_sdk_truncate_head(content, options) {
  const source = content === undefined || content === null ? "" : String(content);
  const opts = options || {};
  const maxLines = typeof opts.maxLines === "number" ? opts.maxLines : 2000;
  const maxBytes = typeof opts.maxBytes === "number" ? opts.maxBytes : 50 * 1024;

  const totalBytes = __pi_sdk_utf8_byte_length(source);
  const sourceLines = __pi_sdk_split_lines_for_counting(source);
  const totalLines = sourceLines.length;

  if (totalLines <= maxLines && totalBytes <= maxBytes) {
    return {
      content: source,
      truncated: false,
      truncatedBy: null,
      totalLines: totalLines,
      totalBytes: totalBytes,
      outputLines: totalLines,
      outputBytes: totalBytes,
      lastLinePartial: false,
      firstLineExceedsLimit: false,
      maxLines: maxLines,
      maxBytes: maxBytes,
    };
  }

  if (__pi_sdk_utf8_byte_length(sourceLines[0]) > maxBytes) {
    return {
      content: "",
      truncated: true,
      truncatedBy: "bytes",
      totalLines: totalLines,
      totalBytes: totalBytes,
      outputLines: 0,
      outputBytes: 0,
      lastLinePartial: false,
      firstLineExceedsLimit: true,
      maxLines: maxLines,
      maxBytes: maxBytes,
    };
  }

  const kept = [];
  let outputBytes = 0;
  let truncatedBy = "lines";
  for (let i = 0; i < sourceLines.length && i < maxLines; i++) {
    const lineBytes = __pi_sdk_utf8_byte_length(sourceLines[i]) + (i > 0 ? 1 : 0);
    if (outputBytes + lineBytes > maxBytes) {
      truncatedBy = "bytes";
      break;
    }
    kept.push(sourceLines[i]);
    outputBytes += lineBytes;
  }
  if (kept.length >= maxLines && outputBytes <= maxBytes) truncatedBy = "lines";

  const outputContent = kept.join("\n");
  return {
    content: outputContent,
    truncated: true,
    truncatedBy: truncatedBy,
    totalLines: totalLines,
    totalBytes: totalBytes,
    outputLines: kept.length,
    outputBytes: __pi_sdk_utf8_byte_length(outputContent),
    lastLinePartial: false,
    firstLineExceedsLimit: false,
    maxLines: maxLines,
    maxBytes: maxBytes,
  };
}

function __pi_sdk_truncate_line(line, maxChars) {
  const source = line === undefined || line === null ? "" : String(line);
  const limit = typeof maxChars === "number" && maxChars > 0 ? Math.floor(maxChars) : 500;
  if (source.length <= limit) return { text: source, wasTruncated: false };
  return { text: source.slice(0, limit) + "... [truncated]", wasTruncated: true };
}

function __pi_sdk_parse_yaml_scalar(raw) {
  const value = raw.trim();
  if (value === "" || value === "~" || value === "null") return null;
  if (value === "true") return true;
  if (value === "false") return false;
  if (/^-?\d+$/.test(value)) return Number(value);
  if (/^-?\d+\.\d+$/.test(value)) return Number(value);
  const first = value.charAt(0);
  const last = value.charAt(value.length - 1);
  if (value.length >= 2 && ((first === '"' && last === '"') || (first === "'" && last === "'"))) return value.slice(1, -1);
  if (first === "[" && last === "]") {
    return value.slice(1, -1).split(",").map((entry) => __pi_sdk_parse_yaml_scalar(entry));
  }
  if (first === "{" && last === "}") {
    const object = {};
    for (const entry of value.slice(1, -1).split(",")) {
      const separator = entry.indexOf(":");
      if (separator === -1) continue;
      object[entry.slice(0, separator).trim()] = __pi_sdk_parse_yaml_scalar(entry.slice(separator + 1));
    }
    return object;
  }
  return value;
}

// Mirrors `parseFrontmatter` from packages/coding-agent/src/utils/frontmatter.ts.
// Upstream parses YAML with the `yaml` package; this shim parses a flat/nested
// subset (scalars, inline flow collections, one nesting level) and leaves
// anchors, block scalars and comments after values as strings — see
// docs/SDK_MODULES.md.
function __pi_sdk_strip_bom(value) {
  return value.charCodeAt(0) === 0xfeff ? value.slice(1) : value;
}

function __pi_sdk_parse_frontmatter(content) {
  const normalized = __pi_sdk_strip_bom(String(content === undefined || content === null ? "" : content))
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "\n");
  if (normalized.indexOf("---", 0) !== 0) return { frontmatter: {}, body: normalized };
  const endIndex = normalized.indexOf("\n---", 3);
  if (endIndex === -1) return { frontmatter: {}, body: normalized };
  const yamlString = normalized.slice(4, endIndex);
  const body = normalized.slice(endIndex + 4).trim();
  const frontmatter = {};
  let currentKey = null;
  for (const rawLine of yamlString.split("\n")) {
    if (rawLine.trim() === "" || rawLine.trim().indexOf("#") === 0) continue;
    const indent = rawLine.length - rawLine.replace(/^\s+/, "").length;
    const separator = rawLine.indexOf(":");
    if (separator === -1) continue;
    if (indent > 0 && currentKey !== null) {
      const nestedKey = rawLine.slice(0, separator).trim();
      if (!frontmatter[currentKey] || typeof frontmatter[currentKey] !== "object" || Array.isArray(frontmatter[currentKey])) {
        frontmatter[currentKey] = {};
      }
      frontmatter[currentKey][nestedKey] = __pi_sdk_parse_yaml_scalar(rawLine.slice(separator + 1));
      continue;
    }
    currentKey = rawLine.slice(0, separator).trim();
    const rawValue = rawLine.slice(separator + 1);
    frontmatter[currentKey] = rawValue.trim() === "" ? {} : __pi_sdk_parse_yaml_scalar(rawValue);
  }
  return { frontmatter: frontmatter, body: body };
}

function __pi_sdk_strip_frontmatter(content) {
  return __pi_sdk_parse_frontmatter(content).body;
}

const __pi_sdk_file_mutation_queues = new Map();

function __pi_sdk_with_file_mutation_queue(filePath, fn) {
  const key = __pi_path_module.resolve(__pi_sdk_expand_home(String(filePath)));
  const previous = __pi_sdk_file_mutation_queues.get(key) || Promise.resolve();
  const run = previous.then(
    () => fn(),
    () => fn(),
  );
  const settled = run.then(
    () => {},
    () => {},
  );
  __pi_sdk_file_mutation_queues.set(key, settled);
  settled.then(() => {
    if (__pi_sdk_file_mutation_queues.get(key) === settled) __pi_sdk_file_mutation_queues.delete(key);
  });
  return run;
}

const __pi_sdk_compaction_summary_prefix =
  "The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
const __pi_sdk_compaction_summary_suffix = "\n</summary>";
const __pi_sdk_branch_summary_prefix =
  "The following is a summary of a branch that this conversation came back from:\n\n<summary>\n";
const __pi_sdk_branch_summary_suffix = "</summary>";

function __pi_sdk_bash_execution_to_text(message) {
  let text = "Ran `" + message.command + "`\n";
  if (message.output) text += "```\n" + message.output + "\n```";
  else text += "(no output)";
  if (message.cancelled) text += "\n\n(command cancelled)";
  else if (message.exitCode !== null && message.exitCode !== undefined && message.exitCode !== 0) {
    text += "\n\nCommand exited with code " + message.exitCode;
  }
  if (message.truncated && message.fullOutputPath) text += "\n\n[Output truncated. Full output: " + message.fullOutputPath + "]";
  return text;
}

function __pi_sdk_convert_to_llm(messages) {
  const out = [];
  for (const message of Array.isArray(messages) ? messages : []) {
    if (!message || typeof message !== "object") continue;
    switch (message.role) {
      case "bashExecution":
        if (message.excludeFromContext) continue;
        out.push({
          role: "user",
          content: [{ type: "text", text: __pi_sdk_bash_execution_to_text(message) }],
          timestamp: message.timestamp,
        });
        break;
      case "custom":
        out.push({
          role: "user",
          content: typeof message.content === "string" ? [{ type: "text", text: message.content }] : message.content,
          timestamp: message.timestamp,
        });
        break;
      case "branchSummary":
        out.push({
          role: "user",
          content: [{ type: "text", text: __pi_sdk_branch_summary_prefix + message.summary + __pi_sdk_branch_summary_suffix }],
          timestamp: message.timestamp,
        });
        break;
      case "compactionSummary":
        out.push({
          role: "user",
          content: [{ type: "text", text: __pi_sdk_compaction_summary_prefix + message.summary + __pi_sdk_compaction_summary_suffix }],
          timestamp: message.timestamp,
        });
        break;
      case "user":
      case "assistant":
      case "toolResult":
        out.push(message);
        break;
      default:
        break;
    }
  }
  return out;
}

const __pi_sdk_tool_result_max_chars = 2000;

function __pi_sdk_content_text(content, separator) {
  const joiner = separator === undefined ? "\n" : String(separator);
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .filter((block) => block && block.type === "text")
    .map((block) => String(block.text))
    .join(joiner);
}

function __pi_sdk_serialize_conversation(messages) {
  const parts = [];
  for (const message of Array.isArray(messages) ? messages : []) {
    if (!message || typeof message !== "object") continue;
    if (message.role === "user") {
      const content = __pi_sdk_content_text(message.content, "");
      if (content) parts.push("[User]: " + content);
      continue;
    }
    if (message.role === "assistant") {
      const thinking = [];
      const toolCalls = [];
      const blocks = Array.isArray(message.content) ? message.content : [];
      for (const block of blocks) {
        if (!block) continue;
        if (block.type === "thinking") thinking.push(String(block.thinking));
        else if (block.type === "toolCall") {
          const args = block.arguments && typeof block.arguments === "object" ? block.arguments : {};
          const rendered = Object.keys(args)
            .map((key) => key + "=" + JSON.stringify(args[key]))
            .join(", ");
          toolCalls.push(String(block.name) + "(" + rendered + ")");
        }
      }
      if (thinking.length > 0) parts.push("[Assistant thinking]: " + thinking.join("\n"));
      if (blocks.some((block) => block && block.type === "text")) parts.push("[Assistant]: " + __pi_sdk_content_text(message.content));
      if (toolCalls.length > 0) parts.push("[Assistant tool calls]: " + toolCalls.join("; "));
      continue;
    }
    if (message.role === "toolResult") {
      const content = __pi_sdk_content_text(message.content, "");
      if (!content) continue;
      const truncated =
        content.length > __pi_sdk_tool_result_max_chars
          ? content.slice(0, __pi_sdk_tool_result_max_chars) +
            "\n\n[... " +
            (content.length - __pi_sdk_tool_result_max_chars) +
            " more characters truncated]"
          : content;
      parts.push("[Tool result]: " + truncated);
    }
  }
  return parts.join("\n\n");
}

function __pi_sdk_define_tool(tool) {
  return tool;
}

// --- built-in tool factories ----------------------------------------------
//
// `createReadTool` / `createWriteTool` / … wrap the host's built-in tool
// bundle so an extension can delegate to the same implementation the agent
// uses (upstream's `built-in-tool-renderer.ts` re-registers a built-in tool
// with custom rendering but the original `execute`).
//
// The host exposes two imports: `host_builtin_tool_definition(name)` is
// synchronous, so the factory can hand out the exact schema the Rust
// executor coerces arguments against, and `host_builtin_tool(name, argsJson,
// cwd)` is async and actually runs the tool. Without a runner the factory
// still returns an object (with a permissive schema) and `execute` rejects
// with `ERR_PI_BUILTIN_TOOL` instead of the import failing the whole
// extension.

function __pi_builtin_tool_definition(name) {
  if (typeof globalThis.host_builtin_tool_definition !== "function") return null;
  let envelope;
  try {
    envelope = JSON.parse(globalThis.host_builtin_tool_definition(name));
  } catch (_e) {
    return null;
  }
  if (envelope && envelope.ok === true && envelope.definition) return envelope.definition;
  return null;
}

function __pi_builtin_tool_error(message, details) {
  const error = new Error(message);
  error.code = "ERR_PI_BUILTIN_TOOL";
  if (details !== undefined) error.details = details;
  return error;
}

function __pi_make_builtin_tool(name, cwd) {
  const info = __pi_builtin_tool_definition(name);
  const resolvedCwd = cwd === undefined || cwd === null ? __pi_process_module.cwd() : String(cwd);
  return {
    name: info && typeof info.name === "string" ? info.name : name,
    label: info && typeof info.label === "string" ? info.label : name,
    description: info && typeof info.description === "string" ? info.description : name,
    parameters: info && info.parameters ? info.parameters : { type: "object" },
    /**
     * Run the built-in tool through the host.
     *
     * Two call conventions reach this function and both work:
     *
     *   1. upstream `AgentTool.execute(toolCallId, params, signal, onUpdate)`
     *      – a string first argument is the tool-call id;
     *   2. the shim's own registered-tool convention
     *      `execute(args, ctx)` – a non-string first argument is the params
     *      object.
     *
     * Resolves to `{ content, details, isError }`; `isError` is passed
     * through so a failed built-in surfaces as a structured tool result the
     * way a `pi.registerTool` tool does. Rejects only when the host bridge
     * itself failed (no runner, unknown name, malformed reply).
     */
    execute: function (a, b) {
      const params = typeof a === "string" ? b : a;
      const args = params && typeof params === "object" ? params : {};
      let raw;
      try {
        if (typeof globalThis.host_builtin_tool !== "function") {
          return Promise.reject(
            __pi_builtin_tool_error(
              'built-in tool "' + name + '" is not available: the extension host exposes no host_builtin_tool bridge',
            ),
          );
        }
        raw = globalThis.host_builtin_tool(name, JSON.stringify(args), resolvedCwd);
      } catch (e) {
        return Promise.reject(e);
      }
      return Promise.resolve(raw).then(function (json) {
        let envelope;
        try {
          envelope = JSON.parse(json);
        } catch (e) {
          throw __pi_builtin_tool_error(
            'built-in tool "' + name + '" returned an unparsable host reply',
          );
        }
        if (!envelope || envelope.ok !== true) {
          throw __pi_builtin_tool_error(
            envelope && typeof envelope.error === "string"
              ? envelope.error
              : 'built-in tool "' + name + '" failed in the host',
          );
        }
        return {
          content: Array.isArray(envelope.content) ? envelope.content : [],
          details: envelope.details === undefined ? null : envelope.details,
          isError: envelope.isError === true,
        };
      });
    },
  };
}

function __pi_sdk_create_builtin_tool(name) {
  return function (cwd, _options) {
    // Upstream `createBashTool(cwd, { spawnHook })` takes a second options
    // argument; the hook has no host equivalent, so it is accepted and
    // ignored (documented in docs/SDK_MODULES.md).
    return __pi_make_builtin_tool(name, cwd);
  };
}

const __pi_sdk_create_read_tool = __pi_sdk_create_builtin_tool("read");
const __pi_sdk_create_write_tool = __pi_sdk_create_builtin_tool("write");
const __pi_sdk_create_edit_tool = __pi_sdk_create_builtin_tool("edit");
const __pi_sdk_create_bash_tool = __pi_sdk_create_builtin_tool("bash");
const __pi_sdk_create_find_tool = __pi_sdk_create_builtin_tool("find");
const __pi_sdk_create_grep_tool = __pi_sdk_create_builtin_tool("grep");
const __pi_sdk_create_ls_tool = __pi_sdk_create_builtin_tool("ls");

// --- pi-ai helpers --------------------------------------------------------

function __pi_sdk_string_enum(values, options) {
  const schema = { type: "string", enum: Array.isArray(values) ? values.slice() : [] };
  const opts = options || {};
  if (opts.description !== undefined) schema.description = opts.description;
  if (opts.default !== undefined) schema.default = opts.default;
  return __pi_typebox_module.Type.Unsafe(schema);
}

const __pi_sdk_uuid_v7_max_timestamp = 0xffffffffffff;
const __pi_sdk_uuid_v7_max_sequence = (1n << 42n) - 1n;
let __pi_sdk_uuid_v7_last_timestamp = 0;
let __pi_sdk_uuid_v7_sequence;

function __pi_sdk_uuid_v7(timestampMs) {
  const requested = timestampMs === undefined ? Date.now() : Number(timestampMs);
  if (!Number.isInteger(requested) || requested < 0 || requested > __pi_sdk_uuid_v7_max_timestamp) {
    throw new RangeError("UUIDv7 timestamp must be an integer between 0 and " + __pi_sdk_uuid_v7_max_timestamp);
  }
  const effective = timestampMs === undefined ? Math.max(requested, __pi_sdk_uuid_v7_last_timestamp) : requested;
  if (timestampMs === undefined) __pi_sdk_uuid_v7_last_timestamp = effective;

  const bytes = __pi_crypto_module.randomBytes(16);
  if (__pi_sdk_uuid_v7_sequence === undefined) {
    __pi_sdk_uuid_v7_sequence =
      (BigInt(bytes[1]) << 32n) |
      (BigInt(bytes[2]) << 24n) |
      (BigInt(bytes[3]) << 16n) |
      (BigInt(bytes[4]) << 8n) |
      BigInt(bytes[5]);
  } else {
    if (__pi_sdk_uuid_v7_sequence === __pi_sdk_uuid_v7_max_sequence) {
      throw new RangeError("UUIDv7 generator sequence exhausted");
    }
    __pi_sdk_uuid_v7_sequence += 1n;
  }
  const sequence = __pi_sdk_uuid_v7_sequence;

  const timestamp = BigInt(effective);
  for (let index = 5; index >= 0; index--) {
    bytes[index] = Number((timestamp >> BigInt((5 - index) * 8)) & 0xffn);
  }
  bytes[6] = 0x70 | Number((sequence >> 37n) & 0x0fn);
  bytes[7] = Number((sequence >> 29n) & 0xffn);
  bytes[8] = 0x80 | Number((sequence >> 23n) & 0x3fn);
  bytes[9] = Number((sequence >> 15n) & 0xffn);
  bytes[10] = Number((sequence >> 7n) & 0xffn);
  bytes[11] = Number((sequence & 0x7fn) << 1n) | (bytes[11] & 0x01);

  const hex = [];
  for (let i = 0; i < 16; i++) hex.push((bytes[i] & 0xff).toString(16).padStart(2, "0"));
  return (
    hex.slice(0, 4).join("") +
    "-" +
    hex.slice(4, 6).join("") +
    "-" +
    hex.slice(6, 8).join("") +
    "-" +
    hex.slice(8, 10).join("") +
    "-" +
    hex.slice(10).join("")
  );
}

function __pi_sdk_calculate_cost(model, usage) {
  if (!model || !model.cost || !usage || !usage.cost) {
    throw new Error("calculateCost(model, usage) requires model.cost rates and a usage.cost object");
  }
  const rates = model.cost;
  const inputTokens = (usage.input || 0) + (usage.cacheRead || 0) + (usage.cacheWrite || 0);
  let matched = rates;
  let matchedThreshold = -1;
  for (const tier of rates.tiers || []) {
    if (inputTokens > tier.inputTokensAbove && tier.inputTokensAbove > matchedThreshold) {
      matched = tier;
      matchedThreshold = tier.inputTokensAbove;
    }
  }
  const longWrite = usage.cacheWrite1h || 0;
  const shortWrite = (usage.cacheWrite || 0) - longWrite;
  usage.cost.input = ((matched.input || 0) / 1000000) * (usage.input || 0);
  usage.cost.output = ((matched.output || 0) / 1000000) * (usage.output || 0);
  usage.cost.cacheRead = ((matched.cacheRead || 0) / 1000000) * (usage.cacheRead || 0);
  usage.cost.cacheWrite = ((matched.cacheWrite || 0) * shortWrite + (matched.input || 0) * 2 * longWrite) / 1000000;
  usage.cost.total = usage.cost.input + usage.cost.output + usage.cost.cacheRead + usage.cost.cacheWrite;
  return usage.cost;
}

// --- pi-ai event stream (utils/event-stream.ts) ---------------------------

// Upstream `packages/ai/src/utils/event-stream.ts` is pure JS: a FIFO queue
// behind an `AsyncIterable`, plus a promise the terminal event resolves. It
// needs no host bridge — the earlier "streaming needs a model-streaming
// bridge" gap note was wrong for this class — so it is ported verbatim here.
// The async iterator is hand-rolled (like `node:readline`) instead of an
// `async function*`, matching how the rest of the shim targets the engine.
class __pi_sdk_fifo_queue {
  constructor() {
    this.__incoming = [];
    this.__outgoing = [];
  }

  get length() {
    return this.__incoming.length + this.__outgoing.length;
  }

  enqueue(value) {
    this.__incoming.push(value);
  }

  dequeue() {
    if (this.__outgoing.length === 0) {
      while (this.__incoming.length > 0) {
        this.__outgoing.push(this.__incoming.pop());
      }
    }
    return this.__outgoing.pop();
  }
}

class EventStream {
  constructor(isComplete, extractResult) {
    this.__queue = new __pi_sdk_fifo_queue();
    this.__waiting = new __pi_sdk_fifo_queue();
    this.__done = false;
    this.__isComplete = isComplete;
    this.__extractResult = extractResult;
    this.__finalResultPromise = new Promise((resolve) => {
      this.__resolveFinalResult = resolve;
    });
  }

  push(event) {
    if (this.__done) return;

    if (this.__isComplete(event)) {
      this.__done = true;
      this.__resolveFinalResult(this.__extractResult(event));
    }

    // Deliver to a waiting consumer, or queue it.
    const waiter = this.__waiting.dequeue();
    if (waiter) {
      waiter({ value: event, done: false });
    } else {
      this.__queue.enqueue(event);
    }
  }

  end(result) {
    this.__done = true;
    if (result !== undefined) {
      this.__resolveFinalResult(result);
    }
    while (this.__waiting.length > 0) {
      const waiter = this.__waiting.dequeue();
      waiter({ value: undefined, done: true });
    }
  }

  [Symbol.asyncIterator]() {
    const self = this;
    return {
      next: () => self.__next(),
      return: () => Promise.resolve({ done: true, value: undefined }),
      [Symbol.asyncIterator]() {
        return this;
      },
    };
  }

  __next() {
    if (this.__queue.length > 0) {
      return Promise.resolve({ value: this.__queue.dequeue(), done: false });
    }
    if (this.__done) {
      return Promise.resolve({ done: true, value: undefined });
    }
    return new Promise((resolve) => this.__waiting.enqueue(resolve));
  }

  result() {
    return this.__finalResultPromise;
  }
}

class AssistantMessageEventStream extends EventStream {
  constructor() {
    super(
      (event) => event.type === "done" || event.type === "error",
      (event) => {
        if (event.type === "done") return event.message;
        if (event.type === "error") return event.error;
        throw new Error("Unexpected event type for final result");
      },
    );
  }
}

function __pi_sdk_create_assistant_message_event_stream() {
  return new AssistantMessageEventStream();
}

// --- pi-ai/compat provider registry (compat.ts) ---------------------------

// Upstream `compat.ts` keeps a module-level `Map<Api, RegisteredApiProvider>`
// and exposes `registerApiProvider` / `stream` / `streamSimple` so an
// extension can plug its own streaming implementation in — that is exactly
// what `packages/coding-agent/examples/extensions/custom-provider-*` does.
// The registry itself is pure JS. The *builtin* provider factories
// (`anthropicMessagesApi` / `openAIResponsesApi` / `openAICompletionsApi` /
// `googleGenerativeAIApi` / `azureOpenAIResponsesApi`) are driven over the
// host streaming bridge below, so they no longer need to stay gaps.
const __pi_sdk_api_providers = new Map();

function __pi_sdk_wrap_api_stream(api, stream) {
  return (model, context, options) => {
    if (!model || model.api !== api) {
      throw new Error("Mismatched api: " + (model && model.api) + " expected " + api);
    }
    return stream(model, context, options);
  };
}

function __pi_sdk_wrap_api_stream_simple(api, streamSimple) {
  return (model, context, options) => {
    if (!model || model.api !== api) {
      throw new Error("Mismatched api: " + (model && model.api) + " expected " + api);
    }
    return streamSimple(model, context, options);
  };
}

function __pi_sdk_register_api_provider(provider, sourceId) {
  if (!provider || typeof provider !== "object") {
    throw new TypeError("registerApiProvider: provider must be an object");
  }
  __pi_sdk_api_providers.set(provider.api, {
    provider: {
      api: provider.api,
      stream: __pi_sdk_wrap_api_stream(provider.api, provider.stream),
      streamSimple: __pi_sdk_wrap_api_stream_simple(provider.api, provider.streamSimple),
    },
    sourceId: sourceId,
  });
}

function __pi_sdk_get_api_provider(api) {
  const entry = __pi_sdk_api_providers.get(api);
  return entry ? entry.provider : undefined;
}

function __pi_sdk_get_api_providers() {
  const providers = [];
  for (const entry of __pi_sdk_api_providers.values()) providers.push(entry.provider);
  return providers;
}

function __pi_sdk_unregister_api_providers(sourceId) {
  for (const entry of Array.from(__pi_sdk_api_providers.entries())) {
    if (entry[1].sourceId === sourceId) __pi_sdk_api_providers.delete(entry[0]);
  }
}

function __pi_sdk_resolve_api_provider(api) {
  const provider = __pi_sdk_get_api_provider(api);
  if (!provider) {
    throw new Error("No API provider registered for api: " + api);
  }
  return provider;
}

function __pi_sdk_compat_stream(model, context, options) {
  return __pi_sdk_resolve_api_provider(model.api).stream(model, context, options);
}

function __pi_sdk_compat_stream_simple(model, context, options) {
  return __pi_sdk_resolve_api_provider(model.api).streamSimple(model, context, options);
}

function __pi_sdk_compat_complete(model, context, options) {
  return __pi_sdk_compat_stream(model, context, options).result();
}

function __pi_sdk_compat_complete_simple(model, context, options) {
  return __pi_sdk_compat_stream_simple(model, context, options).result();
}

// --- pi-ai/compat built-in provider factories (LUM-1180) ------------------

// Upstream `anthropicMessagesApi()` / `openAIResponsesApi()` return a
// `ProviderStreams` whose `stream` / `streamSimple` lazily import the concrete
// provider module (`api/*.lazy.ts` → `lazyApi`). The shim cannot import that
// TypeScript module, so the *host* runs the provider instead:
// `host_pi_ai_stream_start` opens one stream, `host_pi_ai_stream_next` yields
// one upstream-shaped `AssistantMessageEvent` at a time and
// `host_pi_ai_stream_cancel` aborts it. The events land in the pure-JS
// `AssistantMessageEventStream` above, so `for await` and `result()` behave
// exactly like upstream — setup failures and transport errors terminate the
// stream with an `error` event instead of throwing.
//
// The built-in apis whose Rust adapter the host can run: upstream's
// `azureOpenAIResponsesApi` / `googleGenerativeAIApi` / `openAICompletionsApi`
// join `anthropicMessagesApi` / `openAIResponsesApi` here (LUM-1204). The
// remaining five upstream lazy factories stay documented gaps — no Rust
// adapter the bridge can reach, except `mistral-conversations`, whose adapter
// exists but is not routed yet (see `__pi_sdk_stream_gaps` below).
const __pi_sdk_builtin_api_apis = [
  "anthropic-messages",
  "openai-responses",
  "openai-completions",
  "google-generative-ai",
  "azure-openai-responses",
];

// Mirrors upstream `builtinApiProviderInstances`: the registry entry each
// builtin api owns, so a later re-register can be told apart.
const __pi_sdk_builtin_instances = new Map();

function __pi_sdk_builtin_empty_message(model) {
  return {
    role: "assistant",
    content: [],
    api: model && model.api !== undefined ? model.api : "unknown",
    provider: model && model.provider !== undefined ? model.provider : "unknown",
    model: model && model.id !== undefined ? model.id : "unknown",
    usage: {
      input: 0,
      output: 0,
      cacheRead: 0,
      cacheWrite: 0,
      totalTokens: 0,
      cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
    },
    stopReason: "pending",
    timestamp: Date.now(),
  };
}

// Upstream `createSetupErrorMessage` (api/lazy.ts): the `error` field of an
// `error` event is a whole `AssistantMessage`, never a bare string.
function __pi_sdk_builtin_error_message(model, message, stopReason) {
  const output = __pi_sdk_builtin_empty_message(model);
  output.stopReason = stopReason || "error";
  output.errorMessage = message;
  return output;
}

// Best-effort partial for an abort: upstream keeps accumulating blocks and
// flips the stop reason at the end. The host sends `partial` with every event,
// so the last one seen is the closest equivalent.
function __pi_sdk_builtin_partial(model, lastEvent, stopReason, errorMessage) {
  const source =
    lastEvent && lastEvent.partial && typeof lastEvent.partial === "object"
      ? lastEvent.partial
      : __pi_sdk_builtin_empty_message(model);
  const output = Object.assign({}, source);
  if (Array.isArray(source.content)) output.content = source.content.slice();
  output.stopReason = stopReason;
  output.errorMessage = errorMessage;
  return output;
}

// Strip what JSON cannot carry: the `AbortSignal` travels out of band (the
// shim owns the cancel call) and callbacks stay on the JS side.
function __pi_sdk_builtin_serialize_options(options) {
  const out = {};
  if (!options || typeof options !== "object") return out;
  for (const key of Object.keys(options)) {
    if (key === "signal") continue;
    const value = options[key];
    if (value === undefined || typeof value === "function") continue;
    try {
      JSON.stringify(value);
    } catch (_error) {
      continue;
    }
    out[key] = value;
  }
  return out;
}

function __pi_sdk_builtin_envelope(raw, stage) {
  try {
    return typeof raw === "string" ? JSON.parse(raw) : raw;
  } catch (_error) {
    return { ok: false, error: "pi extension host returned malformed JSON for pi-ai stream " + stage };
  }
}

async function __pi_sdk_pump_builtin_api_stream(api, model, context, options, stream, state) {
  let hostId = null;
  try {
    const started = __pi_sdk_builtin_envelope(
      await globalThis.host_pi_ai_stream_start(JSON.stringify({ api: api, model: model, context: context, options: options })),
      "start",
    );
    if (!started || started.ok !== true) {
      stream.push({
        type: "error",
        reason: "error",
        error: __pi_sdk_builtin_error_message(
          model,
          (started && started.error) || "the pi-ai provider stream failed to start",
        ),
      });
      return;
    }
    hostId = started.id;
    state.hostId = hostId;
    if (state.cancelled) return;
    for (;;) {
      const next = __pi_sdk_builtin_envelope(await globalThis.host_pi_ai_stream_next(hostId), "next");
      if (!next || next.ok !== true) {
        if (!state.cancelled) {
          stream.push({
            type: "error",
            reason: "error",
            error: __pi_sdk_builtin_error_message(
              model,
              (next && next.error) || "the pi-ai provider stream failed",
            ),
          });
        }
        return;
      }
      if (next.done) return;
      state.lastEvent = next.event;
      stream.push(next.event);
      if (state.cancelled) return;
      if (next.event && (next.event.type === "done" || next.event.type === "error")) return;
    }
  } catch (error) {
    if (!state.cancelled) {
      stream.push({
        type: "error",
        reason: "error",
        error: __pi_sdk_builtin_error_message(model, error && error.message ? error.message : String(error)),
      });
    }
  } finally {
    if (state.signal && typeof state.signal.removeEventListener === "function" && state.onAbort) {
      state.signal.removeEventListener("abort", state.onAbort);
    }
    // Releases the host-side stream (and its provider connection) even when
    // the consumer stops iterating early or the stream completed.
    if (hostId !== null && typeof globalThis.host_pi_ai_stream_cancel === "function") {
      globalThis.host_pi_ai_stream_cancel(hostId);
    }
    stream.end();
  }
}

// Synchronous `AssistantMessageEventStream`, async work behind it — the same
// contract as upstream `lazyStream`.
function __pi_sdk_builtin_api_stream(api, model, context, options) {
  const stream = new AssistantMessageEventStream();
  const opts = options && typeof options === "object" ? options : {};
  const signal = opts.signal && typeof opts.signal === "object" ? opts.signal : undefined;
  const state = { cancelled: false, hostId: null, lastEvent: null, signal: signal, onAbort: null };

  if (typeof globalThis.host_pi_ai_stream_start !== "function") {
    stream.push({
      type: "error",
      reason: "error",
      error: __pi_sdk_builtin_error_message(model, "pi-ai provider streaming is not available in this host build"),
    });
    stream.end();
    return stream;
  }

  if (signal && typeof signal.addEventListener === "function") {
    state.onAbort = () => {
      if (state.cancelled) return;
      state.cancelled = true;
      if (state.hostId !== null && typeof globalThis.host_pi_ai_stream_cancel === "function") {
        globalThis.host_pi_ai_stream_cancel(state.hostId);
      }
      stream.push({
        type: "error",
        reason: "aborted",
        error: __pi_sdk_builtin_partial(model, state.lastEvent, "aborted", "Request was aborted"),
      });
      stream.end();
    };
    signal.addEventListener("abort", state.onAbort);
  }

  if (signal && signal.aborted) {
    if (state.onAbort) {
      state.onAbort();
    } else {
      stream.push({
        type: "error",
        reason: "aborted",
        error: __pi_sdk_builtin_partial(model, null, "aborted", "Request was aborted"),
      });
      stream.end();
    }
    return stream;
  }

  __pi_sdk_pump_builtin_api_stream(
    api,
    model,
    context,
    __pi_sdk_builtin_serialize_options(opts),
    stream,
    state,
  );
  return stream;
}

// `ProviderStreams` for one builtin api. `stream` / `streamSimple` take the
// same host path: the Rust providers consume the simple option set directly.
function __pi_sdk_builtin_provider_streams(api) {
  return {
    stream: (model, context, options) => __pi_sdk_builtin_api_stream(api, model, context, options),
    streamSimple: (model, context, options) => __pi_sdk_builtin_api_stream(api, model, context, options),
  };
}

function __pi_sdk_anthropic_messages_api() {
  return __pi_sdk_builtin_provider_streams("anthropic-messages");
}

function __pi_sdk_openai_responses_api() {
  return __pi_sdk_builtin_provider_streams("openai-responses");
}

function __pi_sdk_openai_completions_api() {
  return __pi_sdk_builtin_provider_streams("openai-completions");
}

function __pi_sdk_google_generative_ai_api() {
  return __pi_sdk_builtin_provider_streams("google-generative-ai");
}

// Azure is the deployment-scoped dialect of the Responses API: it is a
// separate `api` id (and therefore a separate provider registry entry), but
// it travels the same host bridge. The host's Azure adapter owns the
// deployment-name / api-version resolution.
function __pi_sdk_azure_openai_responses_api() {
  return __pi_sdk_builtin_provider_streams("azure-openai-responses");
}

// Upstream `registerBuiltInApiProviders`: register without clobbering an
// existing entry, because compat can load after a test or extension already
// registered an override for a builtin api id.
function __pi_sdk_register_built_in_api_providers() {
  for (const api of __pi_sdk_builtin_api_apis) {
    if (!__pi_sdk_get_api_provider(api)) {
      const streams = __pi_sdk_builtin_provider_streams(api);
      __pi_sdk_register_api_provider({ api: api, stream: streams.stream, streamSimple: streams.streamSimple });
    }
    __pi_sdk_builtin_instances.set(api, __pi_sdk_get_api_provider(api));
  }
}

// Upstream `resetApiProviders`: drop *everything* (extension overrides
// included) and re-register the builtins, exactly like `clearApiProviders()`
// followed by `registerBuiltInApiProviders()`.
function __pi_sdk_reset_api_providers() {
  __pi_sdk_api_providers.clear();
  __pi_sdk_builtin_instances.clear();
  __pi_sdk_register_built_in_api_providers();
}

// Upstream calls this at module init, so the builtins are available to
// `stream` / `complete` even when an extension imports nothing but those.
__pi_sdk_register_built_in_api_providers();

// --- module plumbing ------------------------------------------------------

const __pi_sdk_manifest_data = {};

function __pi_sdk_gap_error(specifier, name, reason) {
  const error = new Error(
    'Import "' + name + '" from "' + specifier + '" is not implemented by the pi extension host: ' + reason + ". See docs/SDK_MODULES.md.",
  );
  error.code = "ERR_PI_SDK_UNIMPLEMENTED";
  error.specifier = specifier;
  error.exportName = name;
  return error;
}

function __pi_sdk_unknown_export_error(specifier, name) {
  const error = new Error('Module "' + specifier + '" has no export "' + name + '". See docs/SDK_MODULES.md.');
  error.code = "ERR_PI_SDK_UNKNOWN_EXPORT";
  error.specifier = specifier;
  error.exportName = name;
  return error;
}

// Protocol / JS-internal property names the engine and bundlers probe on module
// namespaces. Returning `undefined` for these keeps `await import()`,
// `JSON.stringify`, interop and object inspection working instead of throwing a
// bogus "no export `then`" error.
const __pi_sdk_protocol_names = {
  then: true,
  catch: true,
  finally: true,
  toJSON: true,
  toString: true,
  valueOf: true,
  inspect: true,
  constructor: true,
  prototype: true,
  hasOwnProperty: true,
  __esModule: true,
  $$typeof: true,
};

function __pi_sdk_module(specifier, implemented, gaps) {
  const moduleGaps = gaps || {};
  const target = {};
  for (const name of Object.keys(implemented)) target[name] = implemented[name];

  const handlers = {
    get: function (inner, key, receiver) {
      if (typeof key === "symbol") {
        if (typeof Symbol !== "undefined" && key === Symbol.toStringTag) return "Module";
        return Reflect.get(inner, key, receiver);
      }
      if (Object.prototype.hasOwnProperty.call(inner, key)) return inner[key];
      if (Object.prototype.hasOwnProperty.call(moduleGaps, key)) throw __pi_sdk_gap_error(specifier, key, moduleGaps[key]);
      if (Object.prototype.hasOwnProperty.call(__pi_sdk_protocol_names, key)) return undefined;
      throw __pi_sdk_unknown_export_error(specifier, key);
    },
    // `has` / `ownKeys` / `getOwnPropertyDescriptor` report the implemented
    // exports only, so `Object.keys(mod)` and `"name" in mod` agree with the
    // target object. Documented gaps stay readable (and throw a named error),
    // but they are not reported as properties: they are not exports the shim
    // can hand over.
    has: function (inner, key) {
      return Object.prototype.hasOwnProperty.call(inner, key);
    },
    ownKeys: function (inner) {
      return Reflect.ownKeys(inner);
    },
    getOwnPropertyDescriptor: function (inner, key) {
      return Reflect.getOwnPropertyDescriptor(inner, key);
    },
  };

  const proxy = new Proxy(target, handlers);
  target.default = proxy;
  __pi_sdk_manifest_data[specifier] = {
    implemented: Object.keys(implemented).sort(),
    unimplemented: Object.keys(moduleGaps).sort(),
  };
  return proxy;
}

// An alias shares the canonical module object and its manifest entry.
function __pi_sdk_alias(canonical, alias, proxy) {
  __pi_sdk_manifest_data[alias] = __pi_sdk_manifest_data[canonical];
  return proxy;
}

const __pi_sdk_pi_tui = __pi_sdk_module("@earendil-works/pi-tui", {
  Box: Box,
  CURSOR_MARKER: __pi_sdk_cursor_marker,
  Container: Container,
  CustomEditor: CustomEditor,
  Editor: Editor,
  Input: Input,
  Key: __pi_sdk_Key,
  Loader: Loader,
  Markdown: Markdown,
  SelectList: SelectList,
  SettingsList: SettingsList,
  Spacer: Spacer,
  Text: Text,
  fuzzyFilter: __pi_sdk_fuzzy_filter,
  fuzzyMatch: __pi_sdk_fuzzy_match,
  getMarkdownTheme: __pi_sdk_default_markdown_theme,
  getSelectListTheme: __pi_sdk_default_select_list_theme,
  getSettingsListTheme: __pi_sdk_default_settings_list_theme,
  hyperlink: __pi_sdk_hyperlink,
  isKeyRelease: function (data) {
    return __pi_sdk_is_event(data, "3");
  },
  isKeyRepeat: function (data) {
    return __pi_sdk_is_event(data, "2");
  },
  matchesKey: __pi_sdk_matches_key,
  parseKey: __pi_sdk_parse_key,
  stripTerminalSequences: __pi_sdk_strip_terminal_sequences,
  truncateToWidth: __pi_sdk_truncate_to_width,
  visibleWidth: __pi_sdk_visible_width,
  wrapTextWithAnsi: __pi_sdk_wrap_text,
});

const __pi_sdk_pi_coding_agent = __pi_sdk_module(
  "@earendil-works/pi-coding-agent",
  {
    BorderedLoader: BorderedLoader,
    CONFIG_DIR_NAME: ".pi",
    CustomEditor: CustomEditor,
    DEFAULT_MAX_BYTES: 50 * 1024,
    DEFAULT_MAX_LINES: 2000,
    DynamicBorder: DynamicBorder,
    VERSION: "0.85.1-pi-rust",
    convertToLlm: __pi_sdk_convert_to_llm,
    createBashTool: __pi_sdk_create_bash_tool,
    createEditTool: __pi_sdk_create_edit_tool,
    createFindTool: __pi_sdk_create_find_tool,
    createGrepTool: __pi_sdk_create_grep_tool,
    createLsTool: __pi_sdk_create_ls_tool,
    createReadTool: __pi_sdk_create_read_tool,
    createWriteTool: __pi_sdk_create_write_tool,
    defineTool: __pi_sdk_define_tool,
    formatSize: __pi_sdk_format_size,
    getAgentDir: __pi_sdk_get_agent_dir,
    getEditorTheme: __pi_sdk_default_editor_theme,
    getMarkdownTheme: __pi_sdk_default_markdown_theme,
    getSelectListTheme: __pi_sdk_default_select_list_theme,
    getSettingsListTheme: __pi_sdk_default_settings_list_theme,
    parseFrontmatter: __pi_sdk_parse_frontmatter,
    serializeConversation: __pi_sdk_serialize_conversation,
    stripFrontmatter: __pi_sdk_strip_frontmatter,
    truncateHead: __pi_sdk_truncate_head,
    truncateLine: __pi_sdk_truncate_line,
    withFileMutationQueue: __pi_sdk_with_file_mutation_queue,
  },
  {},
);

// The five upstream lazy apis this build still cannot run. Each reason names
// the missing piece — a concrete Rust adapter the host does not have — instead
// of the flat "needs a provider implementation" a reader cannot act on.
const __pi_sdk_stream_gaps = {
  bedrockConverseStreamApi:
    "the host has no `bedrock-converse-stream` adapter: Bedrock authenticates with AWS SigV4 credentials and a region, which the Rust credential path does not carry",
  googleVertexApi:
    "the host has no `google-vertex` adapter: Vertex needs a GCP project plus location and ADC/access-token credentials, which the Rust credential path does not carry; use `googleGenerativeAIApi` (Gemini API key) instead",
  mistralConversationsApi:
    "the host adapter exists (`pi_ai::providers::MistralProvider`, api `mistral-conversations`) but the extension bridge does not route to it yet, so the factory stays a gap until a follow-up slice wires, tests and documents it",
  openAICodexResponsesApi:
    "the host has no `openai-codex-responses` adapter: the Codex Responses dialect is authenticated with a ChatGPT account token rather than an API key, and the Rust port implements neither that auth path nor the dialect",
  piMessagesApi:
    "the host has no `pi-messages` adapter: it is the first-party pi gateway protocol and this build has no endpoint or credential for it",
};

const __pi_sdk_pi_ai = __pi_sdk_module("@earendil-works/pi-ai", {
  AssistantMessageEventStream: AssistantMessageEventStream,
  EventStream: EventStream,
  StringEnum: __pi_sdk_string_enum,
  Type: __pi_typebox_module.Type,
  calculateCost: __pi_sdk_calculate_cost,
  contentText: __pi_sdk_content_text,
  createAssistantMessageEventStream: __pi_sdk_create_assistant_message_event_stream,
  uuidv7: __pi_sdk_uuid_v7,
});

const __pi_sdk_pi_ai_compat = __pi_sdk_module(
  "@earendil-works/pi-ai/compat",
  {
    anthropicMessagesApi: __pi_sdk_anthropic_messages_api,
    azureOpenAIResponsesApi: __pi_sdk_azure_openai_responses_api,
    complete: __pi_sdk_compat_complete,
    completeSimple: __pi_sdk_compat_complete_simple,
    createAssistantMessageEventStream: __pi_sdk_create_assistant_message_event_stream,
    getApiProvider: __pi_sdk_get_api_provider,
    getApiProviders: __pi_sdk_get_api_providers,
    googleGenerativeAIApi: __pi_sdk_google_generative_ai_api,
    openAICompletionsApi: __pi_sdk_openai_completions_api,
    openAIResponsesApi: __pi_sdk_openai_responses_api,
    registerApiProvider: __pi_sdk_register_api_provider,
    registerBuiltInApiProviders: __pi_sdk_register_built_in_api_providers,
    resetApiProviders: __pi_sdk_reset_api_providers,
    stream: __pi_sdk_compat_stream,
    streamSimple: __pi_sdk_compat_stream_simple,
    unregisterApiProviders: __pi_sdk_unregister_api_providers,
  },
  {
    bedrockConverseStreamApi: __pi_sdk_stream_gaps.bedrockConverseStreamApi,
    googleVertexApi: __pi_sdk_stream_gaps.googleVertexApi,
    mistralConversationsApi: __pi_sdk_stream_gaps.mistralConversationsApi,
    openAICodexResponsesApi: __pi_sdk_stream_gaps.openAICodexResponsesApi,
    piMessagesApi: __pi_sdk_stream_gaps.piMessagesApi,
  },
);

// Every value import from pi-agent-core in the upstream examples is type-only
// (`import type`), which the loader erases, so the module intentionally has no
// runtime exports of its own — it only needs to resolve.
const __pi_sdk_pi_agent_core = __pi_sdk_module("@earendil-works/pi-agent-core", {}, {});

const __pi_sdk_gondolin_gap = "the gondolin sandbox is a third-party VM package and is not bundled with the extension host";
const __pi_sdk_gondolin = __pi_sdk_module(
  "@earendil-works/gondolin",
  {},
  {
    RealFSProvider: __pi_sdk_gondolin_gap,
    VM: __pi_sdk_gondolin_gap,
  },
);

const __pi_sdk_specifiers = {
  "@earendil-works/pi-tui": __pi_sdk_pi_tui,
  "@earendil-works/pi-coding-agent": __pi_sdk_pi_coding_agent,
  "@earendil-works/pi-ai": __pi_sdk_pi_ai,
  "@earendil-works/pi-ai/compat": __pi_sdk_pi_ai_compat,
  "@earendil-works/pi-agent-core": __pi_sdk_pi_agent_core,
  "@earendil-works/gondolin": __pi_sdk_gondolin,
};

// Register the historical `@mariozechner/…` scope and the bare package name so
// extensions written against either naming keep loading.
for (const canonical of Object.keys(__pi_sdk_specifiers)) {
  const proxy = __pi_sdk_specifiers[canonical];
  const bare = canonical.indexOf("@earendil-works/") === 0 ? canonical.slice("@earendil-works/".length) : canonical;
  for (const alias of ["@mariozechner/" + bare, bare]) {
    if (Object.prototype.hasOwnProperty.call(__pi_sdk_specifiers, alias)) continue;
    __pi_sdk_specifiers[alias] = __pi_sdk_alias(canonical, alias, proxy);
  }
}

// Machine-readable inventory consumed by `tests/sdk_modules.rs`.
globalThis.__pi_sdk_manifest = function () {
  return JSON.stringify(__pi_sdk_manifest_data);
};

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
  "node:zlib": __pi_zlib_module,
  zlib: __pi_zlib_module,
  "node:process": __pi_process_module,
  process: __pi_process_module,
  "node:util": __pi_util_module,
  util: __pi_util_module,
  "node:child_process": __pi_child_process_module,
  child_process: __pi_child_process_module,
  "node:module": __pi_module_module,
  module: __pi_module_module,
  "node:readline": __pi_readline_module,
  readline: __pi_readline_module,
  typebox: __pi_typebox_module,
  "@sinclair/typebox": __pi_typebox_module,
  ...__pi_sdk_specifiers,
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

// ===========================================================================
// Web platform globals — `atob` / `btoa` / `crypto` / `URLSearchParams`
//
// QuickJS ships none of them (its C sources have no `URLSearchParams`), but
// upstream extensions treat them as ambient. The clearest case is the legacy
// OAuth example in
// `packages/coding-agent/examples/extensions/custom-provider-anthropic/`,
// whose PKCE step calls `crypto.getRandomValues`, `btoa`, `atob` and
// `crypto.subtle.digest("SHA-256", …)` before it can build the authorize URL
// with `new URLSearchParams({…})`.
//
// `crypto` reuses `__pi_crypto_module` (Node's `node:crypto` attaches the
// same WebCrypto objects), and the digests come from the host's
// `crypto.digest` bridge. `URL` itself is still not bridged — see
// `docs/NODE_BUILTINS.md` for what remains.
// ===========================================================================
(function () {
  const BufferCtor = __pi_buffer_module.Buffer;
  const decoder = new __pi_util_module.TextDecoder("utf-8");
  const encoder = new __pi_util_module.TextEncoder();

  function invalidCharacter(message) {
    const error = new Error(message);
    error.name = "InvalidCharacterError";
    return error;
  }

  // -- btoa / atob ----------------------------------------------------------
  //
  // The binary-string contract: `btoa` consumes one Latin-1 code unit per
  // byte and `atob` produces one. Anything above 0xFF is not representable
  // and must throw, otherwise a caller would silently hash the wrong bytes.

  function btoa(binary) {
    const text = String(binary);
    const bytes = new Uint8Array(text.length);
    for (let index = 0; index < text.length; index++) {
      const code = text.charCodeAt(index);
      if (code > 0xff) {
        throw invalidCharacter(
          "btoa: the string to be encoded contains characters outside of the Latin1 range",
        );
      }
      bytes[index] = code;
    }
    return BufferCtor.from(bytes).toString("base64");
  }

  function atob(encoded) {
    // The spec strips ASCII whitespace before validating; a length that is
    // not a multiple of four or a stray character is an error, and Node's
    // lenient `Buffer.from(x, "base64")` must not be allowed to hide it.
    const clean = String(encoded).replace(/[\t\n\f\r ]+/g, "");
    if (clean.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(clean)) {
      throw invalidCharacter("atob: the string to be decoded is not correctly encoded");
    }
    return BufferCtor.from(clean, "base64").toString("latin1");
  }

  if (typeof globalThis.btoa === "undefined") globalThis.btoa = btoa;
  if (typeof globalThis.atob === "undefined") globalThis.atob = atob;

  // -- crypto ---------------------------------------------------------------

  if (typeof globalThis.crypto === "undefined") {
    globalThis.crypto = __pi_crypto_module.webcrypto;
  }

  // -- URLSearchParams ------------------------------------------------------
  //
  // `application/x-www-form-urlencoded` encode/decode sets, straight from the
  // URL Standard: `*`, `-`, `.`, `_` and alphanumerics survive, a space
  // becomes `+`, everything else is percent-encoded UTF-8.

  const FORM_SAFE = /^[A-Za-z0-9*\-._]$/;

  function percentEncode(text) {
    let out = "";
    for (const byte of encoder.encode(String(text))) {
      if (byte === 0x20) {
        out += "+";
      } else if (FORM_SAFE.test(String.fromCharCode(byte))) {
        out += String.fromCharCode(byte);
      } else {
        out += "%" + byte.toString(16).toUpperCase().padStart(2, "0");
      }
    }
    return out;
  }

  function percentDecode(text) {
    const bytes = [];
    for (let index = 0; index < text.length; index++) {
      const character = text[index];
      if (character === "+") {
        bytes.push(0x20);
        continue;
      }
      if (character === "%") {
        const hex = text.slice(index + 1, index + 3);
        if (/^[0-9A-Fa-f]{2}$/.test(hex)) {
          bytes.push(parseInt(hex, 16));
          index += 2;
          continue;
        }
      }
      // Invalid escapes are kept verbatim, matching the URL parser's
      // percent-decode step (`decodeURIComponent` would throw instead).
      const code = character.charCodeAt(0);
      if (code < 0x80) {
        bytes.push(code);
      } else {
        // A lone surrogate cannot be represented in UTF-8; UTF-8 encode it
        // the way the standard's UTF-8 encoder does.
        for (const byte of encoder.encode(character)) bytes.push(byte);
      }
    }
    return decoder.decode(new Uint8Array(bytes));
  }

  class URLSearchParamsPolyfill {
    constructor(init) {
      this._entries = [];
      if (init === undefined || init === null) return;
      if (typeof init === "string") {
        this._parse(init);
        return;
      }
      if (typeof init === "object") {
        if (typeof init[Symbol.iterator] === "function") {
          for (const entry of init) {
            if (entry === null || typeof entry !== "object") {
              throw new TypeError("URLSearchParams: sequence element is not an object");
            }
            this.append(entry[0], entry[1]);
          }
          return;
        }
        for (const key of Object.keys(init)) this.append(key, init[key]);
      }
    }

    _parse(query) {
      const text = String(query);
      const body = text.startsWith("?") ? text.slice(1) : text;
      if (body === "") return;
      for (const part of body.split("&")) {
        if (part === "") continue;
        const separator = part.indexOf("=");
        if (separator < 0) {
          this._entries.push([percentDecode(part), ""]);
        } else {
          this._entries.push([
            percentDecode(part.slice(0, separator)),
            percentDecode(part.slice(separator + 1)),
          ]);
        }
      }
    }

    append(name, value) {
      this._entries.push([String(name), String(value)]);
    }

    delete(name, value) {
      const key = String(name);
      const hasValue = value !== undefined;
      const wanted = hasValue ? String(value) : null;
      this._entries = this._entries.filter(
        (entry) => entry[0] !== key || (hasValue && entry[1] !== wanted),
      );
    }

    get(name) {
      const key = String(name);
      const found = this._entries.find((entry) => entry[0] === key);
      return found === undefined ? null : found[1];
    }

    getAll(name) {
      const key = String(name);
      return this._entries.filter((entry) => entry[0] === key).map((entry) => entry[1]);
    }

    has(name, value) {
      const key = String(name);
      if (value === undefined) return this._entries.some((entry) => entry[0] === key);
      const wanted = String(value);
      return this._entries.some((entry) => entry[0] === key && entry[1] === wanted);
    }

    set(name, value) {
      const key = String(name);
      const entry = [key, String(value)];
      const index = this._entries.findIndex((candidate) => candidate[0] === key);
      if (index < 0) {
        this._entries.push(entry);
        return;
      }
      this._entries = this._entries.filter(
        (candidate, position) => candidate[0] !== key || position === index,
      );
      this._entries[index] = entry;
    }

    sort() {
      // `Array#sort` is stable in this host, matching the standard's
      // "sort by name, keeping relative order for equal names".
      this._entries.sort((left, right) => (left[0] < right[0] ? -1 : left[0] > right[0] ? 1 : 0));
    }

    get size() {
      return this._entries.length;
    }

    toString() {
      return this._entries
        .map((entry) => percentEncode(entry[0]) + "=" + percentEncode(entry[1]))
        .join("&");
    }

    forEach(callback, thisArg) {
      for (const [name, value] of this._entries) callback.call(thisArg, value, name, this);
    }

    *keys() {
      for (const entry of this._entries) yield entry[0];
    }

    *values() {
      for (const entry of this._entries) yield entry[1];
    }

    *entries() {
      for (const entry of this._entries) yield [entry[0], entry[1]];
    }

    [Symbol.iterator]() {
      return this.entries();
    }
  }
  Object.defineProperty(URLSearchParamsPolyfill.prototype, Symbol.toStringTag, {
    value: "URLSearchParams",
    configurable: true,
  });

  if (typeof globalThis.URLSearchParams === "undefined") {
    globalThis.URLSearchParams = URLSearchParamsPolyfill;
  }

  // -- URL ------------------------------------------------------------------
  //
  // QuickJS has no WHATWG URL parser and upstream extensions treat `URL` as
  // ambient: `packages/coding-agent/examples/extensions/custom-provider-gitlab-duo/`
  // reads its OAuth callback with `new URL(callbackUrl).searchParams.get("code")`.
  // This is a concentrated port of the URL Standard's parser / serializer /
  // resolver for the four special schemes (`http` / `https` / `ws` / `wss`),
  // `ftp` / `file`, and opaque (non-special) schemes such as `mailto:`.
  // Divergences are recorded in `docs/NODE_BUILTINS.md`.

  const SPECIAL_PORTS = {
    ftp: 21,
    file: null,
    http: 80,
    https: 443,
    ws: 80,
    wss: 443,
  };
  const SCHEME_PATTERN = /^([A-Za-z][A-Za-z0-9+.\-]*):/;

  // The URL Standard's percent-encode sets: each string lists the characters
  // the set adds on top of the C0 controls and everything above 0x7E, which
  // are always encoded. `%` is *not* in any of them, so an existing escape
  // survives a re-encode untouched.
  const FRAGMENT_EXTRA = ' "<>`';
  const QUERY_EXTRA = ' "#<>';
  const SPECIAL_QUERY_EXTRA = QUERY_EXTRA + "'";
  const PATH_EXTRA = QUERY_EXTRA + "?`{}";
  const USERINFO_EXTRA = PATH_EXTRA + "/:;=@[\\]^|";

  function urlEncode(text, extra) {
    let out = "";
    for (const byte of encoder.encode(String(text))) {
      if (byte < 0x20 || byte > 0x7e || extra.indexOf(String.fromCharCode(byte)) >= 0) {
        out += "%" + byte.toString(16).toUpperCase().padStart(2, "0");
      } else {
        out += String.fromCharCode(byte);
      }
    }
    return out;
  }

  function isSpecialScheme(scheme) {
    return Object.prototype.hasOwnProperty.call(SPECIAL_PORTS, scheme);
  }

  function indexOfFirst(text, characters) {
    for (let index = 0; index < text.length; index++) {
      if (characters.indexOf(text[index]) >= 0) return index;
    }
    return -1;
  }

  function canonicalPort(scheme, port) {
    if (port === null || port === "" || !/^[0-9]+$/.test(port)) return null;
    const number = parseInt(port, 10);
    const fallback = SPECIAL_PORTS[scheme];
    if (typeof fallback === "number" && number === fallback) return null;
    return String(number);
  }

  /** The standard's "remove leading/trailing C0 control or space" plus the
   *  tab/newline strip that precedes it. */
  function stripControlAndSpaces(input) {
    return String(input)
      .replace(/[\t\n\r]/g, "")
      .replace(/^[\u0000-\u0020]+/, "")
      .replace(/[\u0000-\u0020]+$/, "");
  }

  /** Dot-segment removal + percent-encoding, producing a hierarchical path
   *  (`pathname`) that always starts with `/`. */
  function normalizePath(text) {
    const segments = String(text).split("/");
    const out = [];
    for (const segment of segments) {
      if (segment === ".") continue;
      if (segment === "..") {
        if (out.length > 0) out.pop();
        continue;
      }
      out.push(segment);
    }
    // A trailing `.` / `..` collapses to a trailing slash, as the standard's
    // "shorten a URL's path" leaves one behind.
    const last = segments[segments.length - 1];
    if (last === "." || last === "..") out.push("");
    let path = out.map((segment) => urlEncode(segment, PATH_EXTRA)).join("/");
    if (path === "" || path[0] !== "/") path = "/" + path;
    return path;
  }

  function cloneRecord(record) {
    return {
      scheme: record.scheme,
      username: record.username,
      password: record.password,
      host: record.host,
      port: record.port,
      path: record.path,
      query: record.query,
      fragment: record.fragment,
    };
  }

  function newRecord(scheme) {
    return {
      scheme: scheme,
      username: "",
      password: "",
      host: null,
      port: null,
      path: "",
      query: null,
      fragment: null,
    };
  }

  /** Parse `[userinfo@]host[:port]`. Returns `false` for a malformed
   *  authority (the caller turns that into `new URL` throwing). */
  function parseAuthority(record, authority, special) {
    let userinfo = "";
    let hostport = authority;
    const at = authority.lastIndexOf("@");
    if (at >= 0) {
      userinfo = authority.slice(0, at);
      hostport = authority.slice(at + 1);
    }
    if (userinfo !== "") {
      const colon = userinfo.indexOf(":");
      const user = colon < 0 ? userinfo : userinfo.slice(0, colon);
      const password = colon < 0 ? "" : userinfo.slice(colon + 1);
      record.username = urlEncode(user, USERINFO_EXTRA);
      record.password = urlEncode(password, USERINFO_EXTRA);
    }
    if (hostport.startsWith("[")) {
      const close = hostport.indexOf("]");
      if (close < 0) return false;
      record.host = hostport.slice(0, close + 1).toLowerCase();
      const trailing = hostport.slice(close + 1);
      if (trailing === "") {
        record.port = null;
      } else if (trailing.startsWith(":")) {
        const portText = trailing.slice(1);
        if (portText !== "" && !/^[0-9]+$/.test(portText)) return false;
        record.port = canonicalPort(record.scheme, portText);
      } else {
        return false;
      }
      return true;
    }
    let host = hostport;
    let port = null;
    const colon = hostport.lastIndexOf(":");
    if (colon >= 0) {
      host = hostport.slice(0, colon);
      const portText = hostport.slice(colon + 1);
      // A default port canonicalizes to `null` while still being valid, so
      // reject on the text, not on the canonical result.
      if (portText !== "" && !/^[0-9]+$/.test(portText)) return false;
      port = canonicalPort(record.scheme, portText);
    }
    void special;
    record.host = host.toLowerCase();
    record.port = port;
    return true;
  }

  /** Split a `path[?query][#fragment]` tail and write it into `record`; an
   *  empty path leaves the inherited path in place (the `?q` / `#f` cases of
   *  relative resolution). */
  function applyTail(record, text, special) {
    let rest = String(text);
    let fragment = null;
    const hashIndex = rest.indexOf("#");
    if (hashIndex >= 0) {
      fragment = rest.slice(hashIndex + 1);
      rest = rest.slice(0, hashIndex);
    }
    let query = null;
    let hasQuery = false;
    const queryIndex = rest.indexOf("?");
    if (queryIndex >= 0) {
      query = rest.slice(queryIndex + 1);
      rest = rest.slice(0, queryIndex);
      hasQuery = true;
    }
    if (rest !== "") {
      const pathText = special ? rest.split("\\").join("/") : rest;
      record.path = normalizePath(pathText);
    }
    if (hasQuery) {
      record.query = urlEncode(query, special ? SPECIAL_QUERY_EXTRA : QUERY_EXTRA);
    }
    record.fragment = fragment === null ? null : urlEncode(fragment, FRAGMENT_EXTRA);
    return record;
  }

  function resolveRelative(base, reference) {
    const special = isSpecialScheme(base.scheme);
    const text = String(reference);
    if (text.startsWith("//")) {
      const after = text.slice(2);
      const cut = indexOfFirst(after, special ? "/?#\\" : "/?#");
      const record = newRecord(base.scheme);
      record.path = "/";
      parseAuthority(record, cut < 0 ? after : after.slice(0, cut), special);
      return applyTail(record, cut < 0 ? "" : after.slice(cut), special);
    }
    if (text.startsWith("?")) {
      const record = cloneRecord(base);
      record.query = null;
      record.fragment = null;
      return applyTail(record, text, special);
    }
    if (text.startsWith("#")) {
      // A fragment-only reference keeps the base query intact.
      const record = cloneRecord(base);
      record.fragment = null;
      return applyTail(record, text, special);
    }
    const record = cloneRecord(base);
    record.query = null;
    record.fragment = null;
    let tail = text;
    if (!(tail.startsWith("/") || (special && tail.startsWith("\\")))) {
      const slash = base.path.lastIndexOf("/");
      tail = (slash < 0 ? "/" : base.path.slice(0, slash + 1)) + tail;
    }
    return applyTail(record, tail, special);
  }

  function parseWithScheme(scheme, rest) {
    const special = isSpecialScheme(scheme);
    const record = newRecord(scheme);
    let hasAuthority = false;
    let remainder = rest;
    if (rest.startsWith("//")) {
      hasAuthority = true;
      const after = rest.slice(2);
      const cut = indexOfFirst(after, special ? "/?#\\" : "/?#");
      if (!parseAuthority(record, cut < 0 ? after : after.slice(0, cut), special)) return null;
      remainder = cut < 0 ? "" : after.slice(cut);
    } else if (special) {
      // The standard tolerates a missing `//` for special schemes
      // (`http:example.com/p`): skip the separator run and still parse an
      // authority.
      hasAuthority = true;
      const after = rest.replace(/^[\\/]+/, "");
      const cut = indexOfFirst(after, "/?#\\");
      if (!parseAuthority(record, cut < 0 ? after : after.slice(0, cut), special)) return null;
      remainder = cut < 0 ? "" : after.slice(cut);
    }
    if (!hasAuthority) {
      // Opaque path (`mailto:`, `urn:`, a custom scheme).
      let text = remainder;
      const hashIndex = text.indexOf("#");
      if (hashIndex >= 0) {
        record.fragment = urlEncode(text.slice(hashIndex + 1), FRAGMENT_EXTRA);
        text = text.slice(0, hashIndex);
      }
      const queryIndex = text.indexOf("?");
      if (queryIndex >= 0) {
        record.query = urlEncode(text.slice(queryIndex + 1), QUERY_EXTRA);
        text = text.slice(0, queryIndex);
      }
      record.path = urlEncode(text, PATH_EXTRA);
      return record;
    }
    return applyTail(record, remainder, special);
  }

  function parseUrlRecord(input, base) {
    const value = stripControlAndSpaces(input);
    const match = SCHEME_PATTERN.exec(value);
    if (match) {
      return parseWithScheme(match[1].toLowerCase(), value.slice(match[0].length));
    }
    let baseRecord = null;
    if (base !== undefined && base !== null) {
      if (base instanceof URLPolyfill) baseRecord = cloneRecord(base.__record);
      else if (typeof base === "string" || typeof base === "object") {
        baseRecord = parseUrlRecord(String(base), null);
      }
    }
    if (baseRecord === null) return null;
    return resolveRelative(baseRecord, value);
  }

  function serializeRecord(record) {
    let out = record.scheme + ":";
    if (record.host !== null) {
      out += "//";
      if (record.username !== "" || record.password !== "") {
        out += record.username;
        if (record.password !== "") out += ":" + record.password;
        out += "@";
      }
      out += record.host;
      if (record.port !== null) out += ":" + record.port;
    }
    out += record.path;
    if (record.query !== null) out += "?" + record.query;
    if (record.fragment !== null) out += "#" + record.fragment;
    return out;
  }

  class URLPolyfill {
    constructor(input, base) {
      const record = parseUrlRecord(input, base);
      if (record === null) throw new TypeError("Invalid URL: " + String(input));
      this.__record = record;
      this.__searchParams = null;
    }

    get href() {
      return serializeRecord(this.__record);
    }

    set href(value) {
      const record = parseUrlRecord(String(value), null);
      if (record === null) throw new TypeError("Invalid URL: " + String(value));
      this.__record = record;
      this.__searchParams = null;
    }

    get origin() {
      const record = this.__record;
      if (!isSpecialScheme(record.scheme) || record.scheme === "file") return "null";
      let out = record.scheme + "://" + record.host;
      if (record.port !== null) out += ":" + record.port;
      return out;
    }

    get protocol() {
      return this.__record.scheme + ":";
    }

    set protocol(value) {
      const scheme = String(value).replace(/:$/, "").toLowerCase();
      if (!/^[A-Za-z][A-Za-z0-9+.\-]*$/.test(scheme)) return;
      // The standard refuses to switch between a special and a non-special
      // scheme; a same-kind swap is applied and the port re-canonicalized.
      if (isSpecialScheme(scheme) !== isSpecialScheme(this.__record.scheme)) return;
      this.__record.scheme = scheme;
      if (isSpecialScheme(scheme)) {
        this.__record.port = canonicalPort(scheme, this.__record.port) === null ? null : this.__record.port;
      }
    }

    get username() {
      return this.__record.username;
    }

    set username(value) {
      this.__record.username = urlEncode(String(value), USERINFO_EXTRA);
    }

    get password() {
      return this.__record.password;
    }

    set password(value) {
      this.__record.password = urlEncode(String(value), USERINFO_EXTRA);
    }

    get host() {
      const record = this.__record;
      if (record.host === null) return "";
      return record.port === null ? record.host : record.host + ":" + record.port;
    }

    set host(value) {
      this.__setAuthority(String(value), true);
    }

    get hostname() {
      return this.__record.host === null ? "" : this.__record.host;
    }

    set hostname(value) {
      this.__setAuthority(String(value), false);
    }

    __setAuthority(value, replacePort) {
      const record = this.__record;
      if (record.host === null) return;
      if (value === "") {
        record.host = "";
        if (replacePort) record.port = null;
        return;
      }
      const probe = newRecord(record.scheme);
      if (!parseAuthority(probe, value, isSpecialScheme(record.scheme))) return;
      record.host = probe.host;
      if (replacePort) record.port = probe.port;
    }

    get port() {
      return this.__record.port === null ? "" : this.__record.port;
    }

    set port(value) {
      if (this.__record.host === null) return;
      const text = String(value);
      if (text === "") {
        this.__record.port = null;
        return;
      }
      const port = canonicalPort(this.__record.scheme, text);
      // An invalid port is ignored, matching the standard's setter.
      if (port === null && !/^[0-9]+$/.test(text)) return;
      this.__record.port = port;
    }

    get pathname() {
      return this.__record.path === "" && this.__record.host !== null ? "/" : this.__record.path;
    }

    set pathname(value) {
      const text = String(value);
      if (this.__record.host === null && !isSpecialScheme(this.__record.scheme)) {
        this.__record.path = urlEncode(text, PATH_EXTRA);
        return;
      }
      this.__record.path = normalizePath(isSpecialScheme(this.__record.scheme) ? text.split("\\").join("/") : text);
    }

    get search() {
      const query = this.__record.query;
      return query === null || query === "" ? "" : "?" + query;
    }

    set search(value) {
      const text = String(value).replace(/^\?/, "");
      this.__record.query = text === "" ? null : urlEncode(text, isSpecialScheme(this.__record.scheme) ? SPECIAL_QUERY_EXTRA : QUERY_EXTRA);
      this.__searchParams = null;
    }

    get searchParams() {
      if (this.__searchParams === null) this.__searchParams = makeLiveSearchParams(this);
      return this.__searchParams;
    }

    get hash() {
      const fragment = this.__record.fragment;
      return fragment === null || fragment === "" ? "" : "#" + fragment;
    }

    set hash(value) {
      const text = String(value).replace(/^#/, "");
      this.__record.fragment = text === "" ? null : urlEncode(text, FRAGMENT_EXTRA);
    }

    toString() {
      return this.href;
    }

    toJSON() {
      return this.href;
    }

    static canParse(input, base) {
      try {
        return parseUrlRecord(input, base) !== null;
      } catch (_error) {
        return false;
      }
    }

    static parse(input, base) {
      const record = parseUrlRecord(input, base);
      if (record === null) return null;
      const url = Object.create(URLPolyfill.prototype);
      url.__record = record;
      url.__searchParams = null;
      return url;
    }
  }
  Object.defineProperty(URLPolyfill.prototype, Symbol.toStringTag, {
    value: "URL",
    configurable: true,
  });

  /** A `URL.searchParams` view that writes every mutation back into the
   *  record, so `url.searchParams.set("a", "1")` shows up in `url.href`. */
  function makeLiveSearchParams(url) {
    const record = url.__record;
    const params = new URLSearchParamsPolyfill(record.query === null ? "" : record.query);
    const sync = () => {
      const text = params.toString();
      record.query = text === "" ? null : text;
    };
    for (const name of ["append", "delete", "set", "sort"]) {
      const original = params[name];
      params[name] = function () {
        const result = original.apply(params, arguments);
        sync();
        return result;
      };
    }
    return params;
  }

  if (typeof globalThis.URL === "undefined") {
    globalThis.URL = URLPolyfill;
  }
})();

// ===========================================================================
// `fetch` global — WHATWG subset backed by the host HTTP bridge
//
// Upstream extensions run in Node/Bun, so they get the platform `fetch`.
// QuickJS ships none and a polyfill alone cannot reach the network; the repo's
// own `.pi/extensions/import-repro.ts` fetches GitHub gists and issue comments.
// `host_fetch` performs the request through the same `reqwest` stack the
// agent's providers use (same rustls / proxy-env policy) and returns
// `{status, statusText, url, redirected, headers, body(base64)}`; the classes
// below rebuild a `Response` from it.
//
// Covered: `fetch(input, init)` with `method` / `headers` / `body` (string,
// ArrayBuffer, TypedArray, URLSearchParams) / `signal` / non-standard
// `timeout`; `Headers`; `Request` (URL or `Request` input); `Response.ok` /
// `status` / `statusText` / `url` / `redirected` / `headers` / `bodyUsed` /
// `text()` / `json()` / `arrayBuffer()` / `clone()`.
//
// Deliberately not covered (see `docs/EXTENSIONS.md`): streaming bodies
// (`ReadableStream`, `Response.body`), `FormData` / `Blob` bodies, and the
// `credentials` / `mode` / `cache` / `redirect` options.
// ===========================================================================
(function () {
  // Never shadow an engine-provided implementation.
  if (typeof globalThis.fetch !== "undefined") return;

  const HEADER_INVALID = /[\r\n]/;

  function normalizeHeaderName(name) {
    return String(name).toLowerCase();
  }

  class HeadersPolyfill {
    constructor(init) {
      /** @type {Map<string, {name: string, values: string[]}>} */
      this._map = new Map();
      if (init == null) return;
      if (init instanceof HeadersPolyfill) {
        for (const pair of init._entries()) this.append(pair[0], pair[1]);
      } else if (Array.isArray(init)) {
        for (const pair of init) {
          if (!Array.isArray(pair) || pair.length !== 2) {
            throw new TypeError("Headers: each entry must be a [name, value] pair");
          }
          this.append(pair[0], pair[1]);
        }
      } else if (typeof init === "object") {
        for (const name of Object.keys(init)) this.append(name, init[name]);
      }
    }

    append(name, value) {
      const key = normalizeHeaderName(name);
      const text = String(value);
      if (HEADER_INVALID.test(text)) {
        throw new TypeError("Headers: invalid header value for " + name);
      }
      const existing = this._map.get(key);
      if (existing) existing.values.push(text);
      else this._map.set(key, { name: String(name), values: [text] });
    }

    set(name, value) {
      const key = normalizeHeaderName(name);
      const text = String(value);
      if (HEADER_INVALID.test(text)) {
        throw new TypeError("Headers: invalid header value for " + name);
      }
      this._map.set(key, { name: String(name), values: [text] });
    }

    get(name) {
      const entry = this._map.get(normalizeHeaderName(name));
      return entry ? entry.values.join(", ") : null;
    }

    has(name) {
      return this._map.has(normalizeHeaderName(name));
    }

    delete(name) {
      this._map.delete(normalizeHeaderName(name));
    }

    forEach(callback, thisArg) {
      for (const pair of this._entries()) {
        callback.call(thisArg, pair[1], pair[0], this);
      }
    }

    keys() {
      return this._entries()
        .map((pair) => pair[0])
        .values();
    }

    values() {
      return this._entries()
        .map((pair) => pair[1])
        .values();
    }

    entries() {
      return this._entries().values();
    }

    [Symbol.iterator]() {
      return this.entries();
    }

    /** @internal Flattened `[name, joinedValue]` pairs. */
    _entries() {
      const out = [];
      for (const entry of this._map.values()) {
        out.push([entry.name, entry.values.join(", ")]);
      }
      return out;
    }
  }

  function requireBuffer() {
    if (typeof globalThis.Buffer === "undefined") {
      throw new Error("fetch: Buffer is not available in this host build");
    }
    return globalThis.Buffer;
  }

  function bodyToBase64(body) {
    const BufferCtor = requireBuffer();
    if (typeof body === "string") {
      return BufferCtor.from(body, "utf8").toString("base64");
    }
    if (typeof ArrayBuffer !== "undefined" && body instanceof ArrayBuffer) {
      return BufferCtor.from(new Uint8Array(body)).toString("base64");
    }
    if (typeof ArrayBuffer !== "undefined" && ArrayBuffer.isView(body)) {
      return BufferCtor.from(
        new Uint8Array(body.buffer, body.byteOffset, body.byteLength),
      ).toString("base64");
    }
    if (
      typeof globalThis.URLSearchParams !== "undefined" &&
      body instanceof globalThis.URLSearchParams
    ) {
      return BufferCtor.from(body.toString(), "utf8").toString("base64");
    }
    return BufferCtor.from(String(body), "utf8").toString("base64");
  }

  class ResponsePolyfill {
    constructor(bodyBytes, init) {
      const options = init || {};
      this._bodyBytes = bodyBytes;
      this.status = typeof options.status === "number" ? options.status : 200;
      this.statusText = options.statusText || "";
      this.ok = this.status >= 200 && this.status < 300;
      this.url = options.url || "";
      this.redirected = Boolean(options.redirected);
      this.type = "basic";
      this.bodyUsed = false;
      this.headers =
        options.headers instanceof HeadersPolyfill
          ? options.headers
          : new HeadersPolyfill(options.headers);
      // No `ReadableStream` in this host; see the module banner.
      this.body = null;
    }

    _takeBody() {
      if (this.bodyUsed) {
        throw new TypeError("Failed to execute: body stream already read");
      }
      this.bodyUsed = true;
      return this._bodyBytes || new Uint8Array(0);
    }

    async arrayBuffer() {
      const bytes = this._takeBody();
      const offset = bytes.byteOffset || 0;
      return bytes.buffer.slice(offset, offset + bytes.byteLength);
    }

    async text() {
      const bytes = this._takeBody();
      return new globalThis.TextDecoder("utf-8").decode(bytes);
    }

    async json() {
      return JSON.parse(await this.text());
    }

    clone() {
      if (this.bodyUsed) {
        throw new TypeError("Failed to execute: body stream already read");
      }
      return new ResponsePolyfill(this._bodyBytes, {
        status: this.status,
        statusText: this.statusText,
        url: this.url,
        redirected: this.redirected,
        headers: this.headers,
      });
    }
  }

  class RequestPolyfill {
    constructor(input, init) {
      const options = init || {};
      if (input instanceof RequestPolyfill) {
        this.url = input.url;
        this.method = input.method;
        this.headers = new HeadersPolyfill(input.headers);
        this.body = input.body;
        this.signal = options.signal !== undefined ? options.signal : input.signal;
        if (options.headers !== undefined) this.headers = new HeadersPolyfill(options.headers);
        if (options.body !== undefined) this.body = options.body;
        if (options.method !== undefined) this.method = String(options.method).toUpperCase();
        return;
      }
      if (typeof input !== "string") {
        throw new TypeError("fetch: input must be a URL string or Request");
      }
      this.url = input;
      this.method = options.method === undefined ? "GET" : String(options.method).toUpperCase();
      this.headers = new HeadersPolyfill(options.headers);
      this.body = options.body === undefined ? null : options.body;
      this.signal = options.signal === undefined ? null : options.signal;
    }
  }

  async function fetchPolyfill(input, init) {
    const options = init || {};
    const request =
      input instanceof RequestPolyfill ? input : new RequestPolyfill(input, options);
    const signal = options.signal !== undefined ? options.signal : request.signal;
    if (signal && signal.aborted) throw makeAbortError();
    if (typeof globalThis.host_fetch !== "function") {
      throw new Error("fetch is not available in this host build");
    }

    const headers = [];
    request.headers.forEach((value, name) => headers.push([name, value]));
    const body = request.body == null ? null : bodyToBase64(request.body);
    const timeout =
      typeof options.timeout === "number" && options.timeout > 0 ? options.timeout : undefined;
    // Shared with `pi.exec`: the shim allocates, `host_fetch` registers,
    // `host_fetch_cancel` addresses.
    const id = __pi_next_exec_id++;
    let onAbort = null;
    if (signal && typeof signal.addEventListener === "function") {
      onAbort = () => {
        if (typeof globalThis.host_fetch_cancel === "function") {
          globalThis.host_fetch_cancel(id);
        }
      };
      signal.addEventListener("abort", onAbort);
    }
    let raw;
    try {
      raw = await globalThis.host_fetch(
        JSON.stringify({
          id: id,
          url: request.url,
          method: request.method,
          headers: headers,
          body: body,
          timeout: timeout,
        }),
      );
    } finally {
      if (signal && typeof signal.removeEventListener === "function" && onAbort) {
        signal.removeEventListener("abort", onAbort);
      }
    }
    // The host result can lose the race with an abort that landed while the
    // promise was in flight; the signal's state is authoritative.
    if (signal && signal.aborted) throw makeAbortError();

    let result;
    try {
      result = typeof raw === "string" ? JSON.parse(raw) : raw;
    } catch (_e) {
      throw new Error("pi extension host returned malformed JSON for `fetch`");
    }
    if (!result || result.ok !== true) {
      const error = new Error((result && result.message) || "fetch failed");
      error.name = (result && result.name) || "TypeError";
      throw error;
    }

    const buffer = requireBuffer().from(result.body || "", "base64");
    const bytes = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
    return new ResponsePolyfill(bytes, {
      status: result.status,
      statusText: result.statusText,
      url: result.url,
      redirected: result.redirected,
      headers: result.headers,
    });
  }

  globalThis.Headers = HeadersPolyfill;
  globalThis.Request = RequestPolyfill;
  globalThis.Response = ResponsePolyfill;
  globalThis.fetch = fetchPolyfill;
})();
