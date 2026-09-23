# LUM-1307 — 启动即应用 `settings.json` 的界面切片 + 门禁/差距复测（tip `0ed96bb8e` + 本轮）

> 基准快照：`origin/feature/pi.rs` = `0ed96bb8e`（LUM-1306 的 `/reload` 提交）
> 环境：Linux x86_64 / **rustup toolchain 1.85.0**（cargo/rustc/rustfmt/clippy 同版本）
> `~/.local/bin/cargo` 是坏 wrapper（指向不存在的 `/tmp/cargo-home`），本轮的 `cargo fmt` / `clippy`
> 结论用的是 `$HOME/.rustup/toolchains/1.85.0-x86_64-unknown-linux-gnu/bin/cargo`。
> 对应 issue LUM-1307：基于 rust 重写 pi、推进 LUM-981、代码推到远程并合入 `feature/pi.rs`、
> 学 Martty TUI、打造最佳 UX 的 TUI、截图展示 codex/pi-class 交互、真实审计 Rust↔TS 差距。

## 0. 结论速览

| 口径 | 本轮实测 | LUM-1306 | 说明 |
|---|---|---|---|
| 本轮交付 | 启动即应用 `settings.json` 的 UI 切片（theme / fullscreenCopyOnSelect） | 补 `/reload` | §1 |
| 真 PTY A/B 截图 | **4 张 PNG + 4 份字符 dump**（两组场景 × 修前/修后） | 4 张 | §2 |
| `cargo test --workspace --locked` | ✅ **2457 passed / 0 failed**（1m47s） | 2454 | +3（本轮单测） |
| `cargo fmt --all -- --check`（1.85 的 rustfmt 1.8.0） | ❌ **15 处 diff / 5 个文件，全部为既有**（`interactive.rs` 零 diff） | 上轮记为 ✅ | §3.2：**版本差异**，非缺陷 |
| `cargo clippy --workspace --all-targets -- -D warnings`（1.85 的 clippy） | ❌ **11 条既有 warning**（7 个文件，3 条来自依赖 crate 直接中断编译） | 上轮记为 ✅ | §3.3：**版本差异或口径不同**，无法复核 |
| `cargo clippy --workspace --all-targets`（不加 `-D`） | ✅ EXIT=0（2m14s），11 条 warning 清单见 §3.3 | — | 本轮新增的可复现清单 |
| 纯代码规模（src） | 129,174 / 153,066 ≈ **84.4%** | — | §4.1，与旧文档的 57.5% / 82.5% 口径不一致，已标注 |
| 测试规模（用例） | 2,390 标记 / 5,309 ≈ **45.0%** | 2,232 / 5,309 ≈ 42.0% | §4.2 |
| `app.*` 接线 | **35 / 44 (79.5%)** | 35/44 | `scripts/app_action_coverage.py` 实跑 |
| slash 内置命令（按上游名单逐名核对） | **17 / 23** | 上轮记 20/23 | §4.4 口径修正 |
| 加权完成度 | **不重算**（理由见 §4.5） | 82.2% | 只逐项报「已关闭 / 仍开放」 |

一句话：**这轮修的是一个「用户能立刻感知、但只影响一行配置」的真缺口**，并把上两轮「门禁全绿」的
结论改成了更准确的形状：**绿不绿取决于未钉住的工具链版本，本环境下两条门禁都红且与代码缺陷关系不大**。

## 1. 本轮交付：`settings.json` 的界面切片在启动时就生效

### 1.1 缺口是什么

上游在**构造交互会话时**读 settings 文件，所以用户手写的 `theme` /
`fullscreenCopyOnSelect` 在第一帧就已经生效。本移植只让两个入口读它：`/settings`
（`interactive.rs:3022` 附近的 `open_settings`，其 Theme 行取的是**运行中的调色板**，不是文件）
和 LUM-1306 新加的 `/reload`（`reload.rs`）。于是：

- `settings.json` 写 `theme: "light"`，启动后仍是暗色，直到用户主动打开设置面板或敲 `/reload`；
- `fullscreenCopyOnSelect: false` 同理，`app.copy_on_select()` 保持内置默认 `true`。

LUM-1306 §4.3 把这个缺口写在文档里，但没修。本轮把它关掉。

### 1.2 语义

| `settings.json` 键 | 落点 | 失败时 |
|---|---|---|
| `theme` | `App::set_theme_by_name`（`pi-tui/src/app.rs:1761`） | **不吞**：`theme_error` 带上解析器的原文，并在 transcript 打印 `theme → not applied: <原因>`；旧调色板保留，App 不会变成「无主题」 |
| `fullscreenCopyOnSelect` | `App::set_copy_on_select`（`pi-tui/src/app.rs:1752`） | 无失败路径；缺键即内置默认（`true`） |

两个键都**无条件设置**：文件里没有这个键，语义就是「用内置默认」，而 `UiSettings::default()`
正是内置默认。这样「文件缺失」和「文件里写默认值」得到同一结果，和 `/settings`、`/reload`
读的是同一个 `config::load_ui_settings()`，不存在第二套解析。

### 1.3 落地位置

| 文件 | 内容 |
|---|---|
| `crates/pi-coding-agent/src/interactive.rs:3048` | `pub fn apply_startup_ui_settings(app, sources) -> StartupUiSettings` |
| 同上 `:3022` | `pub struct StartupUiSettings { theme, theme_error, copy_on_select }`（让启动行为可断言） |
| 同上 `:477` | `run_loop` 里 `App::new` 之后的调用点（紧邻既有的「持久化 thinking level 启动即应用」块，两者同一模式） |
| 同上 `:4167` / `:4190` / `:4205` | 3 个单测：写盘→生效、文件缺失→内置默认、主题名解析失败→报错且保留默认 |
| `scripts/pty_scenarios/lum1307-startup-settings.json` | A/B 场景（真 PTY） |
| `scripts/pty_scenarios/lum1307-startup-theme-error.json` | 失败分支场景 |
| `scripts/theme_palette_report.py` | 新增：把截图**当颜色**读回来并对齐到内置主题 token（§2.3） |

### 1.4 为什么这里需要一个新工具（`theme_palette_report.py`）

`pty_capture.py` 的断言是**文本**匹配，而主题是**颜色**：`expect "light"` 只会在 `/settings`
的面板文字里命中，证明不了「第一帧用的是浅色调色板」。所以本轮补了一个小工具，把截图顶部区域
的颜色直方图打出来，并把每个观测到的 hex 对齐到 `crates/pi-tui/assets/themes/*.json` 里的
token 名，再打印 dark→light 的迁移。这样「调色板在启动时换了」这句话可以被审阅者用仓库里的
真主题文件逐步核对，而不是靠一段散文。

## 2. 证据（真 PTY + pyte 终端仿真，非 mock）

### 2.1 A/B 是怎么跑的

同一个场景文件、同一串按键、同一份准备好的 `HOME`，**只换二进制**：

```bash
# 修前二进制：pristine tip 0ed96bb8e 的独立 worktree 构建
git worktree add /tmp/lum1307/before-tip 0ed96bb8e && (cd /tmp/lum1307/before-tip/pi-rust && cargo build -p pi-coding-agent --bin pi)
# 修后二进制：本轮工作树
cargo build -p pi-coding-agent --bin pi
# fixture：theme=light, fullscreenCopyOnSelect=false
printf '{"theme":"light","fullscreenCopyOnSelect":false}' > /tmp/lum1307/home-light/.pi/agent/settings.json
# 两组场景 × 两个二进制
python3 scripts/pty_capture.py --bin /tmp/lum1307/pi-after  --steps scripts/pty_scenarios/lum1307-startup-settings.json --home /tmp/lum1307/home-light --out docs/screenshots/lum1307-startup-ui-after.png
python3 scripts/pty_capture.py --bin /tmp/lum1307/pi-before --steps scripts/pty_scenarios/lum1307-startup-settings.json --home /tmp/lum1307/home-light --out docs/screenshots/lum1307-startup-ui-before.png
```

### 2.2 断言矩阵（两次运行的原始输出）

| 场景 / 断言 | 修前 | 修后 |
|---|---|---|
| **启动帧**：`pi v0.1.0`、`type a prompt` 存在 | PASS | PASS |
| `/settings`：`Theme` 行存在 | PASS | PASS |
| `/settings`：`^  Theme\s+light$` | **XFAIL**（显示 `dark`） | **PASS**（显示 `light`） |
| `/settings`：`^  Fullscreen copy on select\s+false$` | **XFAIL**（显示 `true`） | **PASS**（显示 `false`） |
| 主题名不可解析时启动帧出现 `theme → not applied` | **XFAIL**（什么都不说） | **PASS**（`theme → not applied: Theme not found: solarized`） |
| 主题名不可解析时 `/settings` 仍报 `^  Theme\s+dark$` | PASS | PASS |
| 合计 | 5 PASS / 2 XFAIL，0 FAIL | **9 PASS / 0 FAIL** |

`/settings` 那一行是本轮最干净的文本级证据：该面板的 Theme 行读的是**运行中的 App**
（`app.theme().name()`，「实时调色板胜过持久化值」），所以修前显示 `dark`、修后显示 `light`，
不是面板自己在读一次文件。

截图（`docs/screenshots/`，`--sheet` 未开启，一张 PNG 内含两个面板的纵向拼图）：

- `lum1307-startup-ui-before.png` / `lum1307-startup-ui-after.png`（+ 同名 `.png.txt` 字符网格 dump）
- `lum1307-startup-theme-error-before.png` / `lum1307-startup-theme-error-after.png`

### 2.3 调色板取证（颜色维度，文本断言看不到的）

```bash
python3 scripts/theme_palette_report.py docs/screenshots/lum1307-startup-ui-before.png docs/screenshots/lum1307-startup-ui-after.png
```

输出（截图上半部分 = 启动帧区域，2,581,044 / 2,581,220 像素）：

| 观测颜色 | 修前像素 | 修后像素 | 对齐到的主题 token |
|---|---|---|---|
| `#8abeb7` | 9,594 | 0 | `dark.accent` |
| `#5a8080` | 0 | 9,697 | `light.teal`（= 浅色主题的 accent） |
| `#808080` | 21,847 | 0 | `dark.gray`（muted） |
| `#6c6c6c` | 0 | 21,827 | `light.mediumGray`（muted） |
| `#666666` | 6,879 | 0 | `dark.dimGray`（dim） |
| `#767676` | 0 | 6,793 | `light.dimGray` / `light.thinkingMinimal` |
| `#d4d4d4` | 2,081 | 2,081 | —— 见下 |

结论：启动帧上的 accent / muted / dim 三个 token **整体从 dark 方案换成了 light 方案**，
且 hex 与仓库里 `light.json` 的值逐一对得上。这是「调色板在第一帧之前就已装载」的直接证据。

两个诚实注记（避免读者被数字误导）：

1. `#d4d4d4` 两边**完全相同**：那是 `pty_capture.py:160` 的 `DEFAULT_FG`，用于前景色是
   *终端默认* 的单元格（工具链自身的渲染色），不是 pi 主题 token。它不随主题变化，
   所以不能拿它当证据。
2. 背景色两边都是 `#0a0a0c`、顶部横幅 `#2a2c36` / `#eeeef0` 也两边相同：前者是 pi
   不铺满背景的既有行为，后者是 harness 画的 panel 标题条。**本轮没有改变背景填充**，
   浅色主题在深色终端里仍然是「浅色前景 + 终端背景」，这与上游一致（上游主题同样只改
   前景/token，不改终端底色）。

## 3. 门禁实况（本轮实测，与上轮文档的乐观结论不同）

### 3.1 测试：绿

```
cargo test --workspace --locked --no-fail-fast   # EXIT=0，1m46.979s
test result: 2457 passed / 0 failed   （2454 基线 + 本轮 3 个单测）
```

### 3.2 `cargo fmt --all -- --check`：红，**是 rustfmt 版本差异，且仓库没钉住工具链**

- 在 pristine tip `0ed96bb8e` 的独立 worktree 里跑同一条命令：**15 处 diff / 5 个文件**
  （`pi-tui/src/prompt.rs` 5、`pi-coding-agent/tests/reload_config.rs` 4、
  `pi-tui/tests/truncated_above.rs` 3、`pi-tui/tests/composer_multiline.rs` 2、
  `pi-coding-agent/src/commands/slash.rs` 1，其中 `reload_config.rs` 与 `slash.rs`
  是 LUM-1306 自己新增/改动的文件），`interactive.rs` **0 处**。本轮改动只让 `interactive.rs`
  多出 1 处，已按 1.85 rustfmt 的期望改掉，现在
  `rustfmt --edition 2021 --check crates/pi-coding-agent/src/interactive.rs` EXIT=0。
- **这些 diff 是排版版本差异，不是代码缺陷。** 同一批文件在 1.85 的 rustfmt 1.8.0 下换配置重跑，
  怎么配都复现不出仓库现状：默认 **15** 处、`use_small_heuristics=Max` **55** 处、
  `--edition 2024` 与 `--edition 2021` 完全相同、显式 `single_line_if_else_max_width=100` 仍剩 **14** 处。
  也就是说，仓库现有代码是用**另一个版本的 rustfmt** 排版的（它把单行 `if/else` 留在同一行、
  把长方法链保留在一行），本环境里只有 1.85.0 是完整工具链（`~/.rustup/toolchains/`，1.88.0 只有 clippy 组件）。
- 判断：`scripts/ci.sh` 的 `cargo fmt --all -- --check` 在这台机器上**必然红**，但红的含义是
  「工具链未钉住」，而不是「有人提交了没排版的代码」。LUM-1306 记的「fmt ✅」在它当时记录的
  `cargo+rustc 1.98.1` 环境下可能为真——那套工具链现在**不在这台机器上**，所以无法复核。
  这是**门禁可复现性**的缺陷：仓库里没有 `rust-toolchain.toml`，也没有 `rustfmt.toml`。

### 3.3 `cargo clippy --workspace --all-targets -- -D warnings`：红，11 条既有 warning

不加 `-D` 时全工作区 EXIT=0（2m14s），完整清单（`cargo clippy --workspace --all-targets --locked --message-format short`）：

| 位置 | lint / 原文 | 评估 |
|---|---|---|
| `pi-telemetry/src/memory.rs:228` | `needless_lifetimes` | 机械，`--fix` 可改；**它和下面两条会直接中断 `-D warnings` 的编译** |
| `pi-telemetry/src/noop.rs:37` | `needless_lifetimes` | 同上 |
| `pi-ai/src/utils/deferred_tools.rs:92` | `needless_lifetimes` | 同上 |
| `pi-tui/src/theme.rs:213` | `needless_lifetimes`（`impl<'de> Visitor<'de>`） | 机械 |
| `pi-tui/src/autocomplete.rs:427` | `nonminimal_bool`：建议 `!trimmed.starts_with('/') \|\| trimmed.contains(' ')` | 等价改写，值得顺手做 |
| `pi-tui/src/locale.rs:374` | `this expression always evaluates to false` | 测试里对常量串的**同义断言**（`!EXTENSIONS_DISABLED_ZH.is_empty()`），无害但要复核 |
| `pi-server/src/transports/unix.rs:13` | `duplicated attribute`（`#![cfg(unix)]`） | 冗余属性 |
| `pi-extensions/src/deflate.rs:650` | `precedence`：`(a << 8 \| b) % 31` | Rust 里 `<<` 优先于 `\|`，**语义等价**，纯可读性 |
| `pi-extensions/tests/zlib_deflate.rs:55` | `format!` 拼字符串 | 机械 |
| `pi-coding-agent/tests/reload_config.rs:70,72` | `unnecessary to_path_buf` | 机械（LUM-1306 新增的测试） |

本轮**没有**顺手修这 11 条：其中 `pi-extensions` 正被并发的 LUM-1308 轮次改动（扩展事件），
跨 crate 改会制造合并冲突；且修 lint 需要各自的验证预算。但「门禁红」这件事本身必须落在纸面上——
上两轮把 clippy 记为绿，本环境下**任何 crate 都过不了这条门禁**。

同样要说得准确：这 11 条里绝大多数是 `needless_lifetimes` / `unnecessary to_path_buf` / `precedence`
这类**随 clippy 版本变松变紧**的风格 lint，上一轮在 `1.98.1` 下记绿很可能是真的（新版 clippy 对
`needless_lifetimes` 的判定更聪明）。真正值得人工看两眼的是两条：
`pi-tui/src/locale.rs:374`（对常量串的同义断言，测试里无害但是空断言）与
`pi-extensions/src/deflate.rs:650`（Rust 里 `<<` 优先于 `|`，**语义等价**，纯可读性）。
**结论不是「有人写错了代码」，而是「门禁的绿没有钉住工具链就不算数」**：
仓库没有 `rust-toolchain.toml`，CI 脚本直接用 PATH 上的 `cargo`，而 PATH 上那个 `~/.local/bin/cargo`
是坏的 wrapper（指向不存在的 `/tmp/cargo-home`）——这正是前几轮一度得出「1.85 不可用」误判的同一个坑。

## 4. 完成度复测（每条都附可复现命令）

### 4.1 规模

```bash
find pi-rust/crates -path '*/src/*' -name '*.rs' ! -name 'mod.rs' | xargs wc -l | tail -1   # 129,174
find packages -path '*/src/*' -name '*.ts' ! -name '*.d.ts'   | xargs wc -l | tail -1        # 153,066
```

- src 口径：**129,174 / 153,066 ≈ 84.4%**（上轮 LUM-1306 写 57.5%，分子 183,252 是「含 tests 的全部 `.rs`」，
  分母 319,010 是「TS src + 测试」——**两边口径不同却并列成一行**，是那份文档里最容易误读的数字。
  本节起统一为 src↔src；含 `tests/` 的全部 `.rs` = 183,815 行，TS 含测试 = 293,110 行，该口径 ≈ 62.7%。）
- LOC ≠ 质量：`pi-session`（rusqlite + zstd 落盘）、`pi-protocol`（wire types）、`pi-chord`、
  `pi-telemetry` 是 Rust 侧有、TS 侧没有或更薄的部分。

### 4.2 测试

```bash
grep -rhc '#\[test\]\|#\[tokio::test\]' pi-rust/crates --include=*.rs   # 2,390（标记数）
cargo test --workspace --locked --no-fail-fast                          # 2,457 passed / 0 failed
```

TS 侧 `it(`/`test(` = 5,309 → 用例口径 **45.0%**（上轮 42.0%）。测试规模仍是最大的一块隐性债务。

### 4.3 `app.*` 接线

```bash
python3 pi-rust/scripts/app_action_coverage.py     # wired 35/44 (79.5%)，advertised 2/44，silent 7/44
```

silent 的 7 个：`app.models.{clearAll,enableAll,reorderUp,reorderDown,save,toggleProvider}`、`app.tree.editLabel`。

### 4.4 slash 内置命令：**17 / 23**（对上一轮 20/23 的口径修正）

上游 `packages/coding-agent/src/core/slash-commands.ts` 的 `BUILTIN_SLASH_COMMANDS` 有 23 条。
把 Rust 侧「已接线」清单（`crates/pi-coding-agent/src/commands/slash.rs:1154` 的 `IMPLEMENTED`
数组，它有测试保证「下拉里出现的命令解析器必须认」）与上游名单**逐名**核对：

```bash
sed -n '1154,1175p' pi-rust/crates/pi-coding-agent/src/commands/slash.rs
sed -n '19,44p' packages/coding-agent/src/core/slash-commands.ts
```

- 命中：`settings model tree thinking export copy name session hotkeys fork clone trust new compact resume reload` + `quit`（Rust 里叫 `exit`）= **17**
- 缺 6：`scoped-models import share changelog login logout`
- Rust 独有 3：`help clear extensions`（上游无对应内置，属本移植的补充）
- 上一轮的「20/23」把 Rust 独有的 3 条计入了分子，**分母是上游、分子却是并集**，口径不成立。

### 4.5 加权完成度：本轮**不重算**，只报逐项状态

LUM-1306 的 82.2% 来自一张主观权重表（13 轴），本轮：TUI/设置轴向关闭 1 项
（「启动不读 settings.json」，LUM-1306 §4.3 记录的缺口），其余轴未动。我**没有**给出新的加权总分，
因为那张表的分项分值不在同一份文档里可追溯，硬算出来的小数（82.3/82.4）无法被审阅者复核。
可复核的是本节的 4 个实测口径 + §3 的门禁实况。

## 5. Rust ↔ TS 差距（本轮视角，按影响排序）

| 级别 | 差距 | 证据 |
|---|---|---|
| **P0** | 扩展生命周期事件：声明 7/36、运行期 2/36。上游插件最常用的 `tool_call` / `tool_result` / `turn_*` / `message_*` / `session_shutdown` 在 Rust 下是死变体或缺失 → 直接决定「兼容 pi 插件生态」成败 | LUM-1306 §6；`pi-protocol/src/events.rs`、LUM-1308 正在处理 |
| **P1** | **门禁不可复现**：仓库没有 `rust-toolchain.toml`，而 PATH 上的 `cargo` 是坏 wrapper；换工具链就换结论（本环境 1.85.0 下 `fmt --check` 15 处、`clippy -D warnings` 11 条） | 本轮实测，§3.2 / §3.3 |
| **P1** | TUI 键盘接线：`app.*` 仍有 23 个 action 无全局消费入口（`models.*` 6、`session.*` 6、`tree.*` 11）；用户可感知的是 `/models` 批量启停、会话重命名/删除、树过滤走不到键盘 | `app_action_coverage.py` |
| **P1** | settings.json 只有 2 个键被消费（`theme`、`fullscreenCopyOnSelect`）。上游 settings 面更宽，其余键目前是**静默忽略** | `config.rs::load_ui_settings` |
| **P2** | CLI flag 面：`--theme/--thinking/--tools/--provider/--api-key/--verbose/--offline` 等 22 个 TS flag 无字面等价物（未触及 `cli.rs`，沿用上轮测量） | LUM-1306 §6 |
| **P2** | slash 6 个缺失（§4.4）；会话**写入**侧只保证读上游 sqlite/JSONL 格式 | 同上 |
| **P3** | 测试用例 45.0%（TS 5,309）；`@` 补全首行 label/value 重复；`#` 触发符无 provider | §4.2 / LUM-1306 §6 |
| — | `powershell` 工具（Windows 专属，本平台可不做） | LUM-1306 §6 |

## 6. 未做与限制（诚实条目）

1. **未重算加权总分**（§4.5 已说明理由）。
2. **未修 §3.3 的 11 条 clippy、未重排 §3.2 的 15 处 fmt**：均为既有、跨 crate，且 `pi-extensions`
   正被并发轮次改动，强行改会制造冲突。已给出清单与复现命令，交给下一轮一次性 `--fix`。
3. **只覆盖 2 个 settings 键**：`settings.json` 其它键仍然不被消费，本轮没有扩大范围。
4. **背景不随主题**：实测启动帧背景恒为终端底色（§2.3 注记 2），这是 pi 的既有行为，本轮未改。
5. **证据工具链本身是模拟器**：`pty_capture.py` 用 pyte 仿真 VT 序列再渲染 PNG，字体是 PIL 的位图字体，
   不等于真实终端 + 真实字形；它证明的是「应用发出了哪些字节、这些字节在该 VT 解释下画出什么」。
6. **并发**：同一 workspace 里 LUM-1302…LUM-1308 等多个轮次在做相邻模块（扩展事件、`interactive.rs`），
   本轮**只改 `interactive.rs` 一个文件**（外加新增脚本/场景/文档），以降低与并发分支的冲突面。
7. **issue 重复**：`LUM-1295/1296/1297` 与 `LUM-1299/1300/1301` 分别重复（扩展事件批次 2、
   jump-to-latest 药丸遮挡尾行、HTML 导出提示）。本轮不新建重复子任务，仅在 issue 里报告。

## 7. 下一步建议

1. **P0**：把 `ExtensionEvent` 补齐到上游 36 个事件名并在 `agent_loop` observer 上转发（LUM-1308 进行中，勿重复开条）。
2. **P1**：**先把门禁钉住**——加 `rust-toolchain.toml`（写死本轮验证的 1.85.0，或补上文档里那套
   1.98.1），并让 `scripts/ci.sh` 用工具链里那个 `cargo` 而非 PATH。然后再清 §3.3 的 11 条 clippy
   （其中 3 条会中断 `-D warnings` 编译，优先；`--fix` 可改大部分）。在此之前「门禁绿」不应当作验收依据。
3. **P1**：接线 `models.*`（6）与 `session.*`（6）两组快捷键；顺带修 `@` 补全首行重复。
4. **P2**：把 settings 消费面从 2 个键扩到上游的完整面（至少 `keybindings`/`thinking`/`theme` 的显式覆盖语义），
   并让未消费的键在启动时**报告一次**而不是静默忽略。
5. **P2**：补 4 个高价值 slash（`import`、`share`、`changelog`、`scoped-models`）。

## 8. 复现清单

```bash
export PATH="$HOME/.rustup/toolchains/1.85.0-x86_64-unknown-linux-gnu/bin:$PATH"
cd pi-rust
cargo test --workspace --locked --no-fail-fast                                  # 2457 passed / 0 failed
rustfmt --edition 2021 --check crates/pi-coding-agent/src/interactive.rs        # EXIT=0（本轮改动文件）
cargo clippy --workspace --all-targets --locked --message-format short          # EXIT=0，11 条 warning
cargo clippy --workspace --all-targets --locked -- -D warnings                  # 红（§3.3）
python3 scripts/theme_palette_report.py docs/screenshots/lum1307-startup-ui-before.png docs/screenshots/lum1307-startup-ui-after.png
```

场景文件即上文 §2.1 的两份；`docs/screenshots/lum1307-*.png.txt` 是每个面板的字符网格 dump，
可直接 grep 断言（例如 `grep -n "Fullscreen copy on select" docs/screenshots/lum1307-startup-ui-*.png.txt`）。
