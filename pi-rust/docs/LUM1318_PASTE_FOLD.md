# LUM-1318 — 大段粘贴折叠：独立验收 + 删除后编号连续（上游 `higherIds`）

> scope（本轮实际改动）：`pi-rust/crates/pi-tui/src/editor.rs`（`remove_paste_marker` +
> `renumber_paste_markers_after` + `rewrite_marker_id`）、
> `pi-rust/crates/pi-tui/tests/composer_paste.rs`（+4 条）、
> `pi-rust/scripts/pty_scenarios/lum1318-paste-fold.json` + `docs/screenshots/lum1318-paste-fold.png{,.txt}`、
> `pi-rust/scripts/pty_capture.py`（panel 的 `paste` 字段）
> branch: `work/LUM-1318` → `feature/pi.rs`（基线 `origin/feature/pi.rs` @ `e0ba60d5c`，含 LUM-1327/LUM-1328/LUM-1330）
> reference: 上游 `packages/tui/src/components/editor.ts` 的 `handlePaste` /
> `PASTE_MARKER_REGEX` / `pastes` 表 / `higherIds` 重编号 / `expandPasteMarkers`；
> 姊妹文档 `docs/LUM1328_PASTE.md`（落地实现的模型与偏差）

一句话结论：composer 的大段粘贴折叠（`[paste #N +M lines]`）在 `feature/pi.rs` 上已由 **LUM-1328**
落地；本轮做两件事——(1) 用**另一套真 PTY 场景**独立验收这条链路，找到并修掉它与 LUM-1318 验收项
的**唯一差异：删除标记后不重编号**（上游 `higherIds`，本轮补齐）；(2) 如实记录两套实现的取舍，
不把第二个粘贴模型再落进同一个 crate。

---

## 1. 本轮之前发生了什么（真实审计）

LUM-1318（LUM-1312 后续 2/3）与 LUM-1328（并行的另一条 autopilot 轮次）**各自实现了一遍**
这个功能：

| | LUM-1318 本轮之前（哨兵模型，本分支 `3ad6cc2cd`） | LUM-1328（已落地 `feature/pi.rs`，`6631b8c77`） |
|---|---|---|
| buffer 内容 | 一个哨兵字符 `PASTE_CHAR`（U+FFFB） | 标记**文本**本身（`[paste #1 +123 lines]`），同上游 |
| 载荷表 | `pastes: Vec<Paste>`，编号 = 出现序号 | `pastes: BTreeMap<u32, String>` + 单调 `paste_counter`，同上游 |
| 删除后重编号 | 位置即编号，`drain` 的副产品 → **自动连续** | 留空号（删 `#1`，`#2` 仍是 `#2`），文档 §6.1 记为偏差 |
| 粘贴清洗 | 无（只 `strip_sentinels`） | CSI-u 解码 / CRLF+tab 归一 / 控制字符过滤 / 路径补空格（上游 `handlePaste` 那一套） |
| 指针（LUM-1327） | 需要把标记算成整条渲染宽度（`raw_byte_for_display_offset` 加分支） | 标记就是普通文本，天然对齐，无需额外映射 |
| 测试 / PTY | 10 单测 + 8 App 测试 + 22 断言 | 23 App 测试 + 17 断言 |
| 文档 | `LUM1318_PASTE_FOLD.md` | `LUM1328_PASTE.md` |

**取舍**：LUM-1328 的 marker 模型与上游 `handlePaste` 同构（清洗、原子段、`expandPasteMarkers`
都能逐条对照），且已合入并被其他轮次引用；本轮**不再把哨兵模型落进 `feature/pi.rs`**
（同一个 crate 里放两套粘贴模型只会让后续每个改动分叉）。因此 `work/LUM-1318` 的最终形态是
**LUM-1328 的模型 + 本轮补齐的缺口**，哨兵实现只留在本分支历史里作为记录。

## 2. 独立验收：找到的唯一不达项

用 LUM-1318 自己的真 PTY 场景（`lum1318-paste-fold.json`，100×30、`--model faux/faux-model`）
打在 **LUM-1328 的二进制**上（改动前，`/tmp/pi-prefix-1318`）：

```
21 条断言：20 PASS / 1 FAIL
FAIL  expect  '> ▍ summary[paste #2 +11 lines]'   # 实际 '> ▍ summary[paste #3 +11 lines]'
```

面板 9：草稿里有两个标记（`[paste #2 +200 lines] summary[paste #3 +11 lines]`），
`Ctrl+A`+`Del` 删掉第一个 → **活下来的那个没有重编号**（仍是 `#3`）。
其余 20 条（折叠成一行、`Enter` 交全文、召回仍是标记、按键重放 A/B 不折叠…）在落地实现上本来就过。

这正是 LUM-1318 验收项里的一条（“删除后编号连续”，上游 `higherIds`），
而 LUM-1328 文档 §6.1 把它记为“留作后续 issue”（理由是 char-indexed buffer 上做
“全局重排 + 光标位移”风险大于收益）。本轮把它补上，代价比预想小：标记是纯 ASCII 文本，
只改 `#` 后那几位数字，行布局（`visual_text`）每帧从 buffer 现算，没有第二份布局状态。

## 3. 本轮改动：上游 `higherIds` 语义

`editor.rs`（`remove_paste_marker` 尾部调用新函数）：

```rust
fn remove_paste_marker(&mut self, span: PasteMarkerSpan) {
    ... // 原样：删文本、把光标留在原来那一侧
    self.pastes.remove(&span.id);
    self.renumber_paste_markers_after(span.id);   // ← 本轮新增
}

fn renumber_paste_markers_after(&mut self, removed_id: u32) {
    // 注册表先按 id 升序下移一位（后缀滑进被删 id 空出的槽，不会撞车）
    let higher: Vec<u32> = self.pastes.keys().copied().filter(|id| *id > removed_id).collect();
    for id in higher {
        if let Some(content) = self.pastes.remove(&id) { self.pastes.insert(id - 1, content); }
    }
    // 再改标签文本：从后往前，因为 `#10`→`#9` 会让 buffer 变短
    let mut spans = paste_marker_spans(&self.buffer);
    spans.sort_by_key(|span| std::cmp::Reverse(span.start));
    for span in spans {
        if span.id <= removed_id { continue; }
        let old = self.buffer[span.start..span.end].to_string();
        let new = rewrite_marker_id(&old, span.id, span.id - 1);
        ... // 重写 + 光标位移（光标在被改标签之后时按长度差平移）
    }
    self.preferred_col = None;   // 草稿在光标下变了，Up/Down 的缓存列失效
}
```

边界与口径（都与上游一致，不粉饰）：

* **计数器不递减**：上游不重置也不递减 `pasteCounter`，所以“删掉 `#1` 后**新**粘贴”仍可能拿到
  `#3`（存活的是 `#1`）。连续的是**草稿里现有的标记**，不是计数器。`a_new_paste_after_a_deletion_takes_the_counter_on`
  把这条钉死。
* **只挂在整块删除路径**：`Backspace`/`Delete` 命中整标记时（`remove_paste_marker`）会重编号；
  行域 kill 把标记**文本**切掉时注册表留下孤儿项（与上游同样：`expandPasteMarkers` 只认识
  buffer 里还在的 id），孤儿项不参与渲染，也不会与下移后的 id 撞车。
* **光标**：删除时光标就在被删标记处，所以“标签重写要位移光标”这条分支在这两条按键路径上
  走不到，代码里保留为防御（将来若有别处整块删标记仍正确）。

测试（`crates/pi-tui/tests/composer_paste.rs`，+4 条，共 27 条）：

| 用例 | 断言 |
|---|---|
| `deleting_the_first_marker_renumbers_the_survivor_and_keeps_its_content` | 删 `#1` → `#2` 变 `#1`，`paste_content(1)` 是第二段载荷，`expanded_text` 也是第二段 |
| `deleting_a_middle_marker_keeps_the_numbering_contiguous` | 三个标记删中间 → `[1,2]`，`#3` 的载荷跟着滑到 `#2` |
| `renumbering_moves_the_caret_with_the_shorter_label` | 11 个标记删 `#1` → `#11`→`#10`（buffer 真的变短），id `1..=10`，展开文本仍是 10 段 |
| `a_new_paste_after_a_deletion_takes_the_counter_on` | 计数器单调：删 `#1` 后新粘贴是 `#3`，两份载荷各自正确 |

## 4. 真 PTY 证据（改动前 / 改动后，同一 scenario）

```bash
python3 scripts/pty_capture.py --bin target/debug/pi \
  --steps scripts/pty_scenarios/lum1318-paste-fold.json \
  --out docs/screenshots/lum1318-paste-fold.png \
  --text-out docs/screenshots/lum1318-paste-fold.png.txt
```

| | 断言 | 失败项 |
|---|---|---|
| 改动前（`/tmp/pi-prefix-1318`，= LUM-1328 的实现） | **20 PASS / 1 FAIL** | 面板 9：删掉第一个标记后幸存者仍是 `#3` |
| 改动后（本轮 `target/debug/pi`） | **21 PASS / 0 FAIL**（`PY-EXIT=0`） | — |

关键帧（字符网格与 `frame`/`px` 哈希都在 `.png.txt` 里逐条可查）：

| 面板 | 结论 | 关键帧 |
|---|---|---|
| 1 | idle，composer 一行 | `676c29a93a8b` |
| 2 | 200 行 bracketed paste → **一行** `> [paste #1 +200 lines]▍`；正文一个字都没进屏幕 | `11d1f46db215` |
| 3 | `Enter` 提交：transcript 出现 `pasted line 200`，且 `reject '[paste #'` → 模型拿到全文 | `bebf837a591a` |
| 4 | `Up` 召回：草稿又是标记本身（注册表跟着 history 条目走），草稿仍然只有一行 | `f0e950128e3e` |
| 5 | `Ctrl+C` 清空召回的大草稿，网格与面板 3 **逐字节相同**（帧哈希同为 `bebf837a591a`） | `bebf837a591a` |
| 6 | 清空后再粘一次：id 来自单调计数器 → `#2`（上游不重置 `pasteCounter`） | `f69c7692dc3d` |
| 7 | 标记就是普通草稿文本：` summary` 打在它旁边 | `c1f7afddb0c3` |
| 8 | 第二个 11 行粘贴 → `#3` | `2bddbf478e1b` |
| 9 | `Ctrl+A`+`Del` 删掉第一个标记 → 幸存者**重编号为 `#2`**，且显示自己的 `+11 lines`（`reject '+200 lines'`） | `c87e4700e697` |
| 10 | 紧接着 `Enter`：transcript 出现 `second paste row 11` → 重编号后**载荷没串位** | `4b1491bede18` |
| 11 | 同进程内 A/B：同样 200 行**按键重放**（无 bracketed paste）→ 不折叠、草稿堆满窗口（`↑` 上方 61 行） | `f9f2af3f0294` |

实测数字：折叠后 composer **1 行**；面板 11 的按键重放 6 秒内只推进到 `pasted line 68`（≈1023 B，
与 LUM-1328 记录的同一条既有读取缺陷一致，见 §6）；本次 21 条断言全部由 pyte 网格判定，
没有“人工描述的成功”。

## 5. 门禁（最终树实测）

```console
$ . pi-rust/scripts/toolchain.sh            # rustc 1.85.0 (4d91de4e4 2025-02-17)
$ CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_NET_OFFLINE=1 \
    cargo fmt --all -- --check
FMT-OK
$ ... cargo clippy --workspace --all-targets --locked -- -D warnings
Finished `dev` profile [unoptimized] target(s) in 32.22s
CLIPPY-EXIT=0
$ ... cargo test --workspace --locked -- --test-threads=2
TEST-EXIT=0
# 171 suites：2685 passed / 0 failed / 2 ignored（9m08s）
```

环境事实（共享盘，不粉饰）：`/` 是 50G overlay、4~6 路并发 run 共用，本轮开工时曾 0 字节可用，
期间出现过两次**与代码无关**的红：一次 `cargo test --workspace` 在磁盘打满时挂
`pi-coding-agent` 的 `interactive::*`（panic 全是 `tempdir: Custom { kind: StorageFull, … }`），
另一次 `history_file::clear_history_deletes_the_file_and_reports_the_path` 报
“did not exit promptly: 41.4s”（当时 `df` 可用 0），单独重跑 3/3 全绿（0.00s）。
磁盘回落后重跑同一条命令得到上面的 exit 0。为腾出构建空间，本轮清理了**已收工轮次**的
`target/` 构建缓存（`work/LUM-1318` 上一轮、`work/LUM-1327`、`lum-1305` 等，均为可重建产物），
未动任何源码或 worktree。

## 6. 偏差与限制（承接 `LUM1328_PASTE.md` §6，本轮新增一条）

1. **按词移动不吃 marker**（承接 LUM-1328）：`Alt+B/F` 会走进标记内部（不坏，只是多按几次）；
   `←`/`→`/退格/前删已经原子。
2. **kill ring 丢载荷**（承接）：跨标记的行域 kill 之后，标记文本没了、注册表留孤儿项，
   yank 出来的是被切掉的纯文本——不会复活一个指向别处的标记，与图片 chip 的既有规则一致。
3. **宽度口径 1 字符 = 1 列**（承接，全 crate 决定）：粘贴/中文在多行换行处会偏窄。
4. **本轮新记：编号仍可能跳号**。删掉 `#1` 后**新**粘贴拿到的 id 来自单调计数器（上游同款），
   所以草稿可能重新出现 `#1` 与 `#3` 并存。上游如此，本轮刻意保持同构；若产品上不接受，
   需要在计数器上另做一次全局重排（那会连 history 里已存的 id 一起动，属于新 issue）。
5. **既有缺陷（两轮都未修）**：一次性向 stdin 写入 >1 KiB 的**按键字节**时，驱动读取循环会在读完
   约一个 1024 字节块后停住（进程仍活、仍写帧）。60 行按键正常，200 行停在 68 行；用不含本次
   改动的 `work/LUM-1327` 二进制可复现同样的停顿。真实粘贴走 bracketed paste 不再经过这条路径，
   但大块按键字节仍是问题，建议单独立项。

## 7. 交付清单

| 文件 | 改动 |
|---|---|
| `crates/pi-tui/src/editor.rs` | `remove_paste_marker` 调用新的 `renumber_paste_markers_after`；新增 `renumber_paste_markers_after` / `rewrite_marker_id` |
| `crates/pi-tui/tests/composer_paste.rs` | +4 条重编号用例（27 条） |
| `scripts/pty_scenarios/lum1318-paste-fold.json` | 11 面板 / 21 断言的独立验收场景（对齐落地模型的 id 口径 + 重编号断言） |
| `docs/screenshots/lum1318-paste-fold.png{,.txt}` | 改动后的真 PTY 截图 + 字符网格 |
| `scripts/pty_capture.py` | panel 的 `paste:` 字段：把面板内容按 bracketed paste（`\x1b[200~…\x1b[201~`）发送 |

上游对照的一句话总结：`higherIds` 的重编号语义已在端口落地（`higherIds` → 注册表下移 + 标签重写，
`expandedText`/`getText` 的分工沿用 LUM-1328 的实现）。
