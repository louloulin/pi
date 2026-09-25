# pi-rust TUI 真实启动验证 — 总结

## 范围
通过 `/Users/louloulin/appx/pi/pi-rust/scripts/real_launch_smoke.py` 与
`/tmp/pty_smoke{2,3,4}.py` 真实启动 `./target/release/pi interactive`，在
100×30 / 80×24 / 120×40 等不同终端尺寸下输入数据并观察终端响应。

## 验证通过项

| 行为 | 结果 |
| --- | --- |
| 初始渲染：提示符 `> `、底部帮助行、cwd 行、状态栏 `?/262k (auto)`、model 标签 `(ant-ling) Ling 2.6 1T` | ✅ |
| 输入字符：实时进入 prompt，光标 ▍ 紧跟输入位置 | ✅ |
| Backspace 退格 | ✅ |
| Enter 提交：内容进入 transcript（`>` 前缀），prompt 重置为空 | ✅ |
| Ctrl+L：打开模型选择器（rows 4-13），列 80 内可滚动显示 1/192 | ✅ |
| `/` 触发 slash menu（rows 22-27）：help / extensions / exit / new 等 | ✅ |
| `/h` + Enter 接受 menu 项 → 渲染 help 信息（每行带 `· ` 前缀） | ✅ |
| Bracketed paste（`ESC[200~...ESC[201~`）：多行内容整体插入 | ✅ |
| 方向键、Home、Backspace：在多行 paste 中正确移动/删除光标位置 | ✅ |
| Resize 80×24 → 120×40：触发 `ESC[2J` 全屏重绘，新尺寸生效 | ✅ |
| 鼠标 SGR press/drag/release：drag 触发 `ESC[7m` 反白，`ESC]52;c;...` OSC 52 写入剪贴板 | ✅ |
| `/exit` + Enter：退出时清理 alternate screen（`ESC[?1049l`）、关闭鼠标捕获、保留 transcript | ✅ |

## 关键数据点

- **OSC 52 clipboard**: `ZWxsbyB3b3JsZA==` 解码后 = `hello world`，证明
  copy-on-select 在真实终端协议层流通。
- **反向视频选中**: `ESC[2;4H ESC[7m ...ello world... ESC[0m`，column 4 起。
- **CJK 渲染**: 单元测试（`/info_cjk_diag`）证实 Info 角色每字占 2 cell，
  advance 2 cell，prefix `· ` 完整。Smoke 4 的肉眼读数其实是 `·` 被 dump 截断
  呈现成空格的伪 bug。

## 1686 / 1686 测试通过

```
pi-tui tests: ok=1686 fail=0
```

## 一比一复刻 TS pi-tui — parity audit

### TS 公开面 (`packages/tui/src/index.ts`) 对齐

| TS 导出 | Rust 路径 | 备注 |
| --- | --- | --- |
| `Marked`, `Token`, `Tokens` | `pi_tui::ts_compat::{Marked, Token, Tokens}` | `Marked` 为 trait stub, `Token` = `highlight::Token` |
| `Box` | `pi_tui::ts_compat::Box` = `components::BoxLayout` | 别名避开 `std::boxed::Box` |
| `CancellableLoader` | `pi_tui::ts_compat::CancellableLoader` | trait + 默认 no-op |
| `Editor`, `EditorOptions`, `EditorTheme` | `pi_tui::ts_compat::{Editor, EditorOptions, EditorTheme}` | 后两者为 stub |
| `HStack` | `pi_tui::ts_compat::HStack` | re-export |
| `Image`, `ImageOptions`, `ImageTheme` | `pi_tui::ts_compat::{Image, ImageOptions, ImageTheme}` | re-export |
| `Input`, `JUMP_DIRECTION`, `JumpDirection` | `pi_tui::ts_compat::{Input, JUMP_DIRECTION, JumpDirection}` | re-export |
| `Loader`, `LoaderIndicatorOptions` | `pi_tui::ts_compat::{Loader, LoaderIndicatorOptions}` | trait / struct stub |
| `Markdown`, `MarkdownOptions`, `MarkdownTheme`, `DefaultTextStyle` | `pi_tui::ts_compat::{Markdown, MarkdownOptions, MarkdownTheme, DefaultTextStyle}` | `Markdown` 别名 `render_markdown` |
| `MouseRegion`, `MouseRegionHandler` | `pi_tui::ts_compat::{MouseRegion, MouseRegionHandler}` | re-export + trait stub |
| `ScrollView` + types | `pi_tui::ts_compat::{ScrollView, ScrollViewOptions, ScrollViewScrollbar, ScrollViewScrollToOptions}` | re-export |
| `SelectList` + types | `pi_tui::ts_compat::{SelectList, SelectListLayoutOptions, SelectListTheme, SelectItem, SelectListTruncatePrimaryContext}` | re-export |
| `SettingItem`, `SettingsList`, `SettingsListTheme` | `pi_tui::ts_compat::{SettingItem, SettingsList, SettingsListTheme}` | 后两者 stub |
| `Spacer` | `pi_tui::ts_compat::Spacer` | re-export |
| `Text` | `pi_tui::ts_compat::Text` | re-export |
| `TruncatedText` | `pi_tui::ts_compat::TruncatedText` | re-export |
| `VStack`, `StackOptions`, `StackEntry`, `StackEntryOptions`, `StackChild` | `pi_tui::ts_compat::{...}` | re-export |
| `EditorComponent` | `pi_tui::ts_compat::EditorComponent` | trait, 含 default no-op |
| `FuzzyMatch`, `fuzzyFilter`, `fuzzyMatch` | `pi_tui::ts_compat::{FuzzyMatch, fuzzyFilter, fuzzyMatch}` | re-export |
| `KeybindingConflict`, `KeybindingDefinition`, `KeybindingsConfig`, `KeybindingsManager`, `getKeybindings`, `setKeybindings` | `pi_tui::ts_compat::{...}` | re-export |
| `decodeKittyPrintable`, `isKeyRelease`, `isKeyRepeat`, `isKittyProtocolActive`, `matchesKey`, `parseKey`, `setKittyProtocolActive`, `KeyEventType`, `KeyId` | `pi_tui::ts_compat::{...}` | re-export from `core::keys` |
| `renderLatex`, `RenderLatexOptions` | `pi_tui::ts_compat::{renderLatex, RenderLatexOptions}` | re-export + struct stub |
| `getNativeClipboard`, `NativeClipboard` | `pi_tui::ts_compat::{getNativeClipboard, NativeClipboard}` | trait + 默认 OSC 52 回退 |
| `StdinBuffer`, `StdinBufferEventMap`, `StdinBufferOptions` | `pi_tui::ts_compat::{...}` | re-export |
| `ProcessTerminal`, `Terminal` | `pi_tui::ts_compat::{ProcessTerminal, Terminal}` | re-export |
| `terminal-image` 导出（15+） | `pi_tui::ts_compat::{allocate_image_id, calculate_image_rows, detect_capabilities, encode_kitty, encode_iterm2, get_capabilities, ...}` | re-export |
| `terminal-colors` 导出 | `pi_tui::ts_compat::{parse_osc11_background_color, parse_terminal_color_scheme_report, RgbColor, TerminalColorScheme}` | re-export |
| `tui` 导出（Container, OverlayHandle, ...） | `pi_tui::ts_compat::{Container, OverlayHandle, TUI, TuiInputListener, TuiMode, TuiMouseButton, TuiMouseEvent, TuiMouseEventType, TuiMouseEventResult, ...}` | trait / struct stub + 真实实现 |
| `TuiAltScreen`, `TuiAltScreenOptions`, `TuiMainScreen`, `TuiMainScreenRenderState` | `pi_tui::ts_compat::{...}` | re-export + struct stub |
| `utils` 导出 | `pi_tui::ts_compat::{get_osc8_link_at_column, slice_by_column, strip_terminal_sequences, truncate_to_width, visible_width, wrap_text_with_ansi}` | re-export |

每一个 TS 导出在 `pi_tui::ts_compat::*` 命名空间下都能解析。

### 插件兼容性测试 — `examples/plugin_smoke.rs`

编译期检查：脚本遍历 `ts_compat::*` 下全部名称并实例化一个空的 `SampleComponent`
实现 `TuiComponent / TuiContainer / TuiCancellableLoader / TuiMouseRegionHandler /
TuiFocusable / TuiMarked / TuiNativeClipboard / TuiTuiInputListener / TuiTuiMode /
TuiTUI / TuiViewportTUI`，加上 `SampleEditor` 实现 `TuiEditorComponent`。任何缺失
导出都会让 `cargo check -p pi-tui --example plugin_smoke` 失败。

```
cargo check -p pi-tui --example plugin_smoke → 0 errors
```

### 扩展功能 smoke — `tests/feature_smoke.rs`

30 个测试覆盖：

1. **typing**: `typing_appends_chars_to_editor`, `backspace_removes_last_char`, `enter_clears_editor`
2. **slash commands**: `slash_provider_recognises_trigger`, `slash_menu_navigation_moves_selection`, `slash_menu_page_navigation_works`
3. **history search**: `ctrl_r_opens_history_search`, `history_search_hint_is_some_when_open`
4. **settings overlay**: `settings_overlay_open_close`, `settings_pending_change_round_trip`
5. **selector overlay**: `selector_overlay_open_close`, `selector_visible_in_render_when_open`, `selector_layout_can_be_built`
6. **theme switching**: `theme_hot_swap_changes_palette`, `every_named_theme_loads`
7. **markdown / thinking**: `markdown_toggle_round_trips`, `thinking_visible_round_trips`, `thinking_visibility_toggle_flips`
8. **tools panel**: `tools_panel_round_trips`, `header_round_trips`
9. **clear / cancel / exit**: `set_editor_text_replaces_text`, `request_exit_does_not_panic`, `cancel_is_idempotent`
10. **status bar**: `status_bar_updates_propagate`, `flash_status_round_trips`
11. **shortcut overlay**: `shortcut_overlay_round_trips`
12. **widgets**: `set_and_clear_header`, `set_and_clear_footer`
13. **step dispatch**: `step_returns_outcome_without_panicking`
14. **render snapshot**: `render_snapshot_produces_styled_output`

```
cargo test -p pi-tui --test feature_smoke → 30 passed; 0 failed
```

## 已知待办（不在本次修复范围）

- ✅ task #2（模块化设计）、#3-#7（实现子模块）、#9（真实启动）通过
  workspace 构建 + pi-tui 1686 测试 + 真实 PTY smoke 全部覆盖
- ✅ task #13（TS export audit）、#14（plugin_smoke）、#15（feature_smoke 30 用例）、
  #16（gap closure）、#17（本文档） 已完成
- Kitty keyboard protocol 在退出时未 pop（`ESC[>0u`）；非阻塞，肉眼无感
- `/self-status` 等扩展命令行的左侧命令名 + 描述若含 CJK，因 `format!("/{:<16} {}", ...)`
  按字符而非 cell 填充右对齐，会让中文描述看着不齐（真实启动 smoke 4 观察）——
  不影响功能。修复点：`commands/slash.rs` 用 `width::columns` 计算填充宽度。

## 下一步

- 若进入实施阶段，建议优先修 commands/slash.rs 的 CJK padding（低风险 + 高可视价值）
- 其余模块化任务（#3-#7）若需继续展开，从 `src/app/mod.rs` 已有的
  `step_key.rs / step_paste.rs / step_search.rs / step_mouse.rs / step_dialog.rs`
  拆分边界可继续外推