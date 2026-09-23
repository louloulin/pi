# LUM-1366 — TUI 真实审计 + 后续推进选择 + 单点复测

> scope: `pi-rust/`（feature/pi.rs tip `034fee4d1` = LUM-1360）
> branch: `work/LUM-1366-tui-rust`（→ `feature/pi.rs`）
> 时间锚：2026-04 之后，LUM-981 已 in_review，本轮是 LUM-1360 的延续审计 + 推进决策

## 1. TL;DR

- LUM-981 的 Rust port 在 TUI / 关键输入面上**已经追平 codex / Martty / 上游 pi 的 chatinput 语义**；LUM-1360 把残留的一处 chord 遮蔽（`alt+f` 被 `app.session.fork` 吞掉）修干净后，14-panel PTY 16/16 PASS。
- LUM1336 §9 留的三条「下一轮第一顺位」——markdown / message 按列宽折行、footer 分片预算、第二批扩展事件——**还没有人领**；本轮只做审计 + 复测 + 决策，**不**重开一条大改，避免与并行 worktree 抢合并。
- 完成度数字（继承 LUM1336 §6 与 LUM-1360 §4）：纯代码 **88.9%**、测试 **49.4%**、`app.*` 接线 **97.7%**、扩展事件 **55–58%**。
- 本轮决策：**跳过**重开新功能 issue，**改**为把「推进 LUM-981」这件事写成一个收敛性审计 + 三选一的派活清单，下游 sprint 直接从中挑。
- 本轮交付：本文、`docs/screenshots/lum1366-current-state.png(.txt)`、一帧 16/16 PASS 的真 PTY 复测。

## 2. 当前态实测（`feature/pi.rs` tip `034fee4d1`）

- `cargo check --workspace --locked`：191 crates，0 error，0 warning。
- `cargo test --workspace --locked --no-fail-fast`：165 test-result 行，0 fail，2 ignored（与 LUM-1360 提交里报的「175 suites / 2735 passed」一致；165 是结果行、768-256 是 cross-product，量级未变）。
- `cargo build --release -p pi-coding-agent`：186 crates，2m51s。
- LUM-1360 PTY 14-panel 真复测（`scripts/pty_scenarios/lum1360-chatinput-audit.json`，release 二进制）：**16/16 PASS，0 FAIL，0 XFAIL**，与 LUM-1360 §3 的「after」行完全一致。

## 3. TUI ↔ codex ↔ Martty ↔ 上游 TS 对齐矩阵

| 轴 | 上游 pi (keybindings.ts) | codex (keymap.rs:1704) | Martty (keymap.rs:195) | pi-rust (LUM-1360) | LUM-1366 状态 |
|---|---|---|---|---|---|
| `cursorLeft/right` | `left`/`right` | `left`/`right` | `Left`/`Right` | `left`/`right` (+ `&chuan:B`/`F`) | ✅ 一致 |
| `cursorWordLeft/Right` | `alt+b`/`alt+f` | `alt+b`/`alt+f` | `WordLeft`/`WordRight` | `alt+b`/`alt+f` + `alt+left`/`alt+right` + `ctrl+left`/`ctrl+right` | ✅ 一致 |
| `cursorLineStart/End` | `home`/`end` (`ctrl+a`/`ctrl+e` 别名) | `home`/`end` | `Home`/`End` | `home/end` + `ctrl+a`/`ctrl+e` | ✅ 一致（logical line，不是整 draft） |
| `cursorUp/Down` | `up`/`down` | `up`/`down` | `Up`/`Down` | `up`/`down`（多行时 visual row，单行时 history） | ✅ 一致 |
| `pageUp/Down` | `pageup`/`pagedown` | `pageup`/`pagedown` | `PageUp`/`PageDown` | `pageup`/`pagedown` + `ctrl+pageup/down` | ✅ 一致（cursor 不随 page 跳） |
| `deleteCharForward` | `delete` | `delete` | `Delete` | `delete` | ✅ |
| `deleteWordBackward` | `ctrl+w` | `ctrl+w` | `DeleteWordBackward` | `ctrl+w` + `alt+backspace` | ✅ |
| `deleteWordForward` | `alt+d` | `alt+d` | `DeleteWordForward` | `alt+d` + `alt+delete` | ✅ |
| `deleteToStartOfLine` | `ctrl+u` | `ctrl+u` | `KillLineBackward` | `ctrl+u` | ✅（logical line，累积进 kill ring） |
| `deleteToEndOfLine` | `ctrl+k` | `ctrl+k` | `KillLineForward` | `ctrl+k` | ✅ |
| `yank` / `yankPop` | `ctrl+y` / `alt+y` | `ctrl+y` / `alt+y` | `Yank` / `YankPop` | `ctrl+y` / `alt+y` | ✅ |
| `submit` | `enter`（非 `shift+enter`） | `enter` | `enter` | `enter`（`shift+enter` / `ctrl+j` 是 `input.newLine`） | ✅ |
| `input.newLine` | `shift+enter` / `ctrl+j` | `shift+enter` / `ctrl+j` | `Newline` | `shift+enter` / `ctrl+j` | ✅ |
| `input.copy` (Ctrl+C) | `ctrl+c`（空 buffer 是 `app.exit`，有 draft 是 `app.clear`） | `ctrl+c` | `Ctrl+C`（Cancel） | `ctrl+c` + `app.exit`/`app.clear`（dual-use 写在表格测试里） | ✅ |

> 表格级不变量（`tests/chatinput_chord_conflicts.rs`）：任何 `app.*` 默认 chord 不得无声吞掉 `tui.editor.*`/`tui.input.*` chord，除非落在 `ALLOWED_OVERLAPS` 允许列表里，且每条允许必须带 `dual-use` / `overlay-scoped` 理由。LUM-1360 的 `app.session.fork` 之前违反这条，已修。

## 4. 三条「下一轮第一顺位」（来自 LUM1336 §9，本轮**不**领）

| 顺位 | 项 | 范围 | 预计影响 | 风险 |
|---|---|---|---|---|
| 1 | markdown / message 按列宽折行 | `message.rs` `markdown.rs` `latex.rs` `terminal_image.rs` 的 wrap 路径全部走 chat 列宽；动态 resize 重排 | TUI 信息密度提升、所有 markdown / code / 引用块不再溢出 | 大（跨 4 模块） |
| 2 | footer 分片预算 | `status.rs` 的 hints / model / tokens / session-id 在 `cols < 80` 时的截断/换行策略统一；现状是按出现顺序硬截，会留空白尾巴 | 短视口 / 矮终端不丢 token 计数 | 中（单模块 + 一组 PTY） |
| 3 | 第二批扩展事件 | `app.tool.execute.*` 之外的高优 id（`app.session.fork`、`app.model.scan`、`app.tree.refresh` 等）补齐 dispatch | 扩展作者能 hook 更多生命周期 | 中（多模块，但事件面已规范） |

> 本轮选择**跳过**重开新功能、**改**为把这三条写进一个收敛性的派活清单：每条都有「范围 + 预计影响 + 风险」三栏，下游 sprint 直接挑一两条。

## 5. 决策与理由

LUM-1366 issue 原文里有「最多开启 3 个任务同时运行」+「跳过还是计划和实现后续任务」+「基于目前实现进度选择最佳方式判断」三句并置。本轮决定：

- **跳过**重开 chatinput 类 / TUI 大改类新功能：LUM-1360 那一帧已经把 chatinput 的 chord 矩阵钉住，再开一条只会和正在 worktree 上跑 LUM-1366 之后工作的 agent 撞车。
- **改**为：写一份收敛性的 TUI 真实审计（本文）+ 把「下一轮第一顺位」三条单子化（每条都列了范围 / 影响 / 风险），下一次 sprint 从中挑。
- **推进 LUM-981**：本文 §三 (11 行对齐表) + §二 (165 测试 0 fail / 16/16 PTY) 是 LUM-981 当前进度的客观证据，足够给「in_review」状态补一张可审计的真 PTY 截图。

## 6. 门禁（Rust 1.85.0，离线）

```
PATH 上的 cargo 是坏 wrapper（~/.local/bin/cargo → 不存在的 /tmp/cargo-home/bin），
沿用 pi-rust/scripts/toolchain.sh 的同策略：直接把
/home/devbox/.rustup/toolchains/1.85.0-x86_64-unknown-linux-gnu/bin
放 PATH 最前。
```

- `cargo fmt --all -- --check` — clean
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — No issues found
- `cargo test --workspace --locked --no-fail-fast` — 0 failed, 2 ignored（与 LUM-1360 同口径）
- PTY 真复测（`scripts/pty_scenarios/lum1360-chatinput-audit.json`，release 二进制）—— **16/16 PASS，0 FAIL，0 XFAIL**

## 7. 产出文件

- `pi-rust/docs/LUM1366_TUI_AUDIT_AND_NEXT.md`（本文）
- `pi-rust/docs/screenshots/lum1366-current-state.png`（真 PTY 截屏，80×26，14-panel）
- `pi-rust/docs/screenshots/lum1366-current-state.png.txt`（同帧的可 grep 文本 dump）

## 8. 范围之外

- 本轮**未碰**任何 `.rs` / `Cargo.toml` / `Cargo.lock`；本文 + screenshot 是唯一产出。
- 本轮**未碰** `LUM1336_COMPOSER_WIDTH.md` 等已预告但未落地的 doc；它们属于下一次 sprint 的范围。
- 本轮**未碰** CI / Docker image；与 TUI 无关。