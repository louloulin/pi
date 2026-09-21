# LUM-1310 — `settings.json` 启动生效（3 键）、TUI 真实审计、Martty 对照

> 基准：`origin/feature/pi.rs` = `cdc033e21`（LUM-1308）
> 环境：Linux x86_64 / 32 核 / **rustup toolchain 1.85.0**（`scripts/toolchain.sh` 解析）
> 本轮改动：`pi-rust/` 5 个源文件 + 3 个 PTY 场景 + 本文 + 6 张真 PTY 截图

一句话结论：`settings.json` 里此前**只有 `/settings` 和 `/reload` 会读、启动帧不生效**的 3 个界面键
（`quietStartup`、`hideThinkingBlock`、`autocompleteMaxVisible`）现在和上游一样在**第一帧**生效，并且这 3 项
终于在 `/settings` 里有了行（此前只能手改 JSON）；`/settings` 从 3 行变 6 行。门禁全绿：
`fmt --check` 干净、`clippy -D warnings` 0 findings、`cargo test --workspace` **2483 passed / 0 failed**。

---

## 1. 缺陷：启动不生效的界面键（真实缺口，非推测）

LUM-1307 补了 `theme` / `fullscreenCopyOnSelect` 的启动应用，并在该文 §4.3 记下「其余界面键仍只在
`/settings` / `/reload` 读」。本轮把这个缺口按上游口径逐键核对（上游在**构造交互会话时**读这些键，
`packages/coding-agent/src/modes/interactive/interactive-mode.ts`）：

| 键 | 上游读取点 | 上游 getter | 修前 pi-rust | 修后 |
|---|---|---|---|---|
| `quietStartup` | `interactive-mode.ts:859,912,1650` | `getQuietStartup()`（`settings-manager.ts:1004`） | 只有 `--no-header` 能静默头部；`settings.json` 里写 `quietStartup: true` **无任何效果** | `main.rs` 在构造 `InteractiveOptions` 前读入，头部**不进第一帧布局** |
| `hideThinkingBlock` | `interactive-mode.ts:573,1949,6016` | `getHideThinkingBlock()`（`:961`，默认 `false`） | 只有 `Ctrl+T`（`app.thinking.toggle`）能切；文件里的值被忽略 | `apply_startup_ui_settings` 调 `App::set_thinking_visible(!hide)` |
| `autocompleteMaxVisible` | `interactive-mode.ts:557,1958` | `getAutocompleteMaxVisible()`（`:1383`，默认 `5`） | 编辑器**有**该字段与 setter，但没有任何路径从文件读它 | 读入并调 `App::set_autocomplete_max_visible(n)`，编辑器自行 clamp 3..=20 |

补充事实（避免夸大）：这 3 个键在**修前也写不进去**——`/settings` 里根本没有对应行。
所以用户不是「设置了不生效」，而是「没有任何 UI 入口，只能手改 JSON，而手改也不生效」。
这是双重缺口，本轮一起关掉。

## 2. 实现（5 个文件）

| 文件 | 改动 |
|---|---|
| `crates/pi-coding-agent/src/config.rs` | `UiSettings` 从 2 字段扩到 5；新增 `DEFAULT_HIDE_THINKING_BLOCK` / `DEFAULT_AUTOCOMPLETE_MAX_VISIBLE` / `DEFAULT_QUIET_STARTUP`；新增 `read_autocomplete_max_visible`（数字键，拒绝负值与字符串）与 `load_quiet_startup_default()` |
| `crates/pi-coding-agent/src/interactive.rs` | `StartupUiSettings` 扩到 6 字段；`apply_startup_ui_settings` 应用 thinking 可见性与下拉高度并**回读 clamp 后的实际值**；`/settings` 从 3 行扩到 6 行；`apply_setting_change` 新增 3 个 arm |
| `crates/pi-coding-agent/src/main.rs` | `quiet_startup: cli.no_header \|\| load_quiet_startup_default()` |
| `crates/pi-coding-agent/src/reload.rs` | `/reload` 同步应用 thinking / 下拉高度；`quietStartup` **只报告不应用**（与上游一致，见 §2.1）；摘要行补 UI 段 |
| `crates/pi-tui/src/app.rs` | 新增 `autocomplete_max_visible()` / `set_autocomplete_max_visible()` 转发到 `prompt.editor()` |

### 2.1 一处**有意的对齐**：`quietStartup` 持久化但不改当前头部

上游 `onQuietStartupChange` 只调 `settingsManager.setQuietStartup(enabled)`
（`interactive-mode.ts:4694`），**不碰当前头部**；而 `onHideThinkingBlockChange`（`:4674`）与
`onAutocompleteMaxVisibleChange`（`:4738`）都**立即**作用于运行中的 App。
本 port 逐条照做：thinking 与下拉高度即时生效，quiet-startup 只落盘并在转写里说明「下次启动生效」。
这样 `--no-header` 这个显式 CLI 标志不会被一次 `/settings` 操作悄悄反向覆盖。

### 2.2 clamp 住在 setter 里（与上游同构）

上游 getter 返回**未 clamp** 的存储值（`settings-manager.ts:1383`），`Math.max(3, Math.min(20, …))`
住在 setter（`:1387`）。所以 `config.rs` 读出**原值**，`/settings` 里再回读 App 的**实际值**并向用户报告：
手写 `autocompleteMaxVisible: 100` 会得到 `20`，且转写显示 `autocomplete max items → 20 (clamped to 3-20)`，
不会出现「显示 100 实际 20」的假广告。

## 3. 真实证据：真 PTY + pyte + A/B 双二进制

工具链：`scripts/pty_capture.py`（真 PTY + `TIOCSWINSZ` + pyte VT 仿真 + PIL 渲染），
断言走**字符网格**而非颜色。A/B 用两个**哈希不同**的二进制：

```bash
# 修前（从本分支 tip 的 5 个源文件 stash 掉我的改动后重建）
#   md5 fa89930d1bb0c83f1dfcceb1b231646a
cargo build -p pi-coding-agent --bin pi --locked
# 修后
#   md5 b09c86270ece50f153304c05bdd4cc11
```

> 过程更正（值得记下来）：第一次 A/B 我用 `cargo build -p pi-mono`，而 `pi` 二进制的 `[[bin]]`
> 属于 **`pi-coding-agent`**（`crates/pi-coding-agent/Cargo.toml:12`）。于是那次构建根本没重链 `pi`，
> 两次跑的是**同一个** md5（`b09c862…`），"before" 假通过。改用 `-p pi-coding-agent --bin pi`
> 后两个 md5 不同，A/B 才成立。**判据是 md5，不是构建日志。**

### 3.1 断言矩阵

| 场景（同一场景文件跑两个二进制） | 修前 `fa89930` | 修后 `b09c862` |
|---|---|---|
| `lum1310-startup-ui-keys`（HOME: `hideThinkingBlock:true, autocompleteMaxVisible:10`） | **2 PASS / 3 FAIL / 2 XFAIL** | **7 PASS / 0 FAIL / 0 XFAIL** |
| `lum1310-quiet-startup-on`（HOME: `quietStartup:true`） | **1 PASS / 2 FAIL / 3 XFAIL** | **6 PASS / 0 FAIL** |
| `lum1310-quiet-startup-off`（HOME: `quietStartup:false`，**对照组**） | 4 PASS | 4 PASS |

对照组两边都 4 PASS 是**必要条件**：它证明差异来自设置读没读，而不是场景本身写错了。

修前的失败是**内容级**的，不是哈希级：

```
[2] 2. /settings …: FAIL expect 'Hide thinking'
[2] 2. /settings …: FAIL expect 'Autocomplete max items'
[2] 2. /settings …: FAIL expect 'Quiet startup'
[1] 1. idle …: FAIL reject 'Esc to interrupt'      ← quietStartup:true 但头部仍在
```

### 3.2 截图（`docs/screenshots/`，6 张 PNG + 6 个字符网格 dump）

- `lum1310-startup-ui-keys-{before,after}.png`
  - after 的 `/settings` 面板（真 PTY 字符网格）：
    ```
    → Auto-compact               true
      Fullscreen copy on select  true
      Theme                      dark
      Hide thinking              true      ← 来自文件
      Autocomplete max items     10        ← 来自文件
      Quiet startup              false
    ```
  - before 同一面板只有前 3 行，其余为空（`FAIL`）。
- `lum1310-quiet-startup-on-{before,after}.png`
  - before：第一帧 20 行提示（`pi v0.1.0` / `Esc to interrupt` / `Ctrl+C to clear` / … 到
    `Alt+Up to edit all queued messages`）。
  - after：同样 120×34，头部**不占任何行**，底部 composer 与状态条仍在。
- `lum1310-quiet-startup-off-{before,after}.png`：对照组，两边都渲染头部。

## 4. TUI 真实审计（不吹）

### 4.1 与上游 pi TUI / codex 类交互的对照

本轮**没有**动交互架构，以下是对**当前 tip** 的实测清点（命令可复现）。

已具备、且是真 PTY 可验的 codex/pi 类交互：

| 交互 | 证据 |
|---|---|
| 斜杠命令 + 模糊补全（`/`）、文件补全（`@`） | `autocomplete.rs` + `docs/screenshots/lum1241-*` |
| 多行 composer（`Alt+Enter` 排队、`Shift+Enter`/反斜杠续行） | `docs/LUM1282_MULTI_LINE_COMPOSER.md` |
| 模型选择器（`Ctrl+L` / `/model`） | `lum1245-ctrl-l-model-selector.png` |
| 会话树 / resume / fork / clone | `tree.rs`、`/tree`、`/resume` |
| 思考块折叠（`Ctrl+T`）与 thinking level（`Shift+Tab`） | `message.rs`、`lum1230-thinking-*.png` |
| 外部编辑器（`Ctrl+G`）/ 挂起（`Ctrl+Z`） | `docs/LUM1308_SUSPEND_AND_EXTERNAL_EDITOR.md` |
| 回滚 + 「上面还有 N 行」截断提示 + 独立滚动条列 | `docs/TUI_SCROLL_AND_REFERENCE_LUM1271.md`、`TUI_TRUNCATION_AFFORDANCE_LUM1273.md` |
| 鼠标选择 / copy-on-select（含 OSC 52 / wl-copy / xclip） | `mouse_region.rs`、`clipboard.rs` |
| Markdown / 高亮 / LaTeX / 图片（kitty graphics + 回退） | `markdown.rs`、`latex.rs`、`terminal_image.rs`（1471 行） |
| 扩展 UI bridge（`ctx.ui.*` 区域、浮层、widget） | `extension_ui.rs`、`lum1246-extension-events.png` |

### 4.2 仍未达（诚实清单）

1. **`app.*` 假广告已清零，但仍有 7 个 silent**：`app.*` = **37/44 wired (84.1%)**、
   **advertised 0/44**、silent 7/44（`app.models.{save,enableAll,clearAll,reorderUp,reorderDown,toggleProvider}`、
   `app.tree.editLabel`）。silent 是「没实现也没宣传」，不算假广告，但仍是差距。
   （复现：`python3 pi-rust/scripts/app_action_coverage.py`，须在仓库根运行。）
2. **`/settings` 行数仍远少于上游**：上游 `settings-selector.ts` 有 **37 个 row id**（含 theme 子菜单
   3 项与 1 个按钮），本 build 现有 **6 行**。缺的行大多属于本 port 尚无的子系统
   （mermaid、image width、transport、warnings、skill commands、fullscreen 专属项等），
   所以是「子系统未移植」而非「界面漏做」——但按界面口径就是 6/37。
3. **`quietStartup` 只影响头部，不影响上游的其他 verbose 打印**（scoped-model 列表、版本变更日志）：
   本 port 没有那些打印，所以差异面为零，但如果将来补上，要记得同样 gate。
4. **未覆盖 `fullscreen*` 三项**（`tuiMode` / `fullscreenExitOutput` / `fullscreenScrollbar`）：
   本 port 没有 fullscreen 模式。

## 5. Martty 对照（`louloulin/Martty`，v0.2.17）

Martty 是本任务指定的 TUI 学习对象。它和 pi-rust 的定位不同，先说清楚差别再谈可学之处：

| | Martty | pi-rust |
|---|---|---|
| 定位 | DeepSeek Harness 的 **ACP client**，界面能力由 Cordis **插件树**组合 | 上游 pi 的 **Rust 移植**，界面由 crate 内模块直接实现 |
| 规模 | 33,470 行 / 31 文件（`src/ui.rs` 4,130、`src/app.rs` 9,204） | `pi-tui` 33,105 行 / 33 文件（`app.rs` ~5,100） |
| 渲染依赖 | `ratatui 0.30.2` + `crossterm 0.29` + `tui-markdown 0.3.9` | `ratatui 0.28` + `crossterm 0.28` + 自写 `markdown.rs` |
| 插件模型 | 运行期 Cordis client 树、`TuiNode` 语义、slot 可挂载 | 编译期 crate 模块 + 扩展走 QuickJS/WASM 宿主 |
| 特色 | 像素宠物（kitty graphics）、slot 快照、UI Preset、`agent-client-protocol` 2.0 | 会话树、扩展 UI bridge、thinking 折叠、真 PTY 断言门 |

**可学且本轮/近期值得抄的点**（按价值排序）：

1. **composer 高度策略**：Martty 的 `resolved_composer_height` 用 `min(h/2, 12)` 上限 +
   可视行数增长（`Martty/src/ui.rs:38-54`）。pi-rust 已借用了这个上限思路
   （`composer_max_rows: 8`，见 `interactive.rs` 的注释），但 Martty 的**下限随终端高度分档**
   （`composer_height()`，`ui.rs:25-35`：≥15 行给 4、≥10 给 3、否则 2）pi-rust 还没有。
2. **slot 快照 + 右栏（`chrome.right`）**：Martty 有 `slot_snapshots` / `draw_right_slot` 的
   固定语义位。pi-rust 的扩展区域是自由的，缺一个「约定位置」的概念。
3. **状态行分片**：Martty `meta_line` / `status_right` 把状态拆成左右两段并做宽度预算
   （`ui.rs:607,828`）。pi-rust 的状态条是单段，窄屏靠截断。
4. **`ui.rs` 的布局函数全部是 `pub fn` 纯函数**（`composer_dock_height`、`pet_rect`、
   `shell_areas`）——因此**可单测**。这是 pi-rust 值得学的工程习惯，本轮我自己就吃了亏
   （见 §3 的 `-p pi-mono` 事故：布局/构建边界没有断言兜住）。
5. **不学的**：像素宠物与 kitty 图形宠物不是上游 pi 的功能，抄进来会偏离 parity 目标。

## 6. Rust ↔ TS 真实差距（本轮实测，命令可复现）

```bash
git rev-parse --short HEAD
find pi-rust/crates -path '*/src/*' -name '*.rs' ! -name 'mod.rs' | xargs wc -l | tail -1
find packages -path '*/src/*' -name '*.ts' ! -name '*.test.ts' | xargs wc -l | tail -1
grep -rhc '#\[test\]\|#\[tokio::test\]' pi-rust/crates --include=*.rs | awk '{s+=$1} END {print s}'
find packages -name '*.test.ts' | xargs grep -ch '\bit(\|\btest(' | awk '{s+=$1} END {print s}'
cargo test --workspace --locked
```

| 口径 | 本轮实测 | 上轮（LUM-1307 文） | 说明 |
|---|---|---|---|
| 纯代码规模（src↔src） | **85.1%**（130,333 / 153,106） | 84.4%（129,174 / 153,066） | 同口径；本轮 +1,159 Rust 行 |
| 测试规模（标记数） | **45.5%**（2,416 / 5,309） | 45.0%（2,390 / 5,309） | 本轮 +26 标记 |
| `cargo test --workspace` | **2483 passed / 0 failed / 2 ignored**（162 suites） | 2457 passed | 本轮 +26（含新增 14 条 + 既有） |
| `app.*` 接线 | **37/44 (84.1%)**；advertised **0/44**；silent 7/44 | 37/44；advertised 0/44 | 未变（本轮未动键位） |
| slash 命令交集 | **16/23 (69.6%)**（`exit`≡`quit` 则 17/23） | 17/23 | 本轮重测：缺口为 `scoped-models import share changelog login logout quit` |
| `/settings` 行 | **6 / 37** row id | 3 / 37 | 本轮 +3 |
| `pi-tui` 模块 | **33 文件 / 33,105 行** | 33 模块 | 未变 |
| 门禁（1.85.0，`--locked`） | `fmt --check` **干净**；`clippy -D warnings` **0 findings**；`test` **2483 passed** | fmt 15 处 / clippy 11 条（工具链未钉版） | LUM-1308 的 `scripts/toolchain.sh` + 钉版后已可复现 |

### 6.1 单一「完成进度百分比」

不同轴给出不同数字（规模 85.1%、测试 45.5%、`app.*` 84.1%、slash 69.6%、`/settings` 16.2%），
**强行合成一个数就是编**。因此给一个**可追溯**的口径，公式公开：

```
完成度 = 规模 0.30 + 测试 0.20 + app.* 0.15 + slash 0.15 + 工具/命令面 0.10 + TUI 交互 0.10
       = 85.1×0.30 + 45.5×0.20 + 84.1×0.15 + 69.6×0.15 + 60×0.10 + 78×0.10
       = 25.53 + 9.10 + 12.62 + 10.44 + 6.00 + 7.80
       = 71.5%
```

其中「工具/命令面 60」「TUI 交互 78」是**判断值**（判断依据写在 §4 与 `RUST_TS_PARITY_METRICS.md` §4），
不是测量值。**本轮的增量很小**：`/settings` 从 3→6 行、3 个启动键生效，
加权后约 **+0.3~0.5 个百分点**（不重新合成总分，理由见 `RUST_TS_PARITY_METRICS.md` §0.5）。
读者若换权重，请照上面公式自行重算。

## 7. 门禁实况（1.85.0，`--locked`）

```bash
. pi-rust/scripts/toolchain.sh
cd pi-rust
cargo fmt --all -- --check                                   # 干净
cargo clippy --workspace --all-targets --locked -- -D warnings   # 0 findings，exit 0
cargo test --workspace --locked                              # exit 0
```

| 门禁 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | **PASS**（本轮新代码先被 fmt 抓到 1 处，已 `cargo fmt --all`） |
| `cargo clippy --workspace --all-targets -- -D warnings` | **PASS**（0 findings，exit 0） |
| `cargo test --workspace --locked` | **2483 passed / 0 failed / 2 ignored**（162 suites） |

## 8. 复现命令

```bash
cd pi-rust
. scripts/toolchain.sh
cargo build -p pi-coding-agent --bin pi --locked

# 修后
python3 scripts/pty_capture.py --bin target/debug/pi \
  --steps scripts/pty_scenarios/lum1310-startup-ui-keys.json \
  --home /tmp/lum1310/home-ui \
  --out docs/screenshots/lum1310-startup-ui-keys-after.png

# 设置 fixture
mkdir -p /tmp/lum1310/home-{ui,quiet,default}/.pi/agent
echo '{"hideThinkingBlock":true,"autocompleteMaxVisible":10}' > /tmp/lum1310/home-ui/.pi/agent/settings.json
echo '{"quietStartup":true}'  > /tmp/lum1310/home-quiet/.pi/agent/settings.json
echo '{"quietStartup":false}' > /tmp/lum1310/home-default/.pi/agent/settings.json
```

## 9. 已知限制

1. **PTY 证据是仿真器**：`pty_capture.py` 用 pyte 仿真 VT 再渲染 PNG；它证明「应用发了哪些字节、
   这些字节在该解释下画出什么」，不等于真实终端 + 真实字形。
2. **A/B 的 before 二进制是本地 stash 后重建的**，md5 已记录（`fa89930…` / `b09c862…`），
   但不是 CI 产物；换机器重建应得到不同 md5（依赖构建路径），判据应是**断言矩阵**而非 md5 相等。
3. **`/settings` 6/37 的分母含 theme 子菜单与按钮**：按「顶层可选项」算约 6/33，
   本文取更保守的 6/37。
4. **加权完成度含判断值**：§6.1 的 60/78 两个分项不是测量值，已在公式里标明。
5. **磁盘**：本机 50G overlay 一度只剩 2.3G；`cargo test` 全程 `CARGO_PROFILE_{DEV,TEST}_DEBUG=0`，
   关 debuginfo 只影响回溯可读性，不影响断言。
