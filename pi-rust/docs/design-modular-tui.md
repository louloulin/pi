# LUM-1501 设计文档: pi-rust TUI 模块化架构

**前置**: [audit-tui-architecture.md](audit-tui-architecture.md)  
**状态**: 设计完成,等待实现

## 1. 设计原则

1. **保留 ratatui 作为渲染后端**: 不重写渲染管线,新增 `Component` trait 作为对 ratatui widget 的薄包装。
2. **兼容现有 API**: 拆分 `App` 时不破坏 `App::step_key/step_mouse/paint` 等公共方法。
3. **增量迁移**: 每完成一个模块就 commit 一次,不出现"大爆炸"提交。
4. **测试先行**: 每个新 trait 至少有一个集成测试。
5. **一比一对齐 pi-ts**: 命名/字段/方法签名尽量贴近 pi-ts,方便插件作者对照。

## 2. 核心 trait 设计

### 2.1 `Component` (核心)

`crates/pi-tui/src/core/component.rs`

```rust
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

pub const CURSOR_MARKER: &str = "\x1b_pi:c\x07";

/// 组件协议 — 与 pi-ts `Component` 一对一。
pub trait Component: std::any::Any + Send {
    /// 渲染到给定宽度的 Buffer 中。
    fn render(&self, area: Rect, buf: &mut Buffer);

    /// 处理键盘输入(可选)。
    fn handle_input(&mut self, _key: &Key) -> InputResult { InputResult::Ignored }
    fn handle_paste(&mut self, _text: &str) -> PasteResult { PasteResult::Ignored }

    /// 处理鼠标事件(可选)。
    fn handle_mouse(&mut self, _event: &MouseEvent) -> MouseResult { MouseResult::Ignored }

    /// 是否需要 key release 事件。
    fn wants_key_release(&self) -> bool { false }

    /// 失效缓存。
    fn invalidate(&mut self) {}

    /// 组件类型标识 (用于调试)。
    fn name(&self) -> &str { "Component" }

    /// 暴露 focused 状态。
    fn as_focusable(&mut self) -> Option<&mut dyn Focusable> { None }
}

/// 焦点协议 — 与 pi-ts `Focusable` 对齐。
pub trait Focusable {
    fn focused(&self) -> bool;
    fn set_focused(&mut self, focused: bool);
}

pub fn is_focusable(c: &dyn Component) -> bool {
    c.as_focusable().is_some()
}
```

### 2.2 `Container`

`crates/pi-tui/src/core/container.rs`

```rust
pub trait Container: Component {
    fn children(&self) -> &[Box<dyn Component>];
    fn children_mut(&mut self) -> &mut Vec<Box<dyn Component>>;

    fn add_child(&mut self, child: Box<dyn Component>);
    fn remove_child(&mut self, child: &dyn Component) -> bool;
    fn clear_children(&mut self);
}

/// VStack/HStack 等布局容器共享的实现。
pub struct VStackContainer {
    children: Vec<Box<dyn Component>>,
    focused: bool,
}

impl VStackContainer {
    pub fn new() -> Self { Self { children: vec![], focused: false } }
}

impl Component for VStackContainer {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let mut y = area.y;
        for child in &self.children {
            let child_area = Rect::new(area.x, y, area.width, /* child height */ 1);
            // 委托到 components::stack 的布局
            child.render(child_area, buf);
            y += 1;
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> MouseResult {
        // 委托到子组件 (类似 pi-ts Container.handleMouse)
        let mut y = 0u16;
        for child in &mut self.children {
            if event.y >= y && event.y < y + /* child_height */ {
                let mut child_event = event.clone();
                child_event.y -= y;
                return child.handle_mouse(&child_event);
            }
            y += 1;
        }
        MouseResult::Ignored
    }
}

impl Container for VStackContainer { /* ... */ }
impl Focusable for VStackContainer { /* ... */ }
```

### 2.3 `TuiBase` (差分渲染 + 焦点 + 输入)

`crates/pi-tui/src/core/tui_base.rs`

```rust
pub struct TuiBase {
    terminal: Box<dyn Terminal>,
    children: Vec<Box<dyn Component>>,
    focused_component: Option<*mut dyn Component>,  // raw pointer for lifetime
    overlays: Vec<OverlayEntry>,
    last_rendered_buffer: Option<Buffer>,
    render_requested: bool,
    min_render_interval_ms: u64,
    show_hardware_cursor: bool,
}

impl TuiBase {
    pub fn new(terminal: Box<dyn Terminal>) -> Self { /* ... */ }

    pub fn add_child(&mut self, child: Box<dyn Component>);
    pub fn remove_child(&mut self, child: &dyn Component) -> bool;
    pub fn clear(&mut self);

    pub fn set_focus(&mut self, component: Option<*mut dyn Component>);
    pub fn focused_component(&self) -> Option<*mut dyn Component>;

    pub fn show_overlay(&mut self, overlay: Box<dyn Component>, opts: OverlayOptions) -> OverlayHandle;
    pub fn hide_overlay(&mut self, handle: OverlayHandle);

    pub fn render_now(&mut self, force: bool);
    pub fn request_render(&mut self, force: bool);

    pub fn dispatch_mouse(&mut self, event: MouseEvent) -> bool;
    pub fn dispatch_key(&mut self, key: Key) -> bool;
}
```

### 2.4 `Tui` trait

```rust
pub trait Tui: Component {
    fn mode(&self) -> TuiMode;
    fn terminal(&self) -> &dyn Terminal;
    fn start(&mut self) -> Result<(), TuiError>;
    fn stop(&mut self, opts: TuiStopOptions);
    fn get_size(&self) -> (u16, u16);

    fn get_show_hardware_cursor(&self) -> bool;
    fn set_show_hardware_cursor(&mut self, enabled: bool);

    fn add_input_listener(&mut self, listener: Box<dyn InputListener>);
}
```

### 2.5 鼠标事件类型

`crates/pi-tui/src/core/mouse.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEventType { Press, Release, Move, Drag, Click, Wheel }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton { Left, Middle, Right, None }

#[derive(Debug, Clone)]
pub struct MouseEvent {
    pub event_type: MouseEventType,
    pub button: MouseButton,
    pub x: u16, pub y: u16,           // 局部坐标
    pub screen_x: u16, pub screen_y: u16, // 绝对坐标
    pub width: u16, pub height: u16,
    pub shift: bool, pub alt: bool, pub ctrl: bool,
    pub wheel_delta: Option<i16>,
    pub click_count: Option<u16>,
}

pub struct MouseEventResult {
    pub handled: bool,
    pub capture: bool,
    pub focus: bool,
    pub render: bool,
}

/// 与 pi-ts `dispatchMouseEvent` 对齐:处理 component.handle_mouse 返回值,
/// 包装为 (handled, target)。
pub fn dispatch_mouse_event(component: &mut dyn Component, event: &MouseEvent) 
    -> Option<MouseDispatchResult> 
{ /* ... */ }
```

### 2.6 Overlay 系统

`crates/pi-tui/src/core/overlay.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAnchor { Center, TopLeft, TopRight, BottomLeft, BottomRight, TopCenter, BottomCenter, LeftCenter, RightCenter }

pub struct OverlayMargin { pub top: u16, pub right: u16, pub bottom: u16, pub left: u16 }

pub enum SizeValue { Cells(u16), Percent(u8) }

pub struct OverlayOptions {
    pub width: Option<SizeValue>,
    pub min_width: Option<u16>,
    pub max_height: Option<SizeValue>,
    pub anchor: Option<OverlayAnchor>,
    pub offset_x: Option<i16>,
    pub offset_y: Option<i16>,
    pub row: Option<SizeValue>,
    pub col: Option<SizeValue>,
    pub margin: Option<OverlayMargin>,
    pub visible: Option<fn(u16, u16) -> bool>,
    pub non_capturing: bool,
}

pub struct OverlayHandle { /* Opaque token */ }
impl OverlayHandle {
    pub fn hide(self, tui: &mut TuiBase);
    pub fn set_hidden(&self, tui: &mut TuiBase, hidden: bool);
    pub fn focus(&self, tui: &mut TuiBase);
    pub fn unfocus(&self, tui: &mut TuiBase);
    pub fn is_focused(&self, tui: &TuiBase) -> bool;
    pub fn get_bounds(&self, tui: &TuiBase) -> Option<Rect>;
}
```

### 2.7 终端抽象

`crates/pi-tui/src/terminal/mod.rs`

```rust
pub trait Terminal: Send {
    fn size(&self) -> Result<(u16, u16), std::io::Error>;
    fn write(&mut self, bytes: &[u8]) -> Result<(), std::io::Error>;
    fn flush(&mut self) -> Result<(), std::io::Error>;
    fn enter_raw_mode(&mut self) -> Result<(), std::io::Error>;
    fn exit_raw_mode(&mut self) -> Result<(), std::io::Error>;
    fn enter_alt_screen(&mut self) -> Result<(), std::io::Error>;
    fn exit_alt_screen(&mut self) -> Result<(), std::io::Error>;
    fn hide_cursor(&mut self) -> Result<(), std::io::Error>;
    fn show_cursor(&mut self) -> Result<(), std::io::Error>;
    fn set_cursor_position(&mut self, x: u16, y: u16) -> Result<(), std::io::Error>;
    fn query_background_color(&mut self, timeout_ms: u64) -> Result<Option<RgbColor>, std::io::Error>;
    fn capabilities(&self) -> &TerminalCapabilities;
}

pub struct ProcessTerminal { /* wraps CrosstermBackend */ }
```

## 3. 组件目录设计

`crates/pi-tui/src/components/`

| 文件 | 对应 pi-ts | 行数估算 |
|------|------------|---------|
| `box.rs` | `components/box.ts` | ~80 |
| `stack.rs` | `components/stack.ts`, `v-stack.ts`, `h-stack.ts` | ~150 |
| `text.rs` | `components/text.ts` | ~100 |
| `truncated_text.rs` | `components/truncated-text.ts` | ~120 |
| `spacer.rs` | `components/spacer.ts` | ~30 |
| `scroll_view.rs` | `components/scroll-view.ts` | ~250 |
| `editor.rs` | `components/editor.ts` (从顶层迁) | ~4500 → 拆出 2000 |
| `input.rs` | `components/input.ts` | ~900 |
| `image.rs` | `components/image.ts` | ~150 |
| `markdown.rs` | `components/markdown.ts` (从顶层迁) | ~2000 → 拆出 1500 |
| `loader.rs` | `components/loader.ts` | ~100 |
| `cancellable_loader.rs` | `components/cancellable-loader.ts` | ~80 |
| `select_list.rs` | `components/select-list.ts` | ~250 |
| `settings_list.rs` | `components/settings-list.ts` | ~300 |
| `alt_screen_flash.rs` | `components/alt-screen-flash.ts` | ~80 |
| `mouse_region.rs` | `components/mouse-region.ts` | ~60 |

## 4. App 拆分计划

### 4.1 第一阶段: 抽出 core 抽象 (任务 #18, #20, #22, #23)
1. 新增 `core/component.rs`, `core/container.rs`, `core/tui_base.rs`, `core/tui.rs`
2. 新增 `core/mouse.rs`, `core/overlay.rs`
3. 新增 `terminal/` 目录,把 `raw_tty.rs`/`terminal.rs` 拆分为 trait + impl
4. App 暂时不用 TuiBase (兼容期)

### 4.2 第二阶段: 抽出 components 目录 (任务 #24)
1. 迁移 `editor.rs` → `components/editor.rs` (分阶段,保留 `crate::editor::Editor` 作为 re-export)
2. 新增 `components/box.rs`、`stack.rs`、`text.rs`、`spacer.rs`、`truncated_text.rs`
3. App.paint_* 改为构造组件树

### 4.3 第三阶段: TuiBase 接管 App (任务 #19, #21)
1. App 内部维护 `TuiBase`,把 step_key/step_mouse 委托到 TuiBase.dispatch_*
2. 覆盖层 (dialog/settings/history_search) 改为 `TuiBase.show_overlay`
3. 差分渲染开关 (可选,先全量,后续优化)

### 4.4 第四阶段: 输入管线 (任务 #26)
1. 新增 `core/input_pipeline.rs`,封装 `StdinBuffer` 概念
2. Kitty 协议解析: 扩展 `input.rs` 支持 release/repeat

### 4.5 第五阶段: 插件兼容 (任务 #27)
1. 在 `ts_compat.rs` 增加组件工厂函数:`make_box`、`make_text`、`make_vstack`
2. 暴露所有 `Component` 子类型到 `ts_compat` 模块

### 4.6 第六阶段: 测试与验证 (任务 #28, #29)
1. 为每个新组件写单元测试
2. 集成测试: 启动 TuiBase,模拟输入,断言渲染
3. 真实执行: `cargo run --bin pi -- --interactive` 跑通

## 5. 公共 API 兼容矩阵

| 现有调用方 | 新接口 | 兼容方案 |
|------------|--------|---------|
| `App::step_key(Key)` | `TuiBase::dispatch_key(Key)` | App 内部代理 |
| `App::step_mouse(MouseEvent)` | `TuiBase::dispatch_mouse(MouseEvent)` | App 内部代理 |
| `App::paint(Frame)` | `TuiBase::render_now()` + `Component::render(area, buf)` | App 内部代理 |
| `App::open_settings()` | `TuiBase::show_overlay(..., OverlayOptions)` | App 内部代理 |
| `App::open_dialog()` | 同上 | 同上 |
| `extension_ui::render(...)` | `components::box::Box::new(editor_view)` | 直接构造组件 |

## 6. 完成度评级

| 任务 | 计划 | 备注 |
|------|------|------|
| #17 设计 | **100%** | 本文档 |
| #18 TuiBase/Container/Tui trait | 启动中 | 见 4.1 |
| #19 鼠标派发链 | 启动中 | 见 4.3 |
| #20 Overlay 栈 | 启动中 | 见 4.1 |
| #21 FramePacer + 差分渲染 | 启动中 | 见 4.3 |
| #22 终端抽象 | 启动中 | 见 4.1 |
| #23 Focusable + CURSOR_MARKER | 启动中 | 见 4.1 |
| #24 components/ | 启动中 | 见 4.2 |
| #25 viewport-based alt-screen | 启动中 | 后期 |
| #26 输入管线 | 启动中 | 见 4.4 |
| #27 插件兼容 shims | 启动中 | 见 4.5 |
| #28 集成测试 | 启动中 | 见 4.6 |
| #29 真实执行验证 | 启动中 | 见 4.6 |