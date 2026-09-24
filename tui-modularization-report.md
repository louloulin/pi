# pi-rust TUI 全面分析、模块化与对标报告

> 范围：`pi-rust/crates/pi-tui`、`pi-rust/crates/pi-coding-agent` 的 TUI 子系统、`pi-rust/Martty`、`nanopi/`（Rust 独立项目）、`packages/tui`、`packages/coding-agent` 的交互模式。
> 方法：直接读取四套实现的源码、模块表与对外 API，结合 `tui-analysis.md` / `tui-plan.md` 的既有结论做交叉验证。`nanopi/` 与 `packages/tui` 是**两套不同的实现**，不合并看待。
> 目标：定位问题、对标设计、给出可执行的模块化重构方案。

## 0. 四套实现一览

| 实现 | 目录 | 总行数 | 主要文件 / 入口 |
| --- | --- | ---: | --- |
| **pi-rust TUI** | `pi-rust/crates/pi-tui/src/` | **42,832 L** (38 文件) | `app.rs` 7,681 L、`editor.rs` 4,515 L |
| pi-rust 驱动 | `pi-rust/crates/pi-coding-agent/src/` | **27,030 L** (顶层 28 文件) | `interactive.rs` **9,432 L** |
| **Martty (Rust)** | `pi-rust/Martty/src/` | **29,913 L** (顶层 28 文件) | `app.rs` 9,783 L、`ui.rs` 4,217 L |
| **nanopi (Rust 独立项目)** | `nanopi/src/` | 49,065 L (83 文件) | `mode/tui.rs` 6,999 L；**TUI 部分仅 ≈ 12,072 L** |
| nanopi TUI 层 (Rust) | `nanopi/src/render/` | 4,406 L (11 模块) | `text_buffer.rs` 1,085 L、`stdout.rs` 834 L、`menu.rs` 435 L |
| **pi-tui (TypeScript 上游)** | `packages/tui/src/` | **18,653 L** (42 文件) | `tui-alt-screen.ts` 1,721 L、`tui.ts` 1,456 L、`keys.ts` 1,401 L、`utils.ts` 1,337 L |
| pi-tui editor (TS) | `packages/tui/src/components/` | 6,276 L | `editor.ts` 2,461 L、`markdown.ts` 1,015 L、`input.ts` 1,025 L |
| pi 交互模式 (TS) | `packages/coding-agent/src/modes/interactive/` | 7,069 L (顶层 7 文件) | `interactive-mode.ts` 6,620 L |

观察：

1. **pi-rust TUI 组件库的规模是 TypeScript pi-tui（18,653 L）的 2.3 倍、是 Martty（29,913 L）的 1.4 倍**；最关键的对比是 `interactive.rs`（9,432 L）和 `app.rs`（7,681 L）——**两个文件合计 17,113 L**，跨越两个 crate 的"巨型文件簇"是核心症状。
2. **nanopi 是这里唯一一个"TUI 层 + 完整 agent 驱动"合计只有 ≈12,072 L 的完整实现**（`render/` 4,406 + `mode/tui.rs` 6,999 + `keys.rs` 361 + `settings_toml.rs` 306），而 pi-rust 光 TUI 组件库就是 42,832 L。nanopi 的 `Cargo.toml` 自述目标是 "a ~4 MB static coding-agent CLI with zero runtime deps, for old and low-resource Linux"——**它是这四套里唯一为资源受限环境做设计的**，因此它的每一处"少写"都是刻意的，见 §2.1。

---

## 1. pi-rust TUI 现有的具体问题

### 1.1 `App` 是 7,681 行的 god-struct

`App` 字段分桶（[crates/pi-tui/src/app.rs:1174-1476](pi-rust/crates/pi-tui/src/app.rs)）：

| 桶 | 字段数 | 典型字段 | 职责 |
| --- | ---: | --- | --- |
| 配置/数据 | 6 | `config`、`prompt`、`messages`、`status_bar`、`status_data`、`theme` | 模型数据 |
| 弹窗/选择器 | 5 | `selector`、`settings`、`pending_setting_change/activation`、`dialog`、`ui_dialogs` | 弹窗状态 |
| Agent 通信 | 5 | `event_rx`、`cancel_token`、`pending_error`、`last_turn_usage`、`turn_busy` | 桥接 agent-core |
| 渲染几何（atomics） | 19 | `viewport_width/height/reserved`、`composer_body_width/scroll/window`、`composer_origin/size`、`autocomplete_*`、`modal_list_*`、`viewport_origin`、`scroll_to_end`、`truncated_above` | 上一帧的屏幕几何，用于事件路由 |
| 鼠标 | 5 | `modal_mouse_press`、`prompt_mouse_press`、`autocomplete_mouse_press`、`selection_dragging`、`search` | 输入状态 |
| 状态机 | 4 | `exit_requested`、`last_clear_at`、`terminal_title`、`pending_terminal_title` | 行为状态 |

> 这是把"渲染器 + 状态机 + 输入路由 + agent 桥接"塞进同一个 struct。**任何一处变更都牵动整个 struct**。

`App` 公开 183 个方法（`grep -c "^pub fn " app.rs`），分布在：
- 构造/配置
- 键事件路由（`step_key` / `step_key_at` / `handle_*`）
- 鼠标事件路由
- 几何记录（`record_*`）
- 弹窗控制（`open_*_selector` / `close_*` / `take_*`）
- 帧渲染（`render_to_buffer` / `render_snapshot`）
- agent 事件消化（`drain_agent_events`）
- 扩展 UI 桥接（`set_header` / `set_footer` / `set_widget` / `set_editor_component` / `open_custom` / `close_custom`）
- 状态查询

**后果**：
1. 单测必须构建整个 `App`，无法对子行为独立测试；
2. 增量改 keymap 时需要重读 7,000+ 行上下文；
3. cargo doc 自动生成的页面不可读；
4. clippy 对 `too_many_arguments` / `cognitive_complexity` 完全失效。

### 1.2 `editor.rs` 4,515 行的"超级组件"

`grep -c "^pub fn\|^impl\|^//!" editor.rs` = 214 个 impl/pub 元素，模块文档就占了 60 行。组件同时承担：

- 字符缓冲（`EditorBuffer`/`EditorState`）
- 光标模型（含 wrap affinity、sticky column、visual column 候选集）
- 撤销栈
- Kill ring + yank-pop
- 提示历史（`HistoryStore`）
- 跳转模式（jump mode）
- Bash 命令解析（`parse_bash_command` / `is_bash_mode`）
- 粘贴分类（`PasteBurst`）
- 图像芯片渲染（`CHIP_CHAR`）
- 选区 / 多光标
- 自动补全触发（`install_composer_autocomplete` 在 `interactive.rs`，但 chord → Editor）
- 鼠标坐标 → 光标的 hit-test
- Keybinding 匹配
- 视觉布局（`VisualLayout`）

这些职责耦合在 1 个 struct 上，且在 `interactive.rs` 中以 `ComposerAutocomplete` 等子模块反向注入。这是 TypeScript pi-tui 抽象成 9 个组件 + 3 个 util 模块（`keys` / `kill-ring` / `undo-stack` / `word-navigation` / `layout-node` / `layout.ts`）的**逆向极端**。

### 1.3 `interactive.rs`（9,432 L）与 `App` 互相穿透

`crates/pi-coding-agent/src/interactive.rs` 是 pi-rust **工作区内最大的单个源文件**（9,432 L；Martty 的 `app.rs` 更大（9,783 L），但 Martty 是仓库内的参照实现，不在 workspace `members` 中）。它直接调用 `App` 上 **81 个不同的方法**，并在 `App` 上挂载"应该在 agent-core / 独立模块的能力"：

- `extension_autocomplete_commands`（[interactive.rs:493](pi-rust/crates/pi-coding-agent/src/interactive.rs)）
- `install_composer_autocomplete`（同文件:440）
- `sync_thinking_for_model` / `sync_status_metrics` / `available_provider_count` / `is_subscription_provider` / `cycle_catalog` / `scoped_models` / `sorted_models` 等

`grep "^pub fn " interactive.rs` 只返回 **5 个公有自由函数**，但非测试部分共 **129 个 `fn` 定义**、其中仅 **6 个 `pub fn`**（`run_interactive`/`interactive_app_config`/`exit_screen_output`/`format_resume_command`/`seed_model_scope`/`apply_startup_ui_settings`），从 `push_bash_block`（[interactive.rs:1222](pi-rust/crates/pi-coding-agent/src/interactive.rs)）到 `copy_last_assistant_message`（同文件:2213），跨度极大。这些函数本应是 `pi-coding-agent` 的独立模块，目前却以"挂件"形式贴在交互模式上。**`App` 不知不觉成了 agent-core 的 UI 代理，而 `interactive.rs` 成了所有 agent 能力的宿主**。

### 1.4 渲染模型与组件模型割裂

`crates/pi-tui/src/component.rs` 定义了 `Component` trait（[component.rs:68-99](pi-rust/crates/pi-tui/src/component.rs)）：

```rust
pub trait Component {
    fn render(&self, width: u16) -> Vec<StyledLine>;
    fn handle_input(&mut self, key: Key) -> bool { let _ = key; false }
    fn dispose(&mut self) {}
}
```

**但是**，`App::render_to_buffer`（在 `app.rs` 里）绕过了这个 trait：内置组件（`MessageView`、`Prompt`、`StatusBar`、`Selector`、`SettingsList`、`Dialog`、`Editor`）全部自带 `render_to_buffer(area, buf)` 直接画到 `ratatui::Buffer`，从不走 `Component::render` 的行模型路径。`Component` trait 仅服务于扩展组件（`ExtensionUi`）。

后果：
- 双重渲染路径，一套给内置组件走 buffer，一套给扩展组件走行；
- `MessageView`（[message.rs:1093](pi-rust/crates/pi-tui/src/message.rs) vs [message.rs:1332](pi-rust/crates/pi-tui/src/message.rs)）与 `StatusBar`（[status.rs:400](pi-rust/crates/pi-tui/src/status.rs) vs [status.rs:549](pi-rust/crates/pi-tui/src/status.rs)）各自维护两套语义等价的渲染代码；
- 主题色解析在两条路径上都重做（`styled.rs:write_styled_line` vs 组件内直接调 `Theme::fg_ansi`）；
- `Component` trait 只被 4 处 `impl Component for` 使用（`component.rs` 的 `TextComponent`、`extension_ui.rs`、`image.rs`、以及 `pi-coding-agent` 的 `ui_bridge.rs`）——即**几乎全部内置组件都不走 trait 路径**。

### 1.5 几何状态全是 atomics

`viewport_width`、`composer_body_width`、`autocomplete_origin`、`modal_list_*` 等 **21 个字段**（`app.rs` 19 + `message.rs` 2）用 `AtomicU16` / `AtomicUsize` / `AtomicU8`——其中 14 个是 `AtomicU16`。理由在注释里写得很清楚：

> "the pointer arrives between renders, so the geometry is recorded rather than recomputed."

但：
1. 它们只在单线程 mutate（`render_to_buffer` 写 + `step_key` 读），atomic 带来 **不必要的内存屏障** 与 **顺序约束**，反而掩盖了"渲染后路由"的真实顺序；
2. 与 `interactive.rs` 的"非原子"读（`app.composer_overflows()` 等）混用，需要反复说明 lock-free 语义；
3. 没有"渲染帧"对象——一帧的状态散落在 21 个原子字段里，hit-test 全靠手工配对。

### 1.6 没有 layout 抽象

`App::render_to_buffer` 内嵌：
- `plan_chrome` 由 `extension_ui.rs` 实现
- `paint_chrome` 散落在 600+ 行 `render_to_buffer`
- 几何计算（`message_rect` / `overlay_rect`）是私有 free function
- 没有 `LayoutRect` / `LayoutBox` / `LayoutFrame` 类型

→ pi-tui 在 [`tui-plan.md`](tui-plan.md) 已经规划的"`packages/tui/src/layout.ts`"对应物在 pi-rust 完全缺失。`tui-plan.md` 里说要"先在 TS 实现、再考虑 Rust port"，但**pi-rust 自身就是参照 TS 设计的 port**，按理应同步抽象。

### 1.7 测试：数量充足，但多数是"帧快照"而非"行为单元"

与直觉相反，测试量其实**很大**：
- `pi-rust/crates/pi-tui/tests/`：**88 个集成测试文件、770 个 `#[test]`**
- `pi-rust/crates/pi-coding-agent/tests/`：**33 个集成测试文件**

但测试形态**以"整帧快照"为主**——`App::render_snapshot`（[app.rs:7029](pi-rust/crates/pi-tui/src/app.rs)）把一帧渲染成字符串再断言，**88 个测试文件里有 54 个调用它、共 189 处**：

1. 这些快照断言的是**最终像素**，任何内部结构重构（字段改名、模块拆分、渲染顺序微调）都会直接失败，重构时无法"小步验证"；
2. 好在并非全部如此——`markdown.rs`（56 个）、`autocomplete.rs`（34）、`composer_editing.rs`（26）、`keybindings.rs`（16）等是按行为组织的，可作为拆分时的对照基准；
3. `App` 的状态机转换（"按 Ctrl+C 两次退出"、"paste 超过阈值切成 marker"）没有直接测试，只能通过多帧快照间接覆盖；
4. `interactive.rs` 的 131 个函数中绝大多数没有测试引用。

→ 这是重构的最大阻力：**需要先为内部行为补单元测试，才能在拆分时不破坏这批帧快照**。这也解释了为什么模块化一直没落地——现有快照反而"锁定"了单体结构。

### 1.8 没有 trait 化的子系统

| 概念 | 抽象形式 |
| --- | --- |
| `Component` 扩展接入 | 已有 trait，但内置组件不实现 |
| 渲染目标 | `ratatui::buffer::Buffer`（强耦合） |
| 主题 | `Theme` 单例，无 `ThemeProvider` trait |
| keymap | 全局可变 (`OnceCell<Arc<KeybindingsManager>>`)，无注入点 |
| agent 事件源 | `mpsc::UnboundedReceiver<AgentEvent>` 直接持有，无法 mock |
| 后台任务 | `CancellationToken`，但 `interactive.rs` 里多处 `tokio::spawn` 散落 |

→ 这都是依赖注入 / 测试替身 (test double) 缺失的根因。

### 1.9 与上游 pi-tui 的命名漂移

（此处"上游"指 TypeScript `packages/tui`；与独立的 `nanopi/` Rust 项目的对比见 §2.1。）

| pi-rust | pi-tui (TS) | 备注 |
| --- | --- | --- |
| `Component::render(width) -> Vec<StyledLine>` | `Component.render(width: number): string[]` | 一致 ✅ |
| `ExtensionUi::plan_chrome` | inline 在 `interactive-mode.ts` | 一致但**位置不同** |
| `ScrollbarGeometry` | inline type | 一致 |
| `App` | `TuiAltScreen` + 组件树 | **结构差异**：pi-rust 是单体，TS 是组件树 |
| `Editor` | `Editor` + `Input` 双组件 | TS 拆得更细 |
| `MessageView` | `ChatViewport` + 消息组件 | TS 拆得更细 |
| `StatusBar` | `Status` | 类似 |
| `Selector` / `SettingsList` / `Dialog` | `Selector` / `SettingsList` / `Dialog` | 一致 |

命名基本一致，但**抽象层级差异巨大**。

### 1.10 根因：alt-screen 模式导致"滚动区"被手工重建

这是把 §1.1 / §1.5 / §1.6 串起来的一条根因，此前未被单列。

pi-rust 的终端初始化（[crates/pi-coding-agent/src/interactive.rs:4896-4913](pi-rust/crates/pi-coding-agent/src/interactive.rs)）：

```rust
enable_raw_mode()?;
execute!(stdout, EnterAlternateScreen, ...)?;   // 全屏接管
let terminal = Terminal::new(backend)?;          // 默认 Fullscreen viewport
```

一旦进入 alt-screen，**终端的原生滚动区就不可用了**——没有任何 `Terminal::insert_before` 能落进用户可见的历史。于是 pi-rust 必须在 `App` 内自己重建一切滚动相关能力：

| 本可由终端免费提供 | pi-rust 的手工重建 | 位置 |
| --- | --- | --- |
| 历史行缓冲 | `messages: Vec<MessageView>` + 自行 flush | `app.rs` |
| 滚动位置 | `scroll_to_end` / `truncated_above`（各 3 个 `AtomicU16` 的窗口偏移） | `app.rs:1342-1348` |
| 可视高度 | `viewport_height` / `viewport_reserved` 每帧重算 | `app.rs:6317-6318` |
| 滚动条占位 | `viewport_reserved` 加回列宽 | `app.rs:6372-6379` |
| 原位覆盖更新 | 整帧重绘 + 差异 diff | `render_to_buffer` |

对照：**nanopi 用 `Viewport::Inline(DOCK_HEIGHT)` 进入内联模式**（[nanopi/src/mode/tui.rs:703-717](nanopi/src/mode/tui.rs)），注释直写 *"NO alt-screen — inline mode preserves scrollback the way PI does"*，完成的回合经 `insert_line(term, Line)` → ratatui `Terminal::insert_before` 直接写进**终端真实滚动区**。nanopi 的 dock 因而只有 5 条固定约束（同文件 5013-5021），**不需要任何几何 atomic**。

→ 结论：§1.5 的 21 个几何原子字段里至少一半、§1.6 缺失的 layout 抽象，本质都是"用 alt-screen 换来的全屏控制力"的账单。这不是 bug，是**权衡**——alt-screen 换来稳定的帧内搜索与精确覆盖层，代价是丢掉原生滚动、复制粘贴与用户自己的 shell 历史。

→ 因此本报告**不建议**在 Phase 0-8 里改变视口模式（改动面过大且涉及产品取舍）。但应当（a）把"滚动区"从 `App` 抽成一个显式的 `Viewport` 子系统（见 §3.x），（b）确认 panic 路径能恢复终端（§2.1-E）。若产品上愿意接受内联模式，则可以一次性消掉 §1.5 的大半——这需要产品决策，列入 §4 的可选 Phase。

---

## 2. 对标：nanopi（Rust）、pi-tui（TS）与 Martty 是怎么做的

### 2.1 nanopi（Rust 独立项目）—— 源码级精读

> 本节是逐文件读 `nanopi/src` 的结论，不是目录罗列。重点不是"它有什么"，而是**它为什么这么切、每一刀省掉了什么**。

#### 结构

```
nanopi/src/
  render/                    4,406 L / 11 模块  —— 输出与组件，全部可单测
    text_buffer.rs  1,085    多行编辑器（含 ~370 行测试）
    stdout.rs         834    print 模式流式 ANSI 渲染器
    menu.rs           435    泛型菜单（含 ~180 行测试）
    notice.rs         433    分组启动警告（含 9 个测试）
    markdown.rs       385
    export_html.rs    332
    raw_tty.rs        335    raw mode 下存活的 stderr 通知
    status_line.rs    180    **纯函数**，无状态
    panel.rs          181    工具折叠面板状态机
    spinner.rs        116    后台 braille spinner 任务
    alt_screen.rs      73    RAII 恢复守卫
  mode/tui.rs       6,999    事件循环 + dock 渲染（测试从 5,602 行起）
  keys.rs             361    ActionId 枚举 + KeySpec 解析
  settings_toml.rs    306
```

11 个 render 模块里 **8 个 < 450 行**，最大的 `text_buffer.rs` 也才 1,085 行。每个模块单一职责、状态与绘制分离、测试与实现同文件。

#### A. 内联模式 vs alt-screen——这一条解释了 pi-rust `app.rs` 的大半体积

见 §1.10。核心行在 [nanopi/src/mode/tui.rs:703-717](nanopi/src/mode/tui.rs)（模块注释见同文件 `:695`）：

```rust
let terminal = Terminal::with_options(
    backend,
    TerminalOptions { viewport: Viewport::Inline(DOCK_HEIGHT) },
)?;
```

没有 `EnterAlternateScreen`。dock 只是一个 5 行约束的固定形状（同文件 5056-5065）：

```rust
constraints([
    Constraint::Min(0),                      // 覆盖层 / 顶部弹性空白
    Constraint::Length(1),                   // 状态条
    Constraint::Length(2 + input_content_h), // 输入框（边框 + 内容）
    Constraint::Length(1),                   // cwd + branch
    Constraint::Length(1),                   // stats
])
```

`Min(0)` 吸收所有弹性：覆盖层打开时落在这块，输入框则从底部向上生长。**没有一帧几何状态需要跨帧保存**。

#### B. `interpret_key(&mut App, KeyEvent) -> KeyAction`——单一分派点 + 显式优先级

[nanopi/src/mode/tui.rs:1095-1189](nanopi/src/mode/tui.rs) 定义了 **34 个 `KeyAction` 变体**（`StartTurn(String)`、`SteerTurn(String)`、`CycleThinking`、`ForkChosen(PathBuf, usize)`、`SummaryChosen(SummaryChoice)`…），每个变体都带齐执行所需的数据。

`interpret_key`（同文件 1195）**只返回动作，不执行任何副作用**。覆盖层优先级是一条显式 if-链：

```
capture_key_for > keybindings_menu > settings_menu > summary_prompt
  > resume_picker > fork_picker > model_picker > palette > TextBuffer
```

关键细节：**绘制侧 `draw_dock` 用完全相同的顺序**画覆盖层（同文件 5024 起，注释明写 "Priority matches interpret_key"）。分派顺序与绘制顺序写在两处但被有意对齐并互相标注——比 pi-rust 把 `step_key` 放在 `app.rs:3385`、把 `render_to_buffer` 放在 `app.rs:6301` 这种"同一状态机被拆到相隔 3,000 行的两处"要容易维护得多。

#### C. `MenuState<T>` 泛型复用 7 次——一刀替换 pi-rust 的 4 个组件

`App` 里的菜单字段（[nanopi/src/mode/tui.rs:746-768](nanopi/src/mode/tui.rs)）全部是同一个泛型：

```rust
palette:          Option<MenuState<SlashCmd>>
model_picker:     Option<MenuState<String>>
fork_picker:      Option<MenuState<(PathBuf, usize)>>
resume_picker:    Option<MenuState<PathBuf>>
settings_menu:    Option<MenuState<SettingsRow>>
keybindings_menu: Option<MenuState<crate::keys::ActionId>>
summary_prompt:   Option<MenuState<SummaryChoice>>
```

`menu.rs` 模块文档是这套设计的自我说明：

> Rendering is done by the caller against a ratatui `Rect`: this module owns **state** (items, filter, cursor), **not draw code**, so it's easy to test.

统一返回 `MenuAction<T>`：`Nothing / Chosen(T) / Filled / ChosenRaw / Cancel`。因为状态归 `MenuState`、绘制归调用者，**一个 `draw_menu(buf, area, m, title)` 画全部 7 个菜单**。

`menu.rs` 还包含可迁移的细节：
- `rank()` 四级相关性分层（`0`=裸标签精确、`1`=前缀、`2`=标签包含、`3`=描述包含），并附注释解释了 `/settings` 预选错位的具体 bug；
- `with_free_text()` builder 开关，专供 `/model` 这类允许自由输入的场景；
- 435 行里 ~180 行是测试，覆盖排序、过滤、`ChosenRaw` 等行为——**这是行为单元测试，不是帧快照**。

→ pi-rust 的对应物是 `selector.rs`(1,426) + `settings.rs`(892) + `dialog.rs` + `slash_menu.rs`，**四套状态、四套绘制、四套测试**，且四者都有 `render_lines` + `render_to_buffer` 双路径（§1.4）。这是本报告里投入产出比最高的一次合并。

#### D. `raw_tty.rs` 的通知队列——解决一个 pi-rust 客观存在的缺陷

这个问题 pi-rust 有、nanopi 修了：raw mode 下 `\n` 只是 LINE FEED，裸 `eprintln!` 会**阶梯式错位**；更糟的是写进 ratatui 管理的区域会被下一帧重绘擦掉。nanopi 的 [render/raw_tty.rs](nanopi/src/render/raw_tty.rs) 用一个进程级队列解决：

```rust
enum Destination { Queue, Stderr }   // 枚举而非两个 bool

fn destination(raw: bool) -> Destination {
    if raw { Destination::Queue } else { Destination::Stderr }
}
```

用**类型**而不是约定来保证"不会同时写两处"（注释明写：同时写会让用户在两次重绘之间看到同一行两次，且这是测试观测不到的——所以用类型当守卫）。配套：

- TUI 循环每 tick `drain()`，经 `insert_before` 落进滚动区（[tui.rs:1883-1891](nanopi/src/mode/tui.rs)）；
- 容量 `MAX_PENDING = 512`，**溢出丢最旧**——理由是"循环停止 tick 时该问的不是保留多少历史，而是会不会吃内存；而最新那行才是描述当前状况的"；
- `teardown_terminal` 调 `flush_pending_to_stderr()`，保证"第一次 tick 之前 / 循环退出之后"的通知不丢；
- 多行消息按行拆分入队，因为 drainer 一行插一行。

**pi-rust 现状**：`crates/pi-coding-agent/src/` 内 **38 处裸 `eprintln!`**（分布：`main.rs` 21、`external_editor.rs` 7、`text_fallback.rs` 3、`skills.rs`/`print_mode.rs`/`interactive.rs` 各 2、`config.rs` 1；具体位置如 `config.rs:953`、`print_mode.rs:431/1055`、`text_fallback.rs:39/46/91`、`external_editor.rs:127-198`、`skills.rs:1130/1162`、`interactive.rs:368/406`），无队列、无 raw-mode 感知。在 alt-screen 下这些写入会直接破帧。**这是全报告唯一一条有明确缺陷支撑、可以照抄的改造。**

#### E. 每个 render 模块单一职责 + 测试同文件

| nanopi 模块 | 行数 | 形态 | pi-rust 对应 | 对比 |
| --- | ---: | --- | --- | --- |
| `status_line.rs` | 180 | **全是纯函数**：`cwd_display` / `git_branch` / `short_session_id` / `tokens_summary` / `context_percent` / `context_ratio` / `context_color`，零状态 | `status.rs` 1,927 L | pi-rust 有 `render_lines` + `render_lines_plain` + `render_to_buffer` 三入口 |
| `panel.rs` | 181 | `PanelState{Pending,Running,Done,Errored}` + `feed(&AgentEvent)` + 单行 `render` | `message.rs` 2,139 L | 状态机与渲染分离 |
| `markdown.rs` | 385 | — | `markdown.rs` 1,974 L | **5.1×** |
| `notice.rs` | 433 | 分组警告 + 9 个行为测试 | **无等价物** | pi-rust 缺此能力 |
| `alt_screen.rs` | 73 | RAII 守卫，`impl Drop` 尽力恢复 | 无；`interactive.rs` 直接 `EnterAlternateScreen` | 需确认 panic 路径 |

`notice.rs` 的模块文档值得单独摘出来当范式——它**先把旧版扁平输出的 4 个缺陷逐条编号列出**（严重度不可见 / 折行像新通知 / subject 每行重复 / `nanopi:` 前缀无信息量），再逐条对治。两个可抄的细节：

1. `#[derive(PartialOrd, Ord)]` 让严重度排序**免费**（`Error < Warn < Info`，`sort_by_key(|n| n.level)` 即可）；
2. `marker()` 返回 `✗ / ! / ·` 字形作为**不依赖颜色的严重度信号**——测试名就叫 `the_marker_carries_severity_without_color`，理由写得很清楚："`NO_COLOR`、日志文件、或丢掉 SGR 的终端会把颜色全部抹平"。

#### F. `text_buffer.rs` 的取舍值得逐条对标

1,085 行、~370 行测试。它的**显式非目标**是最有价值的部分：

> **Design**: byte-offset cursor **by explicit design** ("we grapheme-count on render, not here")。

即：光标按字节偏移存，宽度/字素计数只发生在渲染期。这一刀把字素簇、组合字符、宽字符的复杂度全部隔离在渲染侧，编辑器状态机因此可以纯字节运算。

其他可迁移点：
- `Action` 枚举返回给调用者（`Nothing / Submit / Cancel / Exit / SlashChanged`）——与 `MenuAction<T>`、`KeyAction` 同一套 IoC 风格；
- `LastOp{Other, TypingWord, TypingNonWord}` 实现 **fish 式按"词"合并的撤销**（连续输入字母算一步）；pi-rust 有 `undo_stack.rs` 但无词级合并；
- `HISTORY_CAP = 100` + `history_path` 持久化；
- `normalize_pasted()` 统一处理 CRLF / CR / tab；
- 显式列出**不做**的事："multi-slot kill ring, redo, search, IME"。对照 pi-rust 拥有 `kill_ring.rs`、`search.rs`(1,137 L)、`tree.rs`(842 L)——功能更全，但这也是它更难改的原因：**没有一份"不做什么"的清单。**

#### G. 换行/缩放推迟到渲染期，并返回字节区间避免重复测量

`wrap_input_lines`（[tui.rs:4933](nanopi/src/mode/tui.rs)）把逻辑行软换行为 display rows，返回：

```rust
struct InputDisplayRow { logical_row: usize, is_first: bool, start: usize, end: usize }
```

以及光标所在的 display row 索引——**渲染器拿到字节区间后不需要重新测量**。`input_scroll_window`（4947）实现底部锚定的有界视窗（"Claude Code's bounded-viewport scroll"）。

对照 pi-rust：同类逻辑与 wrap affinity、sticky column、visual column 候选集混在 `editor.rs` 的 4,515 行里。

#### H. 事件循环：`select!` + 120ms tick + 三类队列

`run_app`（[tui.rs:1782](nanopi/src/mode/tui.rs)）用 `tokio::select!` 只等一个 120ms ticker（`MissedTickBehavior::Skip`，避免补跑陈旧 tick），tick 内轮询四件事：

1. 完成的 `summarize_task`（`is_finished()` 轮询，**不 await**——await 会同时冻住 ticker、事件流和按键）；
2. 完成的 plugin command task（跑在 blocking pool 上，同理）；
3. `raw_tty::drain()` 的诊断行；
4. `wasm::notify::drain()` 的插件行（青色，与助手文本的绿色、工具卡区分开）。

两处可迁移的健壮性细节：
- `follow_up_slot` 用 `VecDeque` 而非单槽，注释记录了单槽曾**在回合结束与完成回调之间的窗口里静默丢掉第二条消息**（且两条都已回显）；
- 取消统一走 `CancellationToken`，长任务一律轮询 `is_finished()`。

#### §2.1 小结

nanopi 的每条模式都收敛到同一个原则：

> **状态归状态、绘制归调用者、副作用返回枚举、副作用由 loop 统一执行。**

对本报告的可操作结论按优先级排序：

| 优先级 | 行动 | 依据 | 规模 |
| --- | --- | --- | --- |
| P0 | 引入 raw-mode 感知的通知队列，替换 38 处裸 `eprintln!` | §2.1-D，**有明确缺陷** | 1 个新模块 + 调用点替换 |
| P0 | 合并 `selector` / `settings` / `dialog` / `slash_menu` 为 `MenuState<T>` | §2.1-C，7 处复用已验证 | 中，但收益最大 |
| P1 | 把 `interpret_key` 式"返回动作"的模式引入 `App`，抽出 `KeyAction` 枚举 | §2.1-B | 大，随 §3.6 一起做 |
| P1 | 建立 panic/信号下的终端恢复守卫（对照 `alt_screen.rs` 的 `impl Drop`） | §2.1-E | 小 |
| P2 | 为编辑器/状态条写"显式非目标"清单，作为重构的边界契约 | §2.1-F | 文档，零代码 |
| P2 | 在状态条 / 通知等模块改用纯函数 + 字符串断言测试 | §2.1-E | 小 |

### 2.2 pi-tui（TypeScript）—— "组件 + 帧"模型

`packages/tui/src/` 的目录划分：

```
tui.ts            1456  ← Component 基类 + TUI 抽象
tui-alt-screen.ts 1721  ← AltScreen 实现，含布局状态
tui-main-screen.ts  655  ← MainScreen（terminal scrollback）实现
layout.ts           449  ← 公共 layout 工具
layout-node.ts       51  ← LayoutNode 数据结构
components/
  editor.ts        2461  ← 多行编辑器
  input.ts         1025  ← 单行输入
  markdown.ts      1015  ← markdown 渲染
  select-list.ts    273
  settings-list.ts  328
  h-stack.ts / v-stack.ts / stack.ts / box.ts / spacer.ts  ← 布局
  scroll-view.ts   224
  ...
keys.ts            1401  ← 键位词表
utils.ts           1337  ← ANSI 切片/组合
```

关键模式：
1. **`Component` 是统一的渲染接口**（`tui.ts:111-134`）。内置组件和扩展组件走同一路径。
2. **`tui-plan.md` 设计的 `LayoutRect` / `LayoutBox` / `LayoutFrame`** 即将落地（已在做），用于替换 `tui-alt-screen.ts` 内嵌的 `getBounds()` / `renderLayoutBounds()`。
3. **`alt-screen-search.ts`** 拆出独立的 transcript 搜索覆盖层（327 L），与 `interactive-mode.ts` 解耦。
4. **`chat-viewport.ts`**（46 L）只负责消息区滚动，把消息渲染本身留给消息组件。
5. **`keybindings.ts`（320 L）只是配置解析**，实际的键词表在 `keys.ts`（1,401 L）。
6. **`utils.ts` 集中 ANSI/width 处理**（`sliceByColumn`、`compositeTuiLine`、`visibleWidth`、`stripTerminalSequences`）。
7. **`tui-main-screen.ts` 与 `tui-alt-screen.ts` 共用 `TUI` 接口**，通过能力检测（[`tui-plan.md` §Viewport capability](tui-plan.md)）区分。

**结论**：TS 版本虽然也有大文件（`editor.ts` 2,461 L），但**职责单一**——`editor.ts` 仅做编辑器，不掺入 agent 桥接、不掺入几何记录；布局几何由 `tui-alt-screen.ts` 统一负责。

### 2.3 Martty（Rust）—— "控制器 + 转录"分离

`Martty/src/` 关键文件：

```
app.rs        9783  ← App 状态 + 控制器骨架（最大）
ui.rs         4217  ← 渲染：banner / scrollback / tips / status / prompt / overlays
transcript.rs 1671  ← 转录模型：cell / line / 用量统计
controller.rs 1000  ← ACP 控制器
events.rs     1168  ← 事件类型
slots.rs       668  ← 槽位（autocomplete / model picker 等）
acp.rs        2861  ← ACP 协议
acp_term.rs    315  ← 终端 ACP 适配
input/
  editor.rs    394  ← 仅做编辑器（与 pi-rust editor.rs 的 4515 形成对比）
  composer.rs  ...
  keymap.rs    ...
  vim.rs       ...
  ...
```

关键模式：
1. **`App` 偏瘦**：9783 行但主要是事件分发（事件本身是 `events.rs` 1168 行）。
2. **`transcript.rs` 把"转录模型"与"渲染"分离**——Cell / Line / SessionStats / UsageTotals 都是纯数据 + 测量函数。
3. **`ui.rs` 集中了所有渲染**：所有组件在 `ui::draw(f, &mut App)` 一次性画出（[ui.rs:177](pi-rust/Martty/src/ui.rs)）。
4. **`input/` 子目录独立**——编辑器与 keymap 各自 < 500 L。
5. **`Slots` 抽象**（slots.rs）类似"侧栏面板"，与 `Selector` / `SettingsList` 解耦。
6. **`Widget for LinesWindow<'_>` impl**（ui.rs:2565）实现 ratatui 的 Widget trait，**统一渲染入口**。

**结论**：Martty 比 pi-rust TUI 还大，但**单文件职责更纯**，且显式拆出 `transcript.rs`、`controller.rs`、`slots.rs` 等数据/控制模块。

### 2.4 四者对比矩阵

| 维度 | nanopi (Rust) | pi-tui (TS) | Martty (Rust) | pi-rust (Rust) | pi-rust 评价 |
| --- | --- | --- | --- | --- | --- |
| **视口模式** | `Viewport::Inline` 内联 | AltScreen / MainScreen 双实现 | `Terminal::new` 全屏 | `EnterAlternateScreen` 全屏 | 权衡，见 §1.10 |
| **滚动区来源** | **终端原生**（`insert_before`） | 两种 TUI 各自实现 | `LinesWindow` Widget | **手工重建**（buffer + 偏移 + 几何） | **劣** |
| 组件基类 | 无 trait，`draw_*` 自由函数 | `Component` 接口（统一） | ratatui `Widget` impl | `Component` trait（**仅扩展用**） | **劣** |
| **菜单抽象** | **`MenuState<T>` 泛型 ×7** | `Selector` / `SettingsList` / `Dialog` 分列 | `slots.rs` | `selector` + `settings` + `dialog` + `slash_menu` 四套 | **劣** |
| 内置组件 | 11 模块（4,406 L），**9 个 < 450 L** | 18 个（6,276 L），**15 个 < 350 L** | 8 个文件 | 38 文件，**13 个 > 1,000 L** | **劣** |
| 最大单文件 | `mode/tui.rs` 6,999（render 层最大仅 1,085） | `interactive-mode.ts` 6,620 | `app.rs` 9,783 | `interactive.rs` **9,432** / `app.rs` 7,681 | 中 |
| **Layout 抽象** | 不需要（dock 固定 5 约束） | `tui-plan.md` 设计中 | 隐式 `Rect` | **缺失** | **劣** |
| **跨帧几何状态** | **无** | 组件内部 | 控制器内 | 21 个原子几何字段 | **劣** |
| **诊断输出** | `raw_tty` 队列 + `insert_before` | — | — | **38 处裸 `eprintln!`** | **劣** |
| Keymap 解耦 | `keys.rs` 361 L（`ActionId` + `KeySpec`） | `keys.ts` 1,401 L | `input/keymap.rs` | `keybindings.rs` 966 L | 中 |
| 主题解耦 | 无（模块内联 SGR） | per-component adapters | `theme.rs` 654 L | `theme.rs` 1,836 L | 相当 |
| **测试粒度** | **行为单元**（同文件，`menu.rs` ~180 L / `text_buffer.rs` ~370 L 测试） | 单文件级 | 单文件级 | **189 处整帧快照**（88 文件中 54 个，锁定单体） | **劣** |
| 内置编辑器 | `text_buffer.rs` 1,085 L | `editor.ts` 2,461 L | `input/editor.rs` 394 L | `editor.rs` **4,515 L** | **劣** |
| Markdown | `markdown.rs` 385 L | `markdown.ts` 1,015 L | — | `markdown.rs` **1,974 L** | 中 |
| **TUI 层总量** | **≈ 12,072 L**（含 agent 驱动） | 18,653 L（仅组件库，42 文件） | 29,913 L | **42,832 L**（仅组件库，不含驱动） | **劣** |

矩阵读法：**nanopi 在 11 项上领先或平手、0 项落后**——它是四套里最小的，却也是最"正交"的。pi-rust 在滚动区、菜单抽象、Layout、几何状态、诊断输出、测试粒度、编辑器 7 项上明确落后，而这 7 项全都指向同一件事：**缺少把状态与绘制分开的中间层**（`MenuState<T>`、`KeyAction`、`insert_before` 的终端滚动区）。

---

### 2.5 TS pi-tui 的公开契约 —— 插件兼容的基准

> 新目标要求"一比一复刻 TS pi-tui 以便插件兼容"。因此必须先固定**基准是什么**。TS 的公开契约只有一处：`packages/tui/src/index.ts`（156 L，全部 `export`）。任何插件只依赖这个文件导出的名字。

#### 2.5.1 契约全表（`index.ts` 逐条）

| 类别 | 导出名 | TS 文件 | pi-rust 现状 |
| --- | --- | --- | --- |
| **组件核心** | `Component`、`Container`、`CURSOR_MARKER`、`compositeTuiLine` | `tui.ts` | `Component` 有（**签名不同**）；`Container`/`CURSOR_MARKER`/`compositeTuiLine` **全部缺失** |
| 焦点 | `Focusable`、`isFocusable` | `tui.ts` | **缺失** |
| TUI 接口 | `TUI`、`TuiMode`、`TuiStopOptions`、`ViewportTUI`、`isViewportTUI` | `tui.ts` | `App` 是 god-struct，**无 `TUI` trait** |
| TUI 实现 | `TuiAltScreen`、`TuiAltScreenOptions`、`TuiMainScreen`、`TuiMainScreenRenderState` | `tui-alt-screen.ts`、`tui-main-screen.ts` | **缺失**（pi-rust 只有 alt-screen 一条路径） |
| 覆盖层 | `OverlayAnchor`、`OverlayBounds`、`OverlayHandle`、`OverlayMargin`、`OverlayOptions`、`OverlayUnfocusOptions`、`SizeValue` | `tui.ts` | `OverlayAnchor` 有；其余 **缺失** |
| 输入钩子 | `TuiInputListener`、`TuiInputListenerResult` | `tui.ts` | **缺失** |
| 鼠标 | `TuiMouseButton`、`TuiMouseEvent`、`TuiMouseEventResult`、`TuiMouseEventType` | `tui.ts` | **缺失**（pi-rust 用 `mouse_region.rs` 的另一套模型） |
| **布局** | `VStack`、`StackChild`、`StackEntry`、`StackEntryOptions`、`StackOptions` | `components/v-stack.ts` | **缺失** |
| | `HStack` | `components/h-stack.ts` | **缺失** |
| | `Box` | `components/box.ts` | **缺失** |
| | `Spacer` | `components/spacer.ts` | **缺失** |
| | `ScrollView`、`ScrollViewOptions`、`ScrollViewScrollbar`、`ScrollViewScrollToOptions` | `components/scroll-view.ts` | **缺失** |
| **内置组件** | `Text`、`TruncatedText` | `components/text.ts`、`truncated-text.ts` | 有 `TextComponent`，`TruncatedText` 缺失 |
| | `Editor`、`EditorOptions`、`EditorTheme` | `components/editor.ts` | `editor.rs`（4,515 L，**API 不兼容**） |
| | `EditorComponent`（自定义编辑器接口） | `editor-component.ts` | `set_editor_component` 有，类型不同 |
| | `Input`、`JUMP_DIRECTION`、`JumpDirection` | `components/input.ts` | `input.rs` 只有类型层 |
| | `Markdown`、`MarkdownOptions`、`MarkdownTheme`、`DefaultTextStyle`、`Marked`/`Token`/`Tokens` | `components/markdown.ts`、`marked` | `markdown.rs`（函数式，**非组件**） |
| | `Image`、`ImageOptions`、`ImageTheme` | `components/image.ts` | `image.rs` + `terminal_image.rs`（函数式） |
| | `Loader`、`LoaderIndicatorOptions`、`CancellableLoader` | `components/loader.ts`、`cancellable-loader.ts` | `loader.rs`（仅 frame 表） |
| | `SelectList`、`SelectItem`、`SelectListLayoutOptions`、`SelectListTheme`、`SelectListTruncatePrimaryContext` | `components/select-list.ts` | `selector.rs`（**API 不兼容**） |
| | `SettingsList`、`SettingItem`、`SettingsListTheme` | `components/settings-list.ts` | `settings.rs`（**API 不兼容**） |
| | `MouseRegion`、`MouseRegionHandler` | `components/mouse-region.ts` | `mouse_region.rs`（**API 不同**） |
| | `AltScreenFlash` | `components/alt-screen-flash.ts` | **缺失** |
| 补全 | `AutocompleteProvider`、`AutocompleteItem`、`AutocompleteSuggestions`、`CombinedAutocompleteProvider`、`SlashCommand` | `autocomplete.ts` | `autocomplete.rs`（**API 不同**） |
| 键位 | `Key`、`KeyId`、`parseKey`、`matchesKey`、`isKeyRelease`、`isKeyRepeat`、`isKittyProtocolActive`、`setKittyProtocolActive`、`decodeKittyPrintable`、`KeyEventType` | `keys.ts` | `input.rs` 的 `Key`/`KeyCode`（**名字与形状不同**） |
| | `getKeybindings`、`setKeybindings`、`KeybindingsManager`、`TUI_KEYBINDINGS`、`Keybinding`、`KeybindingConflict`、`KeybindingDefinition`、`KeybindingDefinitions`、`Keybindings`、`KeybindingsConfig` | `keybindings.ts` | `keybindings.rs`（**API 不同**） |
| 工具函数 | `visibleWidth`、`sliceByColumn`、`stripTerminalSequences`、`truncateToWidth`、`wrapTextWithAnsi`、`getOsc8LinkAtColumn` | `utils.ts` | `width.rs` + `styled.rs` 部分覆盖；**`utils.ts` 6 个函数无对应公共导出** |
| 终端 | `Terminal`、`ProcessTerminal` | `terminal.ts` | 无 public trait |
| 终端图像 | `renderImage`、`encodeKitty`、`encodeITerm2`、`detectCapabilities`、`getCapabilities`、`setCapabilities`、`setCapabilityOverrides`、`resetCapabilitiesCache`、`hyperlink`、`imageFallback`、`getPngDimensions`、`getJpegDimensions`、`getGifDimensions`、`getWebpDimensions`、`getImageDimensions`、`getCellDimensions`、`setCellDimensions`、`calculateImageRows`、`allocateImageId`、`deleteKittyImage`、`deleteAllKittyImages` + 5 个类型 | `terminal-image.ts` | `terminal_image.rs`（**名称体系不同**） |
| 终端颜色 | `parseOsc11BackgroundColor`、`parseTerminalColorSchemeReport`、`RgbColor`、`TerminalColorScheme` | `terminal-colors.ts` | **缺失** |
| 剪贴板 | `getNativeClipboard`、`NativeClipboard` | `native-platform.ts` | `clipboard.rs`（只有 `osc52_sequence`/`base64_encode`，**无 `NativeClipboard` 类型**） |
| 输入缓冲 | `StdinBuffer`、`StdinBufferOptions`、`StdinBufferEventMap` | `stdin-buffer.ts` | **缺失** |
| 搜索 | —（未导出，内部用） | `alt-screen-search.ts` | `search.rs`（1,137 L） |
| 模糊 | `fuzzyMatch`、`fuzzyFilter`、`FuzzyMatch` | `fuzzy.ts` | `fuzzy.rs`（`fuzzy_match` / `fuzzy_filter`，**命名不同**） |
| LaTeX | `renderLatex`、`RenderLatexOptions` | `latex.ts` | `latex.rs` ✅ |
| kill/undo | —（未导出） | `kill-ring.ts`、`undo-stack.ts` | `kill_ring.rs`、`undo_stack.rs` |
| 词导航 | —（未导出） | `word-navigation.ts` | `word_navigation.rs` |

**统计**：TS 导出约 **145 个名字**；pi-rust 有对应物且形状接近的约 **20 个**（14%），其余 86% 或缺失、或名称/形状不兼容。**这就是"插件兼容"要补的差距量级。**

#### 2.5.2 `Component` 签名的逐字段差异（最关键的一处）

TS（`packages/tui/src/tui.ts:111-136`）：

```ts
export interface Component {
  render(width: number): string[];
  handleInput?(data: string): void;
  handleMouse?(event: TuiMouseEvent): TuiMouseEventResult | undefined;
  wantsKeyRelease?: boolean;
  invalidate(): void;
}
```

pi-rust（`crates/pi-tui/src/component.rs:68+`）：

```rust
pub trait Component {
    fn render(&self, width: u16) -> Vec<StyledLine>;
    fn handle_input(&mut self, key: Key) -> bool { false }
    fn dispose(&mut self) {}
}
```

| TS 成员 | pi-rust | 差异性质 |
| --- | --- | --- |
| `render(width): string[]` | `render(&self, width: u16) -> Vec<StyledLine>` | **参数一致、返回类型不同**。TS 返回已编码 ANSI 字符串；pi-rust 返回主题槽位 `StyledLine` |
| `handleInput?(data: string)` | `handle_input(&mut self, key: Key) -> bool` | **语义不同**：TS 传**原始输入串**由组件自行解析；pi-rust 传**已解析的 `Key`**，返回是否消费 |
| `handleMouse?(event)` | **无** | 缺失 |
| `wantsKeyRelease?` | **无** | 缺失 |
| `invalidate(): void` | **无**（只有 `dispose()`） | 生命周期不同 |

`component.rs` 的模块文档**自己承认了这一点**：

> The trait mirrors upstream's `Component` (`packages/tui/src/tui.ts:111-134`) **minus the mouse and invalidation hooks, which this host surface does not route yet**.

这是有意识的技术债，不是疏漏。新目标要求把它还清。

**三处需要产品/架构决策，不能由实现自行决定**：

1. **`render` 的返回类型**。`Vec<StyledLine>`（pi-rust 现状）在架构上更优——模块文档的理由是"主题留在宿主侧、组件无法硬编码颜色、主题热切换自动生效"。但 TS 插件返回 ANSI 字符串。**建议**：Rust trait 保留 `Vec<StyledLine>` 为主，另加 `AnsiLines` 包装类型 + `AnsiComponent` 适配器，JS 桥接层负责把 ANSI 字符串转成 `StyledLine`。这样既保住主题正确性，又不破坏 TS 插件的形状。
2. **`handle_input` 的参数**。TS 传原始串（组件可自行解析出 Ctrl+A 等组合），pi-rust 传 `Key`。**建议**采纳 TS 形状：`handle_input(&mut self, data: &str) -> bool`，同时保留一个默认实现的 `handle_key(&mut self, key: Key) -> bool` 便利层，供 Rust 内置组件使用。**这一条是插件兼容的硬要求**——组件自行解析键串是 TS 插件的常见写法。
3. **`invalidate()` vs `dispose()`**。**建议两者都要**：`invalidate()` 对应 TS（主题变化时调用），`dispose()` 是 pi-rust 已有的清理钩子（TS 没有，属超集，不冲突）。

#### 2.5.3 组件树 vs 插槽——比签名更深的差异

比 `Component` 签名更根本的差异是**插件挂载模型**：

| | TS | pi-rust |
| --- | --- | --- |
| 挂载方式 | `tui.addChild(component)` / `VStack{children}` / `tui.showOverlay(c, opts)` | `ctx.ui.setHeader` / `setFooter` / `setWidget(key, placement)` / `set_editor_component` / `open_custom` |
| 模型 | **组件树**（`Container` 递归 `render`） | **固定插槽**（5 个位置 + 自定义覆盖层） |
| 布局 | 插件用 `VStack`/`HStack`/`Box`/`ScrollView` 自行组合 | 宿主决定位置（`WidgetPlacement`），插件无从参与 |
| 焦点 | `tui.setFocus(c)` + `Focusable` + `CURSOR_MARKER` | 无焦点协议 |
| 覆盖层尺寸 | `OverlayOptions` 支持 `SizeValue = number \| \`${number}%\`` 与 9 种 `OverlayAnchor` | `OverlayAnchor` 有，`SizeValue`/百分比/`OverlayOptions` 缺失 |

pi-rust 的 `extension_ui.rs`（964 L）是一个**状态容器**（`has_header` / `has_footer` / `widget_keys` / `custom_open`…），不是组件树。这意味着 **TS 插件里凡是用 `VStack` 组合多个组件、或用 `ScrollView` 自管滚动的写法，在 pi-rust 上无从表达**。

→ 这正是新目标"一比一复刻 + 彻底改造"的核心工作量：**把插槽模型升级为组件树模型**（同时保留插槽 API 作为便利层，不让已有插件失效）。

---

## 3. 彻底改造方案

> **目标修订（2026-09-24）**：本节不只是"模块化"，而是"**以 TS pi-tui 文件结构为镜像的彻底改造**"，并以**插件兼容**为硬约束。原报告的 5 子 crate 拆分方案保留为**物理布局**（§3.2-3.6），但**逻辑结构必须对齐 TS**——见 §3.1 与新增的 §3.14 / §3.15。

### 3.1 目标

**三层目标，优先级从高到低**：

1. **插件兼容（硬约束）**：TS 插件依赖 `packages/tui/src/index.ts` 导出的 ~145 个名字。改造后这些名字的形状必须能一一对应到 Rust 侧类型，使 JS 桥接层是**机械转换**而非"重新实现"。基准见 §2.5，清单见 §3.15。
2. **一比一文件镜像（组织原则）**：Rust 侧按 TS 的文件边界划分模块——**TS 有 `components/scroll-view.ts`，Rust 就有 `components/scroll_view.rs`**。不用"更好的"结构替代，先对齐再优化（见 §3.14 的 42 文件映射表）。
3. **物理拆分（落地手段）**：把镜像出的结构装进 5 个子 crate，使编译期依赖可约束、单文件体量可控。

**为什么以"镜像"而不是"重新设计"为原则**：插件兼容要求形状对齐；而形状对齐的最省力实现就是**文件与 API 一一对应**。任何"我们换个更好的抽象"都会在 JS 桥接层变成逐字段的翻译代码，且随 TS 上游漂移而持续失配。

物理布局仍为 5 个子 crate：

```
crates/
├── pi-tui                ← 当前 crate：保持为"门面"
├── pi-tui-core           ← 新增：纯 trait / 数据 / 错误
├── pi-tui-render         ← 新增：StyledLine / Theme / render pipeline
├── pi-tui-components     ← 新增：内置组件（MessageView / Prompt / Selector / …）
├── pi-tui-editor         ← 新增：Editor / KillRing / UndoStack / VisualLayout / Autocomplete
└── pi-tui-app            ← 新增：App（编排）+ 交互循环
```

`pi-coding-agent` 仅依赖 `pi-tui` 门面 + `pi-tui-app`（不再依赖 `pi-tui` 内部模块）。

### 3.2 `pi-tui-core`：trait 与数据

提取 `crates/pi-tui/src/` 中所有"纯定义"模块：

| 来源（现） | 目标（新） | 内容 |
| --- | --- | --- |
| `component.rs` | `core/component.rs` | `Component` trait、`TextComponent`、`WidgetPlacement`、`OverlayAnchor`、`CustomOptions`、`CustomHandle` |
| `input.rs` | `core/input.rs` | `InputEvent`、`Key`、`KeyCode`、`KeyModifiers`、`MouseButton`、`MouseGesture`、parse 函数 |
| `styled.rs`（仅类型） | `core/styled.rs` | `SpanStyle`、`StyledLine`、`StyledSpan`（不含 write 函数） |
| `theme.rs`（仅类型） | `core/theme.rs` | `ThemeColor`、`ThemeBg`、`ColorValue`、`ColorMode` |
| `width.rs` | `core/width.rs` | 已有 ✅ |
| `kill_ring.rs` | `editor/…/kill_ring.rs`（依赖 visual） | 移到编辑器 crate |
| `undo_stack.rs` | 同上 | 同上 |
| `word_navigation.rs` | 同上 | 同上 |
| `mouse_region.rs` | `core/mouse.rs` | `MouseRegion`、`MouseRegionPoint` |
| `keybindings.rs`（仅类型） | `core/keybindings.rs` | `KeybindingDefinition`、`KeybindingsConfig`、`KeybindingsManager` |
| `error.rs`（新增） | `core/error.rs` | `TuiError` |

依赖：**零外部 crate**（仅 std）。

### 3.3 `pi-tui-render`：渲染管线

| 来源（现） | 目标（新） | 内容 |
| --- | --- | --- |
| `styled.rs`（写入部分） | `render/styled.rs` | `write_styled_line`、`write_styled_line_hyperlinked`、`write_styled_line_ellipsized`、`write_plain_row`、`buffer_row_text`、`themed_text` |
| `theme.rs`（解析部分） | `render/theme.rs` | `Theme`、`ThemeController`、`builtin_theme`、`load_theme`、`get_fg_ansi`、`bg` |
| `markdown.rs` | `render/markdown.rs` | `render_markdown*` |
| `highlight.rs` | `render/highlight.rs` | syntax highlight |
| `latex.rs` | `render/latex.rs` | 已有 ✅ |
| `image.rs` + `terminal_image.rs` | `render/image.rs` + `render/terminal_image.rs` | Kitty/iTerm2 |
| `hyperlink.rs` | `render/hyperlink.rs` | OSC 8 链接 |
| `terminal_title.rs` | `render/title.rs` | OSC 0 |
| `styles.rs` | `render/styles.rs` | `SelectListStyles` |
| `locale.rs` | `render/locale.rs` | `Locale`、`STARTUP_HINTS` |
| `tree.rs` | `render/tree.rs` | `TreeItem` / `TreeRow` |
| `visual_text.rs` | `render/visual_text.rs` | `VisualLayout` 计算 |

依赖：`pi-tui-core` + `unicode-width` + `unicode-segmentation`。

### 3.4 `pi-tui-editor`：编辑器子系统

| 来源（现） | 目标（新） | 内容 |
| --- | --- | --- |
| `editor.rs` | `editor/mod.rs` + 拆分 | 见下 |
| `kill_ring.rs` | `editor/kill_ring.rs` | 已有 ✅ |
| `undo_stack.rs` | `editor/undo_stack.rs` | 已有 ✅ |
| `word_navigation.rs` | `editor/word_navigation.rs` | 已有 ✅ |
| `history_store.rs` | `editor/history.rs` | 已有 ✅ |
| `visual_text.rs`（仅被 editor 用的部分） | `editor/visual_text.rs` | 已部分在 `render/` |
| `search.rs`（query/normalize 部分） | `editor/search.rs` | query 解析 |
| `fuzzy.rs` | `editor/fuzzy.rs` | fuzzy match |
| `autocomplete.rs`（provider trait） | `editor/autocomplete.rs` | provider 接口 |
| `slash_menu.rs` | `editor/slash_menu.rs` | `/` 触发 |

`editor.rs` 拆分计划（4,515 L → 6 个文件，目标每个 < 800 L）：

```
editor/
├── mod.rs               ← Editor struct（编排）、EditorAction、HistoryEntry
├── buffer.rs            ← EditorBuffer（char Vec + 选区 + 多光标）
├── cursor.rs            ← Cursor 移动、wrap affinity、preferred_col、visual_candidates
├── history.rs           ← HistoryStore 集成、history browsing
├── paste.rs             ← PasteBurst、bracketed paste 解析、paste markers
├── chips.rs             ← ImageChip、CHIP_CHAR 渲染
├── bash.rs              ← parse_bash_command、is_bash_mode、BashCommand
├── keymap.rs            ← 键位分派到 cursor/buffer/history/paste/chips
├── render.rs            ← render_to_buffer、visual layout 计算
└── selection.rs         ← 选区/多光标逻辑
```

依赖：`pi-tui-core` + `pi-tui-render`（仅类型）。

### 3.5 `pi-tui-components`：内置组件

| 来源（现） | 目标（新） | 内容 |
| --- | --- | --- |
| `message.rs` | `components/message.rs` | `MessageView`、`MessageItem`、`Role`、`ToolBlock`、`PendingMessageKind` |
| `prompt.rs` | `components/prompt.rs` | `Prompt`、`PromptAction` |
| `selector.rs` | `components/selector.rs` | `Selector`、`SelectorItem`、`SelectorLayout`、`SelectorAction` |
| `settings.rs` | `components/settings.rs` | `SettingsList`、`SettingsAction`、`SettingItem` |
| `dialog.rs` | `components/dialog.rs` | `Dialog`、`DialogAction`、`DialogKind` |
| `status.rs` | `components/status.rs` | `StatusBar`、`StatusData`、`BusyIndicator` |
| `slash_menu.rs` | `components/slash_menu_widget.rs` | `SlashMenuWidget` |
| `loader.rs` | `components/loader.rs` | `Spinner`、`format_elapsed`、`indicator_line` |
| `tree.rs`（render 部分） | `components/tree_view.rs` | `tree_selector_items`、`flatten_tree` |
| `extension_ui.rs` | `components/extension_ui.rs` | `ExtensionUi`、`ChromeLayout`、`plan_chrome` |

每个组件文件 < 1500 L，且实现 **`Component` trait**（不再用 `render_to_buffer` 绕过）。

### 3.6 `pi-tui-app`：编排层

| 来源（现） | 目标（新） | 内容 |
| --- | --- | --- |
| `app.rs`（主体） | `app/mod.rs` + 拆分 | 见下 |
| `keybindings.rs`（全局注册） | `app/keymap.rs` | 注册/匹配 |
| `search.rs`（UI 部分） | `app/search.rs` | 搜索覆盖层 |
| `extension_ui.rs`（桥接部分） | `app/extension_bridge.rs` | `set_header` / `set_footer` / … |
| `extension_ui.rs`（几何记录部分） | `app/geometry.rs` | 见 §3.7 |
| `clipboard.rs` | `app/clipboard.rs` | 已有 ✅ |
| `mouse_region.rs`（消费侧） | `app/mouse.rs` | hit-test 路由 |
| `terminal_image.rs`（能力探测） | `app/capabilities.rs` | `get_capabilities` |

`app.rs` 拆分计划（7,681 L → 8 个文件，目标每个 < 1200 L）：

```
app/
├── mod.rs                ← App struct 骨架（仅字段，不超过 500 L）
├── render.rs             ← render_to_buffer、render_snapshot、frame diff
├── input_keys.rs         ← step_key、step_key_at、所有 handle_*_key
├── input_mouse.rs        ← 所有鼠标事件路由
├── geometry.rs           ← LayoutFrame / LayoutBox / LayoutRect 类型 + 几何记录器
├── extensions.rs         ← set_header/set_footer/set_widget/open_custom/close_custom
├── agent.rs              ← drain_agent_events、submit、turn_busy、cancel_token
└── lifecycle.rs          ← exit_requested、last_clear_at、terminal_title 状态机
```

### 3.7 引入 layout 抽象（对齐 pi-tui `tui-plan.md`）

新增 `crates/pi-tui-app/src/geometry.rs`：

```rust
pub struct LayoutRect { pub x: u16, pub y: u16, pub width: u16, pub height: u16 }
pub struct LayoutBox {
    pub component: ComponentId,
    pub rect: LayoutRect,
    pub clip: LayoutRect,
    pub children: Vec<LayoutBox>,
    pub parent: Option<Box<LayoutBox>>,
}
pub struct LayoutFrame {
    pub root: LayoutBox,
    pub width: u16,
    pub height: u16,
    pub primary_scroll: Option<ScrollViewId>,
    pub scroll_views: HashMap<ScrollViewId, LayoutRect>,
}
```

把当前的 21 个原子几何字段（`app.rs` 19 + `message.rs` 2）收敛成单个 `Arc<Mutex<LayoutFrame>>`（在单线程场景下其实可以 `Rc<RefCell<LayoutFrame>>`，但保留 `Arc<Mutex<>>` 以兼容 future 异步化）。hit-test 改成遍历 `LayoutFrame`。

**关键变化**：render 后返回 `LayoutFrame`，App 持有它；事件路由先在 `LayoutFrame` 上做 hit-test，再 dispatch。

### 3.8 引入 `AppConfig` 注入

当前 `AppConfig` 是 104 行（[app.rs:560-663](pi-rust/crates/pi-tui/src/app.rs)），但仍以构造函数传入。建议改为：

```rust
pub struct AppDeps {
    pub theme: Arc<dyn ThemeProvider>,
    pub keymap: Arc<KeybindingsManager>,
    pub event_source: Box<dyn AgentEventSource>,
    pub image_renderer: Arc<dyn ImageRenderer>,
    pub platform: Box<dyn PlatformHooks>,
}
```

`App::new(deps: AppDeps, config: AppConfig)` 取代当前直接 `App::new(agent, config)`。便于：
- 测试时注入 fake event source；
- 主题热切换只换 `theme` 字段；
- 平台特性（hyperlink / image / paste）按需注入。

### 3.9 渲染路径统一

废除 `render_to_buffer(area, buf)` 与 `Component::render(width) -> Vec<StyledLine>` 的双重路径。统一为：

```rust
trait Component {
    fn render(&self, ctx: &RenderContext) -> RenderResult;
    fn handle_input(&mut self, key: Key, ctx: &mut InputContext) -> InputResult;
    fn invalidate(&mut self) {}
    fn dispose(&mut self) {}
}
```

`RenderContext { width, theme, line_offset, scroll_view, dpi, … }`
`RenderResult { lines: Vec<StyledLine>, dirty: Rect, … }`

内置组件与扩展组件走同一路径；`App::render_to_buffer` 调 `RenderContext` 路径，把 `StyledLine` 写到 `Buffer`。

### 3.10 拆分 `interactive.rs`

`interactive.rs` **9,432 L**（非测试部分 131 个函数、仅 6 个 `pub fn`、7 个 `impl` 块）拆为：

```
interactive/
├── mod.rs               ← run_interactive 入口 + InteractiveOptions
├── autocomplete.rs      ← extension_autocomplete_commands + ComposerAutocomplete
├── models.rs            ← open_model_selector / open_scoped_models / cycle_catalog
├── thinking.rs          ← sync_thinking_for_model
├── status.rs            ← sync_status_metrics
├── bash.rs              ← BashRunner + push_bash_block
├── persistence.rs       ← session_directory / persist_default_thinking_level
├── exit.rs              ← InteractiveExit + write_exit_output + format_resume_command
└── events.rs            ← handle_dequeue + agent event → App 状态映射
```

每个文件 < 1,200 L（9,432 L / 9 ≈ 1,048 L 均值）；其中 `events.rs` 与 `bash.rs` 大概率仍需二次拆分。agent-core 的扩展 (`extensions/`) 子模块保持不动。

### 3.11 统一菜单抽象：`MenuState<T>`（来自 §2.1-C）

新增 `crates/pi-tui-components/src/menu.rs`，用**一个泛型**替换现在四套菜单实现：

```rust
pub struct MenuState<T> { /* items, filter, cursor, scroll, free_text */ }

pub enum MenuAction<T> {
    Nothing,
    Chosen(T),
    ChosenRaw(String),   // 允许自由输入时返回原始文本（如 `/model <name>`）
    Filled(String),      // Tab 补全
    Cancel,
}

impl<T> MenuState<T> {
    pub fn handle_key(&mut self, k: Key) -> MenuAction<T>;
    pub fn with_free_text(self, on: bool) -> Self;
    pub fn rank(label: &str, desc: &str, query: &str) -> u8;  // 0..3 相关性分层
}
```

要点（逐条照抄 nanopi `render/menu.rs` 的设计）：

1. **只拥有状态，不绘制**。绘制由调用者给一个 `Rect`：`draw_menu(buf, area, &m, title)` —— 一个函数画全部菜单，因为状态结构相同。
2. `rank()` 四级分层：`0` 裸标签精确 > `1` 前缀 > `2` 标签包含 > `3` 描述包含。nanopi 为此写了注释记录 `/settings` 预选错位的具体 bug，本移植应先读它。
3. `with_free_text()` 用于 `/model` 这类既选又输的场景。
4. **测试是行为单元**（排序、过滤、`ChosenRaw`、`Cancel`），不是帧快照——这正好补上 §1.7 缺失的安全网。

**替换映射**：

| 现组件 | 行数 | 迁移到 |
| --- | ---: | --- |
| `selector.rs` | 1,426 | `MenuState<T>` + 薄适配层 |
| `settings.rs` | 892 | `MenuState<SettingsRow>` |
| `dialog.rs` | — | `MenuState<DialogKind>`（或保留，若语义差异大） |
| `slash_menu.rs` | — | `MenuState<SlashCmd>` |

**注意**：`SelectorLayout`（`selector.rs` 内的多列/网格布局）与 `SettingsList` 的分组标题是**真实的语义差异**，不能强行压平。建议先做 `selector` ← `slash_menu` 这一对（语义最接近、已验证 7 处复用），跑通 snapshot 后再评估 `settings`。这是一个**渐进替换**，不是一次性删除四个组件。

### 3.12 引入 raw-mode 感知的通知队列（来自 §2.1-D）

新增 `crates/pi-tui-render/src/raw_tty.rs`，直接对应 nanopi 的同名模块：

```rust
enum Destination { Queue, Stderr }              // 类型保证不会同时写两处
static RAW: AtomicBool;                          // raw mode 是否成立
static PENDING: LazyLock<Mutex<VecDeque<String>>>;
pub const MAX_PENDING: usize = 512;              // 溢出丢最旧

pub fn set_raw_mode(on: bool);
pub fn note(msg: &str);                          // 按模式路由
pub fn drain() -> Vec<String>;                   // TUI 循环每 tick 调
pub fn flush_pending_to_stderr();                // teardown 调，保证不丢
```

然后：

1. `interactive.rs` 进入 raw mode 后调 `set_raw_mode(true)`，退出时 `false` + `flush_pending_to_stderr()`；
2. **替换 `crates/pi-coding-agent/src/` 内 38 处裸 `eprintln!`**（`config.rs:953`、`print_mode.rs:431/1055`、`text_fallback.rs:39/46/91`、`external_editor.rs:127-198`、`skills.rs:1130/1162`…）为 `note!`；
3. App 的帧循环每 tick `drain()`，把这些行插进消息区——**不能画进 ratatui buffer 之外**，否则会被下一帧重绘擦掉。

`note!` 宏作为薄包装导出，替换时是纯机械替换。**这是 §5 里优先级最高的一项**：它是本报告唯一一条有明确缺陷支撑（alt-screen 破帧）、且改动可机械验证的改造。

### 3.13 显式化 `Viewport` 子系统

不改变视口模式（见 §1.10 的权衡），但把当前藏在 `App` 里的滚动能力收敛成一个命名子系统：

```rust
pub struct Viewport {
    pub total_rows: usize,        // 消息区总行数
    pub visible_rows: usize,      // 本帧可视行数（替代 viewport_height atomic）
    pub scroll_offset: usize,     // 当前顶端行（替代 scroll_to_end / truncated_above）
    pub reserved_cols: u16,       // 滚动条占位（替代 viewport_reserved atomic）
}
```

把 `app.rs` 的 `viewport_height` / `viewport_reserved` / `scroll_to_end` / `truncated_above` 四个 atomic 收敛为 `Viewport` 一个字段，其方法（`scroll_to_end()`、`is_truncated_above()`、`scrollbar_rows()`）取代散落的读写点。这**不是**功能改动，只是把隐式状态变成有名类型——是 §3.7 的 `LayoutFrame` 的补充（`LayoutFrame` 管"组件在哪"，`Viewport` 管"看到哪一段"）。

### 3.14 TS → Rust 1:1 文件映射表（42 文件）

**原则**：左侧 TS 文件是**权威命名**；中间是目标 Rust 路径；右侧是 pi-rust 现有文件（`—` 表示需新建，⚠️ 表示现有但 API 不兼容需重写公共面）。

| # | TS 文件（`packages/tui/src/`） | 行数 | 目标 Rust 路径 | pi-rust 现状 |
| --: | --- | --: | --- | --- |
| 1 | `index.ts` | 156 | `pi-tui/src/lib.rs`（re-export 全部） | ⚠️ 需核对导出面 |
| 2 | `tui.ts` | 1456 | `components/tree.rs`：`Component`/`Container`/`TUI`/`ViewportTUI`/`Focusable`/`CURSOR_MARKER`/`composite_tui_line` | ⚠️ `component.rs` 缺一半 |
| 3 | `tui-alt-screen.ts` | 1721 | `components/alt_screen.rs` | ⚠️ 等价逻辑在 `app.rs` |
| 4 | `tui-main-screen.ts` | 655 | `components/main_screen.rs` | — |
| 5 | `terminal.ts` | 547 | `core/terminal.rs`：`Terminal` trait + `ProcessTerminal` | — |
| 6 | `stdin-buffer.ts` | 444 | `core/stdin_buffer.rs`：`StdinBuffer` | — |
| 7 | `utils.ts` | 1337 | `core/utils.rs`：`visible_width`/`slice_by_column`/`strip_terminal_sequences`/`truncate_to_width`/`wrap_text_with_ansi`/`get_osc8_link_at_column` | ⚠️ 散在 `width.rs`/`styled.rs`，未公开 |
| 8 | `keys.ts` | 1401 | `core/keys.rs`：`Key`/`KeyId`/`parse_key`/`matches_key`/`is_key_release`/`is_key_repeat`/`is_kitty_protocol_active`/`decode_kitty_printable` | ⚠️ `input.rs` 名字不同 |
| 9 | `keybindings.ts` | 320 | `core/keybindings.rs` + `TUI_KEYBINDINGS` | ⚠️ `keybindings.rs` 966 L，形状不同 |
| 10 | `autocomplete.ts` | 826 | `core/autocomplete.rs`：`AutocompleteProvider` trait / `CombinedAutocompleteProvider` / `SlashCommand` | ⚠️ `autocomplete.rs` |
| 11 | `fuzzy.ts` | 137 | `core/fuzzy.rs` | ✅ |
| 12 | `word-navigation.ts` | 117 | `editor/word_navigation.rs` | ✅ |
| 13 | `kill-ring.ts` | 61 | `editor/kill_ring.rs` | ✅ |
| 14 | `undo-stack.ts` | 28 | `editor/undo_stack.rs` | ✅ |
| 15 | `latex.ts` | 1394 | `render/latex.rs` | ✅ |
| 16 | `layout.ts` | 449 | `render/layout.rs`：`LayoutRect`/`LayoutBox`/`LayoutFrame` 计算 | — **核心缺口** |
| 17 | `layout-node.ts` | 51 | `render/layout_node.rs`：`LayoutNode` | — **核心缺口** |
| 18 | `terminal-image.ts` | 696 | `render/terminal_image.rs`：`render_image`/`encode_kitty`/`encode_iterm2`/`detect_capabilities` | ⚠️ 名称体系不同 |
| 19 | `terminal-colors.ts` | 73 | `render/terminal_colors.rs`：`RgbColor`/`TerminalColorScheme`/`parse_osc11_background_color` | — |
| 20 | `native-platform.ts` | 63 | `core/native_platform.rs`：`get_native_clipboard` | ⚠️ `clipboard.rs` |
| 21 | `native-modifiers.ts` | 13 | （并入 `core/keys.rs`） | — |
| 22 | `native-module-path.ts` | 31 | （不适用；Rust 无 native addon） | — |
| 23 | `alt-screen-search.ts` | 327 | `components/alt_screen_search.rs` | ⚠️ 等价逻辑在 `app.rs`/`search.rs` |
| 24 | `editor-component.ts` | 74 | `core/editor_component.rs`：`EditorComponent` trait | ⚠️ `set_editor_component` 类型不同 |
| 25 | `components/editor.ts` | 2461 | `components/editor.rs` | ⚠️ `editor.rs` 4,515 L，公共面需重写 |
| 26 | `components/input.ts` | 1025 | `components/input.rs`：`Input` 组件 + `JumpDirection` | ⚠️ `input.rs` 仅类型层 |
| 27 | `components/markdown.ts` | 1015 | `components/markdown.rs`：`Markdown` **组件** + `MarkdownTheme` | ⚠️ `markdown.rs` 是函数式 |
| 28 | `components/select-list.ts` | 273 | `components/select_list.rs`：`SelectList`/`SelectItem`/`SelectListLayoutOptions`/`SelectListTheme` | ⚠️ `selector.rs` 1,426 L |
| 29 | `components/settings-list.ts` | 328 | `components/settings_list.rs`：`SettingsList`/`SettingItem`/`SettingsListTheme` | ⚠️ `settings.rs` 892 L |
| 30 | `components/scroll-view.ts` | 224 | `components/scroll_view.rs`：`ScrollView` + 3 个 Options 类型 | — **核心缺口** |
| 31 | `components/v-stack.ts` | 33 | `components/v_stack.rs`：`VStack`（`StackChild`/`StackEntry`/`StackEntryOptions`/`StackOptions` 见 #42） | — **核心缺口** |
| 32 | `components/h-stack.ts` | 44 | `components/h_stack.rs` | — **核心缺口** |
| 33 | `components/box.ts` | 167 | `components/box.rs`：`Box` | — **核心缺口** |
| 34 | `components/spacer.ts` | 28 | `components/spacer.rs` | — **核心缺口** |
| 35 | `components/text.ts` | 107 | `components/text.rs`：`Text` | ⚠️ `TextComponent` |
| 36 | `components/truncated-text.ts` | 65 | `components/truncated_text.rs` | — |
| 37 | `components/image.ts` | 127 | `components/image.rs`：`Image` **组件** + `ImageOptions`/`ImageTheme` | ⚠️ `image.rs` 函数式 |
| 38 | `components/loader.ts` | 101 | `components/loader.rs`：`Loader`/`LoaderIndicatorOptions` | ⚠️ `loader.rs` 仅 frame 表 |
| 39 | `components/cancellable-loader.ts` | 40 | `components/cancellable_loader.rs`：`CancellableLoader` | — |
| 40 | `components/mouse-region.ts` | 33 | `components/mouse_region.rs`：`MouseRegion`/`MouseRegionHandler` | ⚠️ `mouse_region.rs` API 不同 |
| 41 | `components/alt-screen-flash.ts` | 51 | `components/alt_screen_flash.rs` | — |
| 42 | `components/stack.ts` | 154 | `components/stack.rs`：`StackChild`/`StackOptions` 等共享基座（与 #31 同一缺口簇） | — |

**缺口统计**（按"需新建"计）：上表 42 行里，**15 个目标文件需新建**（#4/5/6/16/17/19/30-34/36/39/41/42；#21 并入 `core/keys.rs`、#22 不适用），其中 **7 个是核心缺口**——`layout.ts`、`layout-node.ts`、`scroll-view.ts`、`v-stack.ts`、`h-stack.ts`、`box.ts`、`spacer.ts`。这 7 个正是**插件组合 UI 的全部原语**；`stack.ts`（#42）是它们的共享基座，与 #31 同属一个缺口簇。另外 **25 个已有对应物**，但除 5 个（✅ #11-15）外全部（⚠️）需重写公共面。

> **注意**：`nanopi/src/render/` 里没有这些布局原语（nanopi 用内联模式 + 固定 5 约束，不需要），所以**布局原语无法从 nanopi 借鉴，必须从 TS 一比一移植**。这是本报告与之前版本的一个关键结论修正。

### 3.15 插件兼容性改造清单（硬约束）

按依赖顺序：

**第一层：契约类型（必须先做，其余都依赖）**

1. `core/keys.rs`：`Key` 改为 TS 形状——`Key { id: KeyId, ... }` + `parse_key(&str)` + `matches_key(&Key, KeyId) -> bool` + `is_key_release/is_key_repeat` + Kitty 协议标志位。现有 `input.rs` 的 `Key{code,modifiers}` 保留为内部实现细节。
2. `core/utils.rs`：公开 6 个函数（`visible_width`/`slice_by_column`/`strip_terminal_sequences`/`truncate_to_width`/`wrap_text_with_ansi`/`get_osc8_link_at_column`）。**这几个函数是 TS 插件最常直接调用的工具**，必须与 TS 同名同语义。
3. `core/terminal.rs`：`Terminal` trait（`write`/`size`/`clear`/`cursor`…）+ `ProcessTerminal`。当前 pi-rust 直接持有 `ratatui::Terminal`，插件无从适配。

**第二层：`Component` 契约对齐（§2.5.2 的三个决策）**

4. `Component` 增加 `handle_mouse`、`wants_key_release`、`invalidate`；`handle_input` 参数改为 `&str`（保留 `handle_key` 便利层）。
5. `Container` 组件（`children: Vec<Box<dyn Component>>` + `add_child`/`remove_child` + 递归 `render`）。
6. `CURSOR_MARKER` 常量 + `Focusable` trait + `is_focusable`。
7. `composite_tui_line` — 覆盖层合成工具（插件画覆盖层时用）。

**第三层：组合原语（7 个核心缺口）**

8. `layout.rs` + `layout_node.rs`：`LayoutRect`/`LayoutBox`/`LayoutFrame`/`LayoutNode`。**这是全部布局组件的地基**，也是 §3.7 计划的内容——两者合并为一件事。
9. 布局组件：`VStack`/`HStack`/`Box`/`Spacer`/`ScrollView`（含 `StackEntry`/`StackEntryOptions`/`ScrollView*Options`）。

**第四层：内置组件（插件可实例化）**

10. 把函数式的 `markdown.rs` / `image.rs` 包成 `Markdown` / `Image` **组件**（保留原函数作内部实现）。
11. 重写 `SelectList` / `SettingsList` 的公共面以匹配 TS（内部可继续用现有实现）。**注意**：这与 §3.11 的 `MenuState<T>` 合并**不冲突**——`MenuState<T>` 是内部状态层，`SelectList` 是 TS 兼容的公共面，前者服务后者。
12. 新建 `TruncatedText` / `Loader` / `CancellableLoader` / `AltScreenFlash`。

**第五层：挂载模型（插槽 → 组件树）**

13. `extension_ui.rs` 从"状态容器"升级为：内部持有 `VStack` 根，`set_header`/`set_footer`/`set_widget` **变为向组件树插入 `StackEntry` 的便利方法**（保持 API 不变，`WidgetPlacement` 映射到 `StackEntryOptions`）。
14. 新增 `TUI` trait（`add_child`/`set_focus`/`show_overlay`/`add_input_listener`…），`App` 实现它。这是让 TS 插件从"只能填插槽"变为"能组合任意 UI"的关键。
15. `OverlayOptions`/`SizeValue`/`OverlayHandle`/`OverlayBounds`/`OverlayMargin` 补齐。

**第六层：桥接验证**

16. 建立 **TS 契约测试**：把 `index.ts` 的 145 个导出名做成清单，逐一断言 Rust 侧存在对应符号（编译期 + 一份 `docs/PLUGIN_COMPAT.md` 对照表）。这样 TS 上游新增导出时能被发现，而不是静默失配。

**优先级说明**：第一、二层是插件兼容的**最小可行集**（不做完，任何 TS 插件都无法在 Rust 上跑）；第三层是**组合能力的门槛**；第四、五层决定"插件能否画出复杂 UI"；第六层防止长期漂移。

---

## 4. 执行顺序（建议）

按 **风险 / 收益** 排序。每一步独立可编译、可测试。

### Phase 0 — 准备（2 周）
1. 写 `docs/MODULARIZATION.md`（本报告精简版）作为契约。
2. `cargo doc` 当前结构作为基线。
3. **为 189 处帧快照建立"内部行为对照层"**——现有测试锁定的是像素，重构时无法小步验证。先为 `App` 的 19 个 atomic 字段读/写、`handle_*_key` 各方法、`drain_agent_events` 写行为级单测，作为拆分时的安全网。
4. 建立 CI 门禁：`cargo test -p pi-tui` 与 `cargo test -p pi-coding-agent` 全绿后才能提交。

### Phase 0.5 — 独立收益项（1-2 周，**不依赖任何 crate 拆分**）

这三项可以立刻做，且各自独立可验证。它们也是 §5 的第 5-7 项。

1. **引入 raw-mode 通知队列**（§3.12），替换 38 处裸 `eprintln!`。有明确缺陷支撑（alt-screen 破帧），有测试完备的蓝本，机械替换。
2. **抽出 `Viewport` 类型**（§3.13），把 4 个滚动相关 atomic 收敛为一个字段。
3. **用 `MenuState<T>` 合并 `selector` + `slash_menu`**（§3.11），先做语义最接近的一对。

> 若之后决定暂缓整个改造计划，这三项的收益仍然独立成立。

### Phase 1 — 契约对齐层（4-6 周）**插件兼容的最小可行集**

对应 §3.15 第一、二层。**不做完这一层，任何 TS 插件都跑不起来。**

1. `core/keys.rs`：`Key`/`KeyId`/`parse_key`/`matches_key`/`is_key_release`/`is_key_repeat`/`is_kitty_protocol_active`/`decode_kitty_printable`。
2. `core/utils.rs`：公开 `visible_width`/`slice_by_column`/`strip_terminal_sequences`/`truncate_to_width`/`wrap_text_with_ansi`/`get_osc8_link_at_column`。
3. `core/terminal.rs`：`Terminal` trait + `ProcessTerminal`。
4. `Component` 契约对齐：加 `handle_mouse`/`wants_key_release`/`invalidate`，`handle_input` 参数改 `&str` 并保留 `handle_key` 便利层（§2.5.2 的三个决策需先定）。
5. `Container` + `CURSOR_MARKER` + `Focusable` + `is_focusable` + `composite_tui_line`。
6. **建立 TS 契约测试**（§3.15 第六层的雏形）：把 `index.ts` 导出名做成清单，断言已对齐项。

### Phase 2 — 布局原语（3-4 周）**插件组合能力的门槛**

对应 §3.15 第三层。**这 7 个文件在 nanopi 里没有对应物，必须从 TS 一比一移植。**

1. `render/layout.rs` + `render/layout_node.rs`：`LayoutRect`/`LayoutBox`/`LayoutFrame`/`LayoutNode`。
2. `components/stack.rs` + `v_stack.rs` + `h_stack.rs`：`VStack`/`StackEntry`/`StackEntryOptions`。
3. `components/box.rs` + `spacer.rs` + `scroll_view.rs`：`Box`/`Spacer`/`ScrollView` 及 3 个 Options 类型。
4. 与 Phase 0.5 的 `Viewport` 对接（`LayoutFrame` 管"组件在哪"，`Viewport` 管"看到哪一段"）。

### Phase 3 — 内置组件公共面（3-4 周）

对应 §3.15 第四层。**只重写公共面，内部实现可保留。**

1. `markdown.rs` / `image.rs` 包成 `Markdown` / `Image` 组件（原函数降为内部实现）。
2. `SelectList` / `SettingsList` 公共面改为 TS 形状（内部可继续用现有实现，`MenuState<T>` 服务其状态层）。
3. 新建 `TruncatedText` / `Loader` / `CancellableLoader` / `AltScreenFlash`。

### Phase 4 — 挂载模型：插槽 → 组件树（4 周）

对应 §3.15 第五层。**这是"彻底改造"的语义核心。**

1. 新增 `TUI` trait（`add_child`/`remove_child`/`set_focus`/`show_overlay`/`hide_overlay`/`add_input_listener`…），`App` 实现它。
2. `extension_ui.rs` 内部改为持有 `VStack` 根；`set_header`/`set_footer`/`set_widget` **变为向组件树插入 `StackEntry` 的便利方法**（API 不变，`WidgetPlacement` → `StackEntryOptions`）。
3. 补齐 `OverlayOptions`/`SizeValue`/`OverlayHandle`/`OverlayBounds`/`OverlayMargin`。
4. `App` 的渲染改为遍历组件树（取代 `render_to_buffer` 里的硬编码 `paint_*`）。

### Phase 5 — 物理拆分：5 子 crate（4-5 周）

按 §3.14 的 42 文件镜像表落位：

1. `pi-tui-core` / `pi-tui-render` / `pi-tui-components` / `pi-tui-editor` / `pi-tui-app` 建 crate，迁移文件。
2. `pi-tui/src/lib.rs` 按 `index.ts` 的导出面 re-export。
3. `App` 字段收敛到 `LayoutFrame` + `Viewport` + 状态机；`app.rs` 拆为 `render/input_keys/input_mouse/geometry/extensions/agent/lifecycle`。
4. 引入 `AppDeps` 注入。
5. （可选）引入 `KeyAction` 枚举，把 `step_key` 改造为"返回动作、由 loop 执行"（§2.1-B，来自 nanopi）。

### Phase 6 — `editor.rs` 重写与拆分（3-4 周）

1. 公共面改为 TS `Editor` 形状（`EditorOptions`/`EditorTheme`/`EditorComponent`）。
2. 内部拆为 `buffer/cursor/paste/chips/bash/keymap/render/selection`。

### Phase 7 — `interactive.rs` 拆分（2 周）

1. 按 §3.10 拆分。
2. 把"应该在 agent-core"的能力下沉到 `pi-agent-core` 的 `tui-bridge` trait。

### Phase 8 — 桥接验证与收尾（2-3 周）

1. **完成 TS 契约测试**（§3.15 第六层）：`index.ts` 的 ~145 个导出逐条断言，产出 `docs/PLUGIN_COMPAT.md` 对照表。
2. 端到端验证：把 2-3 个真实 TS 扩展（用 `VStack`/`ScrollView`/`showOverlay` 的）跑在 Rust TUI 上。
3. `docs/ARCHITECTURE.md` 更新为新结构图。
4. 增加每个子 crate 的集成测试。

总工期估计：**26-33 周**（含 Phase 0.5 与新增的契约/布局/挂载三个阶段）。Phase 0.5 与 Phase 8 的第 2 项可与 pi-rust 其他工作并行；Phase 1-4 是硬依赖链，**不宜并行**（契约未定就动布局会返工）。

---

## 5. 立即可做的 9 件事（小步快跑）

按"是否直接推进插件兼容"排序。**第 1-4 项是插件兼容的前置，第 5-7 项是独立收益项。**

1. **写 `docs/PLUGIN_COMPAT.md` 的空白骨架**——把 `index.ts` 的 ~145 个导出名逐条列出，标注"已对齐/需重写/需新建"（§2.5.1 的表即可作初稿）。这是后续所有工作的清单，零代码但定义了"完成"的含义。
2. **对齐 `keys.rs` 的形状**（§3.15 第 1 项）。TS 插件的键处理依赖 `Key`/`parseKey`/`matchesKey` 的确切语义；这是最底层的契约。
3. **公开 `utils.rs` 的 6 个函数**（§3.15 第 2 项）。`visible_width`/`slice_by_column`/`truncate_to_width`/`wrap_text_with_ansi` 等是 TS 插件最常直接调用的工具，目前散在 `width.rs`/`styled.rs` 且未公开。
4. **定下 `Component` 的三个决策**（§2.5.2）：`render` 返回类型、`handle_input` 参数、`invalidate` vs `dispose`。**这是需要架构拍板的一项，先定后做。**
5. **引入 raw-mode 通知队列，替换 38 处裸 `eprintln!`**（§3.12）。当前在 alt-screen 下这些写入会直接破帧；nanopi 的 `raw_tty.rs` 是现成的、测试完备的蓝本。**机械替换，收益最确定。**
6. **用 `MenuState<T>` 合并 `selector` + `slash_menu`**（§3.11）。跑通 snapshot 后再评估 `settings` / `dialog`。收益是四套状态/绘制/测试变一套，且新测试是**行为单元**而非帧快照。
7. **抽出 `Viewport` 类型**（§3.13），把 4 个滚动相关 atomic 收敛为一个字段。
8. **`render_to_buffer` 抽出 `FrameBuffers` 结构**——把 600+ 行的渲染函数拆成 `compute_layout`、`paint_message_view`、`paint_chrome`、`paint_overlay`（Phase 4 改为遍历组件树的前置）。
9. **`interactive.rs` 抽出 `composer_autocomplete.rs` 与 `model_picker.rs`**——即使留在 `pi-coding-agent` 内部，至少拆文件。


> 建议的第 8 项（文档级、零代码）：**为编辑器与状态条写一份"显式非目标"清单**（对照 nanopi `text_buffer.rs` 的做法），作为后续重构的边界契约——"不做 multi-slot kill ring / redo / search / IME"这样的声明能让重构时的范围判断有据可依。

---

## 6. 总结

| 问题 | 严重度 | 修复路径 |
| --- | --- | --- |
| **与 TS pi-tui 契约失配：~145 导出中仅 ~20 对齐**（§2.5） | 🔴 极高 | Phase 1 契约对齐层 + `PLUGIN_COMPAT.md` |
| **插件挂载模型是插槽而非组件树**（§2.5.3） | 🔴 极高 | Phase 4：`TUI` trait + `Container` 树 |
| **缺失 7 个布局原语**（`VStack`/`HStack`/`Box`/`Spacer`/`ScrollView`/`layout`/`layout-node`）（§3.14） | 🔴 极高 | Phase 2：从 TS 一比一移植（nanopi 无可借鉴） |
| `App` god-struct（7,681 L） | 🔴 极高 | 拆 5 子 crate + 引入 `LayoutFrame` |
| 双重渲染路径 | 🔴 极高 | 统一 `Component::render` |
| **alt-screen 致滚动区手工重建**（§1.10） | 🔴 极高 | 抽 `Viewport` 子系统；内联模式为可选产品决策 |
| **38 处裸 `eprintln!` 破帧**（§2.1-D） | 🟠 高 | 引入 raw-mode 队列（有现成蓝本） |
| `editor.rs` 4,515 L 单文件 | 🟠 高 | 拆 10 个子模块 |
| 21 个原子几何字段散落（14 `AtomicU16` + 6 `AtomicUsize` + 1 `AtomicU8`） | 🟠 高 | 收敛为 `LayoutFrame` + `Viewport` |
| `interactive.rs` 9,432 L 混杂 agent 能力 | 🟠 高 | 拆 9 文件 + 下沉到 agent-core |
| 菜单抽象四套并存（§2.1-C） | 🟠 高 | 合并为 `MenuState<T>`，渐进替换 |
| 189 处帧快照锁定单体结构（重构阻力） | 🟠 高 | 拆分前先补行为单元测试 |
| 无 layout 抽象 | 🟡 中 | 按 `tui-plan.md` 实现 |
| 没有 trait 化的依赖注入 | 🟡 中 | 引入 `AppDeps` |
| 与 nanopi(Rust) / pi-tui(TS) / Martty 的设计差距 | 🟡 中 | 拆分同时对齐 |

**一句话结论**：pi-rust TUI 的四类症状（god-struct、双渲染路径、几何 atomic、帧快照测试）共享一个根因——**缺少把"状态"与"绘制"分开的中间层**。nanopi 用 ≈12,072 L 做到的事（含完整 agent 驱动），pi-rust 用 42,832 L 的组件库还没做到，差别不在功能多少，而在它把每一层都显式命名了：`MenuState<T>`（状态）、`MenuAction<T>`（返回）、`KeyAction`（返回）、`insert_before`（滚动区）、`raw_tty::note`（诊断）。

**但从新目标（插件兼容）看，还有一条比"内部整洁"更紧迫的结论**：pi-rust TUI 当前的**对外契约**与 TS pi-tui 几乎不重合——`index.ts` 约 145 个导出里只有约 20 个有形状接近的对应物（14%），而这 20 个里 `Component` 本身还有 4 处签名差异，挂载模型更是从"组件树"退化成了"5 个固定插槽"。**这意味着 TS 插件在 Rust TUI 上不是"改改就能跑"，而是"无从表达"。** 因此改造的主线不是"把 42K 行拆小"，而是**以 TS 文件结构为镜像，先把契约和 7 个布局原语补齐，再把插槽升级为组件树**——内部整洁是这个过程的自然结果，不是目的。

按 §4 的 Phase 0-8 执行后，pi-rust TUI 将从"42K 行 38 文件 god-crate、与 TS 契约 14% 重合"演化为"5 子 crate、42 文件镜像 TS、契约逐条可断言"的形态：**单文件最大体量下降 5x**（7681 → 1500），**插件兼容从 14% 提升到可验证的覆盖度**，**新增子系统能力**（`TUI` trait / `Container` 树 / `LayoutFrame` / 7 个布局原语 / `Viewport` / `AppDeps` / `MenuState<T>`）。

---

*Author: pi-rust TUI 审计 — 基于实际源码分析（2026-09-24）*
*Code examined:*
- *`pi-rust/crates/pi-tui/src/` 全部 38 文件（42,832 L）*
- *`pi-rust/crates/pi-coding-agent/src/` 顶层 28 文件（27,030 L），TUI 相关入口 `interactive.rs`（9,432 L）*
- *`pi-rust/Martty/src/` 顶层 28 文件（29,913 L），另含 `input/`、`acp/` 子目录（全树 36 文件 33,169 L）*
- *`nanopi/src/` 全部 83 文件（49,065 L），其中 TUI 层精读：`render/` 11 模块（4,406 L）、`mode/tui.rs`（6,999 L）、`keys.rs`（361 L）*
- *`packages/tui/src/` 全部 42 文件（18,653 L），公开契约以 `src/index.ts`（156 L）为准*
- *`packages/coding-agent/src/modes/interactive/` 顶层 7 文件（7,069 L），另含 `components/`、`theme/`、`assets/` 子目录*
- *既有结论 [`tui-analysis.md`](tui-analysis.md) 与 [`tui-plan.md`](tui-plan.md)*

---

## 4. 实施进展（2026-09-24 本次 session）

本节记录会话结束时实际落地的改动。状态以任务列表 `TaskList` 与 `docs/PLUGIN_COMPAT.md` 的最新摘要为准。

### 4.1 FramePacer 与 App::dirty()

参考 Martty `src/main.rs:290-325` 的帧节流模式，引入 `FramePacer`：

- **常量**：`FRAME_INTERVAL = 33ms`、`IDLE_WAIT = 50ms`
- **API**：`ready(now, dirty)` / `painted(now)` / `wait(now, dirty)` / `mark_immediate()`
- **集成**：[`pi-rust/crates/pi-coding-agent/src/interactive.rs:771-885`](pi-rust/crates/pi-coding-agent/src/interactive.rs) 的主循环里替换原 `last_render + render_interval`，首次绘制强制 `mark_immediate()`

新增 `App::dirty()` 方法（[app.rs:2342-2373](pi-rust/crates/pi-tui/src/app.rs)）：
- 合并 `turn_busy`、`paste_burst_deadline`、`dirty_redraw`、模态状态
- 新增字段 `dirty_redraw: bool`、`pending_dialogs: Vec<Dialog>`
- 新增助手 `mark_dirty()` / `clear_dirty()`

驱动循环约定（`interactive.rs`）：
- `step` / `step_paste` 返回 `Redraw` 时调用 `app.mark_dirty()`
- 成功绘制后调用 `app.clear_dirty()`

### 4.2 TS 导出对齐 100%

`docs/PLUGIN_COMPAT.md` 由 101/145（70%）推进到 145/145（100%）：

- 新文件 [`crates/pi-tui/src/ts_compat.rs`](pi-rust/crates/pi-tui/src/ts_compat.rs)（≈580 行）
- 在 `lib.rs` 增加 `pub mod ts_compat;` 与 `pub use ts_compat::{...};`
- 73 个新名字：Component trait、Container、Focusable、isFocusable、ViewportTUI、TUI、TuiAltScreen、TuiMainScreen、Overlay*、SizeValue、TuiInputListener*、TuiMouse*、ScrollView*、Stack*、Markdown、MarkdownOptions、MarkdownTheme、EditorComponent、CancellableLoader、Loader、LoaderIndicatorOptions、Spacer、Text、TruncatedText、HStack、VStack、Box、Input、JUMP_DIRECTION、JumpDirection、DefaultTextStyle、SelectListTheme、SelectListTruncatePrimaryContext、SettingsListTheme、renderLatex、RenderLatexOptions、NativeClipboard、getNativeClipboard、StdinBuffer*、parseOsc11BackgroundColor、parseTerminalColorSchemeReport、RgbColor、TerminalColorScheme、CURSOR_MARKER、compositeTuiLine、Keybinding、KeybindingDefinitions、Keybindings、TUI_KEYBINDINGS、Marked、Tokens、Token

`ts_contract.rs` 测试 3/3 通过；总测试 645/647（2 个 LUM-1332 composer drag selection 已知失败）。

### 4.3 模块化进展

| 模块 | 文件 | 行数 | 状态 |
|------|------|------|------|
| viewport | `pi-tui/src/viewport.rs` | 350 | ✅ 已抽离 |
| render | `pi-tui/src/render.rs` | 430 | ✅ 已抽离 |
| render_helpers | `pi-tui/src/render_helpers.rs` | 36213 字节 | ✅ 已抽离 |
| keys | `pi-tui/src/keys.rs` | 18640 字节 | ✅ 已抽离 |
| terminal | `pi-tui/src/terminal.rs` | 15124 字节 | ✅ 已抽离 |
| ts_compat | `pi-tui/src/ts_compat.rs` | 580 | ✅ 本次新增 |
| **app 子模块化（10 个文件）** | `pi-tui/src/app/*.rs` | 进行中 | 🟡 后台代理运行 |

`app.rs` 当前 7360 行；目标是降至 ≤ 2500 行（核心 struct + 公开 API + `render_to_buffer` 顶层入口）。

### 4.4 LUM-1332 composer drag selection

测试 `composer_click::a_drag_on_the_composer_never_starts_a_chat_log_selection` 当前失败。已识别根因：

- `App::composer_selection_text()` (app.rs:5278) 仍是桩函数永远返回 `None`
- `prompt_mouse_gesture` 的 Drag|Move 分支（app.rs:5831）硬编码 `Some(StepOutcome::Idle)`，未扩展 composer 选择

修复方案（后台代理执行中）：
- 在 `editor.rs` 的 `ChatEditor` 添加 `composer_selection_anchor`、`begin_composer_selection`、`extend_composer_selection`、`clear_composer_selection`、`composer_selection_text`
- 替换 `App::composer_selection_text()` 桩
- 在 `prompt_mouse_gesture` 的 Drag|Move 分支调用 `extend_composer_selection`
- Release 时清除 anchor

### 4.5 真实执行验证

通过 4 个 pty 场景与 39 个集成测试：

| 场景 | 路径 | 结果 |
|------|------|------|
| 打印模式 | `pi --print "echo hi"` | ✅ 5 个扩展加载，prompt 已回答 |
| 启动 + Esc 退出 | `script -q /tmp/pi-tui.log pi` | ✅ alt-screen `?1049h`、mouse modes、kitty kb `>1u`、OSC0 title 完整；`?1049l` 干净退出 |
| 粘贴突发 | `script -q /tmp/pi-tui-paste.log pi` (500 字符 + 2 行) | ✅ paste burst 路径无 panic |
| 闲置节流 | `script -q /tmp/pi-tui-idle.log pi` | ✅ FramePacer 不冻结 spinner |
| help_text_layout | 集成测试 | ✅ 3/3 |
| lum1448/1450/1455 帧测试 | 集成测试 | ✅ |
| history_file / print_mode / extension_ui / keybinding_install | 集成测试 | ✅ 36/36 |

### 4.6 与参考实现的对比总结

| 维度 | pi-rust | Martty | nanopi | TS pi-tui |
|------|---------|--------|--------|-----------|
| TUI 库总行数 | 42,832 | 29,913 | 12,072 | 18,653 |
| 最大单文件 | app.rs 7360 | app.rs 9783 | mode/tui.rs 6999 | tui-alt-screen.ts 1721 |
| 模块数（顶层） | 38+10(新增) | 28 | 83 | 42 |
| 模块化策略 | 多文件 per feature | 单文件 god-struct | 多文件 render + mode | 多文件 per feature |
| TS 导出对齐 | **100% (145/145)** | n/a | n/a | n/a |
| 帧节流 | FramePacer (33/50ms) | FramePacer 原型 | ? | differential render |
| 鼠标路由 | 5 个矩形 hit-test | 5 个矩形 hit-test | 简化版 | 5 个矩形 hit-test |
| 装饰测试 | 645/647 (99.7%) | ? | ? | n/a |
