# LUM-1308 — `Ctrl+G` / `Ctrl+Z` 接线、门禁钉版与完成度复测

本轮主题：把 `app.*` 面上**最后两个「广告了却没人消费」的 chord** 真正接上，并顺手把「门禁绿不绿取决于
PATH 上是哪个 cargo」这个反复出现的坑钉死。

一句话结论：`app.editor.external`（`Ctrl+G`）与 `app.suspend`（`Ctrl+Z`）从「绑定了、也印在启动头和
`/hotkeys` 里、但没有任何代码路径会 resolve 它」变成真正的功能；`app.*` 口径 **35/44 → 37/44（84.1%）**，
**advertised 2/44 → 0/44**；仓库现在有 `scripts/toolchain.sh`，在坏 rustup shim 的机器上也能复现同一套门禁。

---

## 1. 缺陷：两个「假广告」

`pi-rust/scripts/app_action_coverage.py` 把每个 `app.*` action 分成三类（wired / advertised / silent）。
上一轮（LUM-1307）收尾时是：

```
wired: 35/44 (79.5%)
advertised: 2/44 (4.5%)      app.editor.external   app.suspend
silent: 7/44 (15.9%)
```

`advertised` 这一类是最坏的：启动头（`pi-tui/src/locale.rs` 的 `STARTUP_HINTS`）和 `/hotkeys`
（`pi-coding-agent/src/commands/slash.rs` 的 APP 表）都会把它印给用户，用户按下却什么都不发生。
这跟 `silent`（只是没实现、也不宣传）不是一回事。

上游 TS 侧两个 chord 都是**有实现**的，所以这不是「上游也没有」：

| chord | 上游位置 | 上游行为 |
|---|---|---|
| `Ctrl+G` | `core/keybindings.ts:127`（`app.editor.external`） | `interactive-mode.ts:4246-4262` → `editInExternalEditor`：把 draft 写进临时 `pi-editor-*/prompt.md`，`$EDITOR` 跑完读回 |
| `Ctrl+Z` | `core/keybindings.ts:97`（`app.suspend`，非 Windows） | `interactive-mode.ts:3550-3600` → 退出 raw mode/备用屏，`process.kill(0, "SIGTSTP")` |

## 2. 实现

### 2.1 `crates/pi-coding-agent/src/external_editor.rs`（新增，391 行 / 12 个测试）

上游 `packages/coding-agent/src/modes/interactive/external-editor.ts` 的逐条对照：

| 上游行为 | Rust |
|---|---|
| `banner`：`Launching external editor: ${cmd}\nPi will resume when the editor exits.` | `banner()` `external_editor.rs:55`，**文案逐字相同**（有测试钉住） |
| `configured.trim() !== ""` 才用配置值 | `resolve_command()` `:64`；空白串忽略，顺序 `settings.externalEditor` → `$VISUAL` → `$EDITOR` → 平台默认（`nano`/`notepad`） |
| 临时目录 `pi-editor-*` + `prompt.md` | `unique_temp_dir()` `:95` = `pi-editor-{pid}-{seq}-{nanos}` |
| `stripBom` + 去掉**一个**结尾换行 | `normalize()` `:81`（上游同样只去一个） |
| 退出码非 0 → `failed`，读回的文件丢弃 | `run_round_trip()` `:143`，非 0 或二进制不存在 → `ExternalEditorResult::Failed` |
| 命令按空格切分、路径作为最后一个参数 | 同左（`sh`-style 单空格切分，与上游 `command.split(" ")` 一致） |

**没有新增依赖**：`tempfile` 在本仓库只是 dev-dependency，运行时不可用，所以 `unique_temp_dir()` 是手写的
（pid + 进程内计数器 + 纳秒）；上游用的 `os.tmpdir()` + `mkdtemp` 语义由 `std::env::temp_dir()` + 自增名覆盖。

`edit_in_directory()` `:124` 把「创建/清理目录」从 `edit_in_external_editor()` `:114` 里拆出来，
目的只有一个：让测试能断言**清理发生了**（成功/失败两条路径都断言，见 `:295`、`:320`）。

### 2.2 `Ctrl+Z` 与终端交接（`interactive.rs`）

`#![forbid(unsafe_code)]` 让 `libc::kill(0, SIGTSTP)` 不可用，仓库也没有 `libc`/`nix`，所以停自己用的是
`sh -c "kill -TSTP 0"`（`stop_process_group()` `interactive.rs:3880`，注释里写明了这个取舍）。

终端交接按「谁拥有 terminal」拆开（driver 拥有，`handle_input_event` 只返回 `InternalAction`）：

- `suspend_tui()` `:3762`：离开 raw mode / 备用屏 / 显示光标（从 `teardown_terminal` 里拆出，后者现在只是调用它）；
- `resume_tui()` `:3778`：重新进 raw mode + 备用屏 + 鼠标捕获 + 隐藏光标 + `clear()`；
- `run_external_editor()` `:3796`：`suspend_tui` → 跑 editor → `resume_tui` → 把结果写回 composer；
  失败时 `flash_status("external editor failed; draft unchanged")` `:3838`（**有意的偏离**：上游往 stdout 打字，
  但 Rust 侧这时刚重画了备用屏，stderr/stdout 会被覆盖，状态条是唯一可见的位置）；
- `suspend_to_background()` `:3845`：`suspend_tui` → `stop_process_group()` →（被 `SIGCONT` 唤醒后）`resume_tui`。

`InternalAction` `:679` 因此**去掉了 `Copy`**（`ExternalEditor { command: String }` 携带 owned 字符串），
这是本轮唯一一个牵动到 `run_loop` 的签名变化。

两个 chord 都在既有的「没有浮层打开」守卫内被认领（`:1089` / `:1103`），因为 `ctrl+g` 同时还是
`tui.altScreen.searchNext`（`packages/tui/src/keybindings.ts:197`）。

### 2.3 广告面

`pi-tui/src/keybindings.rs:424-425` 把两个 id 加进 `CONSUMED_APP_ACTIONS` —— 这一处**同时**翻转两个广告面
（启动头与 `/hotkeys`），因为过滤都走 `app_action_is_consumed`。相应地三个测试从「断言它们是死的」翻成
「断言它们是活的」：`pi-coding-agent/tests/startup_header.rs`（`KNOWN_UNWIRED` 现在为空，保留作绊线）、
`pi-tui/tests/startup_header.rs`、`commands/slash.rs:923` 附近。

### 2.4 门禁钉版：`scripts/toolchain.sh`（新增）

前几轮「fmt/clippy 红」的根因不是代码，而是 `~/.local/bin/cargo` 是 rustup shim 且没有默认工具链，
`cargo +1.85.0` 又会先去同步 channel（沙箱里会卡住）。`scripts/toolchain.sh` 直接按目录找已安装工具链
（`<RUSTUP_HOME|$HOME/.rustup>/toolchains/<ver>-*/bin`），**不联网、不依赖 rustup 默认值**，找不到就报错退出
而不是静默用别的版本。`PI_RUST_TOOLCHAIN` / `PI_RUST_BIN` 可覆盖；`scripts/ci.sh` 现在先 source 它。

---

## 3. 门禁实况（1.85.0，`--locked`）

工具链：`1.85.0`（本机唯一完整安装的 toolchain，`scripts/toolchain.sh` 解析出来的那个）。
所有 cargo 调用都带 `--locked`，`CARGO_INCREMENTAL=0`（容器磁盘 50G，增量编译目录自己就能吃掉 2G）。

```bash
. pi-rust/scripts/toolchain.sh      # 或 export PATH=$HOME/.rustup/toolchains/1.85.0-x86_64-unknown-linux-gnu/bin:$PATH
cd pi-rust
cargo fmt --all -- --check                                  # 干净，无 diff
cargo clippy --workspace --all-targets --locked -- -D warnings   # 0 findings，exit 0
cargo test --workspace --locked                             # exit 0，2469 passed / 0 failed / 2 ignored（162 个 suite）
```

实测结果：

| 门禁 | 结果 | 备注 |
|---|---|---|
| `cargo fmt --all -- --check` | **PASS**（无 diff） | 本轮新代码先被 fmt 抓到 20 处，已 `cargo fmt --all`；顺带修掉 5 个旧文件的格式漂移 |
| `cargo clippy --workspace --all-targets -- -D warnings` | **PASS**（0 findings，exit 0） | 修掉 10 条后归零：3 条 `needless_lifetimes`（`pi-telemetry`×2、`pi-ai`×1）、1 条 `precedence`（`pi-extensions/src/deflate.rs:650`）、1 条 `const_is_empty`＋1 条 `duplicated_attributes`＋1 条 `nonminimal_bool`＋1 条 `format_collect`＋2 条 `useless_conversion`。**其中「`locale.rs:374` 的 `!is_empty()` 是空断言」这条是真缺陷**，改成断言中文文案内容 |
| `cargo test --workspace --locked` | **2469 passed / 0 failed / 2 ignored** | 上轮基线 2457 → +12 = 本轮的 `external_editor` 单测 |

**与前几轮「fmt/clippy 红」的差异不是代码变了，而是 PATH 变了**：本机 `~/.local/bin/cargo` 是没配默认工具链的
rustup shim，谁先跑谁红。`scripts/toolchain.sh` 直接按目录定位工具链（不联网、不依赖 rustup 默认值），
所以上面三条现在任何人跑都是同一结论。

> 磁盘说明：本轮 `cargo test` 跑在 `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` 下。
> 带调试信息的 test 二进制每个 150MB+，`target/debug/deps` 一度涨到 **16G** 并直接 `ENOSPC`
> （第一次跑的 134 个失败全是 `StorageFull`，与代码无关）。关掉 debuginfo 只影响回溯可读性，
> 不影响断言；`fmt`/`clippy` 两条门禁与调试信息无关。

---

## 4. 完成度复测（每条都附可复现命令）

### 4.1 规模

```bash
find pi-rust/crates -path '*/src/*' -name '*.rs' ! -name 'mod.rs' | xargs wc -l | tail -1   # 129,802
find packages -path '*/src/*' -name '*.ts' ! -name '*.d.ts'   | xargs wc -l | tail -1        # 153,066
```

**src↔src = 129,802 / 153,066 ≈ 84.8%**（上轮 84.4%，+628 行为本轮新增）。
含全部 `tests/` 的口径：184,478 / 295,992 ≈ **62.3%**。
LOC 不是质量：`pi-session`（sqlite+zstd 落盘）、`pi-protocol`、`pi-chord`、`pi-telemetry` 是 TS 侧没有的部分。

### 4.2 测试

```bash
grep -rhoE '#\[(tokio::)?test\]' pi-rust/crates --include=*.rs | wc -l                 # 2,402
python3 - <<'PY'  # TS 侧 it()/test()（排除 node_modules/dist/.d.ts）
PY                                                                                      # 5,429
```

**用例口径 2,402 / 5,429 ≈ 44.2%**（上轮 45.0%，分母涨 120 而分子只涨 12 → 比例略降，不是我方退化）。
测试规模仍是最大的隐性债务。

### 4.3 `app.*` 接线

```bash
python3 pi-rust/scripts/app_action_coverage.py    # wired 37/44 (84.1%)，advertised 0/44，silent 7/44
```

silent 的 7 个没变：`app.models.{clearAll,enableAll,reorderUp,reorderDown,save,toggleProvider}`、
`app.tree.editLabel`。它们**不宣传**，所以不是用户可见缺陷，但键盘走不到「批量启停模型 / 重排模型 /
改会话树标签」。注意这个脚本是 **id 分发**口径：靠匹配裸键（`Key::Ctrl('c')`）实现的 chord 会被算成未接线，
所以 37/44 是下界。

### 4.4 slash 内置命令：**17 / 23**（沿用 LUM-1307 §4.4 的修正口径）

上游 `packages/coding-agent/src/core/slash-commands.ts` 的 `BUILTIN_SLASH_COMMANDS` 23 条；
Rust 侧 `crates/pi-coding-agent/src/commands/slash.rs` 的 `IMPLEMENTED` 命中 17 条。
缺 6：`scoped-models import share changelog login logout`。本轮未触碰（`/reload` 已在上轮）。

### 4.5 扩展生命周期事件（本轮最大的开放轴）

```bash
python3 pi-rust/scripts/extension_event_coverage.py
# upstream events: 36；wire tags 21/36 (58.3%)；production emit sites 20/36 (55.6%)
# rust-only: user_message；dead variant: model_select；missing: 15
```

缺的 15 个：`after_provider_response agent_settled before_agent_start before_provider_headers
before_provider_request context project_trust session_before_compact session_before_fork
session_before_switch session_before_tree session_compact_failed session_tree ui_prompt_end ui_prompt_start`。
**`before_agent_start` / `context` / `tool_*` 这类正是插件作者最常用的钩子**，它决定「能不能直接吃 pi 插件生态」。

### 4.6 加权完成度：本轮仍**不重算**，只报逐项状态

LUM-1306 的 82.2% 来自一张主观权重表（13 轴），分项分值不在同一份可追溯文档里。本轮关闭的可见项：
「`app.*` 无消费入口」中的 2 条 + 「`app.*` 假广告」整类（2→0）。若只按本轮可复核的 4 个口径看，
`app.*`（84.1%）与规模（84.8%）在 85% 附近，而**扩展事件（55.6%）、slash（74%）、测试用例数（44.2%）**
明显落后 —— 加权总分落在 80% 上下是合理的，但**我不给新数字**，因为同一张权重表不能凭空重建。

---

## 5. Rust ↔ TS 差距（本轮视角，按影响排序）

| 级别 | 差距 | 证据 |
|---|---|---|
| **P0** | 扩展生命周期事件 20/36，15 个变体缺失（含 `before_agent_start`/`context`），1 个死变体 | §4.5 |
| **P1** | 门禁可复现性：历史上「fmt/clippy 红」是工具链问题而不是代码问题；本轮补了 `scripts/toolchain.sh`，但仓库仍无 `rust-toolchain.toml`（**有意不加**：会让每次 cargo 调用先等 rustup 同步） | §2.4、§3 |
| **P1** | 测试规模 44.2%：TUI 交互路径主要靠 PTY 截图（人看）与少量行为测试，回归网比 TS 薄 | §4.2 |
| **P1** | `app.*` 剩 7 个 silent：`models.*` 批量/重排/保存、`tree.editLabel`，键盘走不到 | §4.3 |
| **P2** | slash 缺 6：`scoped-models import share changelog login logout`（后两个涉及 OAuth/账号面） | §4.4 |
| **P2** | settings.json 只有少数键被消费（`theme`、`fullscreenCopyOnSelect`、本轮的 `externalEditor`） | `config.rs` |
| **P2** | CLI flag 面：22 个 TS flag 无字面等价物（未复测，沿用 LUM-1307 §5） | LUM-1307 §5 |

---

## 6. PTY 证据（真实 PTY，`pyte` 逐字节渲染）

### 6.1 `Ctrl+G`：`pi-rust/scripts/pty_scenarios/lum1308-external-editor.json`

三格来自**一次运行**，`$EDITOR` 是一个会 `sleep 1.2` 的假编辑器（这样第 2 格能抓到「终端已交给编辑器」的画面）：

1. 打草稿 → 第 1 格（`expect: original draft typed by the user`）；
2. `Ctrl+G` → 第 2 格：备用屏已退出，正常屏上是 `Launching external editor: ./editor.sh` /
   `Pi will resume when the editor exits.`（**上游原文**）；
3. 编辑器退出（0）→ 第 3 格：备用屏重新进入，composer 里是编辑器写入的内容
   （`expect: EDITED BY EXTERNAL EDITOR #1` + `second line`，`excludes: original draft typed by the user`）。

harness 为此加了两处支持（`pty_capture.py`）：`"env"` 场景键（可 pin `EDITOR`/`VISUAL`，空串表示 unset）
与 `"fixtures"` 的字典形式（内容 + `.sh` 自动 `chmod 755`）。在此之前 scenario 只能写死「fixture 是空文件」。

运行命令与输出：

```bash
python3 pi-rust/scripts/pty_capture.py --bin pi-rust/target/debug/pi \
  --steps pi-rust/scripts/pty_scenarios/lum1308-external-editor.json \
  --out pi-rust/docs/screenshots/lum1308-external-editor.png
# 3/3 格断言 PASS：
#   [1] expect 'original draft typed by the user'                PASS
#   [2] expect 'Launching external editor: ./editor.sh'          PASS
#   [2] expect 'Pi will resume when the editor exits.'           PASS
#   [3] expect 'EDITED BY EXTERNAL EDITOR #1'                    PASS
#   [3] expect 'second line'                                     PASS
```

第 2 格还有个额外信息：备用屏退出后，露出的正常屏上除了 banner，还有启动时打印的那份
`pi v0.1.0` / 快捷键表 —— 也就是说 `Ctrl+G` 期间用户看到的是「启动头 + banner」，不是一片空白，
这跟上游 `interactive-mode.ts` 的语义一致（退出备用屏 → 跑编辑器 → 重进备用屏）。

证据文件：`pi-rust/docs/screenshots/lum1308-external-editor.png`（`+.png.txt` 里有逐格断言与字符网格）。

### 6.2 `Ctrl+Z`：`pi-rust/scripts/pty_probe_ctrl_z.py`（新增）

suspend 的产物是**进程状态**而不是画面，`waitpid(WNOHANG)` 量不到（stopped 的 child 仍算「活着」），
所以这个 probe 读 `/proc/<pid>/stat` 的状态位。两个 case：

- **case A（直接 spawn，child 自己 `setsid`）**：进程组是孤儿组，内核直接丢弃 `SIGTSTP`（POSIX 语义：
  没有人能把它捞回来），所以 `Ctrl+Z` 只会重画一帧。这解释了为什么 `pty_capture.py` 的截图**无法**断言 suspend。
- **case B（同一个 PTY 里先起交互 `bash`）**：`monitor` 打开、job 在自己进程组里、bash 是同会话父进程 →
  进程组不是孤儿组，stop 真的发生。断言链：`state_before=R/S` → `Ctrl+Z` → `state=T` 且 bash 打印
  `Stopped` → `fg` → `state=R/S` 且备用屏重画。

运行结果（`docs/screenshots/lum1308-ctrl-z-probe.txt` 是本文件的原文快照）：

```
== case A: pi spawned directly (own session -> orphaned group) ==
   startup frame         : True
   Ctrl+Z: state=S alive=True repainted=False     <- 内核丢掉了 SIGTSTP：连一帧都没变
== case B: pi started from an interactive bash (job control) ==
   shell prompt          : True (pid 717)
   monitor mode on       : True
   TUI started           : True
   pi pid / state        : 720 / S
   after Ctrl+Z          : state=T stopped=True bash_said_stopped=True
   after fg              : state=S resumed=True repainted=True alive=True

verdict: case B PASS (Ctrl+Z stopped the TUI, fg resumed and repainted it)
```

`state=T` 是内核给的、bash 的 `Stopped` 是 shell 给的、`repainted=True` 是应用给的 —— 三个独立观察点都成立，
才写成「`Ctrl+Z` 真的能用」。

---

## 7. 已知限制（照实说）

1. **suspend 的孤儿组限制与上游一致**：在 `setsid` 出来的进程里按 `Ctrl+Z` **完全无反应**（内核丢 `SIGTSTP`，
   case A 实测 `state=S` 且 `repainted=False`）。case A 就是这个事实的现场记录，不是「测试没写」。
2. **`sh -c "kill -TSTP 0"` 依赖 `/bin/sh`**：这是没有 `unsafe`、没有 `libc` 时唯一能做的事；
   如果 PATH 上没有 `sh`，`suspend_to_background` 会在 `resume_tui` 之后正常返回（不 panic，但也不会停）。
3. **本轮的测试是在 `CARGO_PROFILE_*_DEBUG=0` 下跑的**：容器磁盘只有 50G 且与另外两个 agent 共用，
   带调试信息的 test 二进制每个 150MB+（`target/debug/deps` 一度 16G，直接 ENOSPC）。
   关掉 debuginfo 只影响回溯可读性，不影响任何断言语义；§3 里同时给出加不加该开关的结论差异。
4. **`pty_capture.py` 的 `env` 键是给这轮加的**：它只做「合并到 env、空串=unset」，没有做白名单校验。
5. `cargo fmt --all` 顺带修了 5 个**本轮没碰过**的文件的格式漂移（`tests/reload_config.rs`、`pi-tui/src/prompt.rs`、
   `tests/composer_multiline.rs`、`tests/truncated_above.rs`），因为「fmt 门禁绿」这件事必须整体成立。

---

## 8. 下一步（建议 ≤3 个 sub-issue）

1. **P0 扩展事件**：补 `before_agent_start` / `context` / `agent_settled` / `session_*` 这 15 个变体的
   emit 点（先挑插件最常用的 4 个），把 20/36 推到 26/36 以上，并让 `extension_event_coverage.py` 进 CI。
2. **P1 `models.*` / `tree.editLabel` 接线**：7 个 silent 里 6 个在同一张 `/models` 面板上，一次增量能全接完。
3. **P1 测试规模**：把本轮这种「一次运行、多格断言」的 PTY 场景固化成可重复的回归（目前仍需人读 PNG），
   优先覆盖 composer 编辑操作与浮层键盘路径。
