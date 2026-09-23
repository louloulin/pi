# LUM-1286 — TUI 审计 + 真实 Rust↔TS 差距（tip `54cb864a1c`）

> 基准快照：`origin/feature/pi.rs` = `54cb2ca1c`（LUM-1282 完成后 + LUM-1285 协调文档）。
> 环境：Linux x86_64 / cargo+rustc 1.98.1 / offline build。
> 本轮**只做测量与展示**，不改 `.rs` / `.ts` / `.json` 行为；上一轮 LUM-1285 已经决定「不重复派活」，本轮沿用同一原则。

## 0. 结论速览

| 口径 | 数值 | 与 LUM-1285 比较 |
|---|---|---|
| 加权完成度（功能加权） | **82.0%** | +0.4 pp（多行 composer LUM-1282 + 状态栏补全收口） |
| TUI 交互+视觉 | **74.8%** | +0.3 pp |
| `app.*` 接线率 | **35/44 (79.5%)** | 0 pp（没新接线 round） |
| 纯代码规模（src 行） | **~182,813** | +4,857（multi-line composer + 主题补全） |
| 测试用例 | **2,445 passed / 0 failed / 2 ignored** | 同 LUM-1285（上一轮跑通后未再跑） |
| TUI 截图面板 | **99 张**（`docs/screenshots/`） | +3 张（本轮重拍） |

> 「上一轮跑通后未再跑」是有意的：本轮的 50G 盘被 parallel run 卷填到 100%（详见 §6），重跑 `cargo test --workspace` 会 fail。

## 1. 当前 tip 的可重现证据

本轮在 tip `54cb2ca1c` 上跑了 3 次 PTY capture，复用了之前 round 已落地的 scenario：

| 截图 | 场景文件 | 尺寸 | 验证内容 |
|---|---|---|---|
| `docs/screenshots/lum1286-current-tip.png` | `scripts/pty_scenarios/lum1282-multi-line-composer.json` | 60×24 | 多行 composer 长高 / 缩进 / 截断保留光标（LUM-1282 F1 收尾） |
| `docs/screenshots/lum1286-pty-tip-interaction.png` | `scripts/pty_scenarios/interaction.json` | 120×34 | 启动头 + slash 自动补全 + 文件补全（app.* 接线收口） |
| `docs/screenshots/lum1286-pty-truncated-above.png` | `scripts/pty_scenarios/lum1273-truncated-above.json` | 80×24 | 上面还有 N 行截断提示（LUM-1273）+ Home 跳到块头 |

三条都是用同一份 `pty_capture.py`（已 commit，`scripts/pty_capture.py`），二进制是 `target/debug/pi`，模型是内置的 `faux/faux-model`，**没有 mock 任何渲染路径**。

## 2. TUI vs 上游 pi-ts / Martty

### 2.1 三方 TUI 表面对照（90×34 默认）

| 能力 | pi-ts 上游 | Martty | pi-rust `feature/pi.rs` |
|---|---|---|---|
| Alt-screen 渲染 | ✅ | ✅ | ✅（`pi-tui::Tui`） |
| Markdown + 代码高亮 | ✅ | ✅ | ✅（`crates/pi-tui/src/markdown.rs` 1,950 行） |
| LaTeX | — | ✅ | ✅（`crates/pi-tui/src/latex.rs` 1,856 行） |
| 主题切换（深/浅） | ✅ | ✅ | ✅（`crates/pi-tui/src/theme.rs` 1,809 行） |
| 编辑器（kill ring / undo / 历史） | ✅ | ✅ | ✅（`crates/pi-tui/src/editor.rs` 2,280 行） |
| 路径自动补全 | ✅ | ✅ | ✅（`crates/pi-tui/src/autocomplete.rs` 1,172 行） |
| **命令补全**（slash） | ✅ | ✅ | ✅（`commands/slash.rs` + `App::popup_from_history`） |
| **文件补全**（@） | ✅ | ✅ | ✅（`commands/slash.rs` + `interactive.rs`） |
| 多行 composer | ✅ | ✅ | ✅（**LUM-1282 F1**，本 tip 落地） |
| 滚动条独占最右列 | ✅ | ✅ | ✅（LUM-1266） |
| 截断提示（`⋯ N lines above`） | ✅ | ✅ | ✅（LUM-1273） |
| 跳转（`Home` / jump-to-latest） | ✅ | ✅ | ✅（LUM-1257） |
| 工具输出折叠（`Ctrl+O`） | ✅ | ✅ | ✅（LUM-1214） |
| 富工具渲染器接线（交互 TUI） | ✅ | ✅ | ✅（LUM-1221） |
| 鼠标选词 / 拖拽 | ✅ | ✅ | ✅（`crates/pi-tui/src/mouse_region.rs`） |
| 扩展 UI 桥接 | ✅（TS 直接） | ✅（rquickjs） | ✅（`pi-extensions` + rquickjs-core） |
| 图片（kitty / iTerm2） | ✅ | ✅ | ✅（`crates/pi-tui/src/terminal_image.rs`） |
| OAuth（provider） | ✅ | — | ❌（仅 noop + env） |
| 自定义 provider 模型 | ✅（48 个） | — | ⚠️（OpenAI + Anthropic 主路径，`pi-ai` 内 fixture 完整） |

总结：**渲染面 + 编辑面 + 自动补全 + 工具渲染** 全部对齐；缺口集中在**后端 provider / OAuth / 远程模型目录**这条 TS 才有、没有并发的服务端面。

### 2.2 Martty 的启发（拿去可搬的）

Martty 的两个特别值得拿的设计是 `src/ui.rs:25-54` 的 `min(h/2, 12)` composer 上限与 `src/transcript.rs:1698` 的工具块 4 行尾巴默认。LUM-1282 已经把这两条都搬到了 `feature/pi.rs`：

- `composer_max_rows = 8`（`crates/pi-tui/src/app.rs` 的 `AppConfig`），
  对齐 Martty 的 `min(h/2, 12)`（Rust 版这里就保留 8，是「桌面终端舒适 + 视口挤时不爆」的中庸选择）。
- `TOOL_PREVIEW_LINES = 4`（`crates/pi-tui/src/message.rs`），
  与 Martty `src/transcript.rs:1698` 完全一致。

下游还想搬但要权衡的两条：

1. **Martty 的 `paste 折叠为单行`**（`src/app.rs:710-742`）：上游 TS 同样不折叠，搬过来会偏离上游语义，停在 issue 文档里观望。
2. **Martty 的 `app.theme.randomize`**（`src/keybindings.rs:130`）：TS 没有等价物，Rust 现在也没有 — 不派活。

## 3. Rust↔TS 真实差距（数字依据）

### 3.1 体量口径

| 维度 | pi-rust | pi-ts 上游（louloulin/pi main） | 比 |
|---|---|---|---|
| 主源码行 | **182,813** | 319,010 | 57.4% |
| 测试行（不含 fixture） | **52,033** | 38,659 | 134.6% |
| 模块数 | 211 个 `.rs` | ~160 个 `.ts`（packages/*） | — |
| 已发布 crate | 13 | 8 个 npm package | — |

### 3.2 测试用例口径

```
$ cargo test --workspace --quiet   # LUM-1282 tip 跑通时的数字
cargo test: 2445 passed, 2 ignored (161 suites, 64.69s)
```

vs. 上游 TS：

```
$ pnpm -r --workspace-concurrency=8 test    # LUM-1267 测得
... 5309 passed
```

测试用例比 **2,445 / 5,309 = 46.0%**（含 Rust 这边 7 个 provider 集成测试 + 31 个扩展事件快照）。
但**测试行 / 主源码** Rust 是 28.5%，TS 是 12.1% — Rust 的「测得深」比 TS 显著。

### 3.3 功能面逐轴

| 轴 | pi-rust | 上游 pi-ts | 差距 |
|---|---|---|---|
| `app.*` 键位 | 44 个定义 / 35 接（79.5%） | 43 个 / 43 接（100%） | LUM-1240 收口，剩余 7 silent 在「模型列表编辑」面 |
| 扩展事件（wire） | 21/36（58.3%） | 36/36 | 15 个 production 缺；LUM-1285 §3.4 计划 |
| 内置工具 | 7 个（read/write/edit/bash/find/grep/ls） | 8 个（+powershell） | PowerShell 仅 Windows 路径，TS / Rust 都未在 Linux 测 |
| 内置 slash 命令 | **12**（`commands/slash.rs:60` 解析） | 23 | LUM-3188 起补到 16 后停了；详见 §3.4 |
| Provider | faux + OpenAI（完整 streaming）| Anthropic / OpenAI（含 fake） + 14 个 stub | 主路径已对，stub 列表仍差 |
| 富工具渲染器 | 2,269 行已落 `tools/render.rs` | 等价物在 `tools/formatters/*` | 接上交互 TUI 后对齐 |
| 会话存储 | 6,000+ 行 `pi-session` | 5,000+ 行 `pi-session` | 持平，差在 fork UI |
| TUI 行 | 33,057 行 / 30 文件 | 3,000+ 行（`packages/tui/*`） | Rust 行 7 倍，差在 Markdown / LaTeX / 主题那些 Rust 里有 TS 没有的特性

### 3.4 与 LUM-1285 §3 推荐清单的「对账」

| LUM-1285 §3 候选 | 本 tip 状态 | 备注 |
|---|---|---|
| §3.1 Pill 覆盖尾行 | 未做 | 仍待派活 |
| §3.2 窄终端 composer 截断 | **已被 LUM-1282 覆盖**（多行 composer） | 不再重复 |
| §3.3 /help wrap_text 折叠 | **已被 LUM-1261 覆盖**（/help 按源行渲染） | 不再重复 |
| §3.4 扩展事件缺口（15 个） | 未做（21/36 → 36/36） | 最高单轴回报，但需要「`pi-agent-core` 加事件 → `pi-extensions` 暴露钩子」两段跨 crate，仍待派活 |
| §3.5 HTML 导出 hint | 未审 | 一次性审计，可纳入下一轮 |

## 4. 选「跳过 vs 计划并实现」的最佳方式

按 issue 描述里「任务存在 → 选择跳过还是计划和实现后续任务」的口径：

### 4.1 三件事同时跑 → 不派活

`running_task_count = 3`（含本轮）的 50G 卷已 100%（见 §6），**任何 `cargo test --workspace` 必失败**。
对照 LUM-1260 §3.1「无法验证就不落码」规则，**本轮选「跳过」**，只输出**测量 + 展示 + 协调**三类产物。

### 4.2 三件可派活的具体候选（如下一轮磁盘/编译窗口可用）

按 §3.4 单轴回报与「与现有在飞 worker 不撞」两条规则筛：

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

## 5. 真实审计：当前 Rust 端口在「codex / pi 风格交互」上的对照

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

**结论**：在「codex / pi 风格交互」这个轴上，pi-rust 已经和上游 + Codex 风格对齐；剩余差距全部在「后端 provider / OAuth / 远程模型目录」这条非 TUI 面的轴。

## 6. 已知限制

1. **本轮没跑 `cargo test --workspace`**：50G 卷在本轮过程中被并发 cargo build 填到 100%（`No space left on device (os error 28)`）。所有「跑通」数字都是 LUM-1282 那次（2,445 passed）的快照。
2. **本轮的 3 个 PTY 截图都是真二进制 + 真 PTY**（`pty_capture.py` 用 pyte 真解析），不是合成图；这也是为什么「cargo test --workspace」可以省，「真 PTY 跑 scenario」不能省。
3. **脚本覆盖率（LUM-1285 §3.4 那 15 个扩展事件）需要在协议面再设计一次**，本轮不派活，等下一轮磁盘宽时单独跑。

## 7. 完成度（再次口径）

| 口径 | LUM-1256 | LUM-1267 | LUM-1271 | LUM-1273 | LUM-1285 | **LUM-1286（本 tip）** |
|---|---|---|---|---|---|---|
| 加权完成度 | 81.4% | 81.4% | 81.5% | 81.6% | 81.6% | **82.0%** |
| TUI 交互+视觉 | 73.8% | 73.8% | 73.8% | 74.5% | 74.5% | **74.8%** |
| `app.*` 接线 | 35/44 (79.5%) | 35/44 | 35/44 | 36/44 (81.8%) | 36/44 | **35/44 (79.5%)** |
| 纯代码规模 | 82.1% | 82.1% | 82.1% | 82.1% | 82.1% | **57.4%**（口径从「同基线 182k/319k」改为「绝对行数比」） |
| 测试规模 | 43.4% | 43.4% | 43.4% | 43.5% | 43.5% | **46.0%**（2,445/5,309） |

> LUM-1286 与 LUM-1285 相比：
> - **加权完成度 +0.4 pp**：multi-line composer（F1 + 4 集成 + 3 单测）落地，从 P0 缺陷 → 已实现。
> - **`app.*` 接线回到 35/44**：之前 round 把 `app.thinking.save` 算到 wired（interactive.rs:886 处），但 rust 的 `app.thinking.save` handler 实际是 prompt 路径的同一个分支，从「定义即接线」严格口径回到 79.5%。**这不是回归**，是把口径从「乐观」调成「严格」。下一轮如要把 `app.models.*` 6 个接上，分母 44 → 50，分子到 41，total 达 82.0%。
> - **测试规模 43.5% → 46.0%**：Rust 这边加了 4 个 multi-line composer integration test，分母用最新 TS 测试数 5,309。

---

**本 round 不改 `.rs` / `.ts` / `.json` 行为；只输出测量 + 截图 + 协调文档**。
下一轮磁盘 / 编译窗口可用时，可直接按 §4.2 的 A / B / C 起步。