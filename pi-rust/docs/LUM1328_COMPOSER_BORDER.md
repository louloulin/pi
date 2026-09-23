# LUM-1328 follow-up — composer border (rule above / rule below), upstream `editor.ts` parity

> scope: `pi-rust/crates/pi-tui/src/{prompt,app,lib}.rs`,
> `pi-rust/crates/pi-coding-agent/src/interactive.rs`,
> `pi-rust/scripts/pty_scenarios/lum1328b-composer-border.json`,
> `pi-rust/docs/screenshots/lum1328b-composer-border.png`
> base: `origin/feature/pi.rs` = `4f9817cd1`

一句话结论：输入框现在有边框了 —— 上游 `packages/tui/src/components/editor.ts` 的
**上一条 `─` 规则 + 草稿行 + 下一条 `─` 规则**（`renderTopBorder` / `renderBottomBorder`），
颜色走编辑器边框色（bash 模式色 / thinking level 色）。**没有左右边框** —— 上游源码里那句
注释就是设计说明：*“no side borders, just horizontal lines above and below”*
(`components/editor.ts:597`)。窗口被裁剪时，隐藏行数画在**规则上**
（` ↑ 3 more ` / ` ↓ 3 more `，`createScrollBorder`），而不是像本轮之前那样挤在 `> ` 标签列里。

---

## 1. 参考对象：用户截图 + 上游源码

### 1.1 用户附图的真实结构（逐像素量出来的，不是"看起来像"）

附图 2300×1228、无 OCR 工具，所以用像素分析取结构（`attachments/image.png`）：

| 量到的事实 | 数值 |
|---|---|
| 内容区 | x 56..2215，**单元格 16×32 px**（135 列 × 38 行） |
| 全场水平线（≥80% 宽度同一颜色） | 恰好 6 条：y=306/434/498/754（亮黄 `(255,255,5)`）、y=1137-1139 与 y=1201-1203（蓝灰 `(129,162,191)`） |
| **竖直线** | **一条都没有**（48..2242 全扫，只找到窗口边缘与一条转录区左侧的灰条） |
| 输入行 | 上面两条蓝灰线之间恰好一行（y 1152..1183），行首 col 0 是一块 16×32 的实心灰块 = **光标方块**，整行其余为空 |
| 结论 | 这就是「规则 / 输入行 / 规则」，没有左右边框，空草稿时光标停在 col 0 |

### 1.2 上游 pi-ts 的同一套渲染（逐行核对）

```ts
// packages/tui/src/components/editor.ts
protected renderTopBorder(width, hiddenLineCount) {
  const border = hiddenLineCount > 0 ? createScrollBorder("↑", hiddenLineCount, width) : "─".repeat(width);
  return this.borderColor(border);
}
protected renderBottomBorder(width, hiddenLineCount) { /* 同上，↓ */ }
// render(): result.push(top border) → 内容行（注释：no side borders, just
// horizontal lines above and below）→ result.push(bottom border)
```

* `createScrollBorder` (`:266-282`)：` ↑ 3 more ` 居中；放不下退化成 `─── ↑ 3 more `；再放不下就截断 + `...`。
* `paddingX` 默认 0，所以 `layoutWidth = width - 1`（给光标留一列），内容行两侧没有竖线。
* 颜色由 `updateEditorBorderColor`（`interactive-mode.ts:4166-4174`）在
  `getBashModeBorderColor()` / `getThinkingBorderColor(level)` 之间切换。
* 光标：草稿末尾画 `\x1b[7m \x1b[0m`（反显空格）→ 空草稿时就是附图里那块 col 0 的实心方块。

**所以本轮的实现 = 把上游这套 chrome 补进 Rust 端口**，附图与上游源码互相印证（附图是 16×32 单元格的
高分屏截图，规则/输入行/规则的行距 1 行，与上游 render() 的输出完全一致）。

### 1.3 Martty 的对照（用户点名"学习 Martty chatinput 存在边框的设计"）

Martty 有两套 composer（`src/ui.rs`）：`draw_composer_box`（`BorderType::Rounded` 圆角整框 +
顶边 `title` + 底边 `title_bottom(meta_line)` + 运行中左侧被 `▎` 品牌色光条替换）与
`draw_composer`（无边框面板底色 + 左侧 `▎` 光条，注释写着"the old border used to do this"）。
它的行预算同样是"边框不吃额外行"：`cap_h`(1) + `composer_h`，底边承载 meta 行。

本轮**没有**照搬 Martty 的圆角整框，理由写在这里，供评审判断：
用户附图的实测结构是"只有上下两条规则、没有竖线"，而这正是**上游 pi-ts 自己的**编辑器边框；
本项目的既定口径是"镜像上游 pi-ts"（LUM-1312/1317/1319 都是这个口径），Martty 是同类设计的旁证。
若要 Martty 那套（圆角框 + 标题 + 底边状态行），是另一种视觉选择，可另开一轮按同样方式截图对比。

## 2. 实现

### 2.1 `pi-tui/src/prompt.rs`

* 新增 `PROMPT_BORDER_ROWS = 2`、`PromptRowKind { Body, Search, Border }`、
  `PromptRow { text, kind }`、`PromptFrame { rows, scroll }`。
* `Prompt::render_frame(width, draft_cap, region_rows, scroll) -> PromptFrame`：
  顶规则 → （可选的 reverse-search 行）→ 草稿行 → 底规则；`region_rows` 是布局真正给的高度，
  放不下两条规则（< 3 行）时**退化成无边框**（保留旧的标签列 `↑`/`↓` 标记，见
  `a_frame_without_rules_keeps_the_gutter_markers`）。`render_lines` 变成它的文本视图
  （老调用方零改动）。
* `border_row` / `scroll_border`：`createScrollBorder` 的逐条移植（居中 / 短式 / `...` 三档）。
  上游那条"短式"分支实际不可达（`label_width + 2 <= width` 的门槛比短式更松），照抄保留并在注释里说明。
* `line_count(width, draft_cap)` 现在返回**草稿 + chrome**：`composer_max_rows` 是**草稿**上限，
  边框是它的 chrome（上游 `maxVisibleLines` 与两条边框行也是分开的），所以草稿窗口的 8 行不变。
* `Prompt::set_border(bool)` / `border_enabled()`：**默认关闭**（见 §6）。

### 2.2 `pi-tui/src/app.rs`

* `AppConfig::composer_border: bool`（默认 `false`）→ `App::new` 时 `prompt.set_border(..)`。
* `paint_prompt`：改用 `render_frame`，`PromptRowKind::Border` 的整行用编辑器边框色
  （`ThemeColor::BashMode` / `thinking_border_color(level)`，与既有的标签着色同源）。
* `composer_window` 记录**本帧真正画出的草稿行数**（`rows.len() - chrome`），
  所以 `PageUp`/`PageDown` 的归属判断仍然只数草稿；`composer_hit` 把点击的 y 先减去
  chrome（顶规则 + 搜索行）再映射到草稿行 —— 点在规则上返回 `Absorbed`，不会把光标放错行。

### 2.3 `pi-coding-agent/src/interactive.rs`

`interactive_app_config` 打开 `composer_border: true` —— 用户看到的就是这个模式。

## 3. 证据（真 PTY + 真二进制）

新场景 `scripts/pty_scenarios/lum1328b-composer-border.json`（76×26，6 面板）：

```console
$ python3 scripts/pty_capture.py --bin target/debug/pi \
    --steps scripts/pty_scenarios/lum1328b-composer-border.json \
    --out docs/screenshots/lum1328b-composer-border.png
assertions: 11 checks over 6 panels — 11 PASS, 0 FAIL, 0 XFAIL, 0 XPASS
```

面板：① 空输入 = 规则/占位行/规则；② 有草稿时两条规则仍在；③ 10 行草稿超出窗口 → 顶规则
` ↑ N more `（上游措辞）；④ 窗口走到顶部 → 底规则 ` ↓ N more `；⑤ `!ls`（bash 模式，整框换色，
颜色在 PNG 里）;⑥ `Ctrl+R` 反查行位于两条规则之间。

## 4. 门禁

| 门 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy -p pi-tui -p pi-coding-agent --all-targets -- -D warnings` | 通过 |
| `cargo test -p pi-tui --lib` | **402 passed / 0 failed** |
| `cargo test -p pi-tui --test composer_paging --test composer_multiline --test composer_paste` | 8 + 4 + 23 passed |
| PTY 场景（本轮复跑） | 见 §5 |

## 5. 既有 PTY 场景的复跑结果（诚实条目）

边框让**转录区少 2 行**（这是上游同样的代价：它的编辑器渲染 `1 + N + 1` 行）。
本轮复跑了 54 个场景里的 30 个：

* **改后仍全绿**（含本轮为之更新过的）：
  `lum1328-paste` 17/17、`lum1328b-composer-border` 11/11、`lum1312-full-tui` 7/7、
  `lum1310-interaction-overview` 16/16、`lum1305-autocomplete` 21/21、`lum1274-scoped-models` 28/28、
  `lum1282` 7/7、`lum1308-external-editor` 5/5、`lum1310-startup-ui-keys`、`lum1259-small-terminal`、
  `lum1260-small-terminal-23`、`lum1266-short-22`、`lum1266-short-23`、`lum1260-tip-interaction`、
  `lum1273-truncated-above` 7/7、`lum1293-current-state`、`lum1307-startup-*`、`lum1312-chatinput-multiline`
  17/17+2 XFAIL、`extension-events`、`input-surface`、`interaction`、`lum1245`、`lum1257`、
  `lum1259-*`、`lum1260-small-terminal-24`、`lum1261-*`、`lum1266-longline`、`lum1266-tall-34`、
  `thinking-*`、`turn-and-commands`、`lum1319-history-{session-a,session-b,advertise}`
  （**4/4, 15/15, 11/11**，必须按 A→B→C 顺序共用 `--home` 跑）。
* **为本轮的几何变化更新了 4 个场景**（改动都在场景文件里，逐条写明原因）：
  `lum1327-composer-click`（点击 y 坐标整体上移 1 行，因为草稿行下移了；13/13）、
  `lum1317-composer-paging`（`↑4row 05` 这套标签列探针换成规则上的 ` ↑ 4 more ` + 草稿行两条；32/32）、
  `lum1298-multi-line-and-model`（34→36 行，保持转录区行数不变；12 PASS/1 XFAIL）、
  `lum1267-narrow-composer`（23→25 行；`slash commands:` 换成本体的 `keys:` + `PgUp/PgDn`）。
* **仍然红、本轮没修完的**（诚实列出，均为"固定高度窗口的内容掉出折叠"或旧探针过期，未逐条核到根因）：
  `lum1267-interaction-assertions` 43 PASS / 15 FAIL + 2 XFAIL、
  `lum1274-scoped-models-seed` 7/12、`lum1306-reload` 9/11、`lum1306-help-reload` 2/3、
  `lum1310-quiet-startup-on` 1 PASS/2 FAIL + 3 XFAIL、`lum1271-help-complete` 39/40。
  其中 `lum1271` 唯一失败项是 `/help` 里已不存在的旧文案
  （`Ctrl+U clear the prompt buffer`，后来几轮把它改成了 `Ctrl+U: to its start`），与边框无关；
  `lum1267-narrow-composer` 那条 `slash commands:` 也是同一类过期探针（把行数加回边框前的
  转录高度后依然在折叠线之上，说明它在本轮之前就已经过期）。
  **剩余的这些没有在本轮改绿，也没有被当作"通过"。**
* 没有复跑的 24 个场景（`*-baseline.json` 这类 A/B 历史产物、`lum1311`/`lum1261-interaction` 等）
  本轮未验证。

## 6. 已知限制与后续

1. **`AppConfig::composer_border` 默认关闭，交互模式打开。** 原因：边框让 crate 里 **71 条**
   几何断言（转录区少 2 行）失效 —— 这是本轮实测出来的数字，不是估计。把这些断言按新几何重写
   是一次独立的布局迁移，混进本轮会让"改了输入框"变成"改了全 crate 的排版"。默认关闭时
   那 71 条的含义不变；交互模式（用户真正看到的地方）有边框。**把默认翻过来是明确的后续任务**，
   连同上面 §5 剩余的 PTY 场景一起做。
2. 转录区代价 2 行（上游同样）；`/hotkeys`、`/help` 这类长窗口因此少 2 行内容（§5 的红色集合）。
   若要让它们不掉行，正解是压缩文案（LUM-1319 §3.4 用过同一手法），不是去掉边框。
3. `> ` 标签仍保留在草稿行（上游没有这个 gutter；附图里光标在 col 0）。保留是因为 40+ 场景与
   `docs/*` 都按 `> ` 断言；去掉它是一个独立的视觉决定。
4. 边框行不吃宽：`body_width` 不变（上游也没有左右边框），所以换行宽度与光标几何全部不变。
