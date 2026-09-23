//! `User-Agent` string — port of `packages/ai/src/utils/pi-user-agent.ts`.
//!
//! Every provider request identifies the client as `pi`; upstream reads
//! `node:os` (`platform()` / `release()` / `arch()`) and falls back to
//! `pi (browser)` when no Node/Bun runtime is present, which keeps the browser
//! bundle from importing `node:os` at the top level.
//!
//! # Deliberate differences from the TypeScript implementation
//!
//! * **Platform names are Node's, not `std`'s.** `std::env::consts::OS`
//!   reports `macos` / `windows` where Node reports `darwin` / `win32`
//!   (`linux` is the same). The user agent is a wire value that upstream
//!   servers may match on, so the Node spelling is mapped back in
//!   [`get_pi_user_agent`] and the output stays byte-identical to upstream's.
//! * **The OS release is best-effort.** There is no `std` API for it. On Linux
//!   the kernel's `osrelease` is read from `/proc/sys/kernel/osrelease` (the
//!   same source `pi-extensions`' `node:os` shim uses); on every other OS the
//!   release is `unknown`. Tests therefore assert the *shape*
//!   (`pi (<platform> <release>; <arch>)`), not a machine-specific release.
//! * **WASM is a compile-time branch, not a runtime probe.** The WASM build
//!   always reports [`WASM_USER_AGENT`] (`pi (browser)`); the native build
//!   never can. The constant is public so a native test can pin the WASM
//!   spelling without a `wasm32` toolchain.

/// The `User-Agent` the WASM build reports (`getPiUserAgent` browser branch).
pub const WASM_USER_AGENT: &str = "pi (browser)";

/// `getPiUserAgent()` — `pi (<platform> <release>; <arch>)` on native,
/// `pi (browser)` on WASM.
#[cfg(not(target_arch = "wasm32"))]
pub fn get_pi_user_agent() -> String {
    format!(
        "pi ({} {}; {})",
        node_platform(),
        os_release(),
        std::env::consts::ARCH
    )
}

/// `getPiUserAgent()` — the browser build has no `node:os` equivalent.
#[cfg(target_arch = "wasm32")]
pub fn get_pi_user_agent() -> String {
    WASM_USER_AGENT.to_string()
}

/// `std::env::consts::OS` spelling → Node's `os.platform()` spelling.
#[cfg(not(target_arch = "wasm32"))]
fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// `os.release()` — best-effort kernel release; `unknown` where it cannot be
/// read without pulling in a platform crate.
#[cfg(not(target_arch = "wasm32"))]
fn os_release() -> String {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|release| release.trim().to_string())
            .ok()
            .filter(|release| !release.is_empty())
            .unwrap_or_else(|| "unknown".to_string())
    }
    #[cfg(not(target_os = "linux"))]
    {
        "unknown".to_string()
    }
}
