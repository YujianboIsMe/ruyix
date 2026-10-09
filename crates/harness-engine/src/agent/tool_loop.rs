//! 工具循环本体（带提问通道）：Read / Write / Execute / Connect 四大原子能力的驱动循环。
//!
//! 从 `agent.rs` 里抽出来的 —— 那个文件已经 4300+ 行，而这一个函数就占 780 行。
//! 边界是这样切的：**循环本体**在这里；它依赖的零件（`Ctx` / 动作解析与校验 / 波次调度 /
//! 历史折叠 / 计划收尾 / 验证门禁）仍然留在 `agent.rs`。所以这是**文件切分**，不是可见性改造 ——
//! 本模块是 `agent` 的子模块，`use super::*` 就能看到父模块的私有项（`agent/tests.rs` 一直这么干）。
//!
//! 对外路径不变：仍是 `agent::run_with_ask`（父模块 `pub use` 再导一次）。
//!
//! 搬的时候**逐字未改** —— 这次只挪位置，不顺手重构，这样 diff 里是一段纯位移，可审。
//! 谁要再拆这个函数（它确实还能按阶段拆成几个私有函数），记得先立好分阶段的判据：
//! 循环里的 `ctx` / `gate` / `out` / `plan_cursor` 是跨阶段共享的可变状态，拆错就是把行为改坏。

use super::*;

/// 工具循环（带提问通道）：宿主把 [`Asker`] 落地（ruyix 走 `agent://ask` + 会话问题卡 + `agent_ask_answer`）。
///
/// `images` 是**这一条用户消息**随行的图（v1.3 多模态；裸 base64，读盘/缩放全在宿主侧）。
/// 只贴在第一条 user 消息上 —— 它是任务消息，属 v1.2 那套上下文布局里的**不可变根之后的头条**，
/// 整轮都在；历史里旧消息的图不重发（见 `ChatMessage::images` 的约定）。
#[allow(clippy::too_many_arguments)]
pub async fn run_with_ask(
    cfg: &AppConfig,
    proj: &Path,
    task: &str,
    history: &[HistoryMsg],
    images: &[ImagePart],
    policy: WritePolicy,
    conn: &dyn Connector,
    asker: &dyn Asker,
    cancel: &CancelFlag,
    sink: &dyn Sink,
) -> Result<AgentOutcome, String> {
    let task = task.trim();
    if task.is_empty() {
        return Err("消息不能为空".into());
    }
    let started = Instant::now();
    // 项目状态根由宿主注入（`<便携根>/projects/<项目 key>`）：暂存 / 备份都写那儿，
    // 一个字节都不进用户仓库。
    let mut ctx = Ctx::new(proj, policy)
            .with_state_root(crate::config::project_state_root(cfg, proj))
            // v1.4 P1：写入白名单。step 子 agent 借的是**同一个** `&mut Ctx`，所以一并生效。
            .with_write_allow(cfg.agent.write_allow.clone());
    // ---- P3：capsule 侧存（`agent.ctx.capsule`）----
    // 建不出来就**退化为内存**并如实说一句：侧存是"更不容易丢掉事实"，不是"必须"。
    if cfg.agent.ctx.capsule {
        let state_root = ctx.state_root().to_path_buf();
        match crate::agent::capsule::Capsule::create(&state_root, "ctx") {
            Ok(c) => {
                sink.log(
                    "info",
                    format!(
                        "[agent] capsule 侧存：{}（结果落盘，账本只留引用）",
                        c.dir().display()
                    ),
                );
                ctx.progress_mut().attach_capsule(c);
            }
            Err(e) => sink.log(
                "warn",
                format!(
                    "[agent] capsule 建不出来（{}），本次退化为内存侧存：{e}",
                    state_root.display()
                ),
            ),
        }
    }
    let mut plan_steps: Vec<PlanStep> = Vec::new();
    // ---- 计划执行（`step.execute_plan` 开启时）----
    // 游标在**引擎**手里：让模型每轮自选"我要做第几步"必然乱序、跳步、重复，而且
    // "选哪一步"本身还要烧一轮。模型的调整能力由"干预轮"补回来 —— 只有子步骤失败时，
    // 才把控制权交回模型一次。
    let mut plan_cursor: usize = 0;
    // content 通道连续被拒的次数（严格模式的"提示一次即拒"：第 1 次讲清道理，之后只说短话）。
    let mut content_strikes: usize = 0;
    // **连续**解析不出的轮数 —— 到上限就停（见 [`MAX_UNPARSEABLE_ROUNDS`]）。
    // 与 `content_strikes` 分开：那个是"话术"计数器（决定下一句说长还是说短），
    // 这个是"预算"闸（决定还烧不烧），两者语义不同，混用会改掉既有话术行为。
    let mut unparsed_streak: usize = 0;
    let mut plan_resets: u32 = 0;
    let mut intervene = false;
    // 已完成步骤的引擎侧事实（跨步骤唯一通道：子步骤输入包里的那一行）
    let mut plan_done: Vec<StepSummary> = Vec::new();
    // 每个步骤的终态（done / error + 说明）。收尾优先用它，而不是"文件是否落地"的推断
    let mut step_states: Vec<Option<(String, String)>> = Vec::new();
    // 总时长闸（0 = 不限）：父循环与每个子步骤共用同一条 deadline
    let deadline = (cfg.agent.max_elapsed_secs > 0)
        .then(|| started + Duration::from_secs(cfg.agent.max_elapsed_secs));
    // 本次是否真的交付了 final（收尾时决定计划步骤能不能算完成，见 settle_steps）
    let mut delivered = false;
    let mut out = AgentOutcome::default();
    let mut gate = GateState::default();
    // 提问计数与留痕：上限是硬闸（`ask` 是稀缺资源），留痕进会话存档
    let mut ask_n: u32 = 0;
    let mut asks: Vec<AskRecord> = Vec::new();
    // 这一轮读过哪些文件（依据核对的证据集合，复核员会看到这份清单）
    let mut read_paths: Vec<String> = Vec::new();
    // 本轮跑过哪些工具轮次（历史折叠的账本：老轮次的正文不再每轮重发）
    let mut round_slots: Vec<RoundSlot> = Vec::new();
    // 停滞守卫已触发 ⇒ 下一轮只收 `final`（模型再发工具调用就终止本轮 run）
    let mut force_final = false;
    // 停滞守卫只宽限**一轮**：让它把已确认的事实落进 findings，再强制交付。
    let mut final_grace_used = false;
    // 连续模型调用失败计数：成功一轮即清零
    let mut llm_failures: u32 = 0;

    // 布局契约的 S 段（不可变根）：**只在循环外建一次**（`Regions::new` 的注释：唯一一次写 root）。
    // 它有两个用处：① 根段字节的**唯一来源**；② 运行时 I1 判据的参照指纹。
    let ctx_root = context::Regions::new(agent_system_prompt());
    // 计量状态（`agent.ctx.metrics` 才用）：上一轮请求体 / 上一轮 S 段指纹 / 全程加权账，
    // 以及厂商回报的缓存命中合计（**真值**那一半）
    let mut prev_req: Option<Vec<ChatMessage>> = None;
    let mut prev_root_digest: Option<u64> = None;
    let mut tally = context::PrefixTally::default();
    let mut cache_hit_sum: u64 = 0;
    let mut cache_in_sum: u64 = 0;
    let mut cache_rounds: usize = 0;
    // G2（有界上下文）的两个读数：prompt 峰值（厂商回报的真值）与被截断的轮数
    let mut g2_max_prompt: usize = 0;
    let mut g2_truncated: usize = 0;

    // ---- P4：重基线调度（`agent.ctx.schedule`）----
    // 关着 ⇒ `history_keep_rounds` 照旧是触发器；开着 ⇒ 它降级为**安全下限**（拍板 3）。
    let sched_cfg = cfg.agent.ctx.schedule.then(|| scheduler::Cfg {
        horizon: cfg.agent.ctx.horizon,
        budget_tokens: cfg.agent.ctx.budget_tokens_effective(),
        carry_rate: cfg.agent.ctx.carry_rate,
        rebuild_rate: cfg.agent.ctx.rebuild_rate,
        floor_rounds: cfg.agent.history_keep_rounds,
    });
    // 距上一次重基线过了几轮（调度器的状态之一）
    let mut rounds_since_rebase = 0usize;
    // 上一次重基线之后阶梯的字节数（"压完会剩下多少"的实测）
    let mut base_bytes = 0usize;
    // 上一轮量到的阶梯字节数 / 每轮实测增长
    let mut ladder_bytes = 0usize;
    let mut growth_bytes = 0usize;

    let mut msgs = vec![ChatMessage::system(ctx_root.root())];
    for m in tail_history(history, 12) {
        msgs.push(if m.role == "assistant" {
            ChatMessage::assistant(m.text)
        } else {
            ChatMessage::user(m.text)
        });
    }
    // Connect：把宿主可连的外部能力一次性摆给模型（零连接时这段不出现，提示词不虚报能力）
    let connectables = conn.list().await.unwrap_or_default();
    let mut head = format!("项目根目录：{}", proj.display());
    if let Some(note) = connect_note(&connectables) {
        head.push_str(&format!("\n\n{note}"));
    }
    // 命令发现：本机到底有哪些命令能用。实测 run agent-20260920-152312（cloud-shop 修一个
    // Maven 依赖）有 5~6 轮纯粹在试探 `mvn` / `java` 在不在，而引擎早就探过 —— 只是从没
    // 告诉模型。可用与**不可用**都要写：只说"有 mvn"治不了空转，还得说"没有 gradle"。
    // 缺失工具怎么补，取决于宿主有没有接"环境准备"连接（没有就不许提 connect，不虚报能力）。
    let tools = discover::discover(
        proj,
        &cfg.discover,
        &cfg.verify.python_bin,
        &cfg.verify.node_bin,
    );
    let env_connector = connectables.iter().any(|c| c.kind == ENV_CONNECTOR_KIND);
    if let Some(note) = discover::render_note(&tools, env_connector) {
        head.push_str(&format!("\n\n{note}"));
    }
    // 发现结论同时**入账**（probe = 确定性证据）：下次不必重探也能知道，且说得清凭什么
    crate::mem::record_discovery(&tools);
    // 项目记忆：把"当前信念"（有界块）注进首条消息。记忆是**核心模块**（不可插件化），
    // 模型不在时自动退化为纯词法检索，并且块里会写明这件事 —— 降级可以，静默不行。
    if let Some(block) = crate::mem::prompt_block_for_current() {
        head.push_str(&format!("\n\n{block}"));
    }
    // 运行模式必须写进上下文：确认模式下 write 只暂存、execute 看到旧文件，
    // 不告诉模型这条，它会拿 execute 的失败反复当"改动错了"来修（见 policy_system_note）
    if let Some(note) = policy_system_note(policy) {
        head.push_str(&format!("\n\n{note}"));
    }
    // 批量调用：与 connect 清单 / 命令发现 / 写入策略同一条通道（首轮 user 消息），
    // 开关关掉就一个字都不提 —— 提示词不许广告一个引擎会拒的形状
    if cfg.agent.batch {
        head.push_str(&format!("\n\n{}", batch_hint(cfg.agent.batch_max, true)));
    }
    // 提问：开关关掉时提示词一字不提（与批调用同款：不虚报能力）
    if cfg.ask.enabled {
        head.push_str(&format!("\n\n{}", ask_hint(cfg)));
    }
    // 联网检索：开关关掉时一字不提（同上）。写的理由是"能力没进提示词 = 模型不会用"：
    // 服务端联网随时可用，但模型不知道自己能联网，就压根不会发起检索。
    if llm::web_search_on(&cfg.llm) {
        head.push_str(&format!("\n\n{WEB_SEARCH_HINT}"));
    }
    // 有图就随任务消息一起发（无图时 `with_images(vec![])` 与原来逐字节相同：
    // 手写的 `Serialize` 只在非空时把 content 变成块数组）
    msgs.push(
        ChatMessage::user(format!("{head}\n\n用户消息：\n{task}")).with_images(images.to_vec()),
    );

    sink.stage(
        "agent",
        "start",
        format!("工具循环（最多 {MAX_STEPS} 轮，写入策略：{policy:?}）"),
    );

    for step in 1..=MAX_STEPS {
        // 这一轮有没有记下新结论（停滞判据的一半）
        let mut new_findings_this_round = false;
        // 取消位：长动作（编译/测试/复核）都在这一层之下，先拦住再谈别的
        if is_cancelled(cancel) {
            out.answer = format!(
                "（用户取消：已完成 {} 轮工具调用、{} 个文件变更）",
                out.steps.len(),
                ctx.changes.len()
            );
            sink.log("warn", out.answer.clone());
            break;
        }
        // 总时长闸：到点就收手，把"为什么停"写进答复（子步骤内部看的是同一条 deadline）
        if let Some(dl) = deadline
            && Instant::now() >= dl
        {
            out.answer = format!(
                "（达到总时长上限 {} 秒，循环停止。已完成 {} 轮工具调用、{} 个文件变更；请基于以上进展继续指示。）",
                cfg.agent.max_elapsed_secs,
                out.steps.len(),
                ctx.changes.len()
            );
            sink.log("warn", out.answer.clone());
            break;
        }

        // 计划即执行：这一轮就做一件事 —— 跑完一个步骤（失败则再给模型一次干预轮）。
        // 步骤由引擎按序派发，模型不参与调度（理由见 plan_cursor 的注释）。
        if cfg.step.execute_plan && !intervene && plan_cursor < plan_steps.len() {
            let idx = plan_cursor;
            let cur = plan_steps[idx].clone();
            let total = plan_steps.len();
            // 派发**之前**就报"进行中"：原先 running 只在 write 之后才发，用户会盯着 ⌛
            // 干等几十秒（一个步骤内部往往要先 read 好几个文件）
            sink.step(
                idx + 1,
                total,
                &StepOutcome {
                    step_id: cur.id,
                    title: cur.title.clone(),
                    status: "running".into(),
                    files: cur.files.clone(),
                    ..Default::default()
                },
            );
            sink.log(
                "info",
                clip(
                    &format!(
                        "[agent] 第 {step} 轮 派发步骤 {}/{}「{}」",
                        idx + 1,
                        total,
                        cur.title.trim()
                    ),
                    240,
                ),
            );
            let inp = StepInput {
                project_root: proj,
                task,
                step: &cur,
                index: idx + 1,
                total,
                done: &plan_done,
            };
            // 致命模型错误上抛（与主循环同一判据）；其余一律是 Ok(status=error) 的报告
            let report = step_agent::run_step(cfg, &mut ctx, &inp, cancel, deadline, sink).await?;
            out.usage.add(&report.usage);
            let ok = report.ok();
            if !report.files.is_empty() {
                gate.dirty = true; // 子步骤写了文件 → final 时该跑全量验证
            }
            // 子步骤的 read 不进父上下文，但它查过什么必须让父知道（复核员的证据集合）
            for p in &report.read_paths {
                if !read_paths.contains(p) {
                    read_paths.push(p.clone());
                }
            }
            let facts = report.as_summary(&cur);
            step_states[idx] = Some((
                report.status.clone(),
                if facts.note.trim().is_empty() {
                    report.error.clone().unwrap_or_default()
                } else {
                    facts.note.clone()
                },
            ));
            if ok {
                plan_done.push(facts);
            }
            sink.step(
                idx + 1,
                total,
                &StepOutcome {
                    step_id: cur.id,
                    title: cur.title.clone(),
                    status: report.status.clone(),
                    notes: report.note.clone(),
                    error: report.error.clone(),
                    files: report.files.clone(),
                    no_files: report.files.is_empty(),
                    ..Default::default()
                },
            );
            let headline = clip(&report.headline(&cur), 400);
            out.steps.push(StepTrace {
                step,
                tool: "step".into(),
                brief: headline.clone(),
                ok,
            });
            sink.log(
                if ok { "info" } else { "warn" },
                format!(
                    "[agent] 第 {step} 轮 step {} {headline}",
                    if ok { "✓" } else { "✗" }
                ),
            );
            plan_cursor += 1;
            if !ok {
                // 停下交回模型一次：第 1 步就崩了，闷头往下跑全是白费
                intervene = true;
                msgs.push(ChatMessage::user(step_failure_feedback(
                    &cur,
                    idx + 1,
                    total,
                    &report,
                )));
            }
            continue;
        }

        // ---- 进展记忆：块放**哪儿**由布局契约决定（`agent.ctx.layout`）----
        //
        // 关（默认 = v1.1 行为）：块拼进 `msgs[0]` 尾部。位置固定，看着"对缓存友好"，
        // 但 **system 属不可变根 S** —— 每轮重建它，就等于每轮作废它**之后**的一切
        // （任务头 + 整段历史都在后面）。字节流在块里就分叉了，后面的消息只是"内容相同"，
        // 位置已经不同 ⇒ 一个字节都算不上复用。证据与判据见
        // `doc/v1.2/架构-上下文账本与重基线调度-v1.2.md` §1（不变式 I1）。
        // 开：块是**易变尾 A** —— 追加在全部稳定字节（S + E）之后，每轮整块重建，
        // 且**只活这一次请求**；`msgs[0]` 从头到尾一个字节不动。
        //
        // 为什么调完就摘掉，而不是留到下一轮开头再删：`fold_history` 是**按下标**改老轮次
        // 正文的（见 `RoundSlot`），任何跨轮的插/删都会让已记录的下标整体偏一格 ——
        // 折错消息、甚至越界 panic。摘掉之后 `msgs` 的下标纪律与今天**一字不差**，
        // 而线上请求体仍是 `[S][E][A]`（尾永远是最后一条消息，两臂在这一点的字节形状相同）。
        let mut tail_idx: Option<usize> = None;
        if cfg.agent.findings_enabled {
            let spill = ctx.state_root().join("findings");
            if ctx.progress_mut().prompt_block(Some(&spill)).is_some() {
                sink.log(
                    "info",
                    clip(
                        &format!("[agent] 第 {step} 轮 {}", ctx.progress().byte_account()),
                        240,
                    ),
                );
            }
            let block = ctx.progress().render();
            if cfg.agent.ctx.layout {
                tail_idx = Some(msgs.len());
                msgs.push(ChatMessage::user(block));
            } else {
                // 根段从 `ctx_root` 来（而不是再喊一次 `agent_system_prompt()`）：S 段有**一个**
                // 来源，运行时 I1 的参照指纹也是它 —— 两处各喊一次就成了两份会漂移的字节。
                msgs[0] = ChatMessage::system(format!("{}{}", ctx_root.root(), block));
            }
        }

        // 计量（`agent.ctx.metrics`，默认关）：请求体在**发之前**留一份 ——
        // 真要量的就是"厂商看到了什么"，而那就是带上易变尾的这一条（事后摘了尾就量不着了）。
        let sent = cfg.agent.ctx.metrics.then(|| msgs.clone());

        let llm_res = llm::chat(
            &cfg.llm,
            cfg.llm_fallback.as_ref(),
            &msgs,
            true,
            cfg.llm.tool_protocol,
        )
        .await;
        // 摘尾：**所有**出口都在这一行之后（含下面"退避后重试"那条 continue）⇒
        // 下一轮不会攒下第二条尾，任何一条路径都不会把尾漏进 `msgs` 的记账里
        if let Some(i) = tail_idx {
            msgs.remove(i);
        }

        // 量尺（只在 `agent.ctx.metrics` 打开时算）：① 公共前缀占比（**代理**）
        // ② 厂商回报的命中 token（**真值**，实测可用）③ S 段指纹（运行时 I1）。
        // 关掉时零额外计算、零额外日志 —— 两臂的门禁数字因此逐项不变。
        if let Some(sent) = sent {
            let stat = context::prefix_stat(prev_req.as_deref(), &sent);
            tally.add(stat);
            // 运行时 I1：S 段指纹必须逐轮相同。开着布局还变 ⇒ 契约被谁绕过去了。
            let d = context::digest(&sent[0].content);
            let root_note = match prev_root_digest {
                None => format!("S 段 {d:016x}（首轮）"),
                Some(p) if p == d => format!("S 段 {d:016x} 未变 ✓"),
                Some(p) => {
                    if cfg.agent.ctx.layout {
                        sink.log(
                            "error",
                            format!(
                                "[agent] 第 {step} 轮 布局契约 I1 被违反：S 段指纹 {d:016x} ≠ 上轮 {p:016x}\
                                 （layout=on 时它必须逐轮逐字节相同）"
                            ),
                        );
                    }
                    format!("S 段 {d:016x} ≠ 上轮 {p:016x}（每轮重建）")
                }
            };
            // 厂商回报的命中 token（**真值**）：拿不到就如实说"没回报"，
            // 不许把"这家不报这个数"说成"命中 0"。
            // 写法上刻意用 `if let Ok(..)`：U52 那道门禁按 `Err(` 上下文抓后端文案，
            // 而这几句是**日志**不是回给前端的错误（引擎日志一律中文，与既有 info 行同款）。
            let vendor = if let Ok(r) = llm_res.as_ref() {
                match r.usage.cache_read() {
                    Some(hit) => {
                        cache_hit_sum += hit;
                        cache_in_sum += r.usage.prompt_tokens;
                        cache_rounds += 1;
                        format!(
                            " · 厂商命中 {hit} tokens（{:.1}%）",
                            hit as f64 * 100.0 / r.usage.prompt_tokens.max(1) as f64
                        )
                    }
                    None => " · 厂商未回报命中 token（**拿不到 ≠ 命中 0**）".to_string(),
                }
            } else {
                " · 本轮请求失败：没有回报".to_string()
            };
            sink.log(
                "info",
                clip(
                    &format!(
                        "[agent] 第 {step} 轮 上下文计量：公共前缀 {} · {root_note}{vendor}",
                        stat.render()
                    ),
                    300,
                ),
            );
            prev_root_digest = Some(d);
            prev_req = Some(sent);
        }

        let reply = match llm_res {
            // 清零挪到「解析成功」处（unparsed_stream 旁边）：HTTP 成功但输出不可用
            // （半成功）不许清零 —— 否则空内容/坏参数交替永远凑不满连续 3 次
            Ok(r) => r,
            Err(e) => {
                llm_failures += 1;
                // 鉴权/余额这类确定性失败重试无意义；其余（空内容/网络抖动）退避后
                // 吃下一轮 —— 轮次预算本身就是防死循环的闸
                if llm::is_fatal_error(&e) || llm_failures >= LLM_FAIL_LIMIT {
                    sink.log("error", format!("[agent] 第 {step} 轮模型调用失败：{e}"));
                    return Err(e);
                }
                sink.log(
                    "warn",
                    format!(
                        "[agent] 第 {step} 轮模型调用失败（连续 {llm_failures}/{LLM_FAIL_LIMIT}，退避后重试）：{e}"
                    ),
                );
                // 失败回灌进可变尾：模型对"上一条输出蒸发了"不再毫不知情，
                // 下一条请求它就能改小体量（不回灌时实测原样重发、六轮零进展）
                msgs.push(ChatMessage::user(llm_failure_feedback(&e)));
                tokio::time::sleep(Duration::from_millis(1200 * llm_failures as u64)).await;
                continue;
            }
        };
        out.usage.add(&reply.usage);
        // G2：prompt 的峰值（真值来自厂商回报）+ 是否被 max_tokens 截断
        g2_max_prompt = g2_max_prompt.max(reply.usage.prompt_tokens as usize);
        if reply.finish_reason.as_deref() == Some("length") {
            g2_truncated += 1;
        }
        // 服务端联网是**黑盒注入**：检索结果直接进了上下文，标题与链接都不回传，
        // 引擎只拿得到查询词。把查询词当作一条取证记下来 —— 否则复核员眼里
        // 模型"凭空知道"最近的事，就会按"证据无处可查"打回，重演上一个死锁。
        if !reply.web_queries.is_empty() {
            let listed = reply
                .web_queries
                .iter()
                .map(|q| format!("- {q}"))
                .collect::<Vec<_>>()
                .join("\n");
            ctx.note_probe(WEB_SEARCH_PROBE_LABEL, &listed);
            sink.log(
                "info",
                format!(
                    "[agent] 第 {step} 轮服务端联网检索 {} 次",
                    reply.web_queries.len()
                ),
            );
        }
        // **严格模式（`llm.tool_protocol`，默认开）：动作只认工具调用。** content 里那一套一律
        // **不执行** —— 但要"认出来再拒"：只回"无法解析"没有指向性，模型会换包装重发
        // （真跑第 13–17 轮连烧五次就是这么来的，见 [`content_channel_error`]）。
        // `tool_protocol = false` 才回到老协议的兼容通道（那时 content 里的动作照执行）。
        let via_tools = !reply.tool_calls.is_empty();
        if !via_tools && cfg.llm.tool_protocol {
            content_strikes += 1;
        }
        let parsed = if via_tools {
            parse_tool_calls(&reply.tool_calls, cfg.agent.batch_max, cfg.agent.batch)
        } else if cfg.llm.tool_protocol {
            Err(content_channel_error(
                &reply.content,
                content_strikes,
                MAIN_TOOLS_HINT,
            ))
        } else {
            parse_actions(&reply.content, cfg.agent.batch_max, cfg.agent.batch)
        };
        let mut actions = match parsed {
            Ok(a) => a,
            Err(e) => {
                // 解析失败不终止：把错误告诉模型让它重出（消耗轮次预算，防死循环）。
                // 截断（finish_reason=length）与格式烂是两种病：截断必须叫模型写短，
                // 否则它原样重发再截断一次（实测连烧三轮才碰巧写短过关）
                // **连续**失败到上限：停下来并给出可执行的诊断，不再空烧轮次。
                // 这条是用户报的"发一句你好就无限死循环"的正面治疗：旧行为只 `continue`，
                // 兜底是 96 轮 / 30 分钟 —— 用户看到的就是"一直转"。
                unparsed_streak += 1;
                if unparsed_streak >= MAX_UNPARSEABLE_ROUNDS {
                    sink.log(
                        "error",
                        format!("[agent] 连续 {unparsed_streak} 轮无工具调用，停止本轮"),
                    );
                    out.answer = unparseable_diagnosis(unparsed_streak, &reply.content);
                    return Err(out.answer.clone());
                }
                let truncated = reply.finish_reason.as_deref() == Some("length");
                let tag = if truncated {
                    "（finish_reason=length，已要求精简重发）"
                } else {
                    ""
                };
                sink.log(
                    "warn",
                    format!("[agent] 第 {step} 轮输出无法解析{tag}：{e}"),
                );
                msgs.push(ChatMessage::user(parse_failure_feedback(
                    &e,
                    reply.finish_reason.as_deref(),
                    cfg.llm.tool_protocol,
                )));
                continue;
            }
        };
        // 这一轮解析成功 ⇒ 连续失败计数清零（偶发一次格式烂不该累计成"病"）。
        // `llm_failures` 也在这里清零（而不是 HTTP 200 处）：半成功轮不许清零 ——
        // 实测 step 第 28-33 轮，坏参数轮把模型失败计数清零，空内容/坏参数
        // 交替永远凑不满连续 3 次，六轮零进展烧到底。
        unparsed_streak = 0;
        llm_failures = 0;
        // ---- 停滞守卫的硬终止：上一轮已判停滞，这一轮模型又发工具调用 ⇒ 立刻收手 ----
        //
        // 必须硬：模型不照做就终止，而不是再等一轮 —— "守卫能被绕过"等于没守卫。
        // `break`（不是 return）：收尾的 settle_steps / 进程回收 / 门禁补注都还得跑。
        // 强制收尾那轮**允许** record_findings：指令里写的是"先记 findings、再 final"，
        // 只认 final 会自相矛盾 —— 实测第 22 轮模型正调 record_findings（一条真结论），
        // 被这一行当场杀掉、连结论一起丢了。允许一轮"只记不答"的宽限，下一轮仍强制 final。
        let only_findings = !actions.is_empty()
            && actions
                .iter()
                .all(|a| matches!(a, Action::Findings(_) | Action::Plan(_)));
        if force_final && !final_grace_used && only_findings {
            final_grace_used = true;
            sink.log(
                "info",
                format!("[agent] 第 {step} 轮停滞守卫：宽限一轮，让它把已确认的事实记下来"),
            );
        } else if force_final && !actions.iter().any(|a| matches!(a, Action::Final(_))) {
            sink.log(
                "warn",
                format!("[agent] 第 {step} 轮 停滞守卫：模型仍发工具调用，终止本轮 run"),
            );
            out.answer = stall_report(cfg, &ctx, step);
            break;
        }
        // 迁移期的观测点：这一轮的动作是从哪条协议来的。`兼容层`只可能出现在
        // `tool_protocol=false`（回滚）那一侧 —— 严格模式下 content 通道在上一段就被拒了。
        sink.log(
            "info",
            format!(
                "[agent] 第 {step} 轮 {} → {} 个动作",
                if via_tools {
                    "工具调用（标准协议）"
                } else {
                    "content 里的 JSON（兼容层，tool_protocol=false）"
                },
                actions.len()
            ),
        );
        // 记下模型那半在 msgs 里的落点 —— 历史折叠要靠它回头把这一格的正文换掉。
        // （ask / final / 解析失败那几条 `continue` 不会走到"记落点"，所以不会留下悬空的轮次）
        let assistant_idx = msgs.len();
        // 工具轮的 content 天生是空的（动作全在 `tool_calls` 里），原样回显等于给模型看一条
        // 空消息 —— 所以把**它自己发的那串调用**写回去（见 [`tool_calls_echo`]）。
        let mut as_msg = ChatMessage::assistant(if via_tools {
            tool_calls_echo(&reply.tool_calls)
        } else {
            reply.content.clone()
        });
        // anthropic 扩展思考：这一轮的思考块挂回助手消息 —— 下一轮回灌要**逐字**带回去
        // （含 `signature`，服务端校验它）。丢了它，开了 `thinking` 的请求下一轮会被打回，
        // 而且模型每轮都在失忆（`142b4ab` 自述遗留的那条）。
        as_msg.thinking = reply.thinking.clone();
        msgs.push(as_msg);

        // 第五个动作：向委托人提问。控制动作独占一轮（批里的 ask_user 已被当面拒）。
        // 上限是硬闸：`ask` 是稀缺资源，超了要求"交付并声明假设"，而不是继续追问。
        if actions.len() == 1 && matches!(actions[0], Action::Ask(_)) {
            let Action::Ask(mut spec) = actions.remove(0) else {
                unreachable!("上面刚判过是 ask")
            };
            // 开关关掉：提示词一字不提 + 这里一律拒（不虚报能力，老语义逐字不变）
            if !cfg.ask.enabled {
                sink.log(
                    "warn",
                    clip(
                        &format!("[agent] 第 {step} 轮 ask_user 被拒：提问未开启"),
                        200,
                    ),
                );
                msgs.push(ChatMessage::user(
                    "ask_user 在当前配置下已关闭。不要再提问：请**交付**，并在说明里写出你的假设与需要用户确认的点。"
                        .to_string(),
                ));
                continue;
            }
            if ask_n >= cfg.ask.max_per_run {
                sink.log(
                    "warn",
                    clip(
                        &format!(
                            "[agent] 第 {step} 轮 ask_user 被拒：本轮上限 {}",
                            cfg.ask.max_per_run
                        ),
                        200,
                    ),
                );
                msgs.push(ChatMessage::user(format!(
                    "ask_user 次数已用完（一次 run 最多 {} 次）。不要再提问：请**交付**，并在说明里写出你的假设与需要用户确认的点。",
                    cfg.ask.max_per_run
                )));
                continue;
            }
            ask_n += 1;
            let id = format!("ask-{ask_n}");
            // 超时由实现负责，但"等多久"由配置定 —— 模型给的 timeout_secs 一律忽略
            spec.timeout_secs = cfg.ask.timeout_secs;
            sink.log(
                "info",
                clip(
                    &format!("[agent] 第 {step} 轮 ask_user：{}", spec.question),
                    240,
                ),
            );
            let rec = match asker.ask(&id, &spec).await {
                Ok(ans) => {
                    sink.log(
                        "ok",
                        clip(&format!("[agent] 第 {step} 轮 用户回答：{}", ans.text), 200),
                    );
                    msgs.push(ChatMessage::user(ask_answer_note(&id, &ans, &spec)));
                    AskRecord {
                        id: id.clone(),
                        question: spec.question.clone(),
                        why: spec.why.clone(),
                        options: spec.options.clone(),
                        answer: Some(ans.text.clone()),
                        state: "answered".into(),
                        ts: crate::workspace::now_iso(),
                    }
                }
                Err(e) => {
                    sink.log(
                        "warn",
                        clip(
                            &format!("[agent] 第 {step} 轮 ask_user 无回答：{}", e.reason()),
                            200,
                        ),
                    );
                    // fail-closed：拒绝依赖它的动作，明确告诉模型"这不是同意"
                    msgs.push(ChatMessage::user(ask_failed_note(&id, &e, &spec)));
                    AskRecord {
                        id: id.clone(),
                        question: spec.question.clone(),
                        why: spec.why.clone(),
                        options: spec.options.clone(),
                        answer: None,
                        state: e.state().into(),
                        ts: crate::workspace::now_iso(),
                    }
                }
            };
            asks.push(rec);
            out.asks = asks.clone();
            continue;
        }

        // 交付走门禁：有改动先过全量验证，再过干净上下文的复核。
        // final 只可能是单动作 —— 批里的 final 已被 parse_actions 当面拒掉
        if actions.len() == 1 && matches!(actions[0], Action::Final(_)) {
            let Action::Final(text) = actions.remove(0) else {
                unreachable!("上面刚判过是 final")
            };
            let blocked = gate_before_final(
                cfg,
                proj,
                &ctx,
                task,
                &read_paths,
                &text,
                &mut gate,
                &mut out,
                cancel,
                sink,
            )
            .await;
            match blocked {
                Some(observation) => {
                    let level = if observation.contains("[机械验证") {
                        "warn"
                    } else {
                        "info"
                    };
                    sink.log(
                        level,
                        format!(
                            "[agent] 第 {step} 轮交付被打回：{}",
                            clip(&observation, 300)
                        ),
                    );
                    msgs.push(ChatMessage::user(observation));
                    continue;
                }
                None => {
                    out.answer = text;
                    delivered = true;
                    sink.log("ok", clip(&format!("[agent] 完成，共 {step} 轮"), 200));
                    break;
                }
            }
        }

        // ---- 执行这一轮的动作：按**波次**（波内并发、波间按序）----
        // 一波内互不冲突（见 conflicts）：一串读、一串命令就是一波 —— 那就是"一起并发"。
        let n = actions.len();
        let waves = batch_waves(&actions);
        // ---- 去重账本（`agent.ctx.dedup`）的**执行前预检** ----
        //
        // 位置选在这里（波次调度之前）是刻意的：命中的那些**根本不进波** ⇒ 不占并发、
        // 不占超时、不改任何调度形状；而"要不要跳过执行"这唯一一次决策也只发生在这一处。
        // 版本快照必须在**执行之前**取（执行本身可能改文件）。
        //
        // 仪器那一半（审计）跟着 `metrics` 走、与 `dedup` 无关：**对照臂也要能量**
        // "重复执行了几次"，否则"下降 ≥50%"这条判据没有基线。
        let repo_ver = if cfg.agent.ctx.dedup || cfg.agent.ctx.metrics {
            ledger::repo_version(proj)
        } else {
            None
        };
        let plans: Vec<Option<ledger::Plan>> = if cfg.agent.ctx.dedup || cfg.agent.ctx.metrics {
            ledger::precheck(&mut ctx, &actions, &repo_ver, cfg.agent.ctx.dedup)
        } else {
            Vec::new()
        };
        if let Some(cap) = cfg.agent.ctx.dedup.then_some(cfg.agent.ctx.dedup_max_bytes) {
            ctx.progress_mut().set_dedup_cap(cap);
        }
        let mut slots: Vec<Option<CallResult>> = (0..n).map(|_| None).collect();
        for wave in &waves {
            if wave.len() == 1 {
                let i = wave[0];
                // 去重命中：**不执行**（结果在预检里已经算好，连波都不进）
                let one = if let Some(hit) = reused_slot(&plans, i) {
                    hit
                } else {
                    match &actions[i] {
                        // plan 是控制动作（parse 拒了批里的 plan，所以只可能是单动作）：
                        // 就地更新引擎手里的计划与游标，回灌文本与老版本逐字一致
                        Action::Plan(steps) => {
                            let is_reset = !plan_steps.is_empty();
                            if cfg.step.execute_plan && is_reset && plan_resets >= MAX_PLAN_RESETS {
                                // 重排次数用尽：忽略这一次，按现有计划继续 —— 否则
                                // "失败 → 重排 → 又失败 → 再重排"能把整个轮次预算烧光却什么都不产出
                                (
                                    "plan".into(),
                                    "重排被忽略（已达上限）".into(),
                                    Ok(format!(
                                        "计划重排次数已达上限（{MAX_PLAN_RESETS} 次），继续按现有计划执行。"
                                    )),
                                )
                            } else {
                                plan_steps = steps.clone();
                                // 新计划 = 从头执行（游标归零）。已完成的步骤事实留在 plan_done 里，
                                // 子步骤输入包会带上它，模型仍能看到"前面做过什么"。
                                plan_cursor = 0;
                                step_states = vec![None; plan_steps.len()];
                                if is_reset {
                                    plan_resets += 1;
                                }
                                let p = plan_to_outline(task, plan_steps.clone());
                                sink.plan(&p);
                                (
                                    "plan".into(),
                                    format!("{} 个步骤", plan_steps.len()),
                                    Ok(if cfg.step.execute_plan {
                                        "计划已收到，引擎将按序执行各步骤；全部做完后输出 final。\
                                     若要调整计划，重新输出 plan（注意：会从第 1 步重新执行）。"
                                            .into()
                                    } else {
                                        "任务清单已展示给用户（大纲区），按清单继续。".into()
                                    }),
                                )
                            }
                        }
                        // 没命中（或这条本来就不参与去重）：照常执行 —— 与 v1.1 逐字一致
                        _ => exec_one(cfg, proj, policy, conn, &mut ctx, actions[i].clone()).await,
                    }
                };
                slots[i] = Some(one);
            } else {
                run_wave(
                    cfg, proj, policy, conn, &mut ctx, &actions, &plans, wave, &mut slots,
                )
                .await;
            }
        }
        let mut results: Vec<CallResult> = slots
            .into_iter()
            .enumerate()
            .map(|(i, o)| {
                o.unwrap_or_else(|| {
                    (
                        "call".into(),
                        format!("第 {} 条调用", i + 1),
                        Err("未执行".into()),
                    )
                })
            })
            .collect();

        // 记账 / 轨迹 / 日志：批里每条都有自己的一行，轮号相同 —— 那正是省下来的轮
        let mut any_write_ok = false;
        for (i, (tool, brief, res)) in results.iter().enumerate() {
            let ok = res.is_ok();
            // ---- 去重账本：**真执行**的那些记账 + 仪器喂数（命中不记，只计数）----
            //
            // 记账用**执行前**的版本快照（`p.versions`）：执行本身可能改文件，
            // 事后取会把"这次读到的东西"绑定到"改完之后的状态"上 —— 那正是
            // "静默给出错答案"的入口。
            let hit = plans
                .get(i)
                .and_then(|p| p.as_ref())
                .is_some_and(|p| p.reuse.is_some());
            if let Some(p) = plans.get(i).and_then(|p| p.as_ref()) {
                if !hit {
                    // **顺序要紧**：先审计、后记账。反过来的话，审计问的"账本会不会拦下它"
                    // 会撞上**刚为这次执行记下的那一条** ⇒ 每次执行都被数成"重复"
                    // （实测踩过：唯一执行 0 · 仍重复执行 2）。
                    if cfg.agent.ctx.metrics {
                        ctx.progress_mut().dedup_audit(Some(&p.call), &p.versions);
                    }
                    if cfg.agent.ctx.dedup
                        && let Ok(text) = res
                    {
                        ctx.progress_mut().dedup_record(
                            p.call.clone(),
                            text.clone(),
                            p.versions.clone(),
                            step as u32,
                        );
                    }
                }
            } else if cfg.agent.ctx.metrics
                && matches!(
                    &actions[i],
                    Action::Write(_)
                        | Action::Execute(_, _)
                        | Action::ExecBg(_)
                        | Action::Proc(_, _)
                        | Action::Connect(_)
                )
            {
                // 不纯的那些（写 / 白名单外的 execute / 后台 / 句柄 / connect）也要进审计：
                // 上游的"唯一执行"里含它们，少了这一支，两边的计数就对不上。
                ctx.progress_mut()
                    .dedup_audit(None, &ledger::VersionVec::default());
            }
            if *tool == "write" && ok {
                any_write_ok = true;
            }
            // 读过什么 = 依据核对的证据集合（复核员会看到这份清单）。
            // 记的是**路径**：同一文件读了两个窗口算"读过它"一次。
            if ok
                && let Action::Read(spec) = &actions[i]
                && !read_paths.contains(&spec.path)
            {
                read_paths.push(spec.path.clone());
            }
            // 取证留痕：命令输出原本用完即弃（只活在 msgs 里），复核员看不到 →
            // 靠命令取证的答复永远"证据无处可查"。单动作与批都在这一汇总。
            if let Ok(text) = res
                && ok
                && *tool == "execute"
            {
                let cmd = match &actions[i] {
                    Action::Execute(c, _) => c.clone(),
                    Action::ExecBg(s) => s.cmd.clone(),
                    Action::Proc(op, h) => format!("{} {h}", proc_op_name(*op)),
                    _ => brief.clone(),
                };
                ctx.note_probe(&cmd, text);
            }
            out.steps.push(StepTrace {
                step,
                tool: tool.clone(),
                brief: brief.clone(),
                ok,
            });
            let head = if n == 1 {
                format!("第 {step} 轮")
            } else {
                format!("第 {step} 轮 [{}/{}]", i + 1, n)
            };
            let level = if ok { "info" } else { "warn" };
            let icon = if ok { "✓" } else { "✗" };
            // 命中的那几条给一条**看得见**的 trace：`dedup <tool> <norm>` 这个形状是契约的
            // 一部分（ui-smoke U70 与真 run 的读数都按它数），措辞别随手改。
            let line = match plans
                .get(i)
                .and_then(|p| p.as_ref())
                .and_then(|p| p.reuse.as_ref())
            {
                // 侧存形态要把"召回"这件事写在同一条 trace 里：`capsule 召回 + sha256 ✓`
                // 就是"为召回而重跑 = 0"的可见证据（两个形状都保留 `dedup <tool> <norm>` 前缀，
                // 判据与真跑读数都按这个形状数）。
                Some(r) if r.from_capsule => format!(
                    "[agent] {head} dedup {tool} {brief} ← 复用第 {} 轮的结果                     （capsule 召回，sha256 ✓，资源版本未变，未重跑）",
                    r.step
                ),
                Some(r) => format!(
                    "[agent] {head} dedup {tool} {brief} ← 复用第 {} 轮的结果（资源版本未变，未重跑）",
                    r.step
                ),
                None => format!("[agent] {head} {tool} {icon} {brief}"),
            };
            sink.log(level, clip(&line, 400));
        }
        if n > 1 {
            // 用户读日志时最想知道的就是"这一轮省了几次往返、几条真并发"
            let same_wave: usize = waves.iter().filter(|w| w.len() > 1).map(|w| w.len()).sum();
            sink.log(
                "info",
                clip(
                    &format!(
                        "[agent] 第 {step} 轮 一批 {n} 个调用（{} 波，同波并发 {same_wave} 条）",
                        waves.len()
                    ),
                    200,
                ),
            );
        }

        // ---- 引擎账本 + 进展记账（与 findings 共用一份状态；**不折叠**）----
        if cfg.agent.findings_enabled {
            let mut wrote_this_round = false;
            let mut new_region_reads = 0usize;
            // 重复读守卫要往**结果正文**里追加一行，而这里正持着 `results` 的不可变借用
            // ⇒ 先收集、再统一追加（同一轮里同一文件读两次也只追加一次）。
            let mut repeat_notes: Vec<(usize, String)> = Vec::new();
            for (i, (tool, brief, res)) in results.iter().enumerate() {
                let ok = res.is_ok();
                if *tool == "write" && ok {
                    wrote_this_round = true;
                }
                if *tool == "findings" {
                    new_findings_this_round = true;
                }
                // 账本一行 = **形状 + 结局**，刻意**不含结果内容**：
                // 带上"结果第一行"看着很有用，实际是偷偷绕过折叠（旧结果的头 100 字符永远在场），
                // 而且账本会随轮数线性涨。要内容就去 findings（模型写的结论）或重新精确读。
                ctx.progress_mut()
                    .ledger_line(ledger_line_for(step, tool, brief, res));
                // 重复读守卫（**账本开着时这条退场**）：`agent.ctx.dedup` 的"包含关系 + 版本"
                // 判据替掉了"区间相交"这条近似（见 v1.2 架构文档 §3）—— 一个被账本覆盖的读，
                // 要么命中（那已经有复用标注了），要么真读了新东西（账本记下 ⇒ 算进展）。
                let ledger_covered =
                    cfg.agent.ctx.dedup && plans.get(i).and_then(|p| p.as_ref()).is_some();
                if !ledger_covered && let Action::Read(spec) = &actions[i] {
                    let start = spec.offset.unwrap_or(1).max(1).min(u32::MAX as usize) as u32;
                    let end = start
                        .saturating_add(spec.limit.unwrap_or(400).min(u32::MAX as usize) as u32)
                        .saturating_sub(1);
                    match ctx.progress_mut().note_read(&spec.path, start, end, step) {
                        Some(earlier) => {
                            let note = ctx.progress().repeat_read_note(&spec.path, earlier);
                            repeat_notes.push((i, note));
                        }
                        // 首次读、且结果不短 ⇒ 追加一次「该沉淀了」的节拍。
                        //
                        // 实测补的：某次 run 43 次工具调用、**0 条 findings** —— 工具在场上却没人用，
                        // 于是"读文件"退化成"再搜一遍"。教训与 step 光标同一条：**引擎自己握节拍**，
                        // 不等模型自觉。这一步零额外轮次，只在结果尾部加一句话。
                        None => {
                            // 读到之前没读过的区域 ⇒ 算进展（否则「系统读完一个文件」会被判成停滞）
                            new_region_reads += 1;
                            let long = matches!(&results[i].2, Ok(t) if t.chars().count() >= 600);
                            if long {
                                repeat_notes.push((
                                    i,
                                    "\n（读完就问自己一句：**这改变了什么？** 改变了就立刻 \
                                     record_findings（claim + 证据指针 path:line），没有改变就别记；\
                                     理由与思路写进 note 字段。）"
                                        .to_string(),
                                ));
                            }
                        }
                    }
                }
            }
            for (i, note) in repeat_notes {
                if let Ok(text) = &mut results[i].2 {
                    text.push_str(&note);
                }
            }
            ctx.progress_mut().note_round(
                step,
                new_findings_this_round,
                wrote_this_round,
                new_region_reads > 0,
            );
            if new_findings_this_round {
                sink.log(
                    "info",
                    clip(
                        &format!("[agent] 第 {step} 轮 {}", ctx.progress().byte_account()),
                        240,
                    ),
                );
            }
            // 停滞判据：连续 K 轮既没有新结论、也没有文件变更 ⇒ 下一轮强制 final
            if cfg.agent.stall_rounds > 0
                && ctx.progress().stalled(step) >= cfg.agent.stall_rounds
                && !force_final
            {
                force_final = true;
                sink.log(
                    "warn",
                    format!(
                        "[agent] 第 {step} 轮 停滞守卫触发：连续 {} 轮无新结论、无文件变更 ⇒ 强制 final",
                        cfg.agent.stall_rounds
                    ),
                );
                msgs.push(ChatMessage::user(stall_instruction(cfg.agent.stall_rounds)));
            }
        }

        // execute_plan 下步骤状态由派发逻辑报告（引擎知道"这一步跑完了"这个事实，比
        // "声明文件是否落地"的推断准得多），两条通道混用只会互相打架
        if !cfg.step.execute_plan && !plan_steps.is_empty() && any_write_ok {
            emit_step_progress(sink, &plan_steps, &ctx.overlay);
        }

        // 机械验证（窄层）：事实触发 —— 这一轮真的写成了文件（批里有写也算一次）。
        // 失败当成"观察"回灌，不打断这一轮（它是即时反馈，不是交付判据；交付判据在 gate_before_final）。
        if any_write_ok && !ctx.changes.is_empty() {
            gate.dirty = true;
            if cfg.gate.narrow && !is_cancelled(cancel) {
                let v = narrow_verify(cfg, &ctx.changes).await;
                let failed = v.status == "failed";
                let observation = v.to_observation();
                sink.log(
                    if failed { "warn" } else { "info" },
                    format!("[agent] 第 {step} 轮 {}", clip(&v.verdict, 200)),
                );
                sink.verify(&v);
                out.verifications.push(v);
                if failed {
                    msgs.push(ChatMessage::user(observation));
                }
            }
        }

        // 折叠账本的原料：形状与结果摘要都在**回灌之前**算好
        // （`n == 1` 那条分支会把 `results` 整个移走）
        let all_ok = results.iter().all(|(_, _, r)| r.is_ok());
        let shape = call_shape(&actions);
        let outcome = results
            .iter()
            .map(|(tool, brief, res)| outcome_line(tool, brief, res))
            .collect::<Vec<_>>()
            .join("；");

        // 回灌：单动作保持老形状（模型学过它），批走 results 数组、按声明顺序逐条给
        if n == 1 {
            let (_, _, r) = results.into_iter().next().expect("n == 1 时必有结果");
            msgs.push(ChatMessage::user(json_result(r)));
        } else {
            msgs.push(ChatMessage::user(batch_json_result(&results)));
        }

        // ---- 历史折叠：老轮次的正文不再每轮重发 ----
        //
        // 账本在第 1 轮就记（哪怕这次不折），因为"保留最近 keep 轮"要看的是**总轮数**，
        // 不是"这一轮动了没有"。折叠是幂等的，所以每轮都调它没有副作用。
        round_slots.push(RoundSlot {
            assistant: assistant_idx,
            result: msgs.len() - 1,
            call_digest: format!("{{\"folded\":\"第 {step} 轮调用（{shape}）—— 请求正文已折叠\"}}"),
            outcome_digest: format!(
                "{{\"ok\":{all_ok},\"folded\":\"第 {step} 轮结果（{outcome}）正文已折叠。\
                 **若这段得出了结论，请立刻 record_findings（claim + 证据 path:line）**；\
                 确需正文时用 read 带精确区间取回，别整文件重读。\"}}"
            ),
        });
        if cfg.agent.history_trim {
            let keep = cfg.agent.history_keep_rounds.max(1);
            // 阶梯现在多大（字节；token 由 `estimate_tokens` 换算 —— **字节代理**，
            // 架构 §6 允许，但 trace 里必须写明是代理）
            let now_bytes: usize = msgs_bytes(&msgs);
            if let Some(sc) = &sched_cfg {
                // ---- P4：何时压由调度器说了算（`keep_rounds` 只是安全下限）----
                // MPC：用**实测**的增长重新规划一次，只取第一步的动作。
                let st = scheduler::State {
                    ladder_tokens: scheduler::estimate_tokens(now_bytes),
                    base_tokens: scheduler::estimate_tokens(base_bytes.max(1)),
                    growth_tokens: scheduler::estimate_tokens(growth_bytes),
                    rounds_since: rounds_since_rebase,
                    // 视野是**配置的 H**（默认 96 = MAX_STEPS）：`0` = 已经跑过 H ⇒ 未知 horizon，
                    // 由 ski-rental 兜底（架构 §7）。用 MAX_STEPS 硬编码会让 horizon 配置失效 ——
                    // 那正是"拍出来的数"的另一种写法。
                    remaining: sc.horizon.saturating_sub(step),
                };
                let d = scheduler::decide(sc, &st);
                // 每次决策都留痕：形状是契约的一部分（ui-smoke U70 与真 run 读数按它数）
                sink.log(
                    "info",
                    format!(
                        "[agent] 第 {step} 轮 {}（字节代理计量；阶梯 {} token / 每轮 +{}）",
                        scheduler::render(&d),
                        st.ladder_tokens,
                        st.growth_tokens
                    ),
                );
                if d.rebase {
                    let (folded, saved) = fold_history(&mut msgs, &round_slots, keep);
                    sink.log(
                        "info",
                        format!(
                            "[agent] 第 {step} 轮 历史折叠 {folded} 轮（省 {saved} 字节；下限仍保 {keep} 轮正文）"
                        ),
                    );
                    rounds_since_rebase = 0;
                    base_bytes = msgs_bytes(&msgs);
                } else {
                    rounds_since_rebase = rounds_since_rebase.saturating_add(1);
                }
            } else {
                // 关掉时：老触发条件一字不变（`fold_history` 幂等，每轮都调）
                let (folded, saved) = fold_history(&mut msgs, &round_slots, keep);
                if folded > 0 {
                    sink.log(
                        "info",
                        format!(
                            "[agent] 第 {step} 轮 历史折叠 {folded} 轮（省 {saved} 字节；保留最近 {keep} 轮正文）"
                        ),
                    );
                }
            }
            // 每轮增长要**从第二轮起**才算：第一轮 `ladder_bytes` 还是 0，
            // 拿它作基线量出来的"增长"等于整条阶梯（实测踩过：2565 token/轮 的假增长
            // 会让 DP 直接判"该压"）。
            if ladder_bytes > 0 {
                growth_bytes = now_bytes.saturating_sub(ladder_bytes);
            }
            ladder_bytes = now_bytes;
        }

        // 模型已经对失败做出过回应（无论它选了哪个动作），把控制权交回引擎继续派发。
        // 解析失败 / 门禁打回那两条 `continue` 不清它 —— 那种场合模型还没给出有效决定。
        intervene = false;

        if step == MAX_STEPS {
            out.answer = format!(
                "（达到 {MAX_STEPS} 轮上限，循环停止。已完成 {} 次工具调用、{} 个文件变更；请基于以上进展继续指示。）",
                out.steps.len(),
                out.changes.len()
            );
            sink.log("warn", out.answer.clone());
        }
    }

    // 收尾：**引擎持有就引擎收**。
    // 不这么做，模型用 start / Start-Process 绕出来的进程就会变成杀不到的孤儿
    // （实测：一个占着 8083 的 java 活了 28 分钟，还让后面每一次重启都拿到假的失败信号）。
    // keep_alive 是唯一例外 —— 显式声明的才留，且仍在进程表里（宿主面板可见可停）。
    let (stopped_procs, kept_procs) = crate::proc::shutdown_for(proj, true);
    if !stopped_procs.is_empty() || !kept_procs.is_empty() {
        let mut note = Vec::new();
        if !stopped_procs.is_empty() {
            note.push(format!(
                "> ⚠️ 本次 run 结束时收掉了 {} 个托管进程（未声明 keep_alive）：{}\n\
                 > 如果本意是让服务持续运行，请让我带上 \"keep_alive\":true 重跑一次 —— 那样的服务会留在【服务】面板里，可见可停。",
                stopped_procs.len(),
                crate::proc::render_listing(&stopped_procs)
            ));
        }
        if !kept_procs.is_empty() {
            note.push(format!(
                "按 keep_alive 留着 {} 个（IDE 退出时一并收）：{}",
                kept_procs.len(),
                crate::proc::render_listing(&kept_procs)
            ));
        }
        sink.log("warn", clip(&note.join(" / "), 300));
        gate.notes.push(note.join("\n"));
    }

    // 计划落定：run 结束了，大纲区不该再留沙漏（⌛ 是"等待执行"，不是"没做成"）
    if !plan_steps.is_empty() {
        settle_steps(
            sink,
            &plan_steps,
            &ctx.overlay,
            proj,
            delivered,
            &step_states,
        );
    }

    // 门禁留下的补充说明（跳过原因 / 预算用尽 / 复核未完成）：
    // 写在答复末尾 —— 用户不该去翻日志才知道"这次其实没验成"
    if !gate.notes.is_empty() {
        out.answer = format!(
            "{}\n\n---\n{}",
            out.answer.trim_end(),
            gate.notes.join("\n")
        );
    }

    if !ctx.changes.is_empty() && policy == WritePolicy::Stage {
        let dir = ctx.flush_stage()?;
        out.stage_dir = Some(dir.to_string_lossy().to_string());
    }
    out.backup_dir = ctx
        .backup_dir
        .as_ref()
        .map(|d| d.to_string_lossy().to_string());
    out.changes = ctx.changes.clone();
    // 上下文计量小结（`agent.ctx.metrics`）：整段 run 的**加权**复用率 ——
    // 单轮的占比会被"这一轮恰好是新内容"带偏，账单要看的是全程。
    // 两个数一起给：字节代理（发前就能算）+ 厂商回报的命中 token（**真值**，首轮必然是 0 命中）。
    if cfg.agent.ctx.metrics && tally.rounds > 0 {
        let vendor = if cache_rounds == 0 {
            " · 厂商回报命中：无（这家/这一路没给这个数）".to_string()
        } else {
            format!(
                " · 厂商回报命中 {} / {} tokens = {:.1}%（{} 轮；输入含命中部分）",
                cache_hit_sum,
                cache_in_sum,
                cache_hit_sum as f64 * 100.0 / cache_in_sum.max(1) as f64,
                cache_rounds
            )
        };
        // G2（有界上下文）：prompt 峰值 vs 预算 B —— 判据是"不退化"，所以要**每次都报**，
        // 哪怕没越界（不报就没人知道它到底有没有挨到边）。
        let budget = cfg.agent.ctx.budget_tokens_effective();
        // **机制状态**（用户质疑过"我怎么看不出到底用没用它"）：关着也照样报 ——
        // 一行说清这次 run 用了哪几件。不报的话，"没生效"与"没启用"在界面上长得一模一样。
        let onoff = |v: bool| if v { "on" } else { "off" };
        let mech = format!(
            " · 机制 layout={} dedup={} schedule={} capsule={}",
            onoff(cfg.agent.ctx.layout),
            onoff(cfg.agent.ctx.dedup),
            onoff(cfg.agent.ctx.schedule),
            onoff(cfg.agent.ctx.capsule)
        );
        let g2 = format!(
            " · prompt 峰值 {g2_max_prompt} token（预算 B = {budget}{}） · 被 max_tokens 截断 {g2_truncated} 次",
            if g2_max_prompt > budget {
                "，**已越界**"
            } else {
                "，未越界"
            }
        );
        sink.log(
            "info",
            clip(
                &format!(
                    "[agent] 上下文计量小结：加权公共前缀 {} · 共 {} 轮（首轮按 0 前缀计 —— 它本来就没有可复用的东西）{vendor}{g2}{mech}",
                    tally.render(),
                    tally.rounds
                ),
                400,
            ),
        );
    }
    // 去重账本小结（`dedup` 或 `metrics` 开着）：**P2 的验收读数**
    // —— "重复工具调用次数下降 ≥50%" 里的那个数就是 `拦截重复` 与 `仍重复执行`。
    if cfg.agent.ctx.dedup || cfg.agent.ctx.metrics {
        sink.log(
            "info",
            clip(&format!("[agent] {}", ctx.progress().dedup_summary()), 300),
        );
    }
    sink.stage(
        "agent",
        "done",
        format!(
            "工具调用 {} 次 · {} 个文件变更 · 验证 {} 次 · 复核 {} 次",
            out.steps.len(),
            out.changes.len(),
            out.verifications.len(),
            out.reflections.len()
        ),
    );
    // 进展记忆（v1.1）：把**完整账本**交给调用方（含被取代的）—— 宿主据此写进会话存档，
    // 否则"模型确认过什么"跑完即焚（§8-5 当年拍了「是」却一直没落）。
    out.findings = ctx.progress().all().to_vec();
    out.elapsed_ms = started.elapsed().as_millis();
    Ok(out)
}
