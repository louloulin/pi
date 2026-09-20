# TUI 快捷键接线审计（LUM-1245）

> 基准：`origin/feature/pi.rs` = `6f45602ec`（LUM-1230 合并后）
> 环境：Linux x86_64 / 32 核 / cargo+rustc 1.98.1 / 离线构建 `--offline`
> 前序：`docs/TUI_INTERACTION_PARITY.md`（LUM-1240 审计，tip `af3aa30a4`）
> 本文只报告**实测**数字；每条结论都给出可复现命令与运行时证据（真 PTY 截图）。

## 0. 结论速览

LUM-1240 在 `af3aa30a4` 上量到 **4 个"广告了但不工作"的快捷键**。本轮修掉 2 个，并把这类回归**变成编译期之外的可测不变量**，让剩下的 2 个不再被广告。

| 指标 | 修前 | 修后 | 说明 |
|---|---|---|---|
| `app.*` 已接线动作 | 18 / 44 = **40.9%** | 19 / 44 = **43.2%** | `app.model.select` 接线（§2） |
| 启动头部广告行 | 20 / 20 行 | **18 / 20 行** | 2 行 bound-but-unwired 被正确隐藏（§3） |
| 头部"撒谎/死键"行 | 4 | **2** | `app.suspend`、`app.editor.external` 仍缺失，但不再宣传 |
| `/hotkeys` app 组死键 | 有 | **无** | 同一份 `CONSUMED_APP_ACTIONS` 过滤（§3） |

一句话结论：**头部和 `/hotkeys` 此前把"有默认键位"当成"已实现"，因此 `Ctrl+L` 一边被宣传成"选择模型"、一边真的去清屏（语义反转），另有 3 个键位完全无消费者。** 本轮补上了缺失的"是否实现"这一维：新增共享注册表 `CONSUMED_APP_ACTIONS`，两个广告位都以它为准；`Ctrl+L` 改由 driver 接管并打开模型选择器；清屏能力保留为公开的 `App::clear_transcript()`（`/clear` 路径）。

## 1. 根因：键位表缺一维

`KeybindingDefinition` 只描述*默认键位*，没有"是否有消费者"这一维。两个广告位各自把"能解析出 chord"读成"可用"：

- 启动头部：`pi-tui/src/app.rs::builtin_header_lines` 遍历 `locale.rs::STARTUP_HINTS`，只跳过 `kb.get_keys(id)` 为空的项。
- `/hotkeys`：`pi-coding-agent/src/commands/slash.rs::hotkeys_text_with`，同一套 `chords.is_empty()` 判断。

于是只要某 id 在默认表里有键位，无论有没有人响应，都会被宣传。`Ctrl+L` 更糟：头部显示"Ctrl+L to select model"，而 `App::step_key` 里硬编码 `Ctrl+L` 去 `messages.clear()`（清屏）——**宣传与实际语义相反**。

repro（修前）：

```bash
# 头部宣称 Ctrl+L 选模型
grep -n 'app.model.select' pi-rust/crates/pi-tui/src/locale.rs
# 但 App 里 Ctrl+L 清屏
grep -n "Char('l')" pi-rust/crates/pi-tui/src/app.rs
```

## 2. 修复 `app.model.select`（Ctrl+L）

上游 `packages/coding-agent/src/core/keybindings.ts:116` 把 `ctrl+l` 映射到 `app.model.select`，语义是"Open model selector"。选择器属于 coding-agent driver，不属于 `pi-tui`。

- 从 `SlashCommand::Model` 分支抽出 `open_model_selector(app, options)`（`crates/pi-coding-agent/src/interactive.rs`），`/model` 与快捷键共用同一实现，避免两份拷贝。
- driver 在 App 之前抢占 `app.model.select`（用与其他 `app.*` 相同的 `matches_with_fallback` + 默认 `ctrl+l`），随后 `return Ok(None)`。
- 删除 `App::step_key` 里硬编码的 `Ctrl+L` 清屏；清屏提为公开 API `App::clear_transcript()`，只由 `/clear` 调用。
- `app.rs` 模块文档同步：删掉"deliberate deviation"段落，改成"Ctrl+L 由 driver 接管"。

## 3. 新增"是否接线"注册表

`pi-tui/src/keybindings.rs`：

```rust
pub const CONSUMED_APP_ACTIONS: &[&str] = &[ /* 19 个 app.* id */ ];
pub fn app_action_is_consumed(id: &str) -> bool {
    if !id.starts_with("app.") { return true; }   // tui.* 由组件自己消费
    CONSUMED_APP_ACTIONS.contains(&id)
}
```

两个广告位各自多一层过滤：头部用 `HeaderHint::is_wired()`（`locale.rs`），`/hotkeys` 直接查 `app_action_is_consumed`。效果是把"死键不再出现在任何广告位"变成**表驱动**的——将来接线上 `app.suspend`，只要把它加进常量，两处广告自动恢复，无需再改渲染逻辑。

## 4. 实测数字与可复现命令

```bash
cd pi && git rev-parse --short HEAD                      # 6f45602ec
# 规模
find pi-rust/crates -path '*/src/*' -name '*.rs' ! -name 'mod.rs' | xargs wc -l | tail -1   # 123,426
find packages -path '*/src/*' -name '*.ts' | xargs wc -l | tail -1                          # 153,106
# 测试
grep -rho '#\[test\]\|#\[tokio::test\]' pi-rust/crates --include=*.rs | wc -l               # 2,252
find packages -name '*.test.ts' | xargs grep -oh '\bit(\|\btest(' | wc -l                   # 5,309
# 接线率
sed -n '/pub const APP_KEYBINDING_IDS/,/];/p' pi-rust/crates/pi-coding-agent/src/keybindings.rs | grep -c '^\s*"app\.'   # 44
sed -n '/pub const CONSUMED_APP_ACTIONS/,/];/p' pi-rust/crates/pi-tui/src/keybindings.rs | grep -c '^\s*"app\.'         # 19
```

| 口径 | 数值 |
|---|---|
| 纯代码规模 | 123,426 / 153,106 = **80.6%** |
| 测试用例数 | 2,252 / 5,309 = **42.4%** |
| `app.*` 接线率 | 19 / 44 = **43.2%**（修前 40.9%） |
| 头部广告真实性 | 18 / 20 行（2 行正确隐藏） |
| 头部死键 | 4 → **2** |

## 5. 运行时证据（真 PTY）

`pi-rust/scripts/pty_capture.py`（真 PTY + `TIOCSWINSZ` + `pyte` 终端仿真 + PIL），场景 `scripts/pty_scenarios/lum1245-model-select.json`：

- `docs/screenshots/lum1245-ctrl-l-model-selector.png` + 同名 `.txt` 字符网格 dump

5 帧覆盖：

1. 启动帧——头部**仍**广告 `Ctrl+L to select model`，且**已无** `to suspend` / `for external editor`；
2. `Ctrl+L` → 弹出 `Pick a model` 选择器（修前是静默清屏）；
3. `Down` → 选择在模型列表内移动（`Ling 2.6 1T` → `Ling 2.6 Flash`）；
4. `Esc` → 选择器关闭，transcript 仍在；
5. `/hotkeys` → app 组含 `Ctrl+L open the model selector`，**不含** suspend / external editor。

字符网格可 grep 断言：

```bash
grep -c 'to suspend'            docs/screenshots/lum1245-ctrl-l-model-selector.png.txt   # 0
grep -c 'for external editor'   docs/screenshots/lum1245-ctrl-l-model-selector.png.txt   # 0
grep -c 'Pick a model'          docs/screenshots/lum1245-ctrl-l-model-selector.png.txt   # 2（第 2、3 帧）
grep -c 'Ctrl+L open the model selector' docs/screenshots/lum1245-ctrl-l-model-selector.png.txt  # 1
```

## 6. 回归防线

| 测试 | 位置 | 断言 |
|---|---|---|
| `the_header_resolves_the_live_chords` | `pi-tui/tests/startup_header.rs` | 头部**不含** `to suspend` / `for external editor` |
| `clear_transcript_drops_the_log_and_repins` | `pi-tui/tests/app_scroll.rs` | `clear_transcript()` 清空并重新贴底；空 log 时 PgUp 是 `Idle` |
| `clear_keeps_the_markdown_switch` | `pi-tui/tests/app_markdown.rs` | 清屏不重置 markdown 开关 |
| `clear_keeps_the_thinking_switch` | `pi-tui/tests/thinking.rs` | 清屏不重置 thinking 开关 |
| `hotkeys_text_skips_bound_but_unimplemented_actions` | `pi-coding-agent/src/commands/slash.rs` | `app.suspend` 仍 bound 但**不**广告；`app.model.select` 广告；遍历 `APP_KEYBINDING_IDS` 断言未接线 id 一个都不出现 |

最后一条是 LUM-1240 点名的"今天会红"的那条不变量，现随补丁同批落地。

## 7. 剩余差距（诚实条目）

1. **`app.suspend`（Ctrl+Z）未实现**——需要 `SIGTSTP`/`SIGCONT` 与终端 handoff，在 tokio 循环里要慎重处理；已从所有广告位隐藏，未计入已接线。
2. **`app.editor.external`（Ctrl+E）未实现**——需要"写临时文件 → 退出 alt-screen/raw mode → 起 `$EDITOR` → 恢复终端并强制重绘 → 读回 buffer"的终端交接；本轮未做，已隐藏。
3. `app.tree.*` / `app.models.*` 家族（约 20 个 id）可能由选择器组件内部消费，全局表查不到；**未复核前不记为已接线**，也未计入差距分。
4. 键位表仍是"声明式"，`CONSUMED_APP_ACTIONS` 是手工维护的镜像。更强的做法是让每个 id 的消费者注册自己（运行期反射），但那会改动 `KeybindingDefinition` 的公共形状；本轮选择低风险的表驱动方案。

## 8. 后续建议

- 用 3 个子任务分别承接：`app.editor.external`（Ctrl+E）、`app.suspend`（Ctrl+Z）、以及 `app.tree.*`/`app.models.*` 的消费面复核。三者都需要终端交接或组件内部复核，做之前先确认各自的验收方式。
- 若采纳运行期注册，`CONSUMED_APP_ACTIONS` 可退化为派生量，本文 §3 的两处过滤逻辑无需改动。
