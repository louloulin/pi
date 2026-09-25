# LUM-1500 Audit: pi-rust TUI 架构与 pi-ts 差距分析

**作者**: pi-rust TUI 团队  
**日期**: 2026-09-25  
**状态**: 调研完成,等待设计阶段

## 1. 总体对比

| 维度 | pi-ts (`packages/tui/`) | pi-rust (`crates/pi-tui/`) | 差距 |
|------|-------------------------|---------------------------|------|
| 总代码行数 | ~18,653 | ~46,586 | rust 实现 2.5× |
| 模块数量 | 30 文件 + 18 组件 | 25 顶层 + 7 app 子模块 | 缺少 `components/` 目录 |
| 入口抽象 | `TuiBase` / `Container` / `Tui` 接口 | `App` 单体 struct (6508 行) | **缺少** |
| 组件协议 | `Component` / `Focusable` / `EditorComponent` | 零散 trait + App 字段 | **缺少统一抽象** |
| 鼠标派发 | `dispatchMouseEvent` + 全套事件类型 | 散落在 `step_mouse.rs` | **缺少** |
| 覆盖层 | `OverlayStack` + `OverlayHandle` + 焦点策略 | 无对应概念 (App 字段模拟) | **缺少** |
| 终端抽象 | `Terminal` trait + `ProcessTerminal` | 直接调用 `crossterm` + `raw_tty.rs` | 部分实现 |
| 渲染管线 | `compositeTuiLine` + 差分渲染 | ratatui 全量重绘 | **缺少差分渲染** |
| 输入管线 | `StdinBuffer` + `parseKey` + Kitty 协议 | `InputEvent` 直接分发 | 部分实现 |

## 2. pi-ts 核心抽象 (TuiBase/Container/Tui)

`tui.ts:111-169` 定义了组件协议:
```ts
interface Component {
    render(width: number): string[];
    handleInput?(data: string): void;
    handleMouse?(event: TuiMouseEvent): TuiMouseEventResult | undefined;
    wantsKeyRelease?: boolean;
    invalidate(): void;
}

interface Focusable {
    focused: boolean;  // TUI 在焦点切换时设置
}

const CURSOR_MARKER = "\x1b_pi:c\x07";  // APC 零宽序列, IME 候选窗口定位
```

`Container` (tui.ts:319-) 是组合容器: `add(child)`, `remove(child)`, `layout()`, `handleInput()`, `handleMouse()`。
`TuiBase` (tui.ts:417-) 维护:
- 差分渲染状态(`lastRenderedLines`)
- 焦点链(`focusedComponent`)
- 覆盖层栈(`overlays: OverlayStackEntry[]`)
- 鼠标捕获(`mouseCaptureTarget`)
- 输入监听(`inputListeners: TuiInputListener[]`)

`Tui` 接口是用户入口: `start()`, `stop()`, `add()`, `remove()`, `showOverlay()`, `getSize()`, `setCursorPosition()`。

## 3. pi-rust 当前实现痛点

### 3.1 `App` 单体过大
`crates/pi-tui/src/app/mod.rs` 6508 行, 内含:
- 状态机 (idle/busy/submitting/cancelling)
- 键盘路由 (`step_key.rs`)
- 鼠标路由 (`step_mouse.rs`)
- 渲染调度 (`paint_*` 散落在 mod.rs)
- 覆盖层逻辑 (settings/dialog/history_search)
- 扩展钩子 (`extension_ui.rs`)
- 自动补全/历史/选择器

无法抽出独立的 `Tui` 层。

### 3.2 缺少 `components/` 目录
pi-ts `components/` 含 18 个原子组件: Box/Stack/HStack/VStack/Text/Image/Editor/ScrollView/Spacer/Markdown/Loader/CancellableLoader/Input/SelectList/SettingsList/TruncatedText/AltScreenFlash/MouseRegion。

pi-rust 直接用 ratatui `Paragraph`/`Block` 等原语拼装,没有组件层抽象。

### 3.3 鼠标派发缺失完整链
- pi-ts: `press → drag → move → click → release → wheel` 完整链,带 `capture`/`focus`/`render` 标记
- pi-rust: `step_mouse.rs` 处理 press/release/move/wheel,但 drag 状态用 `App` 字段 (drag_start/drag_focus) 拼凑

### 3.4 覆盖层栈与焦点策略
pi-ts 支持:
- 覆盖层优先级 (focusOrder)
- 焦点恢复策略 (clear/preserve)
- 焦点阻塞检测 (`BlockedOverlayFocusRestoreState`)
- 多种锚点 (9 种) + 边距 + SizeValue (绝对/百分比)

pi-rust: settings/dialog/history_search 直接路由到 step_*,无统一覆盖栈。

### 3.5 差分渲染
pi-ts `tui-alt-screen.ts:compositeTuiLine()` 把组件行序列拼成单一 ANSI 流,通过 `lastRenderedLines` 比较输出差异。

pi-rust: ratatui 的 `Frame::render_widget` 全量重绘,效率较低(但功能上正确)。

### 3.6 终端抽象
pi-ts `terminal.ts` 提供 `Terminal` trait + `ProcessTerminal` 实现,封装 stdout/能力探测/光标定位。

pi-rust: `terminal.rs` + `raw_tty.rs` + `terminal_image.rs` 等多个文件,缺少统一 trait。

## 4. 关键数据流对比

### pi-ts 输入流
```
stdin bytes → StdinBuffer (批处理) → parseKey → Component.handleInput
                          ↓
                    Kitty 协议解码 → Key (with type: press/release/repeat)
                          ↓
                    TuiBase.routeInput → focusedComponent.handleInput
                          ↓
                    inputListeners (调试/录制)
```

### pi-rust 输入流
```
stdin bytes → input.rs (ANSI 解析) → InputEvent::Key/Resize/Mouse
                          ↓
                    App.step_key/step_mouse → 分发到 step_dialog/step_settings/.../step_composer
                          ↓
                    Prompt/Editor 处理
```

### pi-ts 鼠标流
```
SGR 鼠标字节 → SgrMouseEvent → TuiMouseEvent
                          ↓
                    TuiBase.dispatchMouse → 命中栈 (顶层覆盖层优先)
                          ↓
                    dispatchMouseEvent(component, event)
                          ↓
                    返回 { handled, capture, focus, render }
                          ↓
                    capture 设定后所有后续事件路由到该组件
```

## 5. 模块化目标 (源自任务 #17 设计)

### 5.1 新增目录结构
```
crates/pi-tui/src/
├── components/                  # 新增: 与 pi-ts components/ 对齐
│   ├── mod.rs
│   ├── box.rs                   # Box 容器
│   ├── stack.rs                 # Stack (vstack/hstack)
│   ├── text.rs                  # Text 文本
│   ├── editor.rs                # Editor (从顶层 editor.rs 迁移)
│   ├── input.rs                 # Input 单行输入
│   ├── scroll_view.rs           # ScrollView
│   ├── image.rs                 # Image
│   ├── markdown.rs              # Markdown (从顶层 markdown.rs 迁移)
│   ├── loader.rs                # Loader
│   ├── cancellable_loader.rs    # CancellableLoader
│   ├── spacer.rs                # Spacer
│   ├── select_list.rs           # SelectList
│   ├── settings_list.rs         # SettingsList
│   ├── truncated_text.rs        # TruncatedText
│   ├── alt_screen_flash.rs      # AltScreenFlash
│   └── mouse_region.rs          # MouseRegion
├── core/                        # 新增: 核心抽象层
│   ├── mod.rs
│   ├── component.rs             # Component trait + Focusable + CURSOR_MARKER
│   ├── container.rs             # Container 容器
│   ├── overlay.rs               # OverlayHandle + OverlayStack + 焦点策略
│   ├── tui_base.rs              # TuiBase (差分渲染 + 焦点 + 输入)
│   ├── tui.rs                   # Tui trait
│   ├── mouse.rs                 # 鼠标事件类型 + dispatchMouseEvent
│   └── focus.rs                 # 焦点管理
└── terminal/                    # 新增: 终端抽象
    ├── mod.rs
    ├── trait.rs                 # Terminal trait
    ├── process.rs               # ProcessTerminal 实现
    ├── image.rs                 # (从顶层 terminal_image.rs 迁移)
    ├── colors.rs                # (从顶层 terminal-image.ts 移植 OSC 11)
    └── capabilities.rs          # 能力探测
```

### 5.2 App 拆分目标
```
crates/pi-tui/src/app/
├── mod.rs                       # App struct (核心状态 + 顶层 step_*) 目标 < 2000 行
├── step_key.rs                  # 已有
├── step_mouse.rs                # 已有, 改为调用 TuiBase.dispatchMouse
├── step_paste.rs                # 已有
├── step_search.rs               # 已有
├── step_dialog.rs               # 已有
├── step_settings.rs             # 已有 (从 mod.rs 拆出)
├── render.rs                    # 已有
├── viewport.rs                  # 已有
└── state.rs                     # 新增: 集中所有可变状态
```

## 6. 风险与对策

| 风险 | 影响 | 对策 |
|------|------|------|
| 6508 行 App 拆分引入回归 | 高 | 保持公共 API 不变,逐步迁移;每步跑全部测试 |
| 现有 ratatui 渲染层兼容性 | 中 | 组件层先做 ratatui 适配器,后续再换差分渲染 |
| 鼠标事件重写破坏现有测试 | 中 | 抽出 `dispatchMouseEvent` 纯函数后保留 step_mouse.rs 入口 |
| 模块名冲突 (`editor.rs` 已在顶层) | 低 | 迁移时改用 `use crate::components::editor::Editor` |
| 测试覆盖不足 | 中 | 编写组件级 + 集成级测试 (#28) |

## 7. 完成度评级

| 任务 | 进度 | 备注 |
|------|------|------|
| #16 本审计 | **100%** | 本文档 |
| #17 设计 | 待启动 | 依赖 #16 |
| #18-29 实现 | 待启动 | 依赖 #17 |