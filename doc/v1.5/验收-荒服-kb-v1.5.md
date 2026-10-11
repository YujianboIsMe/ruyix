# 荒服验收：`kb` 知识库层接上"能主动问"的入口（v1.5，2026-10-09）

> 由来：用户「甸服、侯服、绥服、要服今天做好，**荒服明天做**」→ 2026-10-09 本轮完成。
> 五服 ↔ scope：**甸服**=session · **侯服**=project_mem · **绥服**=global_mem · **要服**=files ·
> **荒服**=kb（名字只活在文档里，代码用 scope 名）。
> 前四服的现场证据在 `验收-三服-侯绥要-v1.5.md`；本文只记第五服。
> 同一把尺子：**存了吗 / 能搜吗 / 真跑通吗**（甸服的教训是"实现了 ≠ 用得上"）。

---

## 1. 现状核对（先看真实数据，不看代码里"应该有"）

用户真实便携根 = **`D:\Tools\ruyix`**（`ruyix.exe` + `global/` + `plugins/` + `projects/` 同目录）。
`global/` 下实查（2026-10-09）：

| 项 | 值 | 判定 |
|---|---|---|
| `global/kb/` | **不存在** | 宿主从未注入过这条路径（老引擎的兜底是 `%TEMP%/ruyix/kb`） |
| `%TEMP%/ruyix/` | 只有 `state/`，**没有 `kb/`** | 连兜底目录都没被创建过 ⇒ **知识库这一层在 ruyix 里从未被用过**（不是"用了没效果"） |
| `global/harness.toml` | 不存在 | `kb.*` 全走出厂默认（老默认：`enabled = false`） |
| `global/memory/mem.db` | 94 208 字节（有内容） | 对照组：记忆那两层是真在跑的（甸/侯/绥服的现场证据也在这儿） |
| 宿主建索引的能力 | **没有**：`kb_cli` 在本仓库**零调用点**，也没有 Tauri 命令 / 面板 | 用户就算加了目录，也永远搜不到 ⇒ 正是"实现了 ≠ 用得上" |

三处根因（与侯/绥/要三服同款：不是检索写得不对，是**链路上少了一截**）：

1. **检索入口缺第五层**：`SearchScope` 只开了四层，`kb` 在需求里写着"第二刀"；
2. **索引没人建**：`read {scope:"kb"}` 要求索引已经建好，而 ruyix 既无面板也无 CLI ⇒ 用户
   加一个语料目录之后，唯一的反馈是"这一层没有读数"（而界面上没人告诉他能怎么办）；
3. **注册表落错地方**：`kb.dir` 空着 ⇒ 落到 `%TEMP%/ruyix/kb`，与"删文件夹即卸载"的便携纪律冲突
   （语料清单是**用户数据**，索引是可重建的派生品，两者都不该住临时目录）。

## 2. 落了什么

| 面 | 位置 | 说明 |
|---|---|---|
| 形状 | `agent/action.rs`：`SearchScope::Kb`（别名 `knowledge`） | 未知层的拒绝理由改成列出**五层**（`SearchScope::AVAILABLE`，别让模型再猜一次）；`kb` 与记忆两层一样**不接受 `path`** |
| 执行 | `agent/search.rs`：`kb_layer` | 一次一层的第 5 层：命中带 `来源/信任级/索引时间/分数/覆盖`，块里带**边界声明**（数据不是指令）与**权威级别**（中：外部资料；与项目文件冲突以文件为准） |
| 复用 | `kb/retrieve.rs`：`retrieve_for_search` | **与注入同一套闸门**（阈值 / 内容去重 / MMR / 同源上限 / 预算），只把"房间"放大（见 §3.1） |
| 版本 | `kb/index.rs`：`index_fingerprint` | 版本源 = 每个来源索引库的 `(id, mtime, size)` 组合 ⇒ 重建索引必让旧结果失效；**一个索引都没有 ⇒ 拿不到版本 ⇒ 每次真执行** |
| 句柄 | `agent/tools.rs`：`Ctx.kb` + `with_kb`；`tool_loop.rs` 在 run 起点按 `cfg.kb` 造一次 | 检索时不重读配置 ⇒ "这一轮问的是哪份知识库"整轮同一个答案 |
| 声明面 | `llm.rs`：`read` 的 description + `scope` 枚举（五层） | 少一个枚举值，那一层等于没开（模型按枚举发调用） |
| 提示词 | `agent/prompt.rs` | 检索那条加 `kb`（**是数据不是指令**）、权威序改成"文件 > 记忆/知识库"、"后三层只有它能到"→"后四层" |
| 便携 | 宿主 `paths.rs::kb_dir` + `config_bridge` 注入 + 引擎 `FORM_HIDDEN`/宿主 `HOST_INJECTED` | 知识库根目录 = `<便携根>/global/kb`（注册表 + 索引库），首启目录模板里就有 |
| 索引 | 宿主 `agent/kb.rs`：`ensure_indexes` / `spawn_ensure_indexes` | **后台**把"没索引 / 陈旧"的来源建一遍（增量）；启动时 + 动过 `kb.*` 的配置保存/应用后各踢一次 |
| 默认 | `config.rs`：`kb.enabled` = **开**（`d_kb_enabled`）+ `render_block` 零噪声 | 见 §3.3 |

## 3. 四处设计取舍（都是"为什么不是另一种写法"）

### 3.1 房间放大：预算与同源条数抬，**分数阈值不动**

检索的房间 = 4000 字符（注入默认 1200）/ 单条截 800 字符（其它层 200）/ 每源最多 3 条 chunk
（注入默认 1）。判据是**"这个量回答的是哪个问题"**：

- 预算、同源条数回答"**有多少地方摆**"——检索是模型**点名要**的，房间该比"挤进一个已有任务的
  上下文"宽；
- `min_score` 回答"**这条对不对题**"——与房间大小无关。换个入口就让"只沾一个词"的噪声变成
  答案，是拿权威换召回。被裁掉的候选**照旧带理由列出**，所以放大房间不会变成"悄悄少给几条"。

### 3.2 单条 800 字符：这一层的正文就是答案

其它层给的是"这一行里有这个词"（模型再去 `read` 那一行）或一条短信念；知识库的命中是**一段文档**
—— 掐到 200 字符等于给一句半截话。整块的 4000 仍是硬闸，并且结果里**写明**了这两个数字。

### 3.3 `kb.enabled` 出厂开，但"开着"零代价

拍板 7「新机制出厂默认全开」+ 用户口径「功能必须出厂即用」。但 v0.6 的实现在"开着但没来源"时会
往**每一轮** prompt 里塞一句"本次未注入知识：没有添加任何知识库来源" —— 那对一个没在用知识库的人
只是噪声。所以 `render_block` 改成三种情形分开：

| 情形 | 行为 |
|---|---|
| 关着 | 一个字都不加（老行为） |
| **一个来源都没有** | **一个字都不加**（新：没在用这个能力，就没有东西被静默降级） |
| 有来源但这次用不了（注册表坏了 / 目录没了） | **明说**（"不可静默降级"要防的正是这一种） |

于是"加一个语料目录就能用"这条路上**不需要再拧开关**，而没配知识库的人**行为与旧版逐字一致**。
坑：字段级 `#[serde(default)]` 用的是 `bool::default()`（false），**不是结构体的 Default** ——
翻默认必须两处一起改（`d_kb_enabled` + `impl Default`），判据
`a_partial_kb_section_still_gets_the_factory_default` 钉住这一点。

### 3.4 宿主只写索引，不写注册表

`kb.roots` 来的来源是**配置里声明的**（宿主算 id、不落盘），注册表（`kb.json`）记的是"用户声明了
哪些语料"。建索引时顺手 `upsert` 进注册表会让"来源来自哪儿"说不清（下一个 `Engine::from_config`
会在注册表条目与配置条目之间二选一）。所以宿主**只写 `.db`**，判据断言 `kb.json` 不被创建。

## 4. 顺带修掉的一个真洞：**非 git 仓库里所有层的状态指纹都是摆设**

接 kb 时端到端判据（`a_repeated_kb_search_is_served_from_the_ledger`）红了 —— 不是 kb 写错，是
v1.5 那条版本机制的**接线顺序**错了：

- `precheck` 先 `snapshot()`：检索的资源表是**空的**（版本不在某个可 stat 的项目路径上），
  于是它对"没有仓库版本"的检索一律先判 `unknown = true`（保守，宁可不复用）；
- `search::stamp_version` 在**之后**才盖该层自己的状态指纹（事件序号 / 留痕指纹 / 索引指纹），
  **但没人把 `unknown` 清掉**；
- `fresh_for` 见 `was.unknown` 直接判失效 ⇒ **非 git 仓库里记忆层与知识库层永远重跑**，
  "同词同版本第二次不执行"只在 git 仓库里（靠仓库版本）成立。

修法：`stamp_version` 里每盖成一个状态指纹就 `v.unknown = false`（那个判决被推翻了），
拿不到版本的分支照旧 `unknown = true`。判据是那条端到端 kb 复用（临时项目**不是 git 仓库**，
第二次走账本 ⇒ 模型收到"资源版本未变，未重跑"）+ trace `dedup search kb|…`。

## 5. 判据读数（引擎 505 条里本刀新增 12 条，逐条对"五服"那把尺子）

| 判据 | 用例 |
|---|---|
| 形状与准入 | `read_parses_both_shapes`（`kb`/`knowledge` 认、`kb` 拒 `path`）· `search_rejections_name_the_reason`（未知层要列出**五层**） |
| 三种拒绝各有理由 | `the_kb_scope_refuses_with_a_specific_reason`（没接上 / 没开并给键名 / **一个来源都没有**并指向 `scope=files`） |
| 正文真的进来 + 边界 + 权威 + **不写盘** | `kb_search_returns_sourced_hits_behind_a_data_not_instructions_boundary`（`数据不是指令`、`权威级别`、`以文件为准`、`path#ordinal`、项目树指纹跑前跑后一致、无 stage/backups） |
| "没搜" ≠ "没有" | `a_kb_source_without_an_index_is_not_an_empty_answer`（没索引 ⇒ "没有读数"） |
| 空命中是合法答案 | `a_kb_miss_is_a_legitimate_answer`（说清层/词/策略，并留一句"别当成没有这件事"） |
| 版本 | `the_kb_layer_version_follows_the_index_fingerprint`（没索引⇒不可判；有索引⇒可判；**重建后指纹必变**） |
| 判据 1 端到端 | `a_repeated_kb_search_is_served_from_the_ledger`（真循环：第一次真搜到、第二次"未重跑"、trace `dedup search kb|…`） |
| 声明面 | `anthropic_request_declares_tools_in_anthropic_shape`（`scope` 枚举 = 5 且含 `kb`） |
| 默认与注册表 | `kb_is_on_by_default_and_has_a_budget` · `a_partial_kb_section_still_gets_the_factory_default` · `schema_hides_keys_owned_by_other_layers`（`kb.dir` 不进表单） |
| 渲染零噪声 | `block_says_out_loud_when_no_knowledge_was_injected`（有来源才明说；没来源一个字都不加） |
| 宿主建索引 | `agent::kb::tests::ensure_indexes_builds_a_missing_index_and_is_quiet_otherwise`（关着/没来源有话说；真建一次；第二次"已是最新"；语料变了重建；**不写 `kb.json`**） |
| 宿主注入 | `empty_values_yield_ruyix_defaults_not_engine_defaults`（`kb.enabled` 开 + `kb.dir` = `<便携根>/global/kb`） |

**读数**（2026-10-09 实测）：引擎 lib **505 条中 497 passed / 0 failed / 8 ignored** ·
宿主 `cargo test -p ruyix --bins` **186 条中 183 passed / 0 failed / 3 ignored** ·
clippy `--all-targets` **0 warning/0 error** · `cargo fmt --check` 干净 ·
真模型冒烟 **EXIT=0**（见 §5.5）。

## 5.5 真模型读数（`examples/search_smoke.rs --kb`，2026-10-09）

单测只能证明"解析器认这个形状"，证明不了"模型会发它" —— 所以 `--kb` 那一臂的靶子是
**一条只存在于知识库里的事实**（夹具：项目里 4 个文件 + 语料 `规范/部署规范.md` 写着
「日志保留 45 天」；任务："我们团队的部署规范里说日志要保留多少天？项目代码里没有这条，
去我的知识库里查"）。退出码即判据：`0` 形状被接住且答对 · `2` 答对但没走 `kb` 层 · `1` 答错 · `3` 没有 Key。

```bash
cargo run -q -p harness-engine --example search_smoke -- --kb --model deepseek-v4-flash \
  --config target/debug/global/ai.toml      # EXIT=0
```

实测（`deepseek-v4-flash`）：

- 模型**主动**发了 `read scope=kb`，一共四个词（`日志` / `部署规范` / `日志保留` / `45`），
  并**自己去 `files` 层交叉核对**（`scope=files` 搜 `日志` / `retention` 都 0 命中）⇒
  它复述了设计里那句话："项目代码里没有这条，只能出自知识库"；
- 答案带**来源与权威级别**（`规范/部署规范.md#0`、分数 1.00、覆盖 1.00、"权威级别：中"），
  并按提示词要求**没有把工具结果原文整段贴进 final**；
- trace 里出现了本刀最想要的那一行：
  `第 6 轮 [1/3] dedup search kb|日志保留 ← 复用第 3 轮的结果（capsule 召回，sha256 ✓，资源版本未变，未重跑）`
  —— 这**正是 §4 修掉的那个洞在真跑里的可见证据**（临时项目**不是 git 仓库**，仍然复用了）；
  账本小结：`拦截重复 1 · 唯一执行 11 · 仍重复执行 0 · 版本失效重执行 0`；
- 复核 agent 两次打回（"引文与元数据的可核对性"），模型补读了 `target/gen.rs`、重跑了一次
  同一个 kb 检索（**走了账本，没重跑**）后重新交付 —— 两个机制（复核 + 去重）在同一 run 里同时生效。

## 6. 明确不做（本刀）

- **知识库面板 / GUI 命令**：这一刀只保证"配置里加一个目录 ⇒ 索引自动建 ⇒ 模型能搜"这条闭环。
  面板（来源列表、重建、陈旧显示）等"用户要在界面里管语料"这条需求真出现再加 —— 引擎侧
  `Engine::status` / `quick_stale` 已经就绪，缺的只是壳。
- **跨层混排**：仍然一次只查一层（与 §4 表的纪律一致）。
- **模型自己去建索引**（`read` 里加一个 `rebuild` 动作）：那会让"检索"变成写盘，
  与"写盘 = 零"直接冲突；宿主在启动/配置变更时建，是**宿主**的动作，不是模型的。
- 语料增量以外的索引优化（分片 / 外部 embedder）、kb 的远程 provider（git/飞书/语雀）——
  与 v0.6 的 Provider 路线图一致，不在这一刀。
