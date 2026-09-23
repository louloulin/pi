# LUM-1457 — Windows 真实 PTY 打通，并修掉它抓出的第一个缺陷：每次按键都执行两遍

> 基线：`feature/pi.rs` = `815b21d13`（LUM-1448 + LUM-1450 合并 tip）
> 环境：Windows 10 x86_64 / cargo+rustc 1.97.1 / `--offline` / Python 3.12 + `pywinpty` 3.0.5 + `pyte` 0.8.2
> 本轮改 `pi-tui`（`app.rs` + 1 个新测试文件 + 2 个既有测试的 `.unwrap()`）、
> `pi-coding-agent`（`interactive.rs` 一处调用点）、`scripts/`（ConPTY 后端 + 3 个场景 + 1 处共享渲染器增强）。
> 不碰 `pi-protocol` / `pi-ai` / `pi-agent-core` / `pi-extensions` / `pi-session` / 上游 TS（只读取证）。

## 0. 一句话结论

issue 里「截图展示整个 tui 是否实现类似 codex 和 pi tui 的交互」这条，在 Windows 上**过去做不到**：
`pty_capture.py` 只有 POSIX 后端（`pty.openpty` + `fcntl`），本机没有 `pty` 模块，所以此前每一个
Windows round 的截图都走「frame-buffer 通道」，报告里也都老实写着「证明几何与高亮，不证明按键时序」。
本轮把 **ConPTY 后端**（`scripts/pty_capture_win.py`）做出来并入库，第一次在 Windows 上跑真终端、
真按键、真重绘；**它立刻抓到一个 P0 缺陷**：Windows 控制台为每次按键发 **press + release 两条记录**，
端口两条都当输入处理，于是**每个按键都执行两遍**——打 `alpha` 出 `aallpphhaa`，一次 `Backspace`
删两个字符，一次 Enter 提交两次。已修（`translate_event` 丢弃 release），9 条新回归测试 +
真 ConPTY 前后对照（9 PASS vs 3 PASS/6 FAIL）+ 反向验证（去掉过滤 7/9 立刻红）全部落地。

加权完成度 **86.8%**（13 轴，公式与权重见 `RUST_TS_PARITY_METRICS.md` §4.1；本轮只动测试轴
2,711 → 2,720，加权 +0.01pt，一位小数不变）；纯代码规模 **91.5%**（140,063 / 153,106）；
TUI 交互+视觉轴 **90.3%**；`app.*` 接线 **43/44 = 97.7%**；`tui.*` 消费 **49/49**；
扩展生命周期事件 **36/36**。**没有把本轮说成「TUI 大改」**：本轮关掉的是一条谁都没测过的
**Windows 输入通路缺陷**和一个**证据通道缺口**，二者都不在那 13 个轴的任何一格里。

## 1. 为什么是这两件事（而不是再改一版 composer）

开工前把这个 issue 的四项要求逐条对到既有证据上：

| issue 要求 | 开工前的事实 | 本轮处置 |
|---|---|---|
| 「分析后续哪些功能可以制定计划并实现，最多 3 个任务」 | `docs/LUM1450_HINT_CHORDS.md` §7 已单子化 5 条缺口；LUM-1453（提示面残留）已在 backlog、LUM-1434（CLI flag）在办 | 见 §7：新立 3 条 backlog（不启动 run，避免超并发上限） |
| 「选择最佳方式，跳过还是计划和实现」 | `app.*` 只剩 `app.tree.editLabel` 一条 silent，但它是**跨 crate 缺组件**（会话模型里根本没有 label entry，见 §6.3），不是「补一个键位」 | 本轮**不硬做**，单子化派发；本轮做能闭环且可反证的 |
| 「截图展示整个 tui…像 codex / pi tui 的交互」 | Windows 无法跑真 PTY：所有截图都是 frame-buffer，**按键时序不可证** | **打通 ConPTY**（§2），并用它做真机对照（§4） |
| 「chatinput 改造参考 codex 和 Martty，修复问题」 | `docs/LUM1445_MODAL_POINTER.md` §7 已逐条对照过 Martty（tip `93e9231`）：Martty 的 composer 没有撤销栈 / 历史反查 / 粘贴折叠 / kill-ring，输入框不是鼠标目标；差距方向是**反的** | 不在 composer 语义上重复投资；改为**用真终端找新缺陷**——找到的正是 Windows 输入通路这一条（§3） |

一句话：这个 issue 的「chatinput 差距」在源码级已经被前几轮收敛完了，剩下的真实差距在
**运行期平台通路**上，而平台通路恰恰是「没有真 PTY 就永远测不到」的那一类。

## 2. 交付一：Windows ConPTY 捕获后端（`scripts/pty_capture_win.py`，新）

设计原则：**只重写 OS 相关的三个原语，其余全部复用** `pty_capture.py`：

| POSIX（既有） | Windows（本轮） |
|---|---|
| `spawn`（`os.fork` + `openpty` + `TIOCSCTTY`） | `ConPty.spawn`（`winpty.PtyProcess`，ConPTY） |
| `pump` / `drain`（`select` 在 master fd 上） | `ConPty.drain`（读线程 + 队列 + idle/hard-timeout，规则与 POSIX 相同） |
| `child_alive` / `reap`（`waitpid`） | `ConPty.is_alive` / `close` |

复用的部分（同一份代码，不是抄一份）：`encode_keys`（`<Enter>` / `<C-a>` token 语言）、
`Renderer`（pyte → PNG + 字符网格）、`snapshot`、`evaluate_panel` / `summarize_assertions`
（`expect` / `reject` / `probe` / `xfail` 断言 schema）、调色板与字体选择。
所以**同一个场景 JSON 两边都能跑**：`lum1360-chatinput-audit.json`（为 Linux 写的 14 面板场景）
本轮在 Windows 上原样跑通（§4.3）。

为了能在 Windows 上 import，`pty_capture.py` 做了一处最小改动：`fcntl` / `pty` / `termios`
改为**软 import**（Windows 没有这三个模块），并把字体候选表扩展为「Linux 等宽 → Windows
`consola`/`lucon`」，帧里含 CJK 时优先 CJK 字体（`msyh`/`simhei`/Noto）。这三处都不改变
POSIX 行为（Linux 路径仍在候选表首位）。

### 2.1 顺带修掉的捕获器缺陷：pyte 没有备用屏

第一次抓帧就抓到「两屏文字叠在一行」：

```text
pi v0.1.0ers\ADMINI~1\AppData\Local\Temp\...\pi-pty-cwd-1343
hints hidden on a short terminal — Alt+H shows themd.
```

原始字节流（`scripts/pty_capture_win.py` 的 dump 之外，直接用 `os.write` 探针取的）证明这不是 app 的锅：
主屏上先打印了 `pi: <cwd> is not trusted …` 警告，随后 app 发 `\x1b[?1049h` 进备用屏，只画它自己
拥有的那些行——**`pyte` 0.8.x 没有备用屏缓冲，1049 是未知私有模式，被忽略**，于是主屏残留留在网格上。
修法：新增 `AltScreenFeeder`（`scripts/pty_capture.py:485`），在 `\x1b[?1049h` 处原地 `screen.reset()`
清屏后继续喂（对象身份不变，所以两边的 `snapshot` / `deepcopy` 都不用改）。同一缺陷在 Linux 后端
也存在（app 也打印信任警告），本轮一并修掉——`pty_capture.py` 的主循环现在也用这个 feeder。
断言：合成用例 `BANNER…` + `1049h` + 半屏首帧 → 只留 `pi v0.1.0`。

### 2.2 诚实边界（写进 `--help` 与每次运行的 stderr）

ConPTY 不是 Unix pty：它是把控制台**重新渲染**给客户端。因此面板对**布局、文字、颜色、光标、
「某个按键确实改变了屏幕」**是可信的，但它不是 app 写出字节的逐字节录音。§6.2 记录了一处
已实测到的差异（hyperlink 打开时 9 个链接格丢失），这条边界是有实测支撑的，不是免责套话。

## 3. 交付二：Windows 每次按键执行两遍（P0 缺陷，已修）

### 3.1 缺陷

`App::translate_event`（`crates/pi-tui/src/app.rs:6147`）把每一个 `crossterm::Event::Key`
都转成 `InputEvent`，**不看 `key.kind`**。而 crossterm 的 Win32 后端对**每一次按键**都会产出两条
记录：`KeyEventKind::Press` 和 `KeyEventKind::Release`（`crossterm-0.28.1/src/event/sys/windows/parse.rs:226,289`
两处都构造成 `Release`，且 crossterm 的 `read()` 不做全局过滤）。于是端口在 Windows 上把
**同一次按键处理两遍**。

### 3.2 证据（三层，都能自己复跑）

**① crossterm 探针**（ConPTY 内运行，`D:\keyprobe`，crossterm 0.28，与工作区同版本）：

```text
$ python run_probe.py            # 探针在 ConPTY 里读 crossterm 事件，向 pty 写入 "ab"
KEY kind=Press   code=Char('a') mods=KeyModifiers(0x0)
KEY kind=Release code=Char('a') mods=KeyModifiers(0x0)
KEY kind=Press   code=Char('b') mods=KeyModifiers(0x0)
KEY kind=Release code=Char('b') mods=KeyModifiers(0x0)
```

**② Windows 上的第一张真机截图**（修复前二进制 = `lum-1450` 树里 `815b21d13` 的 `pi.exe`，
场景 `scripts/pty_scenarios/lum1457-win-key-release.json`，80×26）：

```text
> aallpphhaa  bbeettaa▍
```

**③ 反向验证**：把过滤改回「两个 kind 都映射」，同一个新测试文件
（`crates/pi-tui/tests/key_event_kinds.rs`）**9 条里 7 条立刻红**，其中
`a_typed_word_is_not_doubled` 的失败断言正是 `left: "aallpphhaa"` / `right: "alpha"`。

### 3.3 修法

```rust
// crates/pi-tui/src/app.rs:6147
pub fn translate_event(event: CtEvent) -> Option<InputEvent> {
    match event {
        CtEvent::Key(key) => match key.kind {
            CtKeyEventKind::Release => None,
            CtKeyEventKind::Press | CtKeyEventKind::Repeat => Some(InputEvent::from(key)),
        },
        ...
```

* 签名从 `InputEvent` 变成 `Option<InputEvent>`：release **没有输入**，这是语义上的实话；
  驱动侧（`crates/pi-coding-agent/src/interactive.rs:705`）改成 `let Some(translated) = … else { continue }`，
  编译器保证不会再有人忘记处理 `None`。
* `Press` 与 `Repeat`（长按自动重复）仍然映射——长按要真的重复。
* **为什么这是对齐上游而不是「Windows 特例」**：上游 pi 跑在 Node `readline` 上，永远收不到 release，
  一次按键就是一次动作；codex 亦然。端口此前是在 Windows 上多执行了一遍，不是别处少执行。
* 影响面：全仓 **没有任何** chord 消费 `KeyEventKind`（`grep -rn KeyEventKind crates/` 只命中本处），
  所以不存在「某个 chord 依赖 release」的隐藏语义。

### 3.4 测试（`crates/pi-tui/tests/key_event_kinds.rs`，9 条，全绿）

从**最底层**（`CtEvent::Key`）起步，所以过滤被删掉就会红；再走到用户看得见的编辑器草稿：

| 测试 | 钉住什么 |
|---|---|
| `a_release_event_carries_no_input` | release 转出 `None`（字符键 + Backspace） |
| `press_and_auto_repeat_still_translate` | Press / Repeat 仍然映射（长按不丢） |
| `a_tapped_character_is_typed_once` | 一次点按 = 一个字符 |
| `a_typed_word_is_not_doubled` | 打 `alpha` 就是 `alpha`（就是截图里那条症状） |
| `one_backspace_deletes_one_character` | 一次 Backspace 删一个字符 |
| `one_enter_submits_once` | 一次 Enter 提交一次 |
| `a_held_key_repeats` | press+repeat+release → 两个字符 |
| `control_chords_are_not_applied_twice` | 一次 `Ctrl+W` 只杀一个词 |
| `non_key_events_keep_their_translation` | 鼠标 / resize 路径未被误伤（滚轮坐标仍带 x/y） |

### 3.5 门禁（本 tip 实测）

| 门禁 | 基线 `815b21d13` | 本轮 |
|---|---|---|
| `cargo test -p pi-tui --no-fail-fast` | 1,045 / 0（合并后实测值） | **1,054 / 0**（+9） |
| `cargo test -p pi-coding-agent --lib --no-fail-fast` | 584 / 8 | **584 / 8**（同一批 8 条 Windows 环境类：绝对路径、trust、node fs、`resource_loader`） |
| `cargo test -p pi-coding-agent --lib -- drain_ready input_event interactive handle_input` | —— | **107 / 0** |
| `cargo fmt --all -- --check` | 干净 | 干净 |
| `cargo clippy -p pi-tui --all-targets -- -D warnings` | 干净 | 干净 |
| `cargo build -p pi-coding-agent --bin pi` | —— | 成功（PTY 用的就是这个二进制） |
| 真 ConPTY 场景 | —— | 见 §4（9/9、15/16+1 XFAIL、8 帧） |

数字口径：`pi-tui` 基线 1,045 是 §0.16.1（LUM-1450 合并后）登记值，本轮 +9 恰好等于新文件条数，
无历史重估。

## 4. 交付三：真机证据（全部为 Windows ConPTY 现场截图 + 可 grep 的字符网格 dump）

复现命令（三条都用同一个二进制 `target/debug/pi.exe`）：

```bash
python pi-rust/scripts/pty_capture_win.py --bin pi-rust/target/debug/pi.exe \
    --steps pi-rust/scripts/pty_scenarios/<scenario>.json --out <png> [--sheet N]
```

### 4.1 A/B：按键双执行（`lum1457-win-key-release.json`）

| 帧 | 修复前（`815b21d13` 二进制） | 修复后（本轮） |
|---|---|---|
| 2. 打 `alpha beta` | `> aallpphhaa  bbeettaa▍` ❌ | `> alpha beta▍` ✅ |
| 3. 一次 Backspace | 删掉两个字符 ❌ | `> alpha bet▍` ✅ |
| 4. 一次 Ctrl+W | 杀两个词 ❌ | `> alpha ▍` ✅ |
| 5. Enter 提交 | 草稿 `hhii`、回复出现 2 次 ❌ | `> hi` + 回复 1 次 ✅ |
| 断言总计 | **3 PASS / 6 FAIL** | **9 PASS / 0 FAIL** |

截图：`docs/screenshots/lum1457-win-key-release-before-80x26.png`(+`.txt`)、
`...-after-80x26.png`(+`.txt`)。

### 4.2 全 TUI 交互（`interaction.json`，120×34，Linux 场景原样复跑）

8 帧真机画面覆盖「启动头（17 行键位提示）+ onboarding + `/` 触发补全下拉（模糊收窄 + `(1/21)` 计数）
+ `@` 文件补全 + composer 行 + footer 计量」：

```text
❯ help           Show this help text
  clear          Clear the message view
  new            Start a new session
  copy           Copy last agent message to clipboard
  name           <name> — Set session display name
  (1/21)
> /▍
Faux test model  session-18d7cbbc7a633204        in 0 out 0 ?/8.2k  ? for help
```

截图：`docs/screenshots/lum1457-tui-interaction-conpty-120x34.png`(+`.txt`)。
这是本仓库**第一张在 Windows 上由真 PTY 会话产生**的多帧交互截图。

### 4.3 chatinput 审计场景（`lum1360-chatinput-audit.json`，80×26，14 面板）

为 Linux PTY 写的场景在 ConPTY 上**原样**跑：**15 PASS / 0 FAIL / 1 XFAIL**。逐条对照 codex /
Martty / 上游的行仍然成立：`Ctrl+W` 删一个词、`Alt+B`/`Alt+F` 词跳、`Ctrl+A`/`Ctrl+E` 行内、
`Ctrl+K`+`Ctrl+Y` kill-ring 往返、composer 双击/拖拽选词、历史反查、`>` 前缀转录回显。

唯一 XFAIL 是第 8 面板的「第二行草稿」`  second line▍`——不是编辑器语义问题，而是
**Ctrl+J 在 Windows 上根本不是 Ctrl+J**，见 §6.1。

截图：`docs/screenshots/lum1457-chatinput-audit-conpty-80x26.png`(+`.txt`)。

## 5. Rust ↔ TS 差距复测（本轮亲自跑，本 tip）

| 量 | 本轮实测 | 上一轮（`815b21d13`） | 命令 |
|---|---|---|---|
| 纯代码规模（src↔src） | **91.5%**（140,063 / 153,106，253 文件） | 91.5%（140,032） | `python pi-rust/scripts/measure_loc.py` |
| 测试用例规模 | **51.2%**（2,720 / 5,309） | 51.1%（2,711） | `grep -rhoE '#\[(tokio::)?test\]' pi-rust/crates --include=*.rs \| wc -l` |
| `app.*` 接线 | **43 / 44 = 97.7%**（silent：`app.tree.editLabel`） | 同 | `python pi-rust/scripts/app_action_coverage.py pi-rust` |
| `tui.*` 消费面 | **49 / 49 = 100%** | 同 | `python pi-rust/scripts/keybinding_coverage.py pi-rust`（口径缺陷见 §6.4） |
| 扩展生命周期事件 | **36 / 36 声明 + 36 / 36 生产构造点** | 同 | `python pi-rust/scripts/extension_event_coverage.py pi-rust` |
| 提示面硬编码 chord | **0 硬编码 / 5 知会** | 同 | `python pi-rust/scripts/hint_chord_literals.py pi-rust` |
| TUI 模块面 | 35 / 42 = 83.3% | 同 | 手工清单，见 `RUST_TS_PARITY_METRICS.md` §3.5 |
| TUI 交互+视觉轴 | **90.3%** | 90.3% | `(14×0.905 + 8×0.90)/22` |

加权（13 轴，权重与公式见 §4.1）：

```text
5×1.00 + 13×0.90 + 8×1.00 + 6×0.70 + 14×0.905 + 8×0.90 + 7×0.783 + 7×0.70
  + 8×0.95 + 7×1.000 + 9×0.85 + 5×0.5123 + 3×0.95 = 86.81% → **86.8%**
```

**诚实说明**：本轮加权只从 86.80% 走到 **86.81%**，一位小数不变。原因是那条 P0 缺陷落在
「可构建/可测/可运行」轴里——而该轴在**修复前也是 100%**（Linux 上能构建、能测、能跑；
缺陷只在 Windows 真终端里可见）。这正说明当前 13 轴口径**测不出平台通路类缺陷**：
它缺一条「真终端输入通路在目标平台上与上游一致」的轴。本轮**不擅自把它塞进权重表**
（新轴要先把测量做实，见 §0.16 的同一条规矩），只在 §8 登记为候选轴。

**三个数字都对，取决于问的是什么**：代码搬了 **91.5%**、测试覆盖 **51.2%**、
功能加权 **86.8%**。

## 6. 本轮新增发现（都已入库，未修的两条已按 §7 派发）

### 6.1 `Ctrl+J` 在 Windows 上是 `Ctrl+Enter`（真机实拍）

ConPTY 探针（写 `\n` = POSIX 上的 `Ctrl+J`）：

```text
KEY kind=Press   code=Enter mods=KeyModifiers(CONTROL)   <- 0x0A 进来是 Ctrl+Enter
KEY kind=Press   code=Enter mods=KeyModifiers(0x0)       <- 0x0D 是普通 Enter
```

Windows 控制台把 LF 归到 `VK_RETURN` 上，crossterm 按虚拟键解析，于是 `Ctrl+J` 永远带着
`KeyCode::Enter + CONTROL` 到达。端口 `tui.input.newLine` 的出货键位是 `shift+enter` / `ctrl+j`
（`keybindings.rs`），**`ctrl+enter` 无人认领**——所以 `Ctrl+J` 在 Windows 上按下去「什么都不发生」
（§4.3 第 8 面板的 XFAIL 就是它）。用户仍有 Shift+Enter 与「行尾反斜杠 + Enter」两条路可走，
所以不是致命，但**广告的键位不生效**属于本项目最忌讳的缺陷类。
处置：见 §7 第 1 条（需要 Linux 侧交叉验证后再改，避免在 Unix 上凭空多出一个 `ctrl+enter` 语义）。

### 6.2 hyperlink 打开时，助手行会掉格（ConPTY 实测，未定位到根因）

`supports_hyperlinks()` 在本机为真（环境里 `WT_SESSION` 存在），此时同一个 turn 的助手行：

```text
PI_HYPERLINKS 未设（= 开）：   4;3H <OSC8 "faux"> f <OSC8 end>  4;14H hello
PI_HYPERLINKS=0（= 关）：      4;3H faux-model  (faux)   4;21H hello
```

即：开启时**只写出 1 个链接格（`f`），另外 9 格与 ` (faux)` 一起消失**，文字整体前移 7 列。
两个可分离的事实：

1. **`[faux-model](faux) hello` 被当成 markdown 链接**。根因清楚：`MessageView::begin_assistant_stream`
   （`crates/pi-tui/src/message.rs:689`）把模型名写成 `[{model}]`，紧接着流式文本是
   faux provider 的 `(faux) hello`，markdown 解析器把 `[faux-model]` + `(faux)` 配成一个链接
   （label `faux-model`、URL `faux`），开 hyperlink 时按上游语义「label 变 OSC 8、丢掉 `(url)`」。
   这是**平台无关**的：kitty / ghostty / wezterm / Windows Terminal 上都会变成
   「模型名是个指向 `faux` 的可点链接、provider 名不见」。Linux 侧的既有 PTY dump 之所以是
   `faux-model (faux) hello`，是因为那里的终端没有触发 hyperlink 探测。
2. **9 个链接格丢失**只发生在 ConPTY 上。对照实验排除了两方嫌疑：把 `OSC8+字符+OSC8` 逐格
   写 10 遍给 ConPTY（`celltest.py`）**10 格全部原样回传**；把同一段一次性包在一个 OSC 8 里也正常。
   所以既不是「ConPTY 不认逐格 OSC 8」，也不是 `pyte` 解析问题（丢格发生在**原始字节流**里）。
   尚未定位到根因，按 §7 派发。

### 6.3 `app.tree.editLabel` 不是一个键位问题（顺带审计更正）

`/tree` 选择器已经有 filter（5 种）、fold/unfold、`shift+t` 的 label-time 开关，但
`TreeFilter::LabeledOnly` 的实现是**拿 `SessionEntry::Extension` 顶替 label**
（`crates/pi-coding-agent/src/commands/tree.rs:113-117` 自己写明了这条 deviation）：
`pi-protocol` 的 `SessionEntry`（`crates/pi-protocol/src/session.rs:23`）里**没有 label 变体**，
`pi-session` 也没有存它的地方，上游的 `appendLabelChange` / `getLabel` 在 Rust 侧没有对应物。
所以补齐 `app.*` 最后一条 = **会话模型加 label + 树视图画 label + 内联输入框（上游
`tree-selector.ts:1271` 的 `LabelInput`：`Enter` 保存、空值清除、`Esc` 取消）+ 持久化**，
是跨 4 个 crate 的切片，不是补键位。见 §7 第 3 条。

### 6.4 口径缺陷登记（沿用 LUM-1450 §7 第 2 条，本轮未修）

`keybinding_coverage.py` 报 `tui.* 49/49`，但它数的是「字面量曾被消费」。§6.1 与 LUM-1450 的
`Selector` 写死 `KeyCode::Enter` 都属于它漏报的那一类。本轮**没有**再靠人工发现新的一处
（`Selector` / `SettingsList` 已在 LUM-1450 接上 `kb.matches`），但工具本身仍缺一条
「组件级 handler 轨迹必须出现 `kb.matches`」的断言。

## 7. 「最多 3 个任务」的处置 = 1 自做 + 3 条 backlog（不启动 run）

* **自做（1 件）**：ConPTY 后端 + `translate_event` 修复 + 9 条回归 + 3 张真机截图 + 本文件。
* **新立 3 条，全部 `backlog`（入库不启动，避免超上限）**：
  1. **LUM-1477**（指派 `编程助手-window`，验收需用 `scripts/pty_capture_win.py` 复跑）— Windows 键位通路：`Ctrl+J` → `Ctrl+Enter`（`\n` 探针证据 + 推荐修法：`cfg!(windows)` 下把
     `ctrl+enter` 作为 `tui.input.newLine` 的别名，并在 Unix 侧加「不得凭空新增 `ctrl+enter` 语义」的断言）。
  2. **LUM-1478**（指派 `编程助手-devbox1`，可在 Linux 用 `PI_HYPERLINKS=1` 交叉复现）— hyperlink 开启时助手行掉格（跨平台 markdown 语义 + ConPTY 格丢失两件事分开验收；
     建议先修平台无关那条：模型名不应该是一个指向 provider id 的 markdown 链接）。
  3. **LUM-1479**（指派 `编程助手-devbox2`）— `app.tree.editLabel` 与 tree label 全链路（`pi-protocol` + `pi-session` + `pi-coding-agent` + `pi-tui`）。
* **并发数**：自做 1 + 在办/待办（LUM-1434 CLI flag、LUM-1448 已并入收尾、LUM-1453 backlog）= 上限内，
  本轮不新起 run。

## 8. 仍然缺的（供下一轮起手）

1. **平台输入通路轴**（本轮候选新轴）：13 轴测不出「按键双执行」这类缺陷。可量化的形式：
   「每个出货 chord 在目标平台上是否有可交付的字节/键记录」——`\n` → `Ctrl+Enter`、
   LF/CR 不可分、release 记录，都属于这一轴。要进权重表需先把测量做实（同 §0.16 规矩）。
2. **ConPTY 格丢失**（§6.2 第 2 条）未定位：影响的是「Windows 截图能否当逐格证据」。
   在定位之前，Windows 捕获的**断言**仍以「app 自己写出的文本」为准（composer / 对话框 / footer
   均逐字核对过），转录里带 OSC 8 的格子不作为证据。
3. `/help` 的 `keys:` 段与 `/hotkeys` 两套行表（LUM-1450 §7 第 4 条）仍在。
4. modal 内拖拽 / 悬停、`settings` 滚轮按矩形认领（LUM-1445 §8 第 4、5 条）仍在。
5. 扩展事件保真度 6 条（provider 三个事件返回值不生效、`context` 不链式等）仍在，
   见 `RUST_TS_PARITY_METRICS.md` §6.1。

## 9. 范围之外

未碰 `pi-protocol` / `pi-ai` / `pi-agent-core` / `pi-extensions` / `pi-session` / `pi-server` /
`pi-client` / `pi-chord` / `pi-evals` / `pi-telemetry` 的源码；未碰上游 TS（`packages/**`，只读取证）；
未改 `/hotkeys`、slash 命令表、CLI flag 面、CI / Docker。`pi-tui` 侧只动事件翻译与两个测试文件的
`.unwrap()`（签名从 `InputEvent` 变成 `Option<InputEvent>` 的机械后果）。
