# Pi Rust WASM host — Stage 6 sample

A minimal browser-side host for the Pi Rust agent compiled to
`wasm32-unknown-unknown`. The page is plain HTML + ESM + Vite; the only
runtime dependency is the `pi-agent-core` wasm package produced by
`wasm-pack build -p pi-agent-core --target web --features wasm`.

## Layout

```
wasm-host/
  index.html        # page shell — input + send button + event log
  main.mjs          # JS host: init WASM, register faux provider, wire UI
  test.mjs          # node `--test` runner — drives AgentHandle end-to-end
  vite.config.js    # Vite config (port 5173, optimizeDeps.exclude)
  package.json      # depends on the local pi-agent-core/pkg/ directory
```

The `pi-agent-core` dependency is a `file:` reference to
`../../crates/pi-agent-core/pkg` — the directory `wasm-pack build`
emits. Rebuilding the crate regenerates that directory; re-running
`npm install` re-links it.

## Quick start

```bash
# 1. Build the wasm package.
cd ../../../crates/pi-agent-core
wasm-pack build --target web --features wasm

# 2. Install JS deps.
cd ../../examples/wasm-host
npm install

# 3. Run the dev server.
npm run dev
# → open http://localhost:5173

# 4. Run the node smoke tests (no browser required).
npm test
```

## What the page does

1. `await init()` — instantiate the WebAssembly module.
2. `register_faux_provider(["hello from the faux agent", "second reply — queue exhausted, replaying"])` — seed the in-memory model catalog with a faux provider and a 2-step script. The faux provider pops one entry per `AgentHandle.prompt` call and reuses the last entry forever after.
3. `new AgentHandle("faux:faux-model")` — construct an agent backed by the registered model.
4. `agent.subscribe((event) => ...)` — register a JS callback for every `AgentEvent` the agent emits.
5. `await agent.prompt(text)` — drive the turn. The promise resolves when the agent loop exits; rejected promises carry the `AgentError` message.

The event log prints one row per emitted event. The faux provider
emits `turn_start` → `message_start` → `done` → `message_end` →
`turn_end` for each prompt (tool-call handling is wired but the faux
script never produces one — see `pi_ai::providers::faux`).

## JS-side surface

The `pi-agent-core` package exports:

| Symbol                  | Purpose                                                    |
| ----------------------- | ---------------------------------------------------------- |
| `default init(...)`     | Instantiate the wasm module (returns a `Promise<InitOutput>`) |
| `AgentHandle`           | Class wrapping `pi_agent_core::wasm::AgentHandle`          |
| `register_faux_provider` | Seed the model catalog with a faux provider                |
| `lookup_model`          | Resolve a model id → `{provider, id, ...}`                 |
| `list_models`           | Snapshot the registered models as a `{provider: [...]}` map |

`AgentHandle` instances expose:

| Member                          | Purpose                                                  |
| ------------------------------- | -------------------------------------------------------- |
| `new AgentHandle(modelId)`      | Construct an agent backed by `modelId`                  |
| `prompt(text) → Promise<void>`  | Run a single turn                                        |
| `subscribe(cb) → number`        | Register a JS callback, return its id                    |
| `unsubscribe(id)`               | Remove a callback                                        |
| `subscriber_count`              | Read-only count of registered callbacks                  |
| `model_id`                      | Read-only configured model id                            |

## Failure modes

* `AgentHandle.new("missing:model")` throws `model not found: ...` if
  no faux provider has been registered (or the model id does not
  resolve in the catalog).
* `AgentHandle.new(...)` throws `no stream function registered; call
  register_faux_provider() before constructing an AgentHandle` when
  the JS host forgets to call `register_faux_provider` first.
* `prompt(...)` rejects with the `AgentError` message when the
  streaming layer surfaces an error (e.g. malformed stream, abort).
