# LUM-1436 — composer 下拉框滚轮（chatinput 鼠标面收口）+ 真实审计

> scope: `pi-rust/`（分支 `work/LUM-1436`，基线 `origin/feature/pi.rs` = `b0c9f89a1` = LUM-1431）
> 时间锚：LUM-1431 之后。本轮领 LUM-1431 §7 的**第一顺位**（下拉框滚轮），并把该节的其余三条
> 逐条复核——其中一条**经上游源码核对后判定为伪缺口**，见 §1.2。
> 交付物：`crates/pi-tui/src/{input,app}.rs`、`crates/pi-tui/tests/autocomplete_wheel.rs`（新，7 条）、
> `crates/pi-tui/tests/lum1436_autocomplete_wheel_frames.rs`（新，2 帧）、两张真帧截图、
> `RUST_TS_PARITY_METRICS.md` §0.11、本文。

## 0. TL;DR

- **chatinput 缺口（本轮修掉）**：`InputEvent::Mouse`（滚轮）**不带坐标**，所以 App 只能把滚轮丢给
  聊天日志视口 —— 下拉框虽然是鼠标目标（LUM-1431 补的点选），但**滚轮在它上面会滚背后的聊天记录**。
  现在滚轮带上终端格坐标，按上游的优先级派发：**下拉框先认领**（在列表矩形内则移动高亮一格、钳制在两端），
  没人认领才走 `routeWheel`（滚视口 + 刷新滚动条 hover）。这是 composer 鼠标面的最后一块
  （点击定位光标 LUM-1426 → 下拉框点选 LUM-1431 → 下拉框滚轮 本轮）。
- **审计更正 1（伪缺口）**：LUM-1431 §7 第 2 条「`#` 触发符空转」**不成立**。上游
  `DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS = ["@", "#"]`（`packages/tui/src/components/editor.ts:251`）
  是**扩展注入点**：基础 `CombinedAutocompleteProvider` 没有 `#` 分支，`getSuggestions` 返回 `null`，
  上游因此**不打开下拉框**。Rust 端口逐字同构（`editor.rs:240`、`request_autocomplete` 在
  `None` 上 `cancel_autocomplete()`），不是缺口、也不该"补"——补了反而与上游分叉。
- **审计更正 2（门禁）**：LUM-1431 §5 声称 `cargo fmt --all -- --check` 干净，**在本机复现不出来**：
  基线 `b0c9f89a1` 上 `lum1431_autocomplete_frames.rs` 有 4 处 rustfmt 差异（cargo 1.97.1）。
  本轮 `cargo fmt --all` 顺手修绿，并把差异记在这里（不是本轮引入的）。
- **实测**：`pi-tui` **1007 passed / 0 failed**（基线 998 / 0 → **+9** = 行为 7 + 帧 2）；
  `--workspace` **2629 passed / 39 failed / 2 ignored**（基线 2620 / 39 / 2，**+9 通过、失败条数不变**，
  38 个唯一失败名的逐条落点与 LUM-1431 §5 同一批 Windows 环境类，无一在 `pi-tui`）。
- **截图**：`docs/screenshots/lum1436-autocomplete-wheel-before-76x16.png`（列表打开、`❯ help` 高亮）
  与 `-after-`（在列表行上滚一格后 `❯ model`、草稿仍是 `/`、被列表借用的 transcript 行逐字未变），
  均带可 grep 的 `.txt`。

## 1. 真实审计

### 1.1 缺口的形态（可复算）

上游把滚轮当成**带坐标的鼠标事件**走完整条派发链
（`packages/tui/src/tui-alt-screen.ts:679-694`）：

```ts
const wheelEvent = this.parseWheelEvent(data);
if (wheelEvent) {
  const event = this.createMouseEvent("wheel", wheelEvent.button, wheelEvent.x, wheelEvent.y, {
    wheelDelta: wheelEvent.direction * this.getWheelScrollLines(wheelEvent.button),
  });
  const overlay = this.dispatchMouseToOverlay(event);
  const result = overlay.result ?? (overlay.hit ? undefined : this.dispatchMouseToLayout(event));
  if (result) { …; return { consume: true }; }      // ← 组件先认领
  if (this.shouldDeferViewportInputToOverlay()) return undefined;
  this.routeWheel(wheelEvent);                       // ← 没人认领才滚视口 + updateScrollbarHover
  return { consume: true };
}
```

`dispatchMouseToLayout` 把格子转成组件局部坐标交给命中的组件
（`tui-alt-screen.ts:808-828`），编辑器的 `handleMouse` **第一件事**就是试列表矩形
（`packages/tui/src/components/editor.ts:618-638`，只判 `y`，因为编辑器的布局盒是整行宽）；
`SelectList.handleMouse` 的滚轮分支（`packages/tui/src/components/select-list.ts:110-121`）：

```ts
if (event.type === "wheel" && event.wheelDelta) {
  const delta = event.wheelDelta < 0 ? -1 : 1;      // 只看方向，不看幅度
  this.selectedIndex = Math.max(0, Math.min(len - 1, this.selectedIndex + delta));  // 钳制，不回绕
  …
}
```

Rust 端口在 LUM-1436 之前：`InputEvent::Mouse { up, alt }` 没有坐标（`input.rs` 的旧文档明写
"stays wheel-only"），`App::step` 直接 `scroll_viewport_up/down`（`app.rs:2730` 附近），
下拉框**不在任何滚轮路径上** —— 与 LUM-1431 之前的点选缺陷同源：列表画在 transcript 的格子上，
指针落进去时吃事件的是背后的聊天记录。

### 1.2 「`#` 触发符空转」是伪缺口（逐行核对）

| 事实 | 位置 | 结论 |
|---|---|---|
| 上游默认触发符含 `#` | `packages/tui/src/components/editor.ts:251` | 是**扩展点**，不是内置功能 |
| 上游触发模式由 `buildTriggerPattern` 用触发符拼正则 | 同上 `:257-263` | `#` 会**让编辑器尝试**请求补全 |
| 基础 provider 没有 `#` 分支 | `packages/tui/src/autocomplete.ts:278-350`（`CombinedAutocompleteProvider`：`@` 文件、`/` 命令、命令参数） | 返回 `null` |
| 上游 `null` 的语义 | `packages/tui/src/components/editor.ts` 的 `updateAutocomplete`：无候选即关闭列表 | **不开空下拉框** |
| Rust 逐字同构 | `editor.rs:239-240`、`editor.rs:1913-1955`（`None` → `cancel_autocomplete()`） | 无差异 |

所以 LUM-1431 §7 把「provider 不处理 `#`」写成缺口是**把扩展点当成了未实现功能**。
真正的 `#` 语义属于**技能/工具引用面**（上游由 `pi-coding-agent` 的 provider wrapper 注入），
要补也应在 provider 侧加候选，而不是改编辑器的触发符表。该条已从缺口清单移出。

### 1.3 chatinput ↔ codex / Martty / 上游 pi-ts（本轮后）

| 能力 | codex | Martty | 上游 pi-ts | 本轮前 | 本轮后 |
|---|---|---|---|---|---|
| 多行 compose + 视觉行 Up/Down | ✅ | ✅ | ✅ | ✅（LUM-1282/1312） | ✅ |
| 行域 Home/End、Ctrl+U/K/W/A/E | ✅ | ✅ | ✅ | ✅（LUM-1312） | ✅ |
| 粘贴折叠 `[paste #N +M lines]` | ✅ | — | ✅ | ✅（LUM-1318） | ✅ |
| 草稿内翻页 + 窗口跟随光标 | ✅ | — | ✅ | ✅（LUM-1317） | ✅ |
| `Ctrl+R` 反查 + 跨会话 history | ✅ | ❌（`src/input/editor.rs` 无搜索） | ❌ | ✅（LUM-1415） | ✅ |
| 点击定位光标 | ✅ | ✅（`app.rs:2306+` slot/rect 命中） | ✅ | ✅（LUM-1426） | ✅ |
| 下拉框点选（press/click） | ✅ | ❌（菜单只由键盘过滤） | ✅ | ✅（LUM-1431） | ✅ |
| **下拉框滚轮** | ✅ | ❌ | ✅ | ❌ **缺口** | ✅ |
| 滚轮刷新滚动条 hover | ✅ | — | ✅（`routeWheel` → `updateScrollbarHover`） | ❌ | ✅ |
| `#` 触发符 | —（codex 无此概念） | — | ✅ 扩展点（无内置候选） | ✅ 同构 | ✅ 同构 |

**诚实结论**：issue 原文「chatinput 与 codex/Martty 差距很大」——就**编辑/历史/指针**三面而言，
在 LUM-1312/1415/1426/1431 之后已经不成立（Martty 的 composer 甚至少于 pi-rust：无 history search、
无下拉框指针）。本轮把**唯一还挂着的**一条（滚轮）关掉后，composer 层面与 codex/上游 pi-ts 的
可观察差异只剩「codex 的 `@` 提及绑定 / 会话内图片粘贴预览」这类**别的轴**的事。
Martty 领先 pi-rust 的地方仍然不在 composer：宠物/主题/扩展 UI 宿主（LUM-1415 §1 已记录，本轮复核不变）。

## 2. 改了什么（文件:行）

| # | 位置 | 改动 |
|---|---|---|
| 1 | `pi-tui/src/input.rs:252` | `InputEvent::Mouse`（滚轮）新增 `x`/`y`（终端格，0 基），文档改为引用上游两段派发（组件先认领 → `routeWheel`） |
| 2 | `pi-tui/src/input.rs:334` | 构造函数 `InputEvent::wheel(up, alt, x, y)`（原来只有 `up, alt`） |
| 3 | `pi-tui/src/input.rs:485-486` | `decode_mouse_report` 的滚轮分支把**已经解出来的** `x`/`y` 一起带上（SGR `ESC [ < 64 ; x ; y M` 与 X10 `ESC [ M Cb Cx Cy` 两条编码都走这里） |
| 4 | `pi-tui/src/app.rs:2722-2746` | `App::step` 的滚轮路径：先 `step_autocomplete_wheel`，未认领才滚视口；两者都不变时再用 `update_scrollbar_hover(x, y)` 决定是否重绘 |
| 5 | `pi-tui/src/app.rs:4739` | 新增 `App::step_autocomplete_wheel` —— 上游 `SelectList.handleMouse` 的滚轮语义：一档一格、钳制不回绕、（与 LUM-1431 的坐标口径一致）只判行不判列 |
| 6 | `pi-tui/src/app.rs:5799-5810` | `translate_event` 的 `ScrollUp/ScrollDown` 把 `mouse.column/row` 透传（crossterm 也是 0 基，与 SGR 解码后的口径一致） |
| 7 | `pi-tui/tests/autocomplete_wheel.rs`（新） | **7 条**：一格一格移动且不动 transcript / 两端钳制（不回绕）/ Alt 滚轮也只走一格 / `(n/m)` 计数行同样认领 / 列表外（transcript 行）仍然滚视口 / 列表关闭后原格归还 transcript / 滚轮刷新滚动条 hover |
| 8 | `pi-tui/tests/lum1436_autocomplete_wheel_frames.rs`（新） | 2 个真帧 dump（滚轮前 / 滚轮后），供 `scripts/frame_to_png.py` 出图 |
| 9 | `pi-tui/tests/{mouse_scroll,keybindings,mouse_selection,settings_list}.rs` | 既有构造点补坐标（`wheel(up, alt, 0, 0)`；`translate_event` 的断言用真实的 `column/row = 1`） |
| 10 | `pi-tui/tests/lum1431_autocomplete_frames.rs` | **仅 rustfmt**（基线即带 4 处格式差异，见 §0「审计更正 2」），无语义改动 |
| 11 | `RUST_TS_PARITY_METRICS.md` | 新增 §0.11（本轮可复算数字） |

**未碰**：`pi-coding-agent` / `pi-ai` / `pi-agent-core` / `pi-session` / `pi-server` / `pi-client` /
`pi-extensions`；键位表（`app.*` 接线率 43/44 不变）；slash 命令表；扩展事件轴；无新依赖。

## 3. 派发优先级：上游 vs 本端口（逐条对齐）

| 场景 | 上游 | 本端口（本轮后） |
|---|---|---|
| modal 打开时滚轮 | overlay 先拿（`dispatchMouseToOverlay`） | settings 模态独占（`step_settings_wheel`，未变）；selector/dialog 打开时滚轮 `Idle`（既有行为，未变） |
| 指针在下拉框行上 | 编辑器 → `SelectList` 认领，**不滚视口** | `step_autocomplete_wheel` 认领（含计数行），**不滚视口** ✅ |
| 指针在 transcript 上 | `routeWheel`：指针处 scroll view → 主 scroll view | `scroll_viewport_up/down`（单一视口） |
| 指针在滚动条上 | `routeWheel` 先滚主视口，再 `updateScrollbarHover(x, y)` | 滚视口 + `update_scrollbar_hover(x, y)` ✅ |
| 横向滚轮（6/7） | `parseWheelEvent` 不认 → 丢给按键路径 | `InputEvent::Ignored`（未变） |

## 4. 证据

### 4.1 行为断言（真 `App::step`）

`cargo test -p pi-tui --test autocomplete_wheel` → **7 passed / 0 failed**。
**反向验证**（本机实做）：把 `App::step` 里那一行认领调用换成 `None::<StepOutcome>` 后，
其中 **4 条**（一格移动 / 两端钳制 / Alt 滚轮 / 计数行）立刻红，另外 3 条（transcript 滚动、
关闭后归还、hover）与认领无关仍绿——即这批断言真的钉住了本轮改动，不是"恰好通过"。

### 4.2 真帧截图（无 PTY，frame-buffer 通道）

| 文件 | 画面 |
|---|---|
| `docs/screenshots/lum1436-autocomplete-wheel-before-76x16.png`(+`.txt`) | 列表打开：`❯ help` / `  model` / `  clear`，草稿 `/`，三行候选借用 transcript 的最后三行 |
| `docs/screenshots/lum1436-autocomplete-wheel-after-76x16.png`(+`.txt`) | 在列表行上滚一格后：`  help` / `❯ model` / `  clear`，草稿仍 `/`，**被借用的 transcript 行逐字未变**（帧测试里对滚轮前后各取 3 行做字符串相等断言） |

**诚实分级**：本机 Windows 无 `pty`，这两张是 `App::render_to_buffer` 的**冻结帧**，证明几何与高亮，
**不证明滚轮时序**；交互时序的证据是 §4.1 的 7 条行为断言（它们驱动真实的 `step` 路径）。
`App::translate_event` 层面的坐标透传由 `tests/mouse_scroll.rs::scroll_up_and_down_translate_to_mouse_events`
钉住（断言 `column/row = 1` 落到 `x/y = 1`）。

## 5. 门禁与实测（本机 Windows / cargo 1.97.1 / `--offline`）

```console
$ CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 cargo fmt --all -- --check
  干净（含修正基线遗留的 lum1431_autocomplete_frames.rs）
$ … cargo clippy --offline -p pi-tui --all-targets -- -D warnings
  Finished `dev` profile …（0 warning）
$ … cargo test --offline -p pi-tui
  1007 passed / 0 failed            （基线 origin/feature/pi.rs tip b0c9f89a1 = 998 / 0 → +9）
$ … cargo test --offline --locked --workspace --no-fail-fast
  2629 passed / 39 failed / 2 ignored（基线 2620 / 39 / 2 → +9 通过、失败条数不变）
  失败逐条落在 pi-coding-agent(8 + cli_extensions 10 + cli_tools 4 + reload_config 1
  + system_prompt_resources 1 + tools 4 + tools_navigation 3 + tools_render 2)
  + pi-extensions(node_builtins 3 + sdk_modules 1 + web_globals 1)：38 个唯一失败名，全部是
  真 bash / 绝对路径断言 / node fs / trust 这类 Windows 环境类，与 LUM-1431 §5 同一批；pi-tui 零失败。
```

**运行器约束（本轮真实遇到，记录在案）**：本机磁盘在跑 workspace 门禁时**写满**（`df` 显示 `0` 可用），
rustc 报 `IO failure on output stream: no space on device`，并留下一个损坏的
`lum1431_autocomplete_frames-*.pdb` 让链接器报 `LNK1285`。处置：删掉 **已完成 run 的 rebuildable
构建产物**（`lum-1418/lum-1422/lum-1415` 三个旧 workdir 的 `pi-rust/target`，共约 90G，非交付物、
可从源码重建）、删掉那个损坏的 `.pdb`，然后复跑门禁通过。**未**删除任何交付物、git 分支或日志。

## 6. Rust↔TS 差距复测（本轮亲自跑）

| 量 | 本轮实测 | LUM-1431 | 命令 |
|---|---|---|---|
| 纯代码规模（src↔src） | **88.8%**（136,016 / 153,106） | 88.8%（135,950） | `python pi-rust/scripts/measure_loc.py`（从仓库根跑） |
| 测试规模 | **49.7%**（2,637 / 5,309） | 49.5%（2,628） | `grep -rhc '#\[test\]\|#\[tokio::test\]' pi-rust/crates --include=*.rs \| paste -sd+ \| bc` 与 `find packages -name '*.test.ts' \| xargs grep -ch '\bit(\|\btest(' \| paste -sd+ \| bc` |
| TUI 模块面 | **35 / 42 = 83.3%** | 35/42 | `pi-tui/src`（36 − `lib.rs`）↔ `packages/tui/src` + `components` |
| `app.*` 接线 | **43 / 44 = 97.7%**（silent 1 = `app.tree.editLabel`） | 同 | `python pi-rust/scripts/app_action_coverage.py --check-consumed` → `43 entries; measured wired: 43 / in sync` |
| **composer 鼠标面** | **3 / 3 = 100%** | 2 / 2 = 100% | 点击定位光标 + 下拉框点选 + **下拉框滚轮（本轮）**；口径从"点选"扩到"指针全部" |
| 指针映射面 | 3 / 3 = 100% | 3/3 | 本轮无回退 |
| slash 内置命令 | 18 / 23 = 78.3%（字面 17/23，`exit`≡`quit`） | 同 | 本轮未动 |
| 扩展生命周期事件 | **21/36 声明 = 58.3%；20/36 生产构造点 = 55.6%** | 同 | `python pi-rust/scripts/extension_event_coverage.py`（15 个缺失变体逐条列出） |

**加权完成度**（权重表见 `RUST_TS_PARITY_METRICS.md` §4.1，公式公开）：

```text
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.905 + 8×0.90 + 7×0.783 + 7×0.70
  + 8×0.95 + 7×0.556 + 9×0.85 + 5×0.497 + 3×0.95 = 83.6%
```

只有测试轴从 0.495 走到 0.497（+0.01pt），总分仍落在 **83.6%**。
「composer 鼠标面」与 LUM-1431 一样**不单独进公式**（公式里的 TUI 轴是模块率与接线率的均值），
它的价值是"轴 6 视觉/交互保真的前提"，因此本轮只更新可复算的分项、不重估任何轴的取值。

## 7. 剩余缺口（按性价比，供下一轮起手）

| 顺位 | 项 | 范围 | 预计影响 | 证据 |
|---|---|---|---|---|
| 1 | **扩展生命周期事件补 15 个** | `pi-protocol` + `pi-agent-core` + `pi-extensions` | 兼容 pi 插件生态的头号阻塞；单轴 +3.1pt | `extension_event_coverage.py` 输出 20/36，缺失名单见 §6；**已建 issue LUM-1432（backlog / high）并本轮派发** |
| 2 | **`#` 技能/工具引用面** | `pi-coding-agent` 的 autocomplete provider wrapper | 让 `#` 真的出候选（上游也是由 provider 注入，`editor.ts:251` 只是触发符） | §1.2；`packages/tui/src/autocomplete.ts` 无内置 `#` 分支 |
| 3 | **CLI flag 面** | `pi-coding-agent/src/cli` | 与上游启动参数对齐 | LUM-1426 §8 第 3 条，本轮复核仍缺 |
| 4 | **modal 打开时滚轮不应落到视口** | `pi-tui/src/app.rs` | selector/dialog 打开时滚轮 `Idle` 与上游 `shouldDeferViewportInputToOverlay` 仍有差（不滚，但也不给 overlay） | 上游 `tui-alt-screen.ts:691`；本轮**未动**（属 overlay 面） |

两条已知遗留（记录备查，本轮未动）：`message.rs:63` 的 `tool_fold_hint` 仍硬编码 `Ctrl+O`
（用户改键后提示不跟着变，LUM-1222 §5 已记）；`app.editor.external` 仍缺（Stage 59）。

## 8. 「最多 3 个任务」的处置 = 1 件派发 + 1 件自做

issue 的「最多开启 3 个任务同时运行」是**上限**。本轮：

* **自做（1 件）**：下拉框滚轮 —— 它是 LUM-1431 §7 的第一顺位，且完全落在 `pi-tui` 内，
  与其它线不共享文件；
* **派发（1 件）**：**LUM-1432**（`pi-rust 扩展生命周期事件补齐 20/36 → 36/36`，已在 backlog、high，
  验收标准写全）指派给 **编程助手-devbox1**（`22e8b20d-84ea-43e9-b535-2f76e4aee397`，此前领过
  LUM-1330 工具钩子与 LUM-1244 扩展事件声明，是该轴的既定写者）。它与本轮**文件面零重叠**
  （`pi-protocol`/`pi-agent-core`/`pi-extensions` vs `pi-tui`），issue 正文本身就写了
  「不要与并行的 pi-tui 改动并行改同一批文件」——即它是为并发设计的。
* **不派第三条**：§7 第 3、4 条要么范围含糊、要么与派发线共享 `pi-coding-agent`，再派只会重演
  LUM-1422 那种「同一缺陷两条并发线各修一次」（LUM-1431 §3 亲手清过一次）。

## 9. 范围之外

- **未碰** `pi-coding-agent` / `pi-ai` / `pi-agent-core` / `pi-session` / `pi-server` / `pi-client` /
  `pi-extensions`（LUM-1432 那条线本轮由另一个 agent 负责）。
- **未碰** 扩展事件轴、slash 命令表、CLI flag 面、键位表（`app.*` 接线率不变）、CI / Docker。
- **未碰** 上游 TS 代码（`packages/**`）——只读来做对齐取证；Martty 只读
  （`workdir/Martty`，来自 LUM-1415 的 checkout）。
