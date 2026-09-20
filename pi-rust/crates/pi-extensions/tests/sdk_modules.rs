//! `@earendil-works/*` SDK virtual module tests — LUM-1120.
//!
//! Upstream extensions import pi's own SDK packages (`pi-tui`,
//! `pi-coding-agent`, `pi-ai`, …), not just Node builtins. The shim ships
//! them as virtual modules registered under `@earendil-works/<pkg>`, the
//! historical `@mariozechner/<pkg>` scope and the bare package name. These
//! tests drive that surface through the real host: an extension registered
//! from source, executed through [`JsExtensionHost::execute_tool`], with a
//! scratch directory standing in for the session working tree.
//!
//! The suite checks three things:
//!
//! 1. the modules load and the helpers/components actually work;
//! 2. a name the shim does not implement throws a *named* error instead of
//!    evaluating to `undefined`, and the module protocol names stay inert;
//! 3. every value import in the upstream examples is implemented or a
//!    documented gap — read from `globalThis.__pi_sdk_manifest()` rather
//!    than restated here, so the shim stays the source of truth.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use pi_extensions::{ExtensionEntry, HostOptions, JsExtensionHost, ToolContext};
use serde_json::json;

static SCRATCH_COUNTER: AtomicU32 = AtomicU32::new(0);

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime")
}

/// A unique scratch directory, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let unique = SCRATCH_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "pi_sdk_modules/{}-{name}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Self(dir)
    }

    fn as_str(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn entry_at(id: &str, path: &str) -> ExtensionEntry {
    ExtensionEntry {
        source: PathBuf::from(path),
        id: id.to_string(),
        label: None,
    }
}

async fn host_with_cwd(cwd: &str) -> JsExtensionHost {
    JsExtensionHost::with_options(HostOptions {
        tool_context: ToolContext {
            mode: "print".to_string(),
            has_ui: false,
            cwd: cwd.to_string(),
        },
        ..HostOptions::default()
    })
    .await
    .expect("host")
}

/// One extension exercising the three SDK specifiers plus the alias
/// spellings. Every assertion is packed into one `details` object so a
/// single tool call covers the surface.
#[test]
fn sdk_modules_load_and_exercise_the_upstream_surface() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("surface");
        let host = host_with_cwd(&scratch.as_str()).await;

        let source = r##"
            import {
                Box, CURSOR_MARKER, Container, Key, Markdown, SelectList, SettingsList,
                Spacer, Text, fuzzyFilter, getMarkdownTheme, hyperlink, isKeyRelease,
                matchesKey, parseKey, truncateToWidth, visibleWidth, wrapTextWithAnsi,
            } from "@earendil-works/pi-tui";
            import {
                BorderedLoader, CONFIG_DIR_NAME, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES,
                DynamicBorder, VERSION, convertToLlm, defineTool, formatSize, getAgentDir,
                parseFrontmatter, serializeConversation, stripFrontmatter, truncateHead,
                truncateLine,
            } from "@earendil-works/pi-coding-agent";
            import { StringEnum, Type, calculateCost, contentText, uuidv7 } from "@earendil-works/pi-ai";
            import * as tuiAlias from "@mariozechner/pi-tui";
            import * as bareTui from "pi-tui";
            import { Text as BareText } from "pi-tui";
            import * as compat from "@earendil-works/pi-ai/compat";
            import * as agentCore from "@earendil-works/pi-agent-core";

            export default function (pi) {
                pi.registerTool({
                    name: "sdk_probe",
                    label: "sdk probe",
                    description: "exercises the @earendil-works/* virtual modules",
                    parameters: { type: "object", properties: {} },
                    execute: () => {
                        const rendered = new Text("hello world", 1, 1).render(10);
                        const box = new Box(1, 0);
                        box.addChild(new Text("hi", 0, 0));
                        const container = new Container();
                        container.addChild(new Spacer(2));
                        const md = new Markdown("# Title\n\n- item\n\n`code`", 0, 0, getMarkdownTheme());
                        const list = new SelectList(
                            [{ value: "a", label: "alpha" }, { value: "b", label: "beta" }],
                            5,
                            tuiAlias.getSelectListTheme(),
                        );
                        const settings = new SettingsList(
                            [{ id: "x", label: "X", currentValue: "1", values: ["1", "2"] }],
                            5,
                            tuiAlias.getSettingsListTheme(),
                            function () { settings.changed = true; },
                            function () {},
                        );
                        settings.handleInput("\r");
                        const loader = new BorderedLoader(undefined, undefined, "loading");
                        const border = new DynamicBorder();
                        const fm = parseFrontmatter("---\nname: demo\n---\nbody");
                        const head = truncateHead("a\nb\nc", { maxLines: 2 });
                        const line = truncateLine("abcdef", 3);
                        const usage = { input: 1000000, output: 500000, cacheRead: 0, cacheWrite: 0, cost: {} };
                        const cost = calculateCost(
                            { cost: { input: 3, output: 15, cacheRead: 0, cacheWrite: 0 } },
                            usage,
                        );
                        const tool = defineTool({ name: "t" });
                        const llm = convertToLlm([
                            { role: "bashExecution", command: "ls", output: "f", exitCode: 0, cancelled: false, truncated: false, timestamp: 1 },
                        ]);
                        const uuid = uuidv7(1700000000000);
                        const enumSchema = StringEnum(["a", "b"], { description: "pick" });

                        return {
                            content: [{ type: "text", text: "ok" }],
                            details: {
                                renderedLines: rendered.length,
                                renderedWidth: visibleWidth(rendered[1]),
                                boxWidth: visibleWidth(box.render(10)[0]),
                                containerLines: container.render(5).length,
                                mdHasTitle: md.render(30).join("\n").indexOf("Title") !== -1,
                                mdHasCode: md.render(30).join("\n").indexOf("code") !== -1,
                                listSelected: list.getSelectedItem().value,
                                settingsValue: settings.getSelectedItem().currentValue,
                                settingsChanged: settings.changed === true,
                                loaderSignal: loader.signal instanceof AbortSignal,
                                loaderRendered: loader.render(20).join("\n").indexOf("loading") !== -1,
                                borderWidth: visibleWidth(border.render(4)[0]),
                                fmName: fm.frontmatter.name,
                                fmBody: fm.body,
                                strippedBody: stripFrontmatter("---\na: 1\n---\nbody"),
                                headContent: head.content,
                                headTruncatedBy: head.truncatedBy,
                                headOutputLines: head.outputLines,
                                lineText: line.text,
                                configDir: CONFIG_DIR_NAME,
                                version: VERSION,
                                maxLines: DEFAULT_MAX_LINES,
                                maxBytes: DEFAULT_MAX_BYTES,
                                size512: formatSize(512),
                                size2048: formatSize(2048),
                                agentDirOk: getAgentDir().indexOf("agent") !== -1,
                                costTotal: cost.total,
                                llmRoles: llm.map((m) => m.role),
                                serialized: serializeConversation([{ role: "user", content: "hi" }]),
                                contentText: contentText([{ type: "text", text: "a" }, { type: "image" }, { type: "text", text: "b" }]),
                                uuid,
                                enumType: enumSchema.type,
                                enumValues: enumSchema.enum,
                                typeUnsafe: typeof Type.Unsafe,
                                toolName: tool.name,
                                aliasSameObject: tuiAlias.Text === Text && bareTui.Text === Text && BareText === Text,
                                compatResolves: typeof compat === "object",
                                agentCoreResolves: typeof agentCore === "object",
                                keyUp: Key.up,
                                parsedUp: parseKey("\x1b[A"),
                                matchesDown: matchesKey("\x1b[B", Key.down),
                                releaseFlag: isKeyRelease("\x1b[97;1:3u"),
                                fuzzy: fuzzyFilter([{ n: "alpha" }, { n: "beta" }], "bt", (i) => i.n).map((i) => i.n),
                                truncated: truncateToWidth("hello world", 8),
                                wrapped: wrapTextWithAnsi("aaa bbb ccc", 7),
                                link: hyperlink("x", "http://a"),
                                cursorWidth: visibleWidth(CURSOR_MARKER),
                            },
                        };
                    },
                });
            }
        "##;

        host.load(entry_at("sdk_probe", "/tmp/pi_sdk_modules/surface.mjs"), source)
            .await
            .expect("load sdk probe extension");

        let outcome = host
            .execute_tool("sdk_probe", &json!({}).to_string())
            .await
            .expect("execute sdk probe");
        assert!(!outcome.is_error, "{outcome:?}");
        let d = outcome.details.expect("details");

        // pi-tui components
        assert_eq!(d["renderedLines"], 4, "2 wrapped content lines + paddingY");
        assert_eq!(d["renderedWidth"], 10);
        assert_eq!(d["boxWidth"], 10);
        assert_eq!(d["containerLines"], 2);
        assert_eq!(d["mdHasTitle"], true);
        assert_eq!(d["mdHasCode"], true);
        assert_eq!(d["listSelected"], "a");
        assert_eq!(d["settingsValue"], "2", "Enter advances to the next value");
        assert_eq!(d["settingsChanged"], true);
        assert_eq!(d["loaderSignal"], true);
        assert_eq!(d["loaderRendered"], true);
        assert_eq!(d["borderWidth"], 4);

        // pi-coding-agent helpers
        assert_eq!(d["fmName"], "demo");
        assert_eq!(d["fmBody"], "body");
        assert_eq!(d["strippedBody"], "body");
        assert_eq!(d["headContent"], "a\nb");
        assert_eq!(d["headTruncatedBy"], "lines");
        assert_eq!(d["headOutputLines"], 2);
        assert_eq!(d["lineText"], "abc... [truncated]");
        assert_eq!(d["configDir"], ".pi");
        assert_eq!(d["version"], "0.85.1-pi-rust");
        assert_eq!(d["maxLines"], 2000);
        assert_eq!(d["maxBytes"], 50 * 1024);
        assert_eq!(d["size512"], "512B");
        assert_eq!(d["size2048"], "2.0KB");
        assert_eq!(d["agentDirOk"], true);
        assert_eq!(d["serialized"], "[User]: hi");

        // pi-ai helpers
        assert_eq!(d["costTotal"], 10.5);
        assert_eq!(d["llmRoles"], json!(["user"]));
        assert_eq!(d["contentText"], "a\nb");
        let uuid = d["uuid"].as_str().expect("uuid string");
        assert_eq!(uuid.len(), 36, "uuidv7 shape: {uuid}");
        assert_eq!(uuid.as_bytes()[14], b'7', "uuidv7 version nibble: {uuid}");
        assert!(
            matches!(uuid.as_bytes()[19], b'8' | b'9' | b'a' | b'b'),
            "uuidv7 variant nibble: {uuid}"
        );
        assert!(uuid.starts_with("018bcfe5"), "uuidv7 timestamp: {uuid}");
        assert_eq!(d["enumType"], "string");
        assert_eq!(d["enumValues"], json!(["a", "b"]));
        assert_eq!(d["typeUnsafe"], "function");
        assert_eq!(d["toolName"], "t");

        // every alias spelling is the same object; the gap-only modules resolve
        assert_eq!(d["aliasSameObject"], true);
        assert_eq!(d["compatResolves"], true);
        assert_eq!(d["agentCoreResolves"], true);

        // keys / geometry
        assert_eq!(d["keyUp"], "up");
        assert_eq!(d["parsedUp"], "up");
        assert_eq!(d["matchesDown"], true);
        assert_eq!(d["releaseFlag"], true);
        assert_eq!(d["fuzzy"], json!(["beta"]));
        assert_eq!(d["truncated"], "hello...");
        assert_eq!(d["wrapped"], json!(["aaa bbb", "ccc"]));
        assert_eq!(d["link"], "\u{1b}]8;;http://a\u{1b}\\x\u{1b}]8;;\u{1b}\\");
        assert_eq!(d["cursorWidth"], 0);
    });
}

/// The `pi-ai` event stream and the `pi-ai/compat` provider registry are pure
/// JS upstream (`utils/event-stream.ts` / `compat.ts`), so the shim backs them
/// without a host bridge. This is the surface the `custom-provider-*` upstream
/// examples use to plug a streaming implementation in.
#[test]
fn pi_ai_event_stream_and_compat_registry_match_upstream() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("event-stream");
        let host = host_with_cwd(&scratch.as_str()).await;

        let source = r##"
            import {
                AssistantMessageEventStream, EventStream, createAssistantMessageEventStream,
            } from "@earendil-works/pi-ai";
            import {
                // The built-in provider factories (LUM-1180 / LUM-1204) plus
                // the registry surface: all imported by value so a regression
                // that drops one fails at load, not at a `typeof` check.
                anthropicMessagesApi, azureOpenAIResponsesApi, completeSimple, getApiProvider,
                getApiProviders, googleGenerativeAIApi, openAICompletionsApi, openAIResponsesApi,
                registerApiProvider, registerBuiltInApiProviders,
                resetApiProviders, streamSimple, unregisterApiProviders,
            } from "@earendil-works/pi-ai/compat";

            export default function (pi) {
                pi.registerTool({
                    name: "event_probe",
                    label: "event probe",
                    description: "exercises the pi-ai event stream + compat provider registry",
                    parameters: { type: "object", properties: {} },
                    execute: async () => {
                        const captured = {};

                        // Generic EventStream: completion predicate + result
                        // extractor, async iteration, idempotent push after done.
                        const numbers = new EventStream((n) => n >= 3, (n) => n * 10);
                        const iterator = numbers[Symbol.asyncIterator]();
                        const seen = [];
                        numbers.push(1);
                        numbers.push(2);
                        seen.push((await iterator.next()).value);
                        seen.push((await iterator.next()).value);
                        numbers.push(3);
                        numbers.push(99); // ignored: already complete
                        seen.push((await iterator.next()).value);
                        captured.eventDone = (await iterator.next()).done;
                        captured.eventSeen = seen;
                        captured.eventResult = await numbers.result();

                        // `for await` drains an ended stream.
                        const forAwaited = [];
                        const ended = new AssistantMessageEventStream();
                        ended.end("explicit");
                        for await (const event of ended) forAwaited.push(event);
                        captured.forAwaitCount = forAwaited.length;
                        captured.forAwaitResult = await ended.result();

                        // `createAssistantMessageEventStream` resolves on the
                        // terminal event and exposes the message / error payload.
                        const message = { role: "assistant", content: [{ type: "text", text: "hi" }] };
                        const stream = createAssistantMessageEventStream();
                        stream.push({ type: "text_delta", delta: "hi" });
                        stream.push({ type: "done", reason: "stop", message: message });
                        captured.assistantResultText = (await stream.result()).content[0].text;

                        const errorStream = createAssistantMessageEventStream();
                        errorStream.push({
                            type: "error",
                            reason: "error",
                            error: { role: "assistant", errorMessage: "boom" },
                        });
                        captured.assistantErrorText = (await errorStream.result()).errorMessage;

                        // compat registry: register, look up, stream, complete,
                        // unregister.
                        function makeStream() {
                            const s = createAssistantMessageEventStream();
                            s.push({
                                type: "done",
                                reason: "stop",
                                message: { role: "assistant", content: [{ type: "text", text: "registered" }] },
                            });
                            return s;
                        }
                        // `registerBuiltInApiProviders()` ran at module init,
                        // so every bridged api is already registered.
                        captured.builtinApis = getApiProviders().map((p) => p.api).sort();
                        captured.builtinFactoryTypes = [
                            typeof anthropicMessagesApi,
                            typeof openAIResponsesApi,
                            typeof openAICompletionsApi,
                            typeof googleGenerativeAIApi,
                            typeof azureOpenAIResponsesApi,
                            typeof registerBuiltInApiProviders,
                            typeof resetApiProviders,
                        ];
                        captured.builtinStreamReturn = Object.keys(anthropicMessagesApi()).sort();
                        registerApiProvider({ api: "shim-probe-api", stream: makeStream, streamSimple: makeStream }, "probe");
                        const model = { id: "probe-model", provider: "probe", api: "shim-probe-api" };
                        captured.streamText = (await streamSimple(model, { messages: [] }, {}).result()).content[0].text;
                        captured.completeText = (await completeSimple(model, { messages: [] }, {})).content[0].text;
                        captured.registryApi = getApiProvider("shim-probe-api").api;
                        captured.registryCount = getApiProviders().length;

                        let mismatch = null;
                        try {
                            getApiProvider("shim-probe-api").streamSimple(
                                { id: "x", provider: "p", api: "other-api" },
                                { messages: [] },
                                {},
                            );
                        } catch (err) {
                            mismatch = err.message;
                        }
                        captured.mismatch = mismatch;

                        let missing = null;
                        try {
                            streamSimple({ id: "x", provider: "p", api: "nope-api" }, { messages: [] }, {});
                        } catch (err) {
                            missing = err.message;
                        }
                        captured.missing = missing;

                        unregisterApiProviders("probe");
                        captured.registryAfterUnregister = getApiProviders().length;

                        // `resetApiProviders()` is the documented hard reset:
                        // everything goes, then the builtins come back.
                        registerApiProvider({ api: "shim-probe-api", stream: makeStream, streamSimple: makeStream }, "probe");
                        captured.probeBeforeReset = getApiProviders().length;
                        resetApiProviders();
                        captured.afterResetApis = getApiProviders().map((p) => p.api).sort();

                        return { content: [{ type: "text", text: "ok" }], details: captured };
                    },
                });
            }
        "##;

        host.load(entry_at("event_probe", "/tmp/pi_sdk_modules/events.mjs"), source)
            .await
            .expect("load event probe extension");

        let outcome = host
            .execute_tool("event_probe", &json!({}).to_string())
            .await
            .expect("execute event probe");
        assert!(!outcome.is_error, "{outcome:?}");
        let d = outcome.details.expect("details");

        assert_eq!(d["eventSeen"], json!([1, 2, 3]));
        assert_eq!(d["eventDone"], true);
        assert_eq!(d["eventResult"], 30, "extractResult runs on the terminal event");
        assert_eq!(d["forAwaitCount"], 0, "an ended stream has no pending events");
        assert_eq!(d["forAwaitResult"], "explicit", "end(result) resolves the result");
        assert_eq!(d["assistantResultText"], "hi");
        assert_eq!(d["assistantErrorText"], "boom");
        assert_eq!(d["streamText"], "registered");
        assert_eq!(d["completeText"], "registered");
        assert_eq!(d["registryApi"], "shim-probe-api");
        assert_eq!(d["registryCount"], 6, "five builtins + the probe");
        assert_eq!(
            d["builtinApis"],
            json!([
                "anthropic-messages",
                "azure-openai-responses",
                "google-generative-ai",
                "openai-completions",
                "openai-responses",
            ]),
            "registerBuiltInApiProviders runs at module init"
        );
        assert_eq!(
            d["builtinFactoryTypes"],
            json!(["function", "function", "function", "function", "function", "function", "function"]),
            "every bridged builtin provider factory is implemented"
        );
        assert_eq!(
            d["builtinStreamReturn"],
            json!(["stream", "streamSimple"]),
            "a builtin api hands back a ProviderStreams"
        );
        assert_eq!(
            d["mismatch"], "Mismatched api: other-api expected shim-probe-api",
            "a registered provider rejects a model from another api"
        );
        assert_eq!(d["missing"], "No API provider registered for api: nope-api");
        assert_eq!(
            d["registryAfterUnregister"], 5,
            "the builtins survive"
        );
        assert_eq!(d["probeBeforeReset"], 6);
        assert_eq!(
            d["afterResetApis"],
            json!([
                "anthropic-messages",
                "azure-openai-responses",
                "google-generative-ai",
                "openai-completions",
                "openai-responses",
            ]),
            "resetApiProviders() clears overrides and re-registers the builtins"
        );
    });
}

/// The hard rule: a name the shim does not implement throws a named error,
/// an unknown name throws "has no export", and the JS-internal protocol
/// names stay `undefined` so module interop keeps working.
#[test]
fn sdk_gaps_and_unknown_exports_throw_named_errors() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("gaps");
        let host = host_with_cwd(&scratch.as_str()).await;

        let source = r##"
            import * as coding from "@earendil-works/pi-coding-agent";
            import * as ai from "@earendil-works/pi-ai";
            import * as compat from "@earendil-works/pi-ai/compat";
            import * as gondolin from "@earendil-works/gondolin";

            export default function (pi) {
                pi.registerTool({
                    name: "gap_probe",
                    label: "gap probe",
                    description: "checks the unimplemented / unknown export errors",
                    parameters: { type: "object", properties: {} },
                    execute: (args, ctx) => {
                        const captured = {};
                        try {
                            coding.noSuchHelper;
                        } catch (err) {
                            captured.unknownCode = err.code;
                        }
                        captured.streamFactoryType = typeof ai.createAssistantMessageEventStream;
                        captured.compatFactoryTypes = [
                            typeof compat.anthropicMessagesApi,
                            typeof compat.openAIResponsesApi,
                            typeof compat.registerBuiltInApiProviders,
                            typeof compat.resetApiProviders,
                        ];
                        try {
                            compat.googleVertexApi;
                        } catch (err) {
                            captured.streamCode = err.code;
                            captured.streamExportName = err.exportName;
                            captured.streamNamesDoc = err.message.indexOf("SDK_MODULES.md") !== -1;
                            captured.streamReasonNamesAdapter = err.message.indexOf("google-vertex") !== -1;
                        }
                        try {
                            gondolin.VM;
                        } catch (err) {
                            captured.gondolinCode = err.code;
                        }
                        let customIsHandle = false;
                        try {
                            const handle = ctx.ui.custom(function () { return null; });
                            customIsHandle =
                                !!handle &&
                                typeof handle.resolve === "function" &&
                                typeof handle.isVisible === "function" &&
                                typeof handle.then === "function";
                        } catch (err) {
                            captured.customCode = err.code;
                        }
                        let widgetThrew = false;
                        try {
                            ctx.ui.setWidget("x", function () { return null; });
                        } catch (err) {
                            widgetThrew = true;
                        }
                        return {
                            content: [{ type: "text", text: "ok" }],
                            details: {
                                ...captured,
                                customIsHandle,
                                widgetThrew,
                                themeStyled: ctx.ui.theme.fg("accent", "text"),
                                protocolInert:
                                    coding.then === undefined &&
                                    coding.toJSON === undefined &&
                                    coding.default === coding &&
                                    Object.keys(coding).indexOf("defineTool") !== -1,
                                factoriesInKeys: [
                                    "createBashTool",
                                    "createEditTool",
                                    "createFindTool",
                                    "createGrepTool",
                                    "createLsTool",
                                    "createReadTool",
                                    "createWriteTool",
                                ].every((name) => Object.keys(coding).indexOf(name) !== -1),
                                factoryTypes: [
                                    typeof coding.createReadTool,
                                    typeof coding.createBashTool,
                                ],
                            },
                        };
                    },
                });
            }
        "##;

        host.load(
            entry_at("gap_probe", "/tmp/pi_sdk_modules/gaps.mjs"),
            source,
        )
        .await
        .expect("load gap probe extension");

        let outcome = host
            .execute_tool("gap_probe", &json!({}).to_string())
            .await
            .expect("execute gap probe");
        assert!(!outcome.is_error, "{outcome:?}");
        let d = outcome.details.expect("details");

        assert_eq!(d["unknownCode"], "ERR_PI_SDK_UNKNOWN_EXPORT");
        assert_eq!(d["streamCode"], "ERR_PI_SDK_UNIMPLEMENTED");
        assert_eq!(d["streamExportName"], "googleVertexApi");
        assert_eq!(d["streamNamesDoc"], true);
        assert_eq!(
            d["streamReasonNamesAdapter"], true,
            "a remaining gap names the missing adapter instead of a flat 'not implemented'"
        );
        assert_eq!(
            d["streamFactoryType"], "function",
            "createAssistantMessageEventStream is implemented now"
        );
        assert_eq!(
            d["compatFactoryTypes"],
            json!(["function", "function", "function", "function"]),
            "the bridged builtin provider factories are callable"
        );
        assert_eq!(d["gondolinCode"], "ERR_PI_SDK_UNIMPLEMENTED");
        assert_eq!(
            d["customCode"],
            serde_json::Value::Null,
            "custom no longer throws: it returns a handle and resolves in non-interactive mode"
        );
        assert_eq!(
            d["customIsHandle"], true,
            "custom returns the CustomHandle shape"
        );
        assert_eq!(d["widgetThrew"], false, "setWidget is an inert no-op");
        assert_eq!(
            d["themeStyled"], "text",
            "theme helpers are identity functions"
        );
        assert_eq!(d["protocolInert"], true);
        assert_eq!(
            d["factoriesInKeys"], true,
            "the built-in tool factories are enumerable implementations now"
        );
        assert_eq!(
            d["factoryTypes"],
            json!(["function", "function"]),
            "create*Tool are callable"
        );
    });
}

/// A *top-level* value import of an unimplemented name fails the load with
/// the same readable error — the extension never half-initialises with a
/// `undefined` binding.
#[test]
fn top_level_gap_import_fails_the_load() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("top-level-gap");
        let host = host_with_cwd(&scratch.as_str()).await;

        let source = r##"
            import { googleVertexApi } from "@earendil-works/pi-ai/compat";
            export default function () {}
        "##;

        let error = host
            .load(
                entry_at("bad_import", "/tmp/pi_sdk_modules/bad.mjs"),
                source,
            )
            .await
            .expect_err("load must fail");
        let message = format!("{error}");
        assert!(
            message.contains("googleVertexApi"),
            "error names the export: {message}"
        );
        assert!(
            message.contains("@earendil-works/pi-ai/compat"),
            "error names the specifier: {message}"
        );
    });
}

/// The SDK inventory as JSON, read out of the running shim.
async fn sdk_manifest(host: &JsExtensionHost) -> serde_json::Value {
    let source = r##"
        export default function (pi) {
            pi.registerTool({
                name: "sdk_manifest",
                label: "sdk manifest",
                description: "returns the shim's SDK module inventory",
                parameters: { type: "object", properties: {} },
                execute: () => ({
                    content: [{ type: "text", text: "ok" }],
                    details: { manifest: globalThis.__pi_sdk_manifest() },
                }),
            });
        }
    "##;
    host.load(
        entry_at("manifest", "/tmp/pi_sdk_modules/manifest.mjs"),
        source,
    )
    .await
    .expect("load manifest probe");
    let outcome = host
        .execute_tool("sdk_manifest", &json!({}).to_string())
        .await
        .expect("execute manifest probe");
    assert!(!outcome.is_error, "{outcome:?}");
    let details = outcome.details.expect("details");
    let raw = details["manifest"].as_str().expect("manifest string");
    serde_json::from_str(raw).expect("manifest JSON")
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

fn is_pi_sdk_specifier(specifier: &str) -> bool {
    specifier.starts_with("@earendil-works/") || specifier.starts_with("@mariozechner/")
}

/// Collect `(specifier, value names)` for every `import … from
/// "@earendil-works/…"` / `"@mariozechner/…"` in a source file.
///
/// `import type { … }` and inline `type X` specifiers are skipped: the
/// loader erases them, so they never bind at runtime.
fn sdk_value_imports(source: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let bytes = source.as_bytes();
    let mut cursor = 0usize;
    while let Some(rel) = source[cursor..].find("import") {
        let start = cursor + rel;
        cursor = start + "import".len();
        // A word boundary before, and whitespace / `{` / `*` after.
        if start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
            continue;
        }
        let after = &source[start + "import".len()..];
        if !(after.starts_with(char::is_whitespace)
            || after.starts_with('{')
            || after.starts_with('*'))
        {
            continue;
        }
        // Bound the search to one statement so a missing `from` cannot
        // swallow the rest of the file.
        let window_len = after.len().min(4096);
        let window = &after[..window_len];
        let Some(from_rel) = window.find("from") else {
            continue;
        };
        let rest = window[from_rel + "from".len()..].trim_start();
        let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            continue;
        };
        let spec_rest = &rest[quote.len_utf8()..];
        let Some(end) = spec_rest.find(quote) else {
            continue;
        };
        let specifier = &spec_rest[..end];
        if !is_pi_sdk_specifier(specifier) {
            continue;
        }

        let clause = &source[start + "import".len()..start + "import".len() + from_rel];
        let clause = clause.trim();
        if clause.starts_with("type ") {
            // Whole statement is type-only.
            continue;
        }
        let Some(open) = clause.find('{') else {
            // `import * as ns from` / default import: no named bindings.
            continue;
        };
        let Some(close) = clause[open + 1..].find('}') else {
            continue;
        };
        let names = out.entry(specifier.to_string()).or_default();
        for part in clause[open + 1..open + 1 + close].split(',') {
            let part = part.trim();
            if part.is_empty() || part.starts_with("type ") {
                continue;
            }
            if let Some(name) = part.split_whitespace().next() {
                if !name.is_empty() {
                    names.insert(name.to_string());
                }
            }
        }
    }
    out
}

fn manifest_names(manifest: &serde_json::Value, specifier: &str, key: &str) -> BTreeSet<String> {
    manifest
        .get(specifier)
        .and_then(|entry| entry.get(key))
        .and_then(|value| value.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Compatibility gate: every value the upstream examples import from a pi
/// SDK package is either implemented or a documented gap in the shim.
///
/// The implemented / gap sets come from `globalThis.__pi_sdk_manifest()`
/// (the shim is the source of truth), and the scan is skipped when the
/// upstream example directories are not part of the checkout.
#[test]
fn upstream_sdk_imports_are_all_bridged_or_documented() {
    let runtime = rt();
    runtime.block_on(async {
        let scratch = Scratch::new("upstream");
        let host = host_with_cwd(&scratch.as_str()).await;
        let manifest = sdk_manifest(&host).await;

        // Canonical specifiers and their two aliases must all be present.
        for canonical in [
            "@earendil-works/pi-tui",
            "@earendil-works/pi-coding-agent",
            "@earendil-works/pi-ai",
            "@earendil-works/pi-ai/compat",
            "@earendil-works/pi-agent-core",
            "@earendil-works/gondolin",
        ] {
            assert!(
                manifest.get(canonical).is_some(),
                "`{canonical}` is not registered in the shim"
            );
            let bare = canonical
                .strip_prefix("@earendil-works/")
                .expect("canonical scope");
            for alias in [format!("@mariozechner/{bare}"), bare.to_string()] {
                assert_eq!(
                    manifest.get(&alias),
                    manifest.get(canonical),
                    "`{alias}` must share `{canonical}`'s inventory"
                );
            }
        }

        // `crates/pi-extensions` → the repository root, where the upstream
        // examples live.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let mut scanned_files = 0usize;
        let mut upstream: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for dir in [
            ".pi/extensions",
            "packages/coding-agent/examples/extensions",
        ] {
            let dir = root.join(dir);
            if !dir.is_dir() {
                continue;
            }
            for entry in walk(&dir) {
                let is_source = entry
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| matches!(ext, "ts" | "mts" | "js" | "mjs"));
                if !is_source {
                    continue;
                }
                scanned_files += 1;
                let source = std::fs::read_to_string(&entry).expect("read upstream example");
                for (specifier, names) in sdk_value_imports(&source) {
                    upstream.entry(specifier).or_default().extend(names);
                }
            }
        }
        if scanned_files == 0 {
            // Nothing to compare against in this checkout.
            return;
        }

        let documented = include_str!("../docs/SDK_MODULES.md");
        for (specifier, names) in &upstream {
            let entry = manifest.get(specifier).unwrap_or_else(|| {
                panic!("upstream examples import `{specifier}`, which the shim does not register")
            });
            assert!(
                entry.get("implemented").is_some() && entry.get("unimplemented").is_some(),
                "`{specifier}` manifest entry is malformed: {entry}"
            );
            let implemented = manifest_names(&manifest, specifier, "implemented");
            let gaps = manifest_names(&manifest, specifier, "unimplemented");
            for name in names {
                assert!(
                    implemented.contains(name) || gaps.contains(name),
                    "upstream examples import `{name}` from `{specifier}`, which the shim neither \
                     implements nor documents as a gap"
                );
                if gaps.contains(name) {
                    assert!(
                        documented.contains(name),
                        "`{name}` is a documented gap for `{specifier}` but docs/SDK_MODULES.md \
                         does not name it"
                    );
                }
            }
        }

        // The known unbridged third-party package stays a gap, never an
        // implementation.
        let gondolin = manifest
            .get("@earendil-works/gondolin")
            .expect("gondolin registered");
        assert!(
            manifest_names(&manifest, "@earendil-works/gondolin", "implemented").is_empty(),
            "gondolin must not claim implemented exports: {gondolin}"
        );
        assert!(
            documented.contains("gondolin"),
            "docs/SDK_MODULES.md must document the gondolin gap"
        );

        // The tool factories are wired to the host's built-in bundle as of
        // LUM-1175; the upstream examples import all seven, so a regression
        // that drops one from the implemented set must fail here.
        let factories = manifest_names(&manifest, "@earendil-works/pi-coding-agent", "implemented");
        for factory in [
            "createReadTool",
            "createWriteTool",
            "createEditTool",
            "createBashTool",
            "createGrepTool",
            "createFindTool",
            "createLsTool",
        ] {
            assert!(
                factories.contains(factory),
                "`{factory}` must be an implemented export"
            );
        }

        assert!(
            upstream.len() >= 3 && upstream.values().map(BTreeSet::len).sum::<usize>() >= 15,
            "expected several SDK imports in the upstream examples, found {upstream:?}"
        );
    });
}
