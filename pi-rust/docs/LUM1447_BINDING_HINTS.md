# LUM-1447 — 提示面与键位一致性：两个死键位 + 转录折叠提示按生效键位渲染

> 基线：`feature/pi.rs` = `696ab4fd0`（= LUM-1445 合并 tip，本轮从它起）
> 环境：Windows 10 x86_64 / cargo 1.97.1 / `--offline`
> 本轮只改 `pi-tui`（+ 一个新脚本），不碰 `pi-protocol` / `pi-ai` / `pi-agent-core` /
> `pi-coding-agent` / `pi-extensions` 的源码。

## 1. 为什么是这块：一次可反证的「死键位」扫描

LUM-1445 §8 把剩余缺口排到第 4、5 条（modal 拖拽/悬停、settings 滚轮按矩形认领），
都属「已实现功能的指针细节」。本轮没有接着做第 4 条，而是先花一次扫描把**同一个缺陷类**
量清楚：**定义了、被 `/hotkeys` 广告了、但代码里没有任何消费点的键位**。

方法（脚本随本轮入库，可复跑，可反证）：

```bash
python pi-rust/scripts/keybinding_coverage.py pi-rust          # 报告
python pi-rust/scripts/keybinding_coverage.py pi-rust --check  # 门禁：exit 1 当未知死键位出现
```

结果（本 tip）：

| 表 | 消费 | 死键位 |
|---|---|---|
| `tui.*`（`pi-tui/src/keybindings.rs`，49 个 id） | **49 / 49 = 100%** | 本轮前 **47/49**：`tui.editor.historyPrevious`、`tui.editor.historyNext` |
| `app.*`（`pi-coding-agent/src/keybindings.rs`，44 个 id） | 43 / 44 = 97.7% | `app.tree.editLabel`（缺组件，见 §7） |

反向验证（写进脚本的 `--check`）：把 `editor.rs` 本轮改动 stash 掉再跑，脚本立刻报

```text
tui.*: 47/49 consumed
  UNCONSUMED tui.editor.historyNext  (crates\pi-tui\src\keybindings.rs)
  UNCONSUMED tui.editor.historyPrevious  (crates\pi-tui\src\keybindings.rs)
NEW DEAD ID(S): [...]          # exit 1
```

即：这不是「顺手加了个功能」，是**一条能被失败复现的缺陷**被关掉。

## 2. 缺陷一：`tui.editor.historyPrevious` / `historyNext` 是死键位

上游把这两个 id 当**专用历史键**处理，与光标移动完全分开
（`packages/tui/src/components/editor.ts:848-856`）：

```ts
// Dedicated history actions always browse entries instead of moving the cursor.
if (kb.matches(data, "tui.editor.historyPrevious")) {
    this.cancelAutocomplete();
    this.navigateHistory(-1);
    return;
}
if (kb.matches(data, "tui.editor.historyNext")) {
    this.cancelAutocomplete();
    this.navigateHistory(1);
    return;
}
```

Rust 侧两个 id 都有定义（默认不绑定）、`Up` / `Down` 的「首行/末行转历史」也有
（LUM-1312），**但这两个 id 从来没有被匹配过**：`editor.rs` 里只有
`cursorUp` / `cursorDown` 分支。用户在 `keybindings.json` 里写
`{"historyPrevious":["ctrl+p"]}` 得到的是一个**什么都不做的键**（codex 的
`Ctrl+P` 历史正是常见写法）。

本轮实现（`editor.rs:2377-2391`，位置与上游一致：kill-ring 之后、光标移动之前）：

```rust
if Self::matches_binding(kb, &key, "tui.editor.historyPrevious") {
    self.cancel_autocomplete();
    return self.history_prev();
}
if Self::matches_binding(kb, &key, "tui.editor.historyNext") {
    self.cancel_autocomplete();
    return self.history_next();
}
```

两处细节按上游对齐：

* `cancel_autocomplete()` **无条件**调用。`history_prev()` 在历史为空时提前返回，
  此时若不先取消，下拉框会留着把下一个 `Enter` 吃掉（上游同样是先取消后导航）。
* 分支在光标移动**之前**：把 `historyPrevious` 绑到 `up` 的用户拿到的是历史浏览，
  不是行内移动。

可测性改造：`handle_key` 原来直接读进程级注册表（`get_keybindings()`），测试无法注入
override 而不污染同进程的其它测试。现在拆成
`handle_key()`（读全局）+ `handle_key_with(&KeybindingsManager, key)`（可注入），
后者对测试可见（`editor.rs:2239-2252`）。

## 3. 缺陷二：转录里的折叠提示硬编码 `Ctrl+O`

`message.rs:70` 的 `tool_fold_hint` 生成每个折叠工具块下面那行
`… (+M lines, Ctrl+O to expand)`——**TUI 里出现频率最高的提示**。它是写死的字符串，
所以用户改键后：header 广告新键（LUM-1242 起 header 按生效键位渲染）、`/hotkeys` 广告新键、
**唯独转录里那行还在说 `Ctrl+O`**。上游走 `keyHint`
（`components/bash-execution.ts:180-184`、`keybinding-hints.ts` 的
`keyHint = keyText + description`），LUM-1222 §5 与本轮之前的 §8 都记过这条。

本轮实现（`keybindings.rs:796-865` 三个公开辅助 + `message.rs:70` 调用）：

| API | 语义 | 上游对应 |
|---|---|---|
| `key_text(id) -> Option<String>` | 生效键位，格式化后以 `/` 连接；未绑定/未知 → `None` | `keyText` |
| `key_text_or(id, fallback) -> String` | **表里根本没有这个 id** → `fallback`；**表里有但未绑定** → `""` | —— |
| `key_hint_or(id, fallback, desc) -> String` | 键位 + 描述；未绑定 → 只有描述 | `keyHint` |

`key_text_or` 的两分支是刻意的，理由在代码注释里：

1. `app.*` 的 id 定义在 `pi-coding-agent` 的表里，**裸 `pi-tui` 注册表查不到**
   `app.tools.expand`。这时用**出货默认值** `Ctrl+O` 兜底——与 `Editor::matches_app_exit`
   对 `app.exit` 用硬编码 `ctrl+d` 兜底是同一条既有规则，不是新发明的例外。
2. 表里**有**这个 id 而 `get_keys` 为空 = 用户明确解绑。此时**必须**把键位去掉，
   否则提示就是在广告一个按了没反应的键（header 对未绑定行的既有规则）。

效果（`message.rs:70-80`）：

| `app.tools.expand` | 转录那一行 |
|---|---|
| `ctrl+o`（默认） | `* … (+6 lines, Ctrl+O to expand)` |
| `ctrl+u`（override） | `* … (+6 lines, Ctrl+U to expand)` |
| `ctrl+u` + `alt+o` | `* … (+6 lines, Ctrl+U/Alt+O to expand)` |
| 解绑 | `* … (+6 lines, to expand)`（去掉死键位，保留 affordance） |
| 裸 `pi-tui`（表里无此 id） | `* … (+6 lines, Ctrl+O to expand)`（出货默认兜底） |

格式化复用既有 `locale::format_chord`（与 `/hotkeys`、header 同一套拼写），
不新增第二套 `ctrl+o → Ctrl+O` 映射。

## 4. 改动清单

| 位置 | 内容 |
|---|---|
| `pi-rust/crates/pi-tui/src/editor.rs:2239,2249` | `handle_key` 拆出可注入 keybindings 的 `handle_key_with` |
| `pi-rust/crates/pi-tui/src/editor.rs:2377-2391` | `tui.editor.historyPrevious` / `historyNext` 分支（先取消补全再导航） |
| `pi-rust/crates/pi-tui/src/editor.rs:74-77` | 模块文档：「无消费者的键位」段改写为「已接线的专用历史键」 |
| `pi-rust/crates/pi-tui/src/editor.rs:2755-2830` | 3 条新单测：默认未绑定 / override 后浏览而非移动光标 / 空历史时也关下拉框 |
| `pi-rust/crates/pi-tui/src/keybindings.rs:796-865` | `key_text` / `key_text_or` / `key_hint_or` + `format_keys` / `resolved_keys` |
| `pi-rust/crates/pi-tui/src/message.rs:64-80` | `tool_fold_hint` 走 `key_hint_or("app.tools.expand", "Ctrl+O", "to expand")` |
| `pi-rust/crates/pi-tui/tests/hint_bindings.rs`（新） | 1 条集成用例，7 次断言覆盖上表五种状态 + 真 `MessageView::render_lines` 行 |
| `pi-rust/crates/pi-tui/tests/lum1447_binding_hints_frames.rs`（新） | 3 帧：默认 / override / 解绑（进程级注册表用 `Mutex` 串行） |
| `pi-rust/scripts/keybinding_coverage.py`（新） | 两张表的死键位扫描 + `--check` 反测试（`KNOWN_UNCONSUMED` 双向对账） |
| `pi-rust/docs/screenshots/lum1447-fold-hint-*.{png,txt}`（新，6 个） | §5 的三帧及其 cell grid dump |

## 5. 帧截图

`frame-buffer` 通道（本机无 PTY，见 `docs/LUM1426_POINTER_COLUMNS.md` §9）：
真 `App::render_to_buffer` 的 80×20 cell grid → `scripts/frame_to_png.py`。

| 文件 | 内容 |
|---|---|
| `docs/screenshots/lum1447-fold-hint-default_ctrl_o.png` | 默认表：`* … (+6 lines, Ctrl+O to expand)` |
| `docs/screenshots/lum1447-fold-hint-rebound_ctrl_u.png` | override 成 `ctrl+u`：同一行变成 `Ctrl+U to expand` |
| `docs/screenshots/lum1447-fold-hint-unbound.png` | 解绑：`* … (+6 lines, to expand)`，全帧无 `Ctrl+` |

诚实说明：帧证明**画出来的字**，不证明按键时序；按键语义由 §4 的单测与集成用例证明
（`handle_key_with` 真的被喂 `ctrl+p` / `ctrl+n`）。三帧的 `.txt` 是原始格网，
可直接 grep 断言，弥补 PNG 不可搜索。

## 6. 验证（本机 Windows / cargo 1.97.1 / `--offline`）

| 门禁 | 命令 | 结果 |
|---|---|---|
| pi-tui 全量 | `cargo test -p pi-tui` | **1027 passed / 0 failed**（基线 1020，+7 = 3 单测 + 1 集成 + 3 帧） |
| 格式 | `cargo fmt --all -- --check` | 干净（exit 0） |
| lint | `cargo clippy -p pi-tui --all-targets` | 0 告警 |
| 消费方回归 | `cargo test -p pi-coding-agent --test tools_render --test startup_header --test help_text_layout --test keybindings --test keybinding_install --test reload_config --test chatinput_chord_conflicts` | `tools_render` 3/0、`help_text_layout` 3/0、`keybindings` 20/0、`keybinding_install` 1/0、`chatinput_chord_conflicts` 3/0、`startup_header` 3/0；`reload_config` **1 failed**（见下） |
| 死键位门禁 | `python pi-rust/scripts/keybinding_coverage.py pi-rust --check` | exit 0：`tui.* 49/49`、`app.* 43/44`、`in sync: 1 known-unconsumed` |
| 反证 1 | stash 掉 `editor.rs` 再跑上面的脚本 | exit 1，精确报出两个 `UNCONSUMED tui.editor.*`（§1） |
| 反证 2 | `editor.rs` 单测里把两个分支删掉 | `history_chords_browse_entries_instead_of_moving_the_cursor` 与 `history_chord_closes_the_autocomplete_dropdown` 立刻红 |

**`reload_config` 的那一条失败是既有的、与本轮无关**：
`reload_rereads_keybindings_and_ui_settings_mid_session` 断言转录里含
`keybindings.json`，而本机临时目录路径很长，路径在渲染时被折行成
`keybin\ndings.json`，断言才失败。证据：`git stash push -- pi-rust/crates/pi-tui` 后
在**未改动的基线**上跑同一条用例，同一行（`reload_config.rs:129`）同样失败。
与本轮改动无关（本轮没有碰折行 `/reload` 或路径渲染）。

## 7. 剩余缺口（供下一轮起手）

| 顺位 | 项 | 范围 | 说明 |
|---|---|---|---|
| 1 | 扩展注入 autocomplete provider（`ctx.ui.addAutocompleteProvider`，`#` 触发符面） | `pi-extensions` + `pi-coding-agent` + `pi-tui` | 上游 `docs/extensions.md:2698`；Rust 侧 `pi-extensions` **零** autocomplete 引用，且 `pi-tui` 的 `AutocompleteProvider` 是**同步** trait（模块文档已声明），要支持扩展回调得先把编辑器补全路径改成可等待。这是「插件生态 + chatinput」两条线的交叉点，**本轮已建 issue 派发**（§8） |
| 2 | `/help` 的 `keys:` 段仍硬编码 chord | `pi-coding-agent/src/commands/slash.rs:196-210` | 与 `tool_fold_hint` 同一缺陷类：`Ctrl+J/A/E/K/R/C/D/L` 等是死字符串。要做成动态两列排版（改键后列宽变），会牵动 3 条既有断言（其中一条按精确空格断言 `Ctrl+L      open the model selector`） |
| 3 | `dialog.rs:284,294,299` 与 `app.rs:1890` 的页脚提示 | `pi-tui` | `[Enter] submit [Esc] cancel` 等；上游走 `keyHint("tui.select.confirm"/"cancel")`。注意 `settings.rs:478` 的 `Enter/Space to change` **上游自己也是硬编码**（`settings-list.ts:321`），不要「顺手对齐」成上游没有的东西 |
| 4 | `app.tree.editLabel`（唯一 silent `app.*`） | `pi-coding-agent` + `pi-tui` | 需要 tree 改名 UI（组件不存在），不是补键位；补齐后 `app.*` 接线 44/44 = 100% |
| 5 | modal 内拖拽/悬停、`settings` 滚轮按矩形认领 | `pi-tui` | 沿用 LUM-1445 §8 第 4、5 条（本轮未动） |

## 8. 「最多 3 个任务」的处置 = 1 自做 + 1 派发 + 0 收编

* **自做（1 件）**：本轮切片全在 `pi-tui`，与在飞/待合并的 LUM-1434（CLI flag 面，
  `pi-coding-agent/src/cli/`）、LUM-1432（扩展事件，`pi-protocol`/`pi-agent-core`/
  `pi-extensions`，已并入 `feature/pi.rs` 并 in_review）**文件面零重叠**。
* **派发（1 件）**：§7 第 1 条（扩展 autocomplete provider / `#` 触发符面）单独立 issue，
  指派 `编程助手-devbox1`（`22e8b20d`，LUM-1432 的作者，同一 `pi-extensions` 面）。
  验收标准与设计约束写在 issue 里（含「同步 trait → 可等待」这条必须先决策的架构前提）。
* **不收编**：本轮开工时 `feature/pi.rs` 上**没有**漂在 `work/*` 的已完成交付
  （LUM-1432 已由 LUM-1445 并入，LUM-1434 仍在跑），所以没有第三条可收编。
* **并发数**：自做 1 + 派发 1 + 在办 1（LUM-1434）= 3 条，正好在上限内。

## 9. 范围之外

未碰 `pi-protocol` / `pi-ai` / `pi-agent-core` / `pi-coding-agent` / `pi-session` /
`pi-server` / `pi-client` / `pi-extensions` 的源码；未碰 slash 命令表、`app.*` 接线率、
扩展事件轴、CI / Docker；未碰上游 TS（`packages/**`，只读取证）。
`/help` 与 dialog/settings 页脚的同类问题**明确留给下一轮**（§7 第 2、3 条），
本轮不把它们算成「已修」。
