// Node-side smoke test for the Pi Rust agent compiled to
// `wasm32-unknown-unknown`. The `pi-agent-core` package publishes a
// `package.json` that wires its generated `.wasm` and `.js` glue, so
// `import init from "pi-agent-core"` resolves to the artifact set
// `wasm-pack build -p pi-agent-core --target web` produced.
//
// We exercise the same surface the browser host uses:
//   1. `await init()` — instantiate the WebAssembly module.
//   2. `register_faux_provider(["hello"])` — seed the in-memory
//      catalog with one scripted reply.
//   3. `new AgentHandle("faux:faux-model")` — construct an agent
//      backed by the faux provider.
//   4. `agent.subscribe(...)` — collect the events the agent emits
//      while a turn runs.
//   5. `await agent.prompt("hello")` — drive the agent.
//
// Run with `npm test` (the package.json wires `node --test test.mjs`)
// or directly with `node --test test.mjs` from this directory. The
// Node version must be ≥ 18 because `WebAssembly.Module` and
// `WebAssembly.instantiate` are used.
//
// The default `init()` shim shipped by wasm-pack's `web` target
// uses `fetch()` to load the `.wasm`, which fails under Node for
// `file://` URLs. We therefore use `initSync()` with the bytes read
// directly from disk — a pure-Node path that mirrors what the
// browser does (just without the network hop).

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";
import init, { initSync } from "pi-agent-core";
import { AgentHandle, register_faux_provider } from "pi-agent-core";

const here = dirname(fileURLToPath(import.meta.url));
const wasmPath = resolve(here, "../../crates/pi-agent-core/pkg/pi_agent_core_bg.wasm");

let wasmBytes = null;
async function ensureInit() {
  if (wasmBytes === null) {
    wasmBytes = await readFile(wasmPath);
  }
  initSync({ module: wasmBytes });
  await init({ module_or_path: wasmBytes });
}

test("agent emits turn_start + message_start + message_end + turn_end for a faux prompt", async () => {
  await ensureInit();
  register_faux_provider(["hello from node"]);

  const agent = new AgentHandle("faux:faux-model");
  const events = [];
  agent.subscribe((event) => events.push(event));

  await agent.prompt("hello");

  const tags = events.map((e) => e.type);
  assert.ok(
    tags.includes("turn_start"),
    `expected turn_start in event stream, got ${tags.join(",")}`,
  );
  assert.ok(
    tags.includes("message_start"),
    `expected message_start in event stream, got ${tags.join(",")}`,
  );
  assert.ok(
    tags.includes("message_end"),
    `expected message_end in event stream, got ${tags.join(",")}`,
  );
  assert.ok(
    tags.includes("turn_end"),
    `expected turn_end in event stream, got ${tags.join(",")}`,
  );

  // The faux provider always emits a single AssistantMessage with the
  // scripted reply as its first text content block. Confirm the agent
  // recorded the message body.
  const messageEnd = events.find((e) => e.type === "message_end");
  assert.ok(messageEnd, "expected message_end event");
  assert.equal(messageEnd.message?.content?.[0]?.type, "text");
  assert.equal(messageEnd.message.content[0].text, "hello from node");
});

test("agent.subscribe/unsubscribe wires and unwires callbacks", async () => {
  await ensureInit();
  register_faux_provider(["x"]);

  const agent = new AgentHandle("faux:faux-model");
  assert.equal(agent.subscriber_count, 0, "fresh agent has no subscribers");

  const id = agent.subscribe(() => {});
  assert.equal(agent.subscriber_count, 1, "subscribe registers a callback");

  agent.unsubscribe(id);
  assert.equal(
    agent.subscriber_count,
    0,
    "unsubscribe removes the callback",
  );
});

test("register_faux_provider reports the provider and models in the summary", async () => {
  await ensureInit();
  const summary = register_faux_provider(["reply"]);
  assert.equal(summary.provider, "faux");
  assert.ok(Array.isArray(summary.models));
  assert.ok(summary.models.includes("faux-model"));
});

test("AgentHandle.new rejects unknown model ids", async () => {
  await ensureInit();
  register_faux_provider(["reply"]);
  assert.throws(() => new AgentHandle("missing:model"), /model not found/);
});
