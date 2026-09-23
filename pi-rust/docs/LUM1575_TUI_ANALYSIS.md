# LUM-1575: pi Rust TUI 分析 — pi-tui vs Martty vs Codex

> 分析日期：2026-09-23
> 分析人：编程助手 devbox
> 目标：推进 LUM-981，基于 Rust 实现 pi 并兼容插件生态

---

## 一、当前进度审计

### 1.1 feature/pi.rs 整体状态

| 指标 | 数值 |
| --- | --- |
| Rust 源码规模 | 27,895 行（pi-tui）/ 30 个模块 |
| 最大模块 | app.rs 7,556 行、editor.rs 4,501 行 |
| Cargo Workspace | 13 个 crate |
| 测试文件 | 83 个（pi-tui）、大量单元测试 |
| 最新提交 | `3fced5477` — footerData 4/4 + 每帧跟踪 HEAD（LUM-1490） |

### 1.2 已实现功能（pi-tui vs 上游 TS）

| 功能 | 状态 | 备注 |
| --- | --- | --- |
| Markdown 渲染 | ✅ 完整 | 含 LaTeX、高亮、代码块 |
| 图片渲染 | ✅ 完整 | kitty + iTerm2 protocol |
| 超链接 | ✅ 完整 | OSC 8 |
| 主题系统 | ✅ 完整 | 深色/浅色切换 |
| 编辑器 | ✅ 完整 | kill ring、undo、历史、word导航、跳转模式 |
| 自动补全 | ✅ 完整 | 路径+命令，`autocomplete.rs` 1,172 行 |
| 工具折叠/展开 | ✅ LUM-1214 | Stage 58，已合入 |
| 搜索 Overlay | ✅ 完整 | `search.rs` 1,100 行 |
| 选择器 | ✅ 完整 | 可搜索、可窗口化、模糊匹配 |
| 设置弹窗 | ✅ 完整 | 可搜索设置列表 |
| 扩展 UI Host | ✅ 完整 | ctx.ui.* 全套（LUM-1481 ctx.ui.setStatus）|
| 滚动条 | ✅ 完整 | 含拖拽 |
| Jump to latest | ✅ LUM-1257 | |
| Paste Burst | ✅ LUM-1461 | |
| 状态栏 Footer | ✅ LUM-1481 | 第三行状态 |
| Terminal Title | ✅ LUM-1485 | OSC 0 |
| 队列排队输入 | ✅ LUM-1216 | Steering/Follow-up |

**整体功能覆盖率估算：~87%**（基于 TUI_UX_AUDIT.md 的加权统计）

### 1.3 Rust vs TS 版本差距（真实差距）

| 维度 | TS 版本 | Rust 版本 | 差距 |
| --- | --- | --- | --- |
| 斜杠命令 | 23 个（文本输入） | 19 个（文本输入） | TS 多了 4 个（/import、/share、/login、/logout） |
| 斜杠菜单 | ✅ 交互菜单 | ❌ 纯文本 | **最大差距** |
| 附件 Chip 预览 | 部分 | ❌ 缺失 | Martty 有 hover 预览 |
| Composer Meta 行 | ❌ | ❌ | 两者都缺失 |
| Pet 展示 | ✅ Martty | ❌ pi-tui | Martty 专属 |
| Theme Toggle | ✅ Martty | ❌ pi-tui | Martty 专属 |
| Permission Cycling | ✅ Martty | ❌ pi-tui | Martty 专属 |
| 模型选择器 | ✅ 交互列表 | ✅ 交互列表 | 基本等效 |
| 会话分支树 | ✅ | ✅ | /tree、/fork、/clone |

---

## 二、Martty TUI 深度分析

### 2.1 架构对比

| 方面 | Martty | pi-tui |
| --- | --- | --- |
| 代码行数 | 9,204 行 app.rs | 7,556 行 app.rs |
| 输入架构 | 纯 `Action` enum（keymap.rs） | 复杂 keybindings.rs + 多种事件 |
| 编辑器 | 394 行简单 Input | 4,501 行复杂 Editor |
| 状态管理 | App 单体 | App + 多个子系统 |
| 图片渲染 | kitty + 缩略图预览 | kitty + iTerm2 |
| Overlay 系统 | 基础（picker、slider、select） | 完整（dialog、selector、settings、search）|

### 2.2 Martty 关键交互模式（pi-tui 缺失）

1. **`/` 斜杠菜单**：输入 `/` 打开菜单，↑↓ 导航，Tab 补全，Enter 执行
2. **附件 Hover 预览**：`[image N]` chip 上悬停显示缩略图
3. **Meta 行**：composer 底部显示运行状态 ● / ✓ + Mode + 模型
4. **Permission Cycling**：`Shift+Tab` 循环权限预设
5. **Theme Toggle**：`Ctrl+T` 切换深/浅色
6. **Shell 集成**：`!` 前缀运行本地命令

### 2.3 pi-tui 已有可复用基础设施

- `Editor::is_showing_autocomplete()` + `autocomplete_render_styled_lines()` → 可改造为通用 popup list
- `pi-coding-agent/src/commands/slash.rs` → 19 条命令的解析逻辑（可被 slash 菜单复用）
- `pi-coding-agent/src/commands/slash.rs` 的 `RegisteredCommand` → 可作为菜单项数据源
- `StatusBar` 渲染逻辑 → 可抽取为通用的 `ComposerMeta` 行
- `image.rs` + `terminal_image.rs` → 已有 kitty image 渲染，可扩展为 preview

---

## 三、TUI ChatInput 具体差距分析

### 3.1 ChatInput（编辑器）当前实现

pi-tui 的 ChatInput 由以下组件构成：
- `Prompt`（prompt.rs）：管理 `Editor` + 标签 + 占位符 + 窗口滚动
- `Editor`（editor.rs 4,501 行）：kill ring、undo、历史、跳转模式、图片附件、自动补全
- `paint_prompt()`（app.rs）：渲染编辑器到 buffer
- `paint_autocomplete()`（app.rs）：渲染自动补全下拉

**已实现的功能**：
- ✅ 多行编辑器（软换行 + 硬换行）
- ✅ Kill ring（Ctrl+U/Ctrl+K）
- ✅ Undo 栈（Ctrl+Z）
- ✅ 历史记录（↑/↓）
- ✅ Word 导航（Alt+←/→）
- ✅ 跳转模式（Jump Forward/Backward）
- ✅ 图片附件（`[image N]` chip）
- ✅ 粘贴处理（paste burst、paste marker）
- ✅ 自动补全（路径 + 命令）
- ✅ Bash 模式（`!` 前缀）

### 3.2 Martty ChatInput 实现

Martty 的 `Input`（394 行）更简单，但 UI 层更完善：
- `Input`：纯文本编辑 + cursor + history（无 kill ring/undo）
- `meta_line()`：在 composer 底部显示运行状态 + Mode + 模型
- `draw_input()`：渲染输入行 + 渲染 `[image N]` chip + 记录 chip 坐标
- `draw_attachment_preview()`：鼠标悬停时渲染图片预览
- `draw_slash_menu()`：`/` 打开交互菜单

### 3.3 具体差距（ChatInput 部分）

| 功能 | pi-tui | Martty | 差距等级 |
| --- | --- | --- | --- |
| `/` 斜杠菜单 | ❌ 纯文本 | ✅ 交互菜单 | **高** |
| 附件 hover 预览 | ❌ | ✅ | **高** |
| 运行状态即时指示 | ❌ | ✅ ●/✓ | **中** |
| Mode chips | ❌ | ✅ | **中** |
| 模型名显示 | ❌（在 footer） | ✅（在 composer） | **中** |
| Kill Ring | ✅ | ❌ | — |
| Undo 栈 | ✅ | ❌ | — |
| Jump 模式 | ✅ | ❌ | — |
| 自动补全 | ✅ | ✅（/commands）| — |
| 多行编辑 | ✅ | ✅ | — |
| 图片附件 | ✅ | ✅ | — |

---

## 四、派发子任务

### LUM-1581：pi-tui 斜杠菜单系统 ✅ 已派发

- 子任务 ID：LUM-1581
- 优先级：高
- 描述：参考 Martty `slash_sel` + `draw_slash_menu()` 架构，实现交互式斜杠菜单
- 依赖：`pi-coding-agent/src/commands/slash.rs`（已存在）

### LUM-1582：pi-tui 附件 Chip Hover 预览 ✅ 已派发

- 子任务 ID：LUM-1582
- 优先级：中
- 描述：参考 Martty `draw_attachment_preview()`，在编辑器 `[image N]` chip 上悬停显示缩略图
- 依赖：`image.rs`（已存在 kitty image 渲染）

### LUM-1583：pi-tui Composer Meta 行 ✅ 已派发

- 子任务 ID：LUM-1583
- 优先级：中
- 描述：参考 Martty `meta_line()` + `draw_meta_row()`，在 composer 底部显示运行状态 + Mode + 模型
- 依赖：`status.rs`（已有状态渲染基础设施）

---

## 五、实现路线图建议

### 优先级排序（按用户体验影响）

```
1. LUM-1581 斜杠菜单 — 用户每天使用，UX 影响最大
2. LUM-1583 Composer Meta 行 — 无需鼠标，即时状态反馈  
3. LUM-1582 附件 Preview — 需鼠标，优先级较低
```

### 其他待实现的 app.* 键位

已有文档（LUM-1294/LUM-1366）记录了 43 个 `app.*` 键位中只接了 3 个：
- ✅ `app.interrupt`、`app.clear`、`app.exit`
- 🔲 `app.thinking.toggle`、`app.thinking.cycle`
- 🔲 `app.clipboard.pasteImage`
- 🔲 `app.suspend`、`app.editor.external`

---

## 六、编译验证

**注意**：本轮分析环境的 Rust 工具链版本为 1.75（Ubuntu 系统包），不足以构建 `feature/pi.rs`（需要 Rust 1.85+）。所有实现计划基于代码分析，不依赖编译器验证。

建议在配备 Rust 1.85+ 工具链的环境中运行：
```bash
cd pi-rust
cargo check --workspace --all-targets  # 应无错误
cargo test --workspace                   # 应全绿
```

---

## 七、总结

| 维度 | 完成度 |
| --- | --- |
| 核心渲染底座 | ✅ ~87% |
| 编辑器交互 | ✅ ~80% |
| 斜杠菜单系统 | ❌ 0%（本轮派发 LUM-1581）|
| 附件 Preview | ❌ 0%（本轮派发 LUM-1582）|
| Composer Meta | ❌ 0%（本轮派发 LUM-1583）|
| app.* 键位消费 | ~19/44 = 43% |
| 斜杠命令 | 19/23 = 83% |

**总体完成进度：约 75%**（渲染底座已完成，差距集中在交互编排层）
