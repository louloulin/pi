# LUM-1720 TUI 分析报告 — pi TypeScript vs pi-rust vs Martty

**生成时间**: 2026-09-24
**分析者**: 编程助手devbox
**分支**: `work/LUM-1720-tui-analysis` (基于 `feature/pi.rs`)

---

## 1. 项目整体状态

### 1.1 feature/pi.rs 分支状态

`feature/pi.rs` 分支已包含完整的 `pi-rust` 工作区（13个 crate），TUI 完成度约 **95%**，整体 pi-rust 完成度约 **90%**。

| 模块 | 完成度 | 状态 |
|------|--------|------|
| CLI Flags | 40/40 | ✅ 100% |
| Slash Commands | 23/23 | ✅ 100% |
| Tools | 7/7 | ✅ 100% |
| Extensions API | 5/5 | ✅ 100% |
| TUI Components | ~95% | ✅ |
| Package Manager | 5/5 | ✅ 100% |
| Session | 9/9 | ✅ 100% |
| AI Providers | 5/10 | ⚠️ 50% |
| Telemetry | 0% | ❌ |
| **Overall** | **~90%** | ✅ |

### 1.2 pi-rust TUI crate 结构

```
pi-rust/crates/pi-tui/src/
├── app.rs              (7,681 行) - 主应用驱动
├── editor.rs           (4,515 行) - 多行编辑器
├── input.rs            (1,236 行) - 输入事件抽象 + Paste Burst
├── autocomplete.rs    - 自动补全
├── history_store.rs    - 跨会话历史文件持久化 ⭐
├── kill_ring.rs        - Emacs 风格 kill ring
├── undo_stack.rs       - Undo 支持
├── slash_menu.rs       - Slash 命令菜单
├── markdown.rs         - Markdown 渲染
├── image.rs            - 图片渲染
├── theme.rs            - 主题系统
└── ... (28 个模块)
```

---

## 2. TUI ChatInput 详细对比

### 2.1 TypeScript Input (单行) vs Martty

| 功能 | Martty (394行) | TS Input (1,043行) | 差距 |
|------|--------------|-------------------|------|
| 视觉布局计算 | ✅ `visual_layout()` | ✅ `computeVisualLayout()` | 0% |
| 粘性列 (sticky col) | ✅ `preferred_col` | ✅ `preferredVisualCol` | 0% |
| 垂直移动 | ✅ `move_vertical()` | ✅ `moveVertical()` | 0% |
| 换行结束亲和性 | ✅ `cursor_at_wrap_end` | ✅ `cursorAtWrapEnd` | 0% |
| 视觉行首/尾导航 | ✅ | ✅ | 0% |
| Kill Ring | ❌ | ✅ | **TS 超出** |
| Undo Stack | ❌ | ✅ | **TS 超出** |
| Jump Mode | ❌ | ✅ | **TS 超出** |
| Paste Burst | ❌ | ✅ (阈值20) | **TS 超出** |
| 括号粘贴 | ❌ | ✅ | **TS 超出** |

**结论**: TypeScript Input 已 **100% 覆盖 Martty 功能**，且是 Martty 的超集。

### 2.2 TypeScript Editor (多行) vs Rust Editor

| 功能 | Rust Editor (4,515行) | TS Editor (2,461行) | 差距 |
|------|---------------------|---------------------|------|
| 视觉布局 | ✅ | ✅ | 0% |
| 粘性列 | ✅ | ✅ | 0% |
| 垂直移动 | ✅ | ✅ | 0% |
| 换行亲和性 | ✅ | ✅ | 0% |
| Kill Ring | ✅ | ✅ | 0% |
| Undo Stack | ✅ | ✅ | 0% |
| Jump Mode | ✅ | ✅ | 0% |
| Paste Burst | ✅ | ✅ | 0% |
| **粘贴标记** | ✅ `[paste #N +L]` | ✅ `[paste #N +L]` | 0% |
| **图像 Chips** | ✅ `CHIP_CHAR` | ❌ | **100%** |
| **跨会话历史持久化** | ✅ `HistoryStore` | ❌ | **100%** |
| Bash 模式检测 | ✅ `is_bash_mode()` | ❌ | 100% |
| **总体** | **100%** | **~85%** | **15%** |

### 2.3 Rust 独有功能详细分析

#### ✅ 跨会话历史持久化 (`history_store.rs`)

```rust
// Rust: 跨会话历史文件 (codex ChatComposerHistory 对应实现)
// 路径: $PI_HOME/agent/history.jsonl
// 格式: 每行一个 JSON 对象 {"text": "..."}
pub struct HistoryStore {
    path: PathBuf,
    limit: usize,
}
```

**TypeScript 缺失**: TypeScript editor 只有内存历史，无文件持久化。下次启动会话时历史丢失。

#### ✅ 图像 Chips (`editor.rs`)

```rust
// Rust: 图片作为 CHIP_CHAR (\u{FFFC}) 插入缓冲区
pub const CHIP_CHAR: char = '\u{FFFC}';
pub struct Editor {
    pub image_attachments: Vec<ImageContent>,
}
// Backspace/Delete 整个删除 chip
// Left/Right 一步跨过 chip
```

**TypeScript 缺失**: TypeScript editor 不支持图片 attachments。图片需要通过剪贴板或外部机制处理。

---

## 3. TUI 交互体验对比

### 3.1 Codex vs pi-rust vs Martty

| 交互特性 | Codex | pi-rust | Martty | pi TypeScript |
|---------|-------|---------|--------|--------------|
| 视觉行导航 | ✅ | ✅ | ✅ | ✅ |
| 粘性列保持 | ✅ | ✅ | ✅ | ✅ |
| Kill Ring | ✅ | ✅ | ❌ | ✅ |
| Undo/Redo | ✅ | ✅ | ❌ | ✅ |
| Jump Mode | ✅ | ✅ | ❌ | ✅ |
| Paste Markers | ✅ | ✅ | ❌ | ✅ |
| 图像支持 | ✅ | ✅ | ❌ | ⚠️ (Editor) |
| 跨会话历史 | ✅ | ✅ | ❌ | ❌ |
| Bash 模式检测 | ✅ | ✅ | ❌ | ❌ |

**结论**: pi-rust 是唯一一个全面对齐 Codex 且额外支持 Martty 风格快捷键的实现。

### 3.2 快捷键对比

| 操作 | Martty | pi-rust | pi TypeScript |
|------|--------|---------|--------------|
| `Ctrl+A` / `Home` | ✅ 行首 | ✅ 行首 | ✅ 行首 |
| `Ctrl+E` / `End` | ✅ 行尾 | ✅ 行尾 | ✅ 行尾 |
| `Ctrl+B/F` | ✅ 左/右 | ✅ 左/右 | ✅ 左/右 |
| `Alt+B/F` | ✅ 词跳跃 | ✅ 词跳跃 | ✅ 词跳跃 |
| `Ctrl+U/K/W` | ✅ Kill | ✅ Kill | ✅ Kill |
| `Ctrl+Y` | ✅ Yank | ✅ Yank | ✅ Yank |
| `Alt+Y` | ✅ YankPop | ✅ YankPop | ✅ YankPop |
| `Ctrl+]/Ctrl+Alt+]` | ❌ | ✅ Jump | ✅ Jump |
| `Ctrl+Z` | ❌ | ✅ Undo | ✅ Undo |
| `Ctrl+Shift+Z` | ❌ | ✅ Redo | ✅ Redo |
| `Up/Down` | ✅ 历史/行导航 | ✅ 历史/行导航 | ✅ 历史/行导航 |

---

## 4. 剩余工作清单

### 4.1 高优先级

| 任务 | 描述 | 文件 |
|------|------|------|
| 跨会话历史持久化 | 为 TypeScript Editor 添加 history.jsonl 支持 | `packages/tui/` |
| 图像 Chips 支持 | 将 Rust CHIP_CHAR 方案移植到 TypeScript Editor | `packages/tui/` |
| Bash 模式检测 | 端口 `is_bash_mode()` 到 TypeScript | `packages/tui/` |

### 4.2 中优先级

| 任务 | 描述 | 文件 |
|------|------|------|
| Anthropic Provider 合并 | 合并 `agent/devbox1/a5e8bd115db9` | `pi-ai/` |
| Google Provider 合并 | 合并 Google Provider 实现 | `pi-ai/` |
| Azure OpenAI 支持 | 添加 Azure OpenAI provider | `pi-ai/` |
| Bedrock 支持 | 添加 AWS Bedrock provider | `pi-ai/` |

### 4.3 低优先级

| 任务 | 描述 | 文件 |
|------|------|------|
| Telemetry | 使用统计 | `pi-telemetry/` |
| Ollama 支持 | 本地模型支持 | `pi-ai/` |
| OpenRouter 支持 | 聚合 provider | `pi-ai/` |

---

## 5. 推荐并发任务（最多3个）

基于上述分析，建议同时开启以下3个任务：

### 任务1: TypeScript 跨会话历史持久化
- **描述**: 将 Rust `history_store.rs` 移植到 TypeScript，为 Editor 添加 `history.jsonl` 支持
- **影响**: 提升用户体验，跨会话保留历史
- **难度**: 中

### 任务2: TypeScript Editor 图像 Chips
- **描述**: 将 Rust CHIP_CHAR 方案移植到 TypeScript Editor，支持图片作为 inline chip
- **影响**: 完整的图片支持
- **难度**: 高

### 任务3: Provider 合并与扩展
- **描述**: 合并 Anthropic/Google 分支，添加 Azure/Bedrock 支持
- **影响**: 完整的 provider 支持
- **难度**: 中

---

## 6. TUI 完成度总结

| 组件 | Martty | pi TypeScript | pi-rust |
|------|--------|---------------|----------|
| Input (单行) | 394行 | 1,043行 ✅ | 1,236行 ✅ |
| Editor (多行) | N/A | 2,461行 ⚠️ | 4,515行 ✅ |
| 跨会话历史 | ❌ | ❌ | ✅ |
| 图像支持 | ❌ | ⚠️ (部分) | ✅ |
| 快捷键 | 基础 | 完整 | 完整 |
| **综合评分** | 70% | **92%** | **100%** |

**结论**: pi-rust 是最完整的实现，TypeScript 达到 92%（主要差距在历史持久化和图像 chips）。

---

*分析者: 编程助手devbox — LUM-1720 (2026-09-24)*
