# TUI 输入面与布局的独立复核（LUM-1259）

**被测对象（两个 tip，本轮 `feature/pi.rs` 前移过一次）**

| tip | 说明 |
| --- | --- |
| `4b187b418` | 本轮开始时的 `origin/feature/pi.rs`（LUM-1257 合入后） |
| `b5769b585` | 本轮中途 LUM-1255 推上来的合并点：`e25c45c78`(LUM-1238 Stage 70)、`76dfbd9b0`(LUM-1246 Stage 68)、`4d9333bad`(LUM-1239)、`d298ecb74`(LUM-1255 Stage 68)、LUM-1173/1177 镜像 |

基线在中途变了，所以本文**每一项都标了测的是哪个 tip**——这恰好是本轮最有价值的部分：
第十二节 12.4 的两条输入面缺口里，一条已被赶到的修复**真正修好**（我用进程级探针独立验过），
另一条**只修了一半**（前缀变了，排版还是坏的），而修复提交自己的测试看不见剩下那一半。

本文只做测量与复核，不改产品代码。所有命令可复现；测不了的项在 §1.4 明确列出。

---

## 1. `Ctrl+C`：从"一次误触就退出"到"500 ms 双击窗口"（§12.4 第一项）

### 1.1 测量方法

这一项只能靠**进程级事实**判定——画面还在不在，证明不了进程死活。本轮新增
`pi-rust/scripts/pty_probe_ctrl_c.py`：`pty.openpty()` + `os.fork()` 起一个真 PTY，
按键后用 `waitpid(pid, WNOHANG)` 直接问内核"进程还活着吗"，并记录每帧 composer 的内容。

```bash
python3 pi-rust/scripts/pty_probe_ctrl_c.py --bin pi-rust/target/debug/pi
```

（`scripts/pty_capture.py` 只渲染画面，回答不了"是否已退出"；两套工具都在仓库里，下一轮可直接复用。）

### 1.2 旧 tip `4b187b418`：一次 `Ctrl+C` 必定退出，草稿必丢

| 用例 | 操作 | 结果 |
| --- | --- | --- |
| A | 草稿 + **1×** `Ctrl+C` | `alive=False` |
| B | 草稿 + 2× `Ctrl+C`（窗口内） | `alive=False`（第 1 次就已退出） |
| C | 空 composer + **1×** `Ctrl+C` | `alive=False` |
| D | 空 composer + 2× `Ctrl+C` | `alive=False` |
| E | 空 composer + `Ctrl+D` | `alive=False` |

即：**有草稿时同样一次就退**，没有任何双击窗口，而 `Ctrl+C` 恰是终端里最容易误触的键；
启动头的 `Ctrl+C to clear` / `Ctrl+C twice to exit` 与 `/hotkeys` 的
`Ctrl+C clear the prompt (twice: exit)` 三个广告面在那一刻全是假话。

### 1.3 新 tip `b5769b585`：修好了，5 例全过

LUM-1238 的 `e25c45c78` 被救回来后，同一个探针在新二进位上重跑：

| 用例 | 操作 | 旧 tip | 新 tip | 判定 |
| --- | --- | --- | --- | --- |
| A | 草稿 + 1× `Ctrl+C` | `alive=False` | **`alive=True`**，composer 回到占位符 | ✅ |
| B | 草稿 + 2× `Ctrl+C`（窗口内） | `alive=False` | **`alive=False`** | ✅ |
| C | 空 + 1× `Ctrl+C` | `alive=False` | **`alive=True`** | ✅ |
| D | 空 + 2× `Ctrl+C` | `alive=False` | **`alive=False`** | ✅ |
| E | 空 + `Ctrl+D` | `alive=False` | **`alive=False`** | ✅ |

```text
== case A: draft in composer, one Ctrl+C ==
   after typing:     alive=True  composer='> this draft must not be lost▍'
   after ONE Ctrl+C: alive=True  composer='> type a prompt — /help for commands'
== case B: draft in composer, two Ctrl+C inside the double-press window ==
   after Ctrl+C #1: alive=True | after Ctrl+C #2: alive=False
```

截图 `docs/screenshots/lum1259-ctrl-c-draft-loss.png`（面板 3＝第一次按键后：进程还在、草稿被清；
面板 4＝快速两次：进程退出、屏幕复位）。

### 1.4 与上游对齐的细节，以及一处仍在的偏差

- 500 ms 窗口与"第一次按键清草稿但不退出"**与上游一致**：上游 `handleCtrlC`
  （`packages/coding-agent/src/modes/interactive/interactive-mode.ts:3931-3939`）判断
  `now - lastSigintTime < 500` 后 `shutdown()`，否则 `clearEditor()`；而 `clearEditor()`
  （同文件 `:4268-4271`）就是 `this.editor.setText("")`——**草稿文本在上游同样被丢弃**。
  所以"第一次 `Ctrl+C` 丢草稿"是上游语义，不是本端口的缺陷，后续不必再当缺口追。
- **仍有一处偏差（建议记入偏差清单）**：本端口在忙时用 `Ctrl+C` 取消回轮
  （`pi-tui/src/app.rs:2554-2561`：`if self.is_busy() { self.cancel(); … }`），而上游 `app.clear`
  只清编辑器、不取消回轮；上游取消回轮走 `app.interrupt`，默认键是 **`escape`**
  （`packages/coding-agent/src/core/keybindings.ts:93`）。提交信息写着"忙时仍 `cancel()`"，是有意的，
  但与上游语义不同，`/help` 的 `Ctrl+C abort the current turn (or exit on idle)` 一行正是这条偏差的自述。
  **本轮未做行为级验证**：faux 提供方回轮太快，构造不出稳定的 busy 态。

---

## 2. `/help`：前缀修好了，**排版还是坏的**（§12.4 第二项，只修了一半）

### 2.1 旧 tip `4b187b418`：整块帮助与用户输入共用 `> ` 前缀

`docs/screenshots/lum1259-tip-interaction.png` 面板 7（120×34）里 `/help` 的 11 行**全部以 `> ` 开头**，
而 `> ` 正是用户输入的角色前缀（同一屏第 16 行就是真输入 `> hello`），两者无法区分。

### 2.2 新 tip `b5769b585`：前缀变成 `· `，但正文仍被压成一段

同一个场景在新二进位上重抓（同一份 PNG 路径）：

```text
· slash commands: /help show this help text /clear clear the message view /new start a new session /copy copy the last │
· assistant message to the clipboard /name [name] show or set the session display name /model pick a model (opens      │
```

`Role::Info` + `· ` 确实生效了（面板 7 里 `· ` 开头的行 11 行，`> ` 只剩 composer 那一行）。
但 `help_text()`（`pi-coding-agent/src/commands/slash.rs:152`）本来是**排好版的**（18 条命令行 + 9 条键位行、
两空格缩进、列对齐），屏幕上这些**换行与缩进仍然全部消失**，命令和描述粘成一段连续文字。

### 2.3 可执行复现（不需要 PTY，两个 tip 上同样失败）

```bash
cargo test --offline -p pi-tui --test lum1259_info_block_lines -- --ignored
```

本轮新增的 `crates/pi-tui/tests/lum1259_info_block_lines.rs` 是**故意 `#[ignore]` 的验收测试**
（门禁保持绿，`--ignored` 才跑）。它在两个 tip 上给出同一段输出：

```text
a 6-row command reference collapsed into 2 row(s):
[
    "> slash commands: /help show this help text /clear clear the message view keys: Enter submit prompt",
    "> Ctrl+C abort the current turn (or exit on idle)",
]
```

6 个源行（含 1 个空行）→ 2 行，连空行分隔也被吃掉（`message view` 与 `keys:` 粘在一起）。

### 2.4 根因链（精确到行；行号取新 tip）

1. `pi-tui/src/message.rs:880 push_info()`：把系统输出存成 **`role: Role::User`**。
   `/help`、`/hotkeys`、`/session`……以及扩展命令的输出都走 `app.info()`
   （`pi-coding-agent/src/interactive.rs:1899` 起：`SlashCommand::Help => { … app.info(help); }`）。
2. `message.rs:1079 item_lines()`：`Role::User` → 前缀 `> `；`prefix_styled_lines()`（`message.rs:1375`）
   给**每一行折行结果**都加上这个前缀。
3. `message.rs:1399 plain_lines()` → `wrap_text()`（`message.rs:1494`）→ `split_words()`，后者的注释自陈
   「The wrap function joins the runs with a single ASCII space」：**按空白切词再重排**，
   于是 `\n` 与行首缩进一律丢失。

`e25c45c78` 新增的 `Role::Info`（新 tip 上 `message.rs:1115-1122` 的 `"· "` 分支）**走的仍是 `plain_lines`**：
该提交没有碰 `wrap_text` / `plain_lines`（`git show e25c45c78 -- pi-rust/crates/pi-tui/src/message.rs` 里
搜不到这两个名字），而它自己的新测试把断言写在**单行**文本上
（`app.info_block("/help    show this help text")`，只查 `· /help` 在、`> /help` 不在），**看不见重排**。

### 2.5 剩下这一半的完成标准（下一轮可直接照做）

- 期望行为：命令参考块**按源行**渲染（一个源行至少一个渲染行），缩进与列对齐保留，空行保留分隔。
  实现方向：给 `Role::Info`（或所有非流式正文）在 `plain_lines` 之前按 `\n` 预切段，段内再 `wrap_text`；
  或给 `Role::Info` 单开一条保留换行的渲染分支。
- 验收：§2.3 那条测试转绿后去掉 `#[ignore]`，即成为门禁；再补一条用**真实** `help_text()`
  （而不是单行 fixture）渲染的断言。

---

## 3. 标准 80×24：默认布局不可用，两条 tip 上都没变

`docs/screenshots/lum1259-small-terminal.png`（`scripts/pty_scenarios/lum1259-small-terminal.json`）

| 面板 | 场景 | 实测（新 tip） |
| --- | --- | --- |
| 1 | 80×24 启动帧 | 行 0–18 是 19 行键位说明，行 20 是欢迎语（本身被右边界裁断：`… Ask it how to use or exten`），行 21 是**唯一的**转写行，行 22 composer，行 23 状态栏 |
| 2 | `/help` | 帮助正文只挤出**一行**并被裁断 |
| 3 | 真跑一轮 `hello` | 整个对话只剩**一行**：`  faux-model (faux) hello` |
| 4 | `Alt+H`（启动头自己广告的逃生口） | 收起成功，转写区从 1 行变 19 行 |

**21/24 行（87.5%）被开机键位说明占用**，默认观感就是"界面只剩一行内容"。`Alt+H` 有效，
但没有按终端尺寸自动收起的逻辑。建议：高度低于阈值（如 < 30 行）时默认收起，只留
`Startup header: collapsed (Alt+H to expand)`——状态栏已经写着这句话（面板 4 末尾可见 `Startup header: co…`）。

### 3.1 顺带核到的一件事：jump-to-latest 药丸**不是**缺陷

新 tip 上滚到中段时，底部转写行的中间会被药丸盖掉一段（实测：120 列里占 **45–77 列**，共 33 个字符格，
被覆盖的字符被清空）：

```text
· Ctrl+L open the model selector Alt+N start a new session Alt+T open the session tree Alt+F fork a session from a ┃
> type a prompt — /help for commands
```

这在**上游是同样的设计**：`packages/tui/src/tui-alt-screen.ts:1617-1634` 的
`compositeScrollToEndIndicator` 就是"居中（`Math.floor((availableWidth - textWidth) / 2)`）覆盖在裁剪区最后一行上"，
上游自己的注释也写着「centered on the last row of a follow-end [clip]」。所以**不计为缺口**，
记在这里只是为了下次不重复怀疑它。

---

## 4. 复核既有百分比：两处口径偏高，`tui.*` 那条口径本身不成立

### 4.1 `app.*` 接线率 19/44 = 43.2%，不是 21/44 = 47.7%

`docs/RUST_TS_PARITY_METRICS.md` §3.5 记"真正被消费的 21 个"，多出来的两个正是 `app.suspend`
与 `app.editor.external`——它们只出现在 `pi-tui/src/app.rs:113-117` 的**模块注释**里：那段注释把这两个 id
连同 `app.tree.*` / `app.models.*` 一起归入「**no consumer** in this port yet and are deliberately not
implemented here」；`pi-tui/src/keybindings.rs:395` 重申它们 "are still unhonoured here"；
`pi-coding-agent/tests/startup_header.rs:45` 的 `KNOWN_UNWIRED` 更是直接列了这两个名字。
按 `CONSUMED_APP_ACTIONS`（`pi-tui/src/keybindings.rs:408`，19 条）独立计数得 **19/44 = 43.2%**，
与 `TUI_UX_AUDIT.md` §18.6 一致。**§3.5 的 21/44 是注释造成的假阳性。**

### 4.2 `/` 命令面 15/23 = 65.2%，不是 17/23 = 73.9%

TS 侧内置命令 23 个（`packages/coding-agent/src/core/slash-commands.ts`）。Rust 侧
`AUTOCOMPLETE_COMMANDS`（`pi-coding-agent/src/commands/slash.rs:195`）恰好 17 条，但其中 `help`/`clear`
是 Rust 自有（TS 没有），且 TS 的 `quit` 对应 Rust 的 `exit`。同源比较：交集 14 + `exit≡quit` =
**15/23 = 65.2%**，缺 8 个（`changelog import thinking scoped-models share login logout reload`）。
§3.3 的 17/23 是**分子分母不同源**（把自己多出来的 2 条算进了 TS 的分母）。

### 4.3 `tui.*` 的 id 串计数不是有效口径

组件层的分发不认 id 字符串。反例（`pi-tui/src/selector.rs:465`）：

```rust
KeyCode::PageUp => self.page_up(),
```

选择器翻页按 `KeyCode` 直接匹配。于是同一份代码换三种排除集就得到 81% / 87.2% / 70.2% 三个数，
而 `tui.select.pageUp`、`tui.select.confirm` 这类**确实在用**的键位会被判成"未接线"。
建议 `tui.*` 那一格不要引用精确百分比，或改写成"有绑定的 id 是否存在按 Chord/KeyCode 的处理分支"的静态检查
（本轮未实现）。

### 4.4 修正后的加权总分 ≈ **75.0%**（原 76.05%）

用文档自己的权重，只把上面两处被证伪的输入改回实测值：

- 轴 5（TUI 交互面，权重 14%）：原文 58%，其输入是"模块 80.5% / 接线 47.7%"。由 58 反解接线权重
  w = (80.5−58)/(80.5−47.7) = 0.686；接线改 43.2% 后 = 0.314×80.5 + 0.686×43.2 = **54.9%**。
- 轴 7（slash 命令面，权重 7%）：73.9% → **65.2%**。

Δ = 14×(54.9−58)/100 + 7×(65.2−73.9)/100 = **−1.04pt** → 76.05 − 1.04 = **75.0%**。

口径不变的部分（代码规模 82.5%、测试用例 42.0%）复核无误。三个数字都对，取决于问的是
"搬了多少代码 / 测了多少 / 功能能用多少"。**诚实结论：功能面加权完成度 ≈75%，TUI 交互面 ≈55%，
扩展生命周期事件仍是最大的单一摆动项。需要精确数字时请同时报三个口径，不要只报加权值。

---

## 5. 本轮门禁（独立运行）

| 检查 | 旧 tip `4b187b418` | 新 tip `b5769b585` |
| --- | --- | --- |
| `cargo test --offline -p pi-tui` | ✅ 43 target / **769 passed / 0 failed** | ✅ 45 target / **790 passed / 0 failed / 1 ignored** |
| `cargo test -p pi-tui --test lum1259_info_block_lines` | ✅ 门禁绿（1 ignored） | ✅ 门禁绿（1 ignored） |
| 同上加 `--ignored` | ❌ 1 failed（有意） | ❌ 1 failed（有意，钉住 §2 的缺口） |
| `python3 scripts/pty_probe_ctrl_c.py` | 5/5 退出（缺口） | **5/5 符合预期**（§1.3） |
| `cargo build --offline -p pi-coding-agent --bin pi` | — | ✅ 2m20s（warm target，CPU 被三个并发 run 分走） |

新 tip 的 790 是**独立复跑**：45 个 target 全绿，说明 LUM-1238 / LUM-1246 / LUM-1239 / LUM-1255
那一批抢救合并没有把 `pi-tui` 弄坏（旧 tip 43 target / 769）。

---

## 6. 本轮没有新代码合入的原因（协调事实）

- **并发槽位 3/3 满**：`/proc/<pid>/cwd` 复核，活着的是 LUM-1255（pid 24336）、LUM-1256（pid 39335）、
  LUM-1258（pid 51019）。按“最多 3 个并发”的约定，本轮**零派发**、也不抢开发位。
- **两条 run 在抢救同一批分支**：LUM-1255 与 LUM-1256 都在合并 Stage 68/70/71 的未合入交付，
  并一度停在**同一批文件**的未解决冲突上（`UU pi-tui/src/app.rs`、`UU docs/TUI_UX_AUDIT.md`）。
  第三条合并只会造出第三次冲突，所以本轮不碰 `app.rs` 与 `TUI_UX_AUDIT.md`，
  并且把发现写进**新文档**而不是往那份被争用的审计文档里追加。
- **本轮实际发生的事**：基线从 `4b187b418` 前移到 `b5769b585`（LUM-1255 把 LUM-1238/1246/1239 的
  抢救合并推了上来），于是本轮的验证重心从“复现缺口”变成“**验修复**”：
  Ctrl+C 那半条从“确认缺陷”翻成“确认已修”（§1.3），`/help` 那半条从“前缀撞车”变成
  “前缀已修、排版仍未修”（§2.2/§2.4）。这两件事都是原提交自己的测试看不见的。
- **死 run 留下的脏状态**：LUM-1252 的分支**零提交**（问题却还挂在 `in_progress`）；
  磁盘曾被打满到 **0 字节**（`git checkout` 报 No space left on device），根因是三个并发全量构建
  各占 3–7 GB。本轮删掉死 run `lum-1252` 的 `pi-rust/target`（2.9 GB），并把 LUM-1257 那份
  可复用的 6.6 GB warm `target/` 挪进本轮工作区，使本轮所有构建都在 `--offline` 下完成。
- 本轮交付物 = 本文 + 1 个进程级探针 + 3 个 PTY 场景（均重抓）+ 1 条 `#[ignore]` 验收测试 + 3 张截图，
  与正在冲突的文件**零交集**。
---

## 7. LUM-1261 收尾：§2.5 的两条验收标准达成，§3 的"输入框盲打"关闭（tip `c8bc6785a` + 本轮提交）

### 7.1 `/help` 排版：`wrap_text` 改成换行感知（§2.5 两条标准均达成）

改动全部在 `pi-rust/crates/pi-tui/src/message.rs`（本轮之前**没有任何在飞 run 碰过这个文件**）：

| 符号 | 行 | 行为 |
| --- | --- | --- |
| `split_hard_lines()` | `message.rs:1533` | 只按 `\r\n` / `\r` / `\n` 切一次，得到源行 |
| `wrap_text()` | `message.rs:1570` | 改为**逐源行**包装（空输入 → `[""]`），不再把整块正文当一段重排 |
| `wrap_single_line()` | `message.rs:1591` | **原样快路径**：`display_width(line) <= width` 直接返回该行（缩进与列对齐原样保住）；只有超宽行才落回贪心折行；整行空白且超宽 → `[""]` |
| `wrap_words()` | `message.rs:1620` | 旧的贪心折行算法，改名保留，供超宽行使用 |

与上游契约一致：`packages/tui/src/utils.ts:843-866` 的 `wrapTextWithAnsi`（先按 `/\r\n|\r|\n/` 切、再逐行包）与
`utils.ts:873-876` 的 `wrapSingleLine`（`visibleWidth <= width` 时原样返回）。

§2.5 的两条验收标准，逐条落实：

1. **§2.3 的复现转绿并去掉 `#[ignore]`** → `pi-rust/crates/pi-tui/tests/lum1259_info_block_lines.rs`
   （`a_command_reference_block_keeps_its_line_structure`、`an_info_block_renders_one_row_per_source_row`，2 passed）。
2. **补一条用真实 `help_text()` 的断言** → 新增 `pi-rust/crates/pi-coding-agent/tests/help_text_layout.rs`
   （3 passed：32 行真实 `/help` 图例逐行成行、`hotkeys_text()` 逐行成行、80 列下"有界折行且不溢出"）。
   测试读的是 `commands::slash::{help_text, hotkeys_text}` 本体（`slash.rs:404`），不是另写的 fixture。

PTY 实拍（120×50，同一 `lum1259-tip-interaction` / `lum1261-help-layout` 场景）：

```text
# before（tip b5769b585）docs/screenshots/lum1259-tip-interaction.png.txt:205
· slash commands: /help show this help text /clear clear the message view /new start a new session /copy copy the last
# after （tip c8bc6785a+）docs/screenshots/lum1261-help-layout.png.txt:129 / :143 / :150
· slash commands:
·   /thinking set the reasoning level (/thinking off|minimal|low|medium|high|xhigh|max)
· keys:
·   Enter       submit prompt
```

### 7.2 ≤23 行终端：启动头不再把输入框挤出屏幕（§3 + LUM-1260 §3.1 的第 1 条）

**根因**（不是渲染器丢行，是行预算的发放顺序）：`pi-rust/crates/pi-tui/src/extension_ui.rs:415`
的 `plan_chrome` 按渲染顺序发预算——**header 第一、editor 最后**。默认启动头是 **21 行**，
而 `total = 23` 时预算只有 `23 - 1(状态栏) - 1(转写行) = 21`，于是 header 吃光全部预算、
`editor == 0`：输入框被画进零高度区域，用户**盲打**（120×22 / 120×23 实测草稿一个字都不在屏上）。

**修法**：`plan_chrome` **先预留 editor 那一行**（提示行是用户唯一不能没有的区域，而启动头自带
`Alt+H` 逃生口），剩下的再按 header → Above → Below → footer 顺序发。仍然保留"截尾"策略，
被截的是 header 的**尾行**（先丢 onboarding 行、再丢末尾几条键位，标题与前几条键位保留；
`paint_extension_lines` 本来就只画前 `rect.height` 行）。上游是 flex 布局、被压缩的永远是
可伸缩的转写区而不是固定高度的编辑区，这条修法与之同向。

**实测**：同一场景 120×23，前=`docs/screenshots/lum1260-small-terminal-23.png.txt`（tip `b5769b585`），
后=`docs/screenshots/lum1261-small-terminal-23.png(.txt)`（tip `c8bc6785a`，场景文件
`scripts/pty_scenarios/lum1261-small-terminal-23.json`）：

| 120×23 帧 | before（`b5769b585`） | after（`c8bc6785a`+） |
| --- | --- | --- |
| 行 1–19 | 标题 + 18 条键位 | 标题 + 18 条键位（同上） |
| 行 20 / 21 / 22 | 空行 / onboarding 行 / 空行 | 空行 / 转写行 / **`> type a prompt — /help for commands`** |
| 行 23 | 状态栏 | 状态栏 |
| 草稿 `typed blind` | **不出现**（`grep -c` = 0） | 行 22 `> typed blind▍` |
| `Alt+H` | 收起成功 | 收起成功，状态栏显示 `Startup header: collapsed (Alt+H to show)`，草稿仍在 |

行数预算是可复核的：`total=24` 时 `(header, editor, message, status) = (21, 1, 1, 1)`（与旧策略完全一致，24 行以上零行为变化）；
`total=23/22` 时 header 被截到 20/19 行、editor 与 message 各保住 1 行。回归测试写在
`extension_ui.rs:758`（`the_startup_header_cannot_starve_the_prompt`：21 行启动头 + 22/23/24 三档全断言），
`extension_ui.rs:739` 的旧用例按新策略更新（它原来断言"header 独占 3 行、prompt 什么都拿不到"）。

### 7.3 本轮门禁（独立运行）

| 命令 | 结果 |
| --- | --- |
| `cargo test --offline -p pi-tui` | **798 passed / 0 failed**，45 个 test target 全部有结果（含 lib 358 条） |
| `cargo test --offline -p pi-coding-agent --test startup_header --test help_text_layout --test keybindings --test print_mode --test tools_render --test builtin_tool_factories` | **66 passed / 0 failed** |
| `cargo test --offline -p pi-coding-agent --test extension_ui` | 7 passed（但见下条 flaky） |
| `cargo test --offline --workspace` | **仍无法完成**（磁盘），与上轮同一原因，逐字错误：`error: linking with \`cc\` failed: exit status: 1` / `error: couldn't create a temp dir: No space left on device (os error 28)` → `could not compile \`pi-coding-agent\` (test "tools_render")`。50G 共享卷同时有 4–5 条 run 在构建，本轮两次打满到 0 字节，**不是代码问题**。 |

**一条既有 flaky（不是本轮引入）**：`pi-coding-agent --test extension_ui` 的
`interactive_regions_render_into_the_app` 实测 20 次里失败 1 次，失败信息是
`EDITOR missing from [...]`（帧里还是默认 prompt、`FOOTER` 也没到）——它在该 target 首次提交
`d02fb0ace`（LUM-1190）就在，是 `pump()` 投递与渲染之间的异步竞态；与布局预算无关：
40×12 下新旧 `plan_chrome` 的分配**完全相同**（每个区域只占 1 行、预算 10 行且无截断）。

### 7.4 本轮**没**做的（诚实条目）

1. **低于 24 行时自动收起启动头**（§3 的建议）仍未实现。现在不是靠"收起"，而是靠"截尾 + 保住提示行"
   让输入框可见；`total >= 24` 时启动头依旧占满 21 行，80×24 的"界面只剩一行内容"观感没变。
   这条要动 `app.rs`（渲染前按尺寸决定 `header_expanded`），本轮 3 条在飞 run 正在改 `app.rs`，故意不碰。
   > **LUM-1266 已关闭本条**（tip `e4db5cb82`）：内置启动头在 `expanded + HEADER_RESERVED_ROWS > total` 时
   > 自动折叠成"标题行 + 一行 `hints hidden on a short terminal — Alt+H shows them`"（`app.rs::builtin_header_lines`，
   > 常量 `MIN_TRANSCRIPT_ROWS` / `RESERVED_CHROME_ROWS`，文案 `locale.rs::header_folded_line`）。
   > 120×22 的聊天区从 0–1 行回到 17 行，120×23 下 `/help` 与 `/hotkeys` 不再渲染成同一帧；
   > 高终端逐行不变。`ctx.ui.setHeader` 的扩展 header **仍然截尾**，不替扩展做取舍。
   > 细节与 A/B 帧见 `docs/TUI_SHORT_VIEWPORT_AND_SCROLLBAR_LUM1266.md`。
2. **80 列时超宽行的内部列对齐仍会丢**：列宽预算不够时 `Ctrl+C abort the current turn (…)` 这类行会折成两行、
   第二行从正文列 0 起排。上游同款行为（先按空白折叠再包），保留为已知的外观偏差，不做 Rust 特有的增强。
3. **`plan_chrome` 的新顺序改了扩展区域的观测行为**：当扩展 header/Above/Below/footer 的总需求超过终端高度时，
   现在优先保证输入框，其次 header，其余截尾。这是**策略变更**，影响面已由 `pi-tui/tests/extension_ui.rs`
   与 pi-tui 全量用例覆盖（798 条全绿）。
