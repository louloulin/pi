# LUM-1319 — Composer prompt history: cross-session persistence, `Ctrl+R` reverse search, attachment recall

> scope: `pi-rust/crates/pi-tui/src/{history_store,editor,prompt,app,keybindings,locale}.rs`,
> `pi-rust/crates/pi-coding-agent/src/{main,cli,paths,interactive,commands/slash}.rs`,
> `pi-rust/crates/pi-tui/tests/{composer_history,composer_images,startup_header}.rs`,
> `pi-rust/crates/pi-coding-agent/tests/history_file.rs`,
> `pi-rust/scripts/pty_scenarios/lum1319-history-*.json`
> branch: `work/LUM-1319` → merged `origin/feature/pi.rs`（`231abbc4d`）→ fast-forwarded into `feature/pi.rs`
> reference: codex `codex-rs/tui/src/bottom_pane/chat_composer_history.rs` +
> `chat_composer/history_search.rs` + `keymap.rs` (local checkout `7d99ee8`, §5)

一句话结论：composer 的 prompt history 从「进程内 `VecDeque<String>`，退出即丢」升级为
**跨进程持久化 + `Ctrl+R` 反查 + 附件召回**——`~/.pi/agent/history.jsonl`（0600，逐行 JSON）在
启动时读回最近 100 条，提交时追加；会话内条目连图片 chip 一起召回（codex 的 `ChatComposerHistory`
语义），持久条目只有文本（与 codex 相同）；`Ctrl+R` 是独立的 reverse incremental search
（`tui.editor.historySearch`，默认 `ctrl+r`），进了 `/hotkeys` 与启动头广告面。

---

## 1. 真实审计：本轮之前发生了什么

LUM-1312 的 composer 已经能换行、能按视觉行编辑，但 **history 仍然只在进程内**：

| 行为 | 本轮之前（`4ad27d5f8` = `feature/pi.rs` 树，真 PTY/单元测试实测） | 本轮之后（真 PTY 实测） |
|---|---|---|
| 退出后重开 | `Editor::history: VecDeque<String>` 随进程消亡；`Up` 只能召回**本次进程**提交过的 prompt | `~/.pi/agent/history.jsonl` 逐行 JSON；新进程启动时读回最近 100 条（截图 B 面板 1：`Up` 召回**上一个进程**提交的 `third prompt`） |
| 带图片的草稿召回 | `push_history` 只记纯文本，召回走 `set_text_internal` → `images.clear()`，`image_count()` 归零，`[Image #N]` 变成死文本 | 会话内条目捕获原始 buffer（chip 哨兵）+ attachments，召回后 `image_count()` 与 `display_text()` 都还原（`composer_history.rs` 的 4 个单测） |
| 反查 | 无（`tui.editor.historyPrevious/Next` 默认不绑定，只有 `Up`/`Down`） | `Ctrl+R` 打开 `reverse-i-search:`，增量过滤、`Ctrl+R`/`Up` 走更旧、`Ctrl+S`/`Down` 回更新、`Esc`/`Ctrl+C` 还原草稿、`Enter` 接受 |
| 广告面 | `/hotkeys` 与启动头都没有 history 相关行 | `/hotkeys` 的 navigation 段列出两条新 chord；启动头多一行 `Ctrl+R/Ctrl+S to search history` |
| 隐私/体积 | 无文件，无从谈起 | 文件 0600（每次 append 复查权限）、100 行裁剪、只写文本；`pi --clear-history` 是显式清理入口 |

## 2. 真 PTY 证据（不是设计意图）

三个 scenario 驱动的是真二进制 `target/debug/pi`、真 PTY、pyte 仿真，截图与字符网格都在
`docs/screenshots/`：

```bash
# 会话 A：写
python3 scripts/pty_capture.py --bin target/debug/pi \
  --steps scripts/pty_scenarios/lum1319-history-session-a.json \
  --out docs/screenshots/lum1319-history-session-a.png \
  --text-out docs/screenshots/lum1319-history-session-a.png.txt \
  --home /tmp/lum1319/home          # ① 记下这个 --home

# 会话 B：**新进程**读回 + 反查（同一个 --home，这是「跨进程」的载荷）
python3 scripts/pty_capture.py --bin target/debug/pi \
  --steps scripts/pty_scenarios/lum1319-history-session-b.json \
  --out docs/screenshots/lum1319-history-session-b.png \
  --text-out docs/screenshots/lum1319-history-session-b.png.txt \
  --home /tmp/lum1319/home          # ② 同上

# 广告面：启动头 hint + /hotkeys（120x34，因为 76x26 启动头会自己折叠）
python3 scripts/pty_capture.py --bin target/debug/pi \
  --steps scripts/pty_scenarios/lum1319-history-advertise.json \
  --out docs/screenshots/lum1319-history-advertise.png \
  --text-out docs/screenshots/lum1319-history-advertise.png.txt \
  --home /tmp/lum1319/home          # ③ 同上
```

断言（`scripts/pty_capture.py` 的 `expect`，缺一即 FAIL）：

| scenario | 断言 | 关键帧 |
|---|---|---|
| `lum1319-history-session-a` | **4/4 PASS** | 三次 `Enter` 提交，transcript 依次出现 `> first prompt` / `> second prompt` / `> third prompt` |
| `lum1319-history-session-b` | **15/15 PASS** | 面板 1：`Up` 召回 `third prompt`（**本进程从未提交过它**）；面板 3：`Ctrl+R` 显示 `reverse-i-search:` 且**不预览**（codex 的 Idle）；面板 4：输入 `prompt` 后 `> third prompt` + `Enter accept · Esc cancel`；面板 5/6：`Ctrl+R` 走到 `> second prompt` → `> first prompt`，再按保持不动（边界不是 miss）；面板 7：`Esc` 还原空草稿（帧哈希与面板 2 相同 = `cf0a3823a86b`）；面板 8：`Up`+`Enter` 把召回内容真正提交 |
| `lum1319-history-advertise` | **11/11 PASS** | 面板 1：启动头 `Ctrl+R/Ctrl+S to search history`；面板 2：`Alt+H` 折叠后该行消失；面板 4/5：`/hotkeys` 的 navigation 段列出 `Ctrl+R search the prompt history` 与 `Ctrl+S prompt history search match` |

文件层面的实测数字（同一组 run）：

```console
$ wc -l /tmp/lum1319/home/.pi/agent/history.jsonl
3
$ stat -c '%a %s' /tmp/lum1319/home/.pi/agent/history.jsonl
600 121
$ tail -1 /tmp/lum1319/home/.pi/agent/history.jsonl
{"text":"third prompt","ts":1790021636}
$ HOME=/tmp/lum1319/home ./target/debug/pi --clear-history
Cleared prompt history: /tmp/lum1319/home/.pi/agent/history.jsonl
$ ls /tmp/lum1319/home/.pi/agent/
（空：history.jsonl 已删除；目录里其它文件不受影响）
```

上限裁剪（真实二进制，不是单元测试）：先塞 150 行再跑会话 A（3 次提交）：

```console
$ wc -l history.jsonl          # 预置
150
$ wc -l history.jsonl          # 3 次 append 之后
100
$ stat -c '%a %s' history.jsonl
600 3131
$ head -1 history.jsonl
{"text": "seed-53", "ts": 53}    # 最旧的 50 行 + 被挤出的旧行都没了
```

即：**第一次 append 就把文件裁到 100 行**，之后保持 100 行、0600、约 3 KB。

## 3. 实现

### 3.1 `crates/pi-tui/src/history_store.rs`（新增，约 250 行含 8 个单测）

- `HistoryStore { path, limit }`：`load()` 读回**最后 `limit` 行**的 `text`（旧→新），`append(text)`
  追加一行并裁剪，`clear()` 删文件。
- 行格式 `{"ts": <unix 秒>, "text": "..."}`；解析只认「JSON 对象 + 字符串 `text`」，
  其余（非 JSON、缺字段、`text` 不是字符串、空串、超 256 KB 的行）**跳过**。
- 权限：Unix 上创建时 `mode(0o600)`，每次 append 之后再 `set_permissions(0o600)`
  （一个本来世界可读的旧文件会在下一次提交时被收紧）。Windows 无 `mode`，跳过。
- 裁剪走「同目录临时文件 + rename」，避免崩在写一半；同时顺手丢掉损坏行（文件会自愈）。
- 读失败（不存在、目录、权限）→ 返回空 vec，**不报错**：降级成「只有会话内历史」。

### 3.2 `Editor` 的 history 数据模型

```rust
pub struct HistoryEntry {
    text: String,               // 用户可见文本（chip 展开成 `[Image #N]`）
    raw: Option<String>,        // 会话内草稿的原始 buffer（含 CHIP_CHAR 哨兵）
    images: Vec<ImageContent>,  // 与 raw 对齐的附件
}
```

- `history: VecDeque<HistoryEntry>`，index 0 = 最新；`history()` 现在返回**新→旧**
  （旧文档写的是 newest-first 之外的措辞，本轮改成与实际一致）。
- `push_history_entry(text, raw, images)`：空/纯空白丢弃、与队首完全相同的连续重复合并；
  `raw` 缺失或与 `images` 对不上时，用 `rebuild_raw()` 从 `[Image #N]` 标签反推哨兵位置，
  再不行就降级成纯文本条目（`a_raw_buffer_that_disagrees_with_the_images_degrades_to_text`）。
- `history_draft: Option<HistoryEntry>`：进入 history 浏览时保存的草稿**带附件**，所以
  `Up` 之后 `Down` 回到草稿时图片不会丢（原实现是 `Option<String>`）。
- `set_history_store(store)`：读回文件，按「文件旧→新」倒序塞到队尾（队首仍是最新），
  跳过与已有条目同文本的行，重复 attach 幂等。
- 每条 push 都 `store.append(&entry.text)`（**只写文本**），错误忽略：写不进去只是丢掉持久化，
  不会丢用户输入。

### 3.3 `Ctrl+R` 反查（`tui.editor.historySearch` / `historySearchNext`）

`Editor::history_search: Option<HistorySearch { query, matches, selected, draft }>`；
匹配列表在每次 query 变化时**重算**：大小写不敏感 `contains`，且按**完全相同的文本**去重
（`HashSet<&str>`），顺序为「新→旧」。与 codex 的一对一语义：

| 事件 | codex | 本端口 |
|---|---|---|
| 打开 | `begin_history_search`：query 为空、status=Idle，**不预览**、草稿不动 | `begin_history_search`：同上；再按 `Ctrl+R` 等同 Older |
| 输入字符 | `update_history_search_query`：先还原原草稿，再从**最新**匹配重启扫描 | `history_search_push` → `run_history_search`（restart） |
| `Ctrl+R` / `Up` | `history_search_previous`（默认 `ctrl+r`）+ `Up`：Older | `tui.editor.historySearch` + `tui.editor.cursorUp`：Older |
| `Ctrl+S` / `Down` | `history_search_next`（默认 `ctrl+s`）+ `Down`：Newer | `tui.editor.historySearchNext` + `tui.editor.cursorDown`：Newer |
| 边界 | `AtBoundary`：**保持当前预览**，既不报 miss 也不吞掉光标状态 | 同一语义（`step_history_search` 返回 `None`，预览不变） |
| `Backspace` / `Ctrl+H` | 删 query 尾字符并重扫 | `tui.editor.deleteCharBackward` → `history_search_backspace` |
| `Ctrl+U` | 清空 query（回到 Idle + 原草稿） | `tui.editor.deleteToLineStart` → `history_search_clear_query` |
| `Enter` | 只有 `Match` 时接受预览为可编辑草稿，否则什么都不做 | `accept_history_search`：非 Match 返回 `None`（不会变成提交） |
| `Esc` / `Ctrl+C` | `cancel_history_search`：还原进入搜索前的草稿，Ctrl+C 被吞掉（不触发 interrupt） | `cancel_history_search`（还原 text+chips）；`App::step_key_at` 在搜索打开时**先把键交给 composer**，所以 `Esc` 不会去 abort 回合、`Ctrl+C` 不会去清空草稿 |
| 其它键 | `_ =>` 吞掉 | 吞掉（`other_keys_are_swallowed_while_searching`） |
| 无匹配 | `NoMatch`：还原原草稿、搜索框留着继续输入 | 同 |
| 渲染 | footer 一行 `reverse-i-search: <query>` + 状态提示；textarea 预览匹配项 | composer 区域**多一行** `reverse-i-search: <query>  Enter accept · Esc cancel`（`Prompt::history_search_row`），body 预览匹配项；`line_count` 同步 +1，所以区域自己长高，不抢 transcript |

键位表（`pi-tui/src/keybindings.rs`）新增两条 id：`tui.editor.historySearch`（`ctrl+r`）、
`tui.editor.historySearchNext`（`ctrl+s`）；`/hotkeys` 的 navigation 段（coding-agent 的
`hotkeys_text_with`）与启动头 hint（`pi-tui/src/locale.rs` 的 `STARTUP_HINTS`）各加了一条。

### 3.4 两处布局副作用（本轮的真实发现，已修）

1. **启动头多一行 → transcript 少一行 → `/settings` 模态被裁掉最后一行**。
   `App::render_to_buffer_impl` 原来把 settings/dialog overlay 的裁剪边界写成
   `area.y + message_height`（把「消息视口高度」当成了绝对行号），于是**启动头每长一行，
   模态就少一行**：LUM-1310 的 `lum1310-interaction-overview`（120x34）从 16/16 掉到 15/15，
   `Quiet startup` 那一行被裁掉。修法：边界改成 composer 之上的最后一行
   （`area.y + layout.header + layout.above + message_height`），保证「状态栏与输入框永远不被覆盖」
   这条原意不变，同时让模态行数与启动头高度解耦。默认 `AppConfig`（`startup_header: false`）
   下 `layout.header == 0`，`mouse_regions` 那条「矩形裁到消息视口」的契约不变。
2. **启动头净高度必须不变**。新增 hint 行与上面的修法叠加后，120x34 的 `/hotkeys` 尾窗
   会少一行（`lum1298-multi-line-and-model` 的 `page`、`lum1267-interaction-assertions` 的
   `/new …` 与 `open the session tree` 掉出可见窗口）。最终采用两条净额为 0 的改动：
   - 反查两条 chord 合并成**一行** `ChordPair("tui.editor.historySearch", "tui.editor.historySearchNext")`
     （与既有的 `Ctrl+P/Shift+Ctrl+P to cycle models` 同一种写法）；
   - 启动头 hint 列表与 onboarding 之间那行**空行**去掉（上游 `expandedInstructions` 用 `\n\n`
     渲染出的间距）。信息零丢失，启动头净高度与改动前一致。
   证据：全部 45 个既有 PTY scenario 逐条断言与基线二进制**逐字相同**（§4）。

### 3.5 显式清理入口：`pi --clear-history`

`/clear` 有意不删历史（上游行为），而「删文件」也不该是某个按键的副作用，所以清理做成
**CLI 参数**（issue 里允许「入口或参数」）：`pi --clear-history` 删除
`~/.pi/agent/history.jsonl`、打印路径并退出（缺文件也算成功；权限错误 74 = `EX_IOERR`）。
集成测试 `crates/pi-coding-agent/tests/history_file.rs` 用真进程验证「删的就是那个文件、
同目录的 `settings.json` 不动」。

选择参数而不是 slash 命令，也是**行预算**的结果：`/hotkeys`、`/help`、自动补全列表与启动头
都是定高窗口，多一条命令/一行说明就会把老的窗口内容顶出去（本轮实测 `lum1306-help-reload`
在 `/help` 多一行后从 3/3 掉到 2/3）。参数不占任何 TUI 行。

## 4. 门禁与回归

```console
$ . pi-rust/scripts/toolchain.sh && \
  cargo fmt --all -- --check && \
  cargo clippy --workspace --all-targets --locked -- -D warnings && \
  cargo test --workspace --locked
```

| 门 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 通过（无 diff） |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | `No issues found` |
| `cargo test --workspace --locked` | **2622 passed, 0 failed, 2 ignored**（167 suites，67.9s） |
| 新增单测 | `crates/pi-tui/tests/composer_history.rs` **29 passed**；`history_store.rs` 内 8 个；`history_file.rs` 2 个；`startup_header.rs` +1 断言；`slash.rs` +1 单测 |
| 真 PTY | `lum1319-history-session-a` 4/4、`-session-b` 15/15、`-advertise` 11/11 |
| 既有 PTY 回归 | `origin/feature/pi.rs` tip（`231abbc4d`，已含 LUM-1317 的草稿内翻页）与本报改动后二进制各跑 **45 个既有 scenario**，逐条断言文本完全一致（`0` 处差异）；新增/改写的只有 `lum1319-history-*` 三个 |

回归对比方法（可复现）：`git worktree add <base> origin/feature/pi.rs` → 编
`pi-baseline` → 对 `scripts/pty_scenarios/*.json` 逐个跑两个二进制（同一 `--home` 策略、
每个 scenario 之前清空 HOME），把 `PASS/FAIL/XFAIL/XPASS` 行排序后 `diff`。

合并 LUM-1317 时 `prompt.rs` / `app.rs` 有冲突（对方把 `render_lines` 改成
`(&self, width, max_rows, scroll) -> (Vec<String>, usize)`，并新增 `set_page_rows`）：
解决方式是保留对方的滚动窗口参数与 `↑`/`↓` 提示，本轮的搜索行仍占第 0 行、
`line_count` 仍在搜索打开时 +1，草稿窗口在搜索行之下按对方的 `scroll` 语义工作。

## 5. 与 codex `ChatComposerHistory` 的逐条对照

codex 侧引用：`codex-rs/tui/src/bottom_pane/chat_composer_history.rs`（下称 `history.rs`）、
`chat_composer/history_search.rs`（下称 `search.rs`）、`chat_composer.rs`、`keymap.rs`；
本地 checkout `7d99ee8`（只 vendor 了 `codex-rs/tui/`，所以 `codex-message-history` crate 的
字节上限常量不在本对照里，标为「crate 外部」）。

| # | 维度 | codex | 本端口（LUM-1319） | 判定 |
|---|---|---|---|---|
| 1 | 持久文件 | `$CODEX_HOME/history.jsonl`（tui 测试与 startup preflight 都用这个名字）；读写由 `codex-message-history` crate 负责 | `$HOME/.pi/agent/history.jsonl`，由 `pi-tui::history_store` 负责 | 同构，路径随各自 root |
| 2 | 读取时机 | **按需**：`on_entry_response` / `on_batch_response` 拉取，`fetched_history: HashMap<offset, Option<HistoryEntry>>` 缓存；批次有 `MAX_BATCH_READ_RETRIES = 2` | **启动时一次读回**最近 `limit` 行（100），全在内存 | 有意差异（§5.1） |
| 3 | 写入时机 | 每次提交由 core 追加一行；`record_local_submission` 只写会话内元数据 | `Editor::push_history_entry` 里 `store.append(text)`，即每次提交/入队/`!` bash | 同 |
| 4 | 持久条目内容 | **只有文本**（`HistoryEntry::new` 只解 mention；注释明写 persistent history 不存 attachments） | **只有文本**（`append(&entry.text)`；`[Image #N]` 作为字面文本落盘） | 同 |
| 5 | 会话内条目 | 完整草稿元数据：`text_elements` / `local_image_paths` / `remote_image_urls` / `mention_bindings` / `pending_pastes` | `raw`（chip 哨兵 buffer）+ `images: Vec<ImageContent>` | 同构；端口没有 mention/paste 模型，故字段更少 |
| 6 | 条目顺序 | 持久（旧）在前、`local_history` 最新在**末**；offset 0 = 最旧 | `VecDeque` 最新在 index 0；持久条目按倒序 push 到队尾 | 同一空间、镜像索引 |
| 7 | 连续重复 | `record_local_submission_inner`：与上一条 **完全相等**（含附件）才丢 | 与队首 `text` 相同即丢 | 同（端口只比文本） |
| 8 | `Up`/`Down` 召回 | `navigate_up/down` 走合并空间；会话内条目可 rehydrate 元素/图片 | `history_prev/next` 走同一 `VecDeque`；`HistoryEntry.raw+images` 还原 chip | 同 |
| 9 | 草稿保存 | `history_cursor` + 边界处回到草稿 | `history_draft: Option<HistoryEntry>`（**带附件**） | 同 |
| 10 | 反查键 | `composer.history_search_previous = ctrl+r`、`history_search_next = ctrl+s`（`keymap.rs:1684-1685`），另接受 `Up`/`Down` | `tui.editor.historySearch = ctrl+r`、`tui.editor.historySearchNext = ctrl+s`，另接受 `Up`/`Down` | 同 |
| 11 | 反查打开态 | Idle：query 空、不预览、原草稿不动 | 同（`HistorySearchStatus::Idle`） | 同 |
| 12 | 匹配 | `search_matches`：`query.is_empty() \|\| entry.text.to_lowercase().contains(&query_lower)` | `entry.text.to_lowercase().contains(&query.trim().to_lowercase())`（空 query 直接回 Idle） | 同（大小写不敏感） |
| 13 | 去重 | `seen_texts: HashSet<String>`，键是**完全相同的 prompt 文本**，只在一次搜索内有效 | 每次重扫时 `HashSet<&str>` 去重 | 同 |
| 14 | 方向/边界 | Older：restart 从 `total-1` 起；Newer：restart 从 0 起；`AtBoundary` 保持当前、不改隐藏游标 | Older：restart 从 index 0；Newer：`checked_sub`；边界返回 `None` 保持预览 | 同 |
| 15 | query 变化 | `restart=true`，从最新匹配重扫；miss 时还原原草稿并留在搜索态 | 同 | 同 |
| 16 | 接受 | `Enter` 且 status==Match：`history_search = None`、`reset_search()`、光标移到末尾 | `accept_history_search`：仅 Match，关搜索、光标到末尾 | 同 |
| 17 | 取消 | `Esc` / `Ctrl+C`（含 `\u{0003}` 裸字节）：`cancel_history_search` 还原草稿，Ctrl+C 被吞 | `Esc` / `tui.input.copy`（ctrl+c）：`cancel_history_search` 还原 text+chips；`App` 层在搜索打开时先把键给 composer | 同 |
| 18 | 其它键 | `_ =>` 吞掉（不落进草稿） | 同 | 同 |
| 19 | 渲染 | footer 显示 `reverse-i-search: <query>` + `Enter accept · Esc cancel` / `no match`；textarea 预览匹配 | composer 多一行 `reverse-i-search: <query>  Enter accept · Esc cancel` / `no match`；body 预览匹配 | 同文案，位置不同（端口没有独立 footer 行可用） |
| 20 | 异步等待态 | `HistorySearchStatus::Searching` / `HistorySearchResult::Pending`（持久库按需拉取的中间态） | **不存在**（同步读全量，永不等待） | 有意差异（§5.1） |
| 21 | 会话回放 | `record_replayed_submission` / `replay_seeded_history` 把 resume 的 transcript 也喂进 history | 无（端口的 resume 不重放 composer history） | 未对齐 |
| 22 | 上限 | 持久库的字节/行上限由 `codex-message-history` crate 决定（本 checkout 未 vendor，crate 外部） | 行上限 = `HISTORY_LIMIT` = 100 行（文件与内存同一个数） | 口径不同（行 vs crate 内部策略） |
| 23 | 清理入口 | 无专用命令（文件由用户自行删除） | `pi --clear-history`（显式参数，缺文件也成功） | 端口多给了一个 |

### 5.1 三处**有意**的差异

1. **同步读全量 vs 按需分批**。codex 的持久条目要跨进程/异步通道取，因此有
   `Pending`、批次、重试、`fetched_history` 缓存、`persistent_log_id` 过期判定这一整套机器；
   端口的 composer 是同步渲染循环，一次 `read` 100 行 ≪ 一次请求往返，所以直接全量读回，
   `Searching`/`Pending` 状态在本端口不可达（`HistorySearchStatus` 只有 Idle/Match/NoMatch）。
   代价：历史文件很大时启动读回是 O(limit)，以及「不重启看不到别人写的行」。
2. **`PI_HOME` 不参与解析**。端口的历史文件走 `paths::agent_dir_or_default()`
   （= `$HOME/.pi/agent`），与 themes/extensions/keybindings 一致；`PI_HOME` 只影响
   **package 安装器**（`installer.rs`）。PTY harness 里 `PI_HOME` 会被显式 unset，
   `HOME` 才是根。这是需要写明的限制：想让 history 换地方，改 `HOME`（或将来把
   `history_file` 接到 settings）。
3. **启动头广告一行合并两条 chord**。上游 pi-ts 的启动头没有 history search 一行
   （上游 editor 连 reverse search 都没有），端口新增了这一行；为了让启动头净高度不变，
   把 `Ctrl+R`/`Ctrl+S` 合并成 `Ctrl+R/Ctrl+S to search history`，并去掉 hint 与 onboarding
   之间的空行（上游是 `\n\n`）。这是**广告面**的取舍，不是行为取舍。

### 5.2 未对齐 / 已知限制

- **slash 命令不进 history**（内存与文件都不进）：`handle_submitted` 在 `/…` 分支短路，
  只有普通 prompt、入队/steer 文本与 `!cmd` 行会 `push_history_entry`。这是端口既有行为，
  本轮没有改（要改应单独一轮，因为它同时影响 `Up` 召回语义）。
- **`@` mention / paste 折叠标记不参与 history**：端口没有 mention/paste 模型
  （LUM-1318 在做 paste 折叠），所以 `HistoryEntry` 只有 `raw + images`。
- **图片只在同一进程内召回**：跨进程召回带图 prompt 时，`[Image #N]` 是字面文本、没有附件
  ——与 codex 的持久库一致（这是**有意**的行为，不是缺陷）。
- **文件上限按行**，codex 的口径由 `codex-message-history` crate 决定（可能是字节），
  两边不逐字等价。
- **清理只删文件**，不触碰任何 session 存储；`/clear` 仍然不删历史文件。

## 6. 改动清单

| 文件 | 内容 |
|---|---|
| `crates/pi-tui/src/history_store.rs` | 新增：`HistoryStore`（load/append/trim/clear、0600、损坏行降级）+ 8 单测 |
| `crates/pi-tui/src/editor.rs` | `HistoryEntry`；history 改存条目并带 `raw`/`images`；`set_history_store`；`push_history_entry`；`rebuild_raw`；`history_search` 全家（begin/push/backspace/clear/step/cancel/accept/run）+ 键分派；`clear()` 关闭搜索 |
| `crates/pi-tui/src/prompt.rs` | `push_history_entry` / `set_history_store` / `clear_persisted_history` 透传；`history_search_row`；`line_count`/`render_lines` 为搜索行留一行 |
| `crates/pi-tui/src/app.rs` | `AppConfig::history_file`；`App::new` 挂 store；`Submission::raw_text`（提交时捕获原始 buffer）；`submit`/`follow_up_from_editor` 走 `push_history_entry`；搜索打开时把键交给 composer（`step_prompt`）；overlay 裁剪边界改到 composer 之上；`RenderSnapshot` 暴露 `history_search_*` |
| `crates/pi-tui/src/keybindings.rs` | 新增 `tui.editor.historySearch`(ctrl+r) / `tui.editor.historySearchNext`(ctrl+s) |
| `crates/pi-tui/src/locale.rs` | 启动头 hint：`ChordPair(historySearch, historySearchNext)` → `Ctrl+R/Ctrl+S to search history` |
| `crates/pi-tui/src/lib.rs` | 导出 `history_store` 与 `HistoryEntry`/`HistorySearchDirection`/`HistorySearchStatus` |
| `crates/pi-coding-agent/src/{paths,interactive,cli,main}.rs` | `history_file_path()`；`AppConfig.history_file` 接线；`--clear-history` 参数 + `clear_prompt_history_cli()` |
| `crates/pi-coding-agent/src/commands/slash.rs` | `/hotkeys` 增两条 navigation 行（+1 单测） |
| 测试 | `pi-tui/tests/composer_history.rs`（29 个）、`pi-tui/src/history_store.rs` 内 8 个、`pi-coding-agent/tests/history_file.rs`（2 个）、`startup_header.rs`/`composer_images.rs` 补断言 |
| 场景/截图 | `scripts/pty_scenarios/lum1319-history-{session-a,session-b,advertise}.json`；`docs/screenshots/lum1319-history-*.png(.txt)` |

完成度：issue 的 4 项要做的事与 4 条验收全部落地（持久化 / 附件召回 / `Ctrl+R` / 隐私与体积+显式清理、
门禁与真 PTY 证据、审计文档）。**未做且属于后续轮次**的：slash 命令进 history、`@` mention 与 paste
标记随草稿召回、按需（不重启）读到别进程新写的行。
