# LUM-1448 — 扩展注入 autocomplete provider（`ctx.ui.addAutocompleteProvider` → `#` 触发符面）

> scope: `pi-rust/`（分支 `work/LUM-1448`，基线 `origin/feature/pi.rs` = `c6d6df108` = LUM-1447）
> 改动面：`pi-tui` + `pi-extensions` + `pi-coding-agent`（与在飞的 LUM-1434 的
> `pi-coding-agent/src/cli/` 无重叠）
> 交付物：`crates/pi-tui/src/autocomplete.rs`、`crates/pi-tui/src/editor.rs`、
> `crates/pi-extensions/src/autocomplete.rs`（新）、`crates/pi-extensions/src/host.rs`、
> `crates/pi-extensions/runtime/pi-ext-shim.mjs`、
> `crates/pi-extensions/examples/issue-autocomplete.mjs`（新，真实 fixture）、
> `crates/pi-coding-agent/src/extensions/autocomplete.rs`（新）、
> `crates/pi-coding-agent/src/extensions/wiring.rs`、`crates/pi-coding-agent/src/interactive.rs`、
> 三个新测试文件、三张真帧截图、`RUST_TS_PARITY_METRICS.md` §0.15 + §3.6、本文。

## 0. TL;DR

- **开工前必须先定的那件事（异步化范围）：选「宿主侧有界阻塞式 host call + 第一期只接受同步返回的
  provider」**，即 issue 选项 2 的桥 + 选项 3 的限制。理由、被否掉的两个方案与线程模型对齐见 §2。
  一句话：`pi-tui::AutocompleteProvider` 保持同步 trait（上游是 `async` + `AbortSignal`），
  Rust→JS 的调用走 `block_in_place` + `Handle::block_on` 且有 250ms 上限，JS 回调若返回 Promise
  **不 await**，这次调用回落内置 provider 并只告警一次。
- **做出来的东西**：`ctx.ui.addAutocompleteProvider(factory)` 真的可用——wrapper 链活在 shim 里
  （每个 factory 拿到它叠在谁身上的那个 `current`，与 `interactive-mode.ts:734-745` 同构），
  链的 `current` 是宿主注入的内置 `CombinedAutocompleteProvider`（经同步 `host_ui_autocomplete`
  import 回调），链的 `triggerCharacters` 去重后进编辑器触发表（与 `:736-743` 同构），
  `#` 真正出候选、Tab/Enter 真正落地。
- **端到端证据**：`crates/pi-extensions/examples/issue-autocomplete.mjs`（上游
  `github-issue-autocomplete.ts` 的移植，去 `gh`/网络）在真 `JsExtensionHost` + 真 `App` 里驱动，
  3 帧 frame-buffer（§4.3）+ 6 条行为断言（§4.2）。**不是自造 fixture 同义反复**：fixture 的 provider
  形状逐字取自上游示例（`triggerCharacters: ["#"]`、`/(?:^|[ \t])#([^\s#]*)$/`、
  `{value,label,description}`、`current.*` 委托、`shouldTriggerFileCompletion?.(...) ?? true`），
  断言打在**渲染帧**与**草稿文本**上（与 shim/Rust 实现无关的观测面），另有 8 条宿主层断言
  （§4.1）分别验证「扩展自己答」与「扩展委托给内置 provider」两条方向。
- **实测**：`pi-tui` **1034 passed / 0 failed**（基线 1027/0，**+7**）；
  `pi-extensions` **131 / 5**（基线 120/5，**+11 通过、失败集逐字不变**）；
  `pi-coding-agent` **823 / 28**（基线 815/28，**+8 通过、失败集逐字不变**）；
  `cargo fmt --all -- --check` 干净；`cargo clippy` 三个 crate `--all-targets` **0 条新告警**
  （只剩基线就有的 `pi-extensions` `signal_name` 死代码 1 条 + vendor `rquickjs-core` 13 条）。
  28/5 条失败是 Windows 环境类（路径分隔符 / `ls` / `bash`），逐条与基线同名，见 §7。

## 1. 上游对照（逐条）

### 1.1 API 形状

| 上游 | 位置 | 本端口 | 状态 |
|---|---|---|---|
| `ExtensionUIContext.addAutocompleteProvider(factory)` | `core/extensions/types.ts:226-227` | shim `ui.addAutocompleteProvider(factory)`（`pi-ext-shim.mjs:897`） | ✅ |
| `type AutocompleteProviderFactory = (current: AutocompleteProvider) => AutocompleteProvider` | `core/extensions/types.ts:126` | `pi_tui::autocomplete::AutocompleteProviderFactory`（`autocomplete.rs:296`） | ✅ |
| 注册即重建链 `wrappers.push(factory); setupAutocompleteProvider()` | `interactive-mode.ts:2449-2452` | `__pi_autocomplete_register`：push → 立即 rebuild → 通知宿主 generation+1（`pi-ext-shim.mjs:555`） | ✅（多了 generation 通知，见 §2） |
| 文档示例（`#2983`/`#2753` 两条候选） | `docs/extensions.md:2696-2745` | `pi-extensions/tests/autocomplete.rs` 的断言照着这两条写 | ✅ |
| 完整示例 `github-issue-autocomplete.ts`（预载 + 本地过滤） | `examples/extensions/github-issue-autocomplete.ts` | `examples/issue-autocomplete.mjs`（去 `gh`，见 §1.5） | ✅ 子集 |

### 1.2 wrapper 链与触发符去重（`interactive-mode.ts:734-745`）

上游：

```ts
private setupAutocompleteProvider(): void {
  let provider = this.createBaseAutocompleteProvider();
  const triggerCharacters: string[] = [];
  for (const wrapProvider of this.autocompleteProviderWrappers) {
    provider = wrapProvider(provider);                      // current 透传
    triggerCharacters.push(...(provider.triggerCharacters ?? []));
  }
  if (triggerCharacters.length > 0) {
    provider.triggerCharacters = [...new Set(triggerCharacters)];   // 去重
  }
  this.autocompleteProvider = provider;
  this.defaultEditor.setAutocompleteProvider(provider);
}
```

本端口分两层落地，语义逐条对齐：

- **`pi-tui` 的 Rust 组合器**（`autocomplete.rs:296,306,376`）：
  `compose_autocomplete_providers(base, &factories)` 按注册序 `provider = factory(provider)`，
  收集每个 wrapper 的 `trigger_characters()` 后**首次出现去重**（`Vec::contains` 顺序保持，
  等价 `[...new Set()]` 的插入序语义），非空则包一层 `TriggeredAutocompleteProvider`
  覆盖触发表——上游是在最外层 provider 上赋值，Rust 的 trait 返回借用切片，所以用一层薄包装等价表达。
  `base` 本身不贡献触发符（上游 `CombinedAutocompleteProvider` 也没有 `triggerCharacters`），逐字一致。
- **JS 侧的链**（`pi-ext-shim.mjs:439,506,555`）：wrapper 是 JS 闭包，无法搬到 Rust，所以链活在 shim：
  `__pi_autocomplete_rebuild()` 用 `__pi_autocomplete_base_provider` 起头，逐个 `factory(provider)`，
  收集 + `Array.from(new Set(...))` 去重，并把去重结果**同时**赋回最外层 provider（上游形状）
  和返回给宿主（宿主侧真正生效的触发表）。`compose_autocomplete_providers` 在
  `pi-coding-agent` 侧仍被使用：`compose_provider`（`extensions/autocomplete.rs:364`）用一个 Rust
  factory 把 JS 链包在 base 之上，触发表由同一个组合器算出来。

### 1.3 编辑器触发符表（`editor.ts:251,407-408,2329-2340`）

上游 `setAutocompleteProvider` → `setAutocompleteTriggerCharacters(provider.triggerCharacters ?? [])`，
后者**从 `DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS = ["@","#"]` 重建**，逐项过滤
（长度 ≠1 / `/` / 空白 / 重复都跳过）。

Rust `Editor::set_autocomplete_provider`（`editor.rs:1766`）本轮补上「先重建为默认值」这一步：
之前是在已有表上累加，重新安装（re-compose）一条链会留下旧触发符。现在逐条对应上游的过滤规则
（`is_valid_trigger_character` = `c != '/' && !c.is_whitespace()`），并新增只读
`Editor::autocomplete_trigger_characters()`（`editor.rs:1782`）作为可观测面。

### 1.4 provider 语义

| 上游 | 本端口 | 备注 |
|---|---|---|
| `getSuggestions(lines, cursorLine, cursorCol, options) => Promise<Suggestions \| null>` | shim `_pi_autocomplete_call("getSuggestions", …)` → `{items,prefix}`；`null` 原样回传 | 异步→同步限制见 §2 |
| `applyCompletion(lines, cursorLine, cursorCol, item, prefix) => {lines,cursorLine,cursorCol}` | `_pi_autocomplete_call("applyCompletion", …)` | 返回值不是合法 completion（无 `lines` 数组）时回落内置 provider，避免清空草稿 |
| `shouldTriggerFileCompletion?(lines, cursorLine, cursorCol) => boolean` | 缺省 → `true`；wrapper 若委托则走 `current` | 与 `?? true` 一致 |
| `options = { signal, force }` | `{ signal: <never-aborted>, force }` | `signal` 是常量（见 §5） |
| 内置 provider 无 `#` 分支 | 逐字一致（`LUM1436 §1.2` 已核对）；`#` 只是触发符，语义完全由扩展提供 | ✅ 本轮仍未给 `#` 加内置语义 |

### 1.5 fixture 与上游示例的差异（诚实清单）

`crates/pi-extensions/examples/issue-autocomplete.mjs` 相对
`packages/coding-agent/examples/extensions/github-issue-autocomplete.ts`：

1. **`gh issue list` → 本地常量**。上游用 `pi.exec("gh", ["issue","list",…])` 预载 open issues；
   本 fixture 用 6 条本地 issue（含文档示例里的 `#2983` / `#2753`），因此不需要网络、`gh`、git 仓库。
2. **`async getSuggestions` → 同步 `getSuggestions`**。上游是 `async`（要 await 上面的 `pi.exec`）；
   本端口第一期只服务同步回调（§2），所以直接返回对象。
3. 过滤函数是本地实现（上游 `import { fuzzyFilter } from "@earendil-works/pi-tui"`）——数字前缀分支
   逐字照抄，非数字分支用 `includes` 近似。
4. **其余逐字保留**：`triggerCharacters: ["#"]`、`(?:^|[ \t])#([^\s#]*)$/`、`prefix = "#" + token`、
   item 形状、`current.getSuggestions/applyCompletion` 委托、`shouldTriggerFileCompletion?.(…) ?? true`、
   `options.signal.aborted` 检查、`session_start` 注册。

## 2. 线程模型决策（issue 要求开工前先定）

**结论：选「宿主侧有界阻塞式 host call（选项 2 的桥）+ 第一期只接受同步返回的 provider（选项 3 的限制）」。**

被否掉的方案：

1. **编辑器补全改成可等待（选项 1）** —— 最贴近上游，但要给 `pi-tui` 的 provider trait 引入
   async（或 pending 状态机）并把消费点搬进帧循环：`Editor::request_autocomplete` /
   `handle_tab_completion` / `accept_autocomplete` / App 渲染全部要改成「先出框架、候选下一帧补齐」。
   代价是整条补全路径重写 + 现有 7 个 autocomplete 测试与 4 个帧测试的语义变化（上下键/Tab 的时序），
   而收益只是「异步 provider 能 await IO」——本 issue 的验收面（`#` 候选 + Tab/Enter 落地）用不到。
   **留作后续**：若真要做，最小改法是给 trait 加一个可选 `fn get_suggestions_async(...) -> BoxFuture`
   重载 + App 侧 pending 槽，届时本模块的桥可以原样复用（只换驱动线程）。
2. **纯选项 2（允许异步回调，阻塞等它）** —— 阻塞等一个 `await pi.exec(...)` 的回调会把 UI 冻住
   整个子进程时长（可能是秒级），且与 `pi_ai_stream_start` 那类长事务的线程模型冲突。
   所以保留了它的**桥**，砍掉它的**能力**。
3. **纯选项 3 但不做桥（缓存 + 异步预热）** —— 需要给 App 加「重查询」通道并容忍第一帧无候选，
   行为不可确定性断言（帧测试会 flaky），与本仓「帧 + 行为断言」的证据口径冲突。

选中的方案的线程模型，与既有 host op 对齐：

- **JS → Rust（`host_ui_autocomplete`）**：与 `host_ui_region` 同一个形状——同步 import + JSON 信封。
  必须同步，因为 wrapper 在普通函数调用里就要求 `current.getSuggestions(...)` 的返回值，没有可 await 的位置。
  它不做任何 await、不 spawn，只调宿主注入的 `AutocompleteBaseProvider`（纯 Rust，`CombinedAutocompleteProvider`）。
- **Rust → JS（`_pi_autocomplete_call`）**：走既有的 `async_with!` + `drive_call` + 中断 deadline
  （与 `emit_event` / `execute_tool` 完全同一条路径），区别是 shim 侧**同步返回 JSON 字符串**
  （`pi-ext-shim.mjs:688`），所以 future 只 poll 一次就完成——调用方即使超时也不会把 JS 执行砍在中间。
  这一点是「阻塞桥不会破坏 rquickjs 锁」的关键论据。
- **编辑器侧（同步 trait → 异步宿主）**：`block_in_place` + `Handle::block_on`，250ms 上限
  （`AUTOCOMPLETE_BRIDGE_TIMEOUT`，`extensions/autocomplete.rs:53`）。三种情况的兜底：
  runtime flavor 不是 `MultiThread`（例如 `#[tokio::test]` 默认的 current_thread）→ 不调用 JS，
  用内置 provider 兜底；超时 / 宿主已死 / 信封 `ok:false` → 同前；JS 回 `null`（扩展明确说「没有候选」）
  → 返回 `None`，**不**回落（语义边界，见 §5）。
- **回调只拿文本快照**：`AutocompleteRequest { lines, cursorLine, cursorCol, force }`，
  没有 TUI 句柄、没有 IO 能力、`signal` 是常量；wrapper 拿不到任何可阻塞的东西。

## 3. 改了什么（文件:行）

**`pi-tui`**

- `src/autocomplete.rs:288-296` `AutocompleteProviderFactory`（上游 `types.ts:126`）。
- `src/autocomplete.rs:298-374` `TriggeredAutocompleteProvider`：透传 + 覆盖触发表。
- `src/autocomplete.rs:376-401` `compose_autocomplete_providers`：按序叠加 + 触发符去重。
- `src/editor.rs:1758-1785` `set_autocomplete_provider` 改为「先重建默认表」，新增
  `autocomplete_trigger_characters()`。
- `src/lib.rs:50` 导出新类型与函数。

**`pi-extensions`**

- `src/autocomplete.rs`（新，168 行）：`AutocompleteRequest` / `AutocompleteItem` /
  `AutocompleteSuggestions` / `AutocompleteCompletion`（camelCase wire 形状）+
  `AutocompleteBaseProvider` trait（内置 provider 的注入面）+ 3 条单测。
- `src/host.rs:739-746` `HostOptions::autocomplete_base` + `with_autocomplete_base`；
  `:1596-1607` `Inner` 的槽与 generation 计数器；`:1778` 构造。
- `src/host.rs:1454-1546` `handle_autocomplete_call`：同步 import 的四个 op
  （`register` / `baseGetSuggestions` / `baseApplyCompletion` / `baseShouldTriggerFileCompletion`）。
- `src/host.rs:2270-2362` `JsExtensionHost::{autocomplete_generation, has_autocomplete_base,
  autocomplete_call, autocomplete_rebuild}`。
- `src/host.rs:4307-4321` 安装 `host_ui_autocomplete`。
- `runtime/pi-ext-shim.mjs:385-740`：链状态 + `__pi_autocomplete_base_provider` +
  `__pi_autocomplete_rebuild/register/invoke/suggestions` + `_pi_autocomplete_call`（Rust→JS 入口）。
- `runtime/pi-ext-shim.mjs:890-902` `ui.addAutocompleteProvider` 从「no-op + 告警」改成真实现
  （并从 `:1096-1110` 的 unsupported 列表里移除）。
- `examples/issue-autocomplete.mjs`（新，131 行）：真实 fixture，见 §1.5。
- `docs/EXTENSIONS.md:113,147,553`、`docs/SDK_MODULES.md:436`：能力表 / host import 表 / 兼容表
  各补一行（含同步限制这条偏差）。

**`pi-coding-agent`**

- `src/extensions/autocomplete.rs`（新，430 行）：
  `SessionAutocompleteBase`（`pi_extensions::AutocompleteBaseProvider` 的实现，槽内是
  `Arc<dyn pi_tui::AutocompleteProvider>`）、`JsAutocompleteProvider`（`pi-tui` 同步 trait →
  `_pi_autocomplete_call` 的有界阻塞桥，逐方法回落 base）、`compose_provider`、2 条单测。
- `src/extensions/wiring.rs:270,318-346,556-580`：`ExtensionRuntime::autocomplete_base` 槽 +
  `host()` / `rebuild_autocomplete()` / `has_autocomplete_wrapper()`；`load()` 里创建槽并注入 host
  （两条 host_options 分支都注入）。
- `src/interactive.rs:399-422` `install_composer_autocomplete` 返回内置 provider（原来只装不返回）；
  `:424-465` `install_extension_autocomplete`：把内置 provider 发布给 host → `rebuild` 取触发表 →
  `compose_provider` → 装到编辑器。
- `src/interactive.rs:511-532` run_loop 启动时安装；`:653-664` 每帧比较 generation，晚了注册的 wrapper
  会被重新装进编辑器。
- 测试：`src/interactive.rs` 新增 1 条接线单测（`install_extension_autocomplete_wires_the_chain_into_the_editor`）。

**测试与证据**

- `crates/pi-tui/src/autocomplete.rs`（+4 单测）、`crates/pi-tui/tests/autocomplete.rs`（+3）
- `crates/pi-extensions/tests/autocomplete.rs`（新，8 条）
- `crates/pi-coding-agent/tests/lum1448_autocomplete_provider_frames.rs`（新，5 条 + 3 帧）
- `docs/screenshots/lum1448-autocomplete-provider-{hash,tab,enter}-78x14.{txt,png}`

## 4. 证据

### 4.1 宿主层（`cargo test -p pi-extensions --test autocomplete`，8 passed）

真 QuickJS + 真 fixture 文件 + 一个记录调用的 `RecordingBase` 假内置 provider（记录 `getSuggestions`
/ `applyCompletion` / `shouldTriggerFileCompletion` 各被调了几次，用来区分「扩展自己答」与「扩展委托」）：

| 断言 | 结果 |
|---|---|
| `rebuild` 返回 `['#']`（fixture 只声明 `#`，base 不贡献） | ✅ |
| `#29` → `{prefix:"#29", items:[#2983]}`，描述是扩展自建的 `[open] Extension API for autocomplete`，且 **base 的 `getSuggestions` 调用次数 = 0** | ✅ |
| `#reload` → 非数字分支命中 `#2753`（标题模糊） | ✅ |
| `/he` → 委托 base（调用次数 = 1）→ `help` | ✅ |
| `applyCompletion("#29" → "#2983")` → 通过 base 落地，base 的 `applyCompletion` 次数 = 1 | ✅ |
| `shouldTriggerFileCompletion` 委托 base，到 `true`（`?? true` 语义） | ✅ |
| 空链（没有 wrapper）仍委托 base | ✅ |
| 宿主没有内置 provider 时退化 `suggestions: null`（不炸） | ✅ |
| Promise-returning `getSuggestions` → **不 await**：`$never` 从未出现，base 答的 `help` 出现 | ✅ |

### 4.2 交互层（`cargo test -p pi-coding-agent --test lum1448_autocomplete_provider_frames`，5 passed）

真 `JsExtensionHost`（加载磁盘上的 fixture + `session_start`）+ 真 `App`（`commands::slash::
autocomplete_commands()` 作 base）+ 真编辑器；`tokio` multi-thread runtime（桥的前提）：

| 断言 | 结果 |
|---|---|
| 输入 `#29` → 下拉框打开、`prefix == "#29"`、**1 条候选** | ✅ |
| 帧里出现 `❯ #2983` 行，且该行含 `[open] Extension API for autocomplete` | ✅ |
| 草稿行仍是 `#29` | ✅ |
| `Tab` → 草稿变 `#2983`、下拉框关闭 | ✅ |
| `#14` → 2 条候选（`#1448` / `#1436`）；`Down` 后 Enter → 草稿变 `#1436`，**消息数不变**（未提交） | ✅ |
| `/com` 仍出内置候选（组合链不遮蔽内置） | ✅ |
| `#zzz` → 扩展与 base 都无候选 → 下拉框不开、草稿不动 | ✅ |

另 1 条接线单测（`pi-coding-agent/src/interactive.rs`，current_thread runtime，故意走「桥不可用」分支）：
`install_extension_autocomplete` 把 wrapper 的 `$` 触发布进编辑器表（`['@','#','$']`），
没有 wrapper 的 runtime 返回 `false` 且表保持 `['@','#']`。

### 4.3 真帧截图（无 PTY，frame-buffer 通道）

`docs/screenshots/lum1448-autocomplete-provider-hash-78x14.{png,txt}`（`#29` 时下拉框带
`[open] …` 描述）、`-tab-`（Tab 后草稿 `#2983`）、`-enter-`（Enter 后草稿 `#1436`）。
78×14，真 `App::render_to_buffer` 网格，`scripts/frame_to_png.py` 上色；`.txt` 是可 grep 的载荷。
**注意口径**：这是 frame-buffer 截图，不是 PTY 实拍——本机 Windows runner 没有 PTY
（见 `docs/LUM1426_POINTER_COLUMNS.md` §9），所以它证明的是「画进了帧」+ 草稿状态，
不证明真实终端按键时序（`scripts/pty_capture.py` 仍是声称终端行为的唯一证据）。

## 5. 已知偏差（逐条，供后续收敛）

1. **只服务同步回调**（最大的一条）。上游 `getSuggestions` 是 `async`；本端口若回调返回 Promise，
   那次调用**不 await**，回落内置 provider，并通过 `ctx.ui.notify(…, "warning")` 只告警一次
   （`pi-ext-shim.mjs:576-660`）。因此上游 `github-issue-autocomplete.ts` 的**原始 `.ts` 源码
   在本端口不可用**（它 `await pi.exec`），必须像 fixture 那样改成同步预载。
2. **`options.signal` 是常量**：`new AbortController().signal`，永不 abort。上游用它取消过期的
   `fd`/`gh` 调用；本端口同步回调没有可取消的窗口。扩展写 `options.signal?.aborted` 能跑但恒 `false`。
3. **`force` 语义**：`force` 透传给扩展（Tab 触发），但内置 provider 的 `force` 分支在
   `getSuggestions` 里被 wrapper 的委托覆盖时才生效；扩展若不看 `force`，行为与上游一致（上游示例也不看）。
4. **回调抛异常 / apply 返回非法值 → 回落内置 provider**（上游会让异常冒到编辑器）。这是有意的
   健壮性偏差：一个坏扩展不能把内置补全一起弄坏。`applyCompletion` 返回无 `lines` 数组时**不**清空草稿。
5. **`options` 对象没有 `AbortSignal` 的其他字段**（`addEventListener` / `reason` 等由 polyfill 提供，
   语义为空）。
6. **`ctx.ui.addAutocompleteProvider` 在 print / RPC 下静默注册**（无编辑器可装）。上游 print 模式的
   UI context 是同名 no-op（`runner.ts:256`），口径一致；差别是这里的注册会真的进链但没人消费。
7. **触发表来自「组合器最后一次 rebuild」**：晚注册的 wrapper 由 run_loop 每帧比 generation 后重装
   （`interactive.rs:653-664`），所以最坏延迟 1 帧（渲染间隔）。上游是同步重装，无延迟。
8. **`pi-extensions` 的 provider wire 类型与 `pi-tui` 的类型是两套**（`pi-extensions` 不能依赖
   `pi-tui`）。字段一一对应，转换点在 `SessionAutocompleteBase` / `JsAutocompleteProvider`；
   新增字段要两边同时改（这是既有 `UiRegionHost` 模式，非本轮引入）。

## 6. 不可测项（明确说明）

- **真实终端按键时序**：本机没有 PTY，Tab/Enter 走的是 `App::step(InputEvent::Key)`，不是
  crossterm 读到的字节流。帧与草稿是真 App 的产物，但「用户按 Tab 的那一瞬间」不可复现。
- **上游 TS 侧交叉验证**：本机无 `node_modules`、未安装依赖，无法跑
  `packages/coding-agent/test/interactive-mode-status.test.ts:304`（上游对同一 API 的单测）来对齐。
  因此「上游行为」的判据是**源码逐行核对**（§1 的每一行都给了 `path:line`），不是运行上游。
- **多扩展叠加**：只测了「1 个 wrapper」的链。JS 侧对多个 factory 是按序 `factory(provider)`
  （与上游同构，代码可读即可验证），但没有第二个真扩展 fixture 去证多扩展交错。
- **异步 provider 的正向路径**：只测了「返回 Promise → 回落 + 告警」这条。真 await 需要在
  帧循环里消费 pending（选项 1），本轮不做。
- **`signal` 被 abort 后扩展的行为**：信号恒不 abort，无法测。

## 7. 门禁与实测数字

环境：Windows x86_64 / cargo 1.97.1 / rustc 1.97.1 / 冷 `target`（本轮新建）。

| 门禁 | 基线 `c6d6df108` | 本轮 | 说明 |
|---|---|---|---|
| `cargo test -p pi-tui --no-fail-fast` | 1027 / 0 | **1034 / 0** | +7 = `autocomplete.rs` 4 单测 + `tests/autocomplete.rs` 3 条（组合/触发表复位/`#` 端到端） |
| `cargo test -p pi-extensions --no-fail-fast` | 120 / 5 | **131 / 5** | +11 = 新 `tests/autocomplete.rs` 8 条 + `src/autocomplete.rs` 3 单测；失败集逐字不变 |
| `cargo test -p pi-coding-agent --no-fail-fast` | 815 / 28 | **823 / 28** | +8 = 帧测试 5 条 + `extensions/autocomplete.rs` 2 单测 + `interactive.rs` 1 单测；失败集逐字不变 |
| `cargo fmt --all -- --check` | 干净 | **干净**（exit 0） | 本轮先 `cargo fmt --all` |
| `cargo clippy -p pi-tui -p pi-extensions -p pi-coding-agent --all-targets` | `pi-extensions` 1 条（`signal_name` 死代码）+ vendor 13 条 | **同基线，0 条新告警** | |

基线与本轮的失败**同名同数**（5 / 28），全是 Windows 环境类：`node_fs_create_*_stream_*` 与
`sdk_modules_load_and_exercise_the_upstream_surface`（临时路径分隔符 `\` vs `/`）、
`*_rejects_absolute_path*`、`ls_*` / `bash_runs_ls`、`paths::tests::absolute_paths_stay_absolute`、
`trust::tests::*`、`*_extension_*_loads_*`（临时目录大小写/短名），以及
`commands::export::tests::cli_export_propagates_the_upstream_error_text` /
`export::session_file::tests::missing_files_report_the_upstream_message`
（`C:\definitely\not\here\…` vs `/definitely/not/here/…`）。本轮的差异命令：
`diff <(基线的失败名集合) <(本轮的失败名集合)` → 空。

## 8. 明确不做 / 范围之外

- **不给 `#` 内置语义**（issue 明确要求）：`#` 只是触发符，基础 `CombinedAutocompleteProvider`
  没有 `#` 分支——`unknown_hash_token_shows_nothing` 正是这条的回归断言。
- **不动 `pi-protocol` 的 wire types**：新增字段都在 `pi-extensions` 自己的 provider 类型里，
  协议枚举零改动（`git diff --stat -- crates/pi-protocol` 为空）。
- **不做内置 `#` 候选来源**（issue/PR 号查询）：那是扩展的职责，正是本 issue 要证明的注入点。
- **不做异步 provider 的帧循环消费**（选项 1），见 §2 的后续方案。
- **不做 PTY 场景脚本**：本机跑不了，写了就是未验证脚本；frame-buffer 帧才是可复现证据。
