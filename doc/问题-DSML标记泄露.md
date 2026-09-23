# 问题：模型自带协议标记（DSML）泄露 —— 诊断、A/B 结论、未做完的修法

归属版本：v0.0.5 · 记录：2026-09-21 · 状态：**诊断完成 + 防漏已入库；"不再白烧一轮"的修法未做**

> 现象（用户报，附调试日志）：某一轮 `[warn] [agent] 第 5 轮输出无法解析：未知能力 ""`，
> 模型明明算对了命令（起服务前的 `if exist node_modules` / `netstat` 检查），却整轮作废。

## 1. 已入库的（commit `6d0fc91`）

- `llm::strip_model_markup(raw) -> (清理后文本, 剥了几处)`：两条常量正则，挂在
  `extract_json_object` 这个**唯一收口点**上（agent / plan / generate / repair / reflect / eval
  九处调用一起受益）。判定=尖括号里成对竖线出现两处；带 `DSML` 字样的放行属性里的引号，
  模板自带的短标记禁引号+卡长度（引号是 JSON 字符串常态，不卡会剪到正文）。
- `agent::json_of` + `markup_note`：解析错误里带上"检测到 N 处自带标记，已剥掉"。
- `parse_one` 空 `tool` 分支：改报"外层缺 `tool`"并把模型刚发的参数贴回去。
- `AGENT_SYSTEM` 规则 1：写明本引擎不走 tools 协议、不要输出工具调用标记。

**但"剥标记"本身不足以修好那一轮**（已复现验证）：`extract_json_object_inner` 的花括号扫描
本来就忽略 JSON 外的尾巴；那轮真正的病是**工具名丢了，只剩参数对象** `{"cmd": …}`。
真正起作用的是上面那两句纠正话术（其鉴别力单独证伪过：停掉注脚，端到端用例立刻变红）。

## 2. A/B：泄露到底跟什么有关（4 臂 × 8 轮，temp=0，同一条上下文）

复现脚本：`scripts/dsml-ab-probe.py`（读 `~/.ruyix/code/ai.toml`，直打官方端点，不回显 key；
预注册判读规则写在脚本头部）。原始逐轮值在跑的时候写盘。

| 臂 | 带标记 | 只尾标记 | 合法动作 | 裸参数 | 走 `tool_calls` |
|---|---|---|---|---|---|
| A `response_format=json_object`（现状） | 2/8 | 2/8 | 6/8 | 2/8 | 0 |
| B 只有提示词（不发 response_format） | 1/8 | 0/8 | 6/8 | 1/8 | 0 |
| C 声明 `tools`（四原语） | **0/8** | 0 | 2/8 | 0 | **6/8** |
| D `tools` + `json_object` | **0/8** | 0 | 2/8 | 0 | **6/8** |

**形状对照（这张是决定性的）**：

- A 臂泄露时的 content = `22 个空格` + `{"path": "."}` + 三行闭标签 —— **开标签整段没了**；
- B 臂泄露时的 content = **完整信封**：开标签行 + `invoke name="execute"` +
  `parameter name="args"` + `{"cmd": "dir /b"}` + 三行闭标签。

⇒ 结论三条：

1. **不是"工具调用与 json_mode 冲突"**：A 2/8 vs B 1/8 是噪声级；D 臂（两者同时）零泄露且
   6/8 正常走 `tool_calls`，两者叠加并不互斥。
2. **根因是"我们从不声明 `tools`"**：模型的原生工具调用无处可去，只能落进 content。
3. **json_mode 是"截头器"不是起因**：约束 content 必须以 JSON 对象开头 → decoder 把信封头部
   连同**工具名**吃掉，只留"对象 + 尾巴"。这解释了真跑里那个"只剩裸参数"的怪形状。

附带两条不太好听的发现，都要记住：

- **靠提示词治不了**：A/B 用的系统提示词**已经含**"不要输出工具调用标记"那句，泄露照样发生。
- **别关 json_mode**：B 臂多出 1 轮纯散文（json_mode 当初就是治这个的），而泄露率没降；
  且 B 臂的泄露是**整段信封**，更难处理。

## 3. 还没做的两条修法（回家接着改，二选一或都做）

### 修法 A（小、当天可上）：按形状把"截头信封"还原回来

泄漏轮的 content 只有参数、没有工具名，但**在四种能力的协议内这些形状是唯一解**（不是两选一）：

| 参数形状 | 还原成 | 依据 |
|---|---|---|
| 含 `cmd` / `op` / `handle` | `execute` | 只有 execute 认这些键（`op`/`handle` 是句柄三态） |
| `path` + `content` 或 `edits` | `write` | 只有 write 需要内容 |
| 只有 `path` | `read` | write 必须有内容，所以只给 path 只可能是读 |
| 顶层是数组 `[{…}]` | 批量调用 | 模型漏了 `actions` 外壳（真跑出现过） |

这四条**覆盖了实测全部 8 次泄露**（真跑 5/16：4 个裸参数 + 1 个裸数组；A/B 3 次）。

落点（都还没动）：

- `crates/harness-engine/src/agent.rs`：`parse_actions` 返回值改成一个带留痕的小结构
  （`ParsedActions { actions, markup, recovered }`），新增 `recover_args_only(&Value)`；
  在 `parse_actions_value` 里：顶层数组 → 补 `actions` 外壳后递归；单动作 `parse_one` 失败 →
  先试 `recover_args_only`，失败才报错（`markup_note` 那条错误文本保留给"认不出来的"）。
- 主循环 `agent.rs` 约 3522 行：`parsed.actions` + 一行 `sink.log("warn", …)` 把
  "剥了几处 / 还原成了什么"记进运行日志（**还原是替模型补语义，必须看得见，不许静默**）。
- `parse_step_actions`（约 1886 行）与 `step_agent.rs` 约 613 行：同款留痕。
- 测试：5 条形状还原 + 1 条"认不出来仍报缺 tool"；并把现有 e2e
  `a_headless_dsml_call_gets_told_what_is_missing_and_recovers` 改成"泄露轮被还原后照常执行、
  不烧轮"（现在那条断的是错误话术，还原上线后它会失效）。

**暂不回灌给模型**"你这轮被还原了"：实测提示词都拦不住它，回灌也治不了；要的是让操作者看见。

### 修法 B（根治，工程量在"协议"不在"解析"）：改走 `tools` 协议

数据支持它真的有效（C/D 两臂零泄露、6/8 标准 `tool_calls`，且与 `json_object` 不冲突）。
代价清单（别低估）：动作形态从"content 里的 JSON"换成 `tool_calls`；`plan` / `ask_user` /
批量调用 / 子步骤 / 反思 / 评测 全部解析点要跟着改；迁移期会出现**混合态**（C/D 各有 2/8 轮
模型仍按提示词的 JSON 协议作答），两套形状得同时认一段时间。

## 4. 回家第一件事

```bash
git pull            # 0.0.5 已含 6d0fc91（剥标记 + 纠正话术）
python scripts/dsml-ab-probe.py      # 可选：先亲眼复现一遍 A/B（需要 ai.toml 里的 key）
```

然后按上面的顺序做**修法 A**（小、可当天验证），B 作为排期项。判据：真跑一轮后
`~/.ruyix/code/debug.log` 里不再出现"未知能力"，且运行日志里能看到
"剥掉 N 处 / 还原为 …"的留痕。

## 5. 已知缺口

- 修法 A 对**认不出形状**的泄露仍然烧一轮（保住的是"报清楚让人改"）。
- 这四条形状规则是**协议内**的唯一解，但如果哪天加了新能力（比如 `delete`），
  `{"path": …}` 就不再唯一 —— 那条规则必须跟着复查（代码注释里也写了这句）。
- 本条只覆盖 content 通道；`reasoning_content` 里的同类标记**不做处理**（它不是协议通道）。
