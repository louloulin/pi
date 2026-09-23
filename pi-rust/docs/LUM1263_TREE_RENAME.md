# LUM-1263 — `/tree` 改名 UI：`app.tree.editLabel` 接线（`app.*` 43/44 → 44/44）

> 结论先行：`/tree` 现在能改节点标签了。`Shift+L` 进入编辑（输入框复用 `Editor`），
> 逐字符输入 / 退格 / 方向键 / 词移动都走composer 的编辑语义，`Enter` 提交（空值 =
> 删除标签）、`Esc` 取消，提交后标签以 `[label] ` 前缀画在树上并写回会话文件。
> `app.*` 接线率 **43/44 → 44/44**，`silent` / `advertised` 双清零。

## 0. 交付清单

| 文件 | 改动 |
|---|---|
| `pi-rust/crates/pi-tui/src/tree.rs` | `TreeLabelEditor` + `TreeLabelAction`（编辑态）、`TreeItem::user_label` / `TreeRow::user_label`、`tree_selector_items` 画 `[label] ` 前缀 |
| `pi-rust/crates/pi-tui/src/selector.rs` | `Selector::with_body` / `body`：用一组行替换 item 列表（上游「标签输入框替换树列表」的等价物） |
| `pi-rust/crates/pi-coding-agent/src/commands/tree.rs` | `LABEL_ENTRY_KIND`、`read_labels`（读回标签）、`append_label_change`（写入）、`TreeView::label_editor`、`labeled-only` 改判 `has_label`、标签时间戳、footer 加 `shift+l label` |
| `pi-rust/crates/pi-coding-agent/src/interactive.rs` | `app.tree.editLabel` 分支 + `open_tree_label_editor` / `handle_tree_label_editor_key`（编辑态独占键盘），帧 dump 用例 |
| `pi-rust/crates/pi-tui/src/keybindings.rs` | `CONSUMED_APP_ACTIONS` 增 `app.tree.editLabel`（43 → 44） |
| `pi-rust/crates/pi-coding-agent/tests/startup_header.rs` | `SELECTOR_SCOPED` 增 `app.tree.editLabel`（组件内宣传，不是全局快捷键） |
| `pi-rust/scripts/app_action_coverage.py` | **修正**：不再把 `CONSUMED_APP_ACTIONS` 自己的字面量算作消费证据（否则 `--check-consumed` 恒真） |
| `pi-rust/scripts/keybinding_coverage.py` | `KNOWN_UNCONSUMED` 清空（最后一个死 id 接线）；消费扫描跳过同一张注册表文件 |
| `pi-rust/docs/screenshots/lum1263-tree-rename-{idle,editing,committed}.{txt,png}` | 三帧 frame-buffer 截图 |
| `pi-rust/docs/RUST_TS_PARITY_METRICS.md` | §0.18 本轮复测 |

未触碰：`pi-ai` / `pi-agent-core` / `pi-protocol` / `pi-extensions` / `pi-session`；
`pi-tui/src/{editor.rs,app.rs,prompt.rs}` 的粘贴通道（LUM-1460 落地、LUM-1461 在飞）；
上游 `packages/**`。无新依赖。

## 1. 上游逐行对照

### 1.1 键位 → 编辑态 → 提交（`tree-selector.ts`）

| 上游 | 行 | Rust 落点 |
|---|---|---|
| `kb.matches(keyData, "app.tree.editLabel")` → `this.onLabelEdit(selected.entry.id, selected.label)` | `:1084-1088` | `interactive.rs:2252`（`handle_picker_key` 的 `PickerKind::Tree` 分支）→ `open_tree_label_editor` (`:2551`) |
| `showLabelInput(entryId, currentLabel)`：`new LabelInput(entryId, currentLabel)`，`treeContainer.clear()` + `labelInputContainer.addChild(...)` | `:1364-1382` | `TreeLabelEditor::new`（`tree.rs:296`）+ `TreeView::label_editor`（`commands/tree.rs:184`）；`Selector::with_body`（`selector.rs:456`）替换列表 |
| `LabelInput`：`Input` 预填当前 label（`setValue` → 光标在尾） | `:1287-1293` | `TreeLabelEditor::new` 里 `Editor::set_text`（光标在尾，`editor.rs:635`） |
| `LabelInput.handleInput`：`tui.select.confirm` → `onSubmit(id, value \|\| undefined)`；`tui.select.cancel` → `onCancel`；其余交给 `Input.handleInput` | `:1312-1322` | `TreeLabelEditor::handle_key`（`tree.rs:322`）：`matches_with_fallback` 先答 `tui.select.confirm` / `tui.select.cancel`，其余 `Editor::handle_key` |
| `LabelInput.render`：`Label (empty to remove):` / input 行 / `save  cancel` 提示 | `:1297-1310` | `TreeLabelEditor::render_lines`（`tree.rs:348`），光标用本仓 composer 的 `▍` 约定 |
| `onSubmit` → `treeList.updateNodeLabel(id, label)` + `onLabelChangeCallback(id, label)` | `:1379-1381` | `handle_tree_label_editor_key` 的 `Commit` 分支：`append_label_change` 写会话后 `refresh_tree_selector`（标签从会话读回，行自然重画） |
| `handleInput` 在编辑态整条路由给 `labelInput` | `:1400-1405` | `interactive.rs:1038`：`kind == Tree && label_editor.is_some()` 时所有键先给编辑态，picker 快捷键不再认领 |
| 树行渲染顺序 `prefix + foldMarker + pathMarker + label + labelTimestamp + content`，label = `[<label>] ` | `:740-749` | `tree_selector_items`（`tree.rs:253`）：`prefix + marker + "[label] " + label` |
| `labeled-only`：`flatNode.node.label !== undefined` | `:374-376` | `TreeFilter::passes`（`commands/tree.rs:129`）新增 `has_label` 参数 |
| label 行显示 `[label: X]` / `(cleared)` | `:839-840` | `entry_display_text`（`commands/tree.rs:406`） |
| `TREE_HELP_ITEMS` 宣传 `app.tree.editLabel` = `label` | `:1222` | `tree_footer`（`commands/tree.rs:346`）加 `shift+l label` |
| `showLabelTimestamps` 只对**有 label 的行**画 label 自己的时间戳 | `:741-744` | `to_tree_item`（`commands/tree.rs:536`）：有标签用标签条目时间戳，无标签保留条目时间戳（见 §5 偏差） |

### 1.2 回调接线与持久化

| 上游 | 行 | Rust 落点 |
|---|---|---|
| `new TreeSelectorComponent(..., onLabelChange)`，回调是 `(entryId, label) => sessionManager.appendLabelChange(entryId, label)` | `interactive-mode.ts:5216-5219`, `:5331-5334` | `handle_tree_label_editor_key` 的 `Commit` 分支 → `commands::tree::append_label_change` |
| `appendLabelChange(targetId, label)`：追加 `label` 条目，`parentId = leafId`，`label` 为空则删除 | `session-manager.ts:1246-1267` | `append_label_change`（`commands/tree.rs:321`）：`SessionWriter::resume` 后 `append(SessionEntry::Extension { kind: "label", payload: {targetId, label} })`，`checkpoint()` 落盘 |
| `_buildIndex`：顺序读条目，`label` 覆盖、`null` 删除 | `session-manager.ts:974-990` | `read_labels`（`commands/tree.rs:281`）：同一套覆盖/删除语义 |
| `getTree()` 把 `labelsById` 解析进 `node.label` | `session-manager.ts:1331-1332` | `to_tree_item` 用 `TreeLabels` 给 `TreeItem::with_user_label` |

### 1.3 存储形态（Rust 侧的必要映射）

上游有原生 `label` 条目类型；`pi-protocol` 的 `SessionEntry` 没有该变体，且本轮不许改
`pi-protocol` / `pi-session`。因此标签以 `SessionEntry::Extension` 写入，
`extension = kind = "label"`，`payload = {"targetId": ..., "label": ...}`；
`pi-session` 的 writer 把它落成 `custom` 条目（`custom_type = "label"`，payload 进 `data`），
reader 反向映射回 `Extension { extension: "custom", kind: "label", payload }`。
`read_labels` 兼容两种载荷形态：本仓自己写的 `custom`（`data` 解包后 `{targetId, label}`）
与上游原生 `label` 条目（`pi-session` 走 `other =>` 透传整包，`targetId` / `label` 在顶层）。
详见 `pi-session/src/{writer.rs,reader.rs}` 的映射表。

## 2. 编辑态语义（`TreeLabelEditor`）

- **进入**：`Shift+L`（`app.tree.editLabel`，`keybindings.rs:337` 的 `shift+l`）在 `/tree`
  高亮行上打开；预填该节点当前标签，光标在尾。
- **逐字符输入 / 退格 / Delete / 左右 / Home/End / Alt+←→ 词移动 / Ctrl+W / undo / kill-ring**：
  全部是 `Editor::handle_key` 自己的行为（**没有第二套输入框**）。
- **`Enter`（`tui.select.confirm`）**：提交。空白 → `Commit(None)` = 删除标签；其余 trim 后
  折行成单行（`\n`/`\r`/`\t` → 空格），因为上游的 `Input` 不可能含换行（本仓 `Shift+Enter`
  能插换行）。
- **`Esc`（`tui.select.cancel`）**：`Cancel`，不写会话。
- **空 label**：提交 `None` → 追加一条 `label: null` 的条目；`read_labels` 删除该目标的标签，
  行前缀消失。会话是 append-only，历史条目保留。

## 3. 验证（可复跑，全部本机实测）

### 3.1 接线门禁

```text
$ python pi-rust/scripts/app_action_coverage.py pi-rust
wired: 44/44 (100.0%)
advertised: 0/44 (0.0%)
silent: 0/44 (0.0%)

$ python pi-rust/scripts/app_action_coverage.py pi-rust --check-consumed
CONSUMED_APP_ACTIONS: 44 entries; measured wired: 44
in sync: the header's hint filter matches the code        # exit 0

$ python pi-rust/scripts/keybinding_coverage.py pi-rust --check
tui.*: 49/49 consumed
app.*: 44/44 consumed
in sync: 0 known-unconsumed id(s)                          # exit 0
```

### 3.2 反向验证（把 `editLabel` 分支摘掉，门禁必须失败）

只删 `interactive.rs:2252-2255` 的四行（`matches("app.tree.editLabel", ...)` 分支），
两条命令都必须挂：

```text
$ python pi-rust/scripts/app_action_coverage.py pi-rust --check-consumed
CONSUMED_APP_ACTIONS: 44 entries; measured wired: 43
  FALSE AD   app.tree.editLabel is listed as consumed but no code resolves it
                                                           # exit 1

$ python pi-rust/scripts/keybinding_coverage.py pi-rust --check
tui.*: 49/49 consumed
app.*: 43/44 consumed
  UNCONSUMED app.tree.editLabel  (crates\pi-coding-agent\src\keybindings.rs)
NEW DEAD ID(S): ['app.tree.editLabel']                     # exit 1
```

复原后两条都回到 exit 0（见 §3.1）。

**本轮为此修了工具**：`app_action_coverage.py` 原先会把 `CONSUMED_APP_ACTIONS`
自己的字面量当成 handler 证据——那么「列在表里」就永远等于「已接线」，
`--check-consumed` 恒真，上面的反向验证根本挂不了。`scan()` 现在同时跳过
`crates/pi-tui/src/keybindings.rs`（注册表，不是消费者），`keybinding_coverage.py`
的消费扫描也同样跳过该文件。修完后 **44/44 不变**（已核对：没有任何 id 的消费证据
只剩注册表一处），但门禁从此可反证。

### 3.3 测试

```text
$ cargo test --offline -p pi-tui -p pi-coding-agent
pi-tui  lib:  438 passed / 0 failed      (基线 429 → +9 = 7 tree + 2 selector)
pi-coding-agent lib: 590 passed / 8 failed   (基线 584 → +6)
其余 99 个 target: 全 ok
```

新增用例：

| 位置 | 用例 |
|---|---|
| `pi-tui/src/tree.rs` | `a_user_label_is_drawn_before_the_display_text`、`the_label_editor_prefills_the_current_label_and_commits_it`、`an_empty_label_commits_as_remove`、`escape_cancels_without_touching_the_buffer`、`the_label_editor_is_the_composer_editor`（Ctrl+W + Home 证明复用 `Editor`）、`a_hard_line_break_is_folded_to_a_space_on_commit`、`the_label_editor_renders_upstream_rows_with_a_caret` |
| `pi-tui/src/selector.rs` | `a_body_override_replaces_the_item_rows_and_keeps_title_and_footer`、`without_a_body_the_item_rows_are_drawn` |
| `pi-coding-agent/src/interactive.rs` | `shift_l_opens_the_label_editor_on_the_highlighted_row`、`the_label_editor_owns_the_keyboard_until_it_closes`、`committing_a_label_writes_it_to_the_session_and_the_row_shows_it`、`clearing_the_label_removes_it`、`the_labeled_only_filter_keeps_labeled_rows`、`frame_dump_tree_rename` |

### 3.4 既有环境类失败：与基线**逐条相同**

本机（Windows runner）有一批既有环境类失败（真 `bash`、`/tmp`、绝对路径、node fs）。
同一命令 `cargo test --offline -p pi-tui -p pi-coding-agent`（无改动基线，3 次复跑）
与带改动（1 次复跑，`lum1263-final2.log`）都得到**同一集合的 28 条**，`diff` 为空：

```text
a_throwing_extension_tool_becomes_an_error_result              bash_runs_ls
commands::export::tests::cli_export_propagates_the_upstream_error_text
explicit_extension_flag_loads_a_file_outside_the_project
export::session_file::tests::missing_files_report_the_upstream_message
extension_discovered_resources_reach_the_model                 extension_theme_paths_are_accepted_and_ignored
extensions::js_loader::tests::load_extensions_loads_an_esm_extension_from_disk
extensions_dir_flag_loads_every_extension_in_a_directory        find_rejects_absolute_paths
ls_rejects_absolute_path                                        ls_rejects_absolute_path_for_grep_and_find
no_extensions_flag_skips_discovery                              no_skills_flag_suppresses_extension_discovered_skills
paths::tests::absolute_paths_stay_absolute                      print_mode_executes_the_bash_tool_and_feeds_the_result_back
print_mode_runs_the_read_tool_against_a_real_file               print_mode_surfaces_real_tool_failures_as_error_results
print_mode_unknown_slash_command_still_reaches_the_model        project_extensions_directory_is_discovered_and_its_tool_runs
relative_entries_are_absolute_in_the_prompt                     reload_rereads_keybindings_and_ui_settings_mid_session
resource_loader::tests::prompt_contains_builtin_tools_context_and_skills
rpc_mode_executes_the_bash_tool_and_feeds_the_result_back
tools::mod_ignore::tests::absolute_paths_are_rejected
trust::tests::detects_trust_requiring_project_resources         trust::tests::has_no_trust_requiring_resources_in_an_empty_project
untrusted_project_extensions_are_skipped_but_user_extensions_load
```

**诚实条目**：`print_mode.rs::sigint_or_clean_exit` 在**合并跑**时偶尔会以
`unexpected exit code: Some(1)` 失败（带改动 4 次合并跑里出现 2 次，基线 3 次都没出现；
但它在单跑 3/3 通过、`-p pi-coding-agent` 整包跑 2/2 通过，手跑 `target/debug/pi
--print=hello` 退出码 0）。判定为机器负载敏感的环境抖动，与本轮改动无关；上面引用的
基线/改动对比用的是**都无该抖动的那一对**（`lum1263-baseline*.log` vs `lum1263-final2.log`）。

### 3.5 编译 / lint

```text
$ cargo fmt --all -- --check                                  # exit 0
$ cargo clippy --offline -p pi-tui -p pi-coding-agent --all-targets
   pi-tui: 0 告警   pi-coding-agent: 0 告警
   （只剩 rquickjs-core(vendor) 13 条与 pi-extensions 1 条的既有告警）
```

## 4. 截图（frame-buffer，**不是** PTY 实拍）

本机没有 PTY（`import pty` 不可用），三帧由测试打印 `App::render_to_buffer` 的 cell
grid，再由 `scripts/frame_to_png.py` 上色：

| 文件 | 内容 |
|---|---|
| `docs/screenshots/lum1263-tree-rename-idle.{txt,png}` | `/tree` 打开、光标在 `e1`，没有任何编辑框 |
| `docs/screenshots/lum1263-tree-rename-editing.{txt,png}` | `Shift+L` 后：`Label (empty to remove):` + `checkpoint▍` + `Enter save  Esc/Ctrl+C cancel`，树列表被输入框替换 |
| `docs/screenshots/lum1263-tree-rename-committed.{txt,png}` | `Enter` 后：`❯ [checkpoint] user: u1`，编辑框关闭 |

**证据分级**：这是**冻结帧**，证明「画出来的东西」，不证明按键/字节时序——时序由
§3.3 的 `App`/driver 级用例覆盖。图上 caption 已写明非 PTY。
（`▍` 是本仓 composer 的既有光标约定。）

## 5. 偏差清单

| 上游 | 本仓 | 理由 / 影响 |
|---|---|---|
| `label` 是原生条目类型 | 存 `custom` 条目（`kind = "label"`） | 不改 `pi-protocol`（本轮约束）；`read_labels` 兼容上游原生 `label` 条目 |
| 标签输入框**替换**树列表 | `Selector::with_body` 替换 item 行，保留标题/分隔线/footer | 本仓 `Selector` 统一拥有这三行；替换语义一致，多留了 footer 提示 |
| 光标用反显（`Input` 的 reverse） | 用 `▍` 字形 | 本仓 dump 是纯文本网格，无颜色；与 composer 光标一致 |
| `showLabelTimestamps` 只画标签时间戳，未打标签的行不画 | 有标签 → 标签时间戳；无标签 → 条目时间戳（现状保留） | 让 `Shift+T` 在还没有任何标签的树上仍可观察；已有回归用例锁定 |
| `formatLabelTimestamp` 相对日期（今天 `HH:MM`，否则 `M/D`） | `HH:MM:SS`（`format_timestamp`） | 沿用本仓既有时间戳格式，不新造第二套 |
| 每行一个 label 条目，`getTree` 里 label 条目本身也是一个节点（default 视图隐藏） | 同 | 无偏差；`labeled-only` 只留有标签的目标行，标签条目自身仍被 settings 过滤隐藏 |
| 编辑态按 `kb.matches` 走 `LabelInput` | `TreeLabelEditor` 用 `matches_with_fallback` | 与全仓其它 chord 一致：装了 coding-agent 表用表，裸 `pi-tui` 注册表用内置键 |

## 6. 已知限制与后续建议

1. **指针**：编辑态替换列表后，`Selector` 的「点行 = 移动光标」几何仍按 item 行算。
   本仓没有 `/tree` label 输入框的鼠标面（上游 `Input` 也主要吃键盘），暂不处理；
   若要补，应在 `App::step_modal_mouse_gesture` 里给 body 模式加一条「不映射 item 行」。
2. **`session_leaf` 与标签条目的叶子语义**：追加标签条目后，`session_tip` 会指向该标签条目，
   于是 default 视图里可能暂时没有 `•` 活动路径标记（标签条目被 settings 过滤隐藏）。
   这与上游 `leafId = 最后一个条目` 的结构一致，不是本轮引入的缺陷；若要更贴近直觉，
   可在 `compute_active_subtrees` 里把「隐藏的叶子向上归到最近可见祖先」，但那会改动
   Stage 68 的既有语义，建议单开一格。
3. **上游原生 `label` 条目导入**：`read_labels` 已兼容整包透传形态，但没有上游写出的
   `label` 条目测试夹具；若后续做上游会话导入回归，应补一条。
4. `pi-rust/docs/FEATURE_PI_RS_STATUS.md` 里仍有历史条目写「只剩 `app.tree.editLabel`」，
   属该文档的按轮快照，未回改；当前口径以本文与 `RUST_TS_PARITY_METRICS.md` §0.18 为准。
