# TUI 参考块/滚动验证 + LUM-1267 交互基线修正（LUM-1271）

> 基准：`origin/feature/pi.rs` = `c3134c87f`；本轮的 `work/LUM-1271` 只新增/修改 `pi-rust/scripts/`
> 与 `pi-rust/docs/`，**没有改任何 `.rs`**（原因见 §1）。
> 本轮所有数字都能用 §7 的命令复现；PTY 证据在 `docs/screenshots/lum1271-*` 与
> `docs/screenshots/lum1267-interaction-assertions*`（文本 dump 是承重证据，PNG 是版式实拍）。

## 1. 本轮约束（为什么又没有 Rust 代码）

`/` 分区本轮开工时只剩 1.1–3.5G（93–98%），同一台机器上有 4 个以上活着的 `cargo` 构建
（其他 in-flight issue 的 `target/`）；中途 sibling 构建结束后回到 8.2G（83%），但**本轮全程
没有编译器可用**的时刻，按仓库既有规则「无法验证就不落码」（同 LUM-1260 / LUM-1267）。

所以本轮的产出不是文档本身，而是**两件能跑、能失败、能复现**的东西：

1. 一次**基线修正**：LUM-1267 发布的那条「48 PASS / 0 FAIL」并不是合并后 tip 的数字（§2）。
2. 一条**视口无关**的新验收门，把「参考块渲染是否完整」和「34 行终端下怎么看到它」拆开（§3）。

## 2. 核心发现：LUM-1267 的交互门测的是**修复前**的二进制

LUM-1267 §3.2 自己写明了基线是对 `artifacts/pi-tip-b5769b585` 跑的（`b5769b585` 的树里
**没有** `c8bc6785a`，本轮用 `git merge-base --is-ancestor c8bc6785a b5769b585` 复核：不成立）。
当时的理由是「合并后 tip 二进制造不出来」。本轮捡到了与 tip 等价的二进制
（`lum-1266` checkout 的 `target/debug/pi`，其 HEAD == `c3134c87f` == origin tip，工作区干净、
无比二进制更新的 `.rs`/`.toml`），于是**第一次真的测了 tip**：

| # | 场景 | 二进制 | 结果 |
|---|---|---|---|
| A1 | 旧门，17 面板 50 检查 | `b5769b585`（LUM-1261 之前） | **48 PASS / 0 FAIL / 2 XFAIL** ← LUM-1267 发布值，本轮独立复现 |
| A2 | 旧门，17 面板 50 检查 | tip `c3134c87f` | **41 PASS / 7 FAIL / 2 XFAIL** |
| B1 | 修正门，20 面板 60 检查 | tip | **58 PASS / 0 FAIL / 2 XFAIL**（EXIT=0） |
| B2 | 修正门，20 面板 60 检查 | `b5769b585` | **45 PASS / 13 FAIL / 2 XFAIL**（EXIT=1） |
| C1 | 完整性门，3 面板 40 检查，120×70 | tip | **40 PASS / 0 FAIL**（EXIT=0） |
| C2 | 完整性门，3 面板 40 检查，120×70 | `b5769b585` | **14 PASS / 26 FAIL**（EXIT=1） |

A2 的 7 条 FAIL 全部落在 `/help`、`/hotkeys` 两个面板的**头部 needle** 上（`slash commands:`、
`/hotkeys`、`/compact` …）。

**机制**（不是"needle 写错了"，是渲染契约变了）：`c8bc6785a` 把参考块从「按终端宽度硬裁 + 合并源行」
改成「按源行 + 软换行」（`crates/pi-tui/src/message.rs` +144 行，配套 `help_text_layout.rs`）。

- 修复前：`/help` 正文被压成 **11 行**，整块**放得进** 34 行终端那 ~11–13 行的转录窗口，所以
  「头部 needle 可见」在那个渲染器上是**真的成立**的（LUM-1267 的 A1 因此 48/0/0）。
- 修复后：同一块是 **~34 行**，窗口仍是 ~11–13 行且**底部锚定**，所以默认帧只能看到**尾部**，
  头部必须 `Home` / `PgDn` 才看得到。

于是 A2 的 7 条 FAIL 是**旧门的口径过时**，不是 tip 的回归（这一条也用 A/B 控制排除过：对
`lum-1269` 那份（HEAD `b8adfe6bd`）预编译二进制跑同一场景，同样是 41/7/2，即与 LUM-1266 的
改动无关）。

### 2.1 修正后的门（B1/B2）

`scripts/pty_scenarios/lum1267-interaction-assertions.json` 从 17 面板扩到 **20 面板 60 检查**：

- 面板 1–14 原样保留（选择器、补全、`@` 文件树、provider 往返、`!bash`、`<C-o>` 都仍然 PASS）。
- 面板 15：`/help` 的**默认帧**断言改为「尾部可见」（`· keys:`、`submit prompt`、
  `Ctrl+U clear the prompt buffer`）。
- 面板 16/17：`Home` / `PgDn` 断言**头部与中段可达**（`slash commands:`、`/help`、`/clear`、
  `/new` / `/resume`、`/thinking`、`/trust`）。
- 面板 18–20：`/hotkeys` 尾部 + `PgUp` 一页回看的 app/selector 区 + `End`+`PgUp` 出现
  `↓ Jump to latest message`。
- 新增两条 **`reject`**：`keys: Enter submit promp` 与 `accept autocomplete commands:`。这两串
  只在**硬裁**渲染里出现（裁点落在词中间、并把本该分开的行挤在一起）。B1 通过、B2 命中
  ——两条 reject 都**只在修复前的二进制上开火**，所以它们是真正的判别器而不是装饰。

> 经验：needle 不能贴在滚动窗口的边缘。同一个场景内 `!bash` 块偶尔渲染 2 行、偶尔 3 行，
> 会让窗口整体挪一行；B1 里贴边的两条 needle 因此翻车过一次，现在都挪到了窗口中段（面板
> 16 的注释里写了这件事）。

## 3. 新增：视口无关的完整性门（120×70）

`scripts/pty_scenarios/lum1271-help-complete.json`（3 面板 40 检查）。**动机**：把两个问题拆开——
「每一行有没有被裁断」是**内容**不变量，跟终端多高无关；「34 行时能不能看到」是**视口交互**，
由 §2.1 那条门负责。120×70 时转录窗口 ~48 行 > `/help` 的 ~34 行正文，于是整块正文在**同一帧**里：

- 面板 1：`/help` 的 19 条命令 + 键位表（`Enter submit prompt`、`Home / End jump to the start /
  end of the chat log`、`Ctrl+U clear the prompt buffer`）全部在同一帧出现；
  `reject` = `keys: Enter submit promp`（硬裁的产物）+ `Jump to latest`（整块放得下，
  日志就在底部，不该出现回滚 pill）。
- 面板 2：`/clear` 真的清空消息视图（`reject` 掉 `slash commands:` / `keys:`）。
- 面板 3：`/hotkeys` 的 `app:` / `selectors and completion:` / `commands:` 三段 + 具体键位同帧；
  `reject` = `accept autocomplete commands:`。

C1/C2 的落差（40/0 vs 14/26）就是「软换行 vs 硬裁」这个契约的判别力：修复前的二进制在**任何**
高度下都拿不到这一帧，因为它根本没有那些完整行。

## 4. 本轮实测的 TUI 观察（诚实记录，含未修项）

1. **34 行终端下转录窗口只有 ~11–13 行**：展开态启动头 ~20 行 + composer/status ~2 行 ≈ 21–22 行
   chrome。方向与 LUM-1266 一致（短视口折叠启动头、滚动条独占最右列），`Alt+H` 可临时折叠。
   但这意味着**任何超过 ~13 行的块（参考块、长工具输出）默认只露尾部**——用户得知道 `Home`/`PgDn`。
2. **`↓ Jump to latest message` pill 会盖住它所在那一行的尾部文字**：实测帧里出现
   `·   /name [name] show or set the session di` + pill（`name` 的说明被 pill 吃掉）。
   建议：pill 走 chrome 行（composer 上方独立一行）或让该行让位，而不是叠在日志行右侧。
3. **工具块折叠对短块是空操作**：1 行输出的 `!bash` 块，collapsed 与 expanded 的**帧完全相同**
   （fold 只对超过预览行数的块生效）。`Ctrl+O` 的语义没错，但短块上按它没有视觉反馈；
   状态行那句 `Tool output: expanded/collapsed` 是唯一的反馈。
4. **向上滚动是 sticky 的**：回滚后新输出不会把视图拽回底部，`End` 明确回到底部 —— 与 pi/codex
   的期望一致（这条也在面板 20 里被断言）。
5. **长块的可发现性缺口**：`/help` 默认帧完全看不出「上面还有 30 行」。这是本轮最值得优先修的
   UX 项（见 §8）。

### 4.1 一次不可复现的观察（未定位，诚实留档）

第 13 面板（`!echo pty-harness-ok`）在某一次运行中只渲染了 `* bash …` + `* Took 0.6s`，
**输出行 `* pty-harness-ok` 缺失**；同一场景其余运行、以及隔离探针（3/3）与另一次 4/4 都正常渲染。
本轮没有编译器，无法进一步二分，故**不写结论**；已按「渲染时序不确定」列入 §8 候选。
本轮的门做了两件事避免被它污染：面板 13 的三条 needle 在后续运行里稳定 PASS，且 §3 的完整性门
在 120×70 下重新覆盖了同一段输出。

## 5. 证据文件

| 文件 | 内容 |
|---|---|
| `screenshots/lum1271-reference-120x70-tip.png{,.txt}` | **C1**：120×70 下 `/help` `/clear` `/hotkeys` 三帧全绿（整块正文同帧） |
| `screenshots/lum1271-reference-120x70-pre1261.png{,.txt}` | **C2**：同一场景跑修复前的 `b5769b585`（14 PASS / 26 FAIL，能直接看到硬裁行） |
| `screenshots/lum1267-interaction-assertions.png{,.txt}` | **B1**：20 面板门在 tip 上的 58/0/2 实拍（本次重新生成，与提交的场景文件一致） |
| `screenshots/lum1267-interaction-assertions-before-1261.png{,.txt}` | **B2**：同一门跑 `b5769b585`（45/13/2，13 条 FAIL 即「头部/完整性」落差） |

「是否达到 codex/pi 那样的交互」这个问题，看这两组图的差别最直接：**选择器 / 补全 / `@` 文件树 /
流式往返 / 工具块 / 键位表 / 回滚 pill 都在**（B1 面板 1–14、18–20 全绿），差距集中在
**长块在低视口下的呈现**（面板 15–17 需要滚动才能看全，§4.1）与下面 §6 的口径。

## 6. 完成度与差距（诚实口径）

本轮没有 `.rs` 改动，所以**上一轮的代码口径数字一个都没动**，本轮做的是**修正度量本身**：

| 口径 | 值 | 本轮状态 |
|---|---|---|
| 纯代码规模（Rust/TS src） | **82.1%** | 不变（`RUST_TS_PARITY_METRICS.md` §0.1） |
| 测试规模 | **43.4%** | 不变（Rust 2,303 `#[test]` / TS 5,309） |
| 功能面加权（主口径） | **79.8%** | 不变 |
| TUI 交互 + 视觉 | **73.8%** | 数字不变，但**支撑它的门被修正**：旧门 17 面板/50 检查（对 tip 是 41/7/2）→ 新门 20 面板/60 检查 + 3 面板/40 检查，tip 全绿、修复前二进制必挂 |
| `app.*` 动作接线 | **35/44 (79.5%)** | 本轮复算，无漂移（§7） |
| 扩展生命周期事件 | **21/36 tag (58.3%)**、**20/36 production (55.6%)** | 本轮复算，无漂移；缺 15 个上游事件 |

**综合结论（不美化）**：Rust 版在「coder 日常交互」这一层已经能跑通 pi 的主路径（这轮 60+40 条
断言在 tip 上全绿），但整体仍比 TS 版少约 20%，缺口集中在三处：**(a)** 15 个扩展生命周期事件没接线、
**(b)** 7 个 `app.*` 动作是 silent（`app.models.*`、`app.tree.editLabel`）、**(c)** TUI 低视口下
长内容的呈现（§4.1/§4.2）。TUI 交互形态本身**不是**最大缺口——它是目前最接近 TS 版的一块。

## 7. 复现命令

```bash
cd <repo>                                   # 仓库根（pi/）
# 覆盖率（本轮复算，与 LUM-1267 一致）
python3 pi-rust/scripts/app_action_coverage.py          # wired 35/44 (79.5%), silent 7/44
python3 pi-rust/scripts/extension_event_coverage.py     # 21/36 (58.3%) tag, 20/36 (55.6%) prod, 缺 15

# 交互门：tip 应 EXIT=0，修复前二进制应 EXIT=1
python3 pi-rust/scripts/pty_capture.py --bin <tip-binary> \
  --steps pi-rust/scripts/pty_scenarios/lum1267-interaction-assertions.json --out /tmp/scroll.png
python3 pi-rust/scripts/pty_capture.py --bin <pre-1261-binary> \
  --steps pi-rust/scripts/pty_scenarios/lum1271-help-complete.json --out /tmp/tall.png

# 用到的两个二进制（本轮）
#   tip        = lum-1266 checkout target/debug/pi   sha256 75b34bf189a2224dc1d35a084d148cca70afebc188be7eccf339c032f8ea2f56
#   pre-1261   = lum-1260 artifacts/pi-tip-b5769b585 sha256 f23fcb3b86ac5baeaa19c7fd07c0f94b549a1c87f974f4829ccdecf5a25d6714
```

## 8. 后续（≤3 并发，按优先级）

1. **长块可发现性**（TUI/UX，最小改动）：块被视口截断时在顶部渲染一行
   `⋯ 上面还有 N 行 · Home 看开头`，复用已有的 `↓ Jump to latest message` pill 机制。
2. **pill 不再吃日志行的尾部文字**（§4.1-2）：让 pill 走 chrome 行或让出该行。
3. **扩展事件接线**：从 15 个缺失里先接高频的 `context` / `before_agent_start` /
   `session_before_compact`（口径见 `extension_event_coverage.py --help`）。
4. **定位 §4.1 那次工具输出行缺失**：需要能构建的机器，用 `tools_expanded` 的两态做二分。
