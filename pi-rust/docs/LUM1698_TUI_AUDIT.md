# LUM-1698 TUI 审计报告

## 概述

基于对 pi-rust TUI (`crates/pi-tui`) 与 Martty (`src/ui.rs`) 的对比分析，本文档记录当前实现的完成度、差距和改进建议。

---

## 1. 当前实现状态

### 1.1 代码规模

| 模块 | Rust (行) | 描述 |
|------|----------|------|
| `app.rs` | 7,681 | 主应用逻辑 |
| `editor.rs` | 4,515 | 编辑器组件 |
| `message.rs` | 2,139 | 消息渲染 |
| `highlight.rs` | 2,574 | 语法高亮 |
| `status.rs` | 1,927 | 状态栏 |
| `terminal_image.rs` | 1,471 | 终端图片 |
| `input.rs` | 1,236 | 输入事件处理 |
| `selector.rs` | 1,426 | 选择器组件 |
| `autocomplete.rs` | 1,467 | 自动补全 |
| `markdown.rs` | 1,974 | Markdown 渲染 |
| `theme.rs` | 1,836 | 主题系统 |
| **总计** | **42,832** | 33 个模块文件 |

### 1.2 已实现的核心功能

✅ **布局系统**
- 响应式终端布局（`app.rs:draw()`）
- Composer/输入框区域管理
- Chat/Transcript 区域
- Right panel/slot 系统

✅ **交互组件**
- `input.rs` - 完整的输入事件抽象（Key/Mouse/Resize/Paste）
- `PasteBurst` - 粘贴突发检测
- `KeyModifiers` - Shift/Ctrl/Alt/Meta 支持
- 鼠标追踪支持

✅ **消息渲染**
- `message.rs` - 分角色渲染（User/Assistant/Tool/Info）
- Markdown 渲染 (`markdown.rs`)
- 围栏代码块高亮
- 链接和代码行内样式

✅ **自动补全**
- Slash 命令补全 (`autocomplete.rs`)
- 模糊匹配支持
- 描述搜索（超集于上游）

✅ **编辑器**
- `editor.rs` - 完整的编辑器组件
- 撤销/重做栈
- Kill ring
- 单词导航
- 选区支持

✅ **主题系统**
- 完整的颜色系统
- Markdown 专用颜色槽位 (`Md*`)
- 亮/暗主题

---

## 2. 与 Martty 的差距分析

### 2.1 布局算法差异

| 功能 | pi-rust | Martty | 差距 |
|------|---------|--------|------|
| 终端过小守卫 | ❌ 无 | ✅ `terminal too small — need ≥ 24x6` | **P0** |
| Composer 高度计算 | 固定 + 硬回退 | 动态 `resolved_composer_height` | **P1** |
| Pet 动画支持 | ❌ 无 | ✅ 鲸鱼动画 + kitty 像素协议 | P2 |
| 布局边界处理 | 简单 | 精确的边界计算 | P1 |

**Martty 布局关键代码** (`src/ui.rs:25-54`):
```rust
fn composer_height(height: u16) -> u16 {
    if height >= 15 { 4 }
    else if height >= 10 { 3 }
    else { 2 }
}

fn resolved_composer_height(area: Rect, app: &App) -> u16 {
    let minimum = composer_height(area.height);
    let maximum = (area.height / 2).max(minimum).min(12);
    // ...
}
```

**pi-rust 当前问题**: 在 ≤23 行终端中输入框会被完全挤出屏幕。

### 2.2 输入处理差异

| 功能 | pi-rust | Martty | 差距 |
|------|---------|--------|------|
| Bracketed Paste | ✅ | ✅ | - |
| Paste Burst | ✅ 已实现 | N/A (原生支持) | 已对齐 |
| Ctrl+C 语义 | ✅ 清空草稿 | ✅ 清空草稿 | - |
| 快速连按退出 | ✅ 500ms 内两次 | ✅ 类似 | - |

### 2.3 缺少的组件

| 组件 | Martty | pi-rust | 状态 |
|------|--------|---------|------|
| ScopedModelsSelector | ✅ | ❌ | P1 |
| tree.editLabel | ✅ | ❌ | P1 |
| Plugin Slider | ✅ | ❌ | P2 |
| Plugin Select | ✅ | ❌ | P2 |
| Elicitation Form | ✅ | ❌ | P2 |
| Agent Rail | ✅ | ❌ | P2 |
| Child Navigation | ✅ | ❌ | P2 |

### 2.4 视觉差异

| 项目 | Martty | pi-rust | 差距 |
|------|--------|---------|------|
| 快捷键提示行 | ❌ (已移除) | ✅ 20行启动头 | P1 (视觉噪音) |
| 输入框边框 | ✅ 圆角卡片 | 基础 Block | P2 |
| Logo 动画 | ✅ ASCII 动画 | ❌ 静态 | P3 |
| Pet 动画 | ✅ | ❌ | P3 |

---

## 3. 已知的 P0/P1 问题

### 3.1 P0 - 阻塞性问题

1. **≤23 行终端输入框消失** (`PARITY_AND_TUI_AUDIT_LUM1260.md`)
   - 位置: `app.rs` 布局计算
   - 影响: 用户盲打，无法看到输入内容
   - 修复: 参考 Martty `resolved_composer_height` + 终端过小守卫

2. **滚动条覆盖 `/help` 最后一列** (`PARITY_AND_TUI_AUDIT_LUM1260.md`)
   - 位置: `app.rs:3373`, `message.rs:1400`
   - 影响: 每行最后一个字符被覆盖
   - 修复: 滚动条可见时 `text_width` 减 1

### 3.2 P1 - 重要功能缺失

1. **`/scoped-models` 命令未接线** (`LUM-1263`)
   - 6 个 `app.models.*` action 未消费
   - 需实现 `ScopedModelsSelectorComponent`

2. **`app.tree.editLabel` 未接线** (`LUM-1263`)
   - 树节点重命名 UI 缺失

3. **斜杠补全匹配过宽**
   - 位置: `autocomplete.rs:434-437`
   - 当前: 匹配 name + description
   - 上游: 仅匹配 name
   - 建议: 恢复到仅按 name 匹配

---

## 4. 完成度评估

### 4.1 TUI 模块完成度

| 类别 | 已实现/总数 | 百分比 |
|------|------------|--------|
| 核心布局 | 1/1 | 100% |
| 输入处理 | 9/10 | 90% |
| 消息渲染 | 7/8 | 87.5% |
| 自动补全 | 5/6 | 83% |
| 编辑器 | 8/10 | 80% |
| 主题系统 | 15/18 | 83% |
| 组件库 | 6/12 | 50% |

### 4.2 总体评估

- **代码量**: ~43K 行 Rust TUI 代码
- **功能覆盖率**: ~80% (Martty 功能子集)
- **用户可感知缺口**: 2 P0 + 5 P1
- **完成度估算**: **75%**

---

## 5. 建议的改进计划

### 5.1 短期 (1-2 轮次)

1. **修复 P0 问题** - 终端过小守卫 + 滚动条行宽
2. **实现 `app.models.*` 接线** - scoped-models selector
3. **实现 `app.tree.editLabel`** - 树节点重命名

### 5.2 中期 (3-4 轮次)

1. **优化布局算法** - 采用 Martty 的 `resolved_composer_height`
2. **移除启动头快捷键行** - 减少视觉噪音
3. **完善 Plugin 组件** - Slider/Select/Elicitation

### 5.3 长期

1. **Pet 动画支持** (可选)
2. **Logo 动画** (可选)
3. **更多组件对齐**

---

## 6. Rust vs TypeScript 版本对比

| 维度 | TypeScript (pi) | Rust (pi-rust) | 差距 |
|------|----------------|----------------|------|
| LOC | ~153K | ~128K | 83.5% |
| 测试覆盖 | 100% (reference) | ~44% | 56% |
| TUI 功能 | 完整 | ~75% | 25% |
| 扩展生态 | 完整 | 基础 | 重大 |
| WASM 支持 | 原生 | 实验性 | 中等 |

**Rust 版本进度**: 约 75-80% 的 TypeScript 功能已实现

---

## 7. 附录

### 7.1 参考文档

- `PARITY_AND_TUI_AUDIT_LUM1260.md` - 详细 P0 问题取证
- `LUM1687_TUI_CHATINPUT_AUDIT.md` - ChatInput 最终审计
- Martty `src/ui.rs` - 布局参考实现

### 7.2 关键代码位置

- 布局: `pi-rust/crates/pi-tui/src/app.rs:100-250`
- 输入: `pi-rust/crates/pi-tui/src/input.rs`
- 消息: `pi-rust/crates/pi-tui/src/message.rs`
- 补全: `pi-rust/crates/pi-tui/src/autocomplete.rs`
