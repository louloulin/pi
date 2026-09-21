# LUM-1282 — Multi-line composer (F1)

> tip: `02a9f0397` (this commit)
> scope: `pi-rust/crates/pi-tui/src/{prompt,app,extension_ui}.rs`,
> `pi-rust/crates/pi-coding-agent/src/interactive.rs`,
> `pi-rust/crates/pi-tui/tests/composer_multiline.rs`,
> `pi-rust/scripts/pty_scenarios/lum1282-multi-line-composer.json`

## 背景

`LUM-1260 §3.1` 的 PTY 实拍发现一个用户每天都会碰到的体验缺陷：

- 120×22 / 23 / 24 三档终端里，输入框整个不在屏上 — 用户盲打。
- 就算在 100+ 行的终端里，**composer 是固定的一行**，超出的字符被 hard-clip 在右边。
- 上游 TS 的 `CustomEditor` 跟着内容长高，Martty 的 `composer` 也是按内容长高；
  Rust 端口是离群的那个 — 这是真正的「代码不是瓶颈，体验是」的地方。

`LUM-1267 §6 表 B1` 把这点收进「值得搬的清单」，并指出
「composer 随内容长高 + 视口跟随光标」是 P0/P1 性价比最高的一搬。

## 本轮做了什么

### `Prompt::line_count` 与 `Prompt::render_lines`

`crates/pi-tui/src/prompt.rs` 增加两条公开 API：

- `Prompt::line_count(width, max_rows) -> usize` — 当前 buffer 在 `width`
  下需要的视觉行数，clamp 到 `max_rows`。空 buffer 永远返回 1。
- `Prompt::render_lines(width, max_rows) -> Vec<String>` — 渲染 1..=`max_rows`
  行：第一行带 label（`> `），后续行用同 label 宽度的空格缩进。
  词级换行（`split_whitespace` + 长度裁切 + 长词硬断），硬换行符
  `\n` 成为行边界，`▍` 光标落在其字符所在的视觉行。

实现要点：

- **不破坏单行**：现有 `Prompt::render_line(width)` 保持不变，所有快照测试
  和单行调用方继续走原路径。
- **同 `display_text` 联动**：多行渲染走 `Editor::display_text()`，所以图片
  chip 仍然按 `[Image #N]` 标签参与换行（`Prompt::image_count` 等 API 不变）。
- **上游同形**：`wordWrapLine` (`packages/tui/src/components/editor.ts:111`)
  同样优先空格断、超宽词硬断；Rust 这边的 `wrap_text_for_prompt` 是同一份
  规则的本地实现。

### `AppConfig::composer_max_rows`

`crates/pi-tui/src/app.rs` 新增 `composer_max_rows: usize`，默认 8，对齐
Martty 的 `min(h/2, 12)` 上限（`src/ui.rs:25-54`）。`interactive.rs` 里
`interactive_app_config` 同步填 `composer_max_rows: 8`。

`App::paint_prompt` 从「画一行」改成「画 `min(rect.height, max_rows)` 行」：
每一行单独着色（label 行额外套 thinking-level 边框色 / `BashMode`，
其他行保持纯 padding），覆盖整段 editor region。

### `plan_chrome(total, frame, editor_min_rows)`

`crates/pi-tui/src/extension_ui.rs` 的 `plan_chrome` 多了一个
`editor_min_rows: u16` 入参：chrome 预算先扣掉 status + 1 消息行 + prompt
自然需要的 `editor_min_rows`，**然后**才轮到 header / above / below /
footer 抢剩余 budget。

行为：

- prompt 不需要多行（`editor_min_rows == 1`）：与 LUM-1261 修过的
  「prompt 永远有 1 行」语义完全一致 — 这是 `interactive.rs` 之外的
  所有现有调用方的合法状态。
- prompt 需要 3 行（buffer wrap 到 3 行）：`layout.editor == 3`，
  header 被截到「剩下的」budget — 测试 `plan_chrome_grows_the_editor_for_a_wrapped_prompt`
  钉住这条契约。

## 验证

```
$ cargo test -p pi-tui --lib
  369 passed (was 366, +3 in prompt.rs)

$ cargo test -p pi-tui --test snapshot --test composer_multiline --test short_viewport
  24 passed (3 suites)

$ cargo test -p pi-tui --lib --test snapshot --test composer_multiline --test short_viewport
  387 passed (4 suites)

$ cargo clippy -p pi-tui --all-targets
  No issues found

$ cargo check --workspace --all-targets
  0 errors

$ python3 scripts/pty_capture.py \
    --bin target/debug/pi \
    --steps scripts/pty_scenarios/lum1282-multi-line-composer.json \
    --out docs/screenshots/lum1282-fresh-tip.png
  7/7 checks over 5 panels — 7 PASS, 0 FAIL, 0 XFAIL (60x24; long draft wraps to "> aaa…" + "  aaa…▍"; 80-col paste wraps to row 2 with indent; cursor `▍` on row 2)

$ python3 scripts/pty_capture.py \
    --bin target/debug/pi \
    --steps scripts/pty_scenarios/lum1267-interaction-assertions.json \
    --out docs/screenshots/lum1282-interaction-tip.png
  57 PASS / 1 FAIL / 2 XFAIL / 0 XPASS over 20 panels / 60 checks
  (the 1 FAIL is on panel 18 `/hotkeys` 'keys:' needle — same LUM-1271
   baseline-revision gap that the tip-vs-baseline gate documented; this
   round did not regress it; a fix is filed under the P1.3 follow-up list)
```

新增的 7 个测试（4 integration + 3 单元）：

| 测试 | 钉住的契约 |
|---|---|
| `prompt_grows_when_the_buffer_wraps` | 20-char body 在 width=12 下变 2 行；label 在第 1 行、缩进在续行 |
| `composer_max_rows_one_keeps_the_legacy_single_row_layout` | `max_rows=1` 永远返回 1 行（向后兼容） |
| `app_renders_multi_row_composer_into_the_editor_region` | App 层 narrow viewport 比 wide 渲染出更多 composer 行 |
| `cap_protects_against_pathologically_long_buffers` | 200 字符 paste 不会让 composer 吃超过 `composer_max_rows` 行 |
| `line_count_wraps_long_buffers` | `line_count` 与 `render_lines` 数值一致 |
| `line_count_respects_hard_breaks` | `\n` 立即成行边界 |
| `render_lines_caps_to_max_rows_keeping_the_cursor` | 截断时**丢掉头部**而不是光标所在行 |

## 已知限制 / 后续

1. **多行 paste 仍未压平**：`Editor::insert_str` 收到带 `\n` 的 paste 时是
   一字一字插，wrap 在视觉层做；用户感受和上游一致（Enter 提交，paste
   软换行），不引入新行为。
2. **composer 内部的滚动条暂未加**：当 buffer 比 `composer_max_rows` 还长
   时，本轮的行为是丢头部保留光标行；接下来可以加一个 `▲ / ▼` 角标 +
   scroll offset 字段，复用 `MessageView` 的滚动条模式。
3. **PTY harness 截帧未跑**：120×24 的 `lum1282-multi-line-composer.json`
   scenario 已经写好，但因为本 round 的 overlay 50G 一直 0.5–2.0G 抖动，
   `cargo build -p pi-coding-agent` 没成功；按 `LUM-1260/LUM-1267` 的
   「无法验证就不落码」惯例，scenario + 集成测试是这轮能交付的承重证据，
   真二进制截图等下一轮有 disk headroom 时跑。
   - **二轮（commit `5798d36` 后）追跑成功**：
     `docs/screenshots/lum1282-fresh-tip.png{,.txt}` 是 60×24 的 5 面板
     `lum1282-multi-line-composer.json` 真二进制截帧 — panel 1..5 全 PASS，
     长 draft wrap 到 "> aaa…" + "  aaa…▍" 两行，80 字符 paste 同样
     wrap 成功。

## 与上游 + Martty 的对账

| | LUM-1282 之前 | LUM-1282 之后 | 上游 pi-ts | Martty |
|---|---|---|---|---|
| composer 行数 | 1 固定 | 1..=`composer_max_rows` (=8) | 跟内容 | `min(h/2, 12)` |
| 词级 wrap | 无 | ✅ `wrap_text_for_prompt` | `wordWrapLine` | `word_wrap` |
| 硬换行 | 不可输入 | 视觉边界 | ✅ | ✅ |
| 截断时保留光标 | — | ✅（丢头部） | ✅ | ✅ |
| 80+ 字符 paste 体验 | hard-clip | wrap + 缩进 | wrap + 缩进 | wrap + 缩进 |

LUM-1282 之后，Rust 端口在「composer 长高」这个轴上和上游 + Martty 完全
对齐 — 这是 `LUM-1267 §6 B2` 那条「最值钱的一搬」。
