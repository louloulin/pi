# LUM-1485 — 终端标题（`ctx.ui.setTitle`，OSC 0）落地 + 真 ConPTY 驱动抓到的 `Ctrl+J` 平台缺陷

> scope: `pi-tui`（`terminal_title` / `app` / `input`）+ `pi-extensions`（`host` / shim / docs）+
> `pi-coding-agent`（`extensions/ui_bridge` / `interactive`）+ `scripts/`（harness 的 raw 断言与分步发送）
> branch: `lum1485-work` → `feature/pi.rs`
> 时间锚：LUM-1481（`c2d40dc7e`）之后的下一轮；issue 正文是 LUM-981 伞形任务的 autopilot 复触发
> （「基于 rust 实现 pi 同时兼容 pi 的插件生态 …… 优先完善 tui 的功能 …… tui chatinput 改造参考
> codex 的 tui 和 Martty …… 真实的分析，并说明完成进度百分比」）。

## 0. 结论速览

| 量 | 本轮实测 | 依据 |
|---|---|---|
| 上游终端标题 | `Terminal.setTitle` 写 **OSC 0 + BEL**（`packages/tui/src/terminal.ts:520-523`）；交互模式在 init / `/new` / `/resume` / `/name` / `session_info_changed` 时自动写成 `"<APP_TITLE> - <session name> - <cwd basename>"`（`interactive-mode.ts:1017-1028`，调用点 `:1064`/`:1991`/`:2263`/`:3210`）；插件可随时 `ctx.ui.setTitle(title)`（`:2443`） | §1.1 |
| Rust 端口（本轮前） | `git grep -n 'setTitle\|set_title' -- pi-rust/crates` 在生产代码里 **0 命中**：shim 的 unsupported 列表里有它（`pi-ext-shim.mjs:1124`），`app.rs` 的 `ctx.ui` 对照表写着 out of scope。终端标题这条通路**整条不存在**，连「自动标题」也没有 | §1.2 |
| 本轮后 | `setTitle` 走完整通路：JS shim → `host_ui_region("setTitle")` → `UiRegionHost::set_title` → `RegionOp::Title` → `App::set_terminal_title` → 驱动写 `\x1b]0;…\x07`；会话变化时由 `set_session_name` / `set_status_cwd` 自动重算 | §2 |
| **真 ConPTY 驱动抓到的第二个缺陷** | **`Ctrl+J`（换行）在 Windows 上完全无效**：`crossterm` 的 Unix 解析器把 `0x0A` 折成 `Char('j') + CONTROL`，Win32 控制台后端把同一字节报成 `Enter + CONTROL`，而 `tui.input.newLine` 绑的是 `"ctrl+j"` → 匹配不到任何绑定，被当成「没绑定的控制键」丢弃。修复后同一场景面板由 **XFAIL 转 PASS** | §3 |
| 新增行为测试 | **20 条**（`pi-tui` +18 = 6 条模块单测 + 8 条标题生命周期 + 4 条 `Ctrl+J`；`pi-extensions` +1；`pi-coding-agent` +1 驱动级） | §4.2 |
| 新增真终端截图 | **2 张**（`docs/screenshots/lum1485-terminal-title.png` 4 面板 + `lum1485-chatinput-audit-conpty-80x26.png` 14 面板），均为 **ConPTY 实拍**（`scripts/pty_capture_win.py`），另新增 harness 的 OS C 字节级断言 | §5 |
| `pi-tui` 全量 | **1216 passed / 0 failed**（同机基线 `c2d40dc7e` = **1198/0** → **+18**） | §4.2 |
| `pi-coding-agent` 全量 | **850 / 28**（失败集合 28 条逐条为 Windows 环境类；基线同机 844/33，**新增失败 0**） | §4.3 |
| `pi-extensions` 全量 | **133 / 5**（5 条全是 `/dev/urandom` 等 Unix 环境类） | §4.4 |
| 纯代码规模（src↔src） | **94.9%**（145,296 / 153,106） | `scripts/measure_loc.py` |
| 测试规模 | **52.3%**（2,907 / 5,563，分母口径与上轮相同） | §4.5 |
| 加权完成度 | **87.2%**（87.173%）——**这 +0.02pt 全来自测试轴**，本轮两项交付都不在 13 轴里 | §4.6 |

一句话结论：**上游把「终端标题」当成一条真的显示通路（会话名 + 目录进 tab 标题，插件可覆盖），
Rust 端口连自动标题都没有；本轮接通它，并用真 ConPTY 把「按键时序」这一层也跑了一遍，顺手抓到
`Ctrl+J` 在 Windows 上根本不生效 —— 这是 frame-buffer 截图与 App 级单测都看不见的那一类缺陷。**

## 1. 真实审计

### 1.1 上游取证（第一手，本机读源码）

```ts
// packages/tui/src/terminal.ts:520-523
setTitle(title: string): void {
    // OSC 0;title BEL - set terminal window title
    process.stdout.write(`\x1b]0;${title}\x07`);
}
```

```ts
// packages/coding-agent/src/modes/interactive/interactive-mode.ts:1017-1028
private updateTerminalTitle(): void {
    const cwdBasename = path.basename(this.sessionManager.getCwd());
    const sessionName = this.sessionManager.getSessionName();
    if (sessionName) {
        this.ui.terminal.setTitle(`${APP_TITLE} - ${sessionName} - ${cwdBasename}`);
    } else {
        this.ui.terminal.setTitle(`${APP_TITLE} - ${cwdBasename}`);
    }
}
```

上游的四个事实决定了本轮的实现形状：

1. **序列是 OSC 0（`\x1b]0;` + BEL）**，不是 OSC 2，也不是 ESC `\\` 结尾 —— 注释里写得很清楚。
2. **自动标题由会话事实决定**，在 init、`/new`、`/resume`、`/name`、`session_info_changed` 与
   一次 Windows 更新检查之后各写一次（`updateTerminalTitle` 的 5 个调用点）。
3. **`ctx.ui.setTitle` 是同一入口**（`:2443` 直接转发给 `terminal.setTitle`），也就是说插件
   可以覆盖标题，而下一次会话变化会把自动标题写回来 —— 上游没有「恢复」API。
4. **退出时不恢复原标题**（上游全文没有还原逻辑），这一点在 §6 明确记为「不是缺口」。

`APP_TITLE` 默认是 `"π"`（`config.ts:503`：`piConfig?.name ? APP_NAME : "π"`），本端口按 §6 偏差第 3 条
用自己 header 的 `"pi"`。

### 1.2 缺口复现（基线 `c2d40dc7e`）

```bash
$ git grep -n 'setTitle\|set_title' -- pi-rust/crates | grep -v tests
pi-extensions/runtime/pi-ext-shim.mjs:1124:    "setTitle",
pi-tui/src/app.rs:201://! `setWorkingMessage` / `setTitle` and the remaining `ctx.ui` methods are ...
# 生产代码里没有任何一处把标题写到终端：0 命中。
```

即：**「终端 tab 标题显示会话名」这件用户每天都能看到的事，Rust 端口一件都没做**。
它不是 TUI 几何、也不是 composer 语义，所以历轮的 compositor / 输入面审计都没有量到它 ——
和 LUM-1481 的 `setStatus` 一样，是「通道缺失」而不是「画得不对」。

### 1.3 为什么这一格值得单独一轮

* 它是 `ctx.ui` **文本类**显示通路里唯一有明确宿主语义的一条（另两条 `setEditorText` / `setTheme`
  要么已有等价入口、要么要动主题系统），LUM-1481 §7 已把它列为下一轮第一顺位。
* 它跨 3 个 crate，且**不与任何 footer / chrome 几何冲突**（不碰 `status.rs` 的行数预算），
  与在办面可以并发。
* 它的验证方式与「画在哪个格子」完全无关：OSC 0 是**终端状态**，帧缓冲里永远看不见 ——
  这正是必须上真 PTY + 字节级断言的地方（§5.1）。

## 2. 交付一：终端标题通路（文件:行号）

| 文件 | 改动 | 说明 |
|---|---|---|
| `pi-rust/crates/pi-tui/src/terminal_title.rs`（新，206 行） | `TITLE_OPEN`/`TITLE_CLOSE`/`title_sequence`（`:53-66`，逐字节等于上游模板串）、`sanitize_title`（`:83`）、`path_basename`（`:103`）、`auto_title`（`:124`） | 纯函数模块：序列格式 + 标题合成 + 控制字符清洗，6 条单测 |
| `pi-rust/crates/pi-tui/src/app.rs` | `terminal_title` / `pending_terminal_title` 两个字段（`:1225`/`:1230`，单槽 pending）、`set_terminal_title`（`:2188`）、`sync_terminal_title`（`:2201`）、`reassert_terminal_title`（`:2217`）、`take_terminal_title`（`:2232`）、`queue_terminal_title`（`:2237`）；`set_session_name`（`:2099`）与 `set_status_cwd`（`:2110`）末尾各加一次 `sync_terminal_title()` | App **不写 stdout**：它只排队，驱动来写（与 `pi-tui` 不能碰 tty 的分层一致） |
| `pi-rust/crates/pi-coding-agent/src/interactive.rs` | `flush_terminal_title`（`:4810`）在每轮 `region_pump` 之后、`terminal.draw` 之前调用（`:711`）；`run_external_editor`（`:4949`）与 `suspend_to_background`（`:4981`）在拿回 tty 后 `reassert_terminal_title()` | 一个 tick 最多写一次、内容不变就不写 |
| `pi-rust/crates/pi-extensions/src/host.rs` | `UiRegionHost::set_title`（`:1194`）、`RegionCommand::Title`（`:1223`）、worker 分发（`:1261`）、`handle_region_call("setTitle")`（`:1354`） | 与 `setStatus` 同一条同步→异步通路 |
| `pi-rust/crates/pi-extensions/runtime/pi-ext-shim.mjs` | `ui.setTitle(title)`（`:872`，`String(title)` 后转发，无区域宿主时按 denial 处理），并从 unsupported 列表移出 | 上游签名是 `setTitle(title: string)`，模板插值天然字符串化 |
| `pi-rust/crates/pi-coding-agent/src/extensions/ui_bridge.rs` | `RegionOp::Title(String)`（`:378`）、`TuiRegionHost::set_title`（`:466`）、pump 应用（`:637`） | 同一屏内到达的 `setTitle` 在本次绘制前落位 |
| `pi-extensions/docs/EXTENSIONS.md` / `SDK_MODULES.md` | `setTitle` 从「inert no-op」改为「转发 → 终端标题；宿主负责清洗控制字符」 | 文档与实际行为对齐 |

### 2.1 三条不变量（都有测试钉住）

1. **App 只排队，不写终端**：`take_terminal_title()` 是唯一的出队口；驱动写的是
   `pi_tui::terminal_title::title_sequence(&title)`，驱动级用例断言它逐字节等于
   `\x1b]0;ext - compiling\x07`，并断言渲染帧里**不含**标题文本（标题不是格子内容）。
2. **边沿触发**：`queue_terminal_title` 与「当前生效的标题」比较，相同则不排队；
   启动时 `set_session_name(None)` 与 `set_status_cwd(Some(..))` 在同一次 tick 里只留最后一次
   （单槽 pending），所以启动只发 **一条** 序列，空闲帧 **零** 字节。
3. **一行永远是一行、一条序列永远是一条序列**：标题里的 C0/DEL/C1 全部换成一个空格再 trim，
   所以扩展塞一个 `BEL`/`ESC` 进去也不可能提前结束 OSC 0 或起一条新序列。

### 2.2 两个刻意的行为选择（都能在上游找到依据）

* **`/name` / `/resume` / 会话树切换后自动标题会覆盖插件标题**：上游 `updateTerminalTitle` 不认识
  `setTitle`，插件标题只在「下一次会话变化」之前有效；本端口照抄。
* **`app.suspend`（`Ctrl+Z`）与 `app.editor.external` 拿回 tty 后强制重写一次**：上游整场不离开
  备用屏，没有这个场景；而子进程（shell / 编辑器）可以把标题改掉，所以回来时 `reassert_terminal_title()`
  无条件排队（绕过去重）。这是本端口**多出来**的一处，写在 §6 已知偏差里。

## 3. 交付二：真 ConPTY 抓到的 `Ctrl+J` 平台缺陷

### 3.1 怎么抓到的（不是猜的）

写 LUM-1485 的标题场景时，`/retitle<Enter>` 一直不提交：帧里 composer 只是多出一行、命令没跑。
用一次性探针脚本（`scripts/pty_capture_win.py` 的 `ConPty` + `pyte`）逐字节复现，再用
`PI_DEBUG_EVENTS` 临时把 `App::translate_event` 收到的原始 `crossterm` 事件打到文件里，得到：

```text
Char('a') KeyModifiers(0x0) Press
Char('a') KeyModifiers(0x0) Release
...
Enter KeyModifiers(0x0) Press        # \r  = Enter
Enter KeyModifiers(0x0) Release
Enter KeyModifiers(CONTROL) Press    # \n  = Ctrl+J（Windows 控制台报的是 Enter+CONTROL）
Enter KeyModifiers(CONTROL) Release
```

### 3.2 根因（两个后端，同一个字节）

```text
0x0A
 ├── crossterm Unix 解析器： b'\x01'..=b'\x1A' → Char('j') + CONTROL
 │     （其源码注释：`\n` = 0xA, which is also the keycode for Ctrl+J … it's better to use Ctrl+J）
 └── crossterm Win32 后端： 控制台 KEY_EVENT(unicode='\n', vkey=VK_RETURN) → Enter + CONTROL
```

`tui.input.newLine` 的默认绑定是 `["shift+enter", "ctrl+j"]`，而 `parse_key_id("ctrl+j")`
解析成 `Key{Char('j'), CONTROL}`。于是 **Windows 上 `Ctrl+J` 落到 `Enter + CONTROL`，与任何绑定都不匹配，
在 `Editor::handle_key_with` 末尾被当成「没绑定的控制键」丢弃**（`editor.rs:2856` 的兜底）。
后果对用户是可感知的：`Ctrl+J` 是本端口在**没有 Kitty 键盘协议**的终端上唯一可靠的换行键
（`Shift+Enter` 需要厂商扩展），`docs/LUM1312_CHATINPUT_MULTILINE.md` 把它写成多行草稿的入口。
LUM-1457 打通 Windows 真 PTY 时用的是 `<C-u>hi<Enter>`，`<C-u>` 恰好让 burst 上下文断开，绕过了这个坑；
**同一台机器上 LUM-1360 的 composer 审计场景第 8 面板一直是 XFAIL**，当时被当成「probe 未满足」。

### 3.3 修复

`crates/pi-tui/src/input.rs`：在 `impl From<CtKeyEvent> for InputEvent` 里加一条归一化
（新增私有谓词 `is_ctrl_j`，`input.rs:328`；折行点在 `input.rs:340`）：

```rust
let code = match event.code {
    // `\n` 是键码，不是换行；见 [`is_ctrl_j`]。
    CtKeyCode::Enter if is_ctrl_j(&event.modifiers) => KeyCode::Char('j'),
    ...
```

只折 **`Enter + CONTROL` 且不带 Shift/Alt/Super** 这一种拼写：`Enter` 仍然提交、
`Shift+Enter` 仍是 `tui.input.newLine` 的另一半、`Ctrl+Alt+Enter` 不会被误读。

选在 `From<CtKeyEvent>` 而不是「让 `key_matches` 多认一种拼写」，理由是分层：
`input.rs` 已经是后端差异的归一化缝（`SUPER`/`META` 折叠、字母大小写代 Shift 都在这里），
而 `key_matches` 的职责是「事件 → 绑定」的语义匹配。折在入口，`InputEvent` 在两平台上完全同形，
编辑器 / 键位表 / 测试都不需要知道平台差异。

## 4. 验证结果（本机 Windows / cargo 1.97.1 / `--offline`）

### 4.1 构建与静态门禁

| 门禁 | 结果 |
|---|---|
| `cargo build --offline -p pi-coding-agent --bin pi` | 成功（下面所有 PTY 证据都用这个二进制） |
| `cargo fmt --all -- --check` | 干净 |
| `cargo clippy -p pi-tui --all-targets -- -D warnings` | **exit 0** |
| clippy `-p pi-tui -p pi-extensions -p pi-coding-agent --all-targets` | 改动文件 **0 告警**（余下 13 条在 vendored `rquickjs-core`、1 条是既存 `pi-extensions::signal_name`） |

### 4.2 测试

| 目标 | 结果 |
|---|---|
| `cargo test -p pi-tui -j 8` | **1216 passed / 0 failed**（基线 `c2d40dc7e` 同机 **1198/0** → **+18**） |
| 其中 `--lib terminal_title` | 6 / 0（序列字节、清洗、basename、合成） |
| 其中 `--test terminal_title` | 8 / 0（启动一条、重命名、清名、去重、扩展标题、注入防护、suspend 重写） |
| 其中 `--test key_event_kinds` | 13 / 0（原 9 条 + 4 条 Ctrl+J） |
| `cargo test -p pi-extensions --no-fail-fast` | **133 / 5**（5 条 `/dev/urandom` 等 Unix 环境类；本轮新用例 ok） |
| `cargo test -p pi-coding-agent --no-fail-fast` | **850 / 28**（28 条全为 Windows 环境类：绝对路径拒绝、真 `bash`、扩展发现、trust、export 文案；**无本轮面**） |
| `--test extension_ui interactive_extension_title_reaches_the_terminal_queue` | 1 / 0（真 JS 扩展 → 真 host → 真 pump → 真 App） |

### 4.3 反向验证（本机实做，两条都做）

1. **标题通路**：用环境变量把 `App::sync_terminal_title` 短路成空 → `tests/terminal_title.rs`
   **8 条里 5 条立刻红**；驱动级用例 `interactive_extension_title_reaches_the_terminal_queue`
   同时红（断言 `startup` 的自动标题不成立）。恢复后全绿。
2. **`Ctrl+J` 归一化**：把 `is_ctrl_j` 短路成 `false` → `tests/key_event_kinds.rs` **2 条立刻红**
   （`a_windows_ctrl_j_translates_to_the_ctrl_j_chord`、`ctrl_j_opens_a_second_composer_row_on_a_windows_console`）。
   恢复后 13/0。

### 4.4 真 PTY A/B（同一条场景，前后各跑一次）

`scripts/pty_scenarios/lum1360-chatinput-audit.json`（14 面板 composer 审计，为 Linux 写的场景，
在 Windows ConPTY 上原样跑）：

| | 修复前 | 修复后 |
|---|---|---|
| 断言 | 15 PASS / **1 XFAIL** / 0 FAIL | **16 PASS / 0 XFAIL / 0 FAIL** |
| 转正的那条 | `[8] 8. Ctrl+J opens a second draft row instead of submitting` → `XFAIL '  second line▍'` | `PASS '  second line▍'` |

即：**同一个场景、同一个二进制参数，只差这一处归一化**，`Ctrl+J` 从「没反应」变成「开第二行草稿」。
另跑既有的 `lum1457-win-key-release.json` 作回归：**9 PASS / 0 FAIL**（证明 harness 改动与归一化
都没破坏按键通路）。

### 4.5 Rust↔TS 口径复测

* 纯代码规模（src↔src）**94.9%**（145,296 / 153,106；`scripts/measure_loc.py`，上轮 94.6%）。
* 测试规模 **52.3%**（2,907 `#[test]` / 5,563；分母口径与上轮相同，本轮自身 +20 条标记）。
* `app.*` 接线 **44/44 = 100%**、`tui.*` **49/49 = 100%**（`scripts/app_action_coverage.py --check-consumed`、
  `scripts/keybinding_coverage.py`）；扩展生命周期事件 **36/36 声明 + 36/36 构造点**。
* TUI 模块面：机械口径 `pi-tui/src` **37/42 = 88.1%**（+1 = `terminal_title.rs`）——
  **但这一格不算成绩**：上游把标题逻辑放在 `terminal.ts` 里，本端口拆成了独立文件，
  计数器涨的是「文件数」不是「多覆盖了上游一个模块」。加权里仍按上轮的 **36/42 = 85.7%** 计。
* `ctx.ui` 显示通路：**区域类 5/5**（不动）+ **文本类 1/3**（本轮把 `setTitle` 从缺变成接通；
  `setEditorText` / `setTheme` 仍是 no-op）。

## 5. 证据分级与截图

### 5.1 本轮给 harness 加的能力（`scripts/pty_capture.py` / `pty_capture_win.py`）

1. **`raw_expect` / `raw_reject` 断言**：对**这一面板自己产生的字节窗口**（`RAW_TRACE` +
   `raw_since(mark)`）做子串/正则断言。理由：OSC 0（终端标题）与 OSC 8（超链接）是**终端状态**，
   在 pyte 的格子里永远看不到；`raw_*` 让「这条序列真的到了终端」变成可失败的断言。
   POSIX 后端在 `pump` / `drain` 里 `note_raw`，Windows 后端在 `ConPty.drain` 里 `note_raw`。
   Windows 上 OSC 0 会被 ConPTY 原样转发（本轮用 `winpty` 直接探针实测：`\x1b]0;probe-title\x07`
   逐字节出现在读回流里），所以这条断言在 ConPTY 上成立。
2. **`send` 支持分步**：`"send": [text, {"pause": 0.4}, "<Enter>"]`。理由：LUM-1461 的
   paste-burst 分类器把「瞬时到达的 ≥3 个普通字符」当粘贴，**窗口内的 `Enter` 会被折成粘贴内容里的
   换行**，所以 harness 一次性写 `"/retitle<Enter>"` 与人类「输入命令再回车」是两个不同输入。
   `lum1360-chatinput-audit.json` 第 12 面板因此改成两步（这是场景修正，不是代码缺陷）。

### 5.2 截图（**ConPTY 实拍**，不是冻结帧）

| 文件 | 内容 | 断言 |
|---|---|---|
| `docs/screenshots/lum1485-terminal-title.png`(+`.txt`) | 4 面板 90×26：启动自动标题 / `/name demo` 重组 / 扩展 `setTitle` / BEL+ESC 清洗 | **11 PASS / 0 FAIL**（含 4 条 `raw_expect` + 3 条 `raw_reject`） |
| `docs/screenshots/lum1485-chatinput-audit-conpty-80x26.png`(+`.txt`) | 14 面板 80×26：composer 词移动（Ctrl+W / Alt+B / Alt+F）、行域 Home/End、kill-ring 往返、`Ctrl+J` 多行、鼠标点选/拖选、Ctrl+C 清稿、Enter 提交、Up 召回、Ctrl+R 反查 | **16 PASS / 0 FAIL / 0 XFAIL** |

**证据分级（重要）**：这两张是 `scripts/pty_capture_win.py` 在 **Windows 10 + ConPTY** 上跑真
`pi.exe`、真按键、真重绘得到的（`alive=True` 写在每帧头里，证明进程在整个录制期间活着）。
ConPTY 不是 Unix pty：它会把应用输出**重新渲染**一遍，所以「布局 / 文本 / 颜色 / 光标 / 重绘顺序」
是可信的，字节流本身是 ConPTY 的渲染结果 —— **唯独 OSC 0 我们单独验证过它会被原样转发**，
这才让 `raw_expect` 在 Windows 上站得住。按键时序则由 18 条 Rust 测试（含驱动级 1 条）与
两个场景的 **25 条面板断言**共同覆盖。

## 6. 范围之外与已知偏差

未碰 `pi-ai` / `pi-agent-core` / `pi-protocol` / `pi-session` / `pi-server` / `pi-client` /
`pi-chord` / `pi-evals` / `pi-telemetry` 的源码；未碰上游 TS（`packages/**`，只读取证）；
未改扩展事件表、slash 命令表、CLI flag 表、`CONSUMED_APP_ACTIONS`。

**已知偏差（写清而不是省略）**：

1. **标题里的控制字符会被换成空格**（上游原样插值）。上游把 `title` 直接拼进 `\x1b]0;…\x07`，
   一个含 `BEL` 的会话名就能提前结束序列并注入后续控制串；本端口用 `sanitize_title`（C0/DEL/C1 → `' '`，
   每个控制字符一个空格、不折叠连续空格）挡住，与 footer 对 `setStatus` 文本的清洗同一套契约。
2. **退出时不恢复原标题**：上游也不恢复（全文没有还原逻辑），所以这不是缺口；但意味着 `pi` 退出后
   终端 tab 仍显示 `pi - …`。`app.suspend` / `app.editor.external` 回来时会**重写**标题（上游无此场景），
   因为子进程可以把标题改掉。
3. **应用名用 `"pi"` 而非上游默认的 `"π"`**：上游 `APP_TITLE = piConfig?.name ? APP_NAME : "π"`，
   本端口 header 一直是 `locale::HEADER_TITLE = "pi"`（`pi v0.1.0`）；标题与 header 不一致才是 bug。
4. **cwd 是文件系统根时不贡献组件**：`path.basename("/")` 在上游是 `""`（标题会变成 `"π - "`），
   本端口得到 `"pi"` / `"pi - <name>"`。Windows 的 `C:\` 同理。
5. **Windows 控制台上的 `Ctrl+Enter`（Kitty 协议 `CSI 13;5u`）会被读成 `Ctrl+J`**：两者在
   `crossterm` 里同为 `Enter + CONTROL`，本端口无法区分；上游的字节匹配器在相反方向也有盲区
   （它永远匹配不到 Kitty 的 `Ctrl+Enter`），且没有任何绑定使用 `ctrl+enter`，结果都是一次换行。
6. **`setTitle` 不注册鼠标目标、不参与帧缓冲**：它是终端状态，不是格子内容（与 §2.1 第 1 条同一件事）。

## 7. 复现命令

```bash
cd pi-rust
cargo test --offline -p pi-tui -j 8                  # 1216 / 0
cargo test --offline -p pi-tui --test terminal_title --test key_event_kinds
cargo test --offline -p pi-extensions --no-fail-fast  # 133 / 5（5 条 Unix 环境类）
cargo test --offline -p pi-coding-agent --no-fail-fast
cargo fmt --all -- --check
cargo clippy --offline -p pi-tui --all-targets -- -D warnings

# 真终端证据（Windows / ConPTY；Linux 用 scripts/pty_capture.py，同一份场景 JSON）
python scripts/pty_capture_win.py --bin target/debug/pi.exe \
    --steps scripts/pty_scenarios/lum1485-terminal-title.json \
    --out docs/screenshots/lum1485-terminal-title.png
python scripts/pty_capture_win.py --bin target/debug/pi.exe \
    --steps scripts/pty_scenarios/lum1360-chatinput-audit.json \
    --out docs/screenshots/lum1485-chatinput-audit-conpty-80x26.png
python scripts/measure_loc.py          # 从仓库根跑
python scripts/app_action_coverage.py --check-consumed
```

## 8. 槽位 / 派发：本轮零派发

开工时实测：

| 量 | 实测 |
|---|---|
| `multica daemon status --output json` 的 `running_task_count` | **2**（含本轮）—— **未满**（上限 3） |
| 本仓在办面 | LUM-1467 / LUM-1469 / LUM-1481 均已合入 `feature/pi.rs`（`c2d40dc7e`） |
| 平台在办 | LUM-1434（CLI flag，另一 runtime）、LUM-1433/… 的同仓在多 runtime 上并行 |

**本轮仍选择零派发**，理由写在 `docs/RUST_TS_PARITY_METRICS.md` §0.26 的「派发判断」一节：
剩余候选里，`setEditorText` / `setTheme`（文本类显示通路剩两条）与 `footerData.getGitBranch()`
都要动 `app.rs` + shim + bridge 的**同一批文件**，而本轮刚把它们改过一遍；在这种「同一缝反复改」
的情形下并发只会制造合并冲突（仓库已经为这种撞车清过两次：LUM-1431 §3、LUM-1445 §8）。
比较划算的下一轮顺序是：

1. `footerData.getGitBranch()` 的 host→JS 查询通道（上游自定义 footer 读得到分支名，本端口硬编码 `undefined`）；
2. `ctx.ui.setTheme` / `setEditorText`（文本类显示通路收尾）；
3. `docs/LUM1481_EXTENSION_STATUS.md` §6.1 记的「44×16 下 `cut above` 提示与正文首行叠字」——
   纯几何缺陷，与上面两条都不冲突，适合与 1 或 2 并发派一条。
