import { defineConfig } from "vite";

// Minimal Vite config — the host page serves a single `index.html` that
// imports the wasm-pack-built `pi-agent-core` package from
// `../../crates/pi-agent-core/pkg`. `optimizeDeps.exclude` keeps Vite
// from trying to pre-bundle the .wasm itself; the package's
// generated `pi_agent_core.js` is responsible for lazy-instantiating
// the WebAssembly module when `init()` is awaited.
export default defineConfig({
  server: {
    port: 5173,
    strictPort: true,
  },
  optimizeDeps: {
    exclude: ["pi-agent-core"],
  },
  build: {
    target: "es2022",
  },
});
