// Minimal JS host for the Pi Rust agent compiled to
// `wasm32-unknown-unknown`. The wasm package lives at
// `../../crates/pi-agent-core/pkg/` and is published as the
// `pi-agent-core` npm dependency (see `package.json`). The package's
// generated `pi_agent_core.js` exposes the WebAssembly init glue
// (`init`, `initSync`) and the JS-friendly class wrappers
// (`AgentHandle`, `register_faux_provider`, …).
import init, {
  AgentHandle,
  register_faux_provider,
} from "pi-agent-core";

const output = document.getElementById("output");
const form = document.getElementById("prompt-form");
const input = document.getElementById("prompt-input");
const sendButton = document.getElementById("send-button");

let agent = null;

function appendEvent(event) {
  const node = document.createElement("div");
  node.className = "event";
  const tag = document.createElement("span");
  tag.className = "tag";
  tag.textContent = `[${event.type ?? "?"}]`;
  node.appendChild(tag);
  const body = document.createElement("span");
  body.className = "body";
  switch (event.type) {
    case "turn_start":
      body.textContent = "turn started";
      break;
    case "message_start":
      body.textContent = `model = ${event.model}`;
      break;
    case "message_update": {
      // `AssistantMessageUpdate` is a tagged enum. The discriminant
      // is in `kind`; the payload lives in the per-variant fields.
      const update = event;
      switch (update.kind) {
        case "text_delta":
          body.textContent = update.delta ?? "";
          break;
        case "thinking_delta":
          body.textContent = `[thinking] ${update.delta ?? ""}`;
          break;
        case "tool_call_delta":
          body.textContent = `tool_call#${update.index} ${update.name ?? ""}`;
          break;
        default:
          body.textContent = JSON.stringify(update);
      }
      break;
    }
    case "message_end":
      body.textContent = "(message finished)";
      break;
    case "tool_execution_start":
      body.textContent = `tool ${event.call?.name ?? "?"} started`;
      break;
    case "tool_execution_end":
      body.textContent = `tool finished in ${event.duration_ms}ms`;
      break;
    case "turn_end":
      body.textContent = "(turn ended)";
      node.classList.add("assistant");
      break;
    case "error":
      body.textContent = String(event);
      node.classList.add("error");
      break;
    default:
      body.textContent = JSON.stringify(event);
  }
  node.appendChild(body);
  output.appendChild(node);
  output.scrollTop = output.scrollHeight;
}

function appendError(message) {
  const node = document.createElement("div");
  node.className = "event error";
  const tag = document.createElement("span");
  tag.className = "tag";
  tag.textContent = "[host-error]";
  const body = document.createElement("span");
  body.className = "body";
  body.textContent = message;
  node.append(tag, body);
  output.appendChild(node);
  output.scrollTop = output.scrollHeight;
}

async function bootstrap() {
  try {
    await init();

    // Seed the faux provider with two scripted replies. The first
    // prompt returns "hello from the faux agent", the second returns
    // "second reply", and any subsequent prompt reuses the last
    // entry.
    const summary = register_faux_provider([
      "hello from the faux agent",
      "second reply — queue exhausted, replaying",
    ]);
    appendEvent({
      type: "host-info",
      payload: `faux provider registered: ${JSON.stringify(summary)}`,
    });

    agent = new AgentHandle("faux:faux-model");
    agent.subscribe((event) => appendEvent(event));
    appendEvent({
      type: "host-info",
      payload: `agent ready: model_id = ${agent.model_id}`,
    });

    form.addEventListener("submit", onSubmit);
    input.disabled = false;
    sendButton.disabled = false;
    input.focus();
  } catch (err) {
    appendError(`bootstrap failed: ${err?.stack ?? err}`);
  }
}

async function onSubmit(event) {
  event.preventDefault();
  const text = input.value.trim();
  if (!text || !agent) return;
  appendEvent({ type: "host-info", payload: `> ${text}` });
  input.value = "";
  input.disabled = true;
  sendButton.disabled = true;
  try {
    await agent.prompt(text);
  } catch (err) {
    appendError(`prompt failed: ${err?.stack ?? err}`);
  } finally {
    input.disabled = false;
    sendButton.disabled = false;
    input.focus();
  }
}

bootstrap();
