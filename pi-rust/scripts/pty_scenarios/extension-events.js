// PTY evidence fixture for LUM-1246 (Stage 68): prove that the upstream
// event names an extension subscribes to actually arrive at runtime in the
// Rust port's TUI.
//
// The widget is the evidence: it is repainted *from a handler*, so a row
// only ever shows a non-zero count when the host really dispatched that
// event into QuickJS during this session. Anything the host cannot emit
// yet stays at 0 and is reported as such.
//
// Run with pi-rust/scripts/pty_capture.py (see
// scripts/pty_scenarios/extension-events.json).

module.exports = function (pi) {
  // Upstream lifecycle names, in the order upstream emits them. `input`
  // and `user_bash` come from the driver, the rest from the agent fan-out.
  const NAMES = [
    "session_start",
    "agent_start",
    "turn_start",
    "message_start",
    "message_update",
    "message_end",
    "tool_execution_start",
    "tool_execution_update",
    "tool_execution_end",
    "turn_end",
    "agent_end",
    "user_bash",
    "input",
  ];
  const counts = Object.create(null);
  let ui = null;
  let handled = 0;

  function render() {
    if (!ui) return;
    const lines = ["LUM-1246 lifecycle events delivered by the host:"];
    for (const name of NAMES) {
      lines.push("  " + name + "=" + (counts[name] || 0));
    }
    lines.push("  handlers-run=" + handled);
    ui.setWidget("lum1246-lifecycle", lines, { placement: "aboveEditor" });
  }

  for (const name of NAMES) {
    pi.on(name, (event, ctx) => {
      ui = ctx.ui;
      counts[name] = (counts[name] || 0) + 1;
      handled += 1;
      render();
    });
  }

  // Teardown proof. The widget is gone by the time `session_shutdown`
  // runs (the TUI is exiting), so the handler appends a line to a file the
  // harness leaves behind in the child's HOME; the screenshot run and the
  // regression test read the same signal.
  pi.on("session_shutdown", (event, ctx) => {
    const fs = require("node:fs");
    const os = require("node:os");
    const path = os.homedir() + "/lum1246-shutdown-proof.txt";
    fs.appendFileSync(
      path,
      "session_shutdown reason=" + event.reason + " handled=" + (handled + 1) + "\n",
    );
  });
};
