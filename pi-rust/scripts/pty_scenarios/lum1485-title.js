// PTY evidence fixture for LUM-1485: `ctx.ui.setTitle(title)` must reach the
// real terminal title (OSC 0 + BEL) through the whole bridge — JS shim →
// `host_ui_region("setTitle")` → `UiRegionHost::set_title` → `RegionOp::Title`
// → `App::set_terminal_title` → the driver's write to the tty.
//
// A terminal title is not cell content, so the frames this scenario captures
// cannot show it; the assertions for it are `raw_expect` / `raw_reject` against
// the bytes the ConPTY stream carried (see `pty_capture.RAW_TRACE`).
//
// Run with pi-rust/scripts/pty_capture_win.py (see
// scripts/pty_scenarios/lum1485-terminal-title.json).

module.exports = function (pi) {
  pi.registerCommand("retitle", {
    description: "Set the terminal title from an extension",
    handler: async function (args, ctx) {
      // The status line is the visible half of this panel: it proves the
      // command handler ran at all, so a missing `raw_expect` on the title
      // below means the title channel is broken, not the dispatch.
      ctx.ui.setStatus("lum1485", "handler ran");
      ctx.ui.setTitle("ext owns the title");
      return "retitled";
    },
  });

  pi.registerCommand("retitle-evil", {
    description: "Set a title containing BEL and ESC",
    handler: async function (args, ctx) {
      // A raw BEL would terminate the OSC 0 sequence early and a following ESC
      // would start a new control sequence. The host must neutralise both, so
      // the terminal sees one title and nothing else.
      ctx.ui.setTitle("evil\u0007\u001b]2;payload");
      return "retitled (sanitised)";
    },
  });
};
