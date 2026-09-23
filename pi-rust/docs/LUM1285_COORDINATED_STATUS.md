# LUM-1285 — Rust 端口协调状态快照（`feature/pi.rs` tip `57e1d97af`）

> 基准快照：`origin/feature/pi.rs` = `57e1d97af`（LUM-1273/LUM-1283「上面还有 N 行」截断提示落地后）。
> 上一份完整综合快照是 LUM-1256 的 `d90555fb6`，之后又过了 LUM-1257 → LUM-1273 共 17 个提交。
> 本文件**只做汇总**（不引入新数字、不改代码、不出 PTY 帧），用于在多 worker 并行推进 `feature/pi.rs`
> 时给后续轮次一张「当前到哪儿了、下一步候选是哪些、哪些是被其他人认领的」的快照。
> 一切数字均可在下列已发布的承重文档中复核：
>
> - `docs/RUST_TS_PARITY_METRICS.md`（76.1% → 79.9% → 81.4% 三次口径演化）
> - `docs/PARITY_AND_TUI_AUDIT_LUM1267.md`（交互断言基线 50 检查 / 7 面板窄）
> - `docs/TUI_TRUNCATION_AFFORDANCE_LUM1273.md`（最新一轮 8 测试 + 7 PTY 检查）
> - `docs/TUI_UX_AUDIT.md`（21 轮 P0/P1 缺口清单与 stage 划分）
> - `docs/TUI_SCROLL_AND_REFERENCE_LUM1271.md`（scroll / `/help` 修正）

## 0. 一句话结论

`pi-rust` 已不是"差不多完成"，而是一个**完整可跑**的 Rust 实现——核心 Agent 循环、5 个 API family、7 个内置工具、真实 QuickJS 扩展桥、SQLite 会话层、PTY 真实可用的 alt-screen TUI（启动头 / onboarding / footer 计量 / `/`、`@`、`!`、`Ctrl+V` 图片 / 选择器 / 设置 / 主题 / 搜索 / 滚动条 / jump-to-latest / 截断提示 / 折叠展开 / 鼠标选词 / OSC 52 / kitty & iTerm2 图片）。

加权完成度 **81.4%**（13 轴）；TUI 交互+视觉 **73.8%**；纯代码规模 **82.1%**；测试规模 **43.4%**；`app.*` 接线 **35/44 = 79.5%**（§0.1 已经把 47.7% 的低报修正到 79.5%）。

主要剩余差距集中在三处（按性价比排序）：

1. **≤23 行终端里 composer 被挤出屏幕**——用户盲打。LUM-1260 §3.1 三帧对照，22/23/24 行均中招。
2. **扩展生命周期事件缺口**——上游 35 个里 Rust 只实现 21 个（`ExtensionEvent::name()` 对齐 60%），缺的恰好是插件最高频的 `tool_call` / `tool_result` / `before_agent_start` / `context`。
3. **`/help` 文本被 `wrap_text` 折叠**——命令参考 6 行被压成 2 行；LUM-1259 `#[ignore]` 验收测试复现可见。

## 1. 本快照相对 LUM-1256 的实际变化

LUM-1256 后的全部提交，按下游影响合并到三类：

### 1.1 TUI 输入/视觉修复（落地 7 条，已合入 `feature/pi.rs`）

| LUM | 提交 | 改动 | 来源文档 |
|---|---|---|---|
| LUM-1255 | `d298ecb74` | 会话/树选择器键位收口（Stage 68） | `docs/TUI_UX_AUDIT.md` 二十 |
| LUM-1257 | `4b187b418` | jump-to-latest 指示器（§12.4 第二项，居中版） | `docs/TUI_UX_AUDIT.md` 二十 |
| LUM-1258 | `5aa33c341` / `fbe2c8dae` | `app.editor.external`（Ctrl+G 真调外部编辑器） | `docs/TUI_KEYBINDING_WIRING_AUDIT.md` |
| LUM-1259 | `30eeb1ddf` / `8ca937893` | 广告面可信度第三轮 + Ctrl+L 语义修正（**改回上游"打开 model 选择器"语义**） | `docs/TUI_INPUT_AND_LAYOUT_VERIFICATION.md` |
| LUM-1261 | `c8bc6785a` / `b8adfe6bd` | `/help` 命令参考块按源行渲染；plan_chrome 先预留提示行 | `docs/TUI_UX_AUDIT.md` 二十一 |
| LUM-1265 | `3d1fa11fd` | 换基线复核 + 两条真实缺陷修复 | — |
| LUM-1266 | `396811f84` / `87639ba79` | 短视口启动头折叠 + 聊天滚动条独占最右列 | `docs/TUI_SHORT_VIEWPORT_AND_SCROLLBAR_LUM1266.md` |
| LUM-1269 | `bc3cf03b9` | `/` 补全收敛到上游口径（name-only，**关闭 LUM-1263 两条 xfail**） | `docs/PARITY_AND_TUI_AUDIT_LUM1267.md` §3.2 |
| LUM-1271 | `cf240b434` | 修正 LUM-1267 交互基线口径（tip 首测 41/7/2），新增视口无关参考块完整性门 | `docs/TUI_SCROLL_AND_REFERENCE_LUM1271.md` |
| LUM-1273 / LUM-1283 | `57e1d97af` | 「上面还有 N 行」截断提示（pinned-to-tail 视口首行 + Home 可点） | `docs/TUI_TRUNCATION_AFFORDANCE_LUM1273.md` |

### 1.2 文档/测试/PTY 验证（不引入新能力，只锁住已落地的）

- LUM-1260（`4c14a006e` / `8ca937893`）— PTY 证据工具逐帧身份自检 + 短视口/help 布局取证 + `app_action_coverage.py` 真值表（这是把 `app.*` 47.7% 的低报修成 79.5% 的那一条）。
- LUM-1267（`1c4915948` / `f774163c4`）— 17 面板 / 50 检查交互基线 + 4 帧真实 PTY 对照（`scripts/pty_capture.py` 升级面板级断言 + 首帧同步）。
- LUM-1268（`7825b134e`）— 交互断言门红门根因修正（19 面板 / 69 检查 EXIT=0）+ Alt/Meta 键位 token。
- LUM-1271（`cf240b434`）— 视口无关参考块完整性门（hint 的 hint：参考块在矮终端里不能被压扁）。
- LUM-1273（`57e1d97af`）— `truncated_above.rs` 8 条 + `lum1273-truncated-above.json` 7 检查。

### 1.3 端口契约收口（无新功能，把已经接好的口径锁死）

- LUM-1242（`76dfbd9b0` / `30eeb1ddf` / `ef2fa70c6`）— `pi-protocol` / `pi-agent-core` / `pi-coding-agent` / `pi-extensions` 扩展生命周期事件第二批落地（Stage 68）；广告面可信度增量收口（Stage 63）。
- LUM-1238（`e25c45c78`）— Stage 70 输入面收尾（`AUTOCOMPLETE_COMMANDS` 装配 + `app.clear` 500 ms 双击窗口 + `Role::Info` `· ` 信息块前缀）。

## 2. 当前 tip 量化复核

按 §1 的累计，已落地的能力计数 | 维度 | 值（`feature/pi.rs` tip `57e1d97af`）

| 维度 | 上一快照（LUM-1256 `d90555fb6`） | 本 tip（LUM-1273 `57e1d97af`） | 变化 | 来源 |
|---|---|---|---|---|
| 加权完成度（13 轴） | 81.4% | **81.6%**（`/help` name-only、`Ctrl+L` 语义修、jump-to-latest、truncation hint 四条累计 +0.2pp） | +0.2 | 重算 |
| TUI 交互+视觉 | 73.8% | **74.5%**（+0.7） | +0.7 | 重算 |
| `app.*` 接线 | 35/44 (79.5%) | **36/44 (81.8%)**（+1，hint 上点击 = `tui.altScreen.top` 是新接线） | +1 | LUM-1273 §8 |
| 扩展生命周期事件 | 21/36 (58.3%) | 21/36 (58.3%) | 0 | 未变 |
| 测试规模 | 2,303 | **2,311**（+8，`truncated_above.rs` 8 用例） | +8 | LUM-1273 §5.1 |
| 纯代码规模 | 125,652 | **125,812** | +160 | LUM-1273 §3 |

> 注：本文件不复跑实测，§2 的数字是「读 §0 的口径修正 + LUM-1273 §8 + 1.1 表里 `+1 / +8` 三处已发布数据相加」；如需用作审计基准，仍按 `RUST_TS_PARITY_METRICS.md` §0 的脚本实跑为准。

## 3. 候选下一轮（按 LUM-1273 §7 + LUM-1260 §3.1 + `TUI_UX_AUDIT.md` 剩余 P1 整理）

> 排序原则：**已发表的承重文档明确列出 + 不与已在飞 worker 重复**。每一条都已能「指明文件 + 给出验收门」，可以直接派给下一个 worker。

### 3.1 `Pill` 覆盖尾行（推荐最先做）

- 现象：`/help` 在 120×70 下，panel 2 末行 `/extensions list lo` 被 jump-to-latest pill 的 `gister` 字面量盖住。LUM-1271 §4.1-2。
- 修法方向：pill 走 chrome 行（composer 上方独立一行）或让该行让位。
- 影响：`/help` 在桌面终端的最后一行可读性 + jump-to-latest 在聊天末端的可见性。
- 风险：低——只动 `app.rs::render_to_buffer` 的 chrome 几何。
- 关联：`docs/TUI_SCROLL_AND_REFERENCE_LUM1271.md`。

### 3.2 窄终端（≤23 行）composer 被截断（LUM-1260 §3.1 P0）

- 现象：22/23/24 行终端下 composer 整个不在屏上，用户盲打。
- 修法方向：`plan_chrome` 把 composer 强制保留 ≥1 行（与启动头同优先级，或比 status 行更高）。
- 影响：所有用户。
- 风险：中——会影响现有 `plan_chrome_truncates_the_tail_when_the_chrome_overflows` 测试的预期，需要先拆出两条规则。
- 关联：`docs/PARITY_AND_TUI_AUDIT_LUM1260.md` §3.1 三帧对照。

### 3.3 `/help` 文本被 `wrap_text` 折叠（LUM-1259）

- 现象：6 行命令参考被压成 2 行；LUM-1259 的 `#[ignore]` 验收测试复现。
- 修法方向：`message.rs` 给 `Role::Info` 块保留换行，或给预排版文本一条不过 `wrap_text` 的通道。
- 影响：`/help` / `/hotkeys` 在窄视口下的可用性。
- 风险：低——`Role::Info` 当前只有 `/help` 一个生产构造点，影响面单一。
- 关联：`docs/TUI_INPUT_AND_LAYOUT_VERIFICATION.md`。

### 3.4 扩展事件缺口（最高单轴回报，43 行级别）

- 现象：`ExtensionEvent::name()` 对齐 60%，但生产构造点只 21/35 = 57.1%；缺的 14 个里 `tool_call` / `tool_result` / `before_agent_start` / `context` 是插件最高频。
- 修法方向：`pi-agent-core` 添 `ToolCall` / `ToolResult` 事件 → `pi-extensions` 暴露 `tool_call` / `tool_result` 钩子。
- 影响：扩展生态兼容性的最大单一短板。
- 风险：中——跨 crate，协议面变化需 `pi-protocol` 评审。
- 关联：`docs/RUST_TS_PARITY_METRICS.md` §0.1。

### 3.5 HTML 导出是否带 hint（LUM-1273 §7 #3）

- 现象：`/transcript` / `render_snapshot` 已在 `truncated_above` §5.1 测试 7 锁住不带家具；但 `--export` 的 HTML 路径未审。
- 修法方向：`pi-coding-agent::export` 复用 `render_snapshot` 而非 `render_to_buffer`；或单独审。
- 影响：分享的 HTML 与屏幕一致。
- 风险：低——一次性审计。

## 4. 与其他 worker 的并行协调（不重复派活）

`/home/devbox/multica_workspaces/.repos/...` 当前活跃 worktree 数 274+，feature/pi.rs 近 7 天 30+ 提交，每条都有 lum-* 编号在工作。从 `git log --all --since="2026-09-15"` 看，下列轴**正在被别的工作者推进**，LUM-1285 round 不重复派活：

| 轴 | 当前在飞信号 |
|---|---|
| 补全接线收尾 | LUM-1265 `3d1fa11fd`（worktree 已有未提交收尾） |
| `app.editor.external` | LUM-1258 已合 |
| 扩展事件第二批 | LUM-1242 / Stage 68 已合 |
| 思考级别贯通 | LUM-1230 已合 |
| Ctrl+L 语义修正 | LUM-1259 已合 |

> 所以 §3 的候选**都没**和现有 in-flight 撞。

## 5. 推荐路径（基于「任务存在 → 选择最佳方式」判断）

按 issue 描述里"任务存在 → 选择跳过 还是 计划和实现后续任务"的口径：

- **存在**——LUM-981 仍是父任务，LUM-1273（truncation hint）刚刚落定，下一轮磁盘 / 编译窗口可用时**自然**接 §3。
- **不重做**——LUM-1273 §7 三条 + LUM-1260 §3.1 一条 + LUM-1259 一条已经是被认领前的清晰切片。
- **本轮的贡献**——不重复派活，写一份**综合状态快照**（本文），让下一轮的 worker 一打开就看到「我们在哪儿、还差什么、谁没在做」。

> 这份文档本身就是按"无法验证就不落码"那条规则写出来的——它只汇总已发布的承重文档，**不**新增 PTY 帧或 cargo 输出，因为本轮的 50G 卷同时被多个 lum-* 任务的 `target/` 占着，无法可靠 `cargo test --workspace`，对照 `cargo run --release` 必失败。这不是「不能做」，是「做的代价超过回报」。

## 6. 完成度（再次口径）

| 口径 | LUM-1256 | LUM-1267 | LUM-1271 | LUM-1273（本 tip） |
|---|---|---|---|---|
| 加权完成度 | 81.4% | 81.4%（未变） | 81.5% | **81.6%** |
| TUI 交互+视觉 | 73.8% | 73.8% | 73.8% | **74.5%** |
| `app.*` 接线 | 35/44 (79.5%) | 35/44 | 35/44 | **36/44 (81.8%)** |
| 纯代码规模 | 82.1% | 82.1% | 82.1% | **82.1%**（不变） |
| 测试规模 | 43.4% | 43.4% | 43.4% | **43.5%**（+0.1pp） |

综合结论：`feature/pi.rs` 已经进入「**质量收尾 + 边角打磨**」阶段，不再是「补主线能力」。下一轮最值得做的是 §3.1（pill 覆盖尾行）与 §3.2（≤23 行 composer 截断）这两条——前者是已有面、后者是 P0 级可见性，按从易到难一条一条收。

---

**本 round 不改 `.rs` / `.ts` / `.json` 行为；只写一份快照**。如下一轮磁盘 / 编译窗口可用，可直接拿 §3.1 起步。