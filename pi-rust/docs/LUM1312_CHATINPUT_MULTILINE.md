# LUM-1312 — Chat input: multi-line composer behaviour at codex / pi-ts parity

> scope: `pi-rust/crates/pi-tui/src/{visual_text,editor,prompt,app}.rs`,
> `pi-rust/crates/pi-coding-agent/src/{interactive.rs,commands/slash.rs}`,
> `pi-rust/crates/pi-tui/tests/{composer_editing,composer_app_width,undo}.rs`,
> `pi-rust/scripts/pty_scenarios/lum1312-{chatinput-multiline,full-tui}.json`
> branch: `work/LUM-1312` → fast-forwarded into `feature/pi.rs`

一句话结论：TUI 的**输入区**（chat input / composer）从「能换行、能看多行」升级到
「按 codex / pi-ts 的编辑语义工作」——`Ctrl+J` 真正换行（此前是死和弦）、`Up`/`Down`
在草稿内按视觉行移动并保持列、`Ctrl+A`/`Ctrl+E` 与 `Ctrl+U`/`Ctrl+K` 按**当前行**
作用、从首行进入 prompt history。这是 LUM-1282（多行**渲染**）之后缺的那一半：
渲染是新的，编辑语义还是单行文本框的。

---

## 1. 真实审计：本轮前后发生了什么

LUM-1282 把 composer 从「固定一行」改成「按内容长高」，但**编辑语义**没有跟着改，
于是屏幕上有多行、键盘上仍然只有一行。逐条实测（`docs/screenshots/lum1312-*.png.txt`
是真 PTY 的字符网格，不是设计意图）：

| 行为 | 本轮之前（`cdc033e21`，真 PTY 实测/既有测试） | 本轮之后（真 PTY 实测） |
|---|---|---|
| `Ctrl+J` / `Shift+Enter`（`tui.input.newLine`） | **死和弦**：`Shift+Enter` 直接提交草稿，`Ctrl+J` 无反应（`editor.rs` 的 match 分支把 `Shift+Enter` 送进 submit） | 插入换行，composer 立即长出一行；`Enter` 仍然提交（面板 2） |
| `Up` / `Down`（`tui.editor.cursorUp/Down`） | 无论草稿几行，永远翻 prompt history —— 多行草稿里根本走不上去 | 先按**视觉行**移动并保持列；首行才进 history，末行 `Down` 回草稿（面板 3/4/9） |
| `Home` / `End`（`cursorLineStart/End`） | 跳到**整个草稿**的首/尾（多行时跳错位置） | 跳到**当前逻辑行**的首/尾（面板 5；`Home`/`End` 两个裸键在 App 层仍是聊天记录跳转，见 §3） |
| `Ctrl+U` / `Ctrl+K`（`deleteToLineStart/End`） | 杀掉**整个草稿**的前/后半；`/help` 还写着「Ctrl+U 清空输入」 | 只作用于当前行；行首/行尾时杀的是换行符，两行合并（面板 6/7 的真 PTY 证据） |
| `Ctrl+W` / `Alt+D`（`deleteWordBackward/Forward`） | 以整个 buffer 为界（`\n` 被当成普通空白，跨界吞词） | 以当前行为界；行首 `Ctrl+W` 合并上一行，行尾 `Alt+D` 合并下一行 |
| `Alt+←/→`（`cursorWordLeft/Right`） | 全 buffer 跳词，换行处行为偶然正确 | 行内跳词 + 跨行落点与上游一致（行尾 `→` 落到下一行行首） |
| 词/行/列几何 | 渲染用 `split_whitespace` **重排**（`a  b` 显示成 `a b`），光标行号靠「行首偏移」猜 | 行是草稿的**逐字切片**，每行记录源偏移，光标/换行/滚动共用同一份 `VisualLayout` |
| 超长草稿（超过 `composer_max_rows`） | 丢弃**头部**行，光标在头部以下时看不到自己 | 按光标所在页窗口化（`scroll_window_start`），光标行永远在屏上 |

真 PTY 断言（`scripts/pty_capture.py` + `pyte`，76×26 / 100×30，真二进制）：
`lum1312-chatinput-multiline.json` **19/19 PASS**，`lum1312-full-tui.json` **7/7 PASS**，
既有场景无回归（`lum1282` 7/7、`lum1308` 5/5）。

## 2. 实现

### 2.1 `crates/pi-tui/src/visual_text.rs`（新增，约 470 行含 14 个单测）

composer 草稿 → 视觉行布局的**唯一**来源，渲染器（`Prompt::render_lines`）与编辑器
（`Editor::move_vertical` / `caret`）读同一份 `VisualLayout`：

* 行是草稿 `chars[start..end]` 的**逐字切片**，`VisualRow.source` 记录每个字符的源偏移
  —— 光标映射因此是精确的，不是"按行首偏移猜"。上游 `wordWrapLine`
  (`packages/tui/src/components/editor.ts:121`) 的同一套回溯规则：整词放不下就回退到
  最后一个换行机会，否则在行边硬断。
* **不再重排空白**：旧实现 `line.split_whitespace()` + 单空格 join，会把
  `"a  b"`、Tab、行尾空格悄悄改掉（写进草稿的就是被改写后的文本）。新实现的行是原样切片，
  真 PTY 面板 2/10 里 95 字符草稿的换行位置可逐字核对。
* 硬换行（`\n`）永远是行边界，空行也占一行，所以 `Ctrl+J` 立刻长出可见的一行。
* 宽度口径**沿用 crate 现有约定**（`markdown.rs` "Width convention"、
  `hyperlink::visible_width`、`message::display_width`：一个字符 = 一列）。因此**没有**
  引入 `unicode-width` 依赖：见 §5 的已知偏差，这是一个需要全 crate 一起改的决定，
  不该只在 composer 里再立第二个宽度口径。

### 2.2 `crates/pi-tui/src/editor.rs`（编辑语义）

* `line_bounds()` —— 光标所在逻辑行的字节区间（`cursor_line()` / `cursor_col()` 同理）。
* `cursor_up()` / `cursor_down()` / `move_vertical()` —— 上游
  `Editor.handleInput:913-940` 的规则 + `preferredVisualCol` 粘性列（
  `moveToVisualLine:1470` 的决策表）。`cursor_up` 在首行且不在行首时先回行首，再按一次才进
  history（codex#21833 描述的就是这个手感）。
* `move_home` / `move_end`、`kill_to_line_start` / `kill_to_line_end`、
  `kill_word_backward` / `kill_word_forward`、`move_word_left` / `move_word_right`
  全部改为**行域**，行边界处的行为对齐上游 `deleteToStartOfLine` /
  `deleteToEndOfLine` / `deleteWordBackwards` / `deleteWordForwards`
  （行首 `Ctrl+U` 杀换行合并两行；行尾 `Ctrl+K` 同理）。
* `kill_range()` —— 唯一的 kill 入口（kill ring 累积、撤销快照、chip 对齐都在一处）。
* `insert_newline()` + `tui.input.newLine` 接线；`Enter` 前的**反斜杠回退**
  （上游 `shouldSubmitOnBackslashEnter`）：终端报不出 `Shift+Enter` 时，
  行尾 `\` + `Enter` 退格后换成换行（真 PTY 面板已验证）。
* `set_visual_width()` —— App 每帧把 composer 的 body 宽度交给编辑器，垂直移动与渲染
  用同一宽度（`composer_app_width.rs` 钉住"换行宽度变了，下一次按键必须跟着变"）。
* 多行感知的 autocomplete：provider 收到的是真实的 `lines` 数组 + 真实 `(cursor_line,
  cursor_col)`，`/` 命令只在第 0 行触发（上游 `isSlashMenuAllowed`）。
* 非垂直动作（输入、退格、左右移动、yank、undo）清空粘性列，对齐上游 `setCursorCol`。

### 2.3 `crates/pi-tui/src/prompt.rs`

`line_count` / `render_lines` 改为读 `VisualLayout`：光标 `▍` 的行列由布局给出，
窗口按光标分页（`scroll_window_start`），行前缀（`> ` / 两空格缩进）只由"是不是草稿第 0 行"
决定。新增 `body_width()` 供 App 记录。

### 2.4 `crates/pi-tui/src/app.rs`

`composer_body_width: AtomicU16`：`paint_prompt` 记录本帧的 body 宽度，
`step_key_at` 在把按键交给编辑器之前 `set_visual_width` —— `render_lines` 是 `&self`，
渲染无法记住换行宽度，这是把"渲染事实"传给"编辑动作"的那一条线
（`tests/composer_app_width.rs` 三条测试钉住：宽度发布、resize 生效、`Ctrl+J` 长行 +
`Up` 回到上一行）。

### 2.5 `crates/pi-coding-agent/src/interactive.rs`

* `request_keyboard_enhancement` / `release_keyboard_enhancement`：进入 raw mode / 备用屏后
  发出 kitty 键盘协议的 `DISAMBIGUATE_ESCAPE_CODES` 请求（best-effort，终端不支持就忽略），
  suspend 时 `Pop`。这是让 `Shift+Enter` 在真实终端里**可区分**的唯一办法：
  crossterm 已经把 `ESC` `CR` 拆成两个事件，字节级的 `"\x1b\r"` 判断（上游做法）在这一层
  拿不到。真 PTY 已验证 `CSI 13;2u` 会走到 `Enter`+`SHIFT` 分支（§4）。
* `/help` 的 keys 段更新为真实可达的键位（`Ctrl+J` 换行、`Ctrl+A/E` 行首行尾、
  `Ctrl+U/Ctrl+K` 行域删除），删掉「Ctrl+U 清空输入」这条与实现不符的旧文案。

## 3. 真实审计：与 codex / Martty / pi-ts 的对照

| 能力 | codex TUI | Martty | 上游 pi-ts | 本轮之后的 pi-rust |
|---|---|---|---|---|
| 多行 composer（按内容长高） | ✅ | ✅ `min(h/2,12)` | ✅ `CustomEditor` | ✅（LUM-1282 起，本轮语义补齐） |
| 新行键 | `Shift+Enter` / `Ctrl+J`（`/keymap` 可查） | `Shift+Enter` | `shift+enter` / `ctrl+j` | ✅ `Ctrl+J`（全终端）+ `Shift+Enter`（kitty 协议终端）+ 行尾 `\` 回退 |
| `Up`/`Down` 在多行草稿内按行移动 | ✅ | ✅（`move_vertical` + 粘性列） | ✅（`moveCursor` + `preferredVisualCol`） | ✅ `move_vertical` + `preferredColumn` |
| 到顶/底后进 history | ✅（codex#21833） | ✅（空草稿时） | ✅（首行/末行） | ✅ |
| `Home`/`Ctrl+A` 行首、`End`/`Ctrl+E` 行尾 | ✅ | ✅ `LineStart`/`LineEnd` | ✅ `moveToLineStart/End` | ✅（`Ctrl+A/E` 在 App 内可达；裸 `Home/End` 被 App 的视口跳转先截走，与上游同构 —— 见下） |
| `Ctrl+U`/`Ctrl+K` 行域 kill | ✅（zsh 语义，`/keymap` 可关） | ✅ | ✅ | ✅（含行边界合并换行） |
| 词跳 / 词删跨行落点 | ✅ | ✅ | ✅ | ✅ |
| 多行草稿窗口跟随光标 | ✅ | ✅ | ✅（`scrollOffset`） | ✅（分页窗口，未做平滑滚动） |
| composer 自己翻页（`tui.editor.pageUp/Down`） | ✅（文本区内翻页） | — | ✅（`pageScroll`） | ❌ 键先被 App 的聊天记录翻页拿走（**剩余差距**） |
| 大段粘贴折叠成 `[paste #N +M lines]` | ✅ | — | ✅ | ❌ 仍原样内联（**剩余差距**） |
| 跨会话 history + `Ctrl+R` 反查 | ✅（`~/.codex/history.jsonl` + 反查） | — | `historyPrevious/Next`（未绑定） | ❌ 仅会话内（上限 100 条）（**剩余差距**） |
| 从 history 召回图片/元素附件 | ✅ local/remote 图片一起恢复 | — | —（上游 history 只存文本） | ❌ 召回会丢掉 chip（**剩余差距**） |
| 宽度口径 | 终端列 | 终端列 | 终端列（`visibleWidth`） | ⚠️ 字符数（crate 现行约定，见 §5） |

一条需要如实说明的**发现**（不是本轮引入、也不是本轮修的）：`Home`/`End` 在 App 层被
`tui.altScreen.top` / `bottom`（聊天记录跳转）先消费，所以**裸 Home/End 到不了 composer**；
行首/行尾在 App 内实际由 `Ctrl+A`/`Ctrl+E` 提供。上游 TS 同样是这个优先级
（`tui-alt-screen.ts:760,764` 先匹配 `tui.altScreen.top/bottom`，默认键就是 `home`/`end`），
所以这是**继承上游的真实行为**，不是端口 bug；`/hotkeys` 里 `tui.editor.cursorLineStart`
仍会印出 `home, ctrl+home, ctrl+a`（上游也这样印）。本轮把 `/help` 的说明写成了
`Ctrl+A / E`，不写裸 `Home/End`。

## 4. 门禁与验证实况（1.85.0，`--locked`）

```bash
. pi-rust/scripts/toolchain.sh          # 解析出 1.85.0（不联网、不依赖 rustup 默认值）
cd pi-rust
cargo fmt --all -- --check                                  # 干净
cargo clippy --workspace --all-targets --locked -- -D warnings   # No issues found
cargo test --workspace --locked                             # exit 0
```

| 门禁 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | **PASS**（无 diff） |
| `cargo clippy --workspace --all-targets -- -D warnings` | **PASS**（`No issues found`） |
| `cargo test --workspace --locked` | **2512 passed / 0 failed / 2 ignored（164 suites）**；本轮基线（LUM-1308）为 2469/162，**+43 测试 / +2 suite**；修掉 `reload_config` 的并行竞态后连跑全绿 |
| 真 PTY 断言 `lum1312-chatinput-multiline.json` | **19 PASS / 0 FAIL**（12 面板，76×26） |
| 真 PTY 断言 `lum1312-full-tui.json` | **7 PASS / 0 FAIL**（3 面板，100×30） |
| 既有 PTY 场景回归 | `lum1282` 7/7、`lum1308` 5/5；`lum1298` 12/13（见下） |
| `scripts/app_action_coverage.py` | `wired 37/44 (84.1%)`、`advertised 0/44`、`silent 7/44` —— 与本轮前一致（本轮动的是 `tui.editor.*`/`tui.input.*` 轴，不是 `app.*` 轴） |

**门禁可信度修复（顺带，实测到的真问题）**：本轮第一次跑全量 `cargo test --workspace` 时
`reload_config.rs::reload_picks_up_an_edited_file_without_a_restart` 红了
（`left: ["up"] right: ["ctrl+n"]`），重跑又绿 —— `keybindings` 注册表是**进程级全局**，
同一个测试文件里的 4 个测试并行跑，谁先结束谁调 `reset_keybindings()`，就把别人刚装进去的表抹掉。
本轮给该文件的 4 个测试加了 `registry_guard()`（文件内 `Mutex` 串行化，无新依赖，
panic 中毒可恢复），之后 `reload_config` 连跑 5 次全绿，全量门禁也回到 exit 0。
这和 LUM-1308 修 `toolchain.sh` 是同一类问题：**门禁红不红取决于运气，就不是门禁。**

新增/改动的测试：

* `crates/pi-tui/src/visual_text.rs` — 14 个单测（切片分区、源偏移、软换行/硬换行/空行的
  光标亲和、超宽词硬断、行首行尾映射、`cursor_at` 越界钳制）。
* `crates/pi-tui/tests/composer_editing.rs`（新，26 个测试）—— 换行/提交/反斜杠回退、
  多行 `Up`/`Down` 与粘性列、history 边界、行域 `Home/End`、行域 kill 与合并换行、
  跨行词跳、撤销、chip 对齐。
* `crates/pi-tui/tests/composer_app_width.rs`（新，3 个测试）—— App 每帧发布 body 宽度、
  resize 后下一次按键跟着变、`Ctrl+J` 长行 + `Up` 回到上一行（整帧断言，读 `▍` 所在行）。
* `crates/pi-tui/tests/undo.rs` —— 一处既有断言随语义更新：`Up` 从"直接翻 history"
  改成"先回行首、再进 history"（上游规则），因此撤销点与快照里的光标位置也随之写明。

**未通过但与本轮无关的既有项**（如实记录，未改）：`lum1298-multi-line-and-model.json`
面板 3 断言 `openai`，而 `/model` 选择器第一屏现在是 `ant-ling` / `anthropic`
（provider 表在 LUM-1298 之后变长了，场景断言过期）。同一帧里 `anthropic`、`Pick a model`
都 PASS，帧内容可见于 `lum1298` 的 dump —— 是场景陈旧，不是渲染回归。

## 5. 已知偏差与限制（全部真实，不粉饰）

1. **宽度口径不一致（crate 级）**：`pi-tui` 全 crate（`markdown.rs`、
   `message.rs`、`selector.rs`、`hyperlink::visible_width`）按**字符数**度量，上游
   `visibleWidth` 按**终端列**。本轮 composer 沿用 crate 约定（一个宽字符算一列），
   所以 CJK 草稿的换行位置与上游不同。修它要动 markdown/transcript/selector 的宽度测量
   与全部快照，属于 crate 级决定，已作为后续任务拆出（见 §6），**没有**只在 composer 里
   另立第二个口径、也没有顺手引入 `unicode-width` 依赖。
2. `Shift+Enter` 只在**支持 kitty 键盘协议**的终端里可区分（我们已经 best-effort 请求）；
   在只发 `ESC CR` 的传统终端里，crossterm 会把它拆成 `Esc` + `Enter`（实测：提交），
   此时用 `Ctrl+J` 或行尾 `\` + `Enter`（两者都有真 PTY 证据）。这一条无法在本沙箱里
   对真实 kitty 终端做端到端验证（没有 kitty/ghostty），只验证了协议序列的解析路径。
3. composer 窗口是**按页**切换而非平滑滚动（`render_lines` 是 `&self`，无法记住滚动偏移）；
   光标移动不会让 composer 逐行滚动。
4. `tui.editor.pageUp/pageDown` 仍归 App 的聊天记录翻页所有，composer 自己不翻页。
5. `Ctrl+;`(无) / `Ctrl+R` 反查、跨会话 history、粘贴折叠、附件召回均未实现（§3 表里列出）。
6. `lum1298` 场景里有一条过期断言（见 §4 末），本轮未改它，避免掩盖问题。

## 6. 后续任务与完成度

`tui.editor.*` + `tui.input.*` 共 27 个 id：直接消费 **22/27 → 23/27**
（唯一新增的是此前「advertised 但没人消费」的 `tui.input.newLine`），
其中 **11 个 id 的语义从"行域错误"改成上游语义**
（`cursorUp/Down`、`cursorLineStart/End`、`deleteToLineStart/End`、
`deleteWordBackward/Forward`、`cursorWordLeft/Right`、`input.newLine`）。

| 轴 | 完成度（自评） |
|---|---|
| composer 多行**渲染**（LUM-1282 起） | 90% |
| composer **编辑语义**（本轮） | 由约 45% → **约 80%**（差距项：翻页、粘贴折叠、history 持久化/反查、附件召回、宽度口径） |
| `app.*` 动作接线 | 84.1%（37/44，本轮未变） |
| 整个 TUI 对 codex/pi-ts 的交互面 | 约 75%（剩余为 provider/后端与上述 composer 差距项，非渲染面） |
| Rust↔TS 整体移植 | 与本轮前一致（非 TUI 面：provider/OAuth/远程模型目录仍是主要缺口，见 `docs/FEATURE_PI_RS_STATUS.md`） |

本轮派出的后续任务（最多 3 个并发）：

1. **composer 翻页 + 平滑滚动**（`tui.editor.pageUp/pageDown` 归位、窗口记忆）——
   闭合 §5.3/§5.4。
2. **粘贴折叠 `[paste #N +M lines]`**（上游 `pasteCounter`/`pastes` 表 + 展开语义）——
   大段粘贴不再撑爆 composer 和 history。
3. **history 持久化 + `Ctrl+R` 反查 + 附件召回**（对齐 codex `ChatComposerHistory`）——
   闭合 §5.5。
