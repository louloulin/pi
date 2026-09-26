# pi-rust TUI vs nanopi TUI 全量差距分析报告

> 报告生成日期：2026-09-25
> 对比对象：nanopi（`/Users/louloulin/appx/pi/nanopi/src/mode/tui.rs` + `src/render/`，4406 行渲染模块）
> 参考实现：pi-rust（`/Users/louloulin/appx/pi/pi-rust/crates/pi-tui/` + `crates/pi-coding-agent/`）
> 报告性质：基于直接读源码的 cell 级对比，给出 N1–N13 改造阶段

---

## 一、执行摘要

| 维度 | nanopi 风格 | pi-rust 当前 | 差距性质 |
|------|-------------|--------------|----------|
| 渲染模式 | **scrollback + insert_before**（保终端滚动） | **alt-screen 全屏**（TS pi-tui 同款） | 设计哲学分歧 |
| 布局 | 5 行 docked 底部（overlay + status + input + cwd + stats） | 整屏 3 段（header + messages + footer） | 重新设计 |
| 输入框 | 顶 + 底 `─` 行，无左/右边框 | 满铺 SelectedBg，无顶/底边 | 大 |
| 输入前缀 | **`> ` cyan bold** | 无（已按 P9 移除） | 用户原话「不需要 `>`」应保留——这是 pi-tui 的设计 |
| 工具块配色 | **Morandi 哑光**（Indexed 65 鼠尾草绿 / 131 灰玫瑰 / 24 海军蓝） | ThemeBg slot（每主题独立配置） | pi-rust 更可主题化，但 Morandi 风格更耐看 |
| 工具块底栏 | `Took Xs` 灰斜体行 | 无 | 中（信息密度） |
| 工具块截断 | `… (N earlier lines, ctrl+o to expand)` | 已实现（P10） | 无 |
| 状态条 | **1 行独占**，工具运行时全宽蓝底条 | 嵌入到 footer 的 spinner 字符 | 大（视觉清晰度） |
| Thinking | `⠋ thinking (1.2s)` italic sage | workingHeader 含 spinner | 类似 |
| Footer 行 1 | cwd (cyan) + ` (branch)` (magenta) + ` · session N` (DarkGray) | 多段拼接 | 简化 |
| Footer 行 2 | tokens (white) + model (LightBlue) + thinking + vendor + context ratio | 已有但 `model+ctx% • 1.2k tps • medium` | 顺序/格式 |
| Token 段 | `↑X ↓Y R89 W12 CH27.4%` | 已实现 P10 | 无 |
| Context 颜色 | `>90` 红 / `>70` 黄 / else 鼠尾草 | 部分实现 | 微调 |
| Context 格式 | `1.4%/205k (auto)` | 已有 | 类似 |
| 调色板 / 选择器 | `→` 鼠尾草加粗 + 2 空格 + 右对齐描述 + 省略号截断 | `❯` + 描述短 | 大（信息密度） |
| 调色板 空匹配 | `(no matches)` dim italic + free-text 时 `⏎ use "X"` | 已部分实现 | 类似 |
| 用户消息底色 | **Indexed 238** 深灰 | ThemeBg userMessageBox | pi-rust 更可配置 |
| 用户/工具缩进 | 行内 **2 空格 indent** 一致 | 无强制 2 空格 | 小 |
| 多行输入光标 | **REVERSED 反显** + overflow `(line n/N)` | terminal cursor | 大（终端兼容） |
| 配色通用性 | **Indexed 256 色**（tmux 兼容） | truecolor + 256 fallback | 已部分支持 |
| 字体/字符 | **粗体 + 斜体** 重度使用 | 已支持 | 类似 |

---

## 二、nanopi 设计哲学（关键洞察）

### 2.1 渲染模式：scrollback-first

nanopi **不**使用 alt-screen——它把消息直接插入到终端 scrollback 中（`term.insert_before(rows_needed, …)`），只把底部的 5 行当作"docked region"实时渲染。

**优势**：
- 用户可用鼠标滚轮回看完整历史（包括折叠/展开前的工具输出）
- terminal-native 体验——终端的 copy/paste/search 全部适用
- 状态栏不会被工具输出吞掉——它们是真的滚动历史

**劣势**：
- 不能用 full-screen 漂亮的 header + 启动教学
- 工具展开（Ctrl+O）只能跳到 scrollback，不能内联展开
- 与 pi-tui 的 TS 设计哲学相反

**我们的取舍**：保留 pi-tui 的 alt-screen 模式（用户已习惯），但**采用 nanopi 的视觉元素**让 docked 区域更像它。

### 2.2 Morandi 配色哲学

```rust
// nanopi src/mode/tui.rs:5322
let blue_bg = Color::Indexed(24); // muted navy — matches Morandi

// nanopi src/mode/tui.rs:4404
let (bar_bg, bar_fg) = if is_error {
    (Color::Indexed(131), Color::Indexed(230))  // dusty rose
} else {
    (Color::Indexed(65), Color::Indexed(230))   // muted sage
};
```

- 鼠尾草绿（Indexed 65 #5f875f）—— 成功
- 灰玫瑰（Indexed 131 #af5f5f）—— 错误
- 海军蓝（Indexed 24）—— 工具运行
- 鼠尾草（Indexed 108）—— 选中/品牌色
- 全用 256 色 Indexed —— tmux-256color 完美兼容

**对比 pi-tui**：theme.json 已经定义了 `ToolPendingBg/ToolSuccessBg/ToolErrorBg`，但默认是 truecolor RGB。Morandi 风格的好处是「哑光」——低饱和度，长时间看不刺眼。

### 2.3 状态条独占 1 行

nanopi 在输入框上方独占 1 行：
- 工具运行：`⠋ bash ls -la  Elapsed 1.2s` —— 全宽海军蓝底
- thinking：`⠋ thinking (1.2s)` italic sage
- 否则空白

**对比 pi-tui**：spinner 是 footer 里的字符 `⠋ working ...`，混在 tokens/model 段之间。**问题是用户一眼看不到是否在转**——必须读懂 spinner 才知道。独占 1 行后，无论 footer 多复杂，用户都能秒判。

### 2.4 工具卡"Took Xs"底栏

每个工具块在末尾追加一行：
```
  Took 1.2s        ← italic dim on same bar bg
```

让用户知道每个工具调用耗时——非常实用的信息密度提升。

### 2.5 Tokens 段精简格式

```
↑1.2k ↓340 R89 W12 CH27.4%
```

- `↑` 输入 `↓` 输出 `R` 缓存读 `W` 缓存写 `CH%` 缓存命中率
- 缓存字段为 0 时省略（R89 之前必须有过 cache_read）
- 不显示 `tps` 和 `thinking level`（这是 pi-tui 的；nanopi 把 thinking 放进 status strip 而不是 footer）

### 2.6 调色板 `→` + 右对齐描述

```
→ /model                       Switch to a different model
  /new                  Start a fresh session in this cwd
  /fork                       Fork the session at a past turn
```

- `→` 选中，sage + bold
- 未选中灰色
- 描述 dim italic，**右对齐到行尾**
- label 过长时省略号截断，避免 description 被挤掉

### 2.7 输入框 reverse-video 光标

终端原生 cursor 在多行 wrap 时容易看不见。nanopi 改成：

```rust
let cursor_char: String = post.chars().next().map(|c| c.to_string()).unwrap_or_else(|| " ".into());
spans.push(Span::styled(cursor_char, Style::default().add_modifier(Modifier::REVERSED)));
```

**用 REVERSED 背景盖在当前字符上**，terminal cursor 关闭或跟随其后。任何终端都能看到。

---

## 三、改造阶段（N1–N13）

### N1 — Morandi Indexed 256 调色板（P1，~1h）

**目标**：把 pi-rust theme 增加一套「Morandi fallback」槽，未配置时自动回落到 Indexed 256 色。

**文件**：
- `crates/pi-tui/src/theme.rs` —— 新增 `Theme::morandi_palette()` 静态方法，返回 8 色 256-color Indexed 映射。
- `crates/pi-tui/src/components/loader.rs`（或新文件 `morandi.rs`）—— 公开 `MorandiColors` 结构 + `ToolStatusColors`/`UserBg`/`StatusStripBg` 常量。

**测试**：
- `morandi_palette_returns_indexed_colors_for_default_theme`
- `tool_status_pending_uses_navy_bg`
- `tool_status_success_uses_sage_65_bg`
- `tool_status_error_uses_rose_131_bg`

### N2 — 1 行独占 StatusStrip（P1，~2h）

**目标**：在 input box 上方加 1 行 status strip，专门显示 tool running / thinking。

**文件**：
- `crates/pi-tui/src/components/status_strip.rs`（新）—— `StatusStrip` 组件：单行 widget，bg 按状态切换（Indexed 24 海军蓝 / Indexed 108 sage italic / Reset）。内含 braille spinner（120 ms 帧）。
- `crates/pi-tui/src/app/mod.rs::render_to_buffer_impl` —— 在 message_area 与 input_area 之间切 1 行给 status_strip。

**测试**：
- `status_strip_shows_tool_running_with_navy_bg`
- `status_strip_shows_thinking_with_sage_italic`
- `status_strip_is_blank_when_idle`
- `status_strip_animates_spinner_at_120ms`

### N3 — Tool 卡 "Took Xs" 底栏 + 截断提示（P1，~1.5h）

**目标**：Tool 块末尾追加 `Took Xs` 行；超过 N 行时顶部显示 `… (N earlier lines, ctrl+o to expand)`。

**文件**：
- `crates/pi-tui/src/components/message.rs::MessageView::tool_block_lines` —— 已有 slot 渲染处追加 `Took` 行。给 `MessageItem::Tool` 加 `elapsed_ms: Option<u64>`。
- `crates/pi-tui/src/app/agent_events.rs` —— 工具结果时计算 elapsed，与 message 关联。

**测试**：
- `tool_block_appends_took_line_with_elapsed`
- `tool_block_truncation_marker_appears_when_output_exceeds_lines`
- `tool_block_truncation_marker_shows_ctrl_o_hint`

### N4 — Editor reverse-video 光标 + overflow hint（P1，~2h）

**目标**：编辑器在 `step_key` 不画原生 cursor，改用 REVERSED 背景盖字符；行数溢出时顶部显示 `(line n/N)` italic DarkGray。

**文件**：
- `crates/pi-tui/src/components/prompt.rs` —— `Prompt::render` 中给当前 cursor char 加 `Style::add_modifier(Modifier::REVERSED)`。当 `lines.len() > max_visible` 时在首行显示 hint。
- `crates/pi-tui/src/app/mod.rs::Editor` —— 关闭 `Cursor.show()`，让 ratatui 完全接管。

**测试**：
- `editor_cursor_uses_reversed_bg_on_active_char`
- `editor_overflow_hint_shows_line_n_over_N`
- `editor_overflow_hint_italic_dim`

### N5 — Tokens 段格式化为 `↑↓ R/W CH%`（P1，~1h）

**目标**：footer tokens 段遵循 nanopi 格式：缓存为 0 时省略字段。

**文件**：
- `crates/pi-tui/src/components/status.rs::StatusData` —— 已实现，复用 P15。Panic：如果 `usage_input=0 && output=0`，footer 隐藏整段。
- `crates/pi-tui/src/components/status.rs::format_tokens_summary` —— 新函数，实现 nanopi tokens_summary 算法。

**测试**：
- `tokens_summary_omits_zero_cache_fields`
- `tokens_summary_includes_ch_when_cache_read_positive`
- `tokens_summary_uses_k_for_thousands`

### N6 — Context 颜色阈值 90/70 + (auto) 后缀（P1，~30min）

**目标**：context 段按阈值上色（>90 红，>70 黄，else sage）；auto_compact=true 时追加 `(auto)`。

**文件**：
- `crates/pi-tui/src/components/status.rs::StatusBar::render_themed` —— 把 context 段颜色从 `Muted` 改为 `context_color(pct)` 解析（已部分实现 P10/P24）。
- `crates/pi-tui/src/components/status.rs::StatusData` —— 已加 `auto_compact: bool`（P24）；渲染处追加 `(auto)`。

**测试**：
- `context_above_90_uses_error_color`
- `context_above_70_uses_warning_color`
- `context_below_70_uses_sage_color`
- `context_with_auto_compact_appends_auto_suffix`

### N7 — 调色板 `→` 右对齐描述 + 省略号截断（P1，~2h）

**目标**：selector / autocomplete 行改成 `→ label_padded + gap + description`，description 右对齐，label 过长时省略号截断。

**文件**：
- `crates/pi-tui/src/components/selector.rs` —— `SelectorRow::render` 重写。`width = arrow_w(2) + gap_w(2) + desc_w + label_max = width - arrow_w - gap_w - desc_w`；label_max 不足时省略号截断。
- `crates/pi-tui/src/components/autocomplete.rs` —— 同模式。

**测试**：
- `selector_palette_row_aligns_description_to_right_edge`
- `selector_palette_row_truncates_label_with_ellipsis`
- `selector_palette_selected_uses_sage_bold_arrow`
- `selector_palette_no_match_shows_dim_italic_indicator`

### N8 — User 消息 Indexed 238 灰底（P2，~30min）

**目标**：User 消息底色采用 Indexed 238（与 nanopi 一致），如果 theme.json 没配 UserMessageBox 则回落。

**文件**：
- `crates/pi-tui/src/theme.rs::DEFAULT_THEME` —— `userMessageBg` 默认改为 `Color::Indexed(238)`（替代当前 RGB）。
- 同时保证 dark/light 主题 JSON 仍生效。

**测试**：
- `default_theme_user_message_bg_is_indexed_238`

### N9 — 卡片内 2 空格统一缩进（P2，~30min）

**目标**：tool / user / skill 块内部每行 prefix 2 空格，与 nanopi 一致。

**文件**：
- `crates/pi-tui/src/components/message.rs::tool_block_lines` —— 每行 prefix `  `。
- `crates/pi-tui/src/components/message.rs::user_block_lines` —— 同。
- `crates/pi-tui/src/components/branch_summary.rs` / `compaction_summary.rs` / `skill_invocation.rs` —— 同。

**测试**：
- `tool_card_every_line_has_two_space_prefix`
- `user_card_every_line_has_two_space_prefix`
- `skill_card_every_line_has_two_space_prefix`

### N10 — Footer 行 1 cwd + (branch) + session（P2，~30min）

**目标**：footer 行 1 改成 `cwd (branch) · session N` 格式。

**文件**：
- `crates/pi-tui/src/components/status.rs::StatusBar::render_themed` —— 行 1 重组：`cwd` cyan + ` (` + branch magenta + `)` + ` · session N` DarkGray。

**测试**：
- `footer_line1_cwd_uses_cyan_color`
- `footer_line1_branch_uses_magenta_in_parens`
- `footer_line1_session_id_is_first_8_chars`

### N11 — 调色板空匹配指示（P2，~30min）

**目标**：palette / autocomplete 0 匹配时显示 `(no matches)` dim italic；free-text 时显示 `⏎ use "X"` sage 选项。

**文件**：
- `crates/pi-tui/src/components/selector.rs::Selector::render` —— 0 visible 时显示 indicator。
- `crates/pi-tui/src/components/autocomplete.rs` —— 同。

**测试**：
- `selector_no_match_shows_dim_italic_indicator`
- `autocomplete_free_text_no_match_shows_use_typed_hint`

### N12 — 配色 Indexed 256 通用化（P2，~1h）

**目标**：在 tmux / 256-color-only 终端下，所有 ThemeColor 都降级到 Indexed 256 而非 RGB。

**文件**：
- `crates/pi-tui/src/theme.rs::Theme::resolve` —— 新增 `use_indexed_fallback()` —— 把 `Rgb(r,g,b)` 转 `Indexed(closest_256(r,g,b))`。
- `crates/pi-tui/src/terminal/color_mode.rs` —— `ColorMode::Indexed256` 触发降级。

**测试**：
- `rgb_to_indexed_256_conversion_finds_nearest`
- `theme_in_indexed_mode_uses_indexed_palette`

### N13 — Markdown 块引用 `▏` gutter 已有（P2，~0h）

P18 已实现。验证 nanopi 风格匹配：
- `crates/pi-tui/src/components/markdown.rs` —— 已有 `▏` 实现（task #58）。

---

## 四、对账与 nanopi cell 级 diff 速查

### 4.1 Status Strip（差异最大）

**nanopi**：
```
⠋ bash ls -la  Elapsed 1.2s   ← 整行海军蓝底（Indexed 24），宽 = 终端宽
```

**pi-rust 当前**：
```
[model] ⠋ working ... 42% ctx • 1.2k tps • medium
                 ↑
         spinner 混在 footer 段里
```

**改后**：
```
[model] 42% ctx • 1.2k tps • medium
─────────────────────────────────────
⠋ bash ls -la  Elapsed 1.2s   ← 海军蓝独占行
┌─────────────────────────────────────┐
│ > Hello world_                       │
└─────────────────────────────────────┘
~/nanopi (main) · session abc12345
```

### 4.2 Tool 卡底栏

**nanopi**：
```
  bash ls -la                       ← Indexed 65 sage bg
  hello world                       ← dim foreground on same bg
  Took 0.3s                         ← italic dim
```

**pi-rust 当前**：
```
[bash] ls -la                      ← ToolSuccessBg bg
  hello world
```

**改后**：
```
  bash ls -la                      ← ToolSuccessBg (theme slot)
  hello world
  Took 0.3s                        ← italic dim on same bg ← 新增
```

### 4.3 调色板

**nanopi**：
```
→ /model                       Switch to a different model
  /new                  Start a fresh session in this cwd
```

**pi-rust 当前**：
```
> /model
  Switch to a different model
```

**改后**：
```
→ /model                       Switch to a different model   ← sage + bold
  /new                  Start a fresh session in this cwd
  /fork                       Fork the session at a past turn
```

### 4.4 编辑器光标

**nanopi**：当前字符 `REVERSED` 背景；行数溢出时首行 `(line n/N)` italic dim

**pi-rust 当前**：依赖 terminal cursor；无 overflow hint

**改后**：
```
┌─────────────────────────────────────┐
│ > Hello world[REVERSED]             │   ← REVERSED bg on char under cursor
│ line 2                              │
│ (line 2/3)                          │   ← italic DarkGray if overflow
└─────────────────────────────────────┘
```

---

## 五、工作量估算

| 阶段 | 范围 | 工作量 | 累计 |
|------|------|--------|------|
| **N1** Morandi palette | 8 色 Indexed 256 + 测试 | 1h | 1h |
| **N2** StatusStrip 1 行 | 新组件 + wiring + 4 测试 | 2h | 3h |
| **N3** Tool "Took" 底栏 | elapsed 字段 + 渲染 + 3 测试 | 1.5h | 4.5h |
| **N4** Editor reverse-video + overflow | REVERSED + hint + 3 测试 | 2h | 6.5h |
| **N5** Tokens 段 `↑↓ R/W CH%` | 格式化函数 + 3 测试 | 1h | 7.5h |
| **N6** Context 颜色 + (auto) | 阈值 + 后缀 + 4 测试 | 0.5h | 8h |
| **N7** Palette `→` + 右对齐 | 重写 + 4 测试 | 2h | 10h |
| **N8** User 238 灰底 | 主题默认 + 1 测试 | 0.5h | 10.5h |
| **N9** 卡片 2 空格缩进 | 4 块统一 + 3 测试 | 0.5h | 11h |
| **N10** Footer 行 1 重排 | cwd+branch+session + 3 测试 | 0.5h | 11.5h |
| **N11** 空匹配指示 | selector/autocomplete + 2 测试 | 0.5h | 12h |
| **N12** Indexed 256 fallback | RGB→Indexed + 2 测试 | 1h | 13h |
| **N13** Markdown `▏` 验证 | 已有测试通过 | 0h | 13h |
| **合计** | | **~13h** | — |

---

## 六、风险与回滚

| 风险 | 缓解 |
|------|------|
| **N2 加 1 行后窄屏挤掉消息区** | 测试覆盖 24 行 / 30 列极窄终端；N29 已做基础窄屏降级，N2 沿用 |
| **N4 改 cursor 可能影响 IME 输入** | 仅在 `ime_compose_entered` 路径下保留 native cursor；其余路径走 REVERSED |
| **N7 重写 selector 破坏既有 dialog 测试** | 现有 selector 测试断言形状；N7 保留 `is_selected` 等核心断言，仅替换布局算法 |
| **N8 改 UserMessageBg 默认色破坏用户主题** | 仅改 `DEFAULT_THEME` 常量；现有 `assets/themes/*.json` 不变，用户配主题优先 |
| **N12 Indexed fallback 改色后视觉差异大** | 提供 `theme.use_indexed_fallback = false` 切换；不改 theme.json 格式 |

---

## 七、约束

- ts_compat 拼写保留（plugin 兼容）
- 既有 1740+ 测试保留
- 不引入新依赖
- 所有改动集中在 `crates/pi-tui/` + `crates/pi-coding-agent/` 两个 crate
- 每阶段独立 commit + 独立验证

---

## 八、Top‑5 高 ROI 项（先做这些）

| 排名 | 阶段 | 视觉冲击 | 改动量 |
|------|------|----------|--------|
| 1 | **N2** StatusStrip 1 行 | 🔴 一眼看到是否在转 | 2h |
| 2 | **N3** Tool "Took" 底栏 | 🔴 信息密度 | 1.5h |
| 3 | **N4** Editor reverse-video | 🟡 终端兼容 | 2h |
| 4 | **N7** Palette `→` 右对齐 | 🟡 描述可见 | 2h |
| 5 | **N1 + N12** Morandi Indexed 256 | 🟡 整体耐看 | 2h 合计 |

先做这 5 项（共 ~10h），剩余 N5/N6/N8–N11 是渐进打磨。

---

## 九、源头文件参考

- nanopi TUI 主文件：`/Users/louloulin/appx/pi/nanopi/src/mode/tui.rs`（6997 行）
- nanopi render 模块：`/Users/louloulin/appx/pi/nanopi/src/render/`（12 文件，4406 行）
  - `status_line.rs` — cwd / branch / tokens / context ratio 算法
  - `menu.rs` — selector 状态机 + `→` cursor 算法
  - `markdown.rs` — streaming Markdown 解析
  - `panel.rs` — ToolPanel（pending/running/done/err）
  - `spinner.rs` — braille 帧表 + 120 ms 间隔
- pi-rust 当前：
  - `crates/pi-tui/src/app/mod.rs` — render loop
  - `crates/pi-tui/src/components/message.rs` — message rendering
  - `crates/pi-tui/src/components/status.rs` — footer
  - `crates/pi-tui/src/components/selector.rs` — selector
  - `crates/pi-tui/src/components/prompt.rs` — editor
  - `crates/pi-tui/src/theme.rs` — theme
- Phase 1–8 计划：`/Users/louloulin/.claude/plans/declarative-exploring-lampson.md`
- Phase 9+ 计划：`/Users/louloulin/appx/pi/pi-rust/docs/PHASE_9_PLUS_UI_GAP_ANALYSIS.md`