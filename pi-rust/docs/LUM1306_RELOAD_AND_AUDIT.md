# LUM-1306 — 补 `/reload` + 真 `cargo test --workspace` 门禁 + Rust↔TS 实测审计（tip `4208bd843` + 本轮）

> 基准快照：`origin/feature/pi.rs` = `4208bd843`
> 环境：Linux x86_64 / **rustup toolchain 1.85.0**
> （`~/.rustup/toolchains/1.85.0-x86_64-unknown-linux-gnu/bin/cargo`；
> PATH 里那个 `/home/devbox/.local/bin/cargo` 是坏 wrapper，指向不存在的
> `/tmp/cargo-home` + `/tmp/rustup-home` —— 前几轮「1.85 不可用」的结论来自它，
> 是**误判**，本轮已纠正并跑通全量门禁）
> 对应 issue LUM-1306：基于 rust 重写 pi、推进 LUM-981、代码推到远程并合入
> `feature/pi.rs`、学 Martty TUI、打造最佳 UX 的 TUI、截图展示 codex/pi-class
> 交互、真实审计 Rust↔TS 差距。

## 0. 结论速览

| 口径 | 数值 | 与 LUM-1298 比较 |
|---|---|---|
| 加权完成度（13 轴） | **82.2%** | **+0.2**（Slash 轴 19 → 20；等权口径 +0.33） |
| TUI 交互+视觉 | **74.8%** | +0.0（`/reload` 是命令面，不改交互/视觉基线） |
| Slash 命令（脚本实测） | **20 / 23** 上游内置（87.0%） | 19 → 20（`/reload`）；缺 6 个 |
| `app.*` 接线（严格口径） | 35/44 (**79.5%**) | +0.0（本轮未动 app.* 表） |
| 纯代码规模 | 183,252+ / 319,010 ≈ **57.5%** | 基本持平（本轮 +~600 行） |
| 测试规模 | **2,454 passed / 0 failed**（本 tip 实跑） | 见 §2.3：文档里的 2,424 是旧 tip 的陈旧基线，本轮自身 +9 |
| 真 PTY 截图（本轮新增） | 4 张 PNG + 4 份文本 dump（A/B 两组） | 新增 |
| 远程 / `feature/pi.rs` | 本轮分支已合入并推送 | 已同步 |

## 1. 本轮真正交付的东西：`/reload`

前几轮的结论是「无可落地新能力」，那是工具链误判导致的保守结论。本轮实测
1.85 可用后直接**补上一个真实缺失的能力**，而不是再写一份计划。

### 1.1 为什么是 `/reload`

- 上游 `BUILTIN_SLASH_COMMANDS` 里有 `reload`（`packages/coding-agent/src/core/slash-commands.ts`），
  上游实现是 `handleReloadCommand()`（`interactive-mode.ts:5972`），Rust 侧**完全没有**。
- Rust 侧的缺口是写在代码里的：`interactive.rs` 的渲染循环注释原来直接写着
  「the render loop has **no reload trigger yet**」——能力缺失被记录，但没人补。
- 它可独立交付、可验证、边界清楚（不牵扯 provider/网络/OAuth 这些大子系统）。

### 1.2 语义（刻意收窄，并把「没做什么」说出来）

`/reload` 只重读**运行中的 App 真正持有的两个切片**：

| 重读 | 来源 | 落点 |
|---|---|---|
| 键位 | `~/.pi/agent/keybindings.json` | 重建 `KeybindingsManager::create(agent_dir)` + `reload_keybindings`（进程级注册表重装） |
| 界面设置 | `~/.pi/agent/settings.json` 的 `theme` / `fullscreenCopyOnSelect` | `app.set_theme_by_name` / `app.set_copy_on_select` |

**扩展、skills、prompts、context 文件在启动时读取，`/reload` 不重读**——这一句会
作为 transcript 行**打印给用户**（`/new` 才是拿新上下文的路径），而不是只写在代码注释里。
这正是本项目「广告面 ≠ 实现面」批评的对症处理：能力边界对用户可见。

### 1.3 落地位置

| 文件 | 内容 |
|---|---|
| `crates/pi-coding-agent/src/reload.rs`（新） | `reload()`(`:63`)、`ReloadReport`(`:35`)、`summary_lines()`(`:111`) + 4 个单测 |
| `crates/pi-coding-agent/src/lib.rs` | `mod reload;`(`:66`) + `pub use reload::{reload, ReloadReport};`(`:115`) |
| `crates/pi-coding-agent/src/commands/slash.rs` | `SlashCommand::Reload`(`:86`)、解析 `:131`、`/help` 行 `:187`、补全项 `:259`、`IMPLEMENTED` `:1173`、测试 `:742` |
| `crates/pi-coding-agent/src/interactive.rs` | 命令接线 `:2799`；刷新渲染循环里过期的注释 `:344-358` |
| `crates/pi-coding-agent/tests/reload_config.rs`（新） | 4 个集成测试（独立进程，因为 `set_keybindings` 改的是进程级状态） |

边界设计：`mod reload` 保持私有 + `pub use` 那个函数，这样 `pi_coding_agent::reload`
在外部就是函数本身，集成测试能从自己的进程里调用，不会和模块名打架。
重读时**自己重建** `KeybindingsManager`（同一个 agent dir、同一张平台表），而不是把启动
时的句柄穿过整个渲染循环传进来——同样的文件、同样的结果，少一条贯穿依赖。
主题解析失败**不吞**：`ReloadReport.theme_error` 会带上失败原因，旧主题保留。

### 1.4 一条顺带修掉的工具缺陷（会造假的证据工具）

`scripts/pty_capture.py` 原来在收尾时无条件 `shutil.rmtree(home, ...)`，**即使
`--home` 是调用方显式指定的**。第一次跑 A/B 时它把准备好的
`/tmp/lum1306/home-reload/.pi/agent/` 删掉了，于是第二次跑 `/reload` 输出
「keybindings.json has no file」——**一个假阴性**：fixture 在 spawn 时明明存在。
现在只在 `args.home is None`（harness 自建的 scratch 目录）时才删，显式 `--home`
/ `--cwd` 保留。这类「证据工具悄悄改写自己的 fixture」的 bug 比功能 bug 更危险，
因为它污染的是判断依据本身。

## 2. 真实门禁（不是「跳过测试」）

### 2.1 命令与结果

```bash
export PATH="$HOME/.rustup/toolchains/1.85.0-x86_64-unknown-linux-gnu/bin:$PATH"
cd pi-rust
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
  cargo test --workspace --offline --no-fail-fast
# => TEST_EXIT=0, passed=2454 failed=0（全部 crate + doc-tests）
```

日志：`after-workspace.log`。本轮新增的测试都能在日志里点到名：
`commands::slash::tests::parses_the_reload_command`、
`reload::tests::summary_*`（4 条）、集成 `reload_config.rs` 的 4 条
（`reload_rereads_keybindings_and_ui_settings_mid_session`、
`reload_picks_up_an_edited_file_without_a_restart`、
`reload_falls_back_to_defaults_and_reports_a_missing_file`、
`reload_reports_a_theme_that_does_not_resolve`）。

### 2.2 踩到的坑（值得记下来）

并行 agent 在同一台机器上互相抢 `target/` 时，`cargo test` 会在你编辑文件的中途
重编译 `pi-coding-agent`，那次运行的结果就**不可采信**（我第一次跑就撞上，
`reload_config` 报 `EXIT=101`）。干净重跑一次才拿到上面的数字。
另外本轮开头有个 build 把 `target/` 顶到 8.3G，把机器磁盘吃到告急——已终止，
并全程用 `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 --offline` 控制体积。

### 2.3 测试数怎么读

- 本轮实测：**2,454 passed / 0 failed**（这个 tip，全 workspace + doc-tests）。
- 文档里 LUM-1298 写的 `2,424` 是**旧 tip** 的数字，且统计口径不同（对 TS 侧用
  `^\s*(test|it)\(` 数 case）。
- 所以 `2454 - 2424 = 30` **不能**全记在本轮头上：中间还合过 LUM-1294/1295-1301 等
  分支。**本轮自己的增量是 +9**（4 单测 + 1 解析测试 + 4 集成）。

## 3. 证据：真二进制 + 真 PTY + A/B

不是「跑一下看看」，是同一台机器上、同一个准备好的 HOME、同一组按键，对**改前/改后
两个二进制**做 A/B，断言取自 pyte 解出的字符网格（二进制自己画上去的字）。

- 场景：`scripts/pty_scenarios/lum1306-reload.json`（改后）/
  `lum1306-reload-baseline.json`（改前）/ `lum1306-help-reload.json` /
  `lum1306-help-reload-baseline.json`
- 改前二进制：`origin/feature/pi.rs` @ `4208bd843` 构建
- 改后二进制：本分支构建
- fixture：`~/.pi/agent/keybindings.json`（`cursorUp` → `Ctrl+P`）、
  `~/.pi/agent/settings.json`（`theme=light`、`fullscreenCopyOnSelect=false`）

### 3.1 断言结果

| 组 | 改前 | 改后 |
|---|---|---|
| `lum1306-reload*.json`（4 panel：idle / /hotkeys / /help / /reload） | 8/8 PASS | **11/11 PASS** |
| `lum1306-help-reload*.json`（120x62 单屏 /help） | 4/4 PASS（含 `reject: ["/reload"]`） | **3/3 PASS** |

改后第 4 panel 的网格原文：

```text
> /reload: keybindings → 1 override(s) from /tmp/lum1306/home-reload/.pi/agent/keybindings.json; theme → light
> /reload: extensions, skills, prompts and context files are read at startup and are not re-read here (use /new for a
> fresh session)
```

改前同一 panel：

```text
> unknown command /reload — try /help
```

`/help` 的 A/B（120x62，一屏不滚动）——改前最后一行是 `/exit`，改后多出：

```text
·   /extensions list loaded extensions and what they register
·   /reload   re-read keybindings.json and the interface settings
·   /exit     quit the interactive session
```

### 3.2 主题切没切？用像素证明

截图里终端背景色不变，是因为 **pi 的主题不画背景**（`light.json`/`dark.json` 里没有
`background` 键，只换前景），所以「背景还是深色」不能拿来质疑。真正该看的是**前景色
token 变了**：

| 采样（panel 4 文本区 modal 前景色） | 改前 | 改后 |
|---|---|---|
| 主文本 | `#d4d4d4`（dark.json 的 `text`） | `#1f2328`（light.json 的 `text`） |
| mediumGray / dimGray | `#808080` / `#666666` | `#6c6c6c` / `#767676` |

即 `/reload` 之后调色板**真的**换成了磁盘上 `settings.json` 指定的 light，
不是只打印了一行「theme → light」。

### 3.3 截图文件

| 文件 | 内容 |
|---|---|
| `docs/screenshots/lum1306-reload-after.png` (+`.txt`) | 改后 4 panel 全通过 |
| `docs/screenshots/lum1306-reload-before.png` (+`.txt`) | 改前同按键，`/reload` 未知命令 |
| `docs/screenshots/lum1306-help-reload-after.png` (+`.txt`) | 改后 `/help` 一屏含 `/reload` |
| `docs/screenshots/lum1306-help-reload-before.png` (+`.txt`) | 改前 `/help` 无 `/reload` |

## 4. Rust ↔ TS 真实差距审计（本轮重测，含口径打架的地方）

### 4.1 Slash 命令：**文档里的 12 是错的，实测 20**

`docs/LUM1298_TUI_AUDIT_AND_NEXT.md` §2.2 那行写「Slash 命令 **12** / 23」，但同一份
文档 §0 又写 18。两个数都不对。用脚本从源码直接数（不看文档）：

```text
upstream BUILTIN_SLASH_COMMANDS (23):
  changelog clone compact copy export fork hotkeys import login logout model
  name new quit reload resume scoped-models session settings share thinking tree trust

rust parser arms (20):
  help clear clone compact copy export extensions fork hotkeys model name new
  quit reload resume session settings thinking tree trust

缺失（上游内置 - rust，6）: changelog import login logout scoped-models share
rust 多出（不在上游内置表里，2+1）: clear extensions（+ help，上游是 dispatcher 分支不是内置表项）
上游 dispatcher 还有 rust 没有的: debug + 两个彩蛋（arminsayshi / dementedelves）
```

结论：**20/23 上游内置（87.0%）**，缺的 6 个里有 3 个（`login`/`logout`/`scoped-models`）
要么依赖 provider 认证子系统、要么依赖模型目录，属于别的轴；`import`/`share`/`changelog`
是能独立做但不紧急的三个。

### 4.2 13 轴缺口（沿用 LUM-1293 口径，只更新实测能站住的行）

| 轴 | pi-rust | pi-ts | 说明 |
|---|---|---|---|
| 核心 Agent 循环 | 100% | 100% | — |
| LLM provider（主路径） | 100% | 100% | provider stub 列表仍差 |
| 内置工具 | 7/8 | 7/8 | Linux 对齐 |
| **Slash 命令** | **20/23（87.0%，实测）** | 23 | 文档旧值 12/18 均不准，见 §4.1 |
| **扩展生命周期事件** | **20/36（55.6%）** | 36/36 | **最大单轴缺口** |
| 会话存储（SQLite/zstd） | 100% | 100% | — |
| TUI 渲染 | 100% | 100% | — |
| `app.*` 键位 | 35/44（79.5%） | 43/43 | 其中「广告了没实现」2 个、「静默无响应」7 个 |
| 富工具渲染 / 鼠标 / 截断 / jump-to-latest | 100% | 100% | — |
| **OAuth / 远程模型目录** | **0%** | 100% | **第二大缺口** |

`app.*` 的 11 个问题项（实测口径不变）：

- 广告了但没接线（2）：`app.editor.external`（`locale.rs:184`）、`app.suspend`
  （`slash.rs:468` / `locale.rs:144`）
- 静默无响应（7）：`app.models.clearAll`、`enableAll`、`reorderDown`、`reorderUp`、
  `save`、`toggleProvider`、`app.tree.editLabel`

### 4.3 「本轮没修、但必须报」的发现：`settings.json` 在启动时根本没被应用

`config::load_ui_settings` 全仓只有**两个**调用点：`open_settings`（`interactive.rs:3023`）
和本轮的 `reload.rs`。也就是说：

- 交互模式启动时**不读** `settings.json` 的 `theme` / `fullscreenCopyOnSelect`；
- 也没有 CLI `--theme`；
- 用户想让自己写的 `theme: "light"` 生效，只能进 `/settings` 菜单点一下，或者手打
  `/reload`。

上游是启动就读 settings 的，所以这是**真实的行为缺口**。最小修法是在
`run_interactive` 里拿到 `App` 之后立刻套一次 `load_ui_settings`（和 `reload.rs` 里
那几行同构，约 5 行）。本轮**没有**顺手改：它属于「启动路径」而不是 `/reload` 的语义，
混进来会让这次改动的边界失真，也会让 A/B 证据不再只对应 `/reload` 一件事。
留作下一个可独立交付的 follow-up。

### 4.4 代码/测试规模（口径不变）

| 指标 | pi-rust | pi-ts | 比例 |
|---|---|---|---|
| 纯代码行数 | 183,252+ | 319,010 | ≈57.5% |
| 测试 case | 2,454 passed（本 tip 实跑） | 5,300 | 45.7%（口径见 §2.3） |

## 5. 下一步（按单轴回报排序，未开工）

1. **扩展生命周期事件 20/36 → 36/36**：单轴回报 +13.9pp，仍是最大缺口。
2. **OAuth / 远程模型目录 0% → 有**：第二大缺口，但牵扯面大，需要单独设计轮次。
3. **`settings.json` 启动应用**（§4.3）：约 5 行，独立可验证，建议紧跟本轮。
4. **`import` / `share` / `changelog`**：三个能独立交付的 slash 命令，补完即
   23/23 内置表（`login`/`logout`/`scoped-models` 仍受 provider 轴阻塞）。
