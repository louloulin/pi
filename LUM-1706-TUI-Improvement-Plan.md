# TUI 改进计划 — LUM-1706

## 当前状态

基于完整审计，TypeScript pi TUI Input 组件已完成以下功能：

| 功能 | 状态 | 备注 |
|------|------|------|
| 视觉布局 | ✅ 100% | 与 Martty 完全匹配 |
| 粘性列 | ✅ 100% | 垂直移动时保持列位置 |
| 垂直移动 | ✅ 100% | 支持上下行导航 |
| 换行亲和性 | ✅ 100% | 处理软换行边界 |
| 视觉行导航 | ✅ 100% | Home/End 键支持 |
| Kill Ring | ✅ 100% | Ctrl+U/K/W, Ctrl+Y, Alt+Y |
| Undo Stack | ✅ 100% | Fish-style 合并 |
| Jump Mode | ✅ 100% | Ctrl+]/Ctrl+Alt+] |
| Paste Burst | ✅ 100% | 阈值已优化 |

## Rust vs TypeScript 差距

主要差距在于 **pi-rust Editor** vs **TypeScript Input**:

| 差距 | 影响 | 优先级 |
|------|------|--------|
| 多行 Input | TypeScript 使用 Editor 处理多行 | 低 (架构选择) |
| 粘贴标记 | 大型粘贴显示 `[paste #N]` | 中 |
| 图像 Chips | Input 中不支持 | 低 (通过 Editor) |
| 历史文件持久化 | 无跨会话历史 | 中 |

## 改进建议

### 1. TypeScript Input 多行支持 (可选)

**描述**: 将 Input 组件扩展为支持硬换行，类似 Martty。

**影响**: 
- 需要修改输入处理逻辑
- 可能影响现有单行行为
- 与 Editor 组件功能重叠

**建议**: 保持当前架构（Input 单行 + Editor 多行），不修改。

### 2. 粘贴标记支持 (中优先级)

**描述**: 为 TypeScript Input 添加粘贴标记，类似于 pi-rust 的 `[paste #N +L lines]`。

**实现方式**:
```typescript
// 检测大型粘贴 (>100 字符或 >10 行)
if (text.length > PASTE_MARKER_CHAR_THRESHOLD || 
    (text.match(/\n/g) || []).length > PASTE_MARKER_LINE_THRESHOLD) {
  // 替换为标记
  const marker = `[paste #${nextPasteId} +${lines} lines]`;
  this.insertMarker(marker);
}
```

**注意**: 由于 Input 是单行组件，粘贴标记主要用于识别大型文本来源，不会在 Input 中显示完整内容。

### 3. 历史文件持久化 (中优先级)

**描述**: 为 TypeScript 添加会话间历史持久化。

**实现方式**:
- 使用文件系统存储历史
- 在会话启动时加载
- 提交提示时追加

### 4. UX 优化 (高优先级)

#### 4.1 输入反馈改进
- 添加轻微动画反馈
- 优化光标闪烁
- 改进滚动行为

#### 4.2 快捷键一致性
- 确保与 Martty 快捷键一致
- 添加更多 Emacs 风格快捷键

### 5. 错误处理改进

**描述**: 增强错误处理和恢复机制。

**实现**:
- 添加输入验证
- 改进边界条件处理
- 添加更详细的调试日志

## 实施计划

### 阶段 1: 完善现有功能 (当前)

- [x] 视觉行导航 ✅
- [x] Kill Ring ✅
- [x] Undo Stack ✅
- [x] Jump Mode ✅
- [x] Paste Burst ✅

### 阶段 2: 新功能开发

- [ ] 粘贴标记支持 (TypeScript)
- [ ] 历史持久化 (TypeScript)
- [ ] UX 优化

### 阶段 3: 长期优化

- [ ] 考虑多行 Input (如需要)
- [ ] 性能优化
- [ ] 跨平台一致性

## 推荐的最多 3 个并发任务

根据任务要求，最多开启 3 个任务同时运行：

1. **TUI 输入优化任务** — 改进输入响应和反馈
2. **粘贴标记功能任务** — 添加大型粘贴识别
3. **历史持久化任务** — 添加跨会话历史支持

---

*创建者: 编程助手devbox — LUM-1706 (2026-09-24)*
