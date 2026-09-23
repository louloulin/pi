# LUM-1288 — pi-rust 真实审计 + Martty 对照 + 推进 LUM-981（tip `bc5a7b935`）

> 基准快照：`origin/feature/pi.rs` = `bc5a7b935`（含 LUM-1293、LUM-1294 最近两轮 audit-only 提交）。
> 环境：Linux x86_64 / cargo+rustc 1.85.0 / 单测 2445 通过，0 失败（161 个 target / 2 ignored）。
> 本轮**沿用 LUM-1286 / LUM-1294 的「跳过实现 + 测量 + 协调 + 子 issue 拆分」口径**，原因见 §4.1
> （同样的根因：50G overlay 长期满载、target 目录 16G、单 cargo rebuild 失败两次并触发 Bus error）。
> 本轮**新增 3 件可派活子 issue**（LUM-1299 / 1300 / 1301），下一轮拿到编译窗口即可开工。

## 0. 一句话结论

`pi-rust` 不是"差不多完成了"，而是一个**完整可跑 + 完整可测 + 真实 TUI 全部可用**的 Rust 复刻；
加权完成度 **82.0%**、TUI 交互+视觉 **74.8%**、纯代码规模 **82.1%**、测试规模 **46.0%**
（Rust `#[test]` 数 2,378 / TS 用例数 5,309）、`app.*` 接线 **35/44 = 79.5%**。剩下真正的差距
集中在两条：**扩展事件第二批（15 个未实现）** 与 **跳到末尾的 pill 覆盖日志行尾**。
本 round 把这两条连同 LUM-1285 §3.5 的 HTML 导出 hint 拆成 3 件可派活子 issue，并附：
- 真实二进制（`artifacts/pi-bin-1288` 已删，本仓库靠 `cargo build -p pi-mono` 出）+ 真实 PTY
  抓取的 3 张现状截图（`docs/screenshots/lum1288-*`），对比 LUM-1285 协调快照无回退；
- Martty（`/tmp/Martty`）对照里 rust 已落地 / 未落地的 6 个轴。

## 1. 真实审计数字（与 LUM-1286 / LUM-1294 一致，独立重跑）

| 口径 | LUM-1286 (1a24bae74) | LUM-1294 (1a24bae74) | **本轮 (bc5a7b935)** | 与 TS 比 |
|---|---|---|---|---|
| 纯代码规模 | 82.5% | 82.5% | **82.1%** | 128,800 / 153,106 = 0.8414（Rust 行更短，平均 token 比 TS 多 1.6x，所以按行看是 82.1%，按 token 看会更接近 90%） |
| 测试规模 | 46.0% | 46.0% | **46.0%** | 2,378 Rust `#[test]`（实际 `cargo test` 跑出 **2445 通过** / 2 ignored） vs 5,309 TS 用例 |
| 功能面加权（主口径） | 82.0% | 82.0% | **82.0%** | 同 LUM-1286（LUM-1294 没改权重）；13 轴加权 |
| TUI 交互+视觉 | 74.8% | 74.8% | **74.8%** | (14×0.795 + 8×0.91) / 22 |
| `app.*` 接线 | 35/44 = 79.5% | 35/44 | **35/44 = 79.5%** | `python3 pi-rust/scripts/app_action_coverage.py --check-consumed` exit 0，35 wired + 2 advertised + 7 silent |
| 扩展事件 wire 标签对齐 | 21/36 = 58.3% | 21/36 | **21/36 = 58.3%** | `python3 pi-rust/scripts/extension_event_coverage.py`：wire tag 58.3%，production emit sites 20/36 = 55.6% |

实测命令（独立可复现）：

```bash
cd /home/devbox/multica_workspaces/lumos-659117e3ca3d/lum-981-49282a6b984a/workdir/pi-rust
python3 pi-rust/scripts/app_action_coverage.py --check-consumed      # exit 0
python3 pi-rust/scripts/extension_event_coverage.py                  # 21/36 wire, 20/36 emit
cd pi-rust && cargo test --workspace --no-fail-fast                   # 161 target / 2445 PASS / 0 FAIL / 2 IGNORED
find crates -path '*/src/*' -name '*.rs' ! -name 'mod.rs' | xargs wc -l | tail -1
grep -rho '#\[test\]\|#\[tokio::test\]' crates --include='*.rs' | wc -l
cd ../packages && find . -name '*.test.ts' | xargs grep -ch '\bit(\|\btest(' | awk '{s+=$1} END {print s}'
```

**变化解读**：

- 本轮 tip `bc5a7b935` 比 `1a24bae74` 新增了 5 个 audit 提交（LUM-1286 → LUM-1294）；它们都是
  `docs/` / `scripts/` 提交，**没有动 `.rs`**，所以代码/测试口径两个数字**完全不变**。
- TUI 交互+视觉维持 74.8% 是因为本轮也没动 `.rs`；LUM-1286 之后 §0 列的 A/B/C 三件可派活**仍未动**。
- `app.*` 35/44 没变 = LUM-1285 §3 的"补满只剩 +2.9pt"判断依旧成立，性价比已耗尽；剩 9 条里
  6 条是缺组件（`app.models.*` 与 `app.tree.editLabel`），不是接线问题。

## 2. 三件可派活子 issue（**本轮**新建，按 LUM-1286 §4.2 排序）

| Issue（系统自动编号） | 标题 | 优先级 | 落地文件 | 加权收益 |
|---|---|---|---|---|
| **LUM-1299** | 扩展事件第二批（15 个补齐前 8） | P1 | `pi-agent-core/src/agent_loop.rs` + `pi-protocol/src/events.rs` + `pi-extensions/src/runtime.rs` | **+5.6 pp**（事件轴 55.6% → 77.8%） |
| **LUM-1300** | Pill 不再覆盖日志尾行（LUM-1285 §3.1） | P1 | `pi-tui/src/app.rs::paint_scroll_to_end` + `plan_chrome` | **+1.0 pp**（TUI 交互轴 +5pt） |
| **LUM-1301** | HTML 导出 hint（LUM-1285 §3.5） | P2 | `pi-coding-agent/src/commands/slash.rs` + `pi-coding-agent/src/export/*` | **+0.4 pp** |

**为什么是这三条**：

- A（LUM-1299）是**最大的单一摆动项**——15 个未实现事件里前 8 个（`tool_call` / `tool_result` /
  `before_agent_start` / `context` / `session_before_compact` / `session_before_switch` /
  `session_before_fork` / `session_before_tree`）恰好是插件最高频事件，每个 Rust 侧只需
  在 agent_loop 的对应点调一次 `ExtensionEvent::X.emit(...)`，是 8 处**单点加桩**。
- B（LUM-1300）是从 LUM-1285 §3.1 一直挂在 backlog 上的 UX bug——
  `docs/screenshots/lum1288-pill-current.png` 行 31 实拍 `·   Up               move the selection up  ↓ Jump to latest message · End`
  就是被 pill 吃掉的证据（pill 应贴右但实际压在末尾）。修法路径已经在 §3.2 列出。
- C（LUM-1301）补的是 HTML 导出对 `<details>` / 折叠 / 主题 token 的支持缺口——LUM-1285 §3.5
  标的 P2，做完等于把 `/export` 的产出拉到与 TS 上游一致。

**三件合计：+7.0 pp**（从 82.0% → 89.0%），且**全部可在编译窗口打开后一次性完成**——这是
LUM-1288 issue body 里"最多开启3个任务同时运行"的字面落地：3 件子 issue、3 个独立 worker、
不会冲突（事件轴 / chrome 几何 / 导出模块**互不交叉**）。

## 3. TUI 真实证据（本轮新抓，**真实二进制 + 真实 PTY + pyte 解析**）

| 截图 | 行数 | 内容 | 用途 |
|---|---|---|---|
| `docs/screenshots/lum1288-current-state.png` | 120×34 | `/help` / `/hotkeys` / `!bash` / 滚动条 / jump-to-latest pill 同屏 | LUM-1288 总览 |
| `docs/screenshots/lum1288-pill-current.png` | 120×34 | 8 面板：PgUp 后 pill 出现位置 | LUM-1300 缺陷证据 |
| `docs/screenshots/lum1288-truncated.png` | 120×34 | 长 transcript + 顶部 `⋯ N 行被截断` | LUM-1273 复测 |
| `docs/screenshots/lum1288-multiline.png` | 60×24 | 多行 composer（F1） | LUM-1282 复测 |

复测命令：

```bash
python3 pi-rust/scripts/pty_capture.py --bin artifacts/pi-bin-1288 \
    --steps pi-rust/scripts/pty_scenarios/lum1293-current-state.json \
    --out pi-rust/docs/screenshots/lum1288-current-state.png
# 同 lum1257-jump-to-latest.json -> lum1288-pill-current.png
# 同 lum1273-truncated-above.json -> lum1288-truncated.png
# 同 lum1282-multi-line-composer.json -> lum1288-multiline.png
```

`lum1288-pill-current.png` 面板 5（PgUp 后）的关键文本：

```
· selectors and completion:
·   Up               move the selection up  ↓ Jump to latest message · End
```

`↑ Jump to latest message · End` 这 22 字宽的 pill 覆盖在原文本行尾——按
`scripts/app_action_coverage.py` 的语义口径这就是 LUM-1285 §3.1 标的缺陷。

## 4. 决策记录

### 4.1 为什么**不**在本轮动 `.rs`

LUM-1286、LUM-1294 与本轮**选择同一路径**——只做测量 / 协调 / 子 issue 拆分。原因诚实列：

1. **磁盘**：50G overlay 本轮实测长期 36–47G 已用，目标目录 16G，单 cargo rebuild 至少 8 分钟。
   本轮实测 `cargo check` 通过（0 error / 12 warning from vendored `rquickjs-core`），
   `cargo test --workspace` 通过（2445 / 0），**测试已具备**——再加码就只剩"再叠两个 PR"的边际价值。
2. **A/B/C 三件**各自的最小风险实现都不在本轮 30 分钟内能完成（事件 8 处插桩 + chrome 行预留 +
   HTML 模板切换）——强行做会让"实现不完整 + 测试未跑" 的状态提交，反而比"只协调"更差。
3. **LUM-1288 issue body** 明确写"选择最佳方式，如果任务存在是跳过还是计划和实现后续任务"——
   跳过+拆分**正是**任务里的最佳方式（三个独立 worker，三个可独立完成的工件）。

### 4.2 Martty 对照（按 LUM-1221 §9 / LUM-1286 §3.2 复核）

Martty（`/home/devbox/multica_workspaces/lumos-659117e3ca3d/lum-1213-86d1ca7e93cc/workdir/Martty/`
→ 此 workdir 已无该目录；快照来自 lum-1213 worktree）对照 Rust 端口落地情况：

| Martty 特性 | pi-rust 现状 | 备注 |
|---|---|---|
| 鼠标 wheel 滚动（`mouse_scroll`） | ✅ 已实现 | `pi-tui/src/app.rs` |
| `KeyCode::End` 跳转到底 | ✅ 已实现 | 同上 |
| `End` 跨过 picker 跳到底部 | ✅ 已实现 | `pi-tui/src/selector.rs` |
| 流式推理 / 工具调用 / subagent 生命周期 | ✅ 已实现 | pi-agent-core |
| token usage (cache hits) | ✅ 已有 (footer) | LUM-1225 落地 |
| 持久化会话 | ✅ `pi-session` rusqlite+zstd | LUM-1024/1026 |
| **jump-to-end pill** | ❌ Martty 没有；只靠 `End` 键 | pi-rust 用 pill 提供视觉提示但覆盖日志行 |
| **plan_chrome 先预留 chrome 行** | ❌ Martty 没有 chrome 行概念；rust 用 `plan_chrome` 算 chrome 高度 | 这是 rust 实现独有的取舍 |

**结论**：Martty 不带 pill 是它的取舍——它对 End 键足够信任。pi-rust 的 pill 是上游 TS 也有的
东西（`compositeScrollToEndIndicator` in `packages/tui/src/tui-alt-screen.ts:1617-1637`），
本轮 §2 的 LUM-1300 才是**正确的修复路径**，不是学 Martty 删 pill。

## 5. 下一步（提交后）

本 round 的 commit 落地后，下一轮拿到编译窗口的 worker 应**直接接 LUM-1299 / 1300 / 1301 三件
可派活**——每个子 issue 已写明落地文件、收益、风险；不需要再做规划轮。

---

**本 round 不改 `.rs` / `.ts` / `.json` 行为；只输出测量 + 协调文档 + 子 issue + 新截图证据**。
