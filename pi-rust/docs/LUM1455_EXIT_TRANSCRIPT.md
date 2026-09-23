# LUM-1455 — 退出 pi 时把会话留在终端里（`fullscreenExitOutput`）+ 全 TUI 复核

> 树：`feature/pi.rs`（本轮开工 tip `b7d93acb5`，推前已并入 `fc18cb09e`）
> 环境：Windows 10 / cargo 1.97.1 / `--offline` / `CARGO_TARGET_DIR=/d/cargo-target-lum1455`
> 本机**没有真 PTY**（`scripts/pty_capture.py` 依赖 `pty`/`termios`），所以图走仓库的
> frame-buffer 通道（§4 说明它证什么、不证什么）。

## 0. 结论先行

本轮补的是**上游默认行为里用户每天都会碰到、而 pi-rust 完全没有的一格**：退出时把会话留在终端里。

上游 `fullscreenExitOutput` 默认 `transcript`——退出时先把整段记录写回**普通屏幕**（滚动缓冲里就留下了它），
再打印一行 `To resume this session: …`（`interactive-mode.ts:790-812`、`:3988`、`settings-manager.ts:1259`）。
pi-rust 的退出路径只做 `LeaveAlternateScreen`，**会话随备用屏幕一起消失**：`pi-rust/crates` 里
`To resume` 一次都没有（`grep -rn "To resume" pi-rust/crates` → 0 命中），
`fullscreenExitOutput` 这个键在 `config.rs` 里也没人读（LUM-1310 §4.1 记的「`fullscreen*` 三项未覆盖」之一）。

本轮把它补齐：`transcript` / `resume-hint` 两种模式 + `resume` 提示（**只在真能 resume 时打印**）+
`/settings` 行。顺带修掉一个被新测试逼出来的真 bug（§5.1）。

## 1. 缺陷：先量后改

| # | 事实 | 怎么量的 | 改前 |
|---|---|---|---|
| 1 | 退出后终端什么都没有 | `grep -rn "To resume\|fullscreen_exit_output" pi-rust/crates --include=*.rs` | 0 命中；`teardown_terminal` 只 `LeaveAlternateScreen` |
| 2 | `fullscreenExitOutput` 被静默忽略 | `grep -n "fullscreenExitOutput" pi-rust/crates/pi-coding-agent/src/config.rs` | 0 命中（LUM-1310 §4.1 已登记） |
| 3 | `/settings` 没有这一行 | `open_settings` 的 item 列表 | 6 行，无 `fullscreen-exit-output` |
| 4 | 提示语若照抄上游会**说谎** | `commands/resume.rs::resolve` 只认 SQLite；`main.rs` 只在 `--resume` 时建 SQLite | 全新交互会话只有 JSONL → `pi --resume <id>` 找不到 |
| 5 | `MessageView::visible_lines` 在 `scroll_to_top` 哨兵上溢出 | 新写的 transcript 测试直接命中 `message.rs:1253` `attempt to add with overflow`（debug） | `usize::MAX + height` 溢出 panic |

第 4 条是本轮最重要的一条判断：**不能把上游那句提示直接抄过来**。上游的交互会话从第一帧就是
persisted（SQLite），它的 `isPersisted()` 门就够用；pi-rust 的交互会话是 JSONL-only，
`--resume <id>` 解析不到它。所以提示的门槛改成「**有 SQLite 会话库且文件真的在**」——
这正是本项目一直在关的那类缺陷（广告了但不干活）。

## 2. 实现

| 文件 | 改动 |
|---|---|
| `crates/pi-tui/src/app.rs` | 新增 `App::transcript_text(width)`：整段聊天记录按**当前主题**渲染成 ANSI 行（复用唯一的布局 `MessageView::render_styled_lines` + `styled::themed_text`），逐行去尾空白；空会话返回空串（不打印一屏空白） |
| `crates/pi-tui/src/message.rs` | `visible_lines_with_links` 的 `height + scroll_from_bottom` 改为 `saturating_add`（§5.1 的溢出） |
| `crates/pi-coding-agent/src/config.rs` | 新增 `FullscreenExitOutput { Transcript, ResumeHint }` + `parse`/`as_str`、`DEFAULT_FULLSCREEN_EXIT_OUTPUT`、`UiSettings::fullscreen_exit_output`、`load_fullscreen_exit_output_default()`；未知字面量/类型 warn 后回落默认（对齐上游 getter 的 `=== "resume-hint" ? … : "transcript"`） |
| `crates/pi-coding-agent/src/paths.rs` | `default_session_dir()` 上移到这里（`main.rs` 的私有副本改为调用它），退出提示要靠它判断「是不是默认会话目录」 |
| `crates/pi-coding-agent/src/interactive.rs` | `InteractiveOptions::exit_output`；`/settings` 新增 `fullscreen-exit-output` 行（标签/描述逐字上游 `settings-selector.ts:685-690`）；`apply_setting_change` 新增该行（立即作用于**本次退出的**输出，并写回 `settings.json`）；新增纯函数 `exit_screen_output` / `format_resume_command` / `quote_if_needed`；`run_loop` 返回 `RunOutcome { exit, exit_output }`；`run_interactive` **先 teardown（离开备用屏幕）再写退出输出** |
| `crates/pi-coding-agent/src/main.rs` | 构造 options 时读 `load_fullscreen_exit_output_default()` |
| 测试 | `crates/pi-tui/tests/transcript_text.rs`（5）、`message.rs` 单测（1）、`interactive.rs` 单测（6）、`config.rs` 单测（3）、`crates/pi-coding-agent/tests/lum1455_exit_output_frames.rs`（3，出图） |

字节级契约（`exit_screen_output`，可纯函数测）：

| 模式 | 终端最后剩下什么 |
|---|---|
| `transcript`（默认） | 整段 transcript，然后 `\x1b[2mTo resume this session:\x1b[22m pi --resume <id>` |
| `resume-hint` | 屏幕恢复原样（备用屏幕已离开），只有那一行提示 |

两种模式都**先**离开备用屏幕再写（顺序就是「留在滚动缓冲里」的全部秘密，对齐上游
`TuiAltScreen.afterTerminalStop` `tui-alt-screen.ts:374-395`）。写失败不改变退出码（`pi | head` 不能把退出变成崩溃）。

## 3. `/settings` 行

```
Fullscreen exit output    Print the transcript or only a session resume hint when exiting fullscreen mode
                          transcript | resume-hint
```

行数 6 → 7（上游该行在 `tui-mode` 之后；本 port 没有 regular 模式，所以**没有**给 `tuiMode` 加行——
加了就是假广告，见 §5.3）。

## 4. 证据（截图是 frame-buffer 通道，不是 PTY 实拍）

`cargo test -p pi-coding-agent --test lum1455_exit_output_frames -- --nocapture` 打印的是
**真实 `exit_screen_output` 的字节**（transcript 行来自真实 `App::transcript_text`），
`scripts/frame_to_png.py` 把格网画成图；`.txt` 是同样内容的原始格网（可 grep / diff，带真 ANSI）。

| 图 | 内容 | 原始格网 |
|---|---|---|
| `docs/screenshots/lum1455-exit-transcript.png` | `transcript` 模式 + 可 resume 会话：10 行记录，末行是 dim 提示 | `lum1455-exit-transcript.txt` |
| `docs/screenshots/lum1455-exit-resume-hint.png` | `resume-hint` 模式：**只有 1 行**提示 | `lum1455-exit-resume-hint.txt` |
| `docs/screenshots/lum1455-exit-transcript-no-resume.png` | 全新会话（只有 JSONL、无 SQLite）：只留 9 行记录，**不打印**不可用的 resume | `lum1455-exit-transcript-no-resume.txt` |

诚实说明：frame-buffer 通道**不证明按键时序，也不是真 tty**；它证明的是**退出时终端收到的字节**
（面板 1 与面板 3 的差异就是第 4 条判断的肉眼可见结果）。提示行的 dim 是 `\x1b[2m`，
`frame_to_png.py` 只认 `0/1/4/7`，所以 PNG 里它是普通字——**原始字节在 `.txt` 里**，图的 caption 也写了这一点。

## 5. 本轮发现的真实问题（不只是做的这一件）

### 5.1 `MessageView::visible_lines` 溢出 panic（已修）

`scroll_to_top()` 用 `usize::MAX` 当「到顶」哨兵（`message.rs:979-987` 的文档就是这么写的），
而 `visible_lines_with_links` 算 `height as usize + self.scroll_from_bottom` → **debug 构建直接 panic**
（新写的 transcript 测试第一次跑就命中 `message.rs:1253`）。App 的渲染路径会先用
`resolved_scroll` 把哨兵解掉，所以线上没炸，但这是**公开 API 的陷阱**：任何直接调用者都会踩。
改成 `saturating_add`（饱和后 skip=0，语义正是「第一行」），并加回归测试
`message::tests::visible_lines_accept_the_scroll_to_top_sentinel`。release 行为不变。

### 5.2 仍缺：`tuiMode: regular`（下一轮第一顺位）

上游有两种 TUI 模式（`--tui-mode regular|fullscreen`，默认 **regular**：行内渲染、吃终端自己的滚动缓冲）；
pi-rust **只有**备用屏幕那一种（`setup_terminal` 无条件 `EnterAlternateScreen`）。
本轮补的是「现有模式的退出输出契约」，**不是模式切换**。真正的差距是一个 `TuiMainScreen` 等价物，量级远超一轮。

### 5.3 `fullscreenScrollbar` 仍未实现（本轮不碰）

`fullscreenScrollbar`（`auto|always|hidden`）与 `tuiMode` 一样是「设置了也没人读」的键。
本轮只关掉三项里的 `fullscreenExitOutput` 一项，**没有**给它加 `/settings` 行。

### 5.4 `app.*` 仍是 43/44：LUM-1263 的交付没进主分支

`scripts/keybinding_coverage.py --check` 在合并树上仍是：

```
tui.*: 49/49 consumed
app.*: 43/44 consumed
  UNCONSUMED app.tree.editLabel
```

而 LUM-1263（`feat(pi-rust): /tree 改名 UI`）的提交 `271cb109f` 停在 `origin/work/LUM-1263`
（`git ls-remote origin work/LUM-1263` = `271cb109f`，**已推、但没并入 `feature/pi.rs`**），
它基于 `c37d80742`，落后 LUM-1460/1464/1457/1466 以及本轮。

**本轮不收编它**，理由要写清楚：它的改动面（`interactive.rs` +402、`tree.rs` +314、`selector.rs`、两个扫描脚本）
与在飞的 LUM-1457/LUM-1466 **和本轮**都在同一批文件同一批区域（`interactive.rs`）上，
把它塞进本轮等于把三条并发线的冲突一次性吞掉——而它的验收标准（`app.* 44/44` + 三张帧图）
需要独立的一轮来复测。所以：**登记为下一轮第一顺位的收编项**，而不是本轮顺手合掉。

### 5.5 交互会话的写入侧（§3.9 的「写兼容」缺口）

全新交互会话只有 JSONL，没有 SQLite；上游从第一帧就持久化。这正是 §1 第 4 条让 resume 提示
**必须**收窄门槛的原因——补上写入侧，提示才能对每个会话都成立。

## 6. Rust ↔ TS 真实差距（本轮口径，本机实测）

| 口径 | 本轮 | 上一轮(LUM-1464) | 命令 |
|---|---|---|---|
| 纯代码规模 src↔src | **92.4%**（141,478 / 153,106） | 91.5%（140,032） | `python pi-rust/scripts/measure_loc.py` |
| 测试规模（新口径，与 §0.18 一致） | **49.8%**（2,773 / 5,572） | 49.5%（2,755 / 5,563） | `grep -rhoE '#\[(tokio::)?test\]' pi-rust/crates --include=*.rs \| wc -l` + `it.each` 类也算的 TS 计数（见 §0.18 的口径声明） |
| 测试规模（旧口径） | **52.2%**（2,773 / 5,309） | 51.1%（2,711 / 5,309） | 同上，TS 只数 `it(`/`test(` |
| 13 轴加权完成度 | **86.9%** | 86.9% | 公式公开在 `docs/RUST_TS_PARITY_METRICS.md` §4.1 |
| TUI 交互+视觉轴 | **90.3%** | 90.3% | （14×0.905 + 8×0.90）/ 22 |
| `fullscreenExitOutput` 消费 | **是**（退出真写 transcript / 提示） | 否（静默忽略） | `grep -rn "fullscreen_exit_output" pi-rust/crates` |
| 退出时会话保留 | **有**（默认 transcript） | **0**（会话随备用屏幕消失） | `grep -rn "To resume" pi-rust/crates` 改前 0 → 改后 3 |
| `/settings` 行 | **7** | 6 | `open_settings` 的 item 表 |
| 提示面硬编码 chord | 0（5 条知会项） | 0 | `scripts/hint_chord_literals.py --check` |
| `tui.*` / `app.*` 接线 | 49/49 · 43/44 | 同 | `scripts/keybinding_coverage.py --check` |
| 扩展生命周期事件 | 36/36 | 36/36 | `scripts/extension_event_coverage.py` |

**加权分一分没动，这是诚实的**：§4.1 那 13 条轴里没有「退出行为」这一格
（TUI 交互轴的口径是「模块率 + `app.*` 接线率」的均值，本轮 `app.*` 一动没动）。
本轮的价值登记在**两条新的、可反证**的轴上：`fullscreenExitOutput` 从「静默忽略」变成「真生效」，
以及「退出时会话保留」从 0 变成 1。**不靠重估旧轴把分数抬上去。**

## 7. 门禁（本机 Windows / cargo 1.97.1 / `--offline`）

| 门禁 | 本轮 | 基线（`git stash` 后同机同命令） |
|---|---|---|
| `cargo test -p pi-tui` | **1096 / 0** | **1090 / 0**（+6，全是本轮新测试） |
| `cargo test -p pi-coding-agent --lib` | **593 / 8** | **584 / 8**（+9；8 个失败**逐条同名**） |
| `cargo test -p pi-coding-agent --no-fail-fast` | 31 个 target，239 / 20 | 20 个失败全在 `cli_extensions 3/10`、`cli_tools 0/4`、`reload_config 3/1`、`system_prompt_resources 4/1`、`tools 35/1`、`tools_navigation 26/3`——与 LUM-1450 记录的基线计数逐条相同 |
| 本轮新测试 target | `transcript_text` 5/0、`lum1455_exit_output_frames` 3/0 | — |
| `cargo fmt --all -- --check` | exit 0 | exit 0 |
| clippy `-p pi-tui --all-targets -D warnings` | 0 告警 | 0 告警 |
| clippy `-p pi-coding-agent --lib -D warnings` | **被既存告警挡住**：`pi-extensions/src/host.rs:3383 signal_name is never used`（本轮未碰该文件），去掉 `-D warnings` 后 `pi-coding-agent` 自身 **0 条** 告警 | 同 |
| 四个扫描门禁 | `hint_chord_literals --check`、`keybinding_coverage --check`、`app_action_coverage --check-consumed`、`extension_event_coverage --check` 全部 in sync（exit 0） | 同 |

反向验证（本轮改动的行为断言在基线上必须失败）：把本轮源码 `git stash` 掉后
`cargo test -p pi-tui` 里根本没有 `transcript_text` 这 5 条用例（文件是本轮新增），
`config::tests::fullscreen_exit_output_*` 3 条与 `interactive::tests::exit_output_*` 也不存在——
即上述 +6 / +9 全部是本轮新增，且它们**只在改后通过**（改前不存在，不存在的东西不可能通过）。

## 8. 「最多 3 个任务」的处置 = **零派发**（槽位已满）

开工时扫 board（`multica issue list --project ae0b46e7… --status in_progress`）只有一个
`LUM-1434`（CLI flag 面，编程助手-go）；但同一条 autopilot 流水线在我这一轮前后**另有两个 run 在飞**，
证据是它们各自 workdir 里**未提交的改动**：

| run | workdir 里改到一半的文件 | 面 |
|---|---|---|
| LUM-1457 | `pi-tui/src/app.rs`、`pi-coding-agent/src/interactive.rs`、`tests/key_event_kinds.rs`(新)、`scripts/pty_capture_win.py`(新) | 鼠标/键盘事件 |
| LUM-1466 | `pi-coding-agent/src/footer.rs`(新)、`pi-tui/src/status.rs`、`tests/lum1466_two_line_footer_frames.rs`(新) | 两行 footer（LUM-1464 §5 的第一顺位） |

加上 LUM-1434，三个槽位都占着；而且 LUM-1457 / LUM-1466 都动 `app.rs` / `interactive.rs`，
本轮再派第四条只会重演「同一缺陷两条并发线各修一次」。所以：**self-do 1 件 + 零派发**。
（本轮开工时远端 `feature/pi.rs` 是 `b7d93acb5`，我推之前它已经被它们推到 `fc18cb09e`——在飞是真的。）

## 9. 完成进度与已知限制

- 进度：规模 **92.4%**、测试 **49.8%（新口径）/ 52.2%（旧口径）**、13 轴加权 **86.9%**、TUI 交互+视觉 **90.3%**。
- 已知限制 1：本机无 PTY，三张图是 frame-buffer 通道；真 tty 实拍需在 Linux runner 上重跑。
- 已知限制 2：`pi-coding-agent` 有 8（lib）+ 20（集成）条**既存** Windows 环境类失败（绝对路径/临时目录/扩展加载），
  已 A/B 逐条证明与本轮无关。
- 已知限制 3：`resume` 提示只对 SQLite 会话出现（§5.5 的写入侧缺口），上游对每个 persisted 会话都打印。
- 已知限制 4：transcript 是**聊天记录**，不含 composer/status chrome（上游 regular 渲染器会把输入区也画出来，而它还不存在，§5.2）。
- 已知限制 5：`frame_to_png.py` 不渲染 `\x1b[2m`，所以图里提示行不显暗——原始字节见 `.txt`。

## 10. 范围之外

未碰 `pi-ai` / `pi-agent-core` / `pi-protocol` / `pi-extensions` / `pi-session` / `pi-server` /
`pi-client` / `pi-chord` / `pi-evals` / `pi-telemetry`；未碰上游 TS（`packages/**`，只读取证）；
未碰 CI / Docker；未碰扩展事件表、slash 命令表、CLI flag 表（LUM-1434 在飞）。
