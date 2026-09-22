# LUM-1328 — Composer paste: bracketed paste, paste markers, atomic delete

> scope: `pi-rust/crates/pi-tui/src/{editor,input,app,prompt}.rs`,
> `pi-rust/crates/pi-coding-agent/src/interactive.rs`,
> `pi-rust/crates/pi-tui/tests/composer_paste.rs`,
> `pi-rust/scripts/pty_scenarios/lum1328-paste.json`,
> `pi-rust/docs/screenshots/lum1328-paste{,-baseline}.png`
> base: `origin/feature/pi.rs` = `6a5a2faf2` → merged back into `feature/pi.rs`

> **历史快照 / 后续更正（LUM-1460，2026-09-23）**：本文写于 LUM-1328 的 `work/LUM-1328`
> 分支，当时记录的"已合回 `feature/pi.rs`"**不成立**——`6631b8c77` 从未进入
> `feature/pi.rs` 谱系，这条通道在主干上一直是 0（`git grep -c 'paste #' origin/feature/pi.rs -- pi-rust/crates` = 0）。
> LUM-1460 已把它（连同 LUM-1318 的删除重编号）救回并适配当前 tip；
> **当前数字、偏差清单与截图以 `docs/LUM1460_PASTE_RESCUE.md` 为准**，
> 本文保留为那条分支的原始交付记录。本文 §6 记为偏差的"删除后不重编号"已由
> `9c20bd8f1` 关闭。

一句话结论：TUI 的输入区**根本没有粘贴通道**。驱动从未请求 bracketed paste
（上游 `packages/tui/src/terminal.ts:184` 请求了），`CtEvent::Paste` 也没有任何消费者 ——
终端一旦按新模式发来 `ESC [ 2 0 0 ~ … ESC [ 2 0 1 ~`，整段内容被 `InputEvent::Ignored`
吞掉，回车提交的是一条**空 prompt**（真 PTY 实测，见 §3 面板 5/9）。
本轮把这条通道补齐，并顺带把"大段粘贴"做成上游那套 marker 模型。

---

## 1. 真实审计：三方 chatinput 对照（逐条读源码，不是设计意图）

| 轴 | 上游 pi-ts (`packages/tui`) | codex (`codex-rs/tui`) | Martty (`src/`) | 本端口（改动前） | 本端口（本轮后） |
|---|---|---|---|---|---|
| 请求 bracketed paste | ✅ `terminal.ts:184` `\x1b[?2004h` | ✅ crossterm 事件流 | ✅ `main.rs:488/534` `EnableBracketedPaste`/`Disable…` | ❌ `setup_terminal` 只开 `EnterAlternateScreen` + `EnableMouseCapture` | ✅ `setup_terminal` / `resume_tui` 开，`suspend_tui` 关 |
| 粘贴事件入口 | `stdin-buffer` 收 `200~/201~` → `paste` 事件 → 重新包成 marker 喂 `editor.handleInput` | `ChatComposer::handle_paste`（显式事件）+ `paste_burst`（终端不发事件时按时间窗把突发字符判为粘贴） | `Event::Paste(text)` → `app.rs:2258` 插入 composer | ❌ `translate_event` 的 `_ => InputEvent::Ignored` | ✅ 驱动识别 `CtEvent::Paste` → `App::step_paste`（`InputEvent` 是 `Copy` 且载荷是 owned，无法做变体，理由写在 `app.rs::step_paste` 的文档注释） |
| 大段粘贴折叠 | >10 行 或 >1000 字符 → `[paste #N +L lines]` / `[paste #N C chars]`，内容进 `pastes` 表（`editor.ts:1248-1330`） | >1000 字符 → `[Pasted Content N chars]`（`chat_composer.rs:431,1920`），同名再来一个加 `#2`；`pending_pastes` 在提交时展开 | ❌ 直接插入全文（无折叠） | ❌ | ✅ 与上游同规则同格式：`[paste #N +L lines]` / `[paste #N C chars]` + 注册表 |
| 提交时还原正文 | `expandPasteMarkers`（`editor.ts:1077`） | `pending_pastes` 展开后才交给模型 | 全文本来就在 buffer 里 | — | ✅ `Editor::expanded_text()`：`Submit` 与外部编辑器（上游 `getExpandedText`）都走它 |
| marker 的编辑原子性 | `segmentWithMarkers` + `isAtomicSegment`：光标、退格、**按词移动**都把 `[paste #1 +12 lines]` 当一个单元；删掉后还会把后面的 id 递减重排（`editor.ts:1383-1410`） | placeholder 是 buffer 里的一个元素（`TextElement`），删除按元素走 | 无 | — | ✅ 退格/前删删整个 marker（连同注册表内容），`←`/`→` 一步跨过；历史回放还原 marker；⚠️ 见 §6 两条偏差 |
| 行尾/控制字节 | `normalizeText`：`\r\n`/`\r`→`\n`、tab→4 空格；过滤除 `\n` 外不可打印字符；粘贴内容若以 `/` `~` `.` 开头且光标前是词字符，补一个空格；解码 CSI-u 的 `ESC [<cp>;5u`（tmux popup 会把 `\n` 编码成这样） | 有 `normalize_pasted_path`、`pasted_image_format`、图片路径分支 | `normalize.rs` 明确"不碰 paste"，另有 CSI-u 用例 | ❌ | ✅ 全部照搬（`decode_csi_u_control` / `normalize_pasted_text` / `needs_path_separator`），单测逐条钉住 |
| 粘贴与自动补全 | `handlePaste` 先 `cancelAutocomplete`，粘贴过程不触发补全 | 同 | — | — | ✅ `insert_paste` 先取消补全、不刷新候选 |
| 一次粘贴 = 一次撤销 | `pushUndoSnapshot()`（快照含 `pastes` 表） | 元素级 | — | — | ✅ 快照带上 `pastes` + counter，`Ctrl+-` 一步还原 marker 与内容 |
| 宽度口径 | `Intl.Segmenter` + 终端宽度 | `unicode-width` | `unicode-width`（`input/editor.rs:6`） | 1 字符 = 1 列（crate 约定，CJK 偏窄） | 未改（见 §6） |

**结论**：`chatinput` 这块，本端口与上游 pi-ts 在**编辑语义**（LUM-1312）之后已经对齐，
但缺的是**通道与折叠**这一整层；codex / Martty 也都开了这条通道，所以这不是"锦上添花"，
而是"能不能粘贴"的基础能力。

## 2. 实现

### 2.1 `pi-coding-agent/src/interactive.rs` —— 打开/关闭模式
* `setup_terminal`：`EnterAlternateScreen + EnableMouseCapture + EnableBracketedPaste`；
  `suspend_tui`：`DisableBracketedPaste`（子进程拿到干净 tty）；`resume_tui` 再开回来。
* 事件循环：`CtEvent::Paste(text)` → `app.step_paste(text)`，其余仍走
  `App::translate_event`。`translate_event` 把 `Paste` 映射为 `Ignored` 是有意的：
  它保证同一段字节永远不会被当成按键重放（改动前的行为就是"按键重放 / 全丢"两种坏法）。
* `run_external_editor` 改用 `App::expanded_editor_text()`（上游
  `interactive-mode.ts:4247` 用的就是 `getExpandedText()`），外部编辑器看到的是正文而不是 marker。

### 2.2 `pi-tui/src/editor.rs` —— marker 模型（上游 `handlePaste` 的移植）
* `Editor::insert_paste(&str) -> PasteInsertOutcome`（`Ignored` / `Inserted` / `Marker(id)`）：
  CSI-u 解码 → `normalizeText` → 过滤控制字符 → 路径分隔 → 阈值判断 →
  折叠成 marker + 注册表，或整段插入。始终只压一个撤销快照，且不打开补全。
* `Editor::expanded_text()`：`display_text()` 里所有"注册表认识的" marker 换成正文
  （不认识的 id 保持字面，和上游 `expandPasteMarkers` 同宽容度）。
* `Editor::backspace` / `delete`：命中完整 marker 时整体删除并清掉注册表内容。
* `Editor::move_left` / `move_right`：一步跨过整个 marker。
* `push_history_entry` / `restore_history_entry`：raw 缓冲里的 marker 若都还有内容，
  历史条目就保留 raw —— 回放时是那个紧凑草稿，而不是被摊平的 12 行（上游历史存的也是
  `getText()` 的 marker 形态）。
* `Editor::clear()` 有意**不**清注册表：提交后从历史里回捞 marker 时还要能展开。

### 2.3 `pi-tui/src/input.rs` / `app.rs` / `prompt.rs`
* `InputEvent` 保持 `Copy`（`App::step` 按值匹配后还要继续用同一个 event），
  所以粘贴不做变体，而是 `App::step_paste(&str)`：模态层（dialog / settings / selector /
  extension custom）开着时**丢弃**粘贴，不偷偷改下面冻结的草稿。
* `App::paste_text`（`Alt+V` 无图片时的文本回退）改走同一条路：上游
  `interactive-mode.ts:2445,2927` 就是把剪贴板文本包成 bracketed paste 再喂编辑器。
* `Prompt::expanded_text()` / `App::expanded_editor_text()` / `App::paste_marker_count()`。

## 3. A/B 证据（真 PTY，同一份场景跑两个真二进制）

```console
$ python3 scripts/pty_capture.py --bin <fixed>    --steps scripts/pty_scenarios/lum1328-paste.json \
      --out docs/screenshots/lum1328-paste.png
assertions: 17 checks over 10 panels — 17 PASS, 0 FAIL, 0 XFAIL, 0 XPASS      (exit 0)

$ python3 scripts/pty_capture.py --bin <6a5a2faf2 baseline> --steps scripts/pty_scenarios/lum1328-paste.json \
      --out docs/screenshots/lum1328-paste-baseline.png
assertions: 17 checks over 10 panels — 10 PASS, 0 FAIL, 7 XFAIL, 0 XPASS
  XFAIL  probe 'one-line paste' / 'line three▍' / 'line three' /
         '[paste #1 +12 lines]' / '[paste #2 +12 lines]' / 'row12' / 'row01'
```

7 条 XFAIL 就是本轮的功能本身。基线的字符网格（`lum1328-paste-baseline.png.txt`）
把缺陷写得很直白：面板 5 的对话区是 `>` 空用户消息 + `faux-model (faux) hello` ——
**粘贴内容被丢掉，回车提交了一条空 prompt**；面板 9 同样。

面板对照（同一份场景）：

| 面板 | 基线 `6a5a2faf2` | 本轮 |
|---|---|---|
| 2 单行粘贴 | 内容消失（`Ignored`） | `one-line paste` 落在草稿里 |
| 4 三行粘贴 | 内容消失，草稿仍是空的 | 三行草稿（`> line one` / `  line two` / `  line three▍`），**没有**回合被触发 |
| 5 回车 | 空 prompt 的回合 | 三行作为**一个** submission（对话区三行 + 一个 `(faux) hello`） |
| 6 12 行粘贴 | 内容消失 | `> [paste #1 +12 lines]▍` |
| 7 退格 | — | marker 整体消失，草稿回到 placeholder |
| 8/9 粘贴 + 回车 | 空 prompt | `[paste #2 +12 lines]` → 对话区出现 `row01 … row12`（正文，不是标记） |
| 10 无 marker 的裸换行字节 | 逐行提交（`raw one` / `raw two` 各自成回合） | 同（这就是"模式没开时"的真实症状，也是本轮为什么必须开模式） |

证据文件：
* `docs/screenshots/lum1328-paste.png` + `.png.txt`（本轮二进制）
* `docs/screenshots/lum1328-paste-baseline.png` + `.png.txt`（基线）

## 4. 门禁（本 tip 实测）

| 门 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 通过（无 diff） |
| `cargo clippy -p pi-tui --all-targets -- -D warnings` | 通过 |
| `cargo clippy -p pi-coding-agent --all-targets -- -D warnings` | 通过 |
| `cargo test -p pi-tui`（全量） | **949 passed / 0 failed**（53 suites，改动全部落地后的那一版；随后一次 `manual_strip` 重写只动了 `decode_csi_u_control` 的取前缀写法） |
| `cargo test -p pi-tui --test composer_paste --test undo --test composer_history --test composer_images` | **66 passed**（最终代码状态的复测） |
| `cargo test -p pi-tui`（最终状态全量复跑） | ⚠️ **未完成：共享 50G overlay 被并发 run 打满（ENOSPC）**，见 §6 |
| 真 PTY 场景 | 17/17 PASS（本轮），基线 7 XFAIL |

工具链用 `scripts/toolchain.sh` 的取法（`RUSTUP_TOOLCHAIN=1.85.0`，`--offline`）。

## 5. 测试用例（`crates/pi-tui/tests/composer_paste.rs`，23 条）

覆盖：多行粘贴不提交 / 回车提交整段 / 一次撤销 / 不打开补全 / CRLF+tab+控制字符归一 /
CSI-u 解码 / 纯控制字符被忽略且不可撤销 / 路径补空格 / 11 行阈值与 10 行边界 /
1001 字符阈值与 1000 字符边界 / 提交展开正文 / 历史回捞仍是 marker /
两个 marker 各自 id / 退格删整标记 / 前删删整标记 / `←`·`→` 跨标记 /
撤销还原 marker 与内容 / 手打的假 marker 不展开 / 模态打开时粘贴被丢弃 /
`translate_event(Paste)` 不会被重放为按键 / `paste_text` 与 `step_paste` 一致。

## 6. 未达项与偏差（诚实条目）

1. **marker id 不重排**：上游删掉 `#1` 后会把后面的 id 逐个递减（`editor.ts:1389-1410`），
   本端口留空号（删了 `#1`，`#2` 仍是 `#2`）。展开、提交、编辑原子性都不受影响，只是标签编号会跳号。
   没做的原因：在 char-indexed buffer 上做"全局重排 + 光标位移"要和 `visual_text` 的行布局
   一起改，风险大于收益 —— 留作后续 issue。
2. **按词移动不吃 marker**：上游 `findWordBackward/Forward` 带 `isAtomicSegment: isPasteMarker`，
   本端口 `word_navigation.rs` 没有原子段钩子，所以 `Alt+B/F` 会走进 marker 内部（不会坏，
   只是多按几次）。`←`/`→`/退格/前删已经原子。
3. **没有 `paste_burst` 兜底**：codex 对"终端不发 paste 事件、但一段字符在极短时间内涌入"
   做了突发判定（`paste_burst`）；上游 pi-ts 没有这个机制（它只依赖 bracketed paste），
   本端口跟上游一致。要兼容老终端才需要补。
4. **宽度口径仍是 1 字符 = 1 列**：Martty 与 codex 都用 `unicode-width`，本 crate 全篇按
   "1 字符 = 1 列"排版（`markdown.rs` 的 Width convention、`hyperlink::visible_width`…），
   粘贴/中文在多行换行处会偏窄。这是**全 crate 的决定**，不能在 composer 里单开第二套口径。
   本轮把"粘贴进去的 tab"换成 4 个空格，正是为了不在这套口径里再引入一个宽度未知的字符。
5. **`#` 触发符仍然只有 `/` `@` 有 provider**（LUM-1305 §6.2 记录过同一件事）：
   `DEFAULT_AUTOCOMPLETE_TRIGGER_CHARACTERS` 里有 `#`，但没有对应 provider。
6. **最终一次全量 `cargo test -p pi-tui` 复跑没跑起来**：环境层面共享 50G overlay 被并发
   run（`lum-1332` 等）打满，`rmeta` 临时目录都建不出来。最终代码状态用 4 个相关 suite
   （含新增的 `composer_paste`）复测通过；`fmt` / 两个 crate 的 `clippy -D warnings` 全绿。
   **这不是"测试通过"的等价声明**：全量复跑需要在磁盘有余量的时间窗补一次。

## 7. 下一步（按性价比）

1. marker id 重排（上游对齐，纯正确性尾巴）；按词移动的 marker 原子性。
2. `#` 触发符接一个真 provider（或从默认触发符里去掉），消除"宣传了不干活"。
3. `unicode-width` 全线宽度口径整改（composer + message + markdown 一起改，配套 PTY 场景）。
4. 扩展事件第二批（LUM-1299）仍然是"插件生态兼容"的头号阻塞，与本轮无耦合。
