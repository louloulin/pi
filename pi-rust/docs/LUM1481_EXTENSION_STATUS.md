# LUM-1481 — `ctx.ui.setStatus` 落地为 footer 的第三行（插件生态 × TUI 交叉点）

> scope: `pi-rust/crates/pi-tui`（`status` / `app`）+ `pi-extensions`（`host` / shim / docs）+
> `pi-coding-agent`（`extensions/ui_bridge`）
> branch: `lum1481-work`（→ `feature/pi.rs`）
> 时间锚：LUM-1469（`4a60bb5da`）之后的下一轮；issue 正文是 LUM-981 伞形任务的 autopilot 复触发
> （「基于 rust 实现 pi 同时兼容 pi 的插件生态 …… 优先完善 tui 的功能」）。
> 本轮同时把两条 **`in_review` 但从未推入远程** 的交付并入 `feature/pi.rs`（§0.1）。

## 0. 结论速览

| 量 | 本轮实测 | 依据 |
|---|---|---|
| 上游 pi-ts | `ctx.ui.setStatus(key, text)` 的文本进 `FooterDataProvider.extensionStatuses`；footer 把它 push 成**第三行**：按 key 排序 → 每项 `sanitizeStatusText` → 空格连接 → `truncateToWidth(width, dim("..."))` | `components/footer.ts:236-251`、`core/footer-data-provider.ts:132-147` |
| Rust 端口（本轮前） | `setStatus` 是 **inert no-op**：shim 的 unsupported 列表里 `reportUnsupported("setStatus")`，`ctx.ui` 对照表写「out of scope」——`git grep -c 'setStatus' -- pi-rust/crates` 只有「不支持」的文档与告警 | `pi-ext-shim.mjs:1100`（基线）、`pi-tui/src/app.rs:200-203`（基线） |
| 本轮后 | `setStatus` 走完整通路：JS shim → `host_ui_region("setStatus", {key,text})` → `UiRegionHost::set_status` → `RegionOp::Status` → `App::set_extension_status` → `StatusBar::render_lines` 的第三行 | §2 |
| 新增行为测试 | **14 条**（`pi-tui` +12 = 5 单测 + 7 帧/行为；`pi-extensions` +1 端到端；`pi-coding-agent` 驱动级 +1） | §4.2 |
| 新增帧截图 | **3 张**（`docs/screenshots/lum1481-*.{png,txt}`） | §5 |
| `pi-tui` 全量 | **1198 passed / 0 failed**（同机同工具链基线 `377e1aa3c` = **1186/0** → **+12**） | §4.2 |
| `pi-coding-agent` | **844 / 33**（基线 **843 / 33**，37 个失败名逐条相同 → **新增失败 0**） | §4.3 |
| `pi-extensions` | **132 / 5**（5 条全是 `ENOENT: open '/dev/urandom'` 的 Windows 环境类，不属本轮面） | §4.4 |
| 纯代码规模（src↔src） | **94.6%**（144,896 / 153,106；基线 144,638） | `scripts/measure_loc.py` |
| 测试规模 | **51.9%**（2,887 / 5,563） | §4.5 |
| `app.*` 接线 | **44/44 = 100%**（silent 0 / advertised 0） | `scripts/app_action_coverage.py` |
| 加权完成度 | **87.2%**（87.1 → 87.155） | §4.6 |

一句话结论：**上游 footer 的第三行是一个「扩展说此刻在干什么」的通道，Rust 端口把它整个当成了不存在——
`setStatus` 被记录为 unsupported，插件调了只拿到一条 warning。** 本轮把它接通到真 TUI，
并顺带把 shim 侧的 map 接给自定义 footer 的 `footerData.getExtensionStatuses()`。

### 0.1 本轮并入的两条未推交付（issue 要求「都合并 feature/pi.rs」）

开工时 `origin/feature/pi.rs` = `9b008633b`（已含 LUM-1455），而两条 **`in_review`** 的交付
**只在本机分支上、从未推入远程**：

| issue | 分支 tip | 内容 | 合并后测试增量 |
|---|---|---|---|
| **LUM-1467** | `agent/winpi/lum1467` = `fe8b6d35e` | footer stats 行对齐上游字段（`↑/↓`、`CH%`、`$cost (sub)`、`(auto)`、`(provider)` 前缀，stats 左 / model 右） | `pi-tui` +17（LUM-1467 自述） |
| **LUM-1469** | `lum1469-tui` = `0b39689ef`（含 `4a60bb5da`） | 排队输入的可见面（composer 上方 `Steering:` / `Follow-up:` 块 + `↳ <chord>` 取回提示）+ 救回的 LUM-1461 / LUM-1263 | `pi-tui` +17（LUM-1469 自述） |

两条并入 `feature/pi.rs` 的合并提交：`f302497b1`（LUM-1467）与 `377e1aa3c`（LUM-1469）。
唯一冲突是 `RUST_TS_PARITY_METRICS.md` 两侧各自追加一节（都叫 `### 0.19`）：按「两段都留、
按并入顺序重排」解决。推入远程前又与另一个 run 推上来的 LUM-1469 分支（`d92a33450`，同时引入了 §0.19 LUM-1461 / §0.20 LUM-1263）合并，最终编号是 §0.19 LUM-1461、§0.20 LUM-1263、§0.21 LUM-1457、§0.22 LUM-1455、§0.23 LUM-1467、§0.24 LUM-1469、§0.25 LUM-1481（本轮）。
合并后的 `pi-tui` 基线实测 **1186/0**（两条自述数字不能直接相加：各自基线不同），
`pi-coding-agent` 基线 **843/33**（失败集合与后续对照逐条相同）。

## 1. 真实审计：缺的是**一整条通道**，不只是「少一行字」

### 1.1 上游取证（第一手，本机读源码，不转述）

```ts
// packages/coding-agent/src/modes/interactive/components/footer.ts:236-251
const pwdLine = truncateToWidth(theme.fg("dim", pwd), width, theme.fg("dim", "..."));
const lines = [pwdLine, dimStatsLeft + dimRemainder];

const extensionStatuses = this.footerData.getExtensionStatuses();
if (extensionStatuses.size > 0) {
    const sortedStatuses = Array.from(extensionStatuses.entries())
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([, text]) => sanitizeStatusText(text));
    const statusLine = sortedStatuses.join(" ");
    lines.push(truncateToWidth(statusLine, width, theme.fg("dim", "...")));
}
```

```ts
// packages/coding-agent/src/core/footer-data-provider.ts:140-147
setExtensionStatus(key: string, text: string | undefined): void {
    if (text === undefined) this.extensionStatuses.delete(key);
    else this.extensionStatuses.set(key, text);
}
```

```ts
// packages/coding-agent/src/modes/interactive/components/footer.ts:13-20
function sanitizeStatusText(text: string): string {
    return text.replace(/[\r\n\t]/g, " ").replace(/ +/g, " ").trim();
}
```

三件事值得单独记下来，因为它们决定了本轮的实现形状：

1. **行数由数据决定**：`extensionStatuses.size === 0` 时**不 push**，即「这个 App 的 footer
   是 1 行 / 2 行 / 3 行」完全取决于此刻有没有扩展在报状态。所以它必须走
   [`ExtensionFrame::status`](../crates/pi-tui/src/extension_ui.rs) 的**数据驱动**路径
   （LUM-1466 已经把 1↔2 行做成数据驱动，本轮把它变成 1↔2↔3）。
2. **`undefined` 是删除**，不是「设为空串」：键的生命周期归扩展。
3. **`ctx.ui.setStatus` 不只给内置 footer 用**：`setFooter` 的自定义 footer 也能通过
   `footerData.getExtensionStatuses()` 读到同一份 map（上游 CHANGELOG #600）。
   Rust 端口此前连 `footerDataStub.getExtensionStatuses()` 都硬编码返回 `new Map()`。

### 1.2 缺口复现（基线 `377e1aa3c`）

```bash
$ git grep -n 'setStatus' 377e1aa3c -- pi-rust/crates
pi-extensions/docs/EXTENSIONS.md:112:  ... (`setStatus`, `setTitle`, …) stay inert no-ops with a one-time warning notification.
pi-extensions/docs/SDK_MODULES.md:437: | `setStatus`, `setTitle`, … | No-op with a one-time warning notification. ...
pi-extensions/runtime/pi-ext-shim.mjs:1085: "setStatus",      # ← unsupported 列表里
pi-tui/src/app.rs:200: //! `setStatus` / `setWorkingMessage` / `setTitle` ... are *not* region-shaped
# 生产代码里没有任何一处把 setStatus 的文本接到渲染面：0 命中。
```

即：**Rust 端口把「插件往 footer 报状态」这件事记成了「不存在的能力」**，而它是上游
`ctx.ui` 里使用频率最高的 fire-and-forget 方法之一（上游文档
`packages/coding-agent/docs/extensions.md:2520` 的「Status indicators (setStatus)」、
`docs/tui.md:772` 的示例都用它）。

### 1.3 为什么这一格值得单独一轮

* **它不在「TUI 长得对不对」这一层**，而在「插件生态能不能表达状态」这一层 ——
  LUM-1445 / LUM-1448 / LUM-1432 已把 `ctx.ui` 的对话框、补全 provider、生命周期事件接通，
  `setStatus` 是剩下唯一的**显示通路**缺口（LUM-1469 §6 #4 列为下一轮第一顺位）。
* **它跨 3 个 crate**：`pi-tui`（渲染）← `pi-coding-agent`（桥）← `pi-extensions`（宿主 + JS shim）。
  只改 `pi-tui` 会得到一个「谁都调不到」的 API（这正是 LUM-1236 在补全面上踩过的坑）。
* **它必须保持既有帧不变**：仓库里有 1100+ 条帧/几何断言，任何「footer 固定 3 行」的实现
  都会把它们全打红。数据驱动是唯一不破坏这些断言的路。

## 2. 落地（文件:行号）

| 文件 | 改动 | 说明 |
|---|---|---|
| `pi-rust/crates/pi-tui/src/status.rs` | `+202/-9` | `StatusData::extension_statuses: Vec<(String,String)>`（`:163`，默认空）；`set_extension_status`（`:203`，保持 key 有序，`None` 删键）；`with_extension_status`（`:214`）；`clear_extension_statuses`（`:226`）；`line_count` 按数据 +1（`:385`）；`render_lines` 追加第三行（`:400-416`）；`extension_status_line`（`:921`，按 key 排序 + 空格连接 + 超宽 `…` 标记）；`sanitize_status_text`（`:949`，对齐上游 `sanitizeStatusText`） |
| `pi-rust/crates/pi-tui/src/app.rs` | `+35/-4` | `ctx.ui` 对照表新增 `setStatus → App::set_extension_status`；`App::set_extension_status`（`:2143`）；`App::clear_extension_statuses`（`:2149`）。渲染路径无需改动：`composed_frame` 已经用 `status_bar.line_count(...)` 定 `frame.status` |
| `pi-rust/crates/pi-extensions/src/host.rs` | `+25` | `UiRegionHost::set_status(key, text)`（`:1187`，带文档的默认实现契约）；`RegionCommand::Status`（`:1214`）；`region_worker` 分发（`:1251`）；`handle_region_call` 的 `"setStatus"` 分支（`:1326`，`null`/缺字段 = 清键） |
| `pi-rust/crates/pi-extensions/runtime/pi-ext-shim.mjs` | `+33/-11` | `extensionStatuses` map（`:763`）；`ui.setStatus(key, text)`（`:852`）真正转发（`undefined`/`null` 清键），并从 unsupported 列表移出；`footerDataStub.getExtensionStatuses()` 返回该 map 的**副本**（`:1045`） |
| `pi-rust/crates/pi-coding-agent/src/extensions/ui_bridge.rs` | `+19/-1` | `RegionOp::Status { key, text }`（`:366`）；`TuiRegionHost::set_status`（`:456`）；pump 应用 → `app.set_extension_status(&key, text.as_deref())`（`:623`） |
| `pi-rust/crates/pi-tui/tests/lum1481_extension_status.rs` | **新增 231 行 / 7 条** | 第三行位置与顺序、正好占 transcript 一行、清空复原、按 key 清、超宽 `…`、3 张帧 |
| `pi-rust/crates/pi-extensions/tests/host.rs` | `+92` | `RecordingRegionHost::set_status` 记录 + `footer()` 访问器；新用例 `region_host_receives_extension_statuses_and_a_custom_footer_sees_them`（含「自定义 footer 拿到的是副本、改不动宿主的 map」） |
| `pi-rust/crates/pi-coding-agent/tests/extension_ui.rs` | `+60` | `interactive_extension_statuses_reach_the_footer_row`：真 JS 扩展 → 真 host → 真 pump → 真 `App` → 渲染帧的末行 |
| `pi-extensions/docs/EXTENSIONS.md` / `SDK_MODULES.md` | `+7/-4` | `setStatus` 从「inert no-op」改为「转发 + 渲染为 footer 第三行 / shim map 供自定义 footer 读取」 |

### 2.1 三条不变量（都有测试钉住）

1. **没有状态 ⇒ 行数不变**：`extension_statuses` 为空时 `line_count` 与 `render_lines` 与
   LUM-1466/1467 的帧**逐字节相同**（`frame_dump_extension_status_cleared_100x30` 就是回归帧）。
2. **有状态 ⇒ 正好多一行，且 transcript 只让出这一行**：
   `the_status_row_costs_the_transcript_exactly_one_row` 断言 header + transcript 区
   **逐行相等**，composer 与既有两行 footer **整体上移一行**，第三行是帧的最后一行。
3. **一行永远是一行**：上游 `sanitizeStatusText` 把 `\r\n\t` 压成空格、折叠连续空格、trim，
   所以扩展塞多行文本也进不了第二行；超宽由 `truncateToWidth` 截断
   （本端口用 crate 统一的 `…` 标记，见 §8 偏差 1）。

### 2.2 为什么第三行画在**最底**而不是 stats 行之上

上游是 `lines.push(statusLine)` —— 追加在 `[pwdLine, statsLine]` **之后**，所以顺序是
`pwd` → `stats` → `status`。本端口照抄（`render_lines` 的 push 顺序），
测试 `an_extension_status_is_the_footers_third_row` 直接钉住这个顺序（第 27/28/29 行）。

## 3. 反向验证（本机实做）

把 `StatusBar::line_count` 里的扩展状态增量删掉（只删这 3 行，不动渲染）：

```
tests/lum1481_extension_status        7 条里 5 条立刻红
  an_extension_status_is_the_footers_third_row
  the_status_row_costs_the_transcript_exactly_one_row
  clearing_the_last_status_gives_the_row_back
  a_status_too_wide_for_the_terminal_is_marked_not_wrapped
  frame_dump_extension_status_100x30
pi-coding-agent/tests/extension_ui
  interactive_extension_statuses_reach_the_footer_row   FAILED（断言 last == "thinking queued"）
```

恢复后 7/7（帧）、42/42（`status::`）、8/8（`extension_ui`）全绿。
即这两批断言真的钉住「第三行由数据驱动」这件事，而不是钉住别的什么。

## 4. 门禁与数字（本机 Windows / cargo 1.97.1 / `--offline`）

### 4.1 fmt / clippy

| 门禁 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --offline -p pi-tui --all-targets -- -D warnings` | **exit 0** |
| `cargo clippy --offline -p pi-tui -p pi-extensions -p pi-coding-agent --all-targets` | 改动文件 **0 告警**；输出里的告警全部落在未改动的 `pi-extensions::signal_name`（dead code，既有）与 vendored `rquickjs-core`（13 条） |

### 4.2 `pi-tui`

```
$ cargo test --offline -p pi-tui -j 8            # 本 tip
targets + doc-tests  passed 1198  failed 0
$ git checkout 377e1aa3c && cargo test --offline -p pi-tui -j 8   # 同机同工具链基线
targets + doc-tests  passed 1186  failed 0
```

**+12 = 5（`status.rs` 单测）+ 7（`lum1481_extension_status.rs`）**，与
`#\[(tokio::)?test\]` 标记数 2,873 → 2,887 的 +14 对得上（另 2 条在
`pi-extensions/tests/host.rs` 与 `pi-coding-agent/tests/extension_ui.rs`）。

### 4.3 `pi-coding-agent`（含集成 target）

```
$ cargo test --offline -p pi-coding-agent -j 4 --no-fail-fast
本轮 tip      passed 844  failed 33
基线 377e1aa3c passed 843  failed 33
```

33 条失败**逐条同名**（`comm -13/-23` 双向为空），全是 Windows 环境类：真 `bash` 工具、
绝对路径断言、`/tmp`、node fs、project trust、扩展发现。新增通过 1 条 = 本轮驱动级用例。

### 4.4 `pi-extensions`

```
$ cargo test --offline -p pi-extensions -j 8 --no-fail-fast
本轮 tip  passed 132  failed 5      # node_builtins 3 + sdk_modules 1 + web_globals 1
```

5 条 panic 文本全是 `ENOENT: 系统找不到指定的路径。 (os error 3), open '/dev/urandom'`，
即 Windows 没有 `/dev/urandom`；与 LUM-1445 §三 记录的同一批（当时记 3 条，本轮同口径重跑
`node_builtins` 仍是那 3 条）。本轮的 `host.rs` 新用例在同一次运行里 **ok**。

### 4.5 Rust↔TS 口径（可复现）

```bash
python pi-rust/scripts/measure_loc.py                    # rust src 144,896 / ts 153,106 = 94.6%
grep -rhoE '#\[(tokio::)?test\]' pi-rust/crates --include=*.rs | wc -l    # 2,887
grep -rhoE '^\s*(it|test)(\.\w+)?\(' packages --include=*.test.ts | wc -l # 5,563
python pi-rust/scripts/app_action_coverage.py            # wired 44/44
```

| 口径 | 本轮 | 基线 `377e1aa3c` | 说明 |
|---|---|---|---|
| Rust src（去 mod.rs） | **144,896 / 254 文件** | 144,638 | 本轮自身 **+258 行 src**（`status.rs` +202、`app.rs` +35、`host.rs` +25、`ui_bridge.rs` +19，扣除删减） |
| TS src | 153,106 / 669 文件 | 同 | 未动（上游只读取证） |
| 规模比 | **94.6%** | 94.5% | |
| Rust `#[test]` | **2,887** | 2,873 | +14 |
| TS 用例 | 5,563 | 同 | 同口径 |
| 测试比 | **51.9%** | 51.7% | 2887/5563 |
| TUI 模块 | 36 / 42 = 85.7% | 同 | 本轮没有新增模块（改的是既有模块内的行） |
| `app.*` 接线 | 44/44 = 100% | 同 | 未动（LUM-1263 已关闭最后一格） |
| 扩展事件 | 36/36 声明 + 36/36 构造点 | 同 | 未动 |
| **`ctx.ui` 显示通路**（本轮新量，分两类） | **区域类 5/5 = 100%**（`setWidget`/`setHeader`/`setFooter`/`setEditorComponent`/`setStatus`）；**文本类 0/3**（`setTitle`/`setEditorText`/`setTheme`） | 区域类 4/5（`setStatus` 缺） | 这是本轮真正动的那条轴 |

### 4.6 加权完成度（公式与权重沿用 `RUST_TS_PARITY_METRICS.md` §4.1）

| # | 轴 | 权重 | 得分 | 依据 |
|---|---|---|---|---|
| 1 | 可构建 / 可测 / 可运行 | 5% | 100% | §4.1–4.4 |
| 2 | 核心 agent 循环 | 13% | 90% | 未动 |
| 3 | provider API family | 8% | 100% | 未动 |
| 4 | provider / 模型目录广度 | 6% | 70% | 未动 |
| 5 | TUI 交互面（模块率与接线率均值） | 14% | 92.9% | (0.857 + 1.000) / 2，未动 |
| 6 | TUI 视觉保真 | 8% | 90% | **不上调**，见下 |
| 7 | slash 命令面 | 7% | 78% | 未动 |
| 8 | CLI / 模式 / 子命令面 | 7% | 70% | 未动 |
| 9 | 扩展宿主能力 | 8% | 95% | 未动 |
| 10 | 扩展生命周期事件 | 7% | 100% | 未动 |
| 11 | 会话 / 存储 / 导入导出 | 9% | 85% | 未动 |
| 12 | 测试与门禁强度 | 5% | **51.9%** | §4.5 |
| 13 | 子包完整度 | 3% | 95% | 未动 |

```
5×1.000 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.929 + 8×0.90 + 7×0.78 + 7×0.70
+ 8×0.95 + 7×1.00 + 9×0.85 + 5×0.519 + 3×0.95 = 87.155% → **87.2%**
```

**口径声明（诚实读法）**：+0.05pt 全部来自第 12 轴（2,873 → 2,887 条用例）。
第 9 轴（扩展宿主能力 95%）**本轮不上调**：`setStatus` 让宿主多了一条显示通路，
但它不是 13 轴里「有没有这个能力」的新能力（`ctx.ui.setWidget` 等早已在位），
把它从 95% 抬到 97% 只值 +0.16pt 且会掩盖真正剩下的缺口（`setTitle` / `setEditorText` /
`setTheme` / 扩展 `registerShortcut` 之外的编辑器接管）。按 LUM-1418 §6 的规矩：
**先把哪一项抬了多少说清楚，再改口径**——本轮不改。

## 5. 截图（frame-buffer，本机无 PTY）

| 文件 | 内容 | 生成方式 |
|---|---|---|
| `docs/screenshots/lum1481-extension-status-100x30.{png,txt}` | 100×30：末尾三行 = `/srv/repo (main)` → stats 行 → `thinking compiling`（按 key 排序后的两个扩展状态） | `cargo test -p pi-tui --test lum1481_extension_status -- --nocapture` → `scripts/frame_to_png.py` |
| `docs/screenshots/lum1481-extension-status-cleared-100x30.{png,txt}` | 同一 App 清空状态后：footer 回落到两行，transcript 多回一行（**回归帧**，与 LUM-1467 的帧同形） | 同上（`frame_dump_extension_status_cleared_100x30`） |
| `docs/screenshots/lum1481-extension-status-cut-44x16.{png,txt}` | 44×16：一条远超终端宽度的状态被截断并打 `…`，**仍只占一行** | 同上（`frame_dump_extension_status_cut_44x16`） |

**证据分级（重要）**：这三张是 `App::render_to_buffer` 的**冻结帧**，证明「画在哪一格、
内容是什么」，**不证明按键/字节时序**；时序与几何由 §3 的 14 条 App/host/驱动级用例覆盖。
本机没有 `pty`（`import pty` 不可用），真 PTY 录制仍应在 Linux runner 上用
`scripts/pty_capture.py` 对同一场景补一次。

## 6. 全 TUI 问题清单（本轮实测更新）

| 顺位 | 问题 | 证据 | 本轮处置 |
|---|---|---|---|
| 1 | ~~footer 只有一行~~ | LUM-1466 §6 #1 | 已在 `feature/pi.rs` |
| 2 | ~~footer stats 行缺 `↑/↓`、`CH%`、`$cost`、`(auto)`、`(provider)` 前缀~~ | LUM-1467 | **本轮并入** `feature/pi.rs`（§0.1） |
| 3 | ~~扩展 `ctx.ui.setStatus` 无通路（`ERR_PI_UI_UNSUPPORTED`）~~ | LUM-1469 §6 #4 | **本轮关闭**（§2，14 条测试 + 3 帧） |
| 4 | ~~排队输入画在日志尾部、无取回提示~~ | LUM-1469 §1 | **本轮并入** `feature/pi.rs`（§0.1） |
| 5 | `setTitle` / `setEditorText` / `setTheme` / `setWorkingMessage` 仍是 no-op | `pi-ext-shim.mjs` 的 unsupported 列表 | **未做**，低于 TUI 面优先（见 §7） |
| 6 | CLI flag 面（字面 18/40） | `RUST_TS_PARITY_METRICS.md` §3.4 | 在办：LUM-1434（另一 agent，面不重叠） |
| 7 | `/transcript` 会把排队块带进快照 | LUM-1469 §6.1 | 未做（判断为「可接受」，记录备查） |
| 8 | 44×16 帧里 `cut above` 提示会与正文首行叠字 | LUM-1469 §6.1 的 `lum1469-cut-queued-draft-44x16.txt` | 未做；与本轮无关，但读起来像乱码，值得单独一轮 |

### 6.1 本轮顺带发现（记录，不在本轮修）

* **`footerData.getGitBranch()` 仍是硬编码 `undefined`**（`pi-ext-shim.mjs` 的
  `footerDataStub`）：上游的自定义 footer 能从它拿到分支名。本轮只把
  `getExtensionStatuses()` 接成真数据，`getGitBranch()` 仍不接——因为分支的权威来源在
  驱动侧（`App::set_status_git_branch` 的输入），把它反向暴露给 JS 需要一条
  host→JS 的查询通道，属于下一个切片。
* **扩展状态的清除时机**：上游 `setStatus` 的 map 生命周期跟着 `FooterDataProvider`
  （进程级），所以 `session_start` 里设的状态**跨 `/new` 仍存在**——本端口把 map 放在
  `App::status_data` 里，`/new` 是否会重置取决于驱动是否重建 App。本轮**不改**这个语义
  （上游行为就是「扩展自己负责清」），但值得在 `ExtensionFrame` 的文档里写一句。

## 7. 计划与派发：**零派发**（并说明为什么不是「跳过」）

**「如果任务存在是跳过还是计划和实现后续任务」的回答**：LUM-981 是已完成（`done`）的伞形任务，
autopilot 把它按 20 分钟一轮复触发（LUM-1469 → 本轮 LUM-1481）；本轮**不跳过**，在同一 run 内
完成 issue 点名的四件事：**TUI 审计 → 缺口实现 → 截图 → 推送合并**（外加把两条未推的交付救回）。

开工时实测 board（`multica issue list --project ae0b46e7…` 过滤非终态）+ 运行器
（`multica daemon status --output json`）：

| 量 | 实测 |
|---|---|
| daemon `running_task_count` | **5**（≥ 「最多 3 个任务同时运行」的上限） |
| 本仓在办面 | LUM-1467（`in_review`，**本轮已合入**）、LUM-1469（`in_progress`，**本轮已合入**） |

**零派发**，理由三条：

1. **槽位已满**：`running_task_count = 5`，任何新 run 都会把并发推到事实上的 6 —— 本仓库已经
   为「同一缺陷两条并发线各修一次」清过两次（LUM-1431 §3、LUM-1445 §8）。
2. **本轮的面是「同一屏几何」**：`setStatus` 要改 `status.rs` 的 `line_count` / `render_lines`，
   与任何 footer / chrome 面的任务正面冲突。
3. **剩下的候选要么已被占，要么不值得并发**：CLI flag 面（LUM-1434）在另一 runtime 上；
   `setTitle` / `getGitBranch` 反向通道与「cut above 叠字」都还要动 `app.rs` 的渲染路径，
   与 LUM-1467 刚合入的 stats 行是同一批断言。

**下一轮第一顺位（单子化，不派 run）**：`setTitle`（终端标题，`OSC 2`）——它是
unsupported 列表里唯一**已有明确宿主语义**、且不碰 footer 几何的一条；本端口的
`pi-tui` 已经有写 OSC 8 超链接的先例（`hyperlink.rs`），通道可以直接复用。
第二顺位是 `footerData.getGitBranch()` 的 host→JS 查询通道（§6.1）。

## 8. 范围之外与已知偏差

未碰 `pi-ai` / `pi-agent-core` / `pi-protocol` / `pi-session` / `pi-server` / `pi-client` /
`pi-chord` / `pi-evals` / `pi-telemetry` 的源码；未碰上游 TS（`packages/**`，只读取证）；
未碰 CI / Docker；未改扩展事件表、slash 命令表、CLI flag 表、`CONSUMED_APP_ACTIONS`
（`app_action_coverage.py --check-consumed` 与 `status.rs` 无关）。

**已知偏差（写清而不是省略）**：

1. **截断标记用 `…` 而不是上游的 `"..."`**：上游 `truncateToWidth(statusLine, width,
   theme.fg("dim", "..."))` 用的是三个点；本端口对**所有** chrome 行的截断标记统一用
   `U+2026`（LUM-1412 的契约，`location_span` 对 pwd 行已经这么做了）。选一致性而不是逐字符
   照抄，并在此写明。
2. **排序用字节序而不是 `localeCompare`**：扩展的 key 是 ASCII 名字，两者结果相同；
   非 ASCII key 的排序可能与 JS 的 locale 规则不同（上游自己也没指定 locale）。
3. **`text` 的非字符串输入**：上游类型是 `string | undefined`；shim 对 `null` 也当清键处理
   （超集），对其它类型做 `String(text)` 强制转换（`null` 除外）。`text === ""` 会**保留**
   一个空状态（上游同样保留：`""` 不是 `undefined`），此时 footer 多一行空白——这是上游行为，
   不是本端口的 bug。
4. **不做 `(sub)` 之外的 provider 前缀联动**：stats 行与状态行互不感知（上游也是两个独立的
   push），所以状态行不会被 LUM-1467 的 `(auto)` / `(provider)` 逻辑影响。
5. **`setStatus` 不注册鼠标目标**：状态行是 host chrome，不是点击目标（上游的 footer 也不是），
   与 LUM-1469 对排队块的处理一致。
