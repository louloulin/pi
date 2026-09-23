# LUM-1490 — `footerData` 宿主→JS 查询通道（`getGitBranch` / `getAvailableProviderCount` / `onBranchChange`）+ 驱动每帧跟踪 `HEAD`

> scope: `pi-extensions`（`host.rs` / `pi-ext-shim.mjs`）+ `pi-coding-agent`（`footer.rs` /
> `interactive.rs` / `extensions/wiring.rs`）+ `scripts/`（`pty_capture*.py` 新增 `git_init`，
> 新场景 `lum1490-footer-data.json`）
> branch: `lum1490-work` → `feature/pi.rs`
> 时间锚：LUM-1485（`8678f7327`）之后的下一轮；issue 正文是 LUM-981 伞形任务的 autopilot 复触发
> （「基于 rust 实现 pi 同时兼容 pi 的插件生态 …… 优先完善 tui 的功能 …… 真实审计当前 rust 版本和
> ts 版本的差距 …… 说明完成进度百分比」）。上一轮 §6.1 把本单列为**下一轮第一顺位**（即 backlog 的
> LUM-1488），本轮兑现它。

## 0. 结论速览

| 量 | 本轮实测 | 依据 |
|---|---|---|
| 上游 `footerData` 面 | `ReadonlyFooterDataProvider` 恰好四个成员：`getGitBranch()` / `getExtensionStatuses()` / `getAvailableProviderCount()` / `onBranchChange(cb)`（`packages/coding-agent/src/core/footer-data-provider.ts:387-390`），随 `setFooter` 工厂的第三个参数交给插件（`interactive-mode.ts:2316-2327`）；官方示例 `examples/extensions/custom-footer.ts:28,45` 两个都用 | §1.1 |
| Rust 端口（本轮前） | `footerDataStub` 只有两个成员，且 `getGitBranch: () => undefined` **硬编码**；`getAvailableProviderCount` / `onBranchChange` **不存在**（插件按官方示例调用会 `TypeError`）。整个 `pi-rust/crates` 生产代码里 `getGitBranch` 0 命中 | §1.2 |
| 本轮后 | 四个成员全部可用：两个由 Rust 驱动**推快照**（`JsExtensionHost::sync_footer_data`）+ shim **同步查回**（`host_ui_region("footerData", {field})`），`getExtensionStatuses` 走会话级状态表，`onBranchChange` 由驱动在 `HEAD` 迁移时回调 | §2 |
| 顺带修掉的真实缺陷 | **内置 footer 的 `(branch)` 此前只在启动时解析一次**（`footer.rs` 文档里写明的刻意偏差），别的终端里 `git checkout` 后不刷新；本轮驱动改为**每帧重读 `HEAD`**，内置 footer 与插件 `footerData` 同时跟上 | §2.3 |
| 真 ConPTY 场景 | **12 条断言 / 4 面板全部 PASS**：`branch=lum1490-main` → `! git checkout -b lum1490-side`（`changes=1`）→ `/default-footer`（内置 footer 也显示 `(lum1490-side)`）→ `git checkout --detach HEAD`（`branch=null`） | §5.2 |
| **反向验证** | 把 shim 打回 `getGitBranch: () => undefined` + `onBranchChange` 空实现、并短路驱动每帧轮询 → 同一场景 **8 / 12 条立刻红**（4 PASS / 8 FAIL），证明面板不是空转 | §4.3 |
| 新增行为测试 | **6 条**（`pi-extensions` +3：快照查询、会话级作用域、迁移/退订/异常隔离；`pi-coding-agent` +3：`BranchTracker` 首次值、非仓库启动后再出现仓库、worktree gitfile） | §4.2 |
| 新增真终端截图 | **1 张 4 面板**（`docs/screenshots/lum1490-footer-data.png` + 同名 `.txt`），**ConPTY 实拍**（真 `pi.exe`、真按键、真重绘、真 git 仓库） | §5 |
| `pi-tui` 全量 | **1216 / 0**（同机基线 `8678f7327` **1216/0**，本轮未改 `pi-tui`，**不变**） | §4.2 |
| `pi-coding-agent` 全量 | **853 / 28**（同机基线 **850 / 28** → **+3**；28 条失败**逐条同名**，全为 Windows 环境类） | §4.2 |
| `pi-extensions` 全量 | **136 / 5**（同机基线 133 / 5 → **+3**；5 条失败同为环境类） | §4.2 |
| 纯代码规模（src↔src） | **95.1%**（145,618 / 153,106） | `scripts/measure_loc.py` |
| 测试规模 | **+6 条**（标记计数 2,969 / 5,448 = 0.5450，同法基线 2,963 / 5,448 = 0.5439） | §4.4 |
| 加权完成度 | **87.2%**（87.173% → 87.179%）——**只动第 12 轴（测试），+0.006pt** | §4.4 |

一句话结论：**上游把 `footerData` 定义成「驱动侧事实 + 会话级状态」的只读视图，插件生态里
官方示例第一行就调它；Rust 端口把它做成了两个假值。本轮把这条唯一的 host→JS 方向通路接通，
并顺手关掉「分支只在启动时解析一次」这条写在文档里的刻意偏差 —— 两件事共用一个每帧 `HEAD` 轮询器。**

## 1. 真实审计

### 1.1 上游取证（第一手，本机读源码）

```ts
// packages/coding-agent/src/core/footer-data-provider.ts:387-390
export type ReadonlyFooterDataProvider = Pick<
	FooterDataProvider,
	"getGitBranch" | "getExtensionStatuses" | "getAvailableProviderCount" | "onBranchChange"
>;
```

- 交给插件的入口：`ctx.ui.setFooter((tui, theme, footerData) => …)`
  （`interactive-mode.ts:2316-2327` 把 `this.footerDataProvider` 作为第三个实参传给工厂）。
- `getGitBranch()`：内部 `cachedBranch` 惰性解析，`null` 表示「不在仓库里 / detached HEAD」
  （`:126-131`，`resolveGitBranchSync` 走 `git symbolic-ref --quiet --short HEAD`）。
- `onBranchChange(cb)`：订阅 `HEAD` 变化，返回退订函数（`:149-160`），实现里用 `fs.watch` +
  reftable 目录监听 + 防抖（`:161-200`）。
- `getAvailableProviderCount()`：内置 footer 用它决定要不要给模型名加 `(provider) ` 前缀
  （`components/footer.ts:192`）。
- `getExtensionStatuses()`：`ctx.ui.setStatus` 写进的那张表（`:132-147`）。
- **官方示例就按这四个成员写**：`examples/extensions/custom-footer.ts:28` 在工厂里第一行
  `const unsub = footerData.onBranchChange(() => tui.requestRender())`，`:45` 读
  `footerData.getGitBranch()`。

### 1.2 缺口复现（基线 `8678f7327`）

```js
// pi-rust/crates/pi-extensions/runtime/pi-ext-shim.mjs（基线）
const footerDataStub = Object.freeze({
  getGitBranch: () => undefined,
  getExtensionStatuses: () => new Map(extensionStatuses),
});
```

```console
$ git grep -n "getGitBranch" 8678f7327 -- pi-rust/crates | grep -v docs
（0 命中：生产代码里没有任何一处实现或查询分支给插件）
```

三条实测后果：

1. **官方示例直接抛错**：`footerData.onBranchChange` 在基线里是 `undefined` →
   `TypeError: footerData.onBranchChange is not a function`，插件的自定义 footer 起不来。
   `getAvailableProviderCount` 同理。
2. **`getGitBranch()` 恒 `undefined`**：与上游 `string | null` 的类型不符，插件的
   `branch ? … : "no git"` 分支永远走 `no git` 一侧。
3. **分支只在启动时解析一次**：`footer.rs` 的模块文档把「no per-frame re-resolution,
   no filesystem watcher」列为**刻意偏差**，即上游「别的终端里 checkout，footer 跟着变」这条
   在本端口**内置 footer 也没有**（不是只有插件缺）。

### 1.3 为什么这一格值得一整轮

`footerData` 是 `ctx.ui` 里**唯一一个 host→JS 方向**的接口（其余全是 JS→host 的变更）。
它的难点不是「多接一个字段」，而是三件容易做错的事：

1. **方向相反**：要在同步的 JS `render()` 里查回 Rust 的每帧状态 → 需要一条新的同步查询 op，
   且必须能在 **渲染回调内部**安全重入（`host_ui_region` 正是这条路，见 §2.2）。
2. **作用域**：`buildCtx` 给每个事件/命令都发一个**新的 `ctx` 对象**，所以「会话级状态」
   （`setStatus` 表、`onBranchChange` 订阅者）**不能**放在 `makeUiContext` 里 —— 这一点在
   §3 里是真实踩到的坑（第一次真 ConPTY 跑出 `changes=0`）。
3. **语义边界**：「初始快照算不算一次迁移」「detached HEAD 是 `null` 还是 `undefined`」
   「订阅者抛错能不能带崩驱动帧」——三条都要有明确依据，否则接上了也是错的。

## 2. 交付：`footerData` 4/4

### 2.1 数据流（文件:行号）

| 文件 | 改动 |
|---|---|
| `pi-rust/crates/pi-extensions/src/host.rs` | 新增 `pub struct FooterData { git_branch, available_provider_count }`；`Inner.footer_data: Arc<Mutex<Option<FooterData>>>`；`handle_region_call` 新增 `"footerData"` 查询 op（`{field}` → `{ok:true,value}`）；`JsExtensionHost::sync_footer_data`（存快照 + 判迁移 + 通知）/ `footer_data()` / `notify_branch_change()`（回调 shim 的 `__pi_footer_branch_changed`，带 per-call 超时，失败不致命） |
| `pi-rust/crates/pi-extensions/runtime/pi-ext-shim.mjs` | **模块级**会话状态 `__pi_ui_statuses` / `__pi_footer_branch_subscribers` + `globalThis.__pi_footer_branch_changed` + `__pi_footer_data_stub`（四成员齐全，`getGitBranch` 把缺值折成 `null`，`onBranchChange` 返回退订函数）；`setStatus` 改写会话级表 |
| `pi-rust/crates/pi-coding-agent/src/footer.rs` | 新增 `pub struct BranchTracker`（`new` / `branch` / `poll`）：`HEAD` 路径解析一次、内容每帧重读；读不到（离开仓库、worktree 移动、会话在非仓库目录启动后又 `git init`）就重新向上解析 |
| `pi-rust/crates/pi-coding-agent/src/interactive.rs` | 启动时用 tracker 播种 `App` 状态与 `FooterData` 快照（`available_provider_count` 从 `available_provider_count(&options)` 抽出，与 footer 的 `(provider) ` 前缀同源）；主循环每帧 `branch_tracker.poll(&branch_cwd)`，迁移时同时更新 `App::set_status_git_branch` 与 `runtime.sync_footer_data(...)` |
| `pi-rust/crates/pi-coding-agent/src/extensions/wiring.rs` | `ExtensionRuntime::sync_footer_data` —— 无扩展时 `false` 的薄包装（无宿主会话每帧只付一次 `Option` 判断） |
| `pi-rust/crates/pi-extensions/docs/{SDK_MODULES,EXTENSIONS}.md` | `footerData` 从「gap / 只有 getExtensionStatuses」改为**四成员实现表**，写清 host→JS 方向、`null` 语义、会话级作用域 |

### 2.2 三条不变量（都有测试钉住）

1. **同步查询，不跨 await**：`getGitBranch()` 在 `render()` 里调用 → `host_ui_region("footerData")`
   → 直接读 `Inner` 里的快照。这条路径是**在渲染回调内部重入 Rust**，与 `src/ui.rs` 的
   `setStatus`/`setTitle` 同一形态（那些已由 LUM-1481/1485 的真终端证据覆盖）；
   `handle_region_call` 不触碰 `AsyncContext`，所以不会与 `async_with!` 的锁互等。
2. **初始快照不是迁移**：`sync_footer_data` 第一次只播种，返回 `false`；只有 `git_branch` 真的
   变了才回调 `onBranchChange`。依据是上游语义 —— `onBranchChange` 监听 `HEAD` 变化
   （`footer-data-provider.ts:149-160`），不是「首次取值通知」。
3. **`null` 而不是 `undefined`**：驱动侧的 `Option<String>` 为 `None`（不在仓库 / detached HEAD）
   时，shim 折成 `null` —— 上游类型就是 `string | null`。未推送过快照的宿主（非交互模式）
   回答同一个默认值。

### 2.3 顺带关掉「分支只在启动时解析一次」

`footer.rs` 的模块文档原先把「no per-frame re-resolution, no filesystem watcher」写成刻意偏差
（LUM-1466 §6）。本轮同一份 `BranchTracker` 同时喂两个读者：`App::set_status_git_branch`
（内置 footer 的 `pwd (branch) • name` 行）与 `footerData` 快照（插件）。成本被压到
**一次 `stat` + 一次小文件读/帧**：`HEAD` 路径解析一次后缓存，只有读不到时才重新向上走。
这与上游 `fs.watch` 的可观测行为一致（差的是延迟上限：这里 ≤ 一个渲染节拍 ≈ 50ms），
差别写在 §6。

## 3. 真 ConPTY 抓到的第一个坑：`ctx` 是每事件新建的

第一次跑场景（`lum1490-footer-data.json`）时，面板 1 全绿，面板 2 的
`branch=lum1490-side` 也对，但 `changes=0` —— **订阅者一次都没被通知**。
Rust 侧 `sync_footer_data` 的单元测试是绿的，所以问题在「谁持有订阅者集合」：

```js
// 基线 shim：buildCtx 每个事件/命令都调一次 makeUiContext
function buildCtx(extra) { … ui: makeUiContext(hasUI) … }
```

`const extensionStatuses = new Map()` 与订阅者集合原先都在 `makeUiContext` 内部 →
① 从 `session_start` 写的 status，换一个 `ctx` 装的 footer 就看不到；
② `globalThis.__pi_footer_branch_changed` 被**最后一次** `buildCtx` 覆盖，指向的是那个 `ctx`
的空集合，驱动回调落在空集合上。

修法：把这两样搬到**模块级**（与 `__pi_ui_components` 同级），`__pi_footer_branch_changed` 只在
模块加载时定义一次。上游本来就每会话一个 `FooterDataProvider`
（`footer-data-provider.ts:50-70`），所以这不是「补丁」而是把作用域改对。
回归测试 `footer_data_state_is_session_scoped_not_per_ctx` 的形状就是它：
**命令里装 footer → 再派发一个 `turn_start`（新 `ctx`）→ 分支迁移必须仍然通知**。

## 4. 验证结果（本机 Windows / cargo 1.97.1 / `--offline`）

### 4.1 构建与静态门禁

| 门禁 | 结果 |
|---|---|
| `cargo build -p pi-coding-agent --bin pi` | 成功（PTY 证据用的就是这个二进制） |
| `cargo fmt --all -- --check` | 干净 |
| `cargo clippy -p pi-tui --all-targets -- -D warnings` | **exit 0** |
| `cargo clippy -p pi-extensions -p pi-coding-agent --all-targets` | 改动文件 **0 告警**（余下在 vendored `rquickjs-core` 与既存 `pi-extensions::signal_name`） |

### 4.2 测试（基线 = `8678f7327`，同一台机器、同一工具链、先后各跑一遍）

| 目标 | 本轮 | 基线 | 差 |
|---|---|---|---|
| `cargo test -p pi-tui --no-fail-fast` | **1216 / 0** | 1216 / 0 | 0（本轮未改 `pi-tui`） |
| `cargo test -p pi-coding-agent --no-fail-fast` | **853 / 28** | 850 / 28 | **+3 通过，失败同名同数** |
| `cargo test -p pi-extensions --no-fail-fast` | **136 / 5** | 133 / 5 | **+3 通过，失败同名同数** |

- 新增 3 条（`pi-extensions/tests/host.rs`）：`footer_data_snapshot_is_queryable_from_a_custom_footer`、
  `footer_data_state_is_session_scoped_not_per_ctx`、`on_branch_change_fires_on_transitions_only_and_unsubscribes`
  （含「退订后不再回调」与「抛错的订阅者只告警、不带崩推送」）。
- 新增 3 条（`pi-coding-agent/src/footer.rs`）：`tracker_reports_the_initial_value_then_only_real_moves`、
  `tracker_notices_a_repository_appearing_after_a_non_repo_start`、`tracker_follows_a_worktree_gitfile`。
- 28 / 5 条失败**逐条同名**：绝对路径文案（`C:\a\b` vs `/a/b`）、`/etc` 拒绝、`/dev/urandom`、
  真 `bash`、扩展发现与 trust（`trust::tests::*`）、以及 `node_builtins` 的路径分隔符断言 —— 全是
  Windows 环境类，本轮改动面 0 条。
- **注意一次环境坑**：第一次跑全量时磁盘写满，`pi-coding-agent` 的 doctest 出现
  `LNK1318 Unexpected PDB error`（链接器 PDB，不是断言失败）。腾出空间后复跑即恢复
  （基线同机同命令 **850/28**，本轮 **853/28**）。

### 4.3 反向验证（本机实做）

把三件事同时打回基线形态：`getGitBranch: () => undefined`、`onBranchChange: () => () => {}`
（空实现）、驱动每帧轮询短路 → 同一份场景、同一个二进制参数：

| | 正向（本轮） | 反向（打回基线） |
|---|---|---|
| 断言 | **12 PASS / 0 FAIL** | **4 PASS / 8 FAIL** |
| 红的那些 | — | 面板 1 的 `branch=lum1490-main`、面板 2 的 `branch=lum1490-side`+`changes=1`、面板 3 的内置 `(lum1490-side)`、面板 4 的 `branch=null`、以及两条 `branch=undefined` 反断言 |

即：**每一格交付都有面板会红**，截图不是「凑巧绿的」。会话级作用域的回归由
`footer_data_state_is_session_scoped_not_per_ctx` 钉住（打回 per-`ctx` 版本时它以
`statuses=` 空表红，见 §3）。

### 4.4 Rust↔TS 口径复测

* 纯代码规模（src↔src）**95.1%**（145,618 / 153,106；`scripts/measure_loc.py`；上轮 94.9% = 145,296）。
* 测试规模：本轮 **+6 条标记**（`#[test]` + `#[tokio::test]`）。同一正则并排：本轮
  **2,969 / 5,448 = 0.5450**，基线 **2,963 / 5,448 = 0.5439**（TS 侧未变）。按 §0.26 的
  headline 口径（分母 5,563、基线 0.5226）折算：**0.5226 + 6/5,563 = 0.5237**。
* `app.*` 接线 **44/44 = 100%**、`tui.*` **49/49 = 100%**（`scripts/app_action_coverage.py`、
  `scripts/keybinding_coverage.py`）；扩展生命周期事件 **36/36 声明 + 36/36 生产构造点**。
* `ctx.ui` 显示通路：区域类 **5/5**、文本类 **1/3**（`setTitle` 有、`setEditorText`/`setTheme` 无）、
  **`footerData` 4/4**（本轮：0/4 → 4/4）。
* **加权完成度 87.2%（87.173% → 87.179%）**：唯一动的是第 12 轴（测试，权重 5%），
  +0.0011×5 = **+0.006pt**。第 9 轴（扩展宿主，8%）**不抬**：§3.6 的缺口是另外 9 个
  `pi.*` 宿主 API（`setActiveTools` / `setThinkingLevel` / `setLabel` / `registerShortcut` /
  `registerFlag` / `registerEntryRenderer` / `registerMessageRenderer` /
  `registerMarkdownTransformer` / `getAllTools`），`footerData` 只是其中一格，
  按 §0.8「不靠重估旧轴抬分」的规矩保留 95%。

## 5. 证据分级与截图

### 5.1 本轮给 harness 加的能力

`scripts/pty_capture.py` / `pty_capture_win.py` 新增场景字段 **`git_init`**：在子进程 cwd 里
`git init --initial-branch <name>` 并**建一个空提交**（让 `HEAD` 是「已出生」的，否则
`git checkout --detach HEAD` 在未出生分支上会失败），两个后端同语义。这样「分支相关」的场景
可以用**真仓库**跑，而不是伪造 `.git/HEAD`。

### 5.2 截图（**ConPTY 实拍**，不是冻结帧）

`docs/screenshots/lum1490-footer-data.png`（+ 可 grep 的 `.png.txt`，100×26，4 面板，
**12 PASS / 0 FAIL**）：

1. 启动：自定义 footer 打出 `branch=lum1490-main`（`changes=0`、`providers=32`）——
   证明 `getGitBranch()` 走的是**真查询**而不是硬编码。
2. `! git checkout -b lum1490-side`：footer 变 `branch=lum1490-side` 且 `changes=1` ——
   证明每帧 `HEAD` 重读 + `onBranchChange` 真回调（这是第一次跑出 `changes=0` 的那一格）。
3. `/default-footer`：内置 footer 的 location 行显示 `(lum1490-side)` ——
   证明**同一个 tracker** 也喂内置 footer（本轮前它只在启动时解析）。
4. `git checkout --detach HEAD` 后重装自定义 footer：`branch=null` —— detached HEAD 的上游语义。

证据分级：真 `pi.exe`、真按键序列（分步发送规避 LUM-1461 的 paste-burst 分类）、真 repaint
（每帧记 `alive=True`）、真 git 仓库（harness 建的）。ConPTY 会重新渲染输出，所以布局/文本/顺序可控，
字节流是 ConPTY 的渲染结果；`footerData` 的值语义另有 6 条 Rust 测试与 12 条面板断言覆盖。

## 6. 范围之外与已知偏差

1. **轮询而不是 `fs.watch`**：上游监听 `HEAD`（含 reftable 目录），本端口每帧读一次。差别是
   延迟上限（≤ 一个渲染节拍 ≈ 50ms）与开销（一次 `stat` + 一次读/帧）；离开仓库、`git init`、
   worktree gitdir 移动都能被重新解析捕到，但没有「无帧活动时不读文件」这条优化。
2. **`onBranchChange` 只带迁移，不带「与上次渲染相比」**：与上游一致（不带参数），但本端口
   订阅者集合是**会话级**的，插件若在 `dispose` 里忘记退订，会一直持有（上游同样是显式退订）。
3. **`getAvailableProviderCount()` 是启动时算的**：`options.models` 在本端口是会话固定的目录，
   插件在运行中注册新 provider（`pi.registerProvider`）不会改变这个数 —— 上游会通过
   `setAvailableProviderCount` 在模型目录变化时更新（`interactive-mode.ts:4903`），本端口暂无这条通路。
4. **`getExtensionStatuses()` 仍是 shim 侧的表**：本端口 `setStatus` 同时镜像到宿主（footer 第三行），
   但表本身在 JS 侧；宿主与 shim 的表理论上可能不同步（例如宿主自己清空状态行时），上游也是
   shim 之外的 provider 持有 —— 记为待观察项。
5. **非交互模式**：没有 region 桥时 `getGitBranch()` 回答 `null`、`getAvailableProviderCount()` 回答 `0`
   （未推送的快照默认值），与上游「尚未 resolve」一致；`setFooter` 本身在非交互模式仍是 denial。

## 7. 复现命令

```bash
cd pi-rust
cargo fmt --all -- --check
cargo clippy -p pi-tui --all-targets -- -D warnings
cargo test -p pi-tui --no-fail-fast
cargo test -p pi-extensions --no-fail-fast --test host footer
cargo test -p pi-coding-agent --no-fail-fast
cargo test -p pi-coding-agent --lib footer

# 真终端证据（Windows / ConPTY；Linux 用 scripts/pty_capture.py，同一份场景 JSON）
cargo build -p pi-coding-agent --bin pi
cd .. && python pi-rust/scripts/pty_capture_win.py \
    --bin pi-rust/target/debug/pi.exe \
    --steps pi-rust/scripts/pty_scenarios/lum1490-footer-data.json \
    --out pi-rust/docs/screenshots/lum1490-footer-data.png
```

## 8. 槽位 / 派发：本轮零派发

开工时在办面：LUM-1434（CLI flag 对齐，`in_progress`，另一 agent）+ 本轮 = 2 槽位，未满 3。
本轮**仍零新 run**，理由与上一轮相同：剩余候选（文本类显示通路 `setEditorText` / `setTheme`）
与本轮改的是**同一批文件**（shim + `host.rs` + `wiring.rs` + 驱动），并发会撞缝。
计划已单子化（**backlog，不启动 run**）：**LUM-1493** —— 文本类显示通路收尾
（`ctx.ui.setEditorText` / `ctx.ui.setTheme`，`ctx.ui` 文本类 1/3 → 3/3），并写明
「不要与 `footerData` 后续项并发」。下一轮顺序：LUM-1493 → 与它不冲突的
`44×16` 窄视口 `cut above` 提示 / 正文首行叠字（纯几何，可并发）。

**顺带清理建议（不在本轮动作内）**：backlog 里的 **LUM-1483**（`ctx.ui.setStatus` → footer 第三行）
已由 LUM-1481 交付、**LUM-1477**（Windows `Ctrl+J`）已由 LUM-1485 修复、**LUM-1479**
（`app.tree.editLabel`）已由 LUM-1263 交付，三条前提均已失效，建议由人工关闭。
