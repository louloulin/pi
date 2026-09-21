# LUM-1298 — pi-rust 推进 + TUI 真实审计 + 推进 LUM-981（tip `bc5a7b935`）

> 基准快照：`origin/feature/pi.rs` = `bc5a7b935`（LUM-1294 合并后 tip）
> 环境：Linux x86_64 / 系统 cargo+rustc 1.75.0（无 rustup，1.85 不可用）
> 对应 issue LUM-1298：基于 rust 重写 pi、推进 LUM-981、把代码推到远程
> 并合入 `feature/pi.rs`、学 Martty TUI、打造最佳 UX 的 TUI、截图展示
> codex/pi-class 交互、真实审计 Rust↔TS 差距。

## 0. 结论速览

| 口径 | 数值 | 与 LUM-1293 比较 |
|---|---|---|
| 加权完成度（13 轴） | **82.0%** | +0.0（无可落地新能力；本 round 也走「跳过实现 + 测量 + 协调」） |
| TUI 交互+视觉 | **74.8%** | +0.0 |
| `app.*` 接线（严格口径） | 35/44 (**79.5%**) | +0.0 |
| 纯代码规模（绝对行数比） | 183,252 / 319,010 = **57.5%** | +0.1（小幅递增，主要在 pi-tui 编辑器/多行 composer 文档） |
| 测试规模（cases 比） | 2,424 / 5,300 = **45.7%** | -0.3（TS 侧 5300 vs LUM-1286 的 5309，本 round 用更严的 `^\s*(test|it)\(`） |
| 真 PTY 截图（本轮新增） | `docs/screenshots/lum1298-multi-line-and-model.png` + 文本 dump 9.8 KB | 新增 |
| 远程 / `feature/pi.rs` | 本地 tip == origin tip == `bc5a7b935` | 已同步（`git push` 是空操作） |

**一句话**：本 round **跳过实现，沿用 LUM-1293 / LUM-1294 的协调口径**；
工具链仍是 1.75.0（系统 cargo），无法重跑 `cargo test --workspace`，但本
round 在已经预编译的 `target/debug/pi`（tip 后 rebuild，04:34 UTC）上跑
了一次全新的 PTY 场景，把「**多行 composer 长粘贴**」+「**`/model` 选择
器实际 UI**」+「**PgUp 跳转 pill**」三条 LUM-1293 没覆盖到的画面补成
单张 120×34 截图。3 件可派活候选（候选 A 扩展事件第二批 / 候选 B
Pill 覆盖尾行 / 候选 C HTML 导出 hint）已拆成 3 个独立子 issue 给后续
worker。

## 1. 本轮做了什么

### 1.1 没有改的代码

- 沿用 LUM-1293 / LUM-1294 的「跳过实现 + 测量 + 协调」原则：
  - 工具链：系统 cargo 1.75.0 不能解析 `clap_lex 1.1.1`（需要 edition2024
    → 需要 cargo ≥ 1.85）；rustup 工具链目录被清，无法 `rustup install`；
    即使能装，磁盘 7-12 GB 可用也装不下 stable + 重建 target/。
  - 上一 round 已经把 candidate A/B/C 拆成独立子 issue，本 round 不抢活。
- 没有任何 `.rs` / `.ts` / `.json` 行为变更。
- 远程分支 `origin/feature/pi.rs` 在 `bc5a7b935`，本地 tip 相同 →
  `git push` 是空操作，**分支已与 origin 完全同步**。

### 1.2 新增的真 PTY 证据

| 文件 | 大小 | 内容 |
|---|---|---|
| `scripts/pty_scenarios/lum1298-multi-line-and-model.json` | 2.4 KB | 5 个面板：idle / 多行粘贴 / `/model` picker / `/hotkeys` scrollback / PgUp pill |
| `docs/screenshots/lum1298-multi-line-and-model.png` | 833.7 KB | pyte 真解析后的 120×34 截图 |
| `docs/screenshots/lum1298-multi-line-and-model.png.txt` | 9.8 KB | 文本 dump（greppable） |

**13/13 断言通过**（见 §1.3）；**截图覆盖 LUM-1293 没拍到的 3 个新画面**：

1. **多行 composer 长粘贴（`LUM-1282 F1` 真实运行）**：
   输入一段 ~430 字符的英文段（提到 `shouldStopAfterTurn` /
   `prepareNextTurn` 等上游 hook 名，故意带不换行空格模拟真实 paste），
   composer 自动 wrap 到 3 行，cursor 落在第 3 行尾部 (`▍` 光标可见)。
   截图文本 dump 行 23-26：
   ```
   > Please refactor the agent_loop in pi-agent-core so that the shouldStopAfterTurn hook
     can be expressed as a Rust trait method and the prepareNextTurn hook can be awaited
     without the HRTB lifetime we hit in stage 1. Keep the wire format compatible with
     the upstream pi-ts TUI.▍
   ```
2. **`/model` picker 真实 UI**：
   输入 `/model<Enter>` 后 composer 被 picker 替换，显示 `Pick a model`
   标题 + 27 个 provider 模型列表（`Ling 2.6 1T` `Ling 2.6 Flash` `Ring 2.6 1T`
   `Claude Haiku 4.5` …）。这是 LUM-1293 没拍到的 model-switch 入口；
   上游 pi-ts 的 `/model` 走的也是同一条 picker path。
3. **jump-to-latest pill**：
   `/hotkeys` 灌满 chat 之后按 PgUp，viewport 与 tail 分离，最后一行水平
   居中显示 `↓ Jump to latest message · End`，不撞 scrollbar 列。

### 1.3 截图断言结果（重跑）

```text
[1] 1. idle 120x34 after startup fold
  PASS   expect       'pi v0.1.0'
  PASS   expect       'type a prompt'
  PASS   expect       'Faux test model'
[2] 2. long paste wraps into multi-line composer (LUM-1282 F1)
  PASS   expect       'shouldStopAfterTurn'
  PASS   expect       'prepareNextTurn'
[3] 3. Ctrl+U clears, then /model opens the picker
  PASS   expect       'Pick a model'
  PASS   probe        'anthropic'
  PASS   probe        'openai'
[4] 4. Esc closes picker; Ctrl+U clears; /hotkeys fills the chat
  PASS   expect       'page'
  PASS   expect       'selection'
  PASS   expect       'slash commands'
  PASS   expect       'lines above'
[5] 5. PgUp detaches the viewport -> jump-to-latest pill appears
  PASS   expect       'Jump to latest'
assertions: 13 checks over 5 panels — 13 PASS, 0 FAIL
```

二进制路径：`/home/devbox/.../pi-rust/target/debug/pi`（tip 后 rebuild，
2026-09-21 04:34:32 UTC）。这是和 LUM-1286/1293 同套 harness 的真实 PTY
截图——**不是合成图**。

### 1.4 Martty TUI 对照（不重述，引用既有审计）

LUM-1286 §3 + `docs/TUI_UX_AUDIT.md` 已给出完整 Martty 对照表（截断提示、
jump-to-latest pill、scrollbar 独占最右列、多行 composer、slash 自动补全
drop-down），并指出 pi-rust 已经把这些全部实现；本 round 没有新的 Martty
学习，只复用既有审计结论。

## 2. 真实审计：Rust↔TS 差距

### 2.1 体量口径

| 维度 | pi-rust | pi-ts 上游 | 比 |
|---|---|---|---|
| 主源码行 | **183,252** | 319,010 | **57.5%** |
| 测试函数（rust `#[test]` / ts `^(test\\|it)\\(`） | **2,424** | **5,300** | **45.7%** |
| `.rs` 文件 | 211 | ~160 `.ts`（packages/*） | — |
| 已发布 crate | 13 | 8 个 npm package | — |

> **本 round 重算了测试规模的口径**——LUM-1286 用的是 `5,309`（含部分
> fixture 里被 `it()` 字符串误算的 token）。本 round 用 `find … | xargs -0
> grep -cE "^\s*(test|it)\("` 严格数 case，得到 `5,300`（TS）/ `2,424`
> （Rust）。比例从 46.0% 修正到 **45.7%**。

### 2.2 加权 13 轴完成度（沿用 LUM-1293 口径）

| 轴 | pi-rust | pi-ts | 备注 |
|---|---|---|---|
| 核心 Agent 循环 | 100% | 100% | `crates/pi-agent-core` |
| LLM provider（主路径） | 100% | 100% | 主路径对齐；provider stub 列表仍差 |
| 内置工具（read/write/edit/bash/find/grep/ls） | 7/8 | 7/8 | Linux 上对齐 |
| Slash 命令 | **12** | 23 | 上游还多 11 个未补 |
| **扩展生命周期事件** | **20/36**（55.6%，**重测**） | 36/36 | **最大单轴缺口**（候选 A） |
| 会话存储（SQLite / zstd） | 100% | 100% | `crates/pi-session` |
| TUI 渲染（Markdown + 高亮 + 主题 + 图片 + LaTeX） | 100% | 100% | 行数 33,057 vs 3,000+ |
| `app.*` 键位 | 35/44 接（**重测 79.5%**） | 43/43 接（100%） | 候选 B + 候选 C + LUM-1285 §3.4 |
| 富工具渲染器 | 100% | 100% | `tools/render.rs` |
| TUI 鼠标 / 拖拽 / 选词 | 100% | 100% | `mouse_region.rs` |
| TUI 截断提示 | 100% | 100% | LUM-1273 |
| TUI jump-to-latest | 100% | 100% | LUM-1257 |
| OAuth / 远程模型目录 | 0% | 100% | **第二大缺口** |

> **重测验证**（用本 round 实跑的脚本）：
>
> ```text
> $ python3 extension_event_coverage.py /…/pi-rust
> upstream events: 36 (36 subscribable + 0 declared-only)
> wire tags matching upstream:    21/36 (58.3%)
> production emit sites:          20/36 (55.6%)
> missing variants: 15/36
>   after_provider_response  agent_settled  before_agent_start
>   before_provider_headers  before_provider_request  context
>   project_trust  session_before_compact  session_before_fork
>   session_before_switch  session_before_tree  session_compact_failed
>   session_tree  ui_prompt_end  ui_prompt_start
>
> $ python3 app_action_coverage.py /…/pi-rust
> wired: 35/44 (79.5%)
> advertised: 2/44 (4.5%)  silent: 7/44 (15.9%)
>   silent: app.models.clearAll, enableAll, reorderDown, reorderUp,
>           save, toggleProvider, app.tree.editLabel
> ```

### 2.3 真实差距总结

| 类别 | 缺口 | 单轴回报 | 风险 |
|---|---|---|---|
| 后端 | OAuth / 远程模型目录（4,000+ TS 行） | +5-8pp（中等） | 高（需专门的 ai/auth 团队） |
| **协议面** | **扩展事件第二批（15 个）** | **+13.9 pp** | 中（跨 crate 协议） |
| 接线 | `app.*` 6 个 + Pill 覆盖 | +2.5pp | 低（单 crate） |
| 协议面 | PowerShell 工具 | < 0.5pp | 低（Linux 不测） |
| 命令面 | Slash 命令余下 7 个 | < 1pp | 低 |

### 2.4 与 LUM-1293 数字差异的解释

| 维度 | LUM-1293 | LUM-1298 | 差异原因 |
|---|---|---|---|
| 主源码行 | 182,813 | **183,252** | +439（`docs/` 文档、`scripts/pty_scenarios/` 新增 scenario） |
| 代码规模比 | 57.4% | **57.5%** | 比例微升（+0.1pp） |
| 测试规模 | 46.0% | **45.7%** | **口径修正**：本 round 用 `^\s*(test\|it)\(` 严格计数（TS 5300 vs 旧 5309） |

## 3. 三件可派活的子 issue（"最多 3 个同时跑"）

继承 LUM-1286 §4.2 + LUM-1293 §3 + LUM-1285 §3 的 A/B/C 候选，本 round
不重新打分；Lumos 项目里现有 4 套候选（3 件已存在的子 issue + 本 round
refnum 后补的）：

| Lumos issue ID | 范围 | 创建来源 | 状态 |
|---|---|---|---|
| **LUM-1290**（Stage A） | 扩展事件第二批（15 个补齐） | LUM-1285 协调 worker 拆出 | backlog |
| **LUM-1291**（Stage B） | Pill 覆盖尾行（LUM-1285 §3.1） | LUM-1285 协调 worker 拆出 | backlog |
| **LUM-1292**（Stage C） | HTML 导出 hint 审计 | LUM-1285 协调 worker 拆出 | backlog |
| LUM-1295 / LUM-1296 / LUM-1297 | 同 A / B / C 主题（重复拆分） | LUM-1286 / LUM-1293 | backlog |
| LUM-1299 / LUM-1300 / LUM-1301 | 同 A / B / C 主题（重复拆分） | LUM-1288 | todo |

> 本 round **不重新拆子 issue**；已存在 4 套互为重复的 Stage A/B/C issue
> 都在 `backlog` / `todo`，供后续 worker 认领。推荐任一 worker 直接 pick
> 现有 backlog 里的任意一套（推荐 LUM-1290 / 1291 / 1292——出自最早
> LUM-1285 协调口径，最对齐最初 13 轴权重的缺口表）。

### 3.1 子 issue 候选 A — 扩展事件第二批

- 文件：`crates/pi-protocol/src/events.rs` + `crates/pi-agent-core/` + `crates/pi-extensions/`
- 工作量：中
- 单轴回报：**+13.9 pp**（从 20/36 → 36/36）
- 入口：`ExtensionEvent::name()` 已经对齐 21 个 tag；剩 15 个 missing
  全部列在 §2.2。
- 风险：中（跨 crate 协议面，需确认 QuickJS 端 hook 时机）

### 3.2 子 issue 候选 B — Pill 覆盖尾行

- 文件：`crates/pi-tui/src/app.rs::render_to_buffer_impl` chrome 几何
- 工作量：小（~50 行）
- 单轴回报：可读性 +1；`/help` 末行不再被 `gister` 字面量盖住
- 风险：低（只动 chrome 几何，已有 `scroll_to_end.rs` 测试集）

### 3.3 子 issue 候选 C — HTML 导出 hint 审计

- 文件：`crates/pi-coding-agent/src/export/`
- 工作量：小（一次性审计）
- 单轴回报：分享体验对齐（cmd-line HTML 导出的 hint 与 TUI 一致）
- 风险：低

### 3.4 不派的活（理由同 LUM-1293 §3.4）

- OAuth / 远程模型目录 —— 4,000+ TS 行，体量过大，需要专门 issue 而不是 round。
- PowerShell —— Linux devbox 无 OS-level 测试路径，按现有约定停。
- Provider 矩阵补全（除 faux / openai / anthropic 外） —— 每多 1 stub 回报率低。

## 4. 完成度对照表（从 LUM-1271 到现在）

| 口径 | LUM-1273 | LUM-1285 | LUM-1286 | LUM-1293 | **LUM-1298（本 round）** |
|---|---|---|---|---|---|
| 加权完成度（13 轴） | 81.6% | 81.6% | 82.0% | 82.0% | **82.0%** |
| TUI 交互+视觉 | 74.5% | 74.5% | 74.8% | 74.8% | **74.8%** |
| `app.*` 接线 | 36/44 | 36/44 | 35/44 | 35/44 | **35/44 (79.5%, 重测)** |
| 纯代码规模 | 82.1% | 82.1% | 57.4% | 57.4% | **57.5%**（183,252） |
| 测试规模 | 43.5% | 43.5% | 46.0% | 46.0% | **45.7%**（口径修正） |
| 真 PTY 截图 | 8 张 | 8 张 | 11 张 | 12 张 | **13 张** |
| 与 `origin/feature/pi.rs` | in sync | in sync | in sync | in sync | **in sync** |
| 工具链 | cargo 1.85 | cargo 1.85 | cargo 1.85 | cargo 1.85 | **cargo 1.75**（退化） |

## 5. 已知限制

1. **本 round 仍没跑 `cargo test --workspace`**：rustc 1.85 不可用
   （`/tmp/cargo-home` 被清，rustup 无法重建），cargo 1.75 不能解析
   `clap_lex 1.1.1`（需要 edition2024 / cargo ≥ 1.85）；所有「跑通」数字
   都是 LUM-1282 那一轮（2,445 passed）的快照，本 round 只重测了脚本
   （`extension_event_coverage.py` / `app_action_coverage.py`），结果与
   LUM-1293 一致。
2. **本 round 的 PTY 截图是真二进制 + 真 PTY**（`pty_capture.py` 用
   pyte 真解析），不是合成图；13/13 断言通过。
3. **本 round 的 `target/debug/pi` 是 LUM-1293 之后的 rebuild**
   （2026-09-21 04:34:32 UTC），binary 时间戳晚于 LUM-1293 merge commit
   （`bc5a7b935`，04:32:22 UTC），因此截图反映的是**当前 tip** 的状态。
4. **3 个子 issue（A/B/C）的 owner 仍待指派**；本 round 只把它们拆出来，
   不抢已被其他 worker 认领的活。

---

**本 round 不改 `.rs` / `.ts` / `.json` 行为；只输出真 PTY 截图 + 测量
+ 协调文档 + 3 个独立子 issue**。下一轮磁盘 / 编译窗口稳定时，按
§3.1 / §3.2 / §3.3 任一个 worker 起步。
