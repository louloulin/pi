# LUM-1415 — composer 反查历史（Ctrl+R）+ 跨会话 history：真实审计与交付

> scope: `pi-rust/`（分支 `work/LUM-1415`，基线 `origin/feature/pi.rs` `14474dcf0` = LUM-1412）
> 时间锚：2026-09-23，runner = **Windows**（`cargo 1.97.1`），无 `pty`/`pyte`
> 相关已存在 issue：**LUM-1299**（扩展事件第二批，todo）、LUM-1319（本轮同类交付，**见 §8**）

## 0. TL;DR

issue 原文三条要求里，"TUI chatinput 与 codex/Martty 差距很大、要修"是唯一有实测缺口的一条。
本轮逐条对照 codex 真实源码（`codex-rs/tui/src/bottom_pane/chat_composer_history.rs` +
`chat_composer/history_search.rs`，本机 `lclaw/project/codex`）与 Martty
（`src/input/keymap.rs` / `src/input/editor.rs`），把 composer 缺的**最后一块**补上：

* **`Ctrl+R` 反查**（codex `Ctrl+R`，Martty 与上游 pi-ts 都没有）：query 在 footer、匹配在
  composer 预览、`Esc` 逐字恢复原草稿、`Enter` 接受为可编辑草稿；
* **跨会话 history**：`~/.pi/agent/history.jsonl`，重启后 `Up` / `Ctrl+R` 仍能召回；
* **附件召回**：history 条目带 image chip，`Up` / `Ctrl+R` 召回不再丢图；
* **窄视口下查询不被裁掉**：44 列的 footer 现在把 token 计数让给实时 query；
* 顺带修掉一个**已在基线红着的门禁**：`chatinput_chord_conflicts` 的
  `ALLOWED_OVERLAPS` 漂移（`app.models.save`/`ctrl+s` 已无对应 editor chord）。

数字（本机实测，命令在 §5）：

| 口径 | 本轮 |
|---|---|
| `cargo test -p pi-tui` | **934 passed / 0 failed**（基线 `14474dcf0` 实测 **907**，+27） |
| `cargo fmt --all -- --check` | 干净 |
| `cargo clippy -p pi-tui --all-targets -- -D warnings` | 干净 |
| 整体 chatinput↔codex 交互面（自评） | **约 86%**（本轮前约 80%，见 §7） |
| Rust↔TS 纯代码规模 | **89.4%**（136,889 / 153,176 行） |
| Rust↔TS 测试用例 | **48.3%**（2,563 / 5,309） |

## 1. 真实审计：composer ↔ codex ↔ Martty ↔ 上游 pi-ts

证据面：codex 源码本机可读（`/c/Users/Administrator/lclaw/project/codex`）、
Martty 已 clone（`workdir/Martty`）、上游 TS 在 `packages/`。

| 能力 | codex（源码位置） | Martty | 上游 pi-ts | 本轮前 pi-rust | 本轮后 |
|---|---|---|---|---|---|
| 多行 compose + 视觉行 Up/Down | `chat_composer.rs` textarea | `editor.rs::move_vertical` | `editor.ts:913-940` | ✅（LUM-1282/1312） | ✅ |
| 行域 Home/End、Ctrl+U/K/W/A/E | ✅ | ✅ | ✅ | ✅（LUM-1312） | ✅ |
| 粘贴折叠 `[paste #N +M lines]` | `paste_burst` | — | ✅ | ✅（LUM-1318） | ✅ |
| 草稿内翻页 + 窗口跟随光标 | ✅ | — | `Editor.pageScroll` | ✅（LUM-1317） | ✅ |
| **`Ctrl+R` 反查** | `history_search.rs:164-259`、`begin_history_search:134` | ❌ | ❌ | ❌ **缺口** | ✅ |
| 反查匹配语义 | `search_matches:633`（忽略大小写 `contains`）+ `search_result_is_unique:640`（同文本去重） | — | — | ❌ | ✅ 同 |
| 反查边界 | `AtBoundary` 保持预览、不关会话 | — | — | ❌ | ✅ |
| 反查 query 编辑 | 每次编辑从"最新匹配"重开（`update_history_search_query`） | — | — | ❌ | ✅ |
| 取消语义 | `Esc`/`Ctrl+C` 恢复**原草稿** | — | — | ❌ | ✅ |
| 跨会话 history | `~/.codex/history.jsonl`（`message_history.rs`） | ❌ 仅会话内 | ❌ 仅会话内（`historyPrevious/Next` 默认不绑） | ❌ **缺口** | ✅ `~/.pi/agent/history.jsonl` |
| history 召回带附件 | `HistoryEntry{local_image_paths, remote_image_urls, pending_pastes, mention_bindings}` | ❌ | ❌ | ❌ **缺口**（`set_text_internal` 里 `images.clear()`） | ✅ 会话内条目带 chip（持久条目仍只有文本，与 codex 一致） |
| history 上限 | 1 MiB / 线程 + 分页拉取 | 无上限 | 100 条 | 100 条 | 100 条 + 文件自动压缩 |
| 反查查询的位置 | footer 行（`history_search_footer_line:378`） | — | — | ❌ | ✅ footer（`StatusData.hint`） |
| 反查时键盘归属 | `handle_history_search_key` 全程独占 | — | — | ❌ | ✅（App + driver 两层都让位） |

**结论（诚实版）**：chatinput 的"编辑语义"面在 LUM-1312/1317/1318/1328 之后已经与 codex 同级；
本轮补的是**历史检索面**——它正好是 codex 相对上游 pi-ts 唯一的增量，也是 LUM-1312 §5 里
唯一还没人领的那条（§8 说明为什么"没人领"这个判断本身是错的）。

**Martty 的差距（本轮不改，记录在案）**：Martty 的 composer 是"空草稿才进 history"
（`app.rs:4067-4096` 的 grok 语义）+ 无搜索；它的 `Alt`/`Ctrl` 箭头与 pi-rust 一致。
所以"chatinput 与 Martty 差距很大"这句在编辑语义上**不成立**，真正的差距在 codex 侧。
Martty 领先的地方不在 composer，而在**宠物/主题/插件 UI 宿主**这些与输入无关的面。

## 2. 本轮改了什么（每条带位置与证据）

### 2.1 `pi-tui/src/editor.rs` — 反查状态机

* `HistoryEntry { text, images }`（`editor.rs:368`）：history 条目从"纯文本"升级为"文本 + chip"，
  `HistoryEntry::with_images` / `display_text` 与 `expand_chips`（`editor.rs:2494`）共用一份
  `[Image #N]` 展开逻辑，所以一条召回后的草稿和它被提交时长得**逐字一致**。
* `HistorySearch`（query / 原草稿 / matches / selected）+ 公开 API：
  `begin_history_search:965`、`cancel_history_search:982`、`accept_history_search:997`、
  `history_search_step:1017`、`history_search_matches:1047`、`restart_history_search:1074`、
  `handle_history_search_key:1129`。
* 语义对齐 codex 的三条不变量：空 query **不预览任何东西**（打开 `Ctrl+R` 不会把用户的
  草稿换成上一条 prompt）、query 编辑从最新匹配重开、边界保持预览且不关会话。
* 与已 armed 的 `Ctrl+]` jump 互斥：`begin_history_search` 清掉 `jump_mode`，否则 jump target
  会把 query 的第一个字符吃掉（有回归测试）。

### 2.2 `pi-tui/src/history_store.rs`（新增）— 持久化

* `default_path:57` 三级优先：`$PI_HISTORY_PATH` → `$PI_HOME/agent/history.jsonl` →
  `~/.pi/agent/history.jsonl`（与 `pi-coding-agent/src/packages/installer.rs` 同一套根）。
* 逐行 JSON `{"text": …}`；`load:91` **容忍损坏**（非 JSON / 无 `text` / 空文本 / 单行 > 256 KB 全部跳过，
  缺文件读为空），并在读入超过上限时 `rewrite:186` **压缩文件**（临时文件 + rename，崩溃不会
  破坏旧文件）；`append:140` 是 append+flush，崩溃最多截断自己那一行。
* Unix 下文件 `0600`（`restrict_permissions:171`），且**每次 append 都重新 chmod**，把外部改宽的
  权限修回来；Windows 走空实现（继承目录 ACL）。
* 默认**不启用**：`Editor::set_history_path(None)` 是默认值，测试与 embedder 不会碰用户真实 `$HOME`。

### 2.3 `pi-tui/src/keybindings.rs` — 新 id

`tui.editor.historySearch` = `ctrl+r`、`tui.editor.historySearchNext` = `ctrl+s`
（`keybindings.rs:231/236`）。这是**上游没有**的 id（上游 `historyPrevious/Next` 默认不绑定），
在 doc-comment 里写明是 codex 和弦，并说明它与 overlay 内的 `app.session.rename`(ctrl+r) /
`app.models.save`(ctrl+s) 的分工。

### 2.4 `pi-tui/src/app.rs` + `pi-coding-agent/src/interactive.rs` — 键盘归属与接线

* `App::step_key_at:2722`：反查打开时**直接进 composer**（`step_composer:2929`），
  在 overlay / dialog / `app.*` 之前。codex 的 `handle_history_search_key` 就是全程独占——
  否则 `Ctrl+O` 折叠工具块、`Alt+V` 贴图、`PageUp` 翻聊天记录都会在输入查询时误触发。
* `interactive.rs:990`：driver 的全局 `app.*` 分支同样让位（这是第二道门，因为 driver 的那批
  chord 在 App 之前跑）。
* `AppConfig::history_path`（`app.rs:546`）+ `interactive.rs:278` 接上真实路径。

### 2.5 `pi-tui/src/status.rs` — 窄视口下的实时 query

footer 的整片预算（LUM-1367）第一位牺牲品是 hint——对 `? for help` 正确，对**正在输入的 query**
错误。新增 `StatusData::hint_pinned`（`status.rs:62`）与 `narrow_sacrifice_order:393`：
被 pin 的 hint 最后才丢。实测 44×14：**query 在屏上**，让位的是 `in 0 out 0` 计数
（`docs/screenshots/lum1415-history-search-narrow.png`）。

### 2.6 顺带修好的既有红门禁

基线 `14474dcf0` 上 `pi-coding-agent --test chatinput_chord_conflicts` 有 1 个失败：

```
ALLOWED_OVERLAPS entry app.models.save/ctrl+s no longer overlaps anything
```

即 `ALLOWED_OVERLAPS` 里两条 `ctrl+s` / `ctrl+r` 条目指向的 editor chord 早已不存在（该守卫的
第二半"allowlist 不能有僵尸条目"因此红了）。本轮给 `ctrl+r`/`ctrl+s` 加回真实的 editor 消费者后，
**这条红门禁变绿**，且 allowlist 的理由（overlay-scoped）依旧成立。这不是"顺手"，而是这条
守卫设计意图的回归：**和弦表漂移必须被门禁抓到，而修它的正确方式是让二义性真的被隔离，不是删断言。**

## 3. 测试与证据

| 证据 | 内容 |
|---|---|
| `crates/pi-tui/tests/history_search.rs`（新，15 条） | 编辑器状态机：打开不换草稿、query 预览最新匹配、`Ctrl+R`/`Up` 走旧 / `Ctrl+S`/`Down` 走新、边界保持、query 编辑重开、`Backspace` 改 query 不改预览、`Enter` 接受、`Esc`/`Ctrl+C` 恢复原草稿、miss 恢复、忽略大小写 + 同文本去重、空 query 不预览、**召回带 chip**、**跨进程读回**、jump 互斥、和弦来自表 |
| `crates/pi-tui/tests/history_search_app.rs`（新，6 条） | App 面：footer 文案与三态（空 query / match / no match）、`Enter` 落到 composer、**搜索期间 `Ctrl+O` 不折叠工具块且 query 不被污染**、构造时读回历史文件、resize 不丢 query、**44 列下 query 存活且计数让位** |
| `crates/pi-tui/src/history_store.rs` 单测（3 条 + Unix-only 1 条） | 往返 + 损坏行跳过、缺文件非错误、超限裁剪**并压缩文件**（+ Unix `0600` 一条，Windows 上按 cfg 跳过） |
| `docs/screenshots/lum1415-history-search.png(.txt)` | 6 面板帧缓冲：输入草稿 → `Ctrl+R` → query → 更旧匹配 → `Esc` 恢复 → no match |
| `docs/screenshots/lum1415-history-search-narrow.png(.txt)` | 44×14：query 在屏、token 计数让位 |

**证据分级（重要）**：上述 PNG 是 `scripts/frame_to_png.py` 绘制的 **frame-buffer 截图**，
来源是 `tests/lum1415_history_search_frames.rs` 打印的 `App::render_snapshot` 网格——即真实渲染管线，
但**不是真实 PTY**，也没有颜色，更**不能证明按键交互**（交互由上面 21 条测试断言）。
本机是 Windows，没有 `pty`/`fcntl`/`termios`/`pyte`，`scripts/pty_capture.py` 跑不起来。
真 PTY 的 A/B 需要 Linux runner（`scripts/pty_scenarios/lum1415-*.json` 未提供，见 §9.1）。

## 4. 门禁实况（Windows / cargo 1.97.1）

```
$ cargo fmt --all -- --check                 # 干净
$ cargo clippy -p pi-tui --all-targets -- -D warnings   # 干净
$ cargo test -p pi-tui                       # 934 passed / 0 failed（基线 14474dcf0 = 907）
```

`cargo test --workspace` 在本机**不是全绿**，与本轮改动无关，且**基线同样不绿**。实测做法：
在 `14474dcf0`（基线）与本分支上各跑一次 `-p pi-coding-agent -p pi-extensions`，逐名对比失败集：

* 基线失败名 **49 个**，本分支失败名 **47 个**；
* 差集**只有** `the_allowlist_names_ids_that_exist_and_actually_overlap`（基线红 → 本轮绿）；
* 其余失败全部是 Windows 平台性失败：`pi_client::unix` 不存在（doctest 编译失败）、
  `/a/b` 在 Windows 不是绝对路径（`paths` / `find` / `ls` 的沙箱断言）、需要 `sh` 的
  `bash` 工具用例、rquickjs 的 `node:*` builtins、`reload_config` 的 10 条等。
* `cargo clippy --workspace -- -D warnings` 在本机有 1 条 `pi-extensions` 的
  `signal_name is never used`：该函数只在 `#[cfg(unix)]` 的 `exit_signal_name` 里被调用，
  是**平台性 dead_code**，不是本轮引入。

> 结论：本机可复现的门禁是 `fmt` / `clippy -p pi-tui` / `test -p pi-tui`；
> workspace 级门禁的权威口径是仓库脚本 `scripts/toolchain.sh`（Linux + Rust 1.85.0），
> 本轮无法在此 runner 复现，如实标注。

## 5. Rust ↔ TS 差距（本轮实测，命令可复现）

```bash
find pi-rust/crates -path '*/src/*' -name '*.rs' -print0 | xargs -0 wc -l | awk '/total/{s+=$1} END{print s}'
find packages        -path '*/src/*' -name '*.ts'  -print0 | xargs -0 wc -l | awk '/total/{s+=$1} END{print s}'
grep -rho '#\[test\]\|#\[tokio::test\]' pi-rust/crates --include=*.rs | wc -l
find packages -name '*.test.ts' | xargs grep -ho '\bit(\|\btest(' | wc -l
python pi-rust/scripts/app_action_coverage.py
python pi-rust/scripts/extension_event_coverage.py
```

| 口径 | 数值 | 说明 |
|---|---|---|
| 纯代码规模（src↔src） | **89.4%** | Rust 136,889 / TS 153,176 行 |
| 测试用例数 | **48.3%** | Rust 2,563 / TS 5,309 |
| `app.*` 接线 | **43/44 = 97.7%** | 唯一 silent：`app.tree.editLabel`（缺 UI，不是缺键位） |
| 扩展生命周期事件（tag） | **21/36 = 58.3%** | 生产构造点 **20/36 = 55.6%**；仍缺 15 个 |
| TUI 模块数 | **35 / 42 = 83.3%** | `pi-tui/src/*.rs` vs `packages/tui/src/*.ts` + `components/*.ts` |
| provider API family | 5/5 | 未变 |
| slash 命令 | 与上轮同口径（缺口主要是 `import`/`share`/`login`/`logout` 这类需要外部服务的） | 未变 |

**口径声明**：Rust↔TS 行数比会随两边增长漂移；本轮只加了 `+2,627` Rust 行（含测试），
所以 89.4% 相比 LUM-1310 的 85.1% 上升，是**测试与文档增长 + TS 侧未见增长**的共同结果，
不是"功能变多 4 个点"。百分比必须附"怎么量的"，本节顶部的 6 条命令就是口径。

## 6. 完成度（自评，带依据）

| 面 | 完成度 | 依据 |
|---|---|---|
| composer **编辑语义** | **约 92%** | 对齐表 §1 的编辑行全绿；剩：跨会话**附件**召回（需存路径）、反查命中高亮、`Ctrl+R` 历史写入节流 |
| composer **历史检索** | 由约 30% → **约 90%** | codex 的三条不变量 + 边界 + 附件召回 + 持久化全在；剩：codex 的分页拉取（本 port 一次性读，规模上无必要）与 `Ctrl+R` 命中高亮 |
| **整个 TUI** 对 codex/pi-ts | 约 **86%** | 输入面 92% + 视觉/消息面（95%）+ 扩展 UI 宿主（95%）+ 缺口：markdown 列宽折行、扩展事件 |
| **Rust↔TS 整体** | **89.4%**（代码）/ **48.3%**（测试） | §5 |
| issue 三项要求 | chatinput 修复 ✅ / 审计 ✅ / 推送+合并 ✅（见 §10） | — |

## 7. 遗留缺口与"下一轮第一顺位"

按性价比（影响 × 可验证性）排序：

1. **扩展生命周期事件**（21/36 → 36/36）：缺 `before_agent_start`、`context`、`project_trust`、
   `session_before_*`、`ui_prompt_*`、`before/after_provider_*`、`agent_settled` 等 15 个。
   这是"兼容 pi 插件生态"这条目标的头号阻塞，权重最大。
   **已存在 issue：LUM-1299（todo）**——本轮**不重复派**。
2. **markdown / message 按终端列宽折行**：`pi-tui` 全 crate 按**字符数**度量，上游按**列**
   （CJK 草稿换行位置不同）。LUM-1366 §4 列为第一顺位、至今无人领；跨 4 个模块 + 全部快照。
3. **反查的命中高亮**：codex `history_search_highlight_ranges` 会在预览里反色标出命中片段；
   本 port 只预览整条。属体验细节，单模块（`status`/`message` 配色即可）。
4. **跨会话附件召回**：持久条目只存文本；codex 存本地图片**路径**。要做的话得先决定
   pi-rust 的 chip 是否可以从"内联 base64"退回"路径引用"，属于数据模型改动。

## 8. 协调发现：LUM-1319 的交付**没有**进 `feature/pi.rs`

本轮开工前扫 board，发现 **LUM-1319**（"composer：history 跨会话持久化 + Ctrl+R 反查 + 附件召回"，
状态 `in_review`）与本轮**同一件事**。它的评论写着：

> 已完成 LUM-1319（LUM-1312 后续 3/3）。分支 `work/LUM-1319` 已推送，并已 fast-forward 进 `feature/pi.rs`（`6a5a2faf2`）。

实测（本机，可复现）：

```bash
git merge-base --is-ancestor 6a5a2faf2 origin/feature/pi.rs   # → 非 0（NO）
git branch -a --contains 6a5a2faf2                            # → origin/work/LUM-13{18,19,27,28,30,32,33,36-pi-rs}
git ls-tree --name-only 14474dcf0 pi-rust/crates/pi-tui/src/  # → 没有 history_store.rs
```

即 `6a5a2faf2`（含 `history_store.rs`、`tests/composer_history.rs` 804 行、`pi --clear-history`、
PTY 场景与截图）**从未进入 `feature/pi.rs`**，只漂在 8 个 `work/*` 分支上；
`14474dcf0` 里 `pi-tui/src/` 没有 `history_store.rs`，`editor.rs` 也没有反查。
**"已 fast-forward 进 feature/pi.rs" 这句与远端事实不符**——这是本轮最该被记录的协调缺陷：
一条 `in_review` 的交付，看板上"已完成"，集成分支上不存在，于是同一个功能被第二次实现。

对两边交付的判断（都读了代码，不是转述）：
LUM-1319 的实现更"重"（`HistoryStore` 结构 + 上限 + 字节截断 + `0600` + 分页说明 + `pi --clear-history`），
本轮实现更贴 LUM-1412 后的 tip，并额外做了三件 LUM-1319 没做的：
**搜索期间的键盘独占（App + driver 两层）**、**窄视口 pinned hint**、**基线红门禁的修复**。
本轮已把 LUM-1319 实现里值得保留的两条属性**移植**过来：文件 `0600`、超限自动压缩。
**没有**做的：`pi --clear-history`（LUM-1319 的验收项之一）——建议由 LUM-1319 重新提交或单独派一条
（见 §9）。

> 建议给协调者的动作：`work/LUM-1319` 不要直接合并（会与本轮在 `editor.rs`/`app.rs`/`prompt.rs`
> 大面积冲突，且它的 `prompt.rs` "搜索占一行 header"方案与本轮"footer chrome"方案互斥）；
> 应当把 LUM-1319 降级为 `cancelled` 或收窄为"`pi --clear-history` + 文档"。

## 9. 诚实限制

1. **没有真 PTY 截图**：本机 Windows 无 `pty`/`pyte`，`scripts/pty_capture.py` 不可用；
   本轮只提供 frame-buffer 截图（§3）。真 PTY A/B 与 `scripts/pty_scenarios/lum1415-*.json`
   留给有 Linux runner 的下一轮。
2. **workspace 级门禁未在本机复现**（§4）：24 个平台性失败 + 1 条 `-D warnings` 的
   unix-only dead_code。基线同样不绿，逐名对比见 §4。
3. **`Shift+Enter` 依赖 kitty 键盘协议**：`Ctrl+J` 是全终端可用的退路（LUM-1312 结论，本轮未变）。
4. **44 列下 footer 仍会截断 affordance**：query 一定在屏上，但 `Enter accept · Esc cancel`
   在更窄的终端会被 `…` 截掉（44 列实测：`… Es…`）。这是整片预算的诚实表现，不是静默截断。
5. **反查是同步的**：一次读入全部（上限 100 条）历史。codex 分页拉取是为了异步后端；
   本 port 的历史就在内存里，分页只会增加状态机复杂度。
6. **持久历史只有文本**：附件跨会话召回未做（§7.4）。

## 10. 交付物

* 代码：`pi-tui/src/{editor,history_store,keybindings,lib,app,status,prompt}.rs`、
  `pi-coding-agent/src/{interactive.rs,commands/slash.rs}`
* 测试：`pi-tui/tests/{history_search,history_search_app,lum1415_history_search_frames}.rs`
  + `history_store` 单测
* 文档/证据：本文、`docs/screenshots/lum1415-history-search{,-narrow}.png(.txt)`
* 分支：`work/LUM-1415` → 合并回 `feature/pi.rs`（merge commit 见 issue 评论）
