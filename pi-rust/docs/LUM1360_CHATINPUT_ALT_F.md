# LUM-1360 — `Alt+F` 复活：chatinput 编辑 chord 与 codex / Martty / 上游 TS 对齐

> scope: `pi-rust/crates/pi-coding-agent/src/{keybindings.rs,interactive.rs}`,
> `pi-rust/crates/pi-coding-agent/tests/chatinput_chord_conflicts.rs`（新）,
> `pi-rust/scripts/pty_scenarios/lum1360-chatinput-audit.json`（新）,
> `pi-rust/docs/screenshots/lum1360-chatinput-audit{,-baseline}.png(.txt)`
> branch: `work/LUM-1360` → `feature/pi.rs`
> baseline: `origin/feature/pi.rs` = `a083f2ac1`

## 0. 一句话

本轮抓住并修掉最后一个仍然遮蔽 composer 编辑 chord 的 `app.*` 默认键：
`app.session.fork` 的 `alt+f` 默认键 shadow 了 `tui.editor.cursorWordRight`，
后果是 Alt+F——codex `move_word_right`、Martty `WordRight`、上游
`cursorWordRight` 都绑的 forward-word chord——在 pi-rust TUI 里实际上是 fork
session。已用真 PTY A/B 钉死，并加上表格级不变量测试防止复发。

## 1. 真实审计：缺口在哪、为什么

### 1.1 三个参考实现对 `Alt+F` 的一致绑定

| 参考 | 行 | 绑定 |
|---|---|---|
| `codex` `codex-rs/tui/src/keymap.rs:1704` | `move_word_right` | `Alt('f')`, `Alt(Right)`, `Ctrl(Right)` |
| `Martty` `src/input/keymap.rs:195` | `KeyCode::Char('f') if alt => WordRight` | `alt+f`（与 WordLeft 的 `alt+b` 配对） |
| 上游 pi-ts `packages/tui/src/keybindings.ts:95` | `tui.editor.cursorWordRight` | `["alt+right", "ctrl+right", "alt+f"]` |
| pi-rust `crates/pi-tui/src/keybindings.rs:250` | `tui.editor.cursorWordRight` | `["alt+right", "ctrl+right", "alt+f"]` ← 编辑器表完全一致 |

### 1.2 缺陷在哪一层

port 的 editor 表与三个参考一致，但 **coding-agent 的全局输入路径**
（`crates/pi-coding-agent/src/interactive.rs::handle_input_event`）在
`app.session.fork` 上挂了 `&["alt+f"]` fallback：

```rust
// before（origin/feature/pi.rs:1116）
matches_with_fallback(&keybindings, &event, "app.session.fork", &["alt+f"])
```

`matches_with_fallback` 的语义是「默认表里没有这个 id 时按 builtin 算」——
也就是说，即使 `app.session.fork` 在默认表里写成了 `["alt+f"]`，全局路径仍
会按 `&["alt+f"]` 命中；更糟的是 `handle_input_event` 在命中后直接
`return Ok(None)`，**composer 永远收不到这个键**。

Stage 65 加这四个 alt+* 默认键时的注释原话：

> The chords are free across all platform tables and unambiguous in the
> terminals pi targets.

这个前提对 `alt+f` 不成立：editor 表已经声明了它。

### 1.3 真 PTY 测得的现象

baseline（`origin/feature/pi.rs`）二词草稿 "alpha beta" 上按 `<M-f>`：

```
> /fork: no session database
```

而不是把光标推到 `beta` 末尾。**`alt+f` 在 pi-rust TUI 里执行的是 fork，
不是 forward-word。** 这就是 issue 描述里说的「chatinput 跟 codex 和 Martty
差距很大」在 chord 轴上的具体形态。

## 2. 修法

### 2.1 `app.session.fork` 还原到上游默认：unbound

```rust
// crates/pi-coding-agent/src/keybindings.rs:288
// 上游 leaves `app.session.fork` unbound (`defaultKeys: []`)。
// Stage 65 把它绑到 alt+f 是错的——遮蔽了 editor 的 forward-word chord。
// fork 仍然可达：`/fork` slash 命令、`interactive.rs` 里
// `open_fork_selector` 的 handler 没动、用户在 `keybindings.json` 自
// 己绑定。
entry("app.session.fork", Vec::<&str>::new(), "Fork current session"),
```

```rust
// crates/pi-coding-agent/src/interactive.rs:1116
// 没有 builtin fallback：让默认表说了算。handler 还在，
// `app_action_is_consumed("app.session.fork")` 仍为 true，
// `/fork` 仍打开 picker。
matches_with_fallback(&keybindings, &event, "app.session.fork", &[])
```

注释里同时把 Stage 65 的错误前提点明，并把测试文件指出来。

### 2.2 表格级不变量测试（防止整类 bug 复发）

`crates/pi-coding-agent/tests/chatinput_chord_conflicts.rs`——三个测试：

1. `no_app_default_shadows_an_editor_chord_unless_allowlisted`：遍历
   merged 默认表，任何 `app.*` 默认 chord 与 `tui.editor.*`/`tui.input.*`
   chord 重合必须落在显式 `ALLOWED_OVERLAPS` 允许列表里，每个允许条目
   必须带 `dual-use` / `overlay-scoped` 这类理由，证明两义由运行时条件
   区分。允许列表 15 项，每项对应一个已有的、合法的 chord 重合：
   `app.clear`/ctrl+c vs `tui.input.copy`（dual-use：有选区时 copy，
   否则 clear）、`app.exit`/ctrl+d vs `tui.editor.deleteCharForward`
   （dual-use：空草稿才 exit）、`app.tree.*`/`app.models.*`/
   `app.session.toggleSort`/`app.thinking.save`/...（overlay-scoped：
   只在对应 picker 打开时消费）。
2. `the_allowlist_names_ids_that_exist_and_actually_overlap`：stale
   allowlist 也会失守——验证允许条目里的 chord 真的还在 editor 表里
   出现，否则认为列表撒谎了；并对 macOS 分支做同样断言
   （`app.tree.*` 在两平台各 fork，mac 那边也要绿）。
3. `alt_f_reaches_the_composer_as_forward_word`：直接断言
   `app.session.fork` 默认无键、`manager.matches(Alt+F, tui.editor.cursorWordRight)`
   为 true、`matches_with_fallback(...fork..., &[])` 为 false；并把
   `Alt+B`/`Alt+F` 的「双向不能单边被遮」写死（避免下次有人又把
   `alt+b` 也送人）。

开发顺序是 TDD：先写测试，跑 red（修复前三个全失败，且每个失败信息都
点名了同一个 chord），再修，再跑 green。

### 2.3 把同一 chord 冲突写进既有 hotkeys / interactive 测试

修复同时让两个老测试也需要更新（它们断言的是被修掉的旧行为）：

- `commands::slash::tests::hotkeys_text_lists_the_session_branch_chords`
  之前断言 `/hotkeys` 里有 `Alt+F` 与 "fork a session from a message"；
  现在 fork 是 unbound，按既有渲染规则（empty chord 就 drop）确实不该出现。
  改成断言 tree + resume 仍在、`fork a session from a message` 不在、
  `Alt+F` **没有**出现在 `app:` 组的 shortcut cell，但**确实**出现在
  `navigation:` 组的 `Alt+Right / Ctrl+Right / Alt+F move by word (right)`
  行——同时是回归测试也是修复的正面证据。
- `interactive::tests::session_tree_fork_and_resume_keybindings_open_their_selectors`
  之前发 `alt+f` 然后断言 fork picker 打开。现在断言两件事：(a) `alt+f`
  **不**打开 picker（它是 editor 的 forward-word），(b) `app.session.fork`
  本身仍被消费——通过 `/fork` slash 命令仍能打开 picker。这样测试
  仍然把 wiring 的整条路径钉住，只是入口从默认 chord 换成了文档化的命令。

## 3. 真 PTY A/B

`scripts/pty_scenarios/lum1360-chatinput-audit.json`：80×26，14 panels
覆盖 chord-table 的关键动词——`<C-w>` delete word back、`<M-b>`/`<M-f>`
word hop、`<C-a>`/`<C-e>` line-scoped、`<C-k>`/`<C-y>` 杀环、
`<C-j>` newline、composer 内 double-click 选词、drag extend 选区、
`<C-c>` clear、`<Enter>` submit、`<Up>` history、`<C-r>` reverse search。

Panel 5 专门钉 LUM-1360：发 `<M-f>`，要求 `> alpha beta▍`（一个词向右）
**且** `reject: "/fork: no session database"`（fork picker 一定没跑）。
这是 hard assertion——harness 在 baseline 上 exit 1，在修复后 exit 0。

```
############ before  (origin/feature/pi.rs)
exit=1
assertions: 16 checks over 14 panels — 11 PASS, 1 FAIL, 4 XFAIL, 0 XPASS

############ after   (work/LUM-1360 + 本轮修复)
exit=0
assertions: 16 checks over 14 panels — 16 PASS, 0 FAIL, 0 XFAIL, 0 XPASS
```

baseline 的 panel 5 帧：

```
pi v0.1.0
hints hidden on a short terminal — Alt+H shows them
> /fork: no session database
```

修复后的 panel 5 帧：composer 干净，`> alpha beta▍`（光标在 beta 末尾），
没有 fork 那行。

截图：

- `docs/screenshots/lum1360-chatinput-audit-baseline.png(.txt)` — baseline
- `docs/screenshots/lum1360-chatinput-audit.png(.txt)` — 修复后

## 4. Rust ↔ TS 差距（本轮实测）

本轮只动 chord 表这一轴，不重算整 TUI 的跨轮进度轴（按
LUM1332 §6 与 LUM1336 §6 的指引，需要的读者直接看 `RUST_TS_PARITY_METRICS.md`
与 `LUM1336_COMPOSER_WIDTH.md` §6）。本轮关心的是：

| 口径 | 修前 | 修后 |
|---|---|---|
| 编辑 chord 与 codex / Martty / 上游 TS 一致 | ❌ alt+f 被 fork 遮蔽 | ✅ 全部一致 |
| chatinput 中用户唯一持续注视的输入面 chord 可达性 | 14/15 可见 chord 中 1 个被吞 | 15/15 |
| `app.session.fork` 默认键 | `alt+f`（invented by Stage 65） | `[]`（与上游 `packages/coding-agent/src/core/keybindings.ts:148` 一致） |

`app.session.tree` (`alt+t`) / `app.session.resume` (`alt+r`) / `app.session.new` (`alt+n`)
是 Stage 60/65 引入的 invented 默认键；本轮的 `ALLOWED_OVERLAPS` 不收
它们，因为它们**没有**与 editor/input chord 冲突（程序化扫描后确认），
所以本轮不动它们——保留作为 port 的有意加项，写进了表格级不变量。
如果未来有人给它们挪位置，再去更新不变量；这是表格测试本来的用法。

## 5. 门禁（Rust 1.85.0，离线）

```
PATH 上的 cargo 是坏 wrapper（~/.local/bin/cargo → 不存在的 /tmp/cargo-home/bin），
用 pi-rust/scripts/toolchain.sh 的同策略：直接把
/home/devbox/.rustup/toolchains/1.85.0-x86_64-unknown-linux-gnu/bin
放 PATH 最前。
```

- `cargo fmt --all -- --check` — clean
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — No issues found
- `cargo test --workspace --locked --no-fail-fast` — **175 suites, 2735 passed, 0 failed, 2 ignored**（profile 用
  `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`，避开 overlay 盘在
  `debug = 2 + incremental` 下偶发的 `No space left on device`）
- PTY A/B：`exit=0`，16/16 PASS（baseline `exit=1`，1 FAIL + 4 XFAIL）

## 6. 范围之外

LUM1336 §9 里那一串「下一轮第一顺位」（markdown/message 按列宽折行、
footer 分片预算、第二批扩展事件）都不在本轮；本轮专注 chatinput 的
chord 可达性。完成度数字（纯代码 88.9%、测试 49.4%、`app.*` 97.7%、
扩展事件 ~55–58%）从 LUM1336 §6 直接引用，本轮没有改这些维度。

新加的 `tests/chatinput_chord_conflicts.rs` 把「app 默认 chord 不得
无声吞掉 editor chord」写成了表格级不变量，让 LUM-1360 这类 bug
（Stage 65 的「自由 chord」假设错了）下次没机会 silent 复发。
