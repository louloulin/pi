# TUI 输入面与布局的独立复核（LUM-1259）

> 被测 tip：`origin/feature/pi.rs` = **`4b187b418`**（LUM-1257 合入后的最新点）
> 二进制：`pi-rust/target/debug/pi`（由该 tip 构建）
> 环境：Linux x86_64 / 32 核 / rustc 1.98.1 / `--offline`；PTY 捕获与探针见 §1.1
> 本文只做**测量与复核**，不改产品代码。所有命令可复现，无法测的项在 §4.4 明确列出。

本文回答三个问题：**（1）第十二节 12.4 的输入面缺口在最新 tip 上是否还在；（2）TUI 在标准尺寸终端上
能不能用；（3）既有审计/指标文档里的百分比是否经得起复核。**

---

## 1. `Ctrl+C` 的语义：一次按下就退出，草稿必然丢失（§12.4 第一项，确认存在）

### 1.1 测量方法

`docs/TUI_UX_AUDIT.md` §12.4 的这一项只能靠**进程级**事实判定——"画面还在不在"无法证明退出。
新提交的 `pi-rust/scripts/pty_probe_ctrl_c.py` 用 `pty.openpty` + `os.fork` 起真 PTY，
再用 `waitpid(WNOHANG)` 直接问内核"进程还活着吗"，并对每次按键后的字符网格快照逐帧记录：

```bash
python3 pi-rust/scripts/pty_probe_ctrl_c.py --bin pi-rust/target/debug/pi
```

（`pty_capture.py` 只能渲染画面，回答不了"是否已退出"；这个探针补的正是那一半。
两套工具都在 `scripts/` 下随仓库提交，下一轮可直接复用。）

### 1.2 实测结果（tip `4b187b418`）

| 用例 | 操作 | 进程状态 | 结论 |
| --- | --- | --- | --- |
| A | 草稿 `this draft must not be lost` + **1×** `Ctrl+C` | `alive=False` | 退出，草稿随进程消失 |
| B | 草稿 `draft two` + **2×** `Ctrl+C`（间隔 200 ms） | `alive=False` | 第 1 次就已经退出 |
| C | 空 composer + **1×** `Ctrl+C` | `alive=False` | 退出 |
| D | 空 composer + **2×** `Ctrl+C` | `alive=False` | 同上 |
| E | 空 composer + **1×** `Ctrl+D` | `alive=False` | 退出（与广告一致） |

原始输出（节选）：

```text
== case A: draft in composer, one Ctrl+C ==
   after typing: alive=True composer='> this draft must not be lost▍'
   after ONE Ctrl+C: alive=False composer='> this draft must not be lost▍'
== case B: draft in composer, two Ctrl+C inside the double-press window ==
   after Ctrl+C #1: alive=False composer='> draft two▍' | after Ctrl+C #2: alive=False
== case C: empty composer, one Ctrl+C ==
   after ONE Ctrl+C: alive=False
```

### 1.3 这条缺口的真实严重程度高于审计里的描述

审计 §12.4 写的是「空 composer 一次 `Ctrl+C` 直接退出、草稿丢失」。实测更糟：**有草稿时同样一次就退**，
没有任何"双击窗口"。而 `Ctrl+C` 恰好是终端里最容易被误触的键，所以实际后果是
**任何未提交的草稿都会被一次误触清掉**，且退出时草稿不落盘、不可恢复。

更糟的是**三个广告面都在说谎**（同一 tip 的启动头与 `/hotkeys` 明写着双击语义）：

| 广告面 | 原文 | 与实测的关系 |
| --- | --- | --- |
| 启动头 | `Ctrl+C to clear` / `Ctrl+C twice to exit` | 假：一次即退出 |
| `/help` 的 keys 段 | `Ctrl+C  abort the current turn (or exit on idle)` | 半真：idle 时不是 abort 而是立刻退出 |
| `/hotkeys` 的 app 段 | `Ctrl+C clear the prompt (twice: exit)` | 假：从不"clear then wait" |

### 1.4 修复草案**已经写好但没合进来**

`work/LUM-1238` 上有一条**未合并**的提交 `e25c45c78`，提交信息第 2 条正是这件事：
「`app.clear`（12.4）：改成上游 `handleCtrlC` 的 500 ms 双击窗口 —— 忙时仍 `cancel()`；idle 时第一次清草稿
并记时间，窗口内第二次退出。时钟经 `App::step_key_at(key, now)` 注入」。也就是说：

- `feature/pi.rs`（用户实际拿到的二进制）**仍然是一次退出**；
- 修复躺在一个**已经死掉的 run** 的工作区里，而且那个工作区停在未解决的合并冲突上（`UU pi-tui/src/app.rs`）；
- 因此本轮不需要再写一遍修复（会与正在抢救同一批分支的 LUM-1255 / LUM-1256 三方撞车），
  本轮的增量是**给它一份可机械判定的验收口径**：§1.2 的 A–E 五例全绿才算修好，
  其中 A 必须 `alive=True`（草稿保留）、B 必须 `alive=False`（窗口内第二次真的退出）。

---

## 2. `/help` 信息块：不是"前缀撞车"，是**整块被重排成一段**（§12.4 第二项，比审计描述严重）

### 2.1 PTY 实证

`docs/screenshots/lum1259-tip-interaction.png`（120×34，面板 7）与
`docs/screenshots/lum1259-small-terminal.png`（80×24，面板 4）的字符网格里，`/help` 输出是这样：

```text
> slash commands: /help show this help text /clear clear the message view /new start a new session /copy copy the last │
> assistant message to the clipboard /name [name] show or set the session display name /model pick a model (opens      │
> selector) /session show the current session info /export [path] export the session (HTML, or JSONL for a .jsonl path)│
...
> type a prompt — /help for commands
```

`/help` 的**每一行都带 `> ` 前缀**，而 `> ` 正是用户输入专用的角色前缀（下面第 16 行就是真输入 `> hello`）。
所以用户无法区分"我打的字"和"命令帮助"。但真正的损害比"前缀撞车"更重：
`help_text()`（`pi-coding-agent/src/commands/slash.rs:152`）本来是**排好版的**——

```text
slash commands:
  /help     show this help text
  /clear    clear the message view
...
keys:
  Enter       submit prompt
  Ctrl+C      abort the current turn (or exit on idle)
```

——18 条命令行 + 9 条键位行、两空格缩进、列对齐。屏幕上这些**换行和缩进全部消失**，
整块被压成一段连续文字，命令与描述粘在一起。

### 2.2 可执行复现（不需要 PTY）

```bash
cargo test --offline -p pi-tui --test lum1259_info_block_lines -- --ignored
```

`crates/pi-tui/tests/lum1259_info_block_lines.rs`（本轮新增，`#[ignore]` 的验收测试）用它自己的
断言打印出根因：

```text
a 6-row command reference collapsed into 2 row(s):
[
    "> slash commands: /help show this help text /clear clear the message view keys: Enter submit prompt",
    "> Ctrl+C abort the current turn (or exit on idle)",
]
```

6 个源行（含 1 个空行）→ 2 行，且**空行分隔也被吃掉**（`message view` 与 `keys:` 粘在同一行）。

### 2.3 根因链（精确到行）

1. `pi-tui/src/message.rs:880 push_info()`：把系统输出存成 **`role: Role::User`**。
   `/help`、`/hotkeys`、`/session`、`/trust`……以及扩展命令的输出**全部**走 `app.info()`（见
   `pi-coding-agent/src/interactive.rs:1899` 起的 `SlashCommand::Help => { … app.info(help); }`）。
2. `message.rs:1079 item_lines()`：`Role::User` → 前缀 `> `；再由 `prefix_styled_lines()`
   给**每一行折行结果**都加上这个前缀（所以 11–16 行全是 `> `）。
3. `message.rs:1399 plain_lines()` → `wrap_text()`（`message.rs:1494`）→ `split_words()`，
   后者的文档注释自己写着「The wrap function joins the runs with a single ASCII space」：
   **按空白切词再重排**，于是 `\n` 与行首缩进一律丢失，整块变成一段。

### 2.4 已写好但未合入的修复只修了第 2 步

`e25c45c78` 新增了 `Role::Info` + `MessageView::push_info_block` + `App::info_block`（`· ` 前缀），
`/help`/`/hotkeys` 改走它。但该提交**没有碰 `wrap_text` / `plain_lines`**（`git show e25c45c78 --
pi-rust/crates/pi-tui/src/message.rs` 里搜不到这两个名字），而 `Role::Info` 走的仍是
`plain_lines` 分支。它自己的新测试把断言写在了**单行**文本上（`app.info_block("/help    show this help text")`），
只检查 `· /help` 在、`> /help` 不在——**看不见重排**。

结论：那条修复合进来之后，`/help` 会从「看起来像我打的字」变成「看起来像一段奇怪的提示」，
**但排版仍然是坏的**。§2.2 的 `#[ignore]` 测试恰好补上这一半，期望行为是"源行结构必须保留"，
修复后去掉 `#[ignore]` 即可当作回归门禁。

---

## 3. 标准 80×24 终端：默认布局不可用（启动头吃掉 21/24 行）

`docs/screenshots/lum1259-small-terminal.png`（`scripts/pty_scenarios/lum1259-small-terminal.json`）：

| 面板 | 场景 | 实测 |
| --- | --- | --- |
| 1 | 80×24 启动帧 | 行 0–18 是键位说明，行 19 空，行 20 是欢迎语，行 21 是**唯一的**转写行，行 22 composer，行 23 状态栏 |
| 2 | `/help` | 帮助正文只挤出**一行**并被右边界裁断（`> selector / cancel turn`） |
| 3 | 真跑一轮 `hello` + Enter | 整个对话只剩**一行**：`  faux-model (faux) hello` |
| 4 | `Alt+H`（启动头自己广告的逃生口） | 启动头收起，转写区从 1 行变 19 行，`/help` 至少能读完 |

即：**80×24 下 21/24 行（87.5%）被开机键位说明占用**，默认观感是"界面只剩一行内容"。
`Alt+H` 确实有效（这是好事），但默认状态才是新用户看到的东西，且**没有任何按尺寸自动收起的逻辑**。
建议（下一轮可直接做）：终端高度低于阈值（如 < 30 行）时默认收起启动头，只留一行提示
`Startup header: collapsed (Alt+H to expand)`——状态栏已经有这句话的位置了。

---

## 4. 复核既有百分比：两处口径偏高，`tui.*` 那条口径本身不成立

### 4.1 `app.*` 接线率是 19/44，不是 21/44

`docs/RUST_TS_PARITY_METRICS.md` §3.5 写"真正被消费的 21 个"并列举 `suspend 5`、`editor.external 3`。
复核方法（去掉声明文件、广告面 `/hotkeys` 与 `STARTUP_HINTS`、以及测试文件，并剔除注释行）：

```bash
# 定义
grep -rhoE '"app\.[a-zA-Z.]+"' crates/pi-coding-agent/src/keybindings.rs | tr -d '"' | sort -u | wc -l   # 44
cargo test --offline -p pi-tui --test lum1259_info_block_lines   # 见 §5，门禁保持绿
```

结果：**19/44 = 43.2%**，与 `CONSUMED_APP_ACTIONS`（`pi-tui/src/keybindings.rs:408`，19 条）完全一致，
也与 `TUI_UX_AUDIT.md` §18.6 的 19/44 一致。多出来的那 2 个正是
`app.suspend` 与 `app.editor.external`——它们只出现在 `pi-tui/src/app.rs:113-117` 的**模块注释**里，
那段注释明确把这两个 id 连同 `app.tree.*` / `app.models.*` 一起归入「**no consumer** in this port yet and are
deliberately not implemented here」，`pi-tui/src/keybindings.rs:395` 也重申它们 "are still unhonoured here"，
`pi-coding-agent/tests/startup_header.rs:45` 的 `KNOWN_UNWIRED` 更直接把这两个名字列为未实现。
**即 §3.5 的 21/44 = 47.7% 是注释与广告面造成的假阳性。**

### 4.2 `/` 命令面是 15/23，不是 17/23

`packages/coding-agent/src/core/slash-commands.ts` 的 23 个内置命令为分母：
`settings model tree thinking scoped-models export import share copy name session changelog hotkeys fork clone trust
login logout new compact resume reload quit`。
Rust 侧 `AUTOCOMPLETE_COMMANDS`（`commands/slash.rs:195`）恰好 17 条，但其中 `help`/`clear` 是 Rust 自有
（TS 没有），且 TS 的 `quit` 对应 Rust 的 `exit`。按同一集合比较：
交集 14 + `exit≡quit` = **15/23 = 65.2%**，缺 8 个
（`changelog import thinking scoped-models share login logout reload`）。
§3.3 的 17/23 = 73.9% 是分子分母不同源（把自己多出来的 2 条算进了 TS 的分母）。

### 4.3 `tui.*` 的 id 串计数不是有效口径（81% / 87% / 70% 都是仪器噪声）

对 `tui.*` 用"id 字符串是否在别的文件出现"来数接线率**方向性错误**：组件层的分发根本不认 id 字符串。
反例（`pi-tui/src/selector.rs:465`）：

```rust
KeyCode::PageUp => self.page_up(),
```

选择器的翻页是按 `KeyCode` 直接匹配的。于是同一份代码换三种排除集就得到三个数
（81% / 87.2% / 70.2%），而 `tui.select.pageUp`、`tui.select.confirm` 这类被上游广告、
也确实在用的键位会被判成"未接线"。**建议**：`tui.*` 那一格不要引用精确百分比，
或者改成"有绑定的 id 里，是否存在按 Chord/KeyCode 的处理分支"的静态检查（本轮未实现）。

### 4.4 修正后的加权总分 ≈ **75.0%**（原 76.05%）

用文档自己的权重，只把上面两处被证伪的输入改回实测值：

- 轴 5（TUI 交互面，权重 14%）：原文 58%，其两个输入是"模块 80.5% / 接线 47.7%"。由 58 反解其接线权重
  w = (80.5−58)/(80.5−47.7) = 0.686；接线改为 43.2% 后 = 0.314×80.5 + 0.686×43.2 = **54.9%**
- 轴 7（slash 命令面，权重 7%）：73.9% → **65.2%**

Δ = 14×(54.9−58)/100 + 7×(65.2−73.9)/100 = −0.43 − 0.61 = **−1.04pt** → 76.05 − 1.04 = **75.0%**。

口径不变的部分（代码规模 82.5%、测试用例 42.0%）复核无误；`§1.3` 的"不可测项"清单也成立。
三个数字都对，取决于问的是"搬了多少代码 / 测了多少 / 功能能用多少"。**诚实结论：功能面加权完成度
≈ 75%，其中 TUI 交互面 55% 左右，扩展生命周期事件那 20% 仍是最大单一摆动项。**

---

## 5. 本轮门禁（tip `4b187b418`，独立运行）

| 检查 | 结果 |
| --- | --- |
| `cargo test --offline -p pi-tui` | ✅ **769 passed / 0 failed**，43 个 target（文档 §5 记的 758 是 LUM-1257 之前的数） |
| `cargo test --offline -p pi-tui --test lum1259_info_block_lines` | ✅ 门禁绿（1 ignored） |
| 同上加 `--ignored` | ❌ 1 failed —— 这是**有意**的验收测试，钉住 §2 的缺口 |
| 真实 PTY 抓帧 ×3 | `docs/screenshots/lum1259-{tip-interaction,ctrl-c-draft-loss,small-terminal}.png` + 同名 `.txt` 字符网格 |
| 进程级 Ctrl+C 探针 | `scripts/pty_probe_ctrl_c.py`，5 例全绿（§1.2） |

---

## 6. 本轮没有新代码合入的原因（协调事实）

- **并发槽位 3/3 满**：`/proc/<pid>/cwd` 复核，活着的是 LUM-1255（pid 24336）、LUM-1256（pid 39335）、
  LUM-1258（pid 51019）。按"最多 3 个并发"的约定，本轮**零派发**。
- **两条 run 正在抢救同一批分支**：LUM-1255 与 LUM-1256 都在合并 Stage 68/70/71 的未合入交付
  （LUM-1246 的 `76dfbd9b0`、LUM-1238 的 `e25c45c78` 等），且两边都停在**同一批文件**的未解决冲突上
  （`UU pi-tui/src/app.rs`、`UU docs/TUI_UX_AUDIT.md`）。第三条合并只会制造第三次冲突，
  所以本轮刻意只做"测量 + 验收口径 + 协调"，不动那两条工作树，也不碰 `app.rs` 与那份审计文档。
- **死 run 留下的脏状态**：LUM-1252 的分支**零提交**（问题却还挂在 `in_progress`）；LUM-1238 / LUM-1246
  的提交仍未合并；磁盘曾被打满到 **0 字节**（`git checkout` 报 No space left on device），
  根因是 3 个并发全量构建各占 3–7 GB。本轮删掉死 run `lum-1252` 的 `pi-rust/target`
  （2.9 GB）换来可用空间，另外把 LUM-1257 那份可直接复用的 6.6 GB warm `target/` 挪到本轮工作区，
  使本轮所有构建都在 `--offline` 下几秒内完成。
- 因此本轮交付物 = 本文 + 两个新 PTY 场景 + 一个进程级探针 + 一个 `#[ignore]` 验收测试 + 3 张截图，
  本轮新增文件与正在冲突的文件**零交集**。
