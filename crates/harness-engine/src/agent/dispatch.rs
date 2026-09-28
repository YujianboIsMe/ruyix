//! 波次调度与批量执行：**动作已经有了，怎么并发地把它跑掉**。
//!
//! 一趟模型回复可以带多个动作（`parse_actions` 的批量形态）。这里决定它们怎么落地：
//!
//! - **形状**（[`Shape`]）：一个动作"碰"了哪些资源（文件 / 命令 / 目录），据此判 [`conflicts`]；
//! - **波次**（[`waves_by`] / [`batch_waves`]）：同波之间无冲突 ⇒ 并发；有冲突 ⇒ 排到下一波。
//!   串行化是**保守**的选择：模型写同一个文件的两次动作之间一定有因果，并发跑就是掷骰子；
//! - **执行**（[`exec_one`] / [`run_wave`]）：一波一个 `JoinAll`（`futures` 风格的并发合并），
//!   结果按原顺序拼成一条 [`batch_json_result`] 观察 —— 顺序不能乱，模型靠下标对齐它自己的动作；
//! - **写回落地**（[`resolve_write`] / [`flush_write_disk`]）：Stage 模式只进覆盖层 + 暂存区，
//!   Apply 模式落盘（被覆盖的先备份）。
//!
//! 从 `agent/action.rs` 拆出的（2026-09-28，那一半当时 1840 行）：它被夹在解析层的两段之间
//! —— 上面是"模型输出 → 动作"，下面是"参数形状校验与纠偏文案"，中间这一层只关心**调度与执行**。

use super::*;

/// 冲突判定只看三件事：**路径 / 是不是写 / 是不是托管进程操作**。
/// 父子两种动作（`Action` / `StepAction`）都映射到它 —— 判定内核只有一份。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Shape<'a> {
    pub(crate) path: Option<&'a str>,
    pub(crate) write: bool,
    pub(crate) proc: bool,
}

pub(crate) fn shape_of(a: &Action) -> Shape<'_> {
    match a {
        Action::Read(s) => Shape {
            path: Some(&s.path),
            write: false,
            proc: false,
        },
        // 冲突只看路径与"是不是写"，不看传的是 content 还是 edits：
        // 同一个文件的两种写法仍然互斥。
        Action::Write(s) => Shape {
            path: Some(&s.path),
            write: true,
            proc: false,
        },
        Action::ExecBg(_) | Action::Proc(..) => Shape {
            path: None,
            write: false,
            proc: true,
        },
        // 进展记忆：没有路径、不写磁盘、不动进程 ⇒ **与谁都不冲突**（放哪一波都合法）。
        // 真正的执行在主波之后同步做：它写的是共享的 `progress`，不能进并发波。
        Action::Findings(_) => Shape {
            path: None,
            write: false,
            proc: false,
        },
        Action::Execute(..)
        | Action::Connect(_)
        | Action::Plan(_)
        | Action::Final(_)
        | Action::Ask(_) => Shape {
            path: None,
            write: false,
            proc: false,
        },
    }
}

pub(crate) fn shape_of_step(a: &StepAction) -> Shape<'_> {
    match a {
        StepAction::Read(s) => Shape {
            path: Some(&s.path),
            write: false,
            proc: false,
        },
        StepAction::Write(s) => Shape {
            path: Some(&s.path),
            write: true,
            proc: false,
        },
        StepAction::ExecBg(_) | StepAction::Proc(..) => Shape {
            path: None,
            write: false,
            proc: true,
        },
        StepAction::Execute(..) | StepAction::Final(_) | StepAction::Unsupported(_) => Shape {
            path: None,
            write: false,
            proc: false,
        },
    }
}

/// 两条调用是否**必须保序**（结构性判定 —— 只看原语与路径，**不猜命令语义**）。
///
/// 只有两种冲突：
/// 1. **同一条路径上的写**：`write` 与同路径的 `read`/`write` 必须保序 —— 覆盖层是"写完立刻
///    读回来"的依据，同文件两次写的先后也是模型表达的意思；
/// 2. **托管进程的生命周期操作**（`background` 起 / `status`·`log`·`stop`）：那张进程表是引擎
///    自己持有的共享状态，句柄又由引擎赋值 —— 同批里"起完再查"必须保序。
///
/// 其余**一律并发**：`execute` 之间共享什么（构建缓存、锁、端口）引擎不知道，凭命令文本猜就是
/// app 知识泄漏 —— 这属于**模型的依赖声明**（提示词已写明"同一批并发跑，有依赖就分两轮发"）。
pub(crate) fn conflicts(a: &Shape<'_>, b: &Shape<'_>) -> bool {
    if a.proc && b.proc {
        return true;
    }
    match (a.path, b.path) {
        (Some(x), Some(y)) => x == y && (a.write || b.write),
        _ => false,
    }
}

/// 一批动作 → **执行波次**：波内互不冲突（并发跑），波与波之间按声明顺序。
///
/// 贪心分层：每条落在"它所有冲突前驱的下一波"。于是 `[read a, read b, write a, read a]`
/// → `[[0,1],[2],[3]]`：两次读并发；写 a 等读 a；最后一个读 a 等写 a（于是读到新内容）。
/// 最常见的批（互不相关的一串读 / 一串命令）**只有一波** —— 那就是"一起并发"。
pub(crate) fn waves_by(shapes: &[Shape<'_>]) -> Vec<Vec<usize>> {
    let mut level: Vec<usize> = Vec::with_capacity(shapes.len());
    for (i, a) in shapes.iter().enumerate() {
        let mut lv = 0;
        for (j, b) in shapes.iter().enumerate().take(i) {
            if conflicts(a, b) {
                lv = lv.max(level[j] + 1);
            }
        }
        level.push(lv);
    }
    let n = level.iter().copied().max().map_or(0, |m| m + 1);
    let mut waves = vec![Vec::new(); n];
    for (i, lv) in level.into_iter().enumerate() {
        waves[lv].push(i);
    }
    waves
}

pub(crate) fn batch_waves(actions: &[Action]) -> Vec<Vec<usize>> {
    let shapes: Vec<Shape<'_>> = actions.iter().map(shape_of).collect();
    waves_by(&shapes)
}

/// 步骤子 agent 的波次（**同一个内核**，父子不分叉）
pub(crate) fn batch_waves_for_step(actions: &[StepAction]) -> Vec<Vec<usize>> {
    let shapes: Vec<Shape<'_>> = actions.iter().map(shape_of_step).collect();
    waves_by(&shapes)
}

/// 并发跑一组只读调用，结果按**声明顺序**落回各自的槽位。
///
/// 顺序错位比慢更糟：模型会拿 B 文件的内容当 A 的依据，而且它没有任何办法察觉。
///
/// 用 `std::thread::scope` 而不是 tokio 任务：`Ctx::tool_read` 是同步文件 I/O，起任务也只是
/// 在同一执行线程上排队；scoped 线程能直接借 `&Ctx`（不必 `'static`、不必克隆覆盖层），
/// 也不要求 runtime 是多线程 —— 单测跑在 `new_current_thread` 上，那里 `block_in_place` 会直接 panic。
pub(crate) fn read_group(
    ctx: &Ctx<'_>,
    specs: &[ReadSpec],
    parallel: bool,
) -> Vec<Result<String, String>> {
    if specs.len() == 1 || !parallel {
        return specs.iter().map(|s| ctx.tool_read(s)).collect();
    }
    let mut slots: Vec<Option<Result<String, String>>> = (0..specs.len()).map(|_| None).collect();
    std::thread::scope(|s| {
        let handles: Vec<_> = slots
            .iter_mut()
            .zip(specs.iter())
            .map(|(slot, spec)| s.spawn(move || *slot = Some(ctx.tool_read(spec))))
            .collect();
        // 全部 join 掉：线程 panic 时它的槽位留 None（下面统一报"线程异常"），
        // 不 join 的话 `scope` 结束时会自己再抛一次 "scoped thread panicked"
        for h in handles {
            let _ = h.join();
        }
    });
    slots
        .into_iter()
        .map(|s| s.unwrap_or_else(|| Err("并发读取未返回（读取线程异常）".into())))
        .collect()
}

pub(crate) fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// 单动作结果回灌：`{"ok":…, "result"|"error":…}`（历史里模型学过的形状，保持不动）
pub(crate) fn json_result(r: Result<String, String>) -> String {
    match r {
        Ok(v) => format!("{{\"ok\": true, \"result\": {}}}", json_str(&v)),
        Err(e) => format!("{{\"ok\": false, \"error\": {}}}", json_str(&e)),
    }
}

/// 把 write 的两种参数形状都归约成"最终整份内容"。**能力没变，变的只是要传多少东西**：
///
/// - [`WriteBody::Content`] —— 原样透传（老路径语义逐字不变；空内容仍然拒）
/// - [`WriteBody::Edits`] —— 拿**当前**内容逐条锚点替换。匹配规则不在这里重写：
///   [`repair::replace_unique`] 负责"恰好出现一次"与行尾归一化（CRLF 文件能匹配 LF 的 `find`，
///   落盘再还原 CRLF —— 真机踩过 `core.autocrlf` 让 LF 片段一条都匹配不上）
///
/// `cur` = 当前内容（覆盖层优先，`None` = 文件不存在）。锚点编辑只能改**已有**文件：
/// 新建文件的锚点无处可锚，那种场合用 `content` 形态。
pub(crate) fn resolve_write(
    rel: &str,
    body: &WriteBody,
    cur: Option<&str>,
) -> Result<String, String> {
    match body {
        WriteBody::Content(c) => {
            if c.is_empty() {
                return Err("content 为空 —— 不允许静默清空文件".into());
            }
            Ok(c.clone())
        }
        WriteBody::Edits(edits) => {
            let Some(text) = cur else {
                return Err(format!(
                    "{rel} 不存在 —— 锚点编辑只能改已有文件；新建文件请用 content 形态（整份内容）"
                ));
            };
            let mut out = text.to_string();
            for (i, e) in edits.iter().enumerate() {
                if e.find.is_empty() {
                    return Err(format!(
                        "第 {} 条 edit：find 为空（{rel}）—— 锚点编辑要给出要被替换的原文；\
                         要给整份内容请用 content 形态",
                        i + 1
                    ));
                }
                // 同一文件的多条改动**按声明顺序累积**（与 repair::apply_edits 同一语义）
                out = repair::replace_unique(&out, &e.find, &e.replace)
                    .map_err(|why| format!("第 {} 条 edit：{why}（{rel}）", i + 1))?;
            }
            Ok(out)
        }
    }
}

/// 锚点写入成功的回灌文案。必须把"改了几处"说出来：只说"已写入 N 字节"，
/// 模型看不出自己那轮传的是 edits 还是 content，下一轮容易再 verify 一遍。
pub(crate) fn write_edits_ok_text(
    rel: &str,
    count: usize,
    len: usize,
    policy: WritePolicy,
) -> String {
    format!(
        "已改 {rel}：{count} 处锚点替换（改后 {len} 字节）{}",
        if policy == WritePolicy::Stage {
            "（已暂存到 IDE 暂存区，项目磁盘未变，用户确认后才生效）"
        } else {
            ""
        }
    )
}

/// 文件内容"怎么给"：整份走老裁剪，窗口走行切片。
pub(crate) fn read_body(rel: &str, content: &str, spec: &ReadSpec) -> Result<String, String> {
    if !spec.is_window() {
        return Ok(clip(content, READ_CLIP));
    }
    window_of(rel, content, spec.offset, spec.limit)
}

/// 行窗口读：从 `offset`（1-based）起最多 `limit` 行。
///
/// 表头不是装饰，它是这个能力的**一半**：模型要靠"共 N 行 / 还有 M 行，接着读用 offset=X"
/// 才知道缺口在哪、怎么接上。没有它，模型只知道自己拿到了 200 行。
pub(crate) fn window_of(
    rel: &str,
    content: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<String, String> {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    if total == 0 {
        return Ok(format!("（{rel} 是空文件）"));
    }
    let start = offset.unwrap_or(1);
    if start > total {
        // 越界当场说清：静默返回最后一行会让模型以为"中间没内容"
        return Err(format!(
            "offset={start} 越界：{rel} 只有 {total} 行（行号从 1 起）"
        ));
    }
    let asked = limit.unwrap_or(READ_WINDOW_DEFAULT_LINES);
    let take = asked.min(READ_WINDOW_MAX_LINES);
    let end = (start - 1 + take).min(total);
    let body = lines[start - 1..end].join("\n");
    let tail = if end < total {
        format!("；还有 {} 行，接着读用 offset={}", total - end, end + 1)
    } else {
        "；已到文件末尾".to_string()
    };
    let clamped = if asked > READ_WINDOW_MAX_LINES {
        format!("（请求 {asked} 行，一次窗口上限 {READ_WINDOW_MAX_LINES} 行）")
    } else {
        String::new()
    };
    Ok(format!(
        "--- {rel}（第 {start}-{end} 行 / 共 {total} 行{tail}）{clamped}---\n{}",
        clip(&body, READ_CLIP)
    ))
}

/// 写入成功的回灌文案。**单动作与批并发两条路径共用**：确认模式必须把"磁盘没变"说清，
/// 只说"已暂存"模型仍会去 execute 里找它的改动。
pub(crate) fn write_ok_text(rel: &str, len: usize, policy: WritePolicy) -> String {
    format!(
        "已写入 {rel}（{len} 字节）{}",
        if policy == WritePolicy::Stage {
            "（已暂存到 IDE 暂存区，项目磁盘未变，用户确认后才生效）"
        } else {
            ""
        }
    )
}

/// 一次写入的**磁盘阶段**：先备份被覆盖的原文件，再写目标。
///
/// 纯磁盘操作、只吃 `&Path`，所以能在并发波里跑；"写入怎么落盘"全仓只允许这一个实现
/// （单动作路径走 [`Ctx::flush_one`]，它委托到这里）。
pub(crate) fn flush_write_disk(
    proj: &Path,
    backup_dir: Option<&Path>,
    rel: &str,
    before: Option<&str>,
    after: &str,
) -> Result<(), String> {
    if let (Some(dir), Some(before)) = (backup_dir, before) {
        let to = dir.join(rel);
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&to, before).map_err(|e| format!("备份 {rel} 失败：{e}（已中止写入）"))?;
    }
    let target = proj.join(rel);
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&target, after).map_err(|e| format!("写入 {rel} 失败：{e}"))
}

/// 最简 `join_all`：在**同一个任务**里并发推进一组 future（引擎不为此引 `futures` 依赖）。
///
/// 为什么不是 `tokio::spawn`：这些 future 借的是 `&dyn Connector`，非 `'static`，spawn 装不下；
/// 也不是 `tokio::join!`：它元数编译期固定，装不下运行时才知道长度的列表。
pub(crate) struct JoinAll<'a, T> {
    pub(crate) futs: Vec<ConnectFuture<'a, T>>,
    pub(crate) out: Vec<Option<Result<T, String>>>,
}

impl<'a, T> JoinAll<'a, T> {
    pub(crate) fn new(futs: Vec<ConnectFuture<'a, T>>) -> Self {
        let out = futs.iter().map(|_| None).collect();
        Self { futs, out }
    }
}

impl<T: Unpin> Future for JoinAll<'_, T> {
    type Output = Vec<Result<T, String>>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // `Pin<Box<dyn Future>>` 与 `Option<..>` 都是 `Unpin`，所以这里能安全拿 `&mut`
        let this = self.get_mut();
        let mut pending = 0;
        for (i, f) in this.futs.iter_mut().enumerate() {
            if this.out[i].is_some() {
                continue;
            }
            match f.as_mut().poll(cx) {
                Poll::Ready(r) => this.out[i] = Some(r),
                Poll::Pending => pending += 1,
            }
        }
        if pending == 0 {
            Poll::Ready(
                this.out
                    .iter_mut()
                    .map(|o| o.take().unwrap_or_else(|| Err("join 状态丢失".into())))
                    .collect(),
            )
        } else {
            Poll::Pending
        }
    }
}

/// 一批结果回灌：`results` 数组**按声明顺序**，逐条带调用摘要与自己的 ok。
///
/// 顶层 `ok` 只是"全成"的汇总 —— 模型拿它当"这一轮有没有事"会漏掉其中一条失败，
/// 所以每条都带 `ok`，并在 note 里点明顺序与执行方式（并发/串行）的关系。
pub(crate) fn batch_json_result(items: &[CallResult]) -> String {
    let all_ok = items.iter().all(|(_, _, r)| r.is_ok());
    let results: Vec<serde_json::Value> = items
        .iter()
        .enumerate()
        .map(|(i, (tool, brief, r))| {
            let mut o = serde_json::json!({
                "index": i + 1,
                "tool": tool,
                "call": brief,
                "ok": r.is_ok(),
            });
            match r {
                Ok(v) => o["result"] = serde_json::Value::String(v.clone()),
                Err(e) => o["error"] = serde_json::Value::String(e.clone()),
            }
            o
        })
        .collect();
    serde_json::json!({
        "ok": all_ok,
        "results": results,
        "note": "以上是同一轮里的多个调用（一批并发跑；只有同一条路径上的写与读、以及托管进程的起停查会按你给的顺序排），results 按你声明的顺序排列。",
    })
    .to_string()
}

/// 去重命中时直接给出的那条结果（**不执行**）。`None` = 没命中 ⇒ 调用方照常执行。
///
/// 措辞与形状都只有这一处：单动作路径与批路径共用它，免得两处各写一遍
/// "原文 + 标注行" 而在某一次改动里分叉（模型看到的标注就那两种，分叉 = 一处漏标）。
pub(crate) fn reused_slot(plans: &[Option<ledger::Plan>], i: usize) -> Option<CallResult> {
    let p = plans.get(i)?.as_ref()?;
    let r = p.reuse.as_ref()?;
    Some((
        p.tool.to_string(),
        p.brief.clone(),
        Ok(format!(
            "{}{}",
            r.text,
            ledger::LedgerCall::reuse_note(r.step)
        )),
    ))
}

/// 一个动作 → `(tool, brief, result)`。批与单动作走**同一份**实现：
/// 两条派发路径必然分叉（老代码里解析与执行就分在两处 `match`），这里只留一条。
pub(crate) async fn exec_one(
    cfg: &AppConfig,
    proj: &Path,
    policy: WritePolicy,
    conn: &dyn Connector,
    ctx: &mut Ctx<'_>,
    action: Action,
) -> (String, String, Result<String, String>) {
    match action {
        Action::Findings(items) => {
            let brief = format!("record_findings {} 条", items.len());
            let mut ids: Vec<String> = Vec::new();
            let mut errs: Vec<String> = Vec::new();
            for it in &items {
                match ctx.record_finding(it) {
                    Ok(id) => ids.push(id),
                    Err(e) => errs.push(e),
                }
            }
            let mut text = if ids.is_empty() {
                String::new()
            } else {
                format!(
                    "已记录：{}。\n{}",
                    ids.join("、"),
                    ctx.progress().byte_account()
                )
            };
            if !errs.is_empty() {
                // 部分失败也必须说清是哪条：只回一句"出错了"会让模型以为全记上了。
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("被拒 {} 条：{}", errs.len(), errs.join("；")));
                return ("findings".into(), brief, Err(text));
            }
            text.push_str(
                "\n（下一轮你仍会看到这些条目；**别**为了回忆它们去重读文件。要修正就用 supersedes 指向它的 id。）",
            );
            ("findings".into(), brief, Ok(text))
        }
        Action::Read(spec) => {
            let brief = format!("read {}", spec.brief());
            ("read".into(), brief, ctx.tool_read(&spec))
        }
        Action::Write(spec) => {
            let brief = spec.brief();
            let r = ctx.apply_write(&spec);
            ("write".into(), brief, r)
        }
        Action::Execute(cmd, t) => {
            let brief = format!("execute {}", clip(&cmd, 80));
            let mut r = tool_execute(proj, &cmd, t);
            // 确认模式 + 已有暂存改动：这条命令必然看不到本次修改，贴一行说明兜住
            r.push_str(staged_execute_note(policy, !ctx.changes.is_empty()));
            ("execute".into(), brief, Ok(r))
        }
        Action::ExecBg(spec) => {
            let brief = format!("execute bg {}", clip(&spec.cmd, 70));
            ("execute".into(), brief, tool_exec_bg(proj, cfg, &spec))
        }
        Action::Proc(op, handle) => {
            let brief = format!("execute {} {handle}", proc_op_name(op));
            ("execute".into(), brief, tool_proc(proj, op, &handle))
        }
        Action::Connect(ca) => connect_step(conn, ca).await,
        // 控制动作进不了批（parse_actions 已当面拒）；这条分支只为让 match 穷尽
        Action::Plan(_) | Action::Final(_) | Action::Ask(_) => (
            "control".into(),
            "控制动作".into(),
            Err("plan / final / ask_user 不能与调用同批执行".into()),
        ),
    }
}

/// 一波（波内互不冲突）的**并发执行**，结果按索引写回 `slots`。
///
/// 落地方式按原语分：
/// - 只读：`read_group`（scoped 线程借 `&Ctx`，同步文件 I/O，不占 runtime）
/// - 写入：**磁盘部分**在 scoped 线程里并行（冲突规则保证同波不同路径），记账回主线程按声明
///   顺序补 —— `Ctx` 的覆盖层与变更表只有一份内存状态，不能并发改
/// - 执行 / 托管进程：每个线程自己起进程（`tool_execute` / `tool_exec_bg` / `tool_proc` 都只吃
///   `&Path` 与 `&AppConfig`，与 `Ctx` 无关）
/// - 连接：同一任务里 [`JoinAll`]（future 借 `&dyn Connector`，非 `'static`，spawn 装不下）
///
/// 波内没有依赖，所以"谁先谁后"无所谓：只读先跑完再起写/执行，纯粹是为了复用同一份并发读实现。
/// 参数多：这是"把一批调用按波并发跑"的执行口，它本来就要同时知道配置、项目、策略、连接、
/// 覆盖层与结果槽位（与本模块其它执行函数同款，见 `#[allow(too_many_arguments)]` 的既有用法）。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_wave(
    cfg: &AppConfig,
    proj: &Path,
    policy: WritePolicy,
    conn: &dyn Connector,
    ctx: &mut Ctx<'_>,
    actions: &[Action],
    plans: &[Option<ledger::Plan>],
    wave: &[usize],
    slots: &mut [Option<CallResult>],
) {
    // 一行回退：波内也不并发（仍是一批一次往返，只是按声明顺序串行）
    if !cfg.agent.batch_parallel {
        for &i in wave {
            if let Some(hit) = reused_slot(plans, i) {
                slots[i] = Some(hit); // 去重命中：不执行
                continue;
            }
            slots[i] = Some(exec_one(cfg, proj, policy, conn, &mut *ctx, actions[i].clone()).await);
        }
        return;
    }

    let mut reads: Vec<(usize, ReadSpec)> = Vec::new();
    let mut writes: Vec<(usize, String, Option<String>, String)> = Vec::new();
    let mut execs: Vec<(usize, String, Option<u64>)> = Vec::new();
    let mut bgs: Vec<(usize, crate::proc::StartSpec)> = Vec::new();
    let mut procs: Vec<(usize, ProcOp, String)> = Vec::new();
    let mut conn_briefs: Vec<(usize, String)> = Vec::new();
    let mut conn_futs: Vec<ConnectFuture<'_, String>> = Vec::new();
    // 进展记忆：先收集、波后同步落（见下面 `findings` 那段的说明）
    let mut finds: Vec<(usize, Vec<FindingSpec>)> = Vec::new();

    for &i in wave {
        // 去重命中：**不进波、不执行** —— 结果在预检里就算好了（`plans` 由 tool_loop 传入）
        if let Some(hit) = reused_slot(plans, i) {
            slots[i] = Some(hit);
            continue;
        }
        match &actions[i] {
            Action::Read(spec) => reads.push((i, spec.clone())),
            Action::Write(spec) => match safe_rel_path(&spec.path) {
                // `before` 与"归约成整份内容"都在主线程做（要碰覆盖层：edits 得先读到当前内容）
                // —— 线程里只做磁盘
                Ok(rel) => {
                    let before = ctx.before_of(&rel);
                    match resolve_write(&rel, &spec.body, before.as_deref()) {
                        Ok(after) => writes.push((i, rel, before, after)),
                        Err(e) => slots[i] = Some(("write".into(), spec.brief(), Err(e))),
                    }
                }
                Err(e) => slots[i] = Some(("write".into(), spec.brief(), Err(e))),
            },
            Action::Execute(cmd, t) => execs.push((i, cmd.clone(), *t)),
            Action::ExecBg(spec) => bgs.push((i, spec.clone())),
            Action::Proc(op, h) => procs.push((i, *op, h.clone())),
            Action::Connect(ca) => {
                conn_briefs.push((i, connect_brief(ca)));
                conn_futs.push(connect_future(conn, ca.clone()));
            }
            // 进展记忆：**不进并发波**（它写的是共享 `progress`），先收进列表，主波跑完再同步落。
            Action::Findings(v) => finds.push((i, v.clone())),
            // 控制动作进不了多人波（parse_actions 已拒批里的 plan / final）
            Action::Plan(_) | Action::Final(_) | Action::Ask(_) => {
                slots[i] = Some((
                    "control".into(),
                    "控制动作".into(),
                    Err("plan / final / ask_user 不能与调用同波执行".into()),
                ));
            }
        }
    }

    // 只读：复用同一份并发读（顺序按声明落位）
    if !reads.is_empty() {
        let specs: Vec<ReadSpec> = reads.iter().map(|(_, s)| s.clone()).collect();
        for ((i, spec), r) in reads.iter().zip(read_group(ctx, &specs, true)) {
            slots[*i] = Some(("read".to_string(), format!("read {}", spec.brief()), r));
        }
    }

    // Apply 模式要覆盖已有文件 → 备份目录在主线程备好（线程里不改 `Ctx`）
    let bdir = if policy == WritePolicy::Apply && writes.iter().any(|(_, _, b, _)| b.is_some()) {
        ctx.ensure_backup_dir(true)
    } else {
        ctx.backup_dir.clone()
    };
    // 贴"暂存改动"横幅用的判据（线程里读 `Ctx` 不方便，提前取）
    let text_changes = !ctx.changes.is_empty();

    let mut write_out: Vec<(usize, Result<(), String>)> = Vec::new();
    let mut exec_out: Vec<(usize, Result<String, String>)> = Vec::new();
    let mut bg_out: Vec<(usize, Result<String, String>)> = Vec::new();
    let mut proc_out: Vec<(usize, Result<String, String>)> = Vec::new();
    std::thread::scope(|s| {
        let hw: Vec<_> = writes
            .iter()
            .map(|(i, rel, before, after)| {
                let (i, rel, before, after) = (*i, rel.clone(), before.clone(), after.clone());
                let bdir = bdir.clone();
                (
                    i,
                    s.spawn(move || {
                        flush_write_disk(proj, bdir.as_deref(), &rel, before.as_deref(), &after)
                    }),
                )
            })
            .collect();
        let he: Vec<_> = execs
            .iter()
            .map(|(i, cmd, t)| {
                let (i, cmd, t) = (*i, cmd.clone(), *t);
                (i, s.spawn(move || tool_execute(proj, &cmd, t)))
            })
            .collect();
        let hb: Vec<_> = bgs
            .iter()
            .map(|(i, spec)| {
                let (i, spec) = (*i, spec.clone());
                (i, s.spawn(move || tool_exec_bg(proj, cfg, &spec)))
            })
            .collect();
        let hp: Vec<_> = procs
            .iter()
            .map(|(i, op, handle)| {
                let (i, op, handle) = (*i, *op, handle.clone());
                (i, s.spawn(move || tool_proc(proj, op, &handle)))
            })
            .collect();
        for (i, h) in hw {
            write_out.push((i, h.join().unwrap_or_else(|_| Err("写入线程异常".into()))));
        }
        for (i, h) in he {
            // `tool_execute` 的失败信息是**嵌在文本里**的（不是 Err）：模型需要读到命令的
            // 实际输出才能改；只有"线程都没回来"才算这一条没结果
            exec_out.push((
                i,
                Ok(h.join().unwrap_or_else(|_| "执行线程异常（未返回）".into())),
            ));
        }
        for (i, h) in hb {
            bg_out.push((i, h.join().unwrap_or_else(|_| Err("启动线程异常".into()))));
        }
        for (i, h) in hp {
            proc_out.push((i, h.join().unwrap_or_else(|_| Err("句柄线程异常".into()))));
        }
    });

    // 写入：磁盘已落 → 这里按**声明顺序**记账（内存状态只有一份）
    for ((i, rel, before, after), (_, r)) in writes.iter().zip(write_out) {
        // 摘要用**调用方声明的形状**（content 报字节、edits 报几处替换）——
        // 只说"写成了 N 字节"会让模型看不出自己那轮传的是哪种形态
        let brief = match &actions[*i] {
            Action::Write(spec) => spec.brief(),
            _ => format!("write {rel}（{} 字节）", after.len()),
        };
        let res = match r {
            Ok(()) => {
                ctx.record(rel.clone(), before.clone(), after.clone());
                Ok(write_ok_text(rel, after.len(), policy))
            }
            Err(e) => Err(e),
        };
        slots[*i] = Some(("write".into(), brief, res));
    }
    for ((i, cmd, _), (_, r)) in execs.iter().zip(exec_out) {
        // 确认模式 + 已有暂存改动：这条命令看不到本次修改，逐条贴一行说明
        let r = r.map(|txt| format!("{txt}{}", staged_execute_note(policy, text_changes)));
        slots[*i] = Some(("execute".into(), format!("execute {}", clip(cmd, 80)), r));
    }
    for ((i, spec), (_, r)) in bgs.iter().zip(bg_out) {
        let brief = format!("execute bg {}", clip(&spec.cmd, 70));
        slots[*i] = Some(("execute".into(), brief, r));
    }
    for ((i, op, handle), (_, r)) in procs.iter().zip(proc_out) {
        let brief = format!("execute {} {handle}", proc_op_name(*op));
        slots[*i] = Some(("execute".into(), brief, r));
    }
    // 连接：同一任务里并发推进
    if !conn_futs.is_empty() {
        for ((i, brief), r) in conn_briefs.iter().zip(JoinAll::new(conn_futs).await) {
            slots[*i] = Some(("connect".into(), brief.clone(), r));
        }
    }
    // 进展记忆：并发部分**全部收敛之后**再同步落。
    //
    // 为什么不进并发波：记账是顺序动作 —— 同一批里两条 findings 会一起写 `progress`，
    // 并发会让顺序与 id 漂移，而 id 正是模型用来 `supersede` 的锚。
    for (i, items) in finds {
        let mut ids: Vec<String> = Vec::new();
        let mut errs: Vec<String> = Vec::new();
        for it in &items {
            match ctx.record_finding(it) {
                Ok(id) => ids.push(id),
                Err(e) => errs.push(e),
            }
        }
        let brief = format!("record_findings {} 条", items.len());
        let res = if errs.is_empty() {
            Ok(format!(
                "已记录：{}。\n{}\n（下一轮你仍会看到这些条目；**别**为了回忆它们去重读文件。）",
                ids.join("、"),
                ctx.progress().byte_account()
            ))
        } else {
            // 部分失败要指名到条：只回"出错了"会让模型以为全记上了
            Err(format!("被拒 {} 条：{}", errs.len(), errs.join("；")))
        };
        slots[i] = Some(("findings".into(), brief, res));
    }
}
