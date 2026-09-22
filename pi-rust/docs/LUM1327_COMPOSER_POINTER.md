# LUM-1327 — composer 指针定位（codex / pi-ts parity）：真 PTY A/B、整 TUI 审计、Rust↔TS 差距实测

> 范围：`pi-rust/crates/pi-tui/src/{app,editor,prompt,visual_text}.rs`、
> `pi-rust/crates/pi-tui/tests/composer_click.rs`、`pi-rust/scripts/pty_capture.py`、
> `pi-rust/scripts/pty_scenarios/lum1327-composer-click*.json`、
> `pi-rust/docs/screenshots/lum1327-composer-click*.png(.txt)`
> 基线：`origin/feature/pi.rs` = `6a5a2faf2`（LUM-1319 的 tip）
> 参照实现：codex `codex-rs/tui/src/bottom_pane/chat_composer/mouse.rs`（本地 checkout `7d99ee8`）、
> 上游 pi `packages/tui/src/components/editor.ts:615-670`（`handleMouse`）、
> Martty `src/ui.rs` / `src/app.rs`（v0.2.17）

一句话结论：**composer 的指针路径原本是死的**——点击落在输入框上既不放光标、也不被任何层消费；
本轮按上游 `Editor.handleMouse` 的点击语义把它接通（含「换行行尾不跨行」这条细节），
13 条真 PTY 断言在「修前二进制」与「修后二进制」上各跑一遍（修前 13/13 断言“点击什么都没发生”，
修后 13/13 断言“光标落在被点的字符上”），全量门禁在 1.85.0 下 `2632 passed / 0 failed`。

---

## 1. 结论速览

| 口径 | 本轮实测 | 上一轮（LUM-1310 文） | 说明 |
| --- | --- | --- | --- |
| 纯代码规模（src↔src） | **88.0%**（134,665 / 153,106） | 85.1% | 同口径，只涨不跌（别的轮次也在加行） |
| 测试规模（标记数） | **48.3%**（2,564 / 5,309） | 45.5% | 本轮 +10 条（8 个集成 + 2 个 `VisualLayout` 单测） |
| `cargo test --workspace --locked` | **2632 passed / 0 failed / 2 ignored**（168 suites） | 2483 passed（该轮 workspace 门未跑完） | 本轮真跑完（`--no-fail-fast`） |
| `app.*` 接线 | **43/44 (97.7%)**，advertised **0/44**，silent **1/44** | 37/44 (84.1%) | silent 只剩 `app.tree.editLabel` |
| slash 内置命令交集 | **17/23 (73.9%)**（`exit`≡`quit` 则 18/23） | 16/23 (69.6%) | 缺 `changelog import share login logout quit` |
| `/settings` 行 | **6 / 37** row id | 6/37 | 本轮未动 |
| TUI 交互面（14% 权重轴） | 本轮**不重算**（见 §6.3） | 64%（LUM-1238 口径） | 本轮只关掉一条真实缺口 |
| 本轮单一进度口径（公开公式） | **75.8%** | 71.5%（LUM-1310 §6.1 同公式） | 公式与各轴得分见 §6.2；工具/命令面、TUI 交互是判断值 |

**不要把这些数字读成「功能完成了 75.8%」**：它由 6 个轴按公开权重合成，
其中 4 个轴是测量值、2 个是判断值（依据逐条写在 §6）。换权重就换结果，读者可自算。

---

## 2. 真实审计：修复前的缺陷（真 PTY A/B，不是设计意图）

### 2.1 缺陷本身

输入框（composer）在屏幕底部独占一行或数行，**不在**聊天视口里：

- `App::selection_point`（`crates/pi-tui/src/app.rs:4454`）先用 `viewport_origin/viewport_height`
  判断指针是否落在聊天视口内，落在视口外直接返回 `None`；
- 于是 `step_selection_mouse_gesture` 的 press 分支立刻 `Idle` 返回；
- 而 composer 一侧**没有任何指针入口**：`App::translate_event`（`app.rs:5388`）把
  `CtEvent::Mouse` 只映射成滚轮 / 手势，手势进 `step_mouse_gesture`（`app.rs:3922`）后
  依次试 modal → 搜索条 → jump-to-latest pill → 截断提示 → 滚动条 → 聊天选择，
  没有一层认领 composer 的格子。

结论：**点击输入框是一个「没人消费」的事件**。这与两个参照实现都不同——
上游 pi-ts 的 `Editor.handleMouse` 有专门的 click 分支（点击定位光标，拖拽则交给
渲染层的屏幕级选择），codex 的 `chat_composer/mouse.rs` 同样处理点击定位、双击选词、
拖选复制（`prepare_mouse` / `handle_mouse` / `copy_selection`）。

### 2.2 证据：同一组手势打给两个二进制

```bash
# 修前：从 origin/feature/pi.rs (6a5a2faf2) 构建的二进制
python3 pi-rust/scripts/pty_capture.py --bin <pre-fix pi> \
  --steps pi-rust/scripts/pty_scenarios/lum1327-composer-click-baseline.json \
  --out /tmp/lum1327-before.png --text-out /tmp/lum1327-before.txt

# 修后：本轮的二进制
python3 pi-rust/scripts/pty_capture.py --bin pi-rust/target/debug/pi \
  --steps pi-rust/scripts/pty_scenarios/lum1327-composer-click.json \
  --out pi-rust/docs/screenshots/lum1327-composer-click.png \
  --text-out pi-rust/docs/screenshots/lum1327-composer-click.png.txt
```

| 手势（100×20 真终端、真二进制、pyte 仿真） | 修前（断言 13/13 PASS 于“什么都没发生”） | 修后（断言 13/13 PASS） |
| --- | --- | --- |
| 草稿 `hello world`，点 `llo` 的第一个 `l`（格 4,18） | `> hello world▍`（光标仍在末尾） | `> he▍llo world` |
| 点标签栏（格 1,18） | 不动 | `> ▍hello world`（第 0 列） |
| 两行草稿，点第二行 `second` 的 `c`（格 4,18 → 第二行） | `  second line▍` | `  se▍cond line` |
| 120 列草稿折成 2 行，点第一行最后一格（99,17） | 光标仍在末行末尾 `  w{22}▍` | `> w{97}▍`（留在被点的那一行，**不跨行**) |
| 点状态栏（20,19，composer 之外） | 不变 | 不变（两侧都断言“只有 1 个光标”） |

截图：`docs/screenshots/lum1327-composer-click.png`（修后，6 个面板）、
`docs/screenshots/lum1327-composer-click-baseline.png`（修前，同一组手势）；
字符网格 dump 与 PNG 同名 `.txt`，断言矩阵逐条 PASS/FAIL 可见。

### 2.3 审计中发现的第二个真实问题（本轮一并修掉）

第一次拍摄时发现：**把 120 个字符和点击放进同一次 `send`，点击会被丢掉**。
原因是帧矩形只在「上一次绘制」时记录，而终端的 paste-and-click（或一次 `poll` 里排队的
多个事件）会在同一轮里到达：解析点击时，记录下来的还是「草稿只有一行」时的 composer 矩形，
而用户看到的 composer 已经长到两行——**记录与屏幕不一致**。

修法：composer 是「底边固定」的区域，所以
**底行**取上一帧（它只在编辑器下方的扩展 chrome 变化时移动），**顶行**由当前草稿需要的行数反推
（`Prompt::line_count`），滚动偏移也按 `render_body` 的 follow-caret 规则夹紧
（`app.rs:4125` 的 `composer_hit`）。这样点击映射与「下一帧要画什么」一致，而不是与「上一帧画了什么」一致。

---

## 3. 实现

| 文件 | 内容 |
| --- | --- |
| `crates/pi-tui/src/app.rs` | `ComposerHit` 枚举（`Offset` / `Absorbed`）；`composer_area` 四元组（上一帧的 composer 矩形，`height == 0` = 这一帧不是 composer）；`composer_press`（进行中的左键按下格）；`step_composer_mouse_gesture`（press 定位 + press/drag/release 吞掉，绝不启动聊天选择）；`composer_hit`（格子 → 草稿偏移，含底边/顶行推导与滚动夹紧）；`paint_prompt` 记录矩形；`frame.editor` 替换 prompt 时清零（否则会往隐藏的草稿里放光标）；`composer_area()` 公共只读访问 |
| `crates/pi-tui/src/editor.rs` | `place_display_cursor`：不推 undo 快照、不改草稿、结束 history browsing 与 kill/typing 链（上游 `exitHistoryBrowsing` + `lastAction = null`）；落点按 `display_cursor` 同一把尺子（chip 计整格，落在 chip 标签内的偏移吸附到哨兵字节）；反向搜索打开时拒绝（防御性，App 也不会路由） |
| `crates/pi-tui/src/prompt.rs` | `place_cursor` 透传 |
| `crates/pi-tui/src/visual_text.rs` | `last_of_line`（每行末行标记）+ `click_offset`：**点击落在软换行行尾不跨行**（上游 `targetIndex = lastGraphemeIndex`），与键盘纵向移动共用的 `cursor_at` 明确分开 |
| `crates/pi-tui/tests/composer_click.rs` | 8 条集成测试，全部读**渲染帧**：光标位置是 `▍` 所在格，点击坐标由同一帧算出，因此标签栏宽度/窗口滚动都不参与断言 |
| `scripts/pty_capture.py` | 新增指针 token `<Click:x,y> <MPress:x,y> <MRelease:x,y> <MDrag:x,y>`（0 基格子 → SGR `ESC[<b;x;yM/m`），harness 从此能驱动鼠标 |

### 3.1 与上游 / codex 的差异（说清楚，不吹）

1. **拖选 + 复制还没有**：codex 的 composer 支持拖选、双击选词、右键复制
   （`chat_composer/mouse.rs` 的 `copy_selection`），上游 pi-ts 把编辑器行交给渲染层的
   屏幕级选择。本轮只做「点击定位」，拖拽一律吞掉（既不启动聊天选择，也不产生 composer 选择）。
2. **补全下拉的指针路由还没有**：上游 `Editor.handleMouse` 会先判下拉框命中
   （`autocompleteStartRow` 起算），命中则交给 `SelectList.handleMouse`。本轮的点击落在下拉框
   所在行时是「聊天视口内的一次普通点击」——既有行为，未改，见 §9 的派发。
3. **没有 vim 模式**：codex 有 `vim_history.rs` / `vim_search.rs`（composer 的 vim 键位），
   上游 pi 没有，本 port 也没有；**不抄**（会偏离 parity 目标）。
4. **行尾规则按上游、不按 codex**：codex 的 textarea 允许点在行尾空白之后继续往后，
   上游 pi-ts 会吸回该行最后一个字素；这里跟上游（同一个端口已经用上游的 `wordWrapLine` 语义）。

---

## 4. 真 PTY 证据的断言矩阵

`lum1327-composer-click.json`（修后）6 个可见面板 / 13 条断言：13 PASS / 0 FAIL。
`lum1327-composer-click-baseline.json`（修前）同 6 个面板 / 13 条断言：13 PASS（断言的是缺陷本身）。

```
assertions: 13 checks over 6 panels — 13 PASS, 0 FAIL, 0 XFAIL, 0 XPASS   (修后)
assertions: 13 checks over 6 panels — 13 PASS, 0 FAIL, 0 XFAIL, 0 XPASS   (修前)
```

其中载重的一条（修后）：

```
[5] 5. a click on the last cell of a wrapped row keeps the caret on that row
  PASS   expect  '^> w{97}▍'    — 光标停在被点的行尾，而不是落到下一视觉行
  PASS   expect  '^> w{97}▍w' (x0)
```

以及「点击不是编辑」（集成测试里断言草稿逐字不变，真 PTY 里断言 `Ctrl+C` 前后行为、
状态栏不被写入光标）：

```
[6] 6. a click on the status bar (outside the composer) moves nothing
  PASS   expect  'Faux test model'
  PASS   expect  '^> w{97}▍' (x1)          — 只有一个光标
  PASS   expect  '▍Faux test model' (x0)   — 状态栏永远不会出现光标
```

harness 对「面板与上一帧逐字节相同」会给 WARN（修前 [2,6]、修后 [6]）——
这正是那些面板要证明的事（点击什么都不改），已在两个 scenario 的 `notes` 里写明。

---

## 5. 整 TUI 的真实分析（本轮**不做**什么，以及为什么）

这一节回答 issue 里的「分析整个 TUI 的问题 / 优先完善 TUI」：先把**在飞与未做**列清楚，
再说本轮为什么挑指针。

### 5.1 在飞：粘贴折叠（LUM-1318，**不要碰**）

`LUM-1318`（`pi-rust composer：大段粘贴折叠成 [paste #N +M lines]`）**此刻正在运行**，
它的 `work/LUM-1318` 分支上已有 322 行未提交改动（`pi-rust/crates/pi-tui/src/editor.rs`：
`PASTE_CHAR` 哨兵 + `pastes: Vec<Paste>` 侧表 + 折叠阈值 + undo 快照携带侧表）。
本轮**故意不碰** paste 相关的任何文件/语义，避免两条 run 互相覆盖。

> 顺带一条真实观察（交给 LUM-1318 的读者）：那份改动把
> `display_text()` 展开成 `[paste #N +M lines]` 标记，而 `EditorAction::Submit` 现在直接带
> `display_text()`。上游 `submitValue()` 用的是 **`getExpandedText()`**（全文），
> 所以提交路径必须走「全文」而不是「标记」，否则模型会收到 `[paste #1 +200 lines]`
> （LUM-1318 的验收里也写了这一条）。本轮没有改它。

### 5.2 仍未做（本轮盘点，按价值排序）

| # | 缺口 | 参照 | 现状 | 本轮 |
| --- | --- | --- | --- | --- |
| 1 | composer 拖选 + 复制（选中即复制） | codex `copy_selection` | 缺（拖拽被吞，无 composer 选择） | 派发（见 §9） |
| 2 | 补全下拉的指针路由（点行选中/滚动） | 上游 `Editor.handleMouse` 下拉分支 | 缺（点下拉行 = 点聊天区） | 派发（见 §9） |
| 3 | 大段粘贴折叠 `[paste #N …]` | 上游 `handlePaste` | LUM-1318 在飞 | 不碰 |
| 4 | bracketed paste 模式与 `Event::Paste` 路由 | 上游 `terminal.ts:184` 开 `?2004h` | 未开；`translate_event` 把 `CtEvent::Paste` 映射成 `Ignored` | 不碰（与 #3 同一条链） |
| 5 | `/settings` 行数 6/37 | 上游 `settings-selector.ts` | 缺的多属未移植子系统 | 未做 |
| 6 | `app.tree.editLabel`（唯一 silent） | 上游树标签编辑 | 未接线 | 派发（见 §9） |
| 7 | slash 缺口 `changelog/import/share/login/logout` | 上游 23 条 | 17/23 | 未做 |
| 8 | Martty 的状态条左右分片（宽度预算） | Martty `meta_line` / `status_right` | 单段 + 截断 | 未做 |

### 5.3 Martty 对照（本轮新增的认识）

Martty（`louloulin/Martty` v0.2.17，ratatui 客户端）与本 port 定位不同，但有一条**直接可比**的：
它的 `handle_mouse`（`src/app.rs`）同样把指针事件按矩形分派，**先判 composer/输入区再判聊天区**，
并且把「点击定位光标」与「拖选」分开处理——这与 codex 的结构一致，也说明本轮的顺序选择
（先接通 composer 的点击，再补拖选）与两个参照实现的演进顺序相同。

Martty 值得学的仍然停在 LUM-1310 §5 的三条（composer 高度下限分档、右栏语义位、状态条左右分片），
本轮没有一条落地，原因是它们都在 `app.rs` 的布局预算里，而 LUM-1318 正在改同一片区域。

---

## 6. Rust ↔ TS 差距（本轮实测，命令可复现）

```bash
cd pi-rust
git rev-parse --short HEAD
find crates -path '*/src/*' -name '*.rs' ! -name 'mod.rs' | xargs wc -l | tail -1
find ../packages -path '*/src/*' -name '*.ts' ! -name '*.test.ts' | xargs wc -l | tail -1
grep -rhc '#\[test\]\|#\[tokio::test\]' crates --include=*.rs | awk '{s+=$1} END {print s}'
find ../packages -name '*.test.ts' | xargs grep -ch '\bit(\|\btest(' | awk '{s+=$1} END {print s}'
python3 scripts/app_action_coverage.py
python3 scripts/extension_event_coverage.py
```

### 6.1 测量值

| 轴 | 实测 | 复现 |
| --- | --- | --- |
| 代码规模 | **134,665 / 153,106 = 88.0%** | 上面的 `find/wc` |
| 测试规模 | **2,564 / 5,309 = 48.3%** | 上面的 `grep -c` |
| `app.*` 接线 | **43/44 = 97.7%**；advertised 0/44；silent 1/44（`app.tree.editLabel`） | `scripts/app_action_coverage.py` |
| slash 命令交集 | **17/23 = 73.9%**（缺 `changelog import share login logout quit`，其中 `exit`≡`quit`） | 逐名对照 `packages/coding-agent/src/core/slash-commands.ts` |
| `/settings` | 6 / 37 row id | 该文件 §5.2 |
| pi-tui 规模 | 35 文件 / 35,725 行 | `find crates/pi-tui/src -name '*.rs' | xargs wc -l` |

### 6.2 本轮单一进度口径（公式公开）

沿用 LUM-1310 §6.1 的公式（同一套权重，便于和上一轮比较）：

```
完成度 = 规模 0.30 + 测试 0.20 + app.* 0.15 + slash 0.15 + 工具/命令面 0.10 + TUI 交互 0.10
       = 88.0×0.30 + 48.3×0.20 + 97.7×0.15 + 73.9×0.15 + 60×0.10 + 80×0.10
       = 26.40 + 9.66 + 14.66 + 11.09 + 6.00 + 8.00
       = 75.80  →  **75.8%**
```

- 前 4 项是测量值（§6.1）；**后 2 项是判断值**：工具/命令面 60 未变（本轮未动这条轴），
  TUI 交互 78 → **80**（+2：关掉「点击输入框无人消费」这条两个参照实现都有的缺口；
  仍扣分于拖选/复制、下拉指针、粘贴折叠、vim 等未做项）。
- 与 LUM-1310 的 71.5% 相比 +4.3，其中**大部分来自别的轮次落地的轴**
  （`app.*` 84.1→97.7、slash 69.6→73.9、规模 85.1→88.0），本轮自己贡献约 +0.6
  （TUI 判断值 +2 × 0.10，加上规模/测试的零头）。

### 6.3 为什么不再给第二个总分

`docs/RUST_TS_PARITY_METRICS.md` 的 13 轴加权（上一版 81.4%）权重表分项在该文自身
§0.2/§0.4 已被标注「不可追溯」，本轮按该文的约定**不重算、不叠小数**；
读者若要 13 轴口径，请用那份权重表替换 §6.2 的 6 轴公式自行重算。

---

## 7. 门禁实况（Rust 1.85.0，`--locked`）

```bash
cd pi-rust && . scripts/toolchain.sh
cargo fmt --all -- --check                                        # 干净
cargo clippy --workspace --all-targets --locked -- -D warnings     # No issues found
cargo test --workspace --locked --no-fail-fast                      # 2632 passed / 0 failed / 2 ignored（168 suites）
```

- 本机 `~/.local/bin/cargo` 是没有默认 toolchain 的 rustup shim，必须
  `. scripts/toolchain.sh`（钉 1.85.0）才能跑门禁，否则每条命令都报
  `rustup could not choose a version of cargo to run`（LUM-1308 的既有结论仍成立）。
- **磁盘是共享卷**：本轮两次遇到 `No space left on device`（另一条 run 同时在构建）。
  为跑完全量门禁，本轮先 `cargo clean`（回收 10.9GiB），再用
  `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` 重跑；
  **关调试信息不改测试语义**，但会让 `target/` 与默认档不同。
- **既有 flaky（不是本轮引入）**：`pi-coding-agent --test extension_ui` 的
  `interactive_regions_render_into_the_app` 是 `pump()` 投递与渲染之间的竞态
  （自 `d02fb0ace`/LUM-1190 就有，20 次约 1 次失败）。本轮第一次全量跑撞上它，
  `--no-fail-fast` 重跑通过（该 target 7/7）。

---

## 8. 复现命令

```bash
# 1. 单元 / 集成测试（本轮新增 10 条）
cd pi-rust && . scripts/toolchain.sh
cargo test -p pi-tui --test composer_click            # 8 passed

# 2. 真 PTY A/B
cargo build -p pi-coding-agent --bin pi
python3 scripts/pty_capture.py --bin target/debug/pi \
  --steps scripts/pty_scenarios/lum1327-composer-click.json \
  --out docs/screenshots/lum1327-composer-click.png \
  --text-out docs/screenshots/lum1327-composer-click.png.txt
# 修前：git checkout origin/feature/pi.rs → 构建 → 跑
#   scripts/pty_scenarios/lum1327-composer-click-baseline.json（同一组手势，断言“什么都没发生”）

# 3. 差距数字
python3 scripts/app_action_coverage.py
python3 scripts/extension_event_coverage.py
```

---

## 9. 已知限制与后续

**已知限制（本轮）**

1. 拖选/复制（composer 内）未做：拖拽被吞，不会产生 composer 选择，也不会复制。
2. 补全下拉的指针路由未做：点下拉行仍是「聊天视口内的一次点击」。
3. 反向搜索打开时 composer 的指针被整体吞掉（与键盘一致），不会定位光标——有意的
   （搜索预览的是另一份草稿，指针量到的是旧的）。
4. 顶部行由当前草稿行数反推，因此**编辑器下方扩展 chrome 变化的那一帧**（below/footer 出现或消失）
   若与点击同批到达，底行仍可能相差一行；下一帧即修正。
5. 真 PTY 断言只覆盖 100×20 一种几何；集成测试另覆盖 40×14 与换行/多行/反向搜索场景。

**派发的后续子 issue（≤3，见 issue 评论）**

1. composer 拖选 + 选中即复制（codex `copy_selection` / 上游屏幕级选择在编辑器行上的语义）。
2. 补全下拉的指针路由（`Editor.handleMouse` 的下拉分支 + `SelectList` 命中）。
3. `app.tree.editLabel` 接线 → `app.*` 44/44。

**明确不派发**：粘贴折叠与 bracketed paste（LUM-1318 在飞，两条 run 会互相覆盖）。
