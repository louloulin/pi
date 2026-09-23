# LUM-1467 — footer stats 行对齐上游（`↑/↓`、`CH%`、`$cost`、`(auto)`、`(provider)` 前缀）

上级任务：LUM-1466（两行 footer，`docs/LUM1466_TWO_LINE_FOOTER.md`）。本轮只动**第二行
（stats 行）**：不改行数、不改位置行、不碰 `plan_chrome` / `ExtensionFrame::status`、
不碰 `pi-extensions`（`ctx.ui.setStatus` 的第 3 行是后续任务）。

## 1. 上游取证

第一手来源 `packages/coding-agent/src/modes/interactive/components/footer.ts`：

```ts
// footer.ts:88-98   最近一次 prompt 的 cache hit rate
const latestPromptTokens = usage.input + usage.cacheRead + usage.cacheWrite;
latestCacheHitRate = latestPromptTokens > 0
  ? (usage.cacheRead / latestPromptTokens) * 100
  : undefined;

// footer.ts:130-146  statsParts 的字段与顺序
if (usageTotals.input)      statsParts.push(`↑${formatTokens(usageTotals.input)}`);
if (usageTotals.output)     statsParts.push(`↓${formatTokens(usageTotals.output)}`);
if (usageTotals.cacheRead)  statsParts.push(`R${formatTokens(usageTotals.cacheRead)}`);
if (usageTotals.cacheWrite) statsParts.push(`W${formatTokens(usageTotals.cacheWrite)}`);
if ((cacheRead > 0 || cacheWrite > 0) && latestCacheHitRate !== undefined)
  statsParts.push(`CH${latestCacheHitRate.toFixed(1)}%`);
if (usageTotals.cost || usingSubscription)
  statsParts.push(`$${usageTotals.cost.toFixed(3)}${usingSubscription ? " (sub)" : ""}`);

// footer.ts:149-176  context 百分比 + auto 标记；>90 error / >70 warning / else dim
const autoIndicator = this.autoCompactEnabled ? " (auto)" : "";

// footer.ts:183-197  右侧 model，多 provider 时加前缀，放不下先去掉前缀
if (this.footerData.getAvailableProviderCount() > 1 && state.model) {
  rightSide = `(${state.model.provider}) ${rightSideWithoutProvider}`;
  if (statsLeftWidth + minPadding + visibleWidth(rightSide) > width) {
    rightSide = rightSideWithoutProvider;   // fall back
  }
}

// footer.ts:205-215  statsLeft + padding + rightSide（model 右对齐）
```

`usingSubscription`（`footer.ts:139-140`）= `provider === "kimi-coding" || modelRuntime.isUsingSubscription(provider)`。

**两处与旧 pi-rust 不同的形状**（本轮一并修正）：

1. 行内排布：上游是 **stats 左、model 右**；旧 pi-rust 是 model 左、stats 右。
2. 零值隐藏：上游每个字段各自 `if`，零值**整个字段消失**；旧 pi-rust 恒定画 `in 0 out 0`。

## 2. 改动清单

### `crates/pi-tui/src/status.rs`

| 位置 | 改动 |
|---|---|
| `status.rs:41-85` | 新增 `StatusPricing`（每 1M token 的 micro-USD 单价）+ `cost_micros()`：`tokens × rate / 1e6` 逐项取整 |
| `status.rs:105-137` | `StatusData` 新增 `cost_micros`、`latest_cache_hit_rate`、`auto_compact`、`provider_count`、`provider_label`、`subscription`、`pricing`；derive 去掉 `Eq`（`f32`） |
| `status.rs:150-160`、`new()` | 默认值 `None/None/false/0/None/false/None` |
| `status.rs:253-285` | `add_usage` 累加 cost、按上游口径更新 `latest_cache_hit_rate`（`prompt_tokens > 0` 否则清空）；新增 `set_pricing` / `with_pricing` / `reset_cost` / `with_auto_compact` / `with_provider` |
| `status.rs:434-500` | `render_styled_line`：左簇 = busy + stats + hint，右簇 = model（可选 `(provider)` 前缀，放不下先回落去前缀），session 仍在中间 |
| `status.rs:556-838` | `Zone` 扩到 9 个（新增 `CacheHit`、`Cost`），重排丢弃顺序与视觉顺序，`zone_body` 按上游条件绘制每个字段 |
| `status.rs:826-834` | `format_cost(cost_micros, subscription)` → `$0.123` / `$0.123 (sub)` |

### `crates/pi-tui/src/app.rs`

`set_status_provider` / `set_status_auto_compact` / `set_status_subscription` /
`set_status_pricing`（`app.rs:2060-2090`）——驱动接线用的四个口子，与 LUM-1466 的
`set_status_cwd` 同形。

### `crates/pi-coding-agent/src/interactive.rs`

| 位置 | 改动 |
|---|---|
| `interactive.rs:1496-1527` | `sync_status_metrics(app, options, model)`：provider 数（`options.models` 去重计数）、当前 provider label、`is_subscription_provider`、`options.compaction.enabled`、`pi_ai::providers::registry::model_pricing()` 换成的 `StatusPricing` |
| `interactive.rs:1529-1537` | `is_subscription_provider`：上游的 `kimi-coding` 字面量（`isUsingSubscription` 那半没有对应实现，见 §6） |
| `interactive.rs:502` | run_loop 启动时接线 |
| `interactive.rs:1922`、`interactive.rs:1973` | 两个 model 切换点（`Ctrl+P` 循环 / `/model` 选择器）后重算 |
| `interactive.rs:3821` | `/settings` 的 `autocompact` 行实时改 footer 的 `(auto)` |

### 测试与截图

- `crates/pi-tui/src/status.rs` 单测：26 → 37（+11）
- `crates/pi-tui/tests/lum1467_stats_fields_frames.rs`（新，6 条）
- 既有断言随新形状更新：`status.rs`（LUM-1367 帧）、`tests/app_theme.rs`、
  `tests/styles.rs`、`tests/lum1466_two_line_footer_frames.rs`
- `docs/screenshots/lum1467-stats-{fields-100x30,fields-narrow-44x14,defaults-100x30}.{png,txt}`

## 3. 帧对照（100×30，真实 `App::render_to_buffer`）

同一 App（`anthropic`/`claude-sonnet-4`、cwd `/srv/repo (main) • demo`、thinking `high`、
`input=12k output=3k cache_read=12k cache_write=300 context_used=64k/200k`、
`latest_cache_hit_rate=65.0 cost=$0.123 auto=true providers=2`）。

**本轮后**（`docs/screenshots/lum1467-stats-fields-100x30.txt` 第 29 行）：

```text
↑12k ↓3.0k R12k W300 CH65.0% $0.123 32.0%/200k (auto)  ? for help (anthropic) claude-sonnet-4 • high
```

逐段对照 `footer.ts`：

| 上游字段 | 本轮帧 | 上游行号 |
|---|---|---|
| `↑12k` | `↑12k` | 130 |
| `↓3.0k` | `↓3.0k` | 131 |
| `R12k` `W300` | `R12k W300` | 132-133 |
| `CH65.0%` | `CH65.0%` | 134-135 |
| `$0.123`（/` (sub)`） | `$0.123` | 142-143 |
| `32.0%/200k (auto)` | `32.0%/200k (auto)` | 149-176 |
| `(anthropic) claude-sonnet-4 • high` | `(anthropic) claude-sonnet-4 • high` | 183-197 |

**本轮前**（LUM-1466 的同一格，`docs/screenshots/lum1466-footer-two-row-100x30.txt` 末行）：

```text
Faux                                                                   in 0 out 0 ?/8.2k  ? for help
```

差别正是本任务点名的四类：无箭头形状、无 `CH%`、无 `$cost`、无 `(auto)`、无 provider 前缀，
且 model 在左、stats 在右。

**默认值帧**（新字段全默认，`docs/screenshots/lum1467-stats-defaults-100x30.txt` 末行）：

```text
?/200k  ? for help                                                            claude-sonnet-4 • high
```

没有 `CH` / `$` / `(auto)` / `(provider)` —— 新字段的默认值不往帧里添任何东西。

**窄帧**（44×14，`docs/screenshots/lum1467-stats-fields-narrow-44x14.txt` 末行）：

```text
32.0%/200k (auto)  claude-sonnet-4 • high…  
```

`? for help`、`CH65.0%`、`R12k W300`、`↑12k ↓3.0k`、`$0.123`、session 段依次整段丢弃
（LUM-1367 的规则），`…` 说明这是子集；model 仍在右端。

## 4. 测试

### 单元（`pi-tui/src/status.rs`）

| 用例 | 断言 |
|---|---|
| `the_stats_cluster_matches_upstreams_field_order` | 全字段行的段序与形状逐字 |
| `each_token_arrow_hides_independently_at_zero` | 只在 `input/output > 0` 时出现 |
| `the_cache_hit_rate_needs_traffic_and_a_reported_prompt` | `(R>0或W>0) && rate.is_some()` |
| `the_cost_part_needs_a_cost_or_a_subscription` | `cost>0 \|\| subscription`；`(sub)` 形态 |
| `the_auto_suffix_follows_the_auto_compact_switch` | `(auto)` 跟随开关 |
| `the_provider_prefix_needs_more_than_one_provider` | `count > 1 && label.is_some()` |
| `the_provider_prefix_is_dropped_before_the_bar_goes_narrow` | 放不下先回落去前缀（20 列去掉、30 列保留） |
| `add_usage_records_the_latest_prompts_cache_hit_rate` | `650/(300+650+50)=65.0`；零 token prompt → `None` |
| `add_usage_accumulates_cost_from_the_installed_rates` | `$3+$15+$0.30+$3.75=$22.050`；`set_pricing(None)` 不清账 |
| `format_cost_renders_micro_usd_with_three_decimals` | `$0.000` / `$1.235` / `(sub)` |
| `a_narrow_all_fields_bar_sheds_the_cache_hit_rate_before_the_cache_totals` | 79/71/61 三档丢弃点 |
| 既有 `context_gauge_escalates_colour_past_the_thresholds` | `>70` / `>90` 阈值不变 |
| 既有 LUM-1367 帧与不变量用例 | 按新形状重写，含 `never_ends_with_a_partial_token` |

### 帧（`pi-tui/tests/lum1467_stats_fields_frames.rs`）

1. `the_stats_row_carries_every_field_in_upstream_order`：100×30，段序 + 右端 model。
2. `the_new_fields_do_not_add_a_row`：同一 App 配置，字段全开 / 全默认的
   `App::render_snapshot` 行数差 **0**，且默认帧不含 `CH` / `$` / `(auto)` / `(provider)`，
   stats 行仍紧贴位置行。
3. `a_44_column_frame_sheds_the_new_parts_in_the_lum1367_order`：44×14，整段丢弃 + `…` + 恰好 44 列。
4-6. 三个 `frame_dump_*`：出 PNG 用的帧源。

## 5. 门禁（本机 Windows / cargo 1.97.1 / `--offline`）

| 命令 | 结果 |
|---|---|
| `cargo test -p pi-tui` | **1121 passed / 0 failed**（基线 1104/0 → +17） |
| `cargo fmt --all -- --check` | 干净（exit 0） |
| `cargo clippy -p pi-tui --all-targets` | 0 warning |
| `cargo clippy -p pi-coding-agent --all-targets` | 本文件面 0 新告警（其余为 `pi-extensions` / `rquickjs-core` 既有告警） |
| `cargo test -p pi-coding-agent --lib interactive::` | **107 passed / 0 failed** |
| `cargo test -p pi-coding-agent --test startup_header --test help_text_layout --test keybindings --test print_mode --test tools_render --test builtin_tool_factories --test extension_ui --test lum1448_* --test lum1450_*` | 全部 0 failed |
| `cargo test -p pi-coding-agent --test reload_config` | `reload_rereads_keybindings_and_ui_settings_mid_session` 1 条 FAILED —— **既有失败**，`git stash` 掉本轮的 `interactive.rs` 后同一条同样 FAILED（Windows 换行把 `keybindings.json` 路径折行，断言找不到连续路径）；非本轮引入 |

**帧高不变**：`the_new_fields_do_not_add_a_row` 直接断言行数相等（30 = 30），且
`StatusData::line_count` 只看 `location_line()`，本轮未碰。

## 6. 已知偏差（写清而不是省略）

1. **cost 只在有模型价目表时才有数**。`pi-protocol::Model` / `Usage` 没有 cost 字段，
   `pi-ai` 的价目表是**静态 catalog**（`providers/registry.rs::Pricing`，micro-USD/1M）。
   驱动用 `model_pricing(provider, id)` 装价目表，`StatusData::add_usage` 自己算钱。
   catalog 没有价目的模型（以及所有自建 endpoint 模型）→ `cost_micros` 保持 `None`，
   `$` 段不出现——与上游"provider 没报 cost 就不显示"同形，但**数值来源不同**：
   上游是 provider 返回的 `Usage.cost`（含 models.dev 动态价），本端口是本地静态表。
   `/models.dev` 动态目录没有移植，故这条口径差异不会在短期内消失。
2. **`usingSubscription` 只做了字面量那半**。上游是
   `provider === "kimi-coding" || modelRuntime.isUsingSubscription(...)`；
   OAuth/订阅制 provider（`kimi-coding` / `github-copilot` / `openai-codex`）在本 build 里
   根本没注册（`pi-ai/src/auth/provider_registry.rs` 模块注释已写明），
   `isUsingSubscription` 没有可问的对象。所以订阅判定目前只认 `kimi-coding`。
3. **中段 session 段保留**（LUM-1466 §8 已知偏差 4 的延续）：上游 stats 行没有 session 段；
   本端口在位置行已经带 `/name` 时抑制它，无名字时把 session id 留在中间。本轮未改这个
   取舍，只把它移到"stats 左 / model 右"的新排布里。
4. **hint（`? for help`）在 stats 簇尾**：上游没有这一段（扩展状态是第 3 行，属后续任务）。
   放在 gauge 之后、session 之前，是为了让 model 仍占最右列，与上游 `rightSide` 同形。
5. `cost_micros` 用整数 micro-USD 而不是浮点 USD：`$x.xxx` 三位小数需要 micro 精度，
   整数可避免累加漂移；逐项取整与上游 `toFixed(3)` 在 1µ$ 量级的差别不可见。
6. **tool-result / compaction 的 usage 不计入**：上游把 `toolResult` 与 `branch_summary` /
   `compaction` 条目的 usage 也加进 `usageTotals`（`footer.ts:100-106`）。`pi-protocol` 的
   `ToolResult` 没有 usage 字段（`AssistantMessage.usage` 才有），所以本端口的 token 总数
   与 cost 都只累加 assistant 消息——这是**先于本轮**存在的 token 口径差，cost 只是继承了它，
   本轮未扩大也未收窄。

## 7. 完成度复算（`RUST_TS_PARITY_METRICS.md` §4.1）

**度量命令**（与 §0.18 同口径，本机执行）：

```bash
# Rust 用例：crates/**/src 与 tests 的 #[test]/#[tokio::test] 出现次数
python -c "import pathlib,re;print(sum(len(re.findall(r'#\[test\]|#\[tokio::test\]',p.read_text(encoding='utf-8',errors='ignore'))) for p in pathlib.Path('pi-rust/crates').rglob('*.rs')))"
# → 2790（本轮 +17：status.rs +11、lum1467 帧 +6）
# TS 用例分母沿用 §0.18 的新口径 → 5563
# 纯代码规模（src↔src）：141902 / 153106
```

| 轴 | 权重 | 本轮前 | 本轮 | 依据 |
|---|---|---|---|---|
| 1 可构建/可测/可运行 | 5% | 1.000 | **1.000** | §5 全绿 |
| 2 核心 agent 循环 | 13% | 0.90 | 0.90 | 未动 |
| 3 provider API family | 8% | 1.00 | 1.00 | 未动 |
| 4 provider/模型目录 | 6% | 0.70 | 0.70 | 未动 |
| 5 TUI 交互面 | 14% | 0.917 | 0.917 | 未加模块、未动 `app.*`（§0.18 值） |
| **6 TUI 视觉保真** | 8% | 0.90 | **0.90（不重估）** | 见下 |
| 7 slash 命令面 | 7% | 0.783 | 0.783 | 未动 |
| 8 CLI/模式/子命令面 | 7% | 0.70 | 0.70 | 未动 |
| 9 扩展宿主能力 | 8% | 0.95 | 0.95 | 未动 |
| 10 扩展生命周期事件 | 7% | 1.00 | 1.00 | 未动 |
| 11 会话/存储/导入导出 | 9% | 0.85 | 0.85 | 未动 |
| **12 测试与门禁强度** | 5% | 0.4953 | **0.5015** | 2790/5563 |
| 13 子包完整度 | 3% | 0.95 | 0.95 | 未动 |

加权和 = `5×1.000 + 13×0.90 + 8×1.000 + 6×0.70 + 14×0.917 + 8×0.90 + 7×0.783
+ 7×0.70 + 8×0.95 + 7×1.00 + 9×0.85 + 5×0.5015 + 3×0.95` = **86.93% ≈ 86.9%**。

与 §0.18 的 86.9% 相同：唯一动的是第 12 轴 **+0.015pt**（+17 条用例），其余 12 轴取值不变。

### 轴 6 是否重估：**否**，理由

- §4.1 的轴 6 依据是 **§3.8**（主题 / Markdown / 高亮 / LaTeX / 图片保真），footer 的
  stats 行不是那 5 项的证据项之一；本轮把 §6 缺口 #2 关掉，是**换证据**，不是把 90%
  这个数字往上推。
- 想重估必须先在 §3.8 里说清"哪一项从 90% 抬到多少、依据是什么"。本轮没有动 §3.8 的任何
  一项，所以不重估。把轴 6 从 0.90 改 0.95 只值 **+0.4pt**（`8×0.05 = 0.4`），
  不足以改变结论——与 §0.8 的处置同一条规矩。
- **可复现**：把上面 13 个数代进公式即可；唯一需要重测的量是 `Rust 用例数`（一条 python 命令）。

**口径诚实声明**：TS 分母 5,563 来自 §0.18 的"新口径"（收 `it.each(` 等包装）；用 §1.1 的
旧口径（`\bit\(|\btest\(`）在同一棵树上量到的是 **5,309**，对应第 12 轴 0.5255、加权
**87.05%**。两个数都对，取决于用哪个尺子；本轮沿用 §0.18 的尺子以便与上一快照逐项对照，
并在此写明换尺子的差额（+0.12pt）。
