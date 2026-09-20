//! Real-runner coverage for the JS `create*Tool` factories — LUM-1175.
//!
//! `pi-extensions/tests/builtin_tool_factories.rs` covers the bridge with a
//! fake [`BuiltinToolRunner`]; it cannot prove the wiring inside
//! `pi-coding-agent` actually reaches the *real* built-in bundle. This test
//! loads an extension through `wiring::load` — the same path `pi --print`
//! takes — and re-registers `read` / `bash` through the factories:
//!
//! * `createReadTool(dir).execute(id, { path: "hello.txt" })` must return
//!   the file's real bytes, i.e. the extension delegated to the same
//!   `ReadTool` the model calls, with `dir` honoured as the base for the
//!   relative path;
//! * `createBashTool(dir)` must run through the real `BashTool` and report a
//!   failing command as a structured `isError` result, not a host-level
//!   rejection.
//!
//! The test spawns a real shell, so it is Unix-only.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use pi_coding_agent::extensions::wiring::{self, ExtensionLoadOptions};
use pi_protocol::{Content, ToolCall};
use serde_json::json;
use tokio_util::sync::CancellationToken;

static SCRATCH_COUNTER: AtomicU32 = AtomicU32::new(0);

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime")
}

fn scratch_dir(name: &str) -> PathBuf {
    let unique = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "pi_coding_agent_builtin_tool_factories/{}-{name}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// The extension re-registers a built-in `read` and `bash` via the
/// factories. Every observable value is derived from a real tool run.
const EXTENSION: &str = r##"
    import { createBashTool, createReadTool } from "@earendil-works/pi-coding-agent";

    export default function (pi) {
        pi.registerTool({
            name: "factory_probe",
            label: "Factory probe",
            description: "Reads a file and runs a command through the create*Tool factories.",
            parameters: {
                type: "object",
                properties: { dir: { type: "string" } },
                required: ["dir"],
            },
            execute: async function (args) {
                const dir = args && args.dir ? args.dir : ".";
                const read = createReadTool(dir);
                const readResult = await read.execute("call-read", { path: "hello.txt" });
                const text = readResult.content && readResult.content[0]
                    ? String(readResult.content[0].text)
                    : "";

                const bash = createBashTool(dir);
                const bashResult = await bash.execute("call-bash", { command: "exit 3" });
                const bashText = bashResult.content && bashResult.content[0]
                    ? String(bashResult.content[0].text)
                    : "";

                return {
                    content: [{ type: "text", text: text }],
                    details: {
                        readName: read.name,
                        readIsError: readResult.isError === true,
                        readDetailsIsNull: readResult.details === null,
                        bashName: bash.name,
                        bashIsError: bashResult.isError === true,
                        bashReportedFailure: bashText.length > 0,
                    },
                };
            },
        });
    }
"##;

#[test]
fn create_tool_factories_delegate_to_the_real_builtin_bundle() {
    let rt = runtime();
    let dir = scratch_dir("real");

    // A file the real `read` tool must return, and a script that fails so
    // `bash` exercises its error path.
    std::fs::write(dir.join("hello.txt"), "line one\nline two\n").expect("write fixture");
    let extension_path = dir.join("probe.mjs");
    std::fs::write(&extension_path, EXTENSION).expect("write extension");

    let mut options = ExtensionLoadOptions::for_mode(None, dir.clone(), "print", false);
    options.explicit = vec![extension_path.clone()];
    let loaded = wiring::load(&rt, &options);
    assert!(loaded.errors.is_empty(), "errors: {:?}", loaded.errors);
    assert!(
        loaded.tools.iter().any(|name| name == "factory_probe"),
        "extension tool was not registered: {:?}",
        loaded.tools
    );

    let call = ToolCall {
        id: "call-probe".to_string(),
        name: "factory_probe".to_string(),
        arguments: json!({ "dir": dir.to_string_lossy() }),
    };
    let result = rt
        .block_on(loaded.executor.execute(&call, CancellationToken::new()))
        .expect("execute factory probe");
    assert!(!result.is_error, "{result:?}");

    let Content::Text(text) = &*result.content else {
        panic!("expected a text result, got {:?}", result.content);
    };
    assert!(
        text.text.contains("line one") && text.text.contains("line two"),
        "createReadTool must return the file's real contents: {:?}",
        text.text
    );

    let details = result.details.expect("details");
    assert_eq!(details["readName"], json!("read"));
    assert_eq!(details["readIsError"], json!(false));
    assert_eq!(
        details["readDetailsIsNull"],
        json!(true),
        "an untruncated read carries no details"
    );
    assert_eq!(details["bashName"], json!("bash"));
    assert_eq!(
        details["bashIsError"],
        json!(true),
        "`exit 3` must surface as a structured tool error"
    );
    assert_eq!(details["bashReportedFailure"], json!(true));

    let _ = std::fs::remove_dir_all(&dir);
}
