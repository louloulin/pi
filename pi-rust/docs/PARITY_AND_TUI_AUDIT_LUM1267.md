# pi-rust ↔ pi-TS 差距审计 + TUI 交互验证（LUM-1267）

> 基准：`origin/feature/pi.rs` = `b8adfe6bd`（含 LUM-1261 的两个 P0 修复 `c8bc6785a`/`b8adfe6bd`）；
> 本 round 的 `work/LUM-1267` 在该 tip 上只新增 `docs/` 与 `scripts/`，**没有改任何 `.rs`**（原因见 §1）。
> 本轮所有数字都可由 §9 的命令复现；PTY 证据落在 `docs/screenshots/lum1267-*`。

## 1. 本轮约束与定位（为什么没有 Rust 代码）

`/` 分区（overlay 50G）本轮长期只剩 0.5–2.0G，同一台机器上有 4+ 个并发 `cargo` 构建（其他
in-flight issue 的 `target/`：lum-1265 13G、lum-1255 6.5G、lum-1252 2.2G、lum-1261 2.2G、
lum-1266 1.3G，全部是活着的 run，不可回收），没有可写的大挂载点（`/tmp/cargo-home` 729M 与
`/tmp/rustup-home` 780M 是共享缓存，不能删）。结论：**本轮无法可靠地 `cargo build`/`cargo test`**，
按仓库既有规则「无法验证就不落码」（同 LUM-1260：只落 docs+scripts，合并 `bdafcd67a`），
不写 `.rs`。

但"不落码"不等于"只写文档"：本轮的产出是**可运行、可复现、能失败**的验证工具与断言基线（§3），
它们今天就能在没有编译器的情况下对真实二进制跑出结论——这正是下一轮写 Rust 时的验收门。

## 2. 交付物

| 文件 | 类型 | 作用 |
|---|---|---|
| `scripts/pty_capture.py` | 改（Python） | 面板级断言引擎 + 首帧/异步同步（§3.1） |
| `scripts/pty_scenarios/lum1267-interaction-assertions.json` | 新 | 120×34 交互断言基线，17 面板 50 检查（§3.2） |
| `scripts/pty_scenarios/lum1267-narrow-composer.json` | 新 | 120×23 窄终端 P0 探针（§3.3） |
| `scripts/extension_event_coverage.py` | 新 | 扩展生命周期事件覆盖率扫描器（§4.2） |
| `docs/screenshots/lum1267-interaction-assertions.png{,.txt}` | 新 | 17 帧证据（文本 dump 是承重证据） |
| `docs/screenshots/lum1267-narrow-composer-23.png{,.txt}` | 新 | P0-1 的 before/after 同一会话证据 |
| `docs/PARITY_AND_TUI_AUDIT_LUM1267.md` | 新 | 本文 |

`scripts/app_action_coverage.py`（LUM-1260）未改动，本轮只是重跑（§4.1）。

## 3. 交互断言基线：把截图从"声明"变成"检查"

### 3.1 工具升级（`scripts/pty_capture.py`）

1. **面板级断言**：每个面板可带 `expect` / `reject` / `probe` / `xfail` / `xfail_reject`，
   值支持 `{text|regex, count, why}`（`count` 取 `N` / `"N..M"` / `">=N"` / `"<=N"`）。
   状态机刻意做出两种"期望失败"的语义差别：
   - `xfail` / `xfail_reject`：**已知缺陷标记**——按预期失败记 `XFAIL`，但一旦**意外通过**
     记 `XPASS` 并让整轮 **非零退出**。符号不可能比缺陷活得久（`--allow-xpass` 可临时降级）。
   - `probe`：**A/B 证据**——未满足记 `XFAIL`（带原因），满足记 `PASS`，两者都不算失败。
     用来在同一条场景里同时记录"修复前/修复后"两侧。
2. **首帧与异步同步**（本轮修掉的工具缺陷）：以前第一个面板可能在应用首次绘制前就被冻结，
   于是一张**空网格**被当成证据（空网格的 `frame` 哈希是空串的 sha256 `e3b0c44298fc`，而面板
   标题仍写着"idle 界面"）。现在：
   - 首个面板前先 pump 到「屏幕上至少有内容」（上限 12s）；
   - 面板可声明 `wait_for`（+`wait_timeout`，默认 8s）pump 到某段文字出现再冻结，
     模型回复、工具执行这类异步状态不再可能被提前截取。
   这一条立刻暴露了 LUM-1260 场景里的一个假证据：它的第 2 面板标题写 "(1/19) 全部命令"，
   但那一帧只拍到 `/`（列表还没画出来，`wait_for` 缺失所致）；加了同步后同一位置稳定拍到
   `❯ help  Show this help text` 与计数器 `(1/19)`（`docs/screenshots/lum1267-interaction-assertions.png.txt` 面板 2）。

### 3.2 交互基线（120×34，`lum1267-interaction-assertions.json`）

针对**修复前的二进制** `artifacts/pi-tip-b5769b585`（tip `b5769b585`，即 LUM-1261/LUM-1266
落地之前）跑出的结果：

```
assertions: 50 checks over 17 panels — 48 PASS, 0 FAIL, 2 XFAIL, 0 XPASS
```

| # | 交互 | 断言要点 | 结果 |
|---|---|---|---|
| 1 | 启动面 | `pi v0.1.0` / 提示表 `/ for commands`、`! to run bash`、`Alt+H to hide this header` / onboarding 行 / `? for help` | PASS |
| 2 | 输入 `/` | 补全列表打开、首项选中 `❯ help`、计数器 `(1/19)` | PASS |
| 3 | 输入 `mo` | `❯ model` 仍在选中位、计数器 `(1/8)` | PASS（另 2 条 `XFAIL`，见下） |
| 4 | `<Down>` | 选中标记移到 `❯ copy`（恰好 1 处）、计数器 `(2/8)` | PASS |
| 5 | `<Tab>` | 候选写入草稿 `> /copy`、列表关闭（`reject` 计数器） | PASS |
| 6 | `<Enter>` | 命令执行并在 transcript 报告 `> nothing to copy yet` | PASS |
| 7 | `/model` | 覆盖层 `Pick a model` + 真实 provider 列（`anthropic`）+ 边框 | PASS |
| 8 | `<Esc>` | 覆盖层消失（`reject`）且回到占位符 | PASS |
| 9 | `@` | 文件补全列出 fixture 树（`❯ src/`、`Cargo.toml`、`README.md`） | PASS |
| 10 | 输入 `src` | 候选收窄到子树（`reject` 掉 `Cargo.toml`/`README.md`） | PASS |
| 11 | `<Tab>` | 路径 token 写入草稿 `> @src/`、列表关闭 | PASS |
| 12 | 一次真实 provider 往返 | `> hi` 与 `faux-model (faux) hello` 同帧 | PASS |
| 13 | `!bash` | 命令、输出、耗时（`* Took N…`）成块 | PASS |
| 14 | `<C-o>` | 状态行给出工具输出展开态 | PASS |
| 15 | `/help` | transcript 渲染出命令参考（`slash commands:`、`/thinking`、`/hotkeys`、`/compact`、`keys:`） | PASS |
| 16 | `/hotkeys` | 键位表渲染（`Ctrl+L open the model selector` 等） | PASS |
| 17 | `<PgUp>` | transcript 回滚且出现 `↓ Jump to latest message` | PASS |

两条 `XFAIL` 是**故意的、必须消失的**标记（`xfail_reject`）：

```json
{"text": "Copy last agent message to clipboard", "why": "LUM-1263: 上游只按命令名过滤候选，Rust 还匹配描述，属偏差，必须去掉"}
{"text": "Create a new fork from a previous user message", "why": "LUM-1263: 同一处过宽匹配"}
```

输入 `mo` 时 Rust 给出 8 个候选（含 `/copy`、`/fork`、`/thinking`…），上游按下拉名过滤只会给
`/model` 一个。这条偏差今天被记录成"预期失败"，一旦 LUM-1263 收窄过滤就会变 `XPASS` 并让
run 非零退出，逼迫把标记改成 `reject`——**缺陷标记不会静默存活**。

### 3.3 窄终端 P0 探针（120×23，`lum1267-narrow-composer.json`）

```
assertions: 7 checks over 4 panels — 5 PASS, 0 FAIL, 2 XFAIL, 0 XPASS
```

对同一颗修复前二进制，两个 `probe` 都是 `XFAIL`（正是 LUM-1262 的 P0-1）：

- 面板 1（120×23 空闲）：状态行在，但**没有 composer 行**——整屏只有 21 行启动头 + onboarding + 状态行。
- 面板 2（盲打 `typed blind`）：帧哈希与面板 1 **完全相同**，屏幕上找不到任何字符，
  所以该场景刻意关掉 `distinct_panels` 硬检查（"两帧相同"在这里就是缺陷本身，由 `probe` 承载）。
- 面板 3（`Alt+H` 折叠启动头）：`> typed blind▍` 立刻出现在第 22 行，状态行写
  `Startup header: collapsed (Alt+H to show)`。

面板 3 是这条 P0 最干净的证据：**composer 一直都在，只是被 21 行的启动头按行预算挤没了**。
LUM-1261 把 `plan_chrome` 的预算顺序改成"先留 editor 行、再给 header"，并把 798 个 pi-tui 测试
跑绿合进 `feature/pi.rs`；因此这两个 `probe` 在合并后的 tip 二进制上会翻成 `PASS`。
本轮拿不到 tip 二进制（§1），所以"修复后"一侧引用 LUM-1261 已入仓的 PTY 帧
`docs/screenshots/lum1261-small-terminal-23.png.txt`（第 22 行即 `> typed blind`）——两侧都是真实
PTY 帧，不是推演。

## 4. 度量口径刷新（本轮可复现的两轴）

### 4.1 app 动作接线（未变）

```
$ python3 pi-rust/scripts/app_action_coverage.py pi-rust --check-consumed
CONSUMED_APP_ACTIONS: 35 entries; measured wired: 35
in sync: the header's hint filter matches the code      # exit 0
```

在 `b8adfe6bd` 上仍是 **35/44 = 79.5%**：已声明但未接线 2 条（`app.editor.external`、
`app.suspend`），无任何处理器的 7 条（`app.models.{clearAll,enableAll,reorderDown,reorderUp,save,toggleProvider}`
与 `app.tree.editLabel`）。后者不是"补键位"就能补上的——`/scoped-models` 组件在 Rust 里不存在，
`app.tree.editLabel` 需要 `pi-session` 新增 `label` 条目类型（`pi-rust/crates/pi-coding-agent/src/commands/tree.rs:113-118` 已注明）。

### 4.2 扩展生命周期事件（本轮新增扫描器，并修正文档口径）

新脚本 `scripts/extension_event_coverage.py` 把"可订阅 / 已声明 / wire tag / 生产构造点"四种口径
分开统计（正则与排除规则写在脚本 docstring 里，`#[cfg(test)]` 与 `tests/` 一律不算生产代码）：

```
$ python3 pi-rust/scripts/extension_event_coverage.py
upstream events: 36 (36 subscribable + 0 declared-only)
wire tags matching upstream:    21/36 (58.3%)
production emit sites:          20/36 (55.6%)
rust-only tags (not upstream):  user_message
dead variants (no production site): model_select
missing variants: 15/36
```

**口径修正（这是本轮的一个实质发现）**：`docs/RUST_TS_PARITY_METRICS.md` §0.1 写"剩余 14 个缺口"
并把分母写成 35（同一页也出现 36），实测两者都不准：

- 上游可订阅事件是 **36** 个（33 条单行 `on(event: "X", …)` 重载 + 3 条多行重载：
  `types.ts:1261` 的 `session_before_switch`、`:1266` 的 `session_before_compact`、
  `:1276` 的 `before_provider_request`；`context` 在 `types.ts:1275` 也是一条重载）。
  只用 `grep -c 'on(event: "'` 会数到 33，漏掉 3 条多行重载。
- 缺口是 **15** 个，不是 14，漏掉的是 **`context`**（既无变体也无构造点，属于最彻底的一类缺口）：

```
after_provider_response  agent_settled        before_agent_start
before_provider_headers  before_provider_request  context
project_trust            session_before_compact   session_before_fork
session_before_switch    session_before_tree      session_compact_failed
session_tree             ui_prompt_end            ui_prompt_start
```

按修正后的分母重算 §0.1 的加权重：扩展事件轴 `7% × 20/36 = 3.892`（原 `7% × 0.57 = 3.99`），
总分 **79.9% → 79.8%**。0.1pt 是四舍五入级，但口径本身应当以脚本为准（`--check-doc` 现在会把
文档里的事件清单与代码比对，`in sync` 才算过）。

## 5. 真实完成度百分比

| 口径 | 数值 | 来源与本轮状态 |
|---|---|---|
| 纯代码规模 | **82.1%** | `RUST_TS_PARITY_METRICS.md` §0.1（Rust src 125,652 / TS 153,106）；本轮无 `.rs` 改动，不变 |
| 测试规模 | **43.4%** | 同上（Rust 2,303 `#[test]` / TS 5,309 用例）；不变 |
| 功能面加权（主口径） | **79.8%** | 上表 79.9% 按 §4.2 修正分母后的值 |
| TUI 交互 + 视觉 | **73.8%** | §0.1（模块面 80.5% 与接线面加权）；本轮未改 TUI 代码，不变 |
| app 动作接线率 | **79.5%**（35/44） | 本轮实测，与 LUM-1260 一致 |
| 扩展事件 tag 对齐 / 生产构造 | **58.3%**（21/36）/ **55.6%**（20/36） | 本轮实测（§4.2） |

结论一句话：**代码已经不是瓶颈（82%），瓶颈是"最后一公里"——扩展生态的 15 个生命周期事件、
输入面缺组件（6 条 `app.models.*` + tree 改名）、以及输入控件本身的体验（§7 的 F1）。**
`docs/RUST_TS_PARITY_METRICS.md` §0.1 的敏感性分析里，扩展事件轴补满 +3.0pt、接线补满 +2.9pt，
仍然是两个最值钱的轴；本轮新证据把 F1（多行/滚动 composer）加进来，因为它是**用户每天都碰**
到的那一层，而不是扩展作者的层面。

## 6. Martty 精读 → 可搬的清单

对象：`https://github.com/louloulin/Martty`（`deepseek-harness-tui` v0.2.17，ratatui 0.30.2 +
crossterm 0.29，~33k 行 Rust；本地只读克隆 `/tmp/martty`，**不引入为依赖**）。逐条对齐 pi-rust
现状后，值得搬的是布局与降级策略，不是它的宠物/主题等本地特色：

| # | 做法 | Martty 位置 | pi-rust 现状 | 建议 |
|---|---|---|---|---|
| B1 | **过小终端硬兜底**：`height < 6 \|\| width < 24` 时只画一行 `terminal too small — need ≥ 24x6` 并 return，绝不交出破版 | `src/ui.rs:104-112` | **完全没有**（`grep -rn "too small"` = 0 命中）；120×23 时是"状态行还在、composer 静默消失" | P1：加最小尺寸兜底 + 明确提示；配合 §7 F2 |
| B2 | **composer 随内容长高 + 视口跟随光标**：`resolved_composer_height` 按折行数增长，上限 `min(h/2, 12)`，下限按终端高度的档位（≥15 行→4、≥10→3、否则 2） | `src/ui.rs:25-54` | composer **恒定 1 行**（§7 F1） | **P0/P1：本轮最值钱的一搬**，见 §7 F1 |
| B3 | **可选 chrome 先让路**：dock/agents/gap 用 `saturating_sub` 从总量里扣，扣不到就不画；会话区拿"剩下的" | `src/ui.rs:129-160` | `plan_chrome` 已是"先留 status/message/editor，再按 render 序分配"（LUM-1261 修） | 方向一致；残余风险是扩展 header 仍可把 message 压到 1 行（§7 F3） |
| B4 | **composer 是带边框的卡片**：`╭ … ─╮` 的 cap 行同时充当上边框，因此"卡片不额外占行"；下边框承载状态 chips | `src/ui.rs:118-145` | composer 是裸行 `> …▍` | P2：pi 的视觉语言里 `> ` 前缀已够辨识，收益主要是"卡片可承载 chips"；优先级低 |
| B5 | **鼠标捕获 + 括号粘贴**：启动即 `EnableMouseCapture` + `EnableBracketedPaste`，`Event::Resize` 只置重绘标志，`Event::Paste(text)` 作为**单个事件**进入（含"粘贴里带转义序列"的测试） | `src/main.rs:487-488`、`src/app.rs:2257-2258` | 需要单独核对（本轮未测，列为待办而不是结论） | P2：粘贴的原子性是真实体验差异；下一轮用 PTY 断言测 `Event::Paste` 等价路径 |
| B6 | **渐进降级到处都是**：logo `< 56` 列不画、宠物 `< 60×10` 不画、plan 视图 `< 88` 列不画 | `src/logo.rs:71`、`src/ui.rs:79/425` | 无同类"宽度阈值降级"约定 | P2：与 B1 同一原则，可在同一个 PR 里定策略 |

一句话：Martty 的布局是**自下而上**的（先在总高度里扣掉 composer/dock/agents/gap，会话区拿余数），
并且对"小到不正常"的终端有显式兜底；LUM-1261 已经在 pi-rust 里补上了预算顺序，但**兜底与
输入控件长高都还没有**。

## 7. 本轮新发现的真实缺陷（按严重度）

### F1（P0，输入面）composer 是单行、不折行、不滚动、不带光标视口

代码：`pi-rust/crates/pi-tui/src/extension_ui.rs:415-449` 的 `plan_chrome` 把 editor 区定为
`frame.editor.map_or(1, …)`——**内建 composer 恒定只拿到 1 行**；`app.rs:4460-4470` 的
`editor_area` 高度即 `layout.editor`；`app.rs:4685 paint_prompt` 只画
`self.prompt.render_line(rect.width)` 一行，越界即 `break`；而
`prompt.rs:114-140 render_line` 的实现是 `label + before + '▍' + after` 再右侧补空格——
**没有任何窗口/滚动偏移**。

PTY 实测（同一颗修复前二进制，120×34，输入 200 字符草稿；证据 `/tmp` 复现见 §9）：
composer 行只显示前 **118** 个字符（= 宽度 120 − 标签 `> `），`▍` 光标符号**一次都没画出来**
（`grep -c ▍` = 0），终端光标报在最后一列 `(120,32)`。用户输入超出宽度后**既看不到尾部，也
看不到插入点**。

对照上游与 Martty：

- pi-TS `packages/tui/src/components/editor.ts:508-560`：草稿折成 layout 行，
  `maxVisibleLines = max(5, floor(terminalRows * 0.3))`（至少 5 行、最多 30% 屏高），
  用 `scrollOffset` 保证光标行可见，并画带上边框 + 滚动指示的输入框。
- Martty `src/ui.rs:36-54`：`resolved_composer_height` 按折行数长高，上限 `min(h/2,12)`，
  内部视口跟随光标（B2）。

即：**这是相对上游 pi 本身的差距，不只是相对 Martty。**

### F2（P1）没有"终端过小"兜底

`grep -rn "too small\|min_height\|MIN_ROWS" pi-rust/crates/pi-tui/src` = 0 命中。120×23 的实测表现是
"状态行还在、composer 静默消失"（§3.3），用户没有任何提示。Martty 的 B1 是现成答案。

### F3（P1）扩展 header 仍可把 message 区压到 1 行

`plan_chrome` 的顺序是 status → message 预留 1 行 → editor → header → above → below → footer。
LUM-1261 之后内建启动头在窄屏由 LUM-1266 折叠，但**扩展通过 `ctx.ui.setHeader` 提供的头**仍会
在 editor 之后吃掉剩余所有行，message 只剩预留的 1 行。LUM-1261 §7.4 已把它列为未做项
（"plan_chrome 策略变更"），本轮把它从"未做"升级为"有明确触发条件的风险"。

### F4（P1）补全候选过宽（已有归属）

`mo` 命中 8 个候选（含描述里的 "copy"/"fork"），上游只按命令名过滤。已由本轮的
`xfail_reject` 标记固定住（§3.2），归 LUM-1263。

### F5（P3，工具侧，已修）首帧时序

见 §3.1.2：修掉"空首帧被当证据"和"异步面板被提前截取"两个证据质量问题，顺手纠正了
LUM-1260 场景第 2 面板的标题与画面不符。

### F6（P3，流程）wasm 门禁覆盖不到 Rust 开发分支

`.github/workflows/rust-wasm.yml` 的 `on.push.branches` 只有
`[main, stage6-wasm-bindgen, stage4-tui-interactive]`，**不含 `feature/pi.rs`**：Rust 主线的
wasm 兼容性在合并前没有任何 CI 保护。属于"验证缺口"，不是代码缺陷；本轮只在文档记录
（改 workflow 需要跑一遍 CI 才能验证，本轮磁盘条件不允许）。

## 8. TUI 与 codex/pi 类交互的达成度

把"codex/pi 类 TUI 该有的交互"逐条对照（左列全部来自 §3.2 的实测，不是设计意图）：

| 交互 | 状态 |
|---|---|
| 启动即给出能力提示（快捷键表、`/`、`!`、`Alt+H`） | ✅ 实测 PASS |
| 输入行 + 可见光标 + 占位符 | ✅（≤1 行的草稿）/ ❌ 超宽草稿（F1） |
| `/` 命令补全：候选、选中态、计数器、`Tab` 应用、`Down` 移动 | ✅ 全 PASS |
| `@` 文件补全：候选、收窄、`Tab` 应用 | ✅ 全 PASS |
| 覆盖层式选择器（`/model`）：打开、`Esc` 关闭、provider 列 | ✅ 全 PASS |
| 一次真实 provider 往返（流式落字、模型标识） | ✅ PASS（`faux-model (faux) hello`） |
| `!bash` 工具块：命令 / 输出 / 耗时 | ✅ PASS |
| 工具输出展开（`Ctrl+O`） | ✅ PASS |
| `/help`、`/hotkeys` 文本渲染 | ✅ PASS |
| transcript 滚动 + "回到最新"指示 | ✅ PASS（`↓ Jump to latest message`） |
| 状态行（模型、session、token、`? for help`） | ✅ PASS |
| 长草稿：折行 / 长高 / 光标跟随 | ❌ 缺失（F1，相对 pi-TS 与 Martty 都是差距） |
| 过小终端的显式降级 | ❌ 缺失（F2） |
| 多行编辑（`\n` 在草稿内）与卡片式输入区 | ⚠️ 未测（`render_line` 只画一行，多行草稿必然丢行——与 F1 同根） |

## 9. 复现命令

```bash
# 0) 证据（本轮的截图/dump 就是这样生成的；--bin 换成任意 pi 二进制）
python3 pi-rust/scripts/pty_capture.py \
  --bin <pi> --steps pi-rust/scripts/pty_scenarios/lum1267-interaction-assertions.json \
  --out pi-rust/docs/screenshots/lum1267-interaction-assertions.png --sheet 2
python3 pi-rust/scripts/pty_capture.py \
  --bin <pi> --steps pi-rust/scripts/pty_scenarios/lum1267-narrow-composer.json \
  --out pi-rust/docs/screenshots/lum1267-narrow-composer-23.png --sheet 2
# 断言失败会非零退出；--allow-xpass 只把"意外通过"降级为警告

# 1) 两轴度量
python3 pi-rust/scripts/app_action_coverage.py pi-rust --check-consumed
python3 pi-rust/scripts/extension_event_coverage.py
python3 pi-rust/scripts/extension_event_coverage.py --check-doc    # 文档清单 vs 代码
python3 pi-rust/scripts/extension_event_coverage.py --require tool_call,context   # LUM-1244 的验收门
```

F1 的最小复现：把 `lum1267-interaction-assertions.json` 传一个 200 字符草稿（见 §7 描述），
断言"`▍` 出现在 composer 行"与"草稿尾部可见"今天都会失败。

## 10. 优先级与并发槽位决策

**槽位：本轮零派发。** 看板已有 21 个 `in_progress` + 10 个 `todo`，远超"最多 3 个并发"的上限；
再派发只会让每个 in-flight issue 的构建更慢、磁盘更紧。这与既有被接受的判断一致
（LUM-1241 "3 槽满 → 零派发"）。本轮改为**在 in-flight 之上加验收基线**：新增的 50 条断言是
下一轮任何输入面/布局改动的回归门。

下一轮（磁盘恢复到能构建之后）的建议顺序：

| 优先级 | 事项 | 依据 | 验收 |
|---|---|---|---|
| P0 | composer 多行化：折行 + 长高（上限 30% 或 `min(h/2,12)`）+ 光标跟随视口 + 可见光标 | §7 F1 | 新断言：超宽草稿尾部可见、`▍` 可见、折行后行数增长；pi-tui 单测补 `render_line` 多行与 `plan_chrome` editor 预算 |
| P1 | 最小终端兜底 + 明确提示 | §7 F2 / B1 | PTY 断言：`20×5` 时出现 `terminal too small`，且不 panic |
| P1 | `plan_chrome` 策略变更：扩展 header 不得把 message 压到 1 行（给 message 一个下限，或让 header 参与折叠） | §7 F3 | 单测 + PTY：带 `setHeader` 扩展在 24 行终端里 message ≥ 3 行 |
| P1 | LUM-1244 的扩展事件补齐（15 个，含 `context`） | §4.2 | `extension_event_coverage.py --require …` 全绿 |
| P1 | LUM-1263 补全匹配收窄到命令名 | §7 F4 | 本场景 2 条 `XFAIL` 翻 `XPASS` → 标记改 `reject` |
| P2 | 粘贴原子性（括号粘贴 / 多行粘贴） | B5 | PTY 断言 + 单测 |
| P2 | wasm 门禁覆盖 `feature/pi.rs` | §7 F6 | CI workflow 改动需跑 CI 验证（本轮无法验证，故未改） |
| P3 | composer 卡片化 / 状态 chips | B4 | 视觉回归 |

## 11. 未做 / 不可验证（诚实清单）

1. **本轮没有任何 `.rs` 改动**：无法 `cargo build`/`test`/`clippy`（磁盘 §1），因此 F1–F3 都是
   **只读审计 + PTY 实测**的结论，没有对应修复代码。上一轮 CI（`scripts/ci.sh`）的绿灯不覆盖本轮。
2. **窄场景"修复后"一侧不是本机实测**：合并后的 tip 二进制本轮造不出来，§3.3 的"修复后"引用
   LUM-1261 已入仓的真实 PTY 帧。两条 `probe` 在 tip 上应当 `PASS`，需要下一轮用新二进制确认。
3. **F1 的"多行草稿"未单独实测**：今天 `render_line` 结构上只画一行，多行草稿必然丢行，与 F1 同根；
   未做单独 PTY 用例（列进 §7 表格的 ⚠️ 项）。
4. **B5/B6 只做了代码阅读**：粘贴原子性、宽度阈值降级策略在 pi-rust 侧没有实测结论。
5. **LUM-1244 验收门未跑全绿**：`--require tool_call,context` 仍退出 1，`context` 无变体；
   本轮只是让这个缺口**可被脚本一键复现**，没有补事件。
6. **看板卫生**：21 个 `in_progress` 里包含多轮早期协调任务（LUM-1237/1225/1191/1190/1189/1187/
   1170/1167/1046/1003/986/992/991/982 等），状态已不代表实际在跑；本轮不擅自改他人 issue 状态，
   只在此记录。
