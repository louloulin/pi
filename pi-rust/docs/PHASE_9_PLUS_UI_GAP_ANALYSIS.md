# pi-rust TUI 全量视觉差距分析与 Phase 9+ 跟进计划

> 报告生成日期：2026-09-25
> 参考源：TS pi-tui（权威，`packages/coding-agent/src/modes/interactive/`）、nanopi（可借鉴，`nanopi/src/mode/tui.rs`）、pi-rust（当前状态，`pi-rust/crates/pi-tui/`）
> 现状：G1–G8 已闭环；本报告聚焦 **G1–G8 之外**的全量 UI 差距。

---

## 一、执行摘要

| 维度 | 已闭环（G1–G8） | **未闭环（Phase 9+）** | 总计 |
|------|---------------|---------------------|------|
| 主题/颜色 | 0 | 4 | 4 |
| 组件库 | 4 | 9 | 13 |
| App 级（header/composer/messages/footer/selectors/dialogs） | 3 | 14 | 17 |
| 交互/键盘 | 0 | 5 | 5 |
| 信息/扩展 | 1 | 4 | 5 |
| 文档/帮助/边角 | 0 | 6 | 6 |
| **小计** | **8** | **42** | **50** |

下面按 A–G 七大维度逐项铺开，最后给出 **Top‑10 优先级 + Phase 9–14 实施计划**。

---

## 二、A — 基础视觉元素（theme/colors）

| ID | 项目 | TS pi-tui | pi-rust 当前 | 差距性质 |
|----|------|-----------|--------------|----------|
| A1 | 主题 schema（dark/light） | `theme.json` 解析 + 颜色模式（TrueColor/Ansi256/None） | ✅ 已实现 | 无 |
| A2 | ThemeBg 槽：userMessageBg / customMessageBg / toolPendingBg / toolSuccessBg / toolErrorBg / selectedBg | 完整 | ✅ 已实现 | 无 |
| A3 | ThemeColor：Accent / Border / BorderAccent / BorderMuted / Success / Error / Warning / Muted / Dim / Text / ThinkingText / UserMessageText / CustomMessageText / CustomMessageLabel / ToolTitle / ToolOutput / MdHeading / MdCode / MdLink / MdQuote / ScrollbarTrack / ScrollbarThumb / SearchMatchText | 21 种 | ✅ 已实现 | 无 |
| A4 | **OSC 4 / OSC 11 终端主题查询** | TS 启动时若 ColorMode=auto 则 `?4` `?11` 探测 | ❌ 未实现，固定 TrueColor | 中（用户体验受损：浅色用户看到深色背景） |

---

## 三、B — 组件库

| ID | 组件 | TS pi-tui 行为 | pi-rust 当前 | 差距 |
|----|------|----------------|--------------|------|
| B1 | Box / BoxLayout（paddingX/paddingY/bgFn） | Box(paddingX, paddingY, bgFn) | ✅ 已实现（G7） | 无 |
| B2 | Text / VStack / HStack / Spacer / Container | 原语齐备 | ✅ 已实现 | 无 |
| B3 | Markdown（tsx、theme、transformer 钩子、orderedList 保留、反斜杠转义保留） | `Markdown(text, 0, 0, theme, {color}, {preserveOrderedListMarkers, preserveBackslashEscapes, transform})` | ⚠️ 已有 `Markdown` 但 `transform` 钩子未暴露给插件作者；orderedList / backslash 保留未保证 | 大 |
| B4 | Editor（hardware cursor marker、scroll border、placeholder、history、IME、image attachments、focusable） | 完整 | ⚠️ 已有但 scroll border `↑/↓ + count more + ─…─` 仅在测试断言，未在生产路径生效 | 中 |
| B5 | Input | 单行输入 + IME | ✅ 已实现 | 无 |
| B6 | Prompt | TS 用 `Editor`（**没有** `> ` prefix）；Rust 单独建了一个 `Prompt` 类型带 label | **🔴 Rust 自创 `> ` 前缀** | **关键** |
| B7 | CustomEditor（app keybindings + 嵌入 workingStatusIndicator） | 在 top border 嵌入 status indicator + overflow label `↑ N more` | ❌ 无；只走纯 `Editor` | **关键** |
| B8 | Spinner / Loader / CancellableLoader / BorderedLoader | 80 ms 帧 + 取消按钮 | ⚠️ Spinner 已实现但 Loader 包装器与 alt-screen 行为差异未验证 | 中 |
| B9 | ScrollView / MouseRegion / MouseEvent 分发（click/wheel/drag） | 完整 | ⚠️ 基础鼠标事件已支持但 MouseRegion 的「折起/展开点击区」未接到工具/分支/压缩消息上 | **关键** |
| B10 | Autocomplete（描述 + 项 + 选中 bg） | 描述头与项之间有 `─` 分隔 + 选中项 SelectedBg 满铺 | ⚠️ 已有但缺少 `─` 分隔（G5 部分覆盖但未彻底） | 小 |
| B11 | DynamicBorder | 一行 `─`，宽度自适应 | ✅ 已实现 | 无 |
| B12 | StatusIndicator（Idle / Working / Retry / Compaction / BranchSummary） | 5 种变体 | ⚠️ 只有 working；其他 4 种缺 | **关键** |
| B13 | SelectList / Selector / SettingsList / Dialog / UserMessageSelector | 多个组件 | ⚠️ 基础 selector 已实现，dialog 部分缺（G5 部分覆盖） | 中 |
| B14 | KeybindingHints / `keyText` / `keyHint` / `keyDisplayText` | 通用工具：把 `KeyId` 翻译成可显示文本；macOS Alt→Option | ⚠️ 已有键位注册，但显示文本助手未独立成模块 | 中 |
| B15 | MarkdownTransform（`createMarkdownTransform` + 错误吞噬） | 错误吞噬（一个 transformer 抛错不影响其它） | ❌ 未实现 | 小 |
| B16 | Image / TerminalImage / Latex / Mermaid | 在 editor 和结果区显示 | ❌ 未实现（仅占位） | 大（功能缺口） |
| B17 | Diff / VisualTruncate / CountdownTimer / OAuthSelector / FirstTimeSetup / ExtensionEditor / ExtensionSelector / SkillInvocation / CompactionSummary / BranchSummary / TrustSelector / ShowImagesSelector / ConfigSelector / SettingsSubmenu / Daxnuts / Armin / EarendilAnnouncement | TS 一大堆专用组件 | ❌ 全部缺失或仅最简占位 | **大（功能缺口）** |

---

## 四、C — App 级别（interactive-mode ↔ pi-rust `app/mod.rs`）

### C1. Header 启动场景（TS `interactive-mode.ts:920–990`）

| 场景 | TS 行为 | pi-rust | 差距 |
|------|--------|---------|------|
| expanded（充足高度） | 7 大段全部展开：title + 3–4 行核心 + provider/models hint + cwd + git branch + 自定义 keybinding + 各 extension 注册 | ✅ 基础 7 段已渲染 | 缺 last-edited 时间戳、cwd 时间戳 |
| folded（中等高度） | 把 4 大段压缩为单行；保留 title + hints | ✅ 已实现 | 无 |
| short-terminal（矮于阈值） | 仅 title + 一行 `Press <chord> to show full startup help and loaded resources.` | ✅ G4 已修文案 | 无 |
| extension-replaced | extension 完全接管时仅渲染其产出 | ⚠️ 部分扩展可接管但事件总线未串通 | 中 |
| quiet（quietStartup=true） | 仅 title + `<chord> for help` 一行 | ⚠️ 实现存在但 **当前用户配置 quietStartup=true 导致「什么都没显示」** | 中（**UX 立即可见问题**） |
| startup_disabled | 完全空白 | ✅ 已实现 | 无 |
| extension-only | 仅 title + extension 渲染 | ⚠️ 框架支持但实际未触发 | 中 |

### C2. Composer / 编辑器

| ID | 项目 | TS 行为 | pi-rust | 差距 |
|----|------|--------|---------|------|
| C2.1 | **composer 标签** | **无 `> `** | **🔴 `> `** | **关键（用户原话："不需要> 存在一个输入框"）** |
| C2.2 | 多行时顶 border | `─` 行 + SelectedBg 满铺 | ✅ G3 | 无 |
| C2.3 | 多行时底 border | 存在时也画 | ⚠️ 仅顶边，避免与状态行重复 | 接受（nanopi 同款） |
| C2.4 | 滚动时 `↑ N more` 提示 | 顶 border 内嵌 N + spinner | ❌ CustomEditor 缺 | 中 |
| C2.5 | bash mode（`/bash`） | label 换色（bash-level fg） | ✅ 已实现 | 无 |
| C2.6 | image attachment chip + 粘贴图片 | 在 draft 上方画 chip + 占位行 | ⚠️ 基础已实现但 chip 渲染未用 SelectedBg | 小 |
| C2.7 | autocomplete 下拉（描述 + 候选 + 选中） | `─` 行 + 选中 SelectedBg 满铺 | ⚠️ 已实现但分隔行 G5 未全覆盖 | 小 |
| C2.8 | placeholder | 空文本时显示 placeholder | ✅ 已实现 | 无 |
| C2.9 | IME / 中文输入法 / 光标 marker | `\x1b[7m` 高亮当前字符 + zero-width marker 给 IME 定位 | ❌ 无 cursor marker | 中（中文用户输入位置错） |

### C3. 消息渲染

| ID | 类型 | TS 行为 | pi-rust | 差距 |
|----|------|--------|---------|------|
| C3.1 | **User 消息前缀** | **无** `> ` | **🔴 `> `** | **关键** |
| C3.2 | User 消息背景 | `Box(1, 1, bg("userMessageBg"))` + `Markdown(text, 0, 0, ..., color: fg("userMessageText"))` | ✅ G7 | 无 |
| C3.3 | User 消息 OSC 133 zone（shell integration） | 顶端 `OSC 133 ; A` 注入、底端 `OSC 133 ; B` + `OSC 133 ; C` 收尾 | ❌ 无 | 中（高级用户脚本化期望） |
| C3.4 | Assistant 消息 OSC 133 zone | 同上 | ❌ 无 | 中 |
| C3.5 | Assistant 消息 thinking 折叠 | `hiddenThinkingLabel = "Thinking..."` + Spacer(1) + MouseRegion 包裹 | ⚠️ 有 `working_header_line(spinner)` 但 label = `Working ...` 而非 `Thinking ...`，无 MouseRegion 包裹 | 中 |
| C3.6 | Assistant 消息 stop-reason 错误尾 | `stopReason === "length"` / `"aborted"` / `"error"` 时追加错误尾段 | ❌ 无 | 中 |
| C3.7 | Tool 消息（pending/success/error） | Box(1, 1, bg slot) + MouseRegion 包裹整块支持点击展开/折叠 + `+N lines, Ctrl+O to expand` 提示 | ⚠️ G1 已盖 slot，但 MouseRegion 包裹未接通 | 中 |
| C3.8 | Tool fallback preview | 未展开时显示前 N 行（FALLBACK_PREVIEW_LINES） | ⚠️ 测试已断言但触发路径不稳定 | 小 |
| C3.9 | Info 消息 | Box(1, 1, bg("customMessageBg")) + Markdown | ✅ G7 | 无 |
| C3.10 | Notice 消息 | 逐行 verbatim + 无 prefix + 无 wrap | ✅ 已实现 | 无 |
| C3.11 | **BranchSummary 消息** | Box(1, 1, bg("customMessageBg")) + `\x1b[1m[branch]\x1b[22m` label + Spacer + (collapsed 时 `Branch summary (keyText to expand)` / expanded 时 `**Branch Summary** + summary`) | ❌ **未实现**（只有占位 Role::Info） | **关键（压缩后丢失的元信息展示）** |
| C3.12 | **CompactionSummary 消息** | 同模式 + `[compaction]` label + token count + expand/collapse | ❌ **未实现** | 关键 |
| C3.13 | **SkillInvocation 消息** | 同模式 + `[skill] <name>` label + expand/collapse | ❌ **未实现** | 关键 |
| C3.14 | **Custom 消息（通用扩展）** | Box(1, 1, bg("customMessageBg")) + expanded flag + rebuild() | ⚠️ 仅占位 | 中 |
| C3.15 | ToolResult 区分 bash / read / write / edit 等 | ToolDefinition.renderResult | ⚠️ 部分实现，bash 高亮等未完 | 中 |

### C4. Footer 段

| ID | 段 | TS | pi-rust | 差距 |
|----|----|----|---------|------|
| C4.1 | model 名称 / 上下文 % | `%ctx • 1.2k tps • medium` | ✅ G2 | 无 |
| C4.2 | **`↑X`** 上行 token（formatTokens） | `↑1.2k` | ❌ **无** | **关键** |
| C4.3 | **`↓Y`** 下行 token | `↓0.4k` | ❌ **无** | **关键** |
| C4.4 | **`R<X>` cache read** | `R10k` | ❌ **无** | **关键** |
| C4.5 | **`W<X>` cache write** | `W0.4k` | ❌ **无** | **关键** |
| C4.6 | **`CH%` cache hit rate** | `CH42.5%` | ❌ **无** | **关键** |
| C4.7 | **`$cost` + `(sub)`** | `$0.012 (sub)` | ❌ **无** | **关键** |
| C4.8 | **provider 前缀**（多 provider 时） | `(anthropic) <其余段>` | ❌ **无** | 中 |
| C4.9 | **xp** 实验特性指示 | `• xp` (dim + warning+bold) | ✅ G2 | 无 |
| C4.10 | **context % 颜色阈值** | `>90` 红色 / `>70` 黄色 | ❌ **无** | 中 |
| C4.11 | **extension statuses 行**（按字母排序，多状态时第二行） | `statusA text • statusB text` dim | ❌ **无** | 关键 |
| C4.12 | git branch（cwd 旁边括号内） | `(branch)` | ⚠️ 已实现但格式略有差异 | 小 |
| C4.13 | auto_compact 指示 | `?%/12k ⚠️` | ❌ **无** | 中 |

### C5. Transcript / Scrollback

| ID | 项目 | TS | pi-rust | 差距 |
|----|------|----|---------|------|
| C5.1 | 滚动状态、选中 | ✅ | ✅ | 无 |
| C5.2 | 行折叠（超长输出） | +N 行 + Ctrl+O 展开 | ⚠️ 实现但 G1 后未端到端验证 | 小 |
| C5.3 | transcript selection（鼠标选 + yank） | TS 支持 | ⚠️ 部分 | 中 |

### C6. Selector / Dialog 族

| ID | 类型 | TS | pi-rust | 差距 |
|----|------|----|---------|------|
| C6.1 | Generic selector（`❯` cursor + SelectedBg 满铺 + DynamicBorder 包裹） | ✅ | ✅ G5 | 无 |
| C6.2 | **ModelSelector**（DynamicBorder + Spacer + scopeText + searchInput + list + bottom border） | 完整 | ⚠️ 基础 selector 已实现但缺 DynamicBorder 顶/底 + 错误信息 + provider 默认标记 | **关键** |
| C6.3 | **SessionSelector**（树状 + 搜索） | 完整 | ⚠️ 仅占位 | 大 |
| C6.4 | **ThemeSelector** | ✅ | ⚠️ 占位 | 中 |
| C6.5 | **SettingsSelector / SettingsSubmenu** | ✅ | ⚠️ 占位 | 中 |
| C6.6 | **ThinkingSelector**（off/minimal/low/medium/high/xhigh） | ✅ | ⚠️ 占位 | 关键（thinking level 是核心交互） |
| C6.7 | **TrustSelector / ShowImagesSelector / ScopedModelsSelector / OAuthSelector / LoginDialog / FirstTimeSetup / ExtensionEditor / ExtensionSelector / ConfigSelector / TreeSelector / UserMessageSelector / SessionSelectorSearch** | ✅ | ❌ 大多缺失 | 大（功能缺口） |

---

## 五、D — 交互 / 键盘

| ID | 项目 | TS | pi-rust | 差距 |
|----|------|----|---------|------|
| D1 | `app.interrupt`（ESC） | 行为 = abort running | ✅ | 无 |
| D2 | `app.exit`（Ctrl+D 仅空编辑时） | ✅ | ✅ | 无 |
| D3 | `app.clipboard.pasteImage` | ✅ | ⚠️ | 小 |
| D4 | `app.models.save`（在 selector 中 = 设为默认） | ✅ | ❌ | 中 |
| D5 | `tui.input.tab`（scope 切换） | ✅ | ❌ | 小 |
| D6 | `tui.editor.historyPrevious/Next`（独立于 app actions） | ✅ | ✅ | 无 |
| D7 | extension 注册的快捷键动态分发 | ✅ | ⚠️ 框架在 CustomEditor 有，但 actionHandlers 注册路径未完整 | 中 |
| D8 | MouseRegion 点击 fold 展开 / 隐藏 thinking | ✅ | ❌ | **关键** |
| D9 | 鼠标 wheel 滚动 transcript | ✅ | ✅ | 无 |
| D10 | 鼠标选择（拖拽 + yank） | ✅ | ⚠️ 基础已实现 | 中 |

---

## 六、E — 信息 / 扩展

| ID | 项目 | TS | pi-rust | 差距 |
|----|------|----|---------|------|
| E1 | 启动加载扩展列表展示 | `Loaded extensions: …` | ✅ | 无 |
| E2 | 扩展状态显示（footer 第二行） | ✅ | ❌ C4.11 | 关键 |
| E3 | 扩展命令注册（`/foo`） | ✅ | ⚠️ 部分 | 中 |
| E4 | 扩展对 composer 的劫持（custom editor） | ✅ | ❌ CustomEditor 未启用 | 中 |
| E5 | 扩展 UI 钩子（replace header / append footer / override footer） | ✅ | ❌ 未实现 | 中 |

---

## 七、F — 文档 / 帮助 / 边角

| ID | 项目 | TS | pi-rust | 差距 |
|----|------|----|---------|------|
| F1 | `<chord> for help` 行 | ✅ | ✅ | 无 |
| F2 | full help 屏（Alt+H 唤起） | 完整 | ✅ | 无 |
| F3 | **Markdown 块引号 gutter `▏` 竖线** | TS 默认 | ❌ 无（仅 markdown 解析） | 小 |
| F4 | **OSC 8 hyperlinks（`file:///path`）** | TS 在 markdown 链接中自动识别 | ❌ 无 | 中 |
| F5 | **CJK 双宽字符宽度** | ✅ | ✅ 已修（LUM 测试） | 无 |
| F6 | **真彩 fallback 到 256 色（terminal 兼容）** | ColorMode=auto 探测 | ❌ 固定 TrueColor | 中 |

---

## 八、G — 边角 / 兼容性

| ID | 项目 | TS | pi-rust | 差距 |
|----|------|----|---------|------|
| G1 | 极窄终端（< 30 列）降级 | TS 用 `visibleWidth` 截断 + 隐藏某些组件 | ⚠️ 基础截断已实现 | 小 |
| G2 | 极宽终端（> 200 列）换行策略 | 段落 wrap | ⚠️ wrap 单元测试有 | 小 |
| G3 | 高 DPI / iTerm2 / kitty / WezTerm 探测 | TS 用 `?CSI u` 等 | ❌ 无 | 中 |
| G4 | 行式 ANSI（colors-disabled） | ColorMode=None 时跳过 SGR | ⚠️ | 小 |
| G5 | Spinner 帧间隔（TS = 80 ms） | 80 ms | ⚠️ 已实现但需验证是否 80 ms | 小 |
| G6 | Output 截断（> N 行） | TS 截到 FALLBACK_PREVIEW_LINES | ⚠️ | 小 |
| G7 | 状态指示器 `workingStatusIndicator` 嵌入 editor 顶 border | ✅ | ❌ C2.4 | 中 |
| G8 | 重连 / SIGWINCH | ✅ | ⚠️ crossterm 处理 | 无 |

---

## 九、Top‑10 关键差距（按 ROI 排序）

| 排名 | ID | 差距 | 视觉影响 | 改动量 | 优先级 |
|------|----|------|----------|--------|--------|
| **1** | **C2.1 + C3.1** | **移除 `> ` 前缀（composer + user 消息）** | **🔴 整屏最显眼** | S（30 min） | **P0** |
| 2 | C4.2–C4.7 | footer 增 `↑X ↓Y R/W CH% $ (sub)` 六段 | 🔴 进度感 | M | P0 |
| 3 | C4.11 | footer 第二行 extension statuses（按字母排序） | 🟡 一眼看见扩展状态 | S | P0 |
| 4 | C4.10 | context % 颜色阈值（>90 红 / >70 黄） | 🟡 警告感 | XS | P0 |
| 5 | C3.3 + C3.4 | User/Assistant OSC 133 shell integration zones | 🟢 高级用户脚本化 | S | P1 |
| 6 | C3.5 | `hiddenThinkingLabel = "Thinking..."`（替换 `Working ...`） | 🟢 一致性 | XS | P1 |
| 7 | B12 | StatusIndicator 五种变体（Working / Retry / Compaction / BranchSummary / Idle） | 🟢 进度感 | M | P1 |
| 8 | C3.11–C3.13 | BranchSummary / CompactionSummary / SkillInvocation 三种 CustomMessage 实现 | 🟢 信息密度 | M | P1 |
| 9 | B9 | MouseRegion 包裹 tool/branch/compaction 消息实现点击 fold 展开 | 🟢 交互 | M | P1 |
| 10 | A4 + F6 | ColorMode=auto（OSC 4 / OSC 11 探测 + 256 色 fallback） | 🟢 兼容 | S | P2 |

> **用户原话**："分析ui的差距是什么 不需要> 存在一个输入框" → **第 1 名就是这条**；改 1 行 + 测试就闭环。

---

## 十、Phase 9+ 跟进计划

> **原则**：每个阶段一个独立分支 + commit；不引入新依赖；保持 ts_compat 拼写；不破坏现有 1740+ 测试。

```
P9 ─► P10 ─► P11 ─► P12 ─► P13 ─► P14
去除 >   Footer  状态指示  Custom  Mouse +   OSC133 +
       段补齐  器变体  消息  折起     ColorMode
```

### Phase 9 — 去除 `> ` 前缀（P0，~30 min）

**目标**：composer 标签改为空串（保留 label_width=0 的占位），user 消息改为无前缀。

**修改文件**：
- `crates/pi-tui/src/components/prompt.rs` —— `Prompt::new(...)` 默认 label 改为 `""`，暴露 `with_label(s)` setter。
- `crates/pi-coding-agent/src/interactive.rs` —— 所有 `Prompt::new("> ")` 调用点改为 `Prompt::new("")`；或保留一个 `prompt_with_chevron()` 帮助函数供显式开启。
- `crates/pi-tui/src/components/message.rs:1429-1445` —— `Role::User` 分支 prefix 改为 `""`；`Role::Tool` prefix 保持 `* `（与 TS 一致：`*` 表示 tool）；`Role::Info` 改为 `""`（TS 是直接渲染 Markdown，无前缀）。
- `crates/pi-tui/tests/user_block_box.rs` —— 调整列偏移断言：user 现在 body[0,0] 在列 0；info 同样列 0。
- `crates/pi-tui/tests/message_view.rs` —— 同步断言。

**测试**：
- `composer_has_no_chevron_prefix`
- `user_message_has_no_chevron_prefix`
- `info_message_has_no_dot_prefix`（若改为空）
- 现有 `prompt_label` / `user_block_*` 测试更新列偏移。

**验证**：`cargo test --workspace && cargo run -p pi-coding-agent --example snapshot_tui`

### Phase 10 — Footer 段补齐（P0，~3h）

**目标**：补齐 `↑X ↓Y R/W CH% $ (sub)` + provider 前缀 + context % 颜色阈值 + extension statuses 第二行。

**修改文件**：
- `crates/pi-tui/src/components/status.rs` —— `StatusData` 新增字段：
  - `usage_input: u64`、`usage_output: u64`、`cache_read: u64`、`cache_write: u64`、`cache_hit_rate: Option<f32>`
  - `cost: f32`、`using_subscription: bool`
  - `provider_count: usize`（>1 时加 `(provider)` 前缀）
  - `extension_statuses: Vec<String>`（多 provider 时第二行，alpha 排序）
- `crates/pi-tui/src/components/status.rs::StatusBar::render_themed` —— 拼接六段；context % 按阈值上色；extension statuses 排序+拼接。
- `crates/pi-coding-agent/src/interactive.rs` —— 从 `Session` / `ModelRuntime` 拉真实 usage；注册到 `App::set_status(...)`。
- 新增 `crates/pi-tui/tests/footer_segments.rs` —— 6 个测试断言各段存在/缺省。
- 新增 `crates/pi-tui/tests/footer_xp_threshold.rs`（扩展现有）—— 断言 context % 在 70/90 阈值颜色切换。

**测试**：
- `footer_shows_input_output_tokens`
- `footer_shows_cache_read_write_hit_rate`
- `footer_shows_cost_with_subscription_badge`
- `footer_shows_provider_prefix_when_multi_provider`
- `footer_colorizes_context_percent_above_70_and_90`
- `footer_second_line_sorts_extension_statuses_alphabetically`

### Phase 11 — StatusIndicator 变体（P1，~2h）

**目标**：实现 Retry / Compaction / BranchSummary / Idle 四种变体，渲染 working 时嵌入到 CustomEditor 顶 border。

**修改文件**：
- `crates/pi-tui/src/components/status_indicator.rs`（新）—— `StatusIndicator` trait + `Idle` / `Working(spinner)` / `Retry(spinner, countdown)` / `Compaction(reason)` / `BranchSummary(target)`。
- `crates/pi-coding-agent/src/components/custom_editor.rs`（新，对应 TS `custom-editor.ts`）—— 覆写 `render_top_border(width, hidden_line_count)`，按 G2/G3 + 嵌入 status。
- `crates/pi-coding-agent/src/interactive.rs` —— 把 `RetryStatusIndicator`、`CompactionStatusIndicator`、`BranchSummaryStatusIndicator` 实例化并 wire 到 custom_editor。
- `crates/pi-tui/src/components/countdown_timer.rs`（新，对应 TS `countdown-timer.ts`）—— setInterval + onTick + onComplete。

**测试**：
- `status_indicator_idle_renders_blank`
- `status_indicator_working_renders_spinner_frame`
- `status_indicator_retry_renders_countdown`
- `status_indicator_compaction_renders_reason`
- `custom_editor_top_border_embeds_status_indicator`

### Phase 12 — CustomMessage 三种实现（P1，~2h）

**目标**：实现 BranchSummary / CompactionSummary / SkillInvocation 三种 Box(paddingX, 1, bg("customMessageBg")) 消息。

**修改文件**：
- `crates/pi-tui/src/components/branch_summary.rs`（新）—— `BranchSummaryMessageComponent` 镜像 TS 第 10–58 行：`expanded` flag + `[branch]` label + Spacer + expanded/collapsed 文本 + keyText 注入。
- `crates/pi-tui/src/components/compaction_summary.rs`（新）—— 同模式，token count。
- `crates/pi-tui/src/components/skill_invocation.rs`（新）—— 同模式，`[skill] <name>`。
- `crates/pi-tui/src/components/message.rs::MessageItem` —— 新增变体 `Role::BranchSummary`、`Role::CompactionSummary`、`Role::SkillInvocation`，或者用 `MessageItem::custom_branch(...)` / `custom_compaction(...)` / `custom_skill(...)` 构造器 + 内部字段区分。
- `crates/pi-tui/tests/custom_message_branch.rs` / `compaction.rs` / `skill.rs` —— 三个测试。

### Phase 13 — MouseRegion 折起（P1，~2h）

**目标**：tool/branch/compaction 消息包 MouseRegion，鼠标点击折叠/展开。

**修改文件**：
- `crates/pi-tui/src/components/mouse_region.rs`（扩展）—— `setOnClick(handler)` 现有；新增 `MouseRegion::wrap(component)` 工厂。
- `crates/pi-tui/src/components/message.rs::tool_block` —— 整块包 MouseRegion，点击调 `toggle_tools_expanded`。
- branch/compaction 同样包 MouseRegion，点击调各自 toggle。
- `crates/pi-coding-agent/src/interactive.rs` —— 鼠标事件分发已就绪，只接 handler 即可。

**测试**：
- `tool_block_mouse_click_toggles_expanded`
- `branch_summary_mouse_click_toggles_expanded`
- `compaction_summary_mouse_click_toggles_expanded`

### Phase 14 — OSC 133 + ColorMode=P2，~3h

**目标**：OSC 133 shell integration zones；ColorMode=auto（OSC 4/OSC 11 探测 + 256 色 fallback）。

**修改文件**：
- `crates/pi-tui/src/components/assistant_message.rs`（新，对应 TS）—— 顶层加 `OSC 133 ; A`，底端加 `OSC 133 ; B` + `OSC 133 ; C`。
- `crates/pi-tui/src/components/user_message.rs`（新）—— 同模式。
- `crates/pi-tui/src/terminal/color_mode.rs`（新）—— `detect()`：`?4` query 探测背景色，`?11` 探测前景；`TERM=xterm-256color` 等降级到 256。
- `crates/pi-tui/src/app/mod.rs` —— 启动时调用 `detect()` 写入 `ColorMode`。
- `crates/pi-tui/tests/osc133_zones.rs` —— 断言 user/assistant 消息首末行有 OSC 133 序列。
- `crates/pi-tui/tests/color_mode_detect.rs` —— 注入伪终端响应，断言解析正确。

---

## 十一、用户痛点 → 修复映射

| 用户原话 | 对应差距 | 修复阶段 |
|----------|----------|----------|
| "不需要> 存在一个输入框" | **C2.1 + C3.1** | **Phase 9** |
| "tui没任何变化 差距还是巨大"（截图空 TUI） | `quietStartup: true` 用户配置 → header 极简 | 已通过 G4 + 提示用户改 config 缓解 |
| "完善整个tui ... 完全实现pi-tui整体ui风格" | Top‑10 列表 | Phase 9–14 |
| "分析后续计划" | 本报告 | — |

---

## 十二、工作量估算

| 阶段 | 范围 | 工作量 | 累计 |
|------|------|--------|------|
| **P9** 去除 `> ` | composer + user + info + tests | ~30 min | 0.5h |
| **P10** Footer 段补齐 | 6 段 + provider 前缀 + context 颜色 + extension 行 | ~3h | 3.5h |
| **P11** StatusIndicator 变体 | 4 变体 + CustomEditor 嵌入 + CountdownTimer | ~2h | 5.5h |
| **P12** CustomMessage | 3 种 Box 包裹消息 + tests | ~2h | 7.5h |
| **P13** MouseRegion 折起 | 3 处点击展开 + tests | ~2h | 9.5h |
| **P14** OSC 133 + ColorMode | shell integration + 自动探测 | ~3h | 12.5h |
| **合计** | | **~12.5h** | — |

不引入新依赖；全部改动在 `pi-tui` + `pi-coding-agent`；插件兼容性保留（所有新组件通过 `pi_tui::ts_compat::*` 暴露，保持 TS 拼写）。

---

## 十三、验证清单（每阶段后）

```bash
cargo test -p pi-tui --lib
cargo test -p pi-tui --tests
cargo test --workspace
cargo check --workspace --examples
cargo run -p pi-coding-agent --example snapshot_tui
# 视觉确认：diff target/snapshot/*.txt vs 上次基线；接受已知动画类 diff
```

---

## 十四、风险与回滚

| 风险 | 缓解 |
|------|------|
| **C2.1/C3.1 移除 `> ` 可能破坏既有测试或用户肌肉记忆** | 仅改默认；提供 `with_chevron()` 显式开关；文档标注「迁移到无前缀」 |
| **P10 footer 六段可能与现有 tps / thinking 段格式不一致** | 把六段视为 append-only；保留旧段不变 |
| **P11 CustomEditor 嵌入 status 可能在窄屏挤掉 overflow label** | 复用 TS 同一 fallback 逻辑：宽度不够时退回纯 spinner |
| **P14 OSC 133 在不支持的终端可能渲染成可见字符** | 加 `\x1b[?2027` 检测；若失败则不输出；测试覆盖 stderr 缓冲 |

---

## 十五、结论

1. **G1–G8** 已闭环，本计划不再触碰。
2. **P0 三件事**（去 `> `、footer 六段、extension statuses 行、context 颜色阈值）能立刻让 pi-rust 视觉上追平 TS pi-tui 的**信息密度**与**现代感**——这是用户最痛的「巨大差距」感受来源。
3. **P1 三件事**（StatusIndicator 变体、CustomMessage 三种、MouseRegion 折起）补齐 TS 已支持的「丰富反馈」——让 TUI 不再像终端命令行，而像一个真正的对话界面。
4. **P2**（OSC 133 + ColorMode auto）是面向高级用户与终端兼容性的最后 1 公里。

完整修复完毕后，pi-rust 与 TS pi-tui 在视觉/交互上的差异将收敛到「**仅语言版本号 + 编译产物**」层面。

---

## 十六、对账与最终 Top‑10 修正（2026‑09‑25 后台 agent 完成）

后台 agent（`ac5056dc07b74d4b3`）完成 60 次工具调用、87 项对照，结论「**87 NONE + 7 MINOR + 10 MAJOR + 0 MISSING**」。**该结论需要修正**——它把「文件存在」当「视觉对齐」，忽略默认值与配置。直接读源码后修正如下：

### 16.1 后台 agent 误判为「NONE」/「MINOR」的项目（实为 MAJOR）

| 项 | 后台 agent 评级 | 直接读源码发现 | 真评级 |
|----|----------------|----------------|--------|
| C2.1 composer 标签 `> ` | NONE（认为基础 Prompt 存在） | `app/mod.rs:1446` 调用 `Prompt::new("> ")`；`prompt.rs:114` `label()` 返回 `&self.label` 默认 `"> "`；多个测试也用 `Prompt::new("> ")` | **MAJOR（用户原话确认）** |
| C3.1 User 消息 `> ` | NONE | `message.rs:1431` 硬编码 `"> "`（带注释引用 LUM‑1238 §15.4 明确保留此为有意） | **MAJOR** |
| C4.2–C4.7 footer 段 | 部分 MINOR | `status.rs:7‑9` 注释 **声称已实现** `↑↓ R/W CH% $ (sub)`；但实际 `StatusBar::render_themed` 路径需要逐行 trace 才能确认每段是否真的 paint | **MAJOR（待 cell 级验证）** |
| B9 MouseRegion 包裹 tool/thinking 折起 | MINOR | 仅基础 mouse_region 存在，但**未接到** `tool-execution.ts:172 createResultRegion` / `assistant-message.ts` 的 MouseRegion 调用点 | **MAJOR** |
| C3.5 thinking label = `Working ...` vs TS `Thinking...` | MINOR | 注释明确写「TS pi-tui paints a one-line `~ Working` header」—— Rust 是 **自创** `Working ...` 而非 TS 的 `Thinking ...` | MAJOR |

### 16.2 后台 agent 正确识别但我低估的 MAJOR

| 项 | 描述 | 改动量 | 阶段 |
|----|------|--------|------|
| **ScrollView 简化** | 缺 `scrollbar: auto | always` 模式、瞬时显示延迟（1s 渐隐）、hover 保留 | M | **P11.5（并入 P11）** |
| **8 个未消费键位** | `app.tree.*` / `app.models.*` / `app.suspend` / `app.editor.external` / `tui.altScreen.halfPageUp/Down` / `lineUp/Down` / `previousPrompt` / `nextPrompt`——`app/mod.rs:122-125` 注释明示「故意不实现」 | M | **P15（新加阶段）** |
| **Custom overlay 桥** | `ctx.ui.custom(factory, options)` 的 JS 工厂 → Rust 组件对象的桥未连通（`wiring.rs:1197` 注释「custom family has no `pi_protocol::Api` variant to round-trip through」就是问题根源） | L | **P16（新加阶段）** |
| **Locale 仅 en/zh** | 缺 ja/ko/es/fr/de；TS 有完整 i18n | M | **P17（新加阶段）** |

### 16.3 后台 agent 识别的 MINOR（已纳入）

| 项 | 修复 |
|----|------|
| Tree selector ASCII 分支符 | 在 `selector.rs` 加 `tree_branch` glyph |
| `setTitle` 未消费 | 加 `Title` region + 终端标题 OSC 0 |
| tmux 超链接探测无子进程 | 改用 `TMUX` env + 常量检测 |
| `…` 截断比 TS 少 2 列 | `truncate_to_width` 改 3-char `...` |
| Tree 分支符、ASCII 树 | `selector.rs` 加 `├─` `└─` `│` |

### 16.4 修正后的最终 Top‑10

| # | 差距 | 阶段 | ROI |
|---|------|------|-----|
| 1 | **移除 `> ` 前缀**（composer + user） | P9 | 🔴 最高 |
| 2 | footer 六段 cell 级验证 + 补齐 | P10 | 🔴 |
| 3 | extension statuses 第二行 | P10 | 🟡 |
| 4 | context % 颜色阈值（>90/>70） | P10 | 🟡 |
| 5 | MouseRegion 包裹 fold/thinking | P13 | 🟢 |
| 6 | `hiddenThinkingLabel = "Thinking..."` 替换 `Working ...` | P11 | 🟢 |
| 7 | StatusIndicator 五变体（Idle/Working/Retry/Compaction/BranchSummary） | P11 | 🟢 |
| 8 | BranchSummary / CompactionSummary / SkillInvocation 三种 CustomMessage | P12 | 🟢 |
| 9 | ScrollView scrollbar 模式 + 1s 渐隐 + hover | P11.5 | 🟢 |
| 10 | OSC 133 zones + ColorMode=auto（OSC 4/11 探测 + 256 fallback） | P14 | 🟢 |

### 16.5 新增 P15–P17 阶段（后台 agent 关键发现）

#### Phase 15 — 8 个未消费键位补齐（P1，~3h）

**目标**：实现 `app.tree.*` / `app.models.*` / `app.suspend` / `app.editor.external` / `tui.altScreen.halfPageUp/Down` / `lineUp/Down` / `previousPrompt` / `nextPrompt` 全部 8 个键位。

**修改文件**：
- `crates/pi-coding-agent/src/app/mod.rs:122-125` —— 删除「故意不实现」注释。
- 新增 handler：`tree_toggle` / `tree_expand` / `tree_collapse` / `models_cycle_next` / `models_cycle_previous` / `models_show_selector` / `suspend`（SIGTSTP） / `editor_external`（$EDITOR 调起）/ `half_page_up` / `half_page_down` / `line_up` / `line_down` / `previous_prompt` / `next_prompt`。
- `crates/pi-coding-agent/src/interactive.rs` —— 把 handler 注册到 `App::register_app_actions`。

**测试**：
- 8 个键位 → 各自 handler 的单元测试（mock TUI）。

#### Phase 16 — Custom overlay JS→Rust 桥（P2，~5h）

**目标**：连通 `ctx.ui.custom(factory, options)`。

**TS 路径**：`factory` 是 JS 函数，调用时 JS 侧构造 Component 树 → 通过 `pi_tui` host 渲染。
**Rust 路径**：`factory` 必须是 `Box<dyn CustomFactory>`（Rust 端 trait），扩展用 `inventory` 或 `linkme` 注册。

**修改文件**：
- `crates/pi-coding-agent/src/extensions/ui_bridge.rs` —— 定义 `CustomFactory` trait（`fn build(&self, opts: serde_json::Value, app: &mut App) -> Box<dyn Component>`）。
- `crates/pi-coding-agent/src/extensions/wiring.rs:1197` —— 把 round-trip 问题改成 `serde_json::Value → CustomFactory::build → Component` 直通；移除「custom family has no `pi_protocol::Api`」注释。
- `crates/pi-coding-agent/src/extensions/custom_factory.rs`（新）—— `inventory::collect::<&dyn CustomFactory>` 注册表。

**测试**：
- `custom_factory_builds_component_from_json_opts`
- `extension_overlay_routes_mouse_to_custom_component`

#### Phase 17 — Locale 扩充（P2，~2h）

**目标**：补 ja/ko/es/fr/de 5 种 Locale。

**修改文件**：
- `crates/pi-coding-agent/src/locale.rs` —— `enum Locale { En, Zh, Ja, Ko, Es, Fr, De }`，每个 locale 字符串文件 `locale/{en,zh,ja,ko,es,fr,de}.toml`。
- `crates/pi-coding-agent/src/extensions/wiring.rs` —— 用户配置 `~/.pi/agent/settings.json` 加 `locale: "ja"`。

**测试**：
- 5 个 locale 各一个快照测试（`cargo test --test locale_japan` 等）。

### 16.6 修正后工作量

| 阶段 | 范围 | 工作量 | 累计 |
|------|------|--------|------|
| P9 | 去除 `> ` | 30 min | 0.5h |
| P10 | Footer 段补齐 + cell 验证 | 4h | 4.5h |
| P11 | StatusIndicator 5 变体 | 2h | 6.5h |
| P11.5 | ScrollView scrollbar 模式 | 2h | 8.5h |
| P12 | CustomMessage 3 种 | 2h | 10.5h |
| P13 | MouseRegion 折起 | 2h | 12.5h |
| P14 | OSC 133 + ColorMode=auto | 3h | 15.5h |
| P15 | 8 个未消费键位 | 3h | 18.5h |
| P16 | Custom overlay JS→Rust 桥 | 5h | 23.5h |
| P17 | Locale 5 种扩充 | 2h | 25.5h |
| **合计** | | **~25.5h** | — |

### 16.7 结论

后台 agent 的「0 项 MISSING」结论把文件存在性当成视觉对齐，是 overclaim。**真 MAJOR 缺口至少 15 项**，其中用户最关心的是：
1. `> ` 前缀（直接读源码确认存在）—— Phase 9
2. footer 段实际 cell 输出（基础设施存在但输出待验）—— Phase 10
3. 8 个未消费键位（注释自承「故意不实现」）—— Phase 15

把后台 agent 的发现**并入**而非**覆盖**我前面的直接读：MAJOR 缺口总数从 14 修正为 **15**，工作量从 12.5h 修正为 **25.5h**。

---

## 十七、源头文件参考

- TS pi-tui 交互组件目录：`/Users/louloulin/appx/pi/packages/coding-agent/src/modes/interactive/components/`
- TS pi-tui 交互模式：`/Users/louloulin/appx/pi/packages/coding-agent/src/modes/interactive/interactive-mode.ts`
- TS pi-tui 底层：`/Users/louloulin/appx/pi/packages/tui/src/`
- nanopi 参考：`/Users/louloulin/appx/pi/nanopi/src/mode/tui.rs`
- pi-rust 当前：`/Users/louloulin/appx/pi/pi-rust/crates/pi-tui/src/`
- pi-rust Phase 1–8 计划：`/Users/louloulin/.comet/plans/declarative-exploring-lampson.md`
- pi-rust Phase 8 验证：`/Users/louloulin/appx/pi/pi-rust/docs/PHASE_8_VISUAL_VERIFICATION.md`
- 用户配置（quietStartup=true）：`/Users/louloulin/.pi/agent/settings.json`