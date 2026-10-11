# 需求：LLM 网关（故障切换 + 多协议）—— v0.5

> 分支：`0.0.5`（基于 `master` @ `626fe05`）
> 范围：harness-engine 的 LLM 调用层（`crates/harness-engine/src/llm.rs`）+ 配置系统 + 宿主桥 + 配置页
> 不在范围：宿主 `src-tauri/src/ai.rs`（AI 命令 → Lua 翻译客户端，单独一套小客户端，本次不动；后续可统一）

---

## 0. 背景与目标

当前 `llm::chat` 只认一个 DeepSeek 端点（`LlmConfig`）。两个现实痛点：

1. **单点不可用**：DeepSeek 抖动/限流/宕机时，整个 Agent 跑死，用户只能手动改配置。
2. **协议单一**：只支持 OpenAI 兼容（`/chat/completions` + `/responses`）。想用 Claude 等
   Anthropic 系模型，或自建 anthropic 协议网关，无处配置。

本版本做两件事：

- **F1 故障切换 AI 网关**：`LlmConfig` 之外再配一个**备用 LLM**；主用不可用时（网络/5xx/429/超时）
  Agent 自动切换到备用，**鉴权失败/余额不足/参数错误不切换**（那些重试也没用）。
- **F2 anthropic 协议**：每个 LLM 配置加一个二选一开关 `api_format`：`openai`（默认）或 `anthropic`。
  选 `anthropic` 时走 `/v1/messages` + `x-api-key` + `anthropic-version` 头，解析 Anthropic 响应体。

两个功能正交、可组合：备用 LLM 可以**是** anthropic 协议（异构切换），也可以和主用同协议。

---

## 1. 五个标准步骤（每个功能）

| 步骤 | F1 故障切换 | F2 anthropic 协议 |
|---|---|---|
| ① 需求/设计 | 本节 | 本节 |
| ② 配置层 | `AppConfig.llm_fallback: Option<LlmConfig>` + 宿主 `ai_fallback` 段 + 桥接 | `LlmConfig.api_format` + 宿主 `ai`/`ai_fallback` 段 `api_format` 下拉 + 桥接 |
| ③ 调用层 | `chat()` 增 `fallback` 参数；`is_switchable_error`；弹性模式（主用 2 次 / 30s 封顶）切备用 | `chat()` 内按 `api_format` 分支建请求 / 解析；anthropic 关联网检索 |
| ④ 测试 | `is_switchable_error` 单测 + ui-smoke U39（配置页含 `ai_fallback` 段） | anthropic 请求/解析单测 + `web_search_on` 对 anthropic 返回 false + ui-smoke U40（`api_format` 下拉） |
| ⑤ 文档收尾 | `CLAUDE.md` / `doc/config.md` 记录备用段与切换语义 | 同上记录 `api_format` 选项与 anthropic 限制（无联网检索、无 `response_format`） |

---

## 2. F1 故障切换 —— 设计

### 2.1 配置形态

```toml
# 主用：~/.ruyix/code/ai.toml（文件名即 section = ai，`ruyix.code.` 前缀不写进文件）
api_url    = "https://api.deepseek.com/v1"
api_key    = "sk-..."
model      = "deepseek-v4-pro"
api_format = "openai"          # 新增

# 备用：~/.ruyix/code/ai_fallback.toml（section = ai_fallback）
api_url    = "https://api.anthropic.com"   # 端点写 base；引擎自己拼 /v1/messages
api_key    = "sk-ant-..."
model      = "claude-sonnet-4"
api_format = "anthropic"      # 异构备用
```

- `AppConfig.llm_fallback: Option<LlmConfig>`，默认 `None`（不配置 = 不切换，行为等同旧版）。
- `Option` 在 `AppConfig::default()` 序列化为 absent，`schema()` 不会多出幽灵键（不污染配置表单）。
- `llm_fallback` 前缀加入 `FORM_HIDDEN`，引擎 harness 段不重复渲染（它由宿主 `ai_fallback` 段管）。

### 2.2 调用层语义

`chat(cfg, fallback, messages, json_mode)`：

- **无备用**（`fallback = None`）：与旧版**逐字节一致**——3 次重试、完整 `timeout_secs`、同样的错误文案。
- **有备用**（弹性模式）：
  - 主用最多 **2** 次尝试，单次请求超时封顶 `min(cfg.timeout_secs, 30s)`（避免主用僵尸挂死 300s 还不切）。
  - 主用最终失败且 `is_switchable_error(底层错误)` 为真 → 切备用，备用同样 2 次 / 30s 封顶。
  - 主用最终失败但**非**可切换错误（401/403/402/400/422/`is_fatal_error`）→ **立即返回，不碰备用**
    （密钥错/配额错/参数错重试也没用，且备用多半同样错）。
  - 备用也失败 → 返回组合错误，明确标注"主用不可用已切备用、备用亦失败"。

`is_switchable_error(e)`：`!is_fatal_error(e)` 且底层属于可用性故障——`网络错误` / `HTTP 5xx` /
`HTTP 429` / `超时` / `重试 N 次`。鉴权/配额/参数类一律不算。

### 2.3 为什么改签名而不是包一层

`chat` 是唯一的 LLM 出口（plan / generate / eval / agent / step_agent / repair / reflect 共 8 处）。
把切换逻辑放进 `chat` 内部，所有调用点**自动**获得故障切换能力，且 retry/超时/观测语义只有一份。
8 处调用点改为 `llm::chat(&cfg.llm, cfg.llm_fallback.as_ref(), ...)`（有 `AppConfig` 的传 `as_ref()`，
只有 `&LlmConfig` 的 `plan::generate` 增一个 `fallback` 形参）。

---

## 3. F2 anthropic 协议 —— 设计

### 3.1 配置形态

`LlmConfig.api_format: String`，默认 `"openai"`，`serde(default)`。值在宿主 `ai` 与 `ai_fallback`
段以 `select`（选项 `["openai", "anthropic"]`）呈现。`llm.api_format` 加入 `FORM_HIDDEN`
（避免与宿主 `ai.*` 段重复出现在 harness 段）。

### 3.2 协议差异（`chat` 内分支）

| 维度 | openai（含 `/responses`） | anthropic |
|---|---|---|
| 端点 | `{base}/chat/completions` 或 `{base}/responses` | `{base}/v1/messages`（base 已含 `/v1` 则不重复拼） |
| 鉴权 | `Authorization: Bearer <key>` | `x-api-key: <key>` + `anthropic-version: 2023-06-01` |
| system | 作为 `messages[0]`（role=system） | 顶层 `system` 字段，`messages` 只含 user/assistant |
| 结构化输出 | `response_format: {type: json_object}` | 无（靠提示词要求 JSON；本版本不引入 tool-use） |
| 联网检索 | `/responses` + `tools:[web_search]` | **不支持**：`web_search_on` 对 anthropic 直接返回 false |
| 用量 | `usage.prompt_tokens / completion_tokens` | `usage.input_tokens / output_tokens` |
| 结束原因 | `finish_reason` | `stop_reason`（`end_turn`→stop，`max_tokens`→length） |

解析入口 `extract_anthropic`：`content` 为 `[{type:"text", text:"..."}]` 拼接；`usage` 映射；
`stop_reason` 映射为 `finish_reason`。

### 3.3 已知限制（v0.5）

- anthropic 不支持 `response_format`，JSON 模式靠提示词保证；不强制、不报错。
- anthropic 不走服务端联网检索（语义不成立）。`web_search_on` 对 `api_format=anthropic` 返回 false。
- 不实现 Anthropic tool-use / 流式；与 openai 路径共享同一套重试与错误分类。

---

## 4. 配置页与 i18n

- `ui/config.js` 的 `SCHEMA`：
  - `ai` 段新增 `{ key: "api_format", kind: "select", options: ["openai", "anthropic"] }`。
  - 新增 `ai_fallback` 段，字段 `api_url / api_key / model / api_format`。
    **有意不带 `alias`**：别名是界面展示字段，而备用 LLM 在界面上没有展示位；
    加一个没人读的字段就是"配了不生效"的形状（`ai` 段的 `alias` 是既有字段，未动）。
  - 模型不做 `/models` 下拉：那份清单来自**主用**端点的 `/models`，对备用端点未必成立。
- i18n（`zh-CN.json` / `en.json`）：
  - `config.section.ai_fallback` = "备用 AI（故障切换）" / "Backup AI (Failover)"
  - `config.field.ai.api_format` / `config.field.ai_fallback.api_format` = "API 格式" / "API Format"
  - `config.desc.*.api_format` = 说明 openai / anthropic 的取舍。
  - `ai_fallback` 段其余字段沿用 `ai` 段的同义标签（API 地址 / 密钥 / 模型）。
  - 每个静态字段都要**同时**有 `config.field.*` 与 `config.desc.*`（U10 守这条）。

---

## 5. 测试策略

- **单测（`crates/harness-engine/src/llm.rs`）**：
  - `is_switchable_error`：网络/5xx/429/超时/重试耗尽 → true；401/402/400/422/"尚未配置" → false。
  - `anthropic_parts`：url 以 `/v1/messages` 结尾、`system` 提到顶层、无 `response_format`、messages 无 system。
  - `extract_anthropic`：真响应 → content / usage / model / finish_reason 正确。
  - `web_search_on` 对 `api_format=anthropic` 返回 false（加进既有测试）。
- **桥接单测（`src-tauri/src/agent/config_bridge.rs`）**：
  - `api_format_and_fallback_llm_bridge_from_the_host_namespaces`：走**真** `ConfigManager`，
    断言 `ruyix.code.ai.api_format` / `ruyix.code.ai_fallback.*` 真进 `cfg.llm` / `cfg.llm_fallback`
    （只测 schema 或只测 `apply_*` 都验不到"界面配了不生效"，因为这两个键都在宿主命名空间、
    不在 `schema()` 遍历里）。
  - `fallback_llm_is_enabled_by_any_single_field`：任一字段有值即启用；全空保持 `None`。
- **引擎 schema 单测（`config.rs`）**：
  - `api_format_is_a_declared_enum_hidden_from_the_harness_form`：合法值恰为 `openai/anthropic`、
    `ui == false`、默认 `openai`。
  - `the_fallback_llm_segment_never_leaks_into_the_harness_form`：默认 `None` → toml 整段省略
    → `schema()` 里没有 `llm_fallback.*`。
- **行为门禁（`scripts/ui-smoke.js`）**：
  - **U39 `failover-config`**：`ai_fallback` 段字段清单 == 桥 read 的键（跨层键契约）；
    主用段也有 `api_format`；`llm_fallback` 在引擎 `FORM_HIDDEN` 且没被抄回 `config.js`。
    **只扫桥的生产代码**（`#[cfg(test)]` 之前）—— 反向验证时实测过：整文件搜会搜到桥单测里
    同样的键名，于是把生产代码改错门禁照样绿。
  - **U40 `anthropic-format`**：两段各一个 `api_format` 二选一下拉、取值恰为 `openai,anthropic`；
    `llm.rs` 有 anthropic 三件套；`request_plan` **最先**判 anthropic；合法值在 `ENUM_KEYS`
    且被 `FORM_HIDDEN` 藏掉；新增文案中英 parity。
- **门禁全绿**后才提交：`CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets`、
  `cargo fmt --all -- --check`、`cargo test -p harness-engine`、`cargo test -p ruyix`、
  `node scripts/ui-smoke.js`、`check-style` / `editor-layout` / `config-layout`。

---

## 6. 回滚与兼容

- `api_format` 默认 `openai` → 旧配置零改动即等同旧行为。
- `llm_fallback` 默认 `None` → 不配置备用 = 旧行为（且 `chat` 无备用分支逐字节一致）。
- 如需回退：删掉 `ruyix.code.ai_fallback.*` 配置即关闭故障切换；把 `api_format` 改回 `openai` 即回退协议。
