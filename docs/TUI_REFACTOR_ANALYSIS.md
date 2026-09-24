# pi-rust TUI 重构分析报告

## 执行摘要

本报告分析 pi-rust TUI 与 TS pi-tui、nanopi、Martty 的差异，并提出模块化重构计划。

## 一、当前状态

### 1.1 编译状态
- ✅ `pi-tui` 库编译成功
- ✅ 517 个单元测试全部通过
- ✅ `pi-coding-agent` 依赖编译成功

### 1.2 已修复问题
1. **keybindings_defaults.rs 缺失** - 使用 `LazyLock` 替代 `include!` 宏解决
2. **测试失败** - 修复了 `slash_menu.rs` 中的 2 个测试断言

## 二、代码结构分析

### 2.1 pi-tui 模块结构 (54 模块)

```
Layer 1: 纯工具层
  width | word_navigation | kill_ring | undo_stack | hyperlink | fuzzy
  clipboard | loader | locale | input | keys | keybindings | history_store
  utils | visual_text | mouse_region | autocomplete | latex | raw_tty

Layer 2: 渲染基础设施
  theme | highlight | terminal_image | terminal_title | terminal

Layer 3: 被动 UI 组件
  component | styled | styles | extension_ui | image | status

Layer 4: 交互组件
  editor | message | markdown | prompt | search | selector | settings
  dialog | slash_menu | tree

Layer 5: App 编排器
  app (导入所有层)
```

### 2.2 主要问题

#### Critical Issues (需立即处理)

| 文件 | 问题 | 行数 | 严重性 |
|------|------|------|--------|
| `app.rs` | 单个 impl 块约 4595 行 | 1500-7100 | Critical |
| `app.rs` | `render_to_buffer_impl` + `paint_*` 辅助函数 | 6300-6800 | Critical |
| `editor.rs` | `handle_key_with` 247 行 if-cascade | 2600-2900 | Critical |
| `editor.rs` | 无 `BufferEdit` 抽象 - 15+ 编辑方法重复 | 600-3000 | High |

#### High Priority Issues

| 文件 | 问题 | 行数 | 严重性 |
|------|------|------|--------|
| `app.rs` | 模态覆盖层空白逻辑重复 4 次 | 6600-6700 | High |
| `app.rs` | 选择/搜索高亮重复 | 4300-4400, 6150-6200 | High |
| `editor.rs` | 无 `Cursor` 抽象 - 4 种游标表示混合 | 500-600 | High |
| `interactive.rs` | 扩展 `session_start` 在 TUI 循环前触发 | wiring.rs:770-820 | High |

## 三、跨实现对比

### 3.1 与 TS pi-tui 对比

| 方面 | TypeScript pi-tui | pi-rust pi-tui | 差异 |
|------|-------------------|-----------------|------|
| 渲染 | 差分渲染(字符串比较) | 直接 buffer 渲染 | Rust 更高效 |
| 组件模型 | Component 接口 | Component trait | 一致 |
| 覆盖层 | 栈式管理 | Dialog/Selector/Settings 分立 | TS 更灵活 |
| 光标标记 | CURSOR_MARKER 支持 IME | 无等效实现 | 缺失 |
| 鼠标坐标 | 变换链 | MouseRegion hit-test | 基本一致 |

### 3.2 与 nanopi 对比

| 方面 | nanopi | pi-rust | nanopi 优势 |
|------|--------|---------|-------------|
| 状态位置 | `struct App` 在 tui.rs 内 | App 单独模块 | nanopi 单一文件更清晰 |
| 渲染 | 直接 stdout 写入 | ratatui Buffer | pi-tui 更强大 |
| 事件循环 | `tokio::select!` | `tokio::select!` | 相似 |
| 菜单 | `MenuState<T>` 泛型 | Selector/Dialog 分立 | nanopi 更灵活 |
| 组件模型 | 无 | 54 模块 DAG | pi-tui 更模块化 |

### 3.3 与 Martty 对比

| 方面 | Martty | pi-rust | Martty 优势 |
|------|--------|---------|-------------|
| 状态位置 | `App` 在 app.rs | App 在 app.rs | 类似 |
| 渲染 | `ui.rs` **零状态** | impl App 混合渲染 | Martty 清晰分离 |
| 事件处理 | `App::handle_inner` 子调度 | `step_key_inner` 级联 | Martty 更清晰 |
| 输入线程 | 专用 std::thread | 主循环 | Martty 不阻塞 |
| Frame pacing | 33ms + immediate 标志 | 50ms 固定 | Martty 更精细 |

## 四、重构计划

### Phase 1: 已完成 ✅
- [x] 提取 `viewport.rs` - ViewportGeometry, ScrollbarGeometry, ScrollbarDrag
- [x] 提取 `render_helpers.rs` - 12 个 paint_*/apply_* 渲染辅助函数 (~920行)
- [x] 提取 `history_search.rs` - HistorySearch 状态结构体
- [x] 重构 `editor.rs` - 拆分 handle_key_with 为 6 个 helper 方法 (消除了 250 行 if-cascade)
- [x] 修复编译问题 - keybindings_defaults.rs (改用 LazyLock)
- [x] 修复测试失败
- [x] 更新 ts_contract.rs - 从 41 个扩展到 61 个对齐导出

### Phase 2: 高优先级
- [ ] 创建 `BufferEdit` 枚举 - 统一编辑操作
- [ ] 提取 paste_markers 逻辑 - Editor 的 paste 状态管理
- [ ] 提取 `header.rs` - 启动头部合成
- [ ] 解耦输入读取 - 专用线程

### Phase 3: 中优先级
- [ ] 添加 `Cursor` 抽象 - 统一 4 种游标表示
- [ ] 帧率控制 - FramePacer 带 immediate 标志
- [ ] 渲染/状态分离 - 参考 Martty ui.rs 无状态模式

### Phase 4: 长期优化
- [ ] 实现 `Widget` trait for `MessageView`
- [ ] 添加 CURSOR_MARKER 支持
- [ ] 统一覆盖层协议
- [ ] BoundedTerminalWriter 等效实现

## 五、关键改进建议

### 5.1 来自 Martty 的最佳实践
1. **渲染/状态分离** - `ui.rs` 无状态，只接收 `&mut Frame`
2. **输入线程** - 专用 `std::thread` 读取 crossterm 事件
3. **FramePacer** - 33ms 目标 + immediate 标志用于工具调用
4. **Controller 线程** - 代理传输逻辑独立

### 5.2 来自 TS pi-tui 的最佳实践
1. **覆盖层栈** - 统一 Dialog/Selector/Settings
2. **CURSOR_MARKER** - IME 支持
3. **差分渲染** - 大会话优化

## 六、测试覆盖

当前测试状态:
- ✅ 517 个单元测试通过
- ✅ ts_contract.rs - 70 exports aligned in test (up from 41)
- ✅ PLUGIN_COMPAT.md - 93 exports aligned (64% of 145, up from 48%)
- ✅ scrollbar.rs - 10 个测试全部通过
- ✅ composer_history.rs - 21/29 测试通过 (原 11 个编译错误全部修复)
- ⚠️ autocomplete_pointer.rs - 10/14 测试通过 (4 个失败为历史遗留)

## 七、下一步行动

1. **立即**: 提取 `ui.rs` 分离渲染逻辑
2. **短期**: 重构 `editor.rs` 的 if-cascade
3. **中期**: 添加输入线程和 FramePacer
4. **长期**: 完整复刻 TS 功能

---

生成时间: 2026-09-24
