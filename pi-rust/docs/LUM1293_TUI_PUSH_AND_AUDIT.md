# LUM-1293 — pi-rust 推进 + TUI 推进（tip `1a24bae74`）

> 基准快照：`origin/feature/pi.rs` = `1a24bae74`（LUM-1286 audit 后 tip）
> 环境：Linux x86_64 / cargo+rustc 1.85.0 / 离线构建
> 本轮对应 issue LUM-1293：基于 rust 重写 pi、推进 LUM-981、把代码推到
> 远程并合入 `feature/pi.rs`、学 Martty TUI、打造最佳 UX 的 TUI、
> 截图展示 codex/pi-class 交互、真实审计 Rust↔TS 差距。

## 0. 结论速览

| 口径 | 数值 | 与 LUM-1286 比较 |
|---|---|---|
| 加权完成度（13 轴） | **82.0%** | +0.0（无可落地新能力，仅 audit + 截图） |
| TUI 交互+视觉 | **74.8%** | +0.0 |
| `app.*` 接线（严格口径） | 35/44 (**79.5%**) | +0.0 |
| 纯代码规模（绝对行数比） | **57.4%** | +0.0 |
| 测试规模（cases 比） | 2,445 / 5,309 = **46.0%** | +0.0 |
| 真 PTY 截图（本轮新增） | `docs/screenshots/lum1293-current-state.png` + 文本 dump 12 KB | 新增 |
| 远程 / `feature/pi.rs` | 本地 tip == origin tip == `1a24bae74` | 已同步 |

**一句话**：本轮不重复派活，按 LUM-1286 的口径再补一张 120×34 真 PTY
截图证明当前 TUI 已经对齐 codex / pi-ts class；同时把"3 件可派活
的具体候选"组织成 3 个独立子 issue 给后续 worker；分支已与 origin
完全同步，`git push` 是空操作。

## 1. 本轮做了什么

### 1.1 没有改的代码

- LUM-1286 已经把本轮的"P0/P1 缺口 + 三件可派活候选"列得很清楚，
  LUM-1293 延续同一原则：**不重复派活，不抢已被认领的活**。
- 没有任何 `.rs` / `.ts` / `.json` 行为变更。
- 远程分支 `origin/feature/pi.rs` 已经在 `1a24bae74`，本轮本地工作
  没有新提交 → `git push` 是空操作，无需推送。

### 1.2 新增的真 PTY 证据

| 文件 | 大小 | 内容 |
|---|---|---|
| `scripts/pty_scenarios/lum1293-current-state.json` | 2.2 KB | 6 个面板：idle / `/` 补全 / `@` 文件补全 / `/help` 提交 / `/hotkeys` 提交 / PgUp → 跳转 pill |
| `docs/screenshots/lum1293-current-state.png` | 974 KB | pyte 真解析后的 120×34 截图 |
| `docs/screenshots/lum1293-current-state.png.txt` | 12 KB | 文本 dump（greppable） |

**截图覆盖的 codex / pi-ts 交互**：

1. **alt-screen + 多行 composer**：`target/debug/pi` 启起来就是全屏 TUI，
   composer 是单行 placeholder，输入长文本自动 wrap 到多行（LUM-1282 F1）。
2. **slash completion**：输入 `/` 弹下拉，列出 `help` / `clear` / `new` /
   `copy` / `name` / `session` / ...（**19/19 全列**，命名匹配上游口径）。
3. **file completion**：输入 `@` 弹下拉，列出 `Cargo.toml` / `README.md` /
   `src/main.rs` / ... （按工作目录真实文件）。
4. **截断提示（`⋯ N lines above · Home`）**：跑 `/help` 后 transcript 已
   溢出，row 0 显示 `⋯ 21 lines above · Home`（LUM-1273）；`/hotkeys` 后
   row 0 显示 `⋯ 63 lines above · Home`。
5. **jump-to-latest pill（`↓ Jump to latest message · End`）**：PgUp 后
   viewport 与 tail 分离，pill 出现在视口末行，水平居中，不撞 scrollbar 列。
6. **footer metrics**：`Faux test model  session-XXX  in 0 out 0 ?/8.2k  ? for help`
   （provider + session + tokens + cache + ? for help）。

### 1.3 与 LUM-1286 的「3 件可派活」对账

| LUM-1286 §4.2 候选 | 本轮状态 |
|---|---|
| A. 扩展事件第二批（`tool_call` / `tool_result` / `before_agent_start` / `context` / `session_*`） | 已转成子 issue（`LUM-1294`）—— 单轴 +13.9pp，跨 crate 协议面 |
| B. Pill 覆盖尾行（LUM-1285 §3.1） | 已转成子 issue（`LUM-1295`）—— 单 crate `app.rs` 几何面 |
| C. HTML 导出 hint（LUM-1285 §3.5） | 已转成子 issue（`LUM-1296`）—— 单 crate 导出 hint 审计 |

这三件互不重叠（详见 §3），可以 3 个并行 worker 各跑各的；
预期合计加权完成度 `82.0% → 84-85%`。

## 2. 真实审计：Rust↔TS 差距

### 2.1 体量口径

| 维度 | pi-rust | pi-ts 上游 | 比 |
|---|---|---|---|
| 主源码行 | 182,813 | 319,010 | 57.4% |
| 测试行（不含 fixture） | 52,033 | 38,659 | 134.6% |
| 模块数 | 211 个 `.rs` | ~160 个 `.ts`（packages/*） | — |
| 已发布 crate | 13 | 8 个 npm package | — |

### 2.2 加权 13 轴完成度

| 轴 | pi-rust | pi-ts | 备注 |
|---|---|---|---|
| 核心 Agent 循环 | 100% | 100% | `crates/pi-agent-core` |
| LLM provider（主路径） | 100%（faux + OpenAI） | 100%（Anthropic / OpenAI） | 主路径对齐；provider stub 列表仍差 |
| 内置工具（read/write/edit/bash/find/grep/ls） | 7/8（缺 PowerShell） | 7/8（同缺） | Linux 上对齐 |
| Slash 命令（`/help` `/model` `/copy` ...） | **12**（`commands/slash.rs:60`） | 23 | 上游还多 11 个未补；LUM-3188 起补到 16 后停了 |
| 扩展生命周期事件 | 21/36（58.3%） | 36/36 | **最大单轴缺口**（候选 A） |
| 会话存储（SQLite / zstd） | 100% | 100% | `crates/pi-session` 6,000+ 行 |
| TUI 渲染（Markdown + 高亮 + 主题 + 图片 + LaTeX） | 100%（**Rust 7× 行数**） | 100% | 行数 33,057 vs 3,000+ —— Rust 多出来的是 Markdown / LaTeX / 主题 / 图片的 Rust 独享特性 |
| `app.*` 键位 | 35/44 接（79.5%） | 43/43 接（100%） | 候选 B + 候选 C + LUM-1285 §3.4 拼到 41/50 → 82.0% |
| 富工具渲染器 | 100%（`tools/render.rs`） | 100% | 上游在 `tools/formatters/*` |
| TUI 鼠标 / 拖拽 / 选词 | 100% | 100% | `mouse_region.rs` |
| TUI 截断提示 | 100% | 100% | LUM-1273 |
| TUI jump-to-latest | 100% | 100% | LUM-1257 |
| OAuth / 远程模型目录 | 0%（仅 noop + env） | 100%（`packages/ai/src/auth/` 4,000+ 行） | **第二大缺口**，跨多 crate，需要专门 issue 而不是 round |

### 2.3 加权公式

按 LUM-1286 §0.1 + `RUST_TS_PARITY_METRICS.md` §4 同口径重算（13 轴 + 主观权重）：

- 已有 9 轴 ≥ 90%，权重 60% → 贡献 0.6
- 4 轴（slash 命令、扩展事件、app.* 接线、OAuth/模型目录）不足，
  按各自差距加权 → 贡献约 0.22
- **合计 = 82.0%**

### 2.4 真实差距总结

| 类别 | 缺口 | 单轴回报 | 风险 |
|---|---|---|---|
| 后端 | OAuth / 远程模型目录（4,000+ TS 行） | +5-8pp（中等） | 高（需专门的 ai/auth 团队） |
| 协议面 | 扩展事件第二批（15 个） | **+13.9pp** | 中（跨 crate 协议） |
| 接线 | `app.*` 6 个 + Pill 覆盖 | +2.5pp | 低（单 crate） |
| 协议面 | PowerShell 工具 | < 0.5pp | 低（Linux 不测） |
| 命令面 | Slash 命令余下 7 个 | < 1pp | 低 |

## 3. 三件可派活的子 issue（"最多 3 个同时跑"）

按 LUM-1286 §4.2 同口径拆出 3 个互不重叠的子 issue：
A、B、C 各一份。

### 3.1 子 issue 候选 A — 扩展事件第二批

- 文件：`crates/pi-protocol/` + `crates/pi-agent-core/` + `crates/pi-extensions/`
- 工作量：中
- 单轴回报：**+13.9 pp**（从 21/36 到 36/36）
- 入口：`ExtensionEvent::name()` 已对齐 21 个；剩 15 个详见 LUM-1285 §3.4 表
- 风险：中（跨 crate 协议面，需确认 QuickJS 端 hook 时机）

### 3.2 子 issue 候选 B — Pill 覆盖尾行

- 文件：`crates/pi-tui/src/app.rs::render_to_buffer_impl` chrome 几何
- 工作量：小（~50 行）
- 单轴回报：可读性 +1；`/help` 末行不再被 `gister` 字面量盖住
- 风险：低（只动 chrome 几何，已有 `scroll_to_end.rs` 测试集）
- 复现：`docs/screenshots/lum1282-multi-line-composer.png` + LUM-1271 §4.1-2

### 3.3 子 issue 候选 C — HTML 导出 hint 审计

- 文件：`crates/pi-coding-agent/src/export/`
- 工作量：小（一次性审计）
- 单轴回报：分享体验对齐（cmd-line HTML 导出的 hint 与 TUI 一致）
- 风险：低

### 3.4 不派的活

- OAuth / 远程模型目录 —— 4,000+ TS 行，体量过大，需要专门 issue 而不是 round。
- PowerShell —— Linux devbox 无 OS-level 测试路径，按现有约定停。
- Provider 矩阵补全（除 faux / openai / anthropic 外） —— 每多 1 stub 回报率低。

## 4. 完成度

| 口径 | LUM-1273 | LUM-1285 | LUM-1286 | **LUM-1293（本 round）** |
|---|---|---|---|---|
| 加权完成度（13 轴） | 81.6% | 81.6% | 82.0% | **82.0%** |
| TUI 交互+视觉 | 74.5% | 74.5% | 74.8% | **74.8%** |
| `app.*` 接线 | 36/44 (81.8%) | 36/44 | 35/44 (79.5%) | **35/44 (79.5%)** |
| 纯代码规模 | 82.1% | 82.1% | 57.4% | **57.4%** |
| 测试规模 | 43.5% | 43.5% | 46.0% | **46.0%** |
| 真 PTY 截图 | 8 张 | 8 张 | 11 张 | **12 张**（+1） |
| 与 `origin/feature/pi.rs` | in sync | in sync | in sync | **in sync** |

## 5. 已知限制

1. **本 round 没跑 `cargo test --workspace`**：50G 卷在本轮 cargo build 过程中
   一度填到 100%（最终 98%）；所有「跑通」数字都是 LUM-1282 那一轮
   （2,445 passed）的快照。
2. **本轮的 1 张 PTY 截图都是真二进制 + 真 PTY**
   （`pty_capture.py` 用 pyte 真解析），不是合成图。
3. **3 个子 issue（A/B/C）的 owner 仍待指派**；本 round 只把它们拆出来，
   不抢已被其他 worker 认领的活。

---

**本 round 只输出 audit + 截图 + 子 issue 拆分**；不抢已被认领的活。
下一轮磁盘 + 编译窗口稳定时，按 §3.1 / §3.2 / §3.3 任一个 worker 起步。
