# ruyix v1.1 问题与方案（ISSUE）

> 跨版本的约定留在 `doc/` 根，**版本专指**的 bug 记在本文件所属版本下（`doc/v1.1/`）。
> 每条格式：**症状（可复现）→ 根因 → 证据 → 改法 → 门禁**。修完把根因与门禁写回同一文件。

---

## ISSUE-1：进程表的"测试隔离锁"锁的不是进程表 —— 两条 agent 用例并行跑会间歇性红

**状态**：**已修**（2026-09-25）。

修复落在三处：
1. **按 handle 查进程限定项目**：新增 `by_handle_in` / `by_handle_in_mut`，
   `status` / `log_tail` / `stop` / `info_of` / `set_state` 全部改成 `(proj, handle)`；
   **删掉了全局扫描的 `by_handle`**（留着它迟早有人再按名字扫一遍）。
2. **面向模型的三个入口**（`tool_proc`）把项目一路透传下来（宿主/子步骤/批处理三条路径）。
3. **`table_lock()` 的注释改成实话**：它是"单测串行锁"，锁的**不是**进程表本身
   （测试不能持表锁 —— std `Mutex` 不可重入，会死锁）。

**门禁（已跑）**：

```bash
cargo test --workspace     # 连跑 3 遍：373 / 160 全绿（此前间歇性红的那两条都稳了）
```

新增判据 `a_handle_lookup_must_not_reach_across_projects`（agent/tests.rs）：
拿项目 A 的 handle 去项目 B 查 status / log / stop，**三条都必须失败**。
这条判据在修之前**必然红**（那时是按名字扫全局表），修完才绿 —— 所以它是这条缺陷的门禁形状。

### 症状（可复现）

单独跑必绿，**并行跑全量**间歇性红，红的两条都属于同一族（都断言"进程必须活着"）：

```bash
cargo test -p harness-engine --lib a_missed_criterion_carries_the_port_comparison -- --test-threads=1
# → ok. 1 passed; 379 filtered out; finished in 5.95s

cargo test --workspace          # 第 1 遍 exit=101；第 2 遍红的是另一条同类：
#   test agent::tests::a_service_reported_as_running_must_survive_the_run ... FAILED
#   test result: FAILED. 371 passed; 1 failed
```

失败信息（`agent/tests.rs:2174` 的 `expect`）：

```text
判据没命中不等于启动失败: "handle=p1 已从进程表移除"
```

### 根因（两条叠在一起）

1. **隔离锁与表锁不是同一把**：
   - `proc.rs:171` `fn lock()` → 锁的是 `table()`（`Mutex<HashMap<u32, Managed>>`），所有真实读写都走它；
   - `proc.rs:1156` `pub(crate) fn table_lock()` → 锁的是一个**独立的** `static G: Mutex<()>`，与进程表毫无关系。
   于是"测试隔离"的语义被误解成"我拿锁了就不会被打扰"，但**没拿这把锁的测试照样能改表** ——
   锁只排除**同样拿它的人**。
2. **handle 是全局唯一名字、查表是全局线性扫描**：
   `proc.rs:180` `by_handle(&t, handle) = t.values().find(|m| m.handle == handle)`，
   而 handle 形如 `p1`/`p2`（按项目内的序号起名）。两个测试并行时各自都有 `p1`，
   查到的是"第一个名字叫 p1 的进程" —— 可能是**别人的**那条，也可能已被清掉 ⇒
   `info_of()` 只能回 `handle=p1 已从进程表移除`（`proc.rs:774`）。
   **这不只是测试问题**：生产里一次只跑一个 run 所以撞不到，但按 handle 查/读日志/停的接口
   **跨项目语义是模糊的**（项目 A 的模型理论上能读到项目 B 的 handle 同名进程）。

### 证据

- `proc.rs:171-173`（`lock` 锁 `table()`）与 `proc.rs:1156-1161`（`table_lock` 锁独立 `G`）并列存在；
- `proc.rs:180-182` `by_handle` 全局扫描 + `proc.rs:770-775` `info_of` 的错误文案；
- 上面两条命令的实测输出（一次串行绿、两次并行红，红的是**不同**的两条同类用例）。

### 改法（下一轮）

1. **查表按项目限定**：`info_of` / `log_tail` / `stop_handle` 三个入口接受 `proj`，
   `by_handle` 改成 `by_handle_in(&t, proj, handle)` —— 这一条同时修掉产品侧的跨项目歧义。
2. **测试隔离改成同一把锁**：去掉 `G`，`table_lock()` 返回**表锁的守卫**会与测试调用的
   引擎 API 自锁造成 std `Mutex` 不可重入的死锁 ⇒ 正确做法是给测试一个
   **全局测试锁**并让**所有碰表的用例**都拿它（现在只有一部分拿），
   或在测试里改用 `clear_table()` + 独立项目根，断言只看自己那条 pid。
3. 顺手：`refresh_all` 的 `is_dead` 判定加注释说明它**只标记不删除**（`t.remove` 只出现在容量腾位与显式停止）。

### 门禁（修完要满足）

- `cargo test --workspace` **连跑 3 遍全绿**（这是本次唯一无法用单条命令判定的地方 ——
  门禁就得是"重复跑"这种形状，因为缺陷本身是时序的）；
- 新增一条判据：**按 handle 查进程必须带项目作用域**（`by_handle_in` 存在且三个入口都用它）；
- `ui-smoke` 无需改（进程表是引擎侧）。

### 补充（修复后仍观察到）：这条用例还有**第二条**通路

修复（按项目限定 handle）之后，`a_handle_lookup_must_not_reach_across_projects` 稳定绿；
但 `a_missed_criterion_carries_the_port_comparison` **仍会间歇性红**（连跑 4 遍里出现过 1 遍，
其余多轮全绿）。

**本次抓到的新证据**：

```text
thread 'agent::tests::a_missed_criterion_carries_the_port_comparison' (25056) panicked at crates\harness-engine\src\agent\tests.rs:2218:48: 判据没命中不等于启动失败: "handle=p1 的进程已被停止，本次等待中止" note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace  
```

**已排除**：项目路径撞车。`TempDir::new` 带 pid + 纳秒（agent/tests.rs:5-16、workspace.rs:300-311），
路径唯一，不存在"两个用例共用同一个项目根、互相收进程"。

**下一步（未做）**：这条用例的 `tool_exec_bg` 在一次调用**内部**就把进程表项丢了，
嫌疑集中在 `refresh_all` 之后的容量腾位（`proc.rs` 的 `while live_in(..) >= max` 会 remove 死条目）
与 `set_state` 的状态推进顺序；要的是一份"表项什么时候被谁删"的时序日志，而不是再加一把锁。
**在修掉之前，别把 `cargo test --workspace` 单跑一次的结果当作门禁结论** —— 连跑三遍才算。

### 第二次尝试（当日）与回退：`poll_child` 也是按名字找

**新证据**：把 `refresh_all` / `set_state` 那条线排除后，抓到这一条 ——

```text
handle=p1 的进程已被停止，本次等待中止
```

它来自 `proc.rs` 的 `poll_child`：那里用的还是**只按名字**的 `by_handle_mut`（全局线性扫描），
别的项目/别的测试里同名、已被收掉的进程会让 `start` 的等待循环误判成"我这条被停了" ⇒ 
**ISSUE-1 是同一个病在两个地方的实例**（查/停/读日志 是一处，轮询子进程 是另一处）。

**尝试与回退**：把 `poll_child` 也改成 `(proj, handle)` 之后，
*新建的* `a_handle_lookup_must_not_reach_across_projects` 反而开始间歇性红
（限定比较在启动路径上不成立 —— 存表与查表两处的路径字面不同源）。
按"不推没验证过的东西"，这一改动**已回退**，工作树回到 `f8bc5db`（那版：按项目限定查/停/读日志
已修 + 回归判据在；残余是这条用例间歇性红）。

**这次的失败原文（回退前抓到的）**：

```text
 thread 'agent::tests::a_missed_criterion_carries_the_port_comparison' (42196) panicked at crates\harness-engine\src\agent\tests.rs:2218:48: 判据没命中不等于启动失败: "handle=p1 的进程已被停止，本次等待中止" note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace   
```

**下一步（要点）**：修 `poll_child` 的正确做法不是照抄参数，而是**统一路径口径** ——
先给 `Managed.proj` 与所有查询入口一个共同来源（例如都由 `start` 写入、
比较用同一套规范化函数），否则"限定项目"这件事在存/查两侧各说各话。
在它修好之前：**`cargo test --workspace` 单跑一次不算门禁结论，连跑三遍才算**。

### 残余已修：`clear_table()` 清的是**全表**

第二轮尝试失败（把 `poll_child` 照抄参数）之后，回到"为什么表项会消失"这条线上，
在 `proc.rs` 的 `cleanup()` 里找到了真凶：

```rust
fn cleanup(proj: &Path) {
    let (_, _) = shutdown_for(proj, false);  // 收自己的
    clear_table();                           // ← 抹**所有人的**（不分项目）
}
```

每个 proc 用例的收尾都会跑到这里，于是**并跑时任何一个用例跑完，都会把别人的条目一起抹掉**
—— 进程还活着，表里却没了 ⇒ `poll_child` 返回"已被停止"（这正是之前抓到的原文）。
`table_lock()` 那种"串行锁"只能保护**也拿了锁的人**，保护不了"收尾动作本身越权"。

**正解（已落地）**：收尾**本身按项目限定** —— `clear_table()` 改成 `clear_table_for(proj)`
（`t.retain(|_, m| m.proj.as_path() != proj)`），并给那条碰了 `start` 却没拿锁的用例补上锁。

**门禁（已跑，连跑三遍全绿）**：

```bash
cargo test --workspace     # 374 / 160，连跑 3 遍 0 failed
```

判据加了**第四环**（`a_handle_lookup_must_not_reach_across_projects`）：
清理 A 之后，B 的条目**必须仍在**（`status(&b, ..)` 仍然 Ok）——
这一环在修之前必红，钉的就是"收尾不许越界"。

教训（写给下一次）：**"限定项目"这件事必须落在每一个动作上**，
而不是"在查询侧加一把锁"。查询侧限定治的是"看错人"，收尾越界治的是"把人弄丢"。

---

## ISSUE-2：启动就报 `Uncaught SyntaxError: Identifier 'L' has already been declared` —— 界面整块死掉

**状态**：**已修**（2026-09-25）。

### 症状（用户实测）

启动即报，界面不工作（标签栏/文件树/会话全无反应）：

```text
Uncaught SyntaxError: Identifier 'L' has already been declared (at main.js:1:1)
```

### 根因：不是函数写错，是**脚本之间**的事

`ui/index.html` 里 15 个 `<script>` **没有一个是 `type="module"`** ⇒ 全是**经典脚本**，
它们共享**唯一一个全局词法作用域**。于是：

| 文件 | 那一行 | 结果 |
|---|---|---|
| `command.js:2` | `const L = (zh, en) => …` | 先加载，拿到全局词法绑定 `L` |
| `main.js:2` | `const L = (zh, en) => …`（**同一个提交里一起加的**，`79f4baf`） | 后加载，**求值前**抛 SyntaxError，**整份不执行** |

`main.js` 不执行 ⇒ `window.state` 没建、事件没挂、面板全没了 —— 用户看到的"界面死掉"就是这么来的。
注意报错位置写着 `main.js:1:1`，但**根本不在 main.js 里**：第二个声明的位置就是报错位置，
这一条曾经误导排查方向（第一反应是去 main.js 里找重复的 `L`，方向错了）。

### 为什么现有五条门禁全都没拦住它（这条最重要）

| 门禁 | 为什么看不见 |
|---|---|
| `ui-smoke` 面板回放 | 把脚本 `eval()` 进 Node —— **eval 有自己的一层作用域**，两个文件各自 eval 也不会撞 |
| `scripts/memory-layout.js` 等布局探针 | 更彻底：先把 `index.html` 的 `<script>` **全删掉**，再 `eval(read("ui/main.js"))` —— 既不加载 `command.js`，也不让**浏览器**去求值脚本 |
| 布局探针的判据 | 它们量的是**几何**（宽高/溢出/滚动条），不是"这份文档能不能起来" |

**教训**：`eval()` 真源码只能验**行为**，验不了**装载**（脚本清单、加载顺序、全局作用域冲突）。
装载这件事必须让**真浏览器按 `index.html` 的真实顺序**跑一遍 —— 这正是 ISSUE-2 逼出来的新门禁。

### 证据

1. **V8 层复现**（同一 context 依序跑两个经典脚本，顺序同 index.html）：

```text
ok   command.js
FAIL main.js -> SyntaxError: Identifier 'L' has already been declared
```

2. **真浏览器探针**（`scripts/startup-probe.js`，无头 Edge + 真 index.html）：

```text
# 修之前
FAIL  A4  零 SyntaxError —— SyntaxError: Uncaught SyntaxError: Identifier 'L' has already been declared
FAIL  A5  脚本真的执行到了（state=false showPane=false L=true EDITOR_PANES=false）
  ·   CDP 另见 1 条异常: … @ main.js:1
# 修之后
PASS  A3  逐个加载成功（15/15）
PASS  A4  零 SyntaxError
PASS  A5  脚本真的执行到了（state=true showPane=true L=true EDITOR_PANES=true）
```

`state=false` 那一格是关键证据：它证明 **main.js 整份没跑**（不是"某个函数出错"）。

3. `git blame`：`command.js:2` 与 `main.js:2` 同为 `79f4baf`（2026-09-25，"英文界面露中文"那一轮）
   —— 同一个提交里往两个文件各写了一次，同源同因。

### 改法

1. **全局 L 只留一处定义，并挂到 `window` 上**（`ui/command.js`）：`window.L = (zh, en) => …`。
   挂 window 才是"定义一次"的硬事实 —— 属性不是词法绑定，重复赋值也不冲突；
   `session.js` / `mcp.js`（分别用 108 / 33 次裸 `L(`，本来就没有自己的声明）照旧拿得到。
2. **删掉 `ui/main.js` 的重复声明**，原地留一条注释说明它从哪来、为什么不能再写一次。
3. 两个文件的行为一字未变（同一个函数，只是装配方式变了）。

### 门禁（已跑）

```bash
node scripts/startup-probe.js     # 真浏览器 + 真清单：15/15 加载、0 SyntaxError、state 就在（SKIP 可分辨）
node scripts/ui-smoke.js          # U56 静态 + U57 真浏览器（347 → 351 项）
```

- **U56 `startup-scope`（静态，不吃浏览器）**：跨文件顶层 `const/let/class/var` **不许重名**；
  全局 `L` 必须恰好一处 `window.L` 定义。修之前必红（就是这条抓住了 `L`）。
  为什么要一条静态的：U57 **没浏览器就 SKIP**（"没跑"与"通过"必须能分辨），
  而"全局作用域冲突"这种缺陷不能只在装了 Edge 的机器上才拦得住。
- **U57 `startup-real`（真浏览器端到端）**：按 `index.html` 的真实顺序把真实的 15 个文件
  交给真浏览器当经典脚本执行，判据 A1–A6（清单自洽 / 不漏接线 / 逐个加载成功 /
  **零 SyntaxError** / 脚本真的执行到了 / 其余错误只报告不判红）。它拦的是**所有启动期崩法**，
  不止重名这一类。

---

## ISSUE-3：用 anthropic 端点时"联网搜索用不了" —— 我们自己把它关掉了，而且连用户设的 `on` 一起吃了

**状态**：**已修**（2026-09-25）。

### 症状（用户实测）

配置 `api_format = anthropic` + `api_url = https://api.deepseek.com/anthropic` + `deepseek-flash`：
联网搜索**没有任何反应**。把 `llm.web_search` 设成 `on` 也一样 —— 配置说开着、请求里没有、
界面上不报错。

### 根因（三层，一层比一层隐蔽）

1. **一句未经实测的断言被当成设计约束**。`web_search_on()` 里写着：

   ```rust
   // anthropic 协议不支持服务端联网检索（语义不成立），一律关。
   if cfg.api_format.trim().eq_ignore_ascii_case("anthropic") { return false; }
   ```

   "语义不成立"是**推断**，不是实测。事实：Anthropic 的 Messages 协议**有**自己的服务端检索工具
   （带版本的类型名 `web_search_20250305`），DeepSeek 的 `/anthropic` 入口实现了它。
2. **早返回发生在看配置之前** ⇒ 用户显式设的 `on` 被静默吃掉。`on` 的文档承诺是"强行开、
   不看能力表"（自建兼容端点用），而这里连协议判断都排在它前面 —— **配置界面在撒谎**：
   用户能看到开关、能设成 on、请求里一个字节都不带、界面不报错。
3. **能力表少了一维**。`ModelCaps` 只有"模型"一维，而联网是**端点提供的服务端工具**、
   两条协议工具名不同 ⇒ 它是 **（协议 × 模型）** 的能力。旧表把 `flash` 标成"搜不了"，
   那是在 `/responses` 上量出来的结论，被当成了模型的固有属性。

### 证据（全是实测，不是推断）

**(1) anthropic 端点真的认它自己的服务端检索工具**

```text
POST https://api.deepseek.com/anthropic/v1/messages
tools = [{"type":"web_search_20250305","name":"web_search","max_uses":3}]
→ HTTP 200，content 里出现：
   {"type":"server_tool_use","name":"web_search","input":{"query":"杭州今天天气"},...}
```

**(2) 发 OpenAI 那套名字会被 422 打回，且服务端自己给出了正确取值**

```text
tools = [{"type":"web_search"}]
→ HTTP 422 unknown variant `web_search`, expected `web_search_20250305` or `web_search_20260209`
```

**(3) 联网是（协议 × 模型）：四个模型两条路各打一遍**

| 模型 | anthropic（`web_search_20250305`） | `/responses`（`{type:web_search}`） |
|---|---|---|
| `deepseek-v4-pro` | ✅ | ✅ |
| `deepseek-flash` | ✅ | ❌ |
| `deepseek-v4-flash` | ✅ | ❌ |
| `deepseek-v4-flash-vision-exp` | ✅ | ❌ |

**(4) 与函数工具同框真的干活**（严格模式的实际形状，实测 `deepseek-flash`）：

```text
tools = [ {name:final,...}, {name:read,...}, {"type":"web_search_20250305",...} ]
→ HTTP 200，一轮里依次出现：
   server_tool_use(web_search, query="杭州今天天气")
   web_search_tool_result
   tool_use(final, {answer:"今天（9月25日）杭州多云到阴…"})
```

**(5) 真机测试（`tests/web_search_live.rs`，新增两臂）**

```text
[live] anthropic 联网查询词 = ["2026年9月18日 上证指数 收盘点位", "Shanghai Composite Index September 18 2026 close"]
[live] anthropic 答复       = {"answer":"…上证指数收盘报3911.87点…"}
[live] anthropic 关联网答复 = {"answer":"2026年9月18日尚未到来，无法获知…"}   ← 对照组：关掉就答不出
[live] anthropic+tools 查询词 = ["2026年9月18日 上证指数 收盘"]  工具调用 = execute {...}  ← 同框各不挤掉
```

**这条测试在修之前必然红**（查询词恒为空）—— 所以它就是这条缺陷的门禁形状。

### 改法

1. **能力表加一维**：`ModelCaps.web_search_anthropic`，四个模型按上表实测填。
2. **一份取口径**：新增 `llm::web_search_capable(model, api_format)` —— 按协议取该维；
   `web_search_on` 去掉早返回，`on` 恢复"强行开"的本义（**这条是本次缺陷的回归判据**）。
3. **anthropic 请求体**：开着时把服务端工具 `{type: web_search_20250305, name: web_search,
   max_uses: 5}` 与函数工具**同一个数组**声明（`type` 区分，常量见
   `WEB_SEARCH_ANTHROPIC_TYPE` / `WEB_SEARCH_MAX_USES`）。
4. **解析**：`extract_anthropic` 收 `server_tool_use` 的 `input.query` 进 `web_queries` ——
   与 `/responses` 路的 `web_search_call` 落**同一个字段**，于是"这一轮查了什么"在两条协议下
   都从同一处显示（`tool_loop` 已经在那儿渲染）。`web_search_tool_result` / `thinking`
   既不是正文也不是我们的工具调用，天然被跳过。
5. **宿主 + 界面**：`ai_model_caps` 现在给"**按当前协议算的有效值**"+两条协议的原始值 + `api_format`；
   会话里那个 🌏 开关禁用时把**协议**写进文案（"在 anthropic 协议下不支持…（换模型或换协议）"）——
   同一个模型换条协议可能就能搜，不写协议用户看不懂为什么突然不能搜。

### 门禁（已跑）

```bash
cargo test --workspace          # 382（+2）/ 160 全绿
cargo clippy --workspace --all-targets && cargo clippy -p ruyix --no-default-features --features custom-protocol
                                # 两模式 0 warning
cargo fmt --check && node scripts/check-style.js && node scripts/ui-smoke.js   # 351/351
DEEPSEEK_API_KEY=sk-xxx cargo test -p harness-engine --test web_search_live -- --ignored --nocapture
```

单测四条（都在 `llm.rs`）：
`web_search_is_decided_per_protocol_not_per_api_format_alone`（含"on 不许被协议吃掉"）、
`the_anthropic_body_carries_the_versioned_server_web_search_tool`（发错名字必被 422，所以工具名要钉）、
`anthropic_extract_reports_server_side_queries_without_confusing_tool_use`（真响应骨架：查询词进
`web_queries`、服务端工具块**不**被当成我们的调用、同轮 `tool_use` 照旧解析）、
能力表两维断言（防"只钉一列"重演）。

### 教训（同一个病在四个 bug 里出现过四次）

`llm.rs` 里的能力判断**必须带实测日期或证据**。这次那句"语义不成立"以**设计约束**的名义活了很久，
表现却是"用户的开关静默失效"。同类前科：bug 5 的"接口格式想当然"（用户当时纠正原话：
"如果是 anthropic 接口时，走 anthropic 的工具调用，这才是正确的改法！"）、
`/responses` 曾经只声明联网不声明函数工具（同一个死循环病）。
**规矩**：协议差异只许写"实测得到什么"，不许写"语义上应该怎样"。

---

## ISSUE-4：任务跑完后**最终回复在工具循环输出上面** —— 读起来像"先给结论再做事"

**状态**：**已修**（2026-09-25，用户实测报告）。

### 症状

用户原话：「对话区域，LLM 的最终回复放工具循环输出的下面，现状是个大 bug：任务做完后工具循环
信息在下面，最终回复在上面。」—— 也就是同一个助手气泡里，正文（最终回复）在最上、轨迹在它下面。

### 根因

`ui/session.js::msgHtml` 的拼接顺序：

```js
bubble + (mine ? "" : gateHtml(m) + askHtml(m) + traceHtml(m, live)) + meta
```

正文 `bubble` 排在最前，而轨迹是**跑的过程中**一段段追加进同一个气泡的 ⇒ 跑完后轨迹落在正文
**下面**。这不是渲染错位，是**顺序写反了**：轨迹是过程，正文是结论，过程该在上。

### 改法

```js
(mine ? bubble : traceHtml(m, live) + askHtml(m) + gateHtml(m) + bubble)
```

顺序定为：轨迹（思考·执行）→ 提问卡 → 最终回复 → 验证/复核 → meta。验证/复核特意留在正文
**之后** —— 它是针对上面那条回复的结论，贴着它才读得通。

### 门禁

`U41 trace-before-answer`：判据落在**真渲染出来的 HTML 索引序**上（`trace < bubble < 正文文本`），
不是源码里的拼接顺序 —— 后者只能证明"代码那么写了"，前者才证明"用户看到的顺序对了"。
另加 `U41 answer-not-in-trace` 的反面（正文不许落进轨迹块里）。
真布局探针 `scripts/session-trace-layout.js` 复跑全绿（顺序改动没有带坏几何，长行仍是省略号收尾）。

## ISSUE-5：配置里手输模型名 → 请求 400（"but you passed on"）—— 这条路**根本不该允许手输**

**状态**：**已修**（2026-09-25，用户实测报的）。

### 症状（用户实测）

配置里出现 `model = "on"`（一个根本不是模型名的值），请求原样发给厂商，回：

```text
HTTP 400 {"error":{"message":"The supported API model names are deepseek-flash,
deepseek-v4-pro, but you passed on.","type":"invalid_request_error"}}
```

用户不但跑不了，**还问不到"那我该填什么"** —— 除非自己去翻厂商文档。

### 根因：不是那个值，是**这条路允许手输**

`ui/config.js` 的 `ai.model` 是自由文本框，注释与宿主那句还把它写成了**设计**：

> `model: "text", dynamic: "models"` + ……「取不到就退回文本框 —— 让用户手填」

也就是说：**拉不到厂商清单时，降级方向是"放开手输"（fail-open）**。于是任何值都能进配置，
而厂商只认它自己的名字 —— 这是把校验责任推给用户，代价是必然有人踩。

三处叠加：
1. **前端 fail-open**：拿不到清单 → 退回文本框（`on` 就是这么进去的）；
2. **宿主鼓励这个方向**：`ai_list_models` 的注释写着"返回错误让前端降级成文本框"；
3. **错误信息不闭环**：400 里的"支持哪些名字"只到日志，界面只说"调用失败"。

### 改法

**前端（用户口径：不要让用户在配置里输入模型名称；默认选第一个）**

- 两个模型字段（`ai.model` / `ai_fallback.model`）**只许从厂商 `/models` 里选**；
- 拿不到清单 → **fail-closed**：渲染成**禁用**的下拉（既有值如实显示）＋ 原因 ＋ 怎么重试
  （修好 api_url/api_key 后点「取消」重开表单）。**代码里不再存在"把模型渲染成文本框"的分支**；
- 拿到清单但当前值**没配 / 不在清单里**（就是 `on` 这种）→ **落到第一个**，并在行下写明
  「原值「on」不在厂商模型列表里 → 已改为第一个：deepseek-flash（保存后生效）」——
  换可以，**静默换不行**；
- **备用 LLM 的清单探它自己的端点**（`ai_list_models(section="ai_fallback")`），
  不再拿主用的清单去凑。

**引擎（兜网：手改 toml 绕过表单也不至于卡死）**

- 真的因为模型名被厂商打回时，用厂商清单的**第一个**重试一次，并把"换了哪个"记进
  `llm-model-corrected` 观察点；
- 重试仍失败 → 错误里贴出厂商认的名字（拿不到清单就什么都不加，**不许瞎猜**）；
- 两条都只在**异常路径**发生：正常路径**一次额外请求都不发**
  （第一版把校准放在请求前，被一条本地桩服务器的单测当场抓到 —— 多出来的那次 `/models`
  把桩连掉了。门禁因此加了一条：不许在 happy path 上探测清单）。

### 证据

- **真机**：`cargo test -p harness-engine --test models_live -- --ignored --nocapture` →
  厂商清单 = `[deepseek-flash, deepseek-v4-pro]`（所以"默认第一个" = `deepseek-flash`，
  且 anthropic 端点仍走主机根回落，不会静默退回旧行为）；
- **回放真 config.js（U62）**：
  - 清单可用 + 配置值是清单外的 `old-model` → 渲染成**下拉**、选中**第一个**、行下写明原值；该行**没有 input**；备用行确实按 `section=ai_fallback` 探了自己的端点；保存提交的是下拉里选中的值；
  - 清单不可用（桩里抛"尚未配置 Key"）→ 两个模型行都是**禁用**的下拉、行内**无 input**、说明里带原因与重试方式；
- **单测**：`llm::tests::pick_model_三条口径`（在清单里→原样；不在/为空→换第一个+说明；**拿不到清单→原样，不擅自换**）。

### 门禁（已跑）

`ui-smoke` **403/403**（U62 十三条：静态 9 条 + 两个回放；U35 那条"拿不到就退回文本框"改成
fail-closed）· `cargo test --workspace` 390/160 · clippy 两模式 0 warning · `fmt --check` /
`check-style` PASS · `models_live`（真机）。

### 教训（与 ISSUE-3 同源）

**"能配成错的东西，用户迟早会配错"** —— 与 ISSUE-3 那句"早返回吃掉用户设的 `on`"是同一类病：
不是算错了，而是**把一个不该由用户负责的判断留给了用户**。
降级路径的方向要选 fail-closed：**拿不到清单 ≠ 可以手填**，而是"暂时不能改，并说清为什么"。

### 缺口（如实记）

**真窗口里手点一遍没验**：证据是"回放真 `config.js` + 真机清单 + 单测"三层，
但没有在运行中的 IDE 窗口里用鼠标打开配置表单看一眼（需要 CDP 驱动真窗口，本轮没做）。

## ISSUE-6：语音转写"永久挂起" —— 自检把 456MB 读进内存，而它跑在主线程上

**状态**：**已修**（2026-09-25，用户实测报的：界面截图 + devtools 网络面板）。

### 症状（用户实测）

> 语音识别卡住了不动啊？一直 transcribing locally，不会变化啊。一直是挂起状态。

devtools 网络面板（用户截图）：`voice_status` 一条花了 **6.58 秒**；两条 `voice_transcribe`
永远停在**挂起**（预检都是 200 / 1ms ⇒ 请求发出去了，回复没回来）。

### 根因（两层叠在一起，第二层才是"挂起"的解释）

1. **自检本身极贵**：`modelstore::status_with` 对每个必需文件做
   `std::fs::read(p)`（把 475MB **整个读进内存**）+ `Sha256::digest`（全量重算）——
   **每次调用**都算。实测这一下发 **6.5 秒**。
2. **贵的那一下跑在主线程上**：`voice_status` 当时是**同步** `#[tauri::command]`
   ⇒ 主线程被占 6.5 秒。界面里在飞的 IPC（正在转写的那一条）**回复送不回去**
   ⇒ 用户看到的就是"永远挂起"。

放大因素：转写命令开头也调 `status()` ⇒ 每次转写先白付 6.5 秒（那份自检它并不需要"重算"，
只需要"知道就绪"）。

### 改法

- **引擎（`modelstore`）**
  - 哈希改**流式**（1MB 缓冲）—— 不再把整个权重读进内存；
  - 加"**每份文件只真校验一次**"的进程内缓存，键 = （路径，大小，**mtime**，期望哈希）：
    mtime 变（重下/换权重）自动失效；期望哈希进键，避免两份规格共用一份文件时互相冒充"已核验"。
- **宿主**
  - `voice_status` 改 `async` + `spawn_blocking`：**自检永不占主线程**；
  - 转写命令里的自检也搬进阻塞闭包（它本来就在 `spawn_blocking` 里，别把它漏在外面）；
  - 转写加**分段事件** `voice://stage`（`status` / `load` / `infer`）并在返回值里带 `stages` 耗时。
- **前端**
  - 收听 `voice://stage` 并把阶段写进状态栏；转写那句状态行改成
    「本机转写中…（N 秒音频，本机推理约需 M 秒）」——
    这个 bug 被报成"卡死"，一半原因是**界面只有一句不动的话**：用户没法区分"在算"和"卡住"。

### 证据（真机实测，不是推断）

真窗口活体探针（`scripts/voice-transcribe-probe.mjs`，CDP over WebView2，真音频 3.77 秒）：

| 项 | 修前（用户 devtools 现场） | 修后 · debug 构建 | 修后 · **release 构建** |
|---|---|---|---|
| `voice_status` 第一次 | **6580 ms** | 7512 ms | **325 ms** |
| `voice_status` 第二次 | 6580 ms（每次重算） | 3 ms | **2 ms** |
| `voice_transcribe` | **永久挂起** | 200s 未到（debug 推理慢十几倍，非挂起） | **12341 ms 返回** ✅ |
| 转写文本 | —— | —— | `帮我把构建命令改成Cargo Build`（与引擎探针一致） |
| `voice://stage` | 只有一句不动的话 | —— | `检查语音模型… → 首次加载语音模型（约 456MB，之后常驻）… → 识别中（3.775 秒音频，本机推理）…` |

第二次那 2-3 ms 就是"每次自检 6.5 秒"的反证 —— 这也是本次修复的核心判据（不靠"等了多久"，
靠"缓存有没有生效"）；而"转写返回了"是"挂起"这个症状唯一诚实的判据。

### 门禁（已跑）

- 引擎单测 `自检缓存只算一次且文件变了就失效`：①同一份文件只算一次 ②文件改了必须重算并报 Broken；
- `ui-smoke` **U63 voice-no-main-thread-block** 六条：`voice_status` 必须 async + spawn_blocking、
  哈希必须流式、必须缓存、分段事件三阶段齐全、前端必须收听、状态行必须给 ETA；
- 活体探针 `scripts/voice-transcribe-probe.mjs`（判据预注册：第二次 `voice_status` < 300ms
  **且** 转写必须返回；跑挂即以非零码退出）。

### 教训

1. **"界面上一句话不动"要先怀疑线程，而不是功能。** 一个同步命令干 6.5 秒的重活，
   会让**别的** IPC 看起来像死了 —— 排查时"功能没坏"和"界面没反应"可以同时成立。
2. **昂贵的自检必须缓存，且绝不许放在主线程。** 判断"文件有没有坏"需要全量哈希，但
   "同一份没变过的文件"不需要第二次全量哈希；而"第一次"那一秒也必须待在阻塞线程里。
3. **进度要分段。** 十几秒的等待，只给一句话，用户只能得出"死了"这个结论。

### 缺口（如实记）

- 探针在 **debug** 构建下看不到"转写返回"：candle 推理慢十几倍，200 秒都不够
  （同一段音频 release 约 11 秒）—— 所以探针默认超时放到 600 秒，并在头注释里写明
  "别用 debug 的耗时判断有没有挂起"。**release 已完成复验（上表）**。
- 用户报障时手上跑的那个实例是**修之前**的二进制：**必须重启**才生效。
- 首次自检仍要付一次全量哈希（release 实测 325 ms / 478MB）。这是"文件有没有坏"的唯一硬证据，
  保留；但它现在待在阻塞线程里、且一次进程只付一次。

## ISSUE-7：配置被"保存"写坏 —— 23 个键变成 `"on"`（表单按位置读控件，读串了值）

**状态**：**已修**（2026-09-25，做"配置保存后刷新面板"时顺出来的；**同时是 ISSUE-5 的真根因**）。

### 症状（用户报障旁边那处）

会话面板那排 chip 里写着 `✗ python 未找到（where / command -v 解析不到 on）`，
`✗ node` 同样。用户截图里红框圈的正是这排 —— 而他问的是"能不能用事件刷新它"。
**先说清楚：那不是渲染陈旧，是数据真的坏了。** 宿主探针老实回报：

```json
"python": { "bin": "on", "available": false, "version": "未找到（where / command -v 解析不到 on）" }
"runs_root": "…\\ruyix\\runs\\on"      // docker.image 同款
```

磁盘上：

```toml
[harness]
"proc.max" = "on"
"sandbox.image" = "on"
"verify.python_bin" = "on"
workspace_root = "on"
…                                 # 共 23 个键，全是 "on"
```

### 根因：`controls[row.idx]` 用 **DOM 位置**当下标

```js
controls = Array.from(body.querySelectorAll("[data-row]"));   // 位置数组
const el = controls[row.idx];                                 // 按 row 下标取
```

只要两者错位，`readValue` 就会读到**别的控件**的值。而 checkbox 的 DOM `value` 默认是
**`"on"`** —— 事故现场正是这个值：尾巴上 23 个**非 bool** 键（`proc.*` / `reflect.*` /
`sandbox.*` / `step.max_steps` / `verify.*` / `workspace_root`）读到了开关的 `value`，
于是被原样写进配置。**bool 键自己反而没事**（它们的 `readValue` 走 `el.checked` 分支）。

这也解释了 ISSUE-5 那条 `model = "on"` —— **同一个字面量、同一个机制**：
用户报的"配置里手输模型名导致 400"，我当时修的是入口（不许手输），
但那个 `on` 根本不是他手输的，是**表单自己写进去的**。

### 改法（四层，缺一层都不够）

1. **按 `data-row` 属性寻址**（`Map<下标, 元素>`）—— 构造上正确，DOM 顺序怎么变都读不错；
   这是真防线（`ui/config.js::render`）。
2. **提交前的类型闸**（`firstInvalid`）：数字键必须能解析成数字、枚举键必须落白名单、
   开关行走勾选态 —— 有一行说不通就**整单不提交**，并说出**是哪一行、为什么**。
3. **读侧的 `acceptable` 加类型校验**（`config_bridge.rs`）：一份**已经坏掉**的配置不该把引擎带偏
   —— int 键上的 `"on"` 一律当没配、回落到默认。（`text` 键拦不住：`image = "on"` 语法合法，
   所以第 1 条才是那道真防线。）
4. **把已坏的配置修回来**：按 schema 判定（值是 `on`、kind 不是 bool、options 不含 `on`、
   默认值也不是 `on`）⇒ 删掉这些键、回落默认。两个便携根各 23 个，**备份在 `*.corrupt-<时间戳>.bak`**。

### 证据（真机，同一实例同一调用，修前 / 修后）

| | 修前 | 修后 |
|---|---|---|
| `python.bin` / 版本 | `"on"` / 未找到 | `python` / **Python 3.11.15** ✅ |
| `node.bin` / 版本 | `"on"` / 未找到 | `node` / **v24.16.0** ✅ |
| `docker.image` | `"on"` | `python:3.12-slim` ✅ |
| `runs_root` | `…\\ruyix\\runs\\on` | `…\\global\\runs` ✅ |

文件侧：`target/{debug,release}/global/harness.toml` 从 23 个 `= "on"` 变成 0 个 ✅。

### 门禁（已跑）

- `ui-smoke` **U65 config-form-no-value-smear**：回放里**故意把控件顺序打乱**
  （位置下标实现会当场读串）⇒ 每行必须仍读到自己的值；再把数字键填成 `"on"` ⇒
  **不许提交**且要说清是哪一行；
- 宿主单测 `说不通的值当没配_坏配置不会把引擎带偏`（int/枚举/0 关死三类都当没配，
  **合法值必须照常生效** —— 这道闸不能把正常配置一起拦下）；
- `ui-smoke` 419/419 · `cargo test` 全绿 · clippy 两模式 0 warning。

### 教训

1. **位置下标是隐式契约**："DOM 顺序 == rows 顺序"没人写下来、也没有门禁守，出事只是时间问题。
   有稳定标识（`data-row`）就该按标识寻址 —— 这不是洁癖，是**把隐式契约变成显式契约**。
2. **界面上的坏显示，先怀疑数据**。我看到 `✗ python on）` 的第一反应是"渲染 bug"，
   真相是配置里真写着 `on` —— 渲染层完全诚实。
3. **一个值能横跨两个 bug**：`"on"` 同时出现在 ISSUE-5（模型名）与 ISSUE-7（23 个键）里。
   修症状（不许手输）有用，但**把值追到源头**才终结了它。

### 缺口

- 那 23 个坏值已从两个便携根删掉（备份留着）；**用户手上的实例仍是修前的二进制**，需重启。
- 类型闸只覆盖"值域明确"的键；`text` 键的坏值仍只能靠猜（真正的防线是第 1 条）。

