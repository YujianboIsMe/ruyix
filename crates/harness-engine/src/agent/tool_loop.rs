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
#[allow(clippy::too_many_arguments)]
pub async fn run_with_ask(
    cfg: &AppConfig,
    proj: &Path,
    task: &str,
    history: &[HistoryMsg],
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
    let mut ctx =
        Ctx::new(proj, policy).with_state_root(crate::config::project_state_root(cfg, proj));
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
    // 连续模型调用失败计数：成功一轮即清零
    let mut llm_failures: u32 = 0;

    let mut msgs = vec![ChatMessage::system(agent_system_prompt())];
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
    msgs.push(ChatMessage::user(format!("{head}\n\n用户消息：\n{task}")));

    sink.stage(
        "agent",
        "start",
        format!("工具循环（最多 {MAX_STEPS} 轮，写入策略：{policy:?}）"),
    );

    for step in 1..=MAX_STEPS {
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

        let reply = match llm::chat(
            &cfg.llm,
            cfg.llm_fallback.as_ref(),
            &msgs,
            true,
            cfg.llm.tool_protocol,
        )
        .await
        {
            Ok(r) => {
                llm_failures = 0;
                r
            }
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
                tokio::time::sleep(Duration::from_millis(1200 * llm_failures as u64)).await;
                continue;
            }
        };
        out.usage.add(&reply.usage);
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
        unparsed_streak = 0;
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
        msgs.push(ChatMessage::assistant(if via_tools {
            tool_calls_echo(&reply.tool_calls)
        } else {
            reply.content.clone()
        }));

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
        let mut slots: Vec<Option<CallResult>> = (0..n).map(|_| None).collect();
        for wave in &waves {
            if wave.len() == 1 {
                let i = wave[0];
                let one = match &actions[i] {
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
                    _ => exec_one(cfg, proj, policy, conn, &mut ctx, actions[i].clone()).await,
                };
                slots[i] = Some(one);
            } else {
                run_wave(
                    cfg, proj, policy, conn, &mut ctx, &actions, wave, &mut slots,
                )
                .await;
            }
        }
        let results: Vec<CallResult> = slots
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
            sink.log(
                level,
                clip(&format!("[agent] {head} {tool} {icon} {brief}"), 400),
            );
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
                "{{\"ok\":{all_ok},\"folded\":\"第 {step} 轮结果（{outcome}）—— 正文已从上下文移除，需要时重新 read\"}}"
            ),
        });
        if cfg.agent.history_trim {
            let (folded, saved) =
                fold_history(&mut msgs, &round_slots, cfg.agent.history_keep_rounds);
            if folded > 0 {
                sink.log(
                    "info",
                    format!(
                        "[agent] 第 {step} 轮 历史折叠 {folded} 轮（省 {saved} 字节；保留最近 {} 轮正文）",
                        cfg.agent.history_keep_rounds
                    ),
                );
            }
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
    out.elapsed_ms = started.elapsed().as_millis();
    Ok(out)
}
