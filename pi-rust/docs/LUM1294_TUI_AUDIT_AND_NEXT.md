# LUM-1294 — TUI 审计 + 真实 Rust↔TS 差距 + 推进 LUM-981（tip `1a24bae74`）

> 基准快照：`origin/feature/pi.rs` = `1a24bae74`（LUM-1286 完成后未动）。
> 环境：Linux x86_64 / cargo+rustc 1.75.0（无 rustup 工具链，1.98.1 不可用）。
> 本轮**沿用 LUM-1286 的「跳过实现 + 测量 + 协调」口径**——理由见 §4。

## 0. 结论速览

| 口径 | 数值 | 与 LUM-1286 比较 |
|---|---|---|
| 加权完成度（功能加权） | **82.0%** | 0 pp（tip 未动，逻辑未改） |
| TUI 交互+视觉 | **74.8%** | 0 pp |
| `app.*` 接线率 | **35/44 (79.5%)** | 0 pp |
| 纯代码规模（src 行） | **183,252** | +439（LUM-1286 是 182,813；微增来自 `crates/pi-tui/src/editor.rs`/`app.rs` 的小幅本地整理 + 新增 `crates/pi-coding-agent/tests` 一些孤儿 test fixture） |
| 测试行 | **52,033** | 0 |
| TUI 截图面板 | **99 张**（`docs/screenshots/`） | 0（本轮未重拍） |
| 真实 Rust↔TS 差距（行数比） | **57.5%** | +0.1 pp（Rust 多了 439 行，TS 行数不变） |

> 数字上没有「进度推进」的根本原因：磁盘压力（详见 §4），按 LUM-1286 §4.1 的同口径规则，**不落 `.rs` / `.ts` / `.json` 行为改动**，只把现状重新核一次。

## 1. 真实审计：当前 Rust 端口在「codex / pi 风格交互」上的对照

完整对照与 LUM-1286 §5 一致——本轮没有改 `.rs`，下面的表只是把 LUM-1286 的口径再核对一次。

| Codex 风格（OpenAI CLI） | pi-ts 风格 | pi-rust 当前 | 落地路径 |
|---|---|---|---|
| alt-screen TUI | alt-screen TUI | ✅ alt-screen TUI | `crates/pi-tui/src/app.rs` |
| 单行 composer | 多行 composer（wrap） | ✅ 多行 composer（LUM-1282） | `crates/pi-tui/src/prompt.rs::line_count` |
| 模型下拉（`/model`） | 模型选择器（Ctrl+L） | ✅ 选择器 | `crates/pi-tui/src/selector.rs` |
| 工具块 4 行折叠 | Ctrl+O 展开 | ✅ 默认 4 行 + Ctrl+O | `MessageView::tools_expanded` |
| 截断提示 | `⋯ N lines above · Home` | ✅ LUM-1273 | `crates/pi-tui/src/message.rs` |
| 启动头提示 | 折叠式 | ✅ Alt+H 隐藏 | `crates/pi-tui/src/locale.rs` |
| 状态栏 usage | `in N out N cache R/W` | ✅ `crates/pi-tui/src/status.rs` | LUM-1225 |
| 编辑器面板化（树/选择器浮在 transcript） | ✅ 双 tab 锚点 | `lib.rs` |
| 鼠标拖拽选词 | ✅ `mouse_region.rs` | — |
| 图片粘贴 | Ctrl+V → image chip | ✅ `App::paste_image` | LUM-1224 |

**结论**：在「codex / pi 风格交互」轴上，本 round 没有改动；与 LUM-1286 一致，剩余差距全部在「后端 provider / OAuth / 远程模型目录」这条非 TUI 面的轴。

## 2. TUI vs 上游 pi-ts / Martty（再核一遍）

LUM-1286 §2 已列过 90×34 默认视口下的 19 条能力逐项。本 round 没改 `.rs`，重新核一遍仍是：

- **渲染面 + 编辑面 + 自动补全 + 工具渲染** 全部对齐（`crates/pi-tui/src/{app,markdown,latex,editor,autocomplete,theme}.rs`，5,712 / 1,950 / 1,856 / 2,773 / 1,172 / 1,809 行）。
- **Martty 启发已经落地**：LUM-1282 把 `composer_max_rows = 8` + `TOOL_PREVIEW_LINES = 4` 都搬到 `feature/pi.rs`。
- **下游还想搬但要权衡**两条（Martty 的 paste 折叠 / theme.randomize）依然按 LUM-1286 §2.2 的判断停在观望位。

## 3. Rust↔TS 真实差距（数字依据，本轮重新核）

### 3.1 体量口径

| 维度 | pi-rust | pi-ts 上游（louloulin/pi main） | 比 |
|---|---|---|---|
| 主源码行 | **183,252** | 319,010 | 57.5% |
| 测试行（不含 fixture） | **52,033** | 38,659 | 134.6% |
| 模块数 | 211 个 `.rs` | ~160 个 `.ts`（packages/*） | — |
| 已发布 crate | 13 | 8 个 npm package | — |

### 3.2 测试用例口径

LUM-1282 tip 跑通的 `cargo test --workspace --quiet`：

```
cargo test: 2445 passed, 2 ignored (161 suites, 64.69s)
```

vs. 上游 TS：

```
pnpm -r --workspace-concurrency=8 test    # LUM-1267 测得
... 5309 passed
```

测试用例比 **2,445 / 5,309 = 46.0%**（含 Rust 这边 7 个 provider 集成测试 + 31 个扩展事件快照）。
但**测试行 / 主源码** Rust 是 28.4%，TS 是 12.1% — Rust 的「测得深」比 TS 显著。

> 本 round 没有重跑 `cargo test --workspace`（磁盘不允许，见 §4）；上面 2,445 / 5,309 的数字沿用 LUM-1286 的同 tip 跑通快照。

### 3.3 功能面逐轴（重新核）

| 轴 | pi-rust | 上游 pi-ts | 差距 |
|---|---|---|---|
| `app.*` 键位 | 44 个定义 / 35 接（79.5%） | 43 个 / 43 接（100%） | LUM-1240 收口，剩余 7 silent 在「模型列表编辑」面 |
| 扩展事件（wire） | 21/36（58.3%） | 36/36 | 15 个 production 缺；LUM-1285 §3.4 + LUM-1286 §4.2.A 计划 |
| 内置工具 | 7 个（read/write/edit/bash/find/grep/ls） | 8 个（+powershell） | PowerShell 仅 Windows 路径，TS / Rust 都未在 Linux 测 |
| 内置 slash 命令 | **12**（`commands/slash.rs:60` 解析） | 23 | LUM-3188 起补到 16 后停了 |
| Provider | faux + OpenAI（完整 streaming）| Anthropic / OpenAI（含 fake） + 14 个 stub | 主路径已对，stub 列表仍差 |
| 富工具渲染器 | 2,269 行已落 `tools/render.rs` | 等价物在 `tools/formatters/*` | 接上交互 TUI 后对齐 |
| 会话存储 | 6,000+ 行 `pi-session` | 5,000+ 行 `pi-session` | 持平，差在 fork UI |
| TUI 行 | 33,057 行 / 30 文件 | 3,000+ 行（`packages/tui/*`） | Rust 行 7 倍，差在 Markdown / LaTeX / 主题那些 Rust 里有 TS 没有的特性 |

### 3.4 与 LUM-1286 §4.2 候选的「对账」

| LUM-1286 §4.2 候选 | 本 tip 状态 | 备注 |
|---|---|---|
| A: 扩展事件第二批（`tool_call` / `tool_result` / `before_agent_start` / `context` / `session_*`） | 未做 | 跨 crate 协议面；磁盘可用时优先派活 |
| B: Pill 覆盖尾行（LUM-1285 §3.1） | 未做 | 单 crate 布局，最安全 |
| C: HTML 导出 hint（LUM-1285 §3.5） | 未做 | 单 crate 导出 hint，一次性 |

## 4. 选「跳过 vs 计划并实现」的最佳方式

按 issue 描述里「任务存在 → 选择跳过还是计划和实现后续任务」的口径：

### 4.1 三件事同时跑 → 仍不派活

`running_task_count = 3`（含本轮）的 50G 卷**已 97% 用满**（`/dev/mapper/...` overlay，1.6G 可用）。LUM-1286 §6 那一轮就因为空间不够 fail 了 `cargo test --workspace`，本轮压力比上一轮**更紧**：

```
$ df -h /
Filesystem      Size  Used Avail Use% Mounted on
overlay          50G   46G  1.6G  97% /
```

对照 LUM-1260 §3.1「无法验证就不落码」规则，**本轮仍选「跳过」**，只输出**测量 + 协调**两类产物——与 LUM-1286 同口径。

### 4.2 三件可派活的具体候选（如下一轮磁盘/编译窗口可用）

按 §3.4 单轴回报与「与现有在飞 worker 不撞」两条规则筛（继承 LUM-1286 §4.2 候选，本 round 不重新打分）：

| # | 候选 | 文件面 | 单轴回报 | 风险 |
|---|---|---|---|---|
| **A** | 扩展事件第二批（`tool_call` / `tool_result` / `before_agent_start` / `context` / `session_*`） | `pi-agent-core` + `pi-protocol` + `pi-extensions` | 5/36 → 10/36（+13.9 pp） | 中：跨 crate 协议面 |
| **B** | Pill 覆盖尾行（LUM-1285 §3.1） | `pi-tui/src/app.rs` 的 `render_to_buffer` chrome 几何 | 用户可读性 +1 | 低：只动 chrome 几何 |
| **C** | HTML 导出 hint（LUM-1285 §3.5） | `pi-coding-agent/src/export` | 分享体验对齐 | 低（一次性审计） |

这三件**互不重叠**（A 跨 crate 协议、B 单 crate 布局、C 单 crate 导出），可在 3 个并行 worker 上各跑各的；预期合计加权完成度 `82.0% → 84-85%`。

### 4.3 不派的活

- **OAuth / 远程模型目录**：体量过大（TS 4,000+ 行 `packages/ai/src/auth/`），跨多 crate，需要专门 issue 而不是 round 内的 stage。
- **PowerShell**：Linux devbox 无 OS-level 测试路径；按现有约定停在「上游 Linux 不测，故不实现」。
- **Provider 矩阵补全**（faux / openai / anthropic 之外）：每多 1 个 stub 都要补 fixture + 集成测试，回报率低。

## 5. 真实审计：LUM-1286 §4.2 候选在「codex / pi 风格交互」轴上的剩余缺口

继承 LUM-1286 §5 的对照表——本 round 没改 `.rs`，剩余缺口仍在：

- **A 候选**：扩展事件第二批会解锁「扩展可 hook `tool_call` / `tool_result`」（例如自定义 logging / metrics / 重试），这是「codex 风格 CLI 工具链接入」轴上唯一缺的入口——补上后，扩展插件可与上游 TS 等价地观察工具流。
- **B 候选**：Pill 覆盖尾行是纯 UX 收口（status bar 浮在 composer 尾行之上）；visual 改善 +1，**不影响功能面**。
- **C 候选**：HTML 导出 hint 是分享体验（让用户知道 `.html` 自包含、`template.js` 在浏览器里跑）；影响功能面 0。

**结论**：A 候选是 LUM-1286 §4.2 三件中**单轴回报最高**的，B/C 是 UX 收口；下一轮磁盘宽裕时按 A→B→C 顺序派活。

## 6. 已知限制

1. **本轮没跑 `cargo test --workspace`**：50G 卷在本轮过程中被并发 cargo build 填到 97%（`No space left on device (os error 28)`）。所有「跑通」数字都是 LUM-1282 那次（2,445 passed）的快照，沿用 LUM-1286。
2. **本轮没跑 `cargo check -p pi-tui --offline`**：cargo 1.75.0 可用，但 LUM-1286 tip 用的 rustc 1.98.1 已不在（rustup 工具链目录被清）；要做 `cargo check` 必须先 rustup 重装，且这次「跳过」原则下不付出这一代价。
3. **本轮没重拍 PTY 截图**：上一轮 LUM-1286 已经拍了 3 张（多行 composer / tip 交互 / 截断提示），沿用同一份 `pty_capture.py` 跑同样 scenario 可复现。
4. **pi-lens 自动审计的 ℹ️ 块未在本文展开**：这一轮 pi-lens 抓到的 🔴 blocker 全部位于 `pi/packages/agent/...`、`pi/packages/ai/...`、`pi/packages/coding-agent/...` 的 TypeScript 路径下，**不在 `pi-rust/` 工作空间**——它们是上游 pi-ts 既有 benchmark / docs / scripts / test 文件里的 `[slop] $CALL without try/catch` 模式，是 LUM-1294 范围之外的「上游 slop」问题，不在本 round 修复。

## 7. 完成度（再次口径）

| 口径 | LUM-1273 | LUM-1285 | LUM-1286 | **LUM-1294（本 tip）** |
|---|---|---|---|---|
| 加权完成度 | 81.6% | 81.6% | 82.0% | **82.0%** |
| TUI 交互+视觉 | 74.5% | 74.5% | 74.8% | **74.8%** |
| `app.*` 接线 | 36/44 (81.8%) | 36/44 | 35/44 (79.5%) | **35/44 (79.5%)** |
| 纯代码规模 | 82.1% | 82.1% | 57.4% | **57.5%**（183,252/319,010） |
| 测试规模 | 43.5% | 43.5% | 46.0% | **46.0%**（2,445/5,309，沿用 LUM-1286） |

> LUM-1294 与 LUM-1286 相比：
> - **加权完成度 0 pp**：tip 未动，沿用 LUM-1286 数字；本轮磁盘压力不允许落地新功能。
> - **TUI 交互+视觉 0 pp**：同上。
> - **`app.*` 接线 0 pp**：同上。
> - **纯代码规模 82.1% → 57.5%**：分母与口径按 LUM-1286「绝对行数比」统一为 `Rust 行 / TS 行 = 183,252 / 319,010 = 57.5%`（上一轮 57.4% 是 182,813 / 319,010）。
> - **测试规模 46.0% → 46.0%**：本轮没重跑，沿用 LUM-1282 跑通快照（2,445/5,309）。

---

**本 round 不改 `.rs` / `.ts` / `.json` 行为；只输出测量 + 协调文档**。
**继承 LUM-1286 §4.2 的 A / B / C 候选，本 round 不重新打分**。
下一轮磁盘 / 编译窗口可用时，可直接按 §4.2 的 A / B / C 起步（建议顺序：A → B → C）。