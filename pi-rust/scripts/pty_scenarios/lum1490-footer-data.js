// PTY evidence fixture for LUM-1490: the `footerData` host→JS query channel.
//
// A custom footer is the only reader of `footerData`, so this fixture installs
// one and prints what it read. Everything it prints goes through the real
// bridge: the Rust driver pushes the snapshot (`JsExtensionHost::sync_footer_data`)
// and `getGitBranch()` / `getAvailableProviderCount()` query it back from inside
// `render()`, which is exactly the re-entrant call the shim has to support.
//
// `onBranchChange` is subscribed to the way upstream's own example does it
// (`packages/coding-agent/examples/extensions/custom-footer.ts:28`), so a
// `git checkout` in another terminal has to reach the footer without a restart.
//
// Run with pi-rust/scripts/pty_capture_win.py (see
// scripts/pty_scenarios/lum1490-footer-data.json).

module.exports = function (pi) {
  let enabled = false;
  let renders = 0;
  let branchChanges = 0;

  function line(data) {
    // `null` is upstream's answer outside a repo / on a detached HEAD
    // (`footer-data-provider.ts:126-131`); print it as text so the frame can
    // tell "no branch" from "channel broken" (which would be `undefined`).
    const branch = data.getGitBranch();
    return [
      "custom-footer branch=" + (branch === null ? "null" : branch),
      "custom-footer changes=" + branchChanges + " renders=" + renders,
      "custom-footer providers=" + data.getAvailableProviderCount(),
    ];
  }

  function install(ctx) {
    ctx.ui.setFooter((tui, theme, data) => {
      // Upstream's example subscribes here, at factory time. It must not throw
      // and it must return an unsubscribe function (`dispose` uses it).
      const unsubscribe = data.onBranchChange(function () {
        branchChanges += 1;
      });
      return {
        invalidate: function () {},
        dispose: unsubscribe,
        render: function () {
          renders += 1;
          return line(data);
        },
      };
    });
    enabled = true;
  }

  pi.registerCommand("custom-footer", {
    description: "Install the footerData-reading footer",
    handler: async function (_args, ctx) {
      install(ctx);
      ctx.ui.notify("custom footer installed", "info");
      return "custom footer installed";
    },
  });

  pi.registerCommand("default-footer", {
    description: "Restore the built-in footer",
    handler: async function (_args, ctx) {
      ctx.ui.setFooter(undefined);
      enabled = false;
      ctx.ui.notify("default footer restored", "info");
      return "default footer restored";
    },
  });

  // Panel 1 wants the custom footer up from the first frame, without a command
  // being typed first — `session_start` is dispatched before the loop paints.
  pi.on("session_start", async function (_event, ctx) {
    install(ctx);
  });

  // A visible marker so a screenshot shows the extension loaded at all.
  pi.on("session_start", async function (_event, ctx) {
    ctx.ui.setStatus("lum1490", enabled ? "footer on" : "footer off");
  });
};
