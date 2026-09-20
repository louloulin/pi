# pi-rust TUI 交互保真审计（LUM-1240，第十轮协调轮）

本轮是**协调 + 审计轮**，不改 TUI 代码：在合并后的 tip `0ab42b5d3` 上用真实 PTY
逐条核对「启动头 / `/hotkeys` 广告出来的键位」与「代码里真的有人消费的键位」是否一致，
并把不一致连证据一起固化成本文。

## 1. 结论先行

1. **广告面 ≠ 实现面**：启动头广告 20 条键位、`/hotkeys` 广告 62 条（其中 `app.*` 18 条），其中
   **4 条按下去什么都不会发生**：`app.suspend`（`Ctrl+Z`）、`app.model.select`（`Ctrl+L`）、
   `app.editor.external`（`Ctrl+G`）、`app.thinking.cycle`（`Shift+Tab`）。
   前两条连 `/hotkeys` 也列了。
2. 另有 **2 处语义不符**（键位有人消费，但行为与广告文案不等价）：`Ctrl+L`（下面第 3 条，
   端口自造语义）与 `Ctrl+C`（`TUI_UX_AUDIT.md` §12.4 已记录、本轮复测仍成立，归 LUM-1238）。
3. `Ctrl+L` 最严重：**它不只没实现广告里的语义，还被硬编码成了另一种语义**——
   清空转写。上游 `packages/coding-agent/src/core/keybindings.ts:116` 把 `ctrl+l` 给
   `app.model.select`（"Open model selector"），全仓没有任何「清屏」键位；
   端口把这枚上游键位挪作他用，而广告文字仍按上游读表，于是「广告说选模型，实际清屏」。
  端口自己的模块注释（`crates/pi-tui/src/app.rs:128-132`）记着这处偏差，但它引用的上游行号
  （`keybindings.ts:101`）也已过期——该条目现在在 `keybindings.ts:116`。
4. 根因是**键位模型少了一个轴**：`KeybindingDefinition` 只有「绑了哪些 chord」，
   没有「实现了没有」；两个广告面（启动头 `locale.rs`、`/hotkeys` `slash.rs`）都用
   「该 id 有默认 chord」当作「该动作可用」的判据（两者注释都明写「无 chord 就不显示」），
   于是**默认绑定表里 28 条没有消费者的 id，只要有 chord 就会被当成可用的快捷键展示**。
5. 反方向（实现了却没广告）本轮**没有发现问题**：16 条有消费者的 `app.*` 动作里，
   只落在 `/hotkeys` 的（`app.message.copy`、`app.session.*`）与「不列也无害」一致，
   没有出现「能用但用户永远发现不了」的 P1。
6. 其余交互面在 tip 上实测正常：`/` 命令下拉、`@` 文件补全 + `Tab` 应用（LUM-1236 接线后
   首次实机可见）、`/model` 选择器锚在消息视口内、一轮完整问答、排队输入不丢、footer 计量。

## 2. 方法（可复现）

- 被测二进制：**同一棵树的构建产物** `pi-rust/target/debug/pi`（debug，与 tip 同期构建，
  本轮**没有**重新编译——原因见 §7）。启动参数 `--model faux/faux-model`，
  环境 `TERM=xterm-256color`、`PI_LANG=en`。
- harness：`pty.fork` + `TIOCSWINSZ`（120×24）+ 自写 VT/CSI/OSC 解析 → 字符网格 → 光栅成图。
  **只在协调侧，不入库**（与第九轮同规则：它依赖终端仿真与图像库，不适合当构建门槛）。
- 每个画面既留截图、也留解析后的**字符网格原文**；本文引用的都是字符网格原文（可逐字核对），
  截图只是它的可视化。
- 「死键位」判定走两步：① 静态查消费者（`app.*` 与 `tui.*` 的 `matches_*` / 抢占分支）；
  ② 实机按键 A/B（同一次运行内比对屏幕字符网格的 md5，并检查进程状态、草稿是否保留）。

## 3. 实机画面

| 截图 | 画面 | 观察 |
| --- | --- | --- |
| `screenshots/lum1240-01-session.png` | 启动 / 输入 / 忙态 | 启动头 20 行键位提示、onboarding、`> type a prompt — /help for commands`、footer（模型 / `session-…` / `in 0 out 0` / 上下文 / `? for help`）都在。**忙态面板不成立**：`faux` 提供商瞬时返回，看不到 spinner 与「Esc to interrupt」，这一格只是「回车后立刻出答案」（真实 key 才能观察） |
| `screenshots/lum1240-02-slash-completion.png` | 一轮回答 + `/` 命令下拉 | `> hello` / `  faux-model (faux) hello` 正常；输入 `/` 后下拉列出命令，继续输入过滤（`/mo`） |
| `screenshots/lum1240-03-file-completion.png` | 下拉选中项落到输入行 + `@` 文件补全 | `Down`+`Tab` 把选中项落到输入行（落到 `/copy `）；`@` 触发文件补全下拉（`@src/` 候选） |
| `screenshots/lum1240-04-hotkeys-and-model.png` | `@` 补全应用 + `/hotkeys` + `/model` | `Tab` 把路径补全成 `@src/`；`/hotkeys` 把生效键位（含 `Ctrl+L open the model selector`）当一段文字贴进转写；`/model` 选择器画在消息视口内，标题 `Pick a model`，10 行窗口 |
| `screenshots/lum1240-05-ctrl-l-parity.png` | `Ctrl+L` 前 / 后 + `Ctrl+P` 对照 | 前者：转写里的 `> hello there` 与回答**整段消失**（清空），而启动头写的是 `Ctrl+L to select model`；对照：`Ctrl+P` 真的切了模型（`> model → fireworks/DeepSeek V4 Flash (0731)`，footer 同步） |
| `screenshots/lum1240-06-noop-chords.png` | `Ctrl+G`、`Shift+Tab`、`Ctrl+Z` | 左三格：`Ctrl+G` 前/后、`Shift+Tab` 后；右两格：`Ctrl+Z` 前/后（另一次会话）。五格的画面与各自的按键前**逐字节相同**（同一次运行内屏幕 md5 一致），草稿 `draft text` 原样保留，进程仍在 `S` |

## 4. 广告面 vs 实现面（逐条核对）

「广告」两列指：启动头（`crates/pi-tui/src/locale.rs` 的提示表）与 `/hotkeys`
（`crates/pi-coding-agent/src/commands/slash.rs` 的 `APP` 分组）。

| 动作 id | chord | 启动头 | `/hotkeys` | 消费者 | 实机 |
| --- | --- | --- | --- | --- | --- |
| `app.suspend` | `Ctrl+Z` | 有（`locale.rs:126`） | 有（`slash.rs:311`） | **无**（全仓 0 处 `SIGTSTP`/`raise`/`Suspend` 动作） | 屏幕 md5 不变，进程仍 `S`，草稿保留 |
| `app.model.select` | `Ctrl+L` | 有（`locale.rs:146`） | 有（`slash.rs:329`） | **无** | 清空转写（`app.rs:2355-2360` 硬编码），消息行消失 |
| `app.editor.external` | `Ctrl+G` | 有（`locale.rs:166`） | 无 | **无** | 屏幕 md5 不变，草稿保留 |
| `app.thinking.cycle` | `Shift+Tab` | 有（`locale.rs:136`） | 无 | **无**（Stage 67 / LUM-1230 在飞） | 屏幕 md5 不变 |
| `app.clear` | `Ctrl+C` | 有（`locale.rs:110-119`，两行：「to clear」+「twice→to exit」） | 有（`slash.rs:310`） | 有（`app.rs:2343-2349`） | **语义不符**：草稿非空时一次也直接退出且草稿丢失（实测：草稿 `draft text` + 一次 `Ctrl+C` → 子进程变 zombie）；上游是 500ms 双击窗口，LUM-1238 在飞 |
| `app.interrupt` … `app.session.resume`（其余 16 条） | — | 有 | 有 | 有（`pi-tui/src/app.rs:2338-2402`、`pi-coding-agent/src/interactive.rs:743-840`） | 逐条实测或由既有测试覆盖（见 §7） |

补充事实：

- 默认绑定表共 **44** 条 `app.*`（`keybindings.rs:149-182`），其中 **16** 条有消费者、
  **28** 条没有；这 28 条里被广告出来的是上面 4 条。
- 上游对照：`packages/coding-agent/src/core/keybindings.ts:116` = `app.model.select → ctrl+l
  "Open model selector"`；`ctrl+l` 在上游的另一处用途是会话树内的
  `app.tree.filter.labeledOnly`（同文件 223 行），**没有**清屏键位。端口把 `ctrl+l`
  硬编码为「清空转写」是自造语义，且只在 `crates/pi-tui/src/app.rs:128-132`
  的模块注释里记着——这是开发者文档，**用户可见的两个广告面都没提**。

## 5. 根因

```text
KeybindingDefinition { keys, description }      // crates/pi-tui/src/keybindings.rs
        ▲ 只有「绑了哪些键」
        │
  两个广告面都拿它当「实现了没有」：
    locale.rs    —— 「A wholly unbound id is not a hint.」（locale.rs:355-356 的断言）
    slash.rs     —— 「A row whose id has no chord is dropped — an unbound action is not a shortcut.」（slash.rs:256-257）
        │
  默认表 44 条里 28 条没有消费者 ⇒ 其中 4 条被当成可用快捷键展示
```

也就是说，这 4 条不是「漏写文档」，而是**广告面的过滤条件用错了轴**：`bound` 被当成了
`implemented`。只要这个轴缺着，以后每加一个「先绑好、后实现」的上游 id，都会自动多出一条死键位。

## 6. 建议补丁（本轮**未落地**，原因见 §7）

**P0-1（`Ctrl+L` 语义，恢复上游一致）**：把 `/model` 的选择器构造抽成
`open_model_selector(app, options)`（与 `open_tree_selector` / `open_fork_selector`
同款，`interactive.rs:1268-1300` 已有先例），在 driver 抢占块
（`interactive.rs:743-840`，`app.model.cycleForward` 分支旁）加一条
`matches_with_fallback(&keybindings, &event, "app.model.select", &["ctrl+l"])` → 调它；
同时删掉 `crates/pi-tui/src/app.rs:2351-2360` 的 `Ctrl+L` 硬编码。
「清空转写」不会丢：`/clear`（`slash.rs`）已经在做同一件事。
改动面仅 driver 一处 + `app.rs` 删 10 行，且**不需要**动 `locale.rs` / `slash.rs` 的文案。

**P0-2（给键位模型补上 `wired` 轴）**：让「哪些 id 真的有人消费」成为一份可查询的事实，
两个广告面共用同一份判据，例如在驱动侧维护

```rust
/// 真的被消费的 `app.*`（driver 抢占块 + `App::step_key`）。
pub const CONSUMED_APP_ACTIONS: &[&str] = &[/* app.interrupt, … */];
```

启动头与 `/hotkeys` 在筛掉「无 chord」之后，再筛掉「未消费」（`app.*` 才需要这一步，
`tui.*` 由组件自身消费）。配套测试：`crates/pi-tui/tests/startup_header.rs` 断言
提示表里每个 `HeaderKey::Chord(id)` 都在 `CONSUMED_APP_ACTIONS` 里——**这条断言今天会红**，
所以它必须和补丁同批落地。
`Shift+Tab` 不必单独处理：LUM-1230（Stage 67）落地 `app.thinking.cycle` 后它自动变真，
届时把它加进名单即可；`Ctrl+Z`（需要 SIGTSTP + 终端 teardown/restore）与
`Ctrl+G`（需要外部编辑器交接）在没有实现之前，应该**从广告面消失**而不是继续挂着。

**P1**：`/hotkeys` 目前是**一整段折行文字**（见 §3 第 4 张图），分组标题（`app:`、
`selectors and completion:`、`commands:`）落在折行中间，`Ctrl+P` 与它的说明被拆到两行；
上游 `interactive-mode.ts:6315-6419` 是按组换行的。建议 `hotkeys_text` 每组之间补空行 /
逐行输出，成本一行代码级。

## 7. 验证（跑了什么、没跑什么）

跑了：

- 真实 PTY：13 帧交互画面 + 9 帧按键对照（`Ctrl+L` 前/后、`Ctrl+G` 前/后、`Shift+Tab` 后、
  `Ctrl+Z` 前/后、`Ctrl+P` 前/后），屏幕字符网格 md5 与 `/proc/<pid>/stat` 状态都在结论里引用了。
- 静态消费者核对：对 44 条 `app.*` id 逐条 grep（排除 `keybindings.rs`（默认表）、
  `locale.rs`（提示表）、`slash.rs`（`/hotkeys` 表）与 `tests/`），结果即 §4 的表。
- 上游对照：读 `packages/coding-agent/src/core/keybindings.ts` 与
  `packages/coding-agent/src/core/interactive-mode.ts` 的键位声明。

**没跑**：`cargo fmt` / `cargo clippy` / `cargo test`（含 `cargo test -p pi-evals` 的文档
一致性 case）。原因是本机磁盘只剩 **252M**（6 个并发 run 在跑），本 checkout 没有
`target/`，任何一次 `cargo check` 都要从零编译依赖，空间不够。
因此本轮**不落任何 Rust 代码**：§6 是补丁规格而不是已编译验证的补丁。
本文只在 `pi-rust/docs/` 下新增（代码面零改动），不触碰在飞文件。

## 8. 与 codex / pi TUI 的差距（真实评估）

已经追平、不要再返工的：

- 常驻启动头 + onboarding + footer 计量（模型 / session / `in`/`out` / 上下文 / `? for help`）、
  `Esc` 中断、`Ctrl+D` 空输入退出、`Ctrl+K` 删到行尾；`Ctrl+C` 的退出路径可用，
  但「先清空、第二次才退出」的双击语义仍缺（§4 表、`TUI_UX_AUDIT.md` §12.4，LUM-1238 在飞）；
- 输入通道：`/` 命令面、`@` 文件补全、`Tab` 应用、`!` / `!!` bash、`Ctrl+V` 贴图（含文本回退）、
  拖文件附件；
- 覆盖层：`/model`、`/hotkeys`、`/tree`、`/fork`、`/resume`、`/settings`、`/compact`、
  转写搜索（`Ctrl+Shift+F`）、思考块/工具块折叠；
- 忙态：排队输入（`Alt+Enter` / 直接回车）、`Alt+Up` 取回、`Esc` 取消。

仍然差（按用户可感知程度排序）：

1. **广告面可信度**（本文）：4 条死键位 + 1 处语义反了（`Ctrl+L`），另有 `Ctrl+C` 的
   双击语义差已在 §12.4 记录；用户是靠这些提示学 TUI 的。
2. **外部编辑器入口**（`Ctrl+G`/`$EDITOR`）：codex 与上游 pi 都有，端口完全没有实现
   （需要终端 teardown/restore 交接，属于 Stage 59 遗留，见 `TUI_UX_AUDIT.md` §14.5 第 3 条）。
3. **挂起与恢复**（`Ctrl+Z`）：上游有，端口全仓 0 处实现。
4. **回放保真度**：`/resume` 重建转写仍只覆盖文本，工具卡片 / 思考块 / 耗时统计不回放
   （第六、七轮记录）。
5. **扩展可见性**（LUM-1239 在飞）与 `/help` 前缀过滤（LUM-1238 在飞）属他人在飞范围，
   本轮不重复投资。
6. 真机盲区：`faux` 提供商不产生工具调用，所以**工具块折叠、spinner、耗时统计在本轮截图里
   无法作为证据**；要出这类截图必须有真实 key 或可脚本化的伪工具轮次。

## 9. 本轮动作（协调轮，零派发）

- **不新建子任务**：任务上限「最多 3 个并行」，而 worker 槽位已被
  LUM-1230（Stage 67 思考级别）、LUM-1238（Stage 70 输入面）、LUM-1239（Stage 71 扩展可见性）
  占满，另有一个并发的协调轮 LUM-1237 在跑。硬塞第 4 个只会互相踩文件。
- 停在 frontier 上、等槽位空出的：Stage 67（LUM-1230）、Stage 68、Stage 69，
  以及 Stage 70 / 71 的收尾。
- 本轮产物：本文 + `docs/screenshots/lum1240-*.png`（6 张），合入 `feature/pi.rs`。
