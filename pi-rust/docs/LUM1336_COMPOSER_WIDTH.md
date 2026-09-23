# LUM-1336 — composer 宽度口径：从「一个字符一列」改成「一个终端列」

> scope: `pi-rust/{Cargo.toml,Cargo.lock}`,
> `pi-rust/crates/pi-tui/{Cargo.toml,src/visual_text.rs,src/prompt.rs,src/editor.rs,src/app.rs}`,
> `pi-rust/crates/pi-tui/tests/composer_wide_chars.rs`,
> `pi-rust/scripts/pty_scenarios/lum1336-cjk-composer{,-baseline}.json`,
> `pi-rust/docs/screenshots/lum1336-cjk-composer{,-baseline}.png(.txt)`
> branch: `work/LUM-1336-pi-rs` → `feature/pi.rs`
> baseline: `feature/pi.rs` = `e0ba60d5c`
> reference: codex `chat_composer`（textarea 用 `unicode-width` 量列）、Martty
> `src/input/editor.rs`（`visual_cursor` / `wrap_end_position` 同样按列）、上游 TS
> `packages/tui/src/components/editor.ts` 的 `wordWrapLine`

一句话结论：composer 用「一个字符 = 一列」量宽度，而**终端的一列就是 frame buffer 的一个
cell**——一个全角字（CJK、多数 emoji）占两列。后果不是"排版略偏"，而是三件用户可见的坏事：
光标（`▍`）常常**根本画不出来**、宽字符后面残留上一帧的字符（`世界` 渲染成 `世C`）、
以及一段中文草稿**只有第一个字在屏幕上**。本轮把 composer 的布局/光标/指针/绘制全部改成
按列量，并用真 PTY 的 A/B 帧把三件事钉死。

---

## 0. 结论速览

| 项 | 本轮之前（`e0ba60d5c`，真二进制实测） | 本轮之后（实测） |
|---|---|---|
| 两个全角字 `世界` 的 composer 行 | `> 世C`（`C` 是上一帧 ASCII 残留的 cell） | `> 世界▍` |
| 45 个全角字（90 列）草稿 | **一行**，屏幕上只有 `> 中C`，其余 88 列不可见 | 两行：`> `+29 字 / 缩进 +16 字，全部可见 |
| 光标标记 | 该帧**没有** `▍`（cell 被 wide-glyph 的 skip 吞掉） | `▍` 落在光标所在列、所在行 |
| 指针点选全角字 | 点在第 2 个 cell 上会落到下一个字 | 落在该字之前（列 → 字符的精确映射） |
| composer 单元/集成测试 | — | `pi-tui` 969 passed（新增 `composer_wide_chars.rs` 8 条，**修前 7/8 红**） |
| 全量门禁 | — | `fmt` 干净、`clippy -D warnings` 干净、`cargo test --workspace --locked` **2709 passed / 2 ignored / 0 failed**（173 suites，**合并树**上复测） |

## 1. 真实审计：缺陷是什么，为什么

### 1.1 症状（真 PTY，`scripts/pty_capture.py`，60×24）

```
# 修前 docs/screenshots/lum1336-cjk-composer-baseline.png.txt
键入 世界          →   > 世C
键入 45 个全角字    →   > 中C          （一行；后面 88 列不在屏上）
                        （该帧没有 ▍）

# 修后 docs/screenshots/lum1336-cjk-composer.png.txt
键入 世界          →   > 世界▍
键入 45 个全角字    →   > 中文宽字符折行验证中文宽字符折行验证中文宽字符折行验证中文
                        宽字符折行验证中文宽字符折行验证▍
```

### 1.2 根因（不是推测：三处代码 + 原始字节流互证）

1. **绘制**：`App::paint_prompt` 按 `line.chars().enumerate()` 一个字符写一个 cell：

   ```rust
   for (col, ch) in line.chars().enumerate() {
       let x = rect.x + col as u16;   // ← 用字符下标当列
       buf.cell_mut((x, y)).set_char(ch);
   }
   ```
   `世` 被写进 cell 2，`界` 被写进 cell 3——但在终端上 `世` 从第 2 列开始、**占 2-3 列**，
   所以 `界` 实际落在第 3-4 列：buffer 的 cell 网格和终端的列从此错开。

2. **diff**：`ratatui 0.28.1` 的 `Buffer::diff`（`buffer/buffer.rs:466-488`）对宽字符是
   `to_skip = symbol_width - 1`，并且 `invalidated = max(current_w, previous_w) - 1`。
   于是当 cell 3 也是宽字符时，cell 3 被"跳过"，cell 4 又被 `invalidated`/`to_skip`
   的连锁跳过——**上一帧的 cell 不会被清掉**。这就是"残留"的来源。

   原始字节流是决定性证据（`/tmp/raw_probe.py` 直接读 PTY 主端，`世界` 一次写入）：

   ```
   \x1b[9;3H世        ← cell 2 = 世（2 列）
   \x1b[9;6H + 之后全是空格   ← 第一个被清掉的 cell 是 5，cell 3、4 被跳过
   ```

   cell 4 因此保留了上一帧的 `p`（placeholder `type a prompt` 的第 3 个字符），
   屏幕上是 `> 世p`；在另一个场景里上一帧是 `ABCDEFG…`，于是残留 `C`。

3. **换行**：`visual_text::display_width` 定义"每字符一列"，`VisualLayout::new(text, width)`
   因此按**字符数**折行。45 个全角字 = 90 列，却按 45 列排进一行；`paint_prompt` 的
   边界判断也是字符数，所以整行都被画进 buffer，终端只能在右边界截断。

4. **光标**：`caret()` 返回"光标之前的字符数"，`build_prompt_row` 又把它当字符下标插入 `▍`，
   于是标记落在错误的 cell；再叠加第 2 条的 skip，标记干脆不画。

### 1.3 影响面（用户可见）

- 中文/日文/韩文草稿：**看不见自己打了什么**（长过半个屏幕就只有第一个字）。
- 任何宽字符后面会残留脏字符，随上一帧内容变化（"幽灵字"）。
- 光标不可见 → 无法判断插入位置。
- 指针点选错位。

对一个中文用户的 coding agent TUI 来说，这是输入面 P0，不是"排版优化"。

## 2. 上游 / 两个参照实现的口径

| 规则 | 上游出处 | 本轮实现 |
|---|---|---|
| 布局按**列**折行，宽字 2 列 | 上游 TS `wordWrapLine`（`editor.ts:121`，`stringWidth` 语义）；codex textarea 用 `unicode-width` | `visual_text::cell_width` 用 `unicode-width`，`wrap_line` 的 `current_width` 累加列 |
| 两个 CJK 字符之间可以断行 | 上游 `wordWrapLine` 第二条规则 | 列口径下自然成立：两个宽字 = 4 列，`current_width + 2 > width` 即强制断行 |
| 光标按列定位 | codex `ChatComposer` 的 textarea 光标；Martty `Input::visual_cursor(width)` 返回 `(row, col)` | `caret()` 返回列；`cursor_at(row, column)` 把列映射回字符偏移 |
| 指针按列命中 | codex `chat_composer/mouse.rs`；上游 `Editor.handleMouse` | `click_offset(row, column)` 按列比较、按列映射（新增 `char_index_at_column`） |
| 宽字符占位要"清掉"后面的 cell | `ratatui::Buffer::set_stringn`（`buffer.rs:313-347`：`set_symbol` + `while x < next_symbol { reset() }`） | `paint_prompt` 对每个字符写 cell 后把其覆盖的 `width-1` 个 cell 置为空格 |
| 零宽字符（组合符号、CRLF 的 `\r`）不占列 | Martty `normalize.rs` 折叠；codex textarea | `cell_width` 返回 0，绘制时**追加**到前一个 cell 的 symbol 上（终端负责合成） |
| 制表符 | 上游无 tab 键；粘贴里的 tab 保留 | 保留旧约定 1 列（`\t`），其余控制字符 0 列 |

## 3. 改动清单

| 文件 | 内容 |
|---|---|
| `pi-rust/Cargo.toml` | `[workspace.dependencies]` 增 `unicode-width = "=0.1.14"`（已在图里：`ratatui` → `unicode-truncate`，锁文件只多一条依赖边，见 §7） |
| `crates/pi-tui/Cargo.toml` | `unicode-width.workspace = true` |
| `crates/pi-tui/src/visual_text.rs` | 新增 `cell_width(ch)`（`unicode-width`，tab=1、控制字符=0）与 `cells(text)`；`wrap_line` 改用列；`VisualRow::width()`；`caret` 返回**列**；`cursor_at`/`click_offset` 接受**列**并按 `char_index_at_column` 映射；`row_width()` 取代 `row_len()` 用于"粘列"夹取；模块文档改写（含"crate 其余部分仍是字符口径"的指向） |
| `crates/pi-tui/src/prompt.rs` | `build_prompt_row` 把光标列换算成字符下标再插 `▍`；`blank_row`/`empty_row`/`render_line`/`history_search_row` 的 padding 与截断改按列（`char_truncate` → `column_truncate`）；`body_width`/`window_prefix` 用 `cells()` |
| `crates/pi-tui/src/app.rs` | `paint_prompt` 按列绘制：写字符、清掉其覆盖的 cell、零宽字符追加到前一 cell；label 着色判断也按列 |
| `crates/pi-tui/src/editor.rs` | `move_vertical` / `page_scroll` 的粘列夹取改用 `row_width`（口径必须和 `caret` 一致） |
| `crates/pi-tui/tests/composer_wide_chars.rs`（新） | 8 条**读渲染帧**的用例：列折行、混合宽度折行、幽灵 cell 不残留、`▍` 落在光标列（纯 CJK / 混合 / 折行）、粘贴块、指针点在宽字第二 cell |
| `scripts/pty_scenarios/lum1336-cjk-composer.json`（新，B 侧） | 5 个 panel / 8 条断言，60×24 |
| `scripts/pty_scenarios/lum1336-cjk-composer-baseline.json`（新，A 侧） | 同样的按键序列，断言**修前**行为（`> 世C`、`> 中C`、无 `▍`） |
| `docs/screenshots/lum1336-cjk-composer{,-baseline}.png(.txt)` | A/B 帧 + 字符网格 |

## 4. 真 PTY 证据（A/B 矩阵）

命令（两侧都是真二进制、真 PTY、同一 scenario 结构；A 侧的二进制是 base tip `e0ba60d5c` 的构建产物）：

```bash
# A 侧
python3 scripts/pty_capture.py --bin <base>/target/debug/pi \
  --steps scripts/pty_scenarios/lum1336-cjk-composer-baseline.json \
  --out docs/screenshots/lum1336-cjk-composer-baseline.png \
  --text-out docs/screenshots/lum1336-cjk-composer-baseline.png.txt
# B 侧
python3 scripts/pty_capture.py --bin /tmp/pi-rust-target-lum1336/debug/pi \
  --steps scripts/pty_scenarios/lum1336-cjk-composer.json \
  --out docs/screenshots/lum1336-cjk-composer.png \
  --text-out docs/screenshots/lum1336-cjk-composer.png.txt
```

| 断言 | A 侧（`e0ba60d5c`） | B 侧（本轮） |
|---|---|---|
| 空 composer 的 placeholder | PASS `> type a prompt` | PASS |
| 40 个半角字符一行 | PASS | PASS（对照组：半角不触发本缺陷） |
| 两个全角字后的 cell | PASS `> 世C`（脏 cell） | PASS `> 世界` |
| 第二个全角字是否上屏 | PASS `> 世界` 计数 0 | PASS `> 世界` 存在 |
| 45 个全角字是否折行 | PASS `> 中C`（一行） | PASS 首行 29 字 = 58 列 |
| 是否有续行 | PASS 计数 0（没有续行） | PASS `  宽字符折行验证中文宽字符折行验证` |
| 是否有 `▍` | PASS 计数 0（**没有光标**） | PASS 末行末尾 `▍` |
| 合计 | **8 PASS / 0 FAIL / 0 XFAIL**（A 侧断言的是缺陷本身） | **8 PASS / 0 FAIL / 0 XFAIL** |

交叉对照（同一 scenario 打在对侧二进制上）也做了：B 侧 scenario 打 A 侧二进制 = 4 PASS / 1 FAIL / 3 XFAIL，
A 侧 scenario 打 B 侧二进制同样全红——两侧断言互斥，说明这组帧不是"两边都能过"的空断言。

另外用 `/tmp/raw_probe.py`（直接读 PTY 主端、不做 VT 解析）拿到 §1.2 那段字节流，
把"跳过的 cell"落在哪个坐标上钉死，避免把渲染工具的差异当成应用缺陷。

## 5. 整 TUI 的真实分析：本轮**只**修了输入面，剩下的列口径问题在哪

这一节回答 issue 里的「分析整个 TUI 的问题 / 优先完善 TUI」。本轮把输入面（composer）做到列口径；
**输出面**仍是「一个字符一列」，这是本轮**没有**做的部分，逐条列出（`grep` 可复现）：

| # | 缺口 | 位置 | 现在的行为 | 影响 |
|---|---|---|---|---|
| 1 | chat 转写与 markdown 折行 | `markdown.rs`（"Width convention"）、`message.rs:1574 wrap_text`、`message.rs:1696 display_width` | 按字符折行 | 中文回复会按 2 倍宽度排版：超过一半的字被 markdown 的边界裁掉 |
| 2 | 共享绘制函数 | `styled.rs:270 write_styled_line_hyperlinked`（`for ch in span.text.chars()` + `col += 1`） | 一个字符一个 cell | 和 §1.2 同一类 cell 网格错位（转写/面板/下拉框/设置页共用这一个函数） |
| 3 | footer / 状态条 | `status.rs:233/241/242/343/368`（`chars().count()`） | 字符数当列数 | 左右分片宽度预算错，长 model id 或中文 session 名会被裁 |
| 4 | 选择器 / 设置页 | `selector.rs:894 display_width`、`settings.rs:486 display_width` | 字符数 | 描述列不对齐，长 CJK 标签溢出 |
| 5 | 超链接 / LaTeX | `hyperlink.rs:215 visible_width`、`latex.rs` 同名函数 | 字符数 | OSC 8 链接的 cell 记账在宽字符上错位 |
| 6 | 对话框 | `dialog.rs:300` 内联字符计数 | 字符数 | 中文按钮/正文折行错 |

上表是**读码定位 + 一处实测**（不是逐条实测）：实测的那条是转写区——把一段中文 body
交给 `MessageView::render_lines(40)`，返回的行**最宽 69 个 cell**（`chars` 37），
即请求 40 列时每行有 29 列在区域外被裁掉（`cargo test -p pi-tui --test <scratch> -- --nocapture`
的一次性探针，未提交）。其余 5 条给了 `file:line`，下一轮的第一件事就是把它们变成真 PTY 断言。

**为什么本轮不顺手改**：这 6 处不是"再改几个 `chars().count()`"——它们是**同一个 crate 级约定**的
多个副本（6 个同名 `display_width`/`visible_width` + markdown 自己的规则），一次性改动会动到
`markdown.rs`(1950 行)/`highlight.rs`(2574 行)/`message.rs`(1941 行) 的折行语义与大量既有断言，
属于独立一轮的工作量；而 composer 是**用户唯一持续注视的输入面**，且它的绘制走的是自己那条
`paint_prompt`（不与 `styled.rs` 共用），所以可以单独收敛、单独取证。本轮的验收范围就锁在这里，
并把上表作为下一轮的第一顺位（见 §9）。

**与在飞任务的关系**（issue 要求「最多 3 个任务同时运行」，本轮盘点时 `in_progress` 恰为 3 个）：
LUM-1318（粘贴折叠）、LUM-1332（composer 拖选+复制）、LUM-1333（补全下拉指针路由）都在
`pi-tui` 的 composer 面上，但都不改宽度口径；本轮**没有**碰粘贴/拖选/下拉框的语义，也没有新增子 issue
（3 个槽位已满，再派只会把同一片代码拆成并发写者）。

## 6. Rust ↔ TS 差距（本轮实测，命令可复现）

| 口径 | 数值 | 怎么量的 |
|---|---|---|
| 纯代码规模 | **88.9%**（136,071 / 153,106 行） | `find pi-rust/crates -path '*/src/*' -name '*.rs' ! -name 'mod.rs' \| xargs wc -l` / `find packages -path '*/src/*' -name '*.ts' \| xargs wc -l` |
| 测试规模 | **49.4%**（2,623 / 5,309） | `grep -rho '#\[test\]\|#\[tokio::test\]' pi-rust/crates --include=*.rs \| wc -l` / `find packages -name '*.test.ts' \| xargs grep -ho '\bit(\|\btest(' \| wc -l` |
| `app.*` 接线 | **43/44 = 97.7%**（silent 1：`app.tree.editLabel`） | `python3 pi-rust/scripts/app_action_coverage.py` |
| 扩展事件 | **21/36 tag = 58.3%、20/36 生产发射点 = 55.6%** | `python3 pi-rust/scripts/extension_event_coverage.py` |
| **TUI 输入面（composer）** | 本轮从"字符口径"改为"列口径"后，**与 codex / Martty / 上游 TS 的列语义一致**；剩余差异是 §5 表里的**输出面** | 见 §2 逐条对照 |

完成度：issue 要求的三件事全部落地——(1) 真审计 + 截图展示 TUI 与 codex/pi 的交互差距（§1、§4），
(2) 修 chatinput 的实质性差距（§3），(3) 真实完成度与 Rust↔TS 差距（本节）。
本轮的增量不在"覆盖率计数表"上：`app.*`/事件数量一个没动，动的是**输入面正确性**这一维度。

## 7. 门禁（Rust 1.85.0，离线）

`PATH` 上的 `cargo` 是坏 wrapper（`~/.local/bin/cargo` 指向已不存在的 `/tmp/cargo-home/bin`），
所以本轮用 `pi-rust/scripts/toolchain.sh` 的同一策略：直接取
`/tmp/rustup-home/toolchains/1.85.0-x86_64-unknown-linux-gnu/bin` 放 PATH 最前。

| 门禁 | 命令 | 结果 |
|---|---|---|
| 格式 | `cargo fmt --all -- --check` | ✅ 干净 |
| 静态检查 | `cargo clippy --offline --workspace --all-targets --locked -- -D warnings` | ✅ 0 warning（首轮抓出 `clippy::repeat_once`，已按建议改掉） |
| 全量测试（本轮的树） | `cargo test --offline --workspace --locked --no-fail-fast` | ✅ **2691 passed / 2 ignored / 0 failed**，172 suites |
| 全量测试（**合并 `origin/feature/pi.rs` 后**的树，`e0ba60d5c` + 本轮 + LUM-1318/LUM-1333） | 同上 | ✅ **2709 passed / 2 ignored / 0 failed**，173 suites；`fmt`/`clippy` 同样干净；A/B 帧在合并树上重拍，结果与 §4 逐字一致 |
| `pi-tui` 单包 | `cargo test --offline -p pi-tui` | ✅ 969 passed / 55 suites（新增 `composer_wide_chars.rs` 8 条） |
| 锁文件 | `git diff pi-rust/Cargo.lock` | 只多一条依赖边（`pi-tui` → `unicode-width`），**没有版本变化** |

修前/修后的单元测试对照（同一测试文件，同一命令；把本轮 `src/` 改动还原成 `e0ba60d5c` 的内容后重跑）：

```
修前: cargo test -p pi-tui --test composer_wide_chars  →  1 passed / 7 failed
修后:                                                   →  8 passed / 0 failed
```
修前的失败信息就是缺陷本身，例如
`left: "> 世你▍" / right: "> 世界你好▍"`、`left: "> ab中中中中中中中中中cd▍" / right: "> ab中…（折行）"`、
`caret cell: left 6 / right 10`。

合并说明：`origin/feature/pi.rs` 在本轮期间前进到 `4eb70815d`（LUM-1318 粘贴折叠重编号 + LUM-1333 补全下拉指针路由）。
合并**无冲突**（他们的改动落在 composer 的粘贴/下拉框语义，不涉及宽度口径），合入后按惯例在最终树上复跑了
`fmt` / `clippy` / 全量测试与真 PTY A/B，数字见上表。

构建环境：`CARGO_TARGET_DIR=/tmp/pi-rust-target-lum1336`（本轮独立 target，避开另外两路的 cargo 锁）、
`CARGO_PROFILE_DEV_DEBUG=0`、`CARGO_INCREMENTAL=0`、`CARGO_HOME=~/.cargo`、`--offline`。
过程中遇到一次 ENOSPC（50 G 卷上同时有多个 autopilot 任务在构建），清掉本轮临时 target 后原样重跑。

## 8. 复现命令

```bash
cd pi-rust
# 1. 单测（列口径契约）
cargo test --offline -p pi-tui --test composer_wide_chars

# 2. 真 PTY A/B（60x24，5 panel / 8 断言）
python3 scripts/pty_capture.py --bin <base-bin> \
  --steps scripts/pty_scenarios/lum1336-cjk-composer-baseline.json \
  --out /tmp/a.png --text-out /tmp/a.txt
python3 scripts/pty_capture.py --bin <fixed-bin> \
  --steps scripts/pty_scenarios/lum1336-cjk-composer.json \
  --out /tmp/b.png --text-out /tmp/b.txt
grep '^> \|^  ' /tmp/a.txt /tmp/b.txt     # 修前 > 世C / > 中C，修后 > 世界▍ / 两行

# 3. 原始字节流（把"被跳过的 cell"钉到坐标上）
python3 /tmp/raw_probe.py <fixed-bin> 世界 2.5
```

## 9. 已知限制与后续

**已知限制（本轮）**

1. 只收敛了 composer 这条绘制/布局路径；§5 表里 6 处输出面仍是字符口径（明确、可复现、本轮不碰）。
2. 组合符号（`e` + U+0301）在 composer 里按"零宽字符追加到前一 cell"处理，没有做 grapheme cluster
   级的布局（`visual_text` 的文档早已写明按 `char` 切分是为了和 editor 的 `char` 光标模型一致）。
3. Emoji 的宽度取 `unicode-width` 的分类：绝大多数 emoji 2 列，但 ZWJ 序列按每个 `char` 分别取宽
   （同上，不做 cluster）。真 PTY 断言覆盖 CJK，未覆盖 emoji 全家桶。
4. `column_truncate` 只保证"不切半个宽字"，不做省略号（与上游 placeholder 截断一致）。
5. A 侧截图里的脏字符（`C`）依赖上一帧内容，所以 A 侧断言写的是**本 scenario 的**确定性结果；
   换成别的按键序列脏字符会不同（这正是缺陷的特征）。

**下一轮第一顺位（按性价比）**

1. §5 第 2 条 `styled.rs::write_styled_line_hyperlinked` 改列口径（一个函数，转写/面板/选择器共用）
   ——但必须先解决 §5 第 1 条（markdown/message 的折行）否则"画得对但排得更早截断"。
2. §5 第 1 条：`message.rs` + `markdown.rs` 的折行改列口径（中文回复当前会掉一半字）。
3. §5 第 3 条 footer 左右分片宽度预算。
4. 扩展事件第二批（`agent_settled` / `before_agent_start` / `ui_prompt_start|end`）——插件生态轴的头号阻塞，
   与本轮无耦合（`docs/LUM1330_TOOL_HOOKS.md` §8.2）。
