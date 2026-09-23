//! Agent 评估：把"Harness 到底有没有用"变成可测的量（v0.7）。
//!
//! 为什么要有这个模块：v0.6 的两轮实验证明了一件事 —— **方法对了，问题就能回答**；
//! 但每次都要手搓一遍实验（CDP 驱动真窗口、一轮 2~3 分钟）。这个模块把那套方法固化成产品能力：
//!
//! ```text
//! 任务集 eval/tasks/*.toml（任务描述 + 机械可判的验收判据）
//!    ├── raw                ：裸模型一次调用 → 解析文件 → 判据（基线）
//!    ├── harness            ：plan → generate → lint → verify →（repair）→ 判据
//!    ├── harness_nokb       ：同上但关知识库（消融）
//!    └── harness_norepair   ：同上但关自我纠正（消融）
//! ```
//!
//! 三条纪律（都是 v0.6 用血换来的，写在 `doc/需求-Harness-v0.7-Agent评估.md` 里）：
//!
//! 1. **判据机械可判、正向且有上下界**：不写"代码质量好"，写"存在 orderkit.py 且每条 return 都是
//!    三元组且出现 `E_*` 错误码"。只判"没有坏东西"会被"什么都不做"满足；
//! 2. **无效轮不算失败**：抛异常/超时的轮标 `ok=false` 并**排除出分母** ——
//!    否则等于把"跑失败"算成"模型不会"；
//! 3. **判据复用已有仪器**：能用 `harness_lint` 规则的（裸 except、分层、循环依赖）就用它，
//!    能用 `verify` 的测试结果就用它 —— 不另造一套判据。
//!
//! 无头 ≠ GUI 管道的替代：GUI 那条路还有 UI 事件、取消位、进度渲染。两者共用同一批库函数，
//! 但阶段顺序与停止条件要**显式对齐**（见需求文档 §4 决策点 4）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::config::{self, AppConfig};
use crate::workspace::RunRecord;
use crate::{exec, generate, kb, lint, llm, pipeline, plan, verify, workspace};

// ============================================================
// 任务集
// ============================================================

/// 一个评估任务：任务描述 + **机械可判**的验收判据。
///
/// 判据刻意做成声明式（文件 / 正则 / 规则 ID / 测试必须通过），因为它们要能被
/// 无歧义地复算 —— "质量好"这种判据在评估里等于没有判据。
#[derive(Deserialize, Clone, Debug)]
pub struct TaskSpec {
    pub id: String,
    #[serde(default)]
    pub title: String,
    /// 给模型的任务描述（与 GUI 里填的是同一个东西）
    pub task: String,
    /// 这次任务专用的知识库夹具目录（相对任务集的父目录）；空 = 不给知识库
    #[serde(default)]
    pub kb: String,
    /// **预置到产物目录里的既有文件**：产物内路径 → 源文件（相对任务集父目录）。
    /// 例：`"store_util.py" = "kb/reuse/store_util.py"`。
    /// 没有这个，"在既有代码库里复用/修改"这类任务根本没法测（import 找不到模块）。
    #[serde(default)]
    pub seed: std::collections::BTreeMap<String, String>,
    /// 必须存在的文件（相对产物根）
    #[serde(default)]
    pub files: Vec<String>,
    /// 必须命中的正则（作用于产物里所有文本文件的内容）
    #[serde(default)]
    pub require_regex: Vec<String>,
    /// 不许命中的正则
    #[serde(default)]
    pub forbid_regex: Vec<String>,
    /// "至少出现 N 次"的形态判据：正则 → 最少命中次数
    /// （例：`"return (True|False), " = 3` —— 少于 3 条三元组 return 就说明约定没真落地）
    #[serde(default)]
    pub min_match: std::collections::BTreeMap<String, usize>,
    /// 不许出现的 lint 规则（例如 HX201 裸 except）
    #[serde(default)]
    pub forbid_rules: Vec<String>,
    /// 覆盖 `lint.max_repair_rounds`（None = 用配置；Some(0) = 关掉自我纠正）
    #[serde(default)]
    pub max_repair_rounds: Option<u32>,
    /// 单元测试是否必须真跑通（默认 true）
    #[serde(default = "yes")]
    pub tests_must_pass: bool,
    /// lint 的 error 上限（默认 0）
    #[serde(default)]
    pub max_lint_errors: Option<u32>,
    /// 是否要求产物真的做出了"三层目录"这类结构（默认不要求）
    #[serde(default)]
    pub require_layers: Vec<String>,
}

fn yes() -> bool {
    true
}

impl TaskSpec {
    fn load(path: &Path) -> Result<TaskSpec, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("读任务失败 {}: {e}", path.display()))?;
        let spec: TaskSpec =
            toml::from_str(&text).map_err(|e| format!("解析任务失败 {}: {e}", path.display()))?;
        Ok(spec)
    }

    fn max_errors(&self) -> u32 {
        self.max_lint_errors.unwrap_or(0)
    }
}

pub fn load_suite(dir: &Path) -> Result<Vec<TaskSpec>, String> {
    let mut out = Vec::new();
    let rd = std::fs::read_dir(dir).map_err(|e| format!("读任务集失败 {}: {e}", dir.display()))?;
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("toml") {
            out.push(TaskSpec::load(&p)?);
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    if out.is_empty() {
        return Err(format!("任务集是空的：{}", dir.display()));
    }
    Ok(out)
}

// ============================================================
// 臂（配置）
// ============================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Arm {
    /// 裸模型：一次调用直接出文件（基线）
    Raw,
    /// 完整 harness
    Harness,
    /// 消融：关知识库注入
    HarnessNoKb,
    /// 消融：关自我纠正循环
    HarnessNoRepair,
}

impl Arm {
    pub fn parse(s: &str) -> Result<Arm, String> {
        match s.trim() {
            "raw" => Ok(Arm::Raw),
            "harness" => Ok(Arm::Harness),
            "harness_nokb" => Ok(Arm::HarnessNoKb),
            "harness_norepair" => Ok(Arm::HarnessNoRepair),
            other => Err(format!(
                "未知臂 {other}（可选：raw / harness / harness_nokb / harness_norepair）"
            )),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Arm::Raw => "raw",
            Arm::Harness => "harness",
            Arm::HarnessNoKb => "harness_nokb",
            Arm::HarnessNoRepair => "harness_norepair",
        }
    }

    fn uses_harness(self) -> bool {
        !matches!(self, Arm::Raw)
    }

    fn uses_kb(self) -> bool {
        matches!(self, Arm::Harness | Arm::HarnessNoRepair)
    }

    fn uses_repair(self) -> bool {
        matches!(self, Arm::Harness | Arm::HarnessNoKb)
    }
}

// ============================================================
// 一次运行的结果
// ============================================================

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CheckOutcome {
    pub name: String,
    pub kind: String,
    pub pass: bool,
    pub detail: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct EvalRun {
    pub task_id: String,
    /// 任务标题（报告里一眼看出这个任务在考什么）
    #[serde(default)]
    pub task_title: String,
    pub arm: String,
    pub round: u32,
    pub run_id: String,
    /// false = **无效轮**（跑挂了），不计入分母
    pub ok: bool,
    pub error: String,
    pub checks: Vec<CheckOutcome>,
    pub acceptance: bool,
    pub syntax_ok: bool,
    pub tests_ran: bool,
    pub tests_ok: bool,
    pub lint_errors: u32,
    pub lint_warnings: u32,
    pub lint_by_rule: BTreeMap<String, u32>,
    /// 修复循环前后的"问题数"（规约违规 + 验证失败），用来量**修复增益**
    pub issues_before: u32,
    pub issues_after: u32,
    pub repair_rounds: u32,
    pub repair_applied: u32,
    pub files: Vec<String>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// 自我纠正循环的 token（`repair::propose` 只给总数）
    #[serde(default)]
    pub repair_tokens: u64,
    /// 知识库**到底进去了没有**（v2 实验的教训：知识没进去时，"没效果"不能算知识没用）
    #[serde(default)]
    pub kb_hits: usize,
    #[serde(default)]
    pub kb_chars: usize,
    #[serde(default)]
    pub kb_reason: String,
    /// 这一轮预置了哪些既有文件（相对产物根）
    #[serde(default)]
    pub seeded: Vec<String>,
    pub elapsed_ms: u128,
}

// ============================================================
// 判据求值
// ============================================================

/// 收集产物里的文本文件内容（判据要对源码整体做形态检查）。
fn read_text_files(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if p.is_dir() {
                if name == ".git" || name == "__pycache__" || name.starts_with(".harness") {
                    continue;
                }
                stack.push(p);
                continue;
            }
            let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
            if !matches!(
                ext,
                "py" | "md" | "txt" | "toml" | "json" | "js" | "rs" | "go" | "cfg" | "ini"
            ) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&p) {
                let rel = p
                    .strip_prefix(root)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, text));
            }
        }
    }
    out.sort();
    out
}

fn rx(pattern: &str) -> Result<regex::Regex, String> {
    regex::Regex::new(pattern).map_err(|e| format!("判据正则写错了 {pattern}: {e}"))
}

/// 对一棵产物树求值全部判据。**判据只看产物，不看过程** —— 过程由 metrics 记。
pub fn evaluate(
    spec: &TaskSpec,
    proj: &Path,
    verify: Option<&verify::VerifyReport>,
    lint_report: Option<&lint::LintReport>,
) -> Result<(Vec<CheckOutcome>, bool), String> {
    let files = read_text_files(proj);
    let joined: String = files
        .iter()
        .map(|(rel, body)| format!("\n// ==== {rel} ====\n{body}"))
        .collect();
    let mut checks: Vec<CheckOutcome> = Vec::new();

    // ① 必须存在的文件
    for f in &spec.files {
        let exists = proj.join(f).is_file();
        checks.push(CheckOutcome {
            name: format!("文件存在：{f}"),
            kind: "files".into(),
            pass: exists,
            detail: if exists {
                String::new()
            } else {
                "缺失".into()
            },
        });
    }

    // ② 必须出现的形态
    for pat in &spec.require_regex {
        let re = rx(pat)?;
        let pass = re.is_match(&joined);
        let detail = if pass {
            String::new()
        } else {
            format!("没有任何一处匹配 /{pat}/")
        };
        checks.push(CheckOutcome {
            name: format!("形态：/{pat}/"),
            kind: "require_regex".into(),
            pass,
            detail,
        });
    }

    // ③ 不许出现的形态
    for pat in &spec.forbid_regex {
        let re = rx(pat)?;
        let hit = re.find(&joined).map(|m| m.as_str().to_string());
        checks.push(CheckOutcome {
            name: format!("禁止：/{pat}/"),
            kind: "forbid_regex".into(),
            pass: hit.is_none(),
            detail: hit.map(|h| format!("命中：{h}")).unwrap_or_default(),
        });
    }

    // ③' 至少出现 N 次（"做到了"的强度判据：出现一次和真落地是两件事）
    for (pat, min) in &spec.min_match {
        let re = rx(pat)?;
        let n = re.find_iter(&joined).count();
        checks.push(CheckOutcome {
            name: format!("至少 {min} 处：/{pat}/"),
            kind: "min_match".into(),
            pass: n >= *min,
            detail: format!("实际 {n} 处"),
        });
    }

    // ④ 不许出现的 lint 规则（复用 linter，不另造判据）
    for rule in &spec.forbid_rules {
        let n = lint_report
            .and_then(|l| l.by_rule.get(rule).copied())
            .unwrap_or(0);
        checks.push(CheckOutcome {
            name: format!("规约：不出现 {rule}"),
            kind: "forbid_rules".into(),
            pass: n == 0,
            detail: if n == 0 {
                String::new()
            } else {
                format!("{rule} × {n}")
            },
        });
    }

    // ⑤ 三层结构这类"目录得真有"的判据（v1 实验的教训：没有层也就没有违规）。
    // 语义：**层里得有文件**才算 —— 空目录不算（空目录里没有任何代码，等于没这一层）

    for layer in &spec.require_layers {
        let present = files
            .iter()
            .any(|(rel, _)| rel.starts_with(&format!("{layer}/")));
        checks.push(CheckOutcome {
            name: format!("目录存在：{layer}/"),
            kind: "require_layers".into(),
            pass: present,
            detail: if present {
                String::new()
            } else {
                "没有这个层".into()
            },
        });
    }

    // ⑥ 单元测试必须真跑通
    if spec.tests_must_pass {
        let test_checks: Vec<&verify::CheckResult> = verify
            .map(|v| v.checks.iter().filter(|c| c.kind == "test").collect())
            .unwrap_or_default();
        let ran = test_checks.iter().any(|c| c.status != "skipped");
        let passed = test_checks.iter().any(|c| c.status == "passed");
        checks.push(CheckOutcome {
            name: "单元测试通过".into(),
            kind: "tests".into(),
            pass: ran && passed,
            detail: if !ran {
                "没跑到测试（环境/沙箱跳过了）".into()
            } else if passed {
                String::new()
            } else {
                test_checks
                    .iter()
                    .map(|c| format!("{}: {}", c.target, c.reason))
                    .collect::<Vec<_>>()
                    .join("; ")
            },
        });
    }

    // ⑦ lint 的 error 上限
    let errors = lint_report.map(|l| l.errors()).unwrap_or(0);
    checks.push(CheckOutcome {
        name: format!("规约 error ≤ {}", spec.max_errors()),
        kind: "lint_errors".into(),
        pass: errors <= spec.max_errors(),
        detail: if errors <= spec.max_errors() {
            String::new()
        } else {
            format!("error {errors} 条")
        },
    });

    let accept = checks.iter().all(|c| c.pass);
    Ok((checks, accept))
}

// ============================================================
// 各臂的执行
// ============================================================

const RAW_SYSTEM: &str = r#"你是资深软件工程师。请**一次性**给出完成该任务所需的全部文件。
只输出一个 JSON 对象，不要任何解释、不要 markdown 代码块：
{"files": [{"path": "相对路径", "content": "文件全文"}]}
注意：content 里必须包含完整可运行的文件内容，路径用 / 分隔。"#;

/// 从模型输出里抠出 {files:[{path,content}]}（裸模型臂专用）。
fn parse_raw_files(raw: &str) -> Result<Vec<(String, String)>, String> {
    let json = llm::extract_json_object(raw);
    let v: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!("不是合法 JSON：{e}"))?;
    let arr = v
        .get("files")
        .and_then(|f| f.as_array())
        .ok_or("输出里没有 files 数组")?;
    let mut out = Vec::new();
    for item in arr {
        let path = item
            .get("path")
            .and_then(|p| p.as_str())
            .unwrap_or("")
            .to_string();
        let content = item
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();
        if path.trim().is_empty() {
            continue;
        }
        out.push((path, content));
    }
    if out.is_empty() {
        return Err("files 是空的".into());
    }
    Ok(out)
}

/// 跑一个任务的一轮，返回可比较的指标。
pub async fn run_once(
    cfg: &AppConfig,
    spec: &TaskSpec,
    arm: Arm,
    round: u32,
    suite_dir: &Path,
    kb_dir_override: Option<PathBuf>,
) -> EvalRun {
    let t0 = Instant::now();
    let mut r = EvalRun {
        task_id: spec.id.clone(),
        task_title: spec.title.clone(),
        arm: arm.label().to_string(),
        round,
        ..Default::default()
    };
    let root = config::runs_root(cfg);
    // run_id 用**任务 id**（ASCII）：标题可能是中文，而 run_id 会进运行目录名与
    // docker 容器名 —— 中文容器名会被 docker 拒掉（exit 125）。
    let run_id = workspace::new_run_id(&plan::slugify(&spec.id));
    let proj = match workspace::project_dir(&root, &run_id) {
        Ok(p) => p,
        Err(e) => {
            r.error = e;
            r.ok = false;
            return r;
        }
    };
    r.run_id = run_id.clone();

    if let Err(e) = run_arm_inner(
        cfg,
        spec,
        arm,
        round,
        suite_dir,
        kb_dir_override,
        &root,
        &proj,
        &mut r,
    )
    .await
    {
        r.error = e;
        r.ok = false;
        r.elapsed_ms = t0.elapsed().as_millis();
        return r;
    }

    r.files = generate::file_listing(&proj);
    r.elapsed_ms = t0.elapsed().as_millis();

    // 判据求值：从**这次运行自己落盘的记录**里读验证/规约结论（两个臂同一条路），
    // 这样"评估看到的东西"与"用户在 GUI / harness context 里看到的东西"是同一份。
    let rec = workspace::load(&root, &r.run_id).ok();
    let vrep = rec.as_ref().and_then(|x| x.verify.as_ref());
    let lint = rec.as_ref().and_then(|x| x.lint.as_ref());
    match evaluate(spec, &proj, vrep, lint) {
        Ok((checks, accept)) => {
            r.checks = checks;
            r.acceptance = accept;
        }
        Err(e) => {
            r.ok = false;
            r.error = format!("判据求值失败：{e}");
            return r;
        }
    }
    // 无效轮：任务要求"测试必须过"，但这轮**压根没跑到测试**（沙箱/环境跳过）——
    // 那是仪器的问题，不是模型不会。按无效轮处理（不计入分母），并在报告里报数量。
    if r.ok && spec.tests_must_pass && !r.tests_ran {
        r.ok = false;
        r.error = "没跑到单元测试（环境/沙箱跳过）—— 仪器问题，不计入分母".to_string();
        return r;
    }
    r.ok = true;
    r
}

#[allow(clippy::too_many_arguments)]
/// 评估的 Sink：**不往 stdout 刷管道日志**（评估自己打进度行），
/// 但照样写观测日志 —— 每轮的 `logs.jsonl` / trace 与 GUI 跑出来的是同一份格式。
struct EvalSink {
    verbose: bool,
}

impl pipeline::Sink for EvalSink {
    fn log(&self, level: &str, msg: String) {
        crate::observe::log(level, "", &msg);
        if self.verbose {
            println!("[{level}] {msg}");
        }
    }
}

/// 跑一个臂的一轮：四个臂**都走同一份管道实现**（raw 只是"没有 harness"的对照）。
/// 基线臂（raw）：**一次调用直接出文件**。没有计划、没有规约检查、没有自我纠正，
/// 也不重试、不回灌诊断 —— 刻意"弱得干净"，这样差值才等于 harness 的净效果。
async fn run_raw_arm(
    cfg: &AppConfig,
    spec: &TaskSpec,
    root: &Path,
    proj: &Path,
    r: &mut EvalRun,
    seeds: &[(String, String)],
) -> Result<(), String> {
    let t0 = Instant::now();
    // 与管道臂**同一份**预置实现：基线也要看到"仓库里已有的东西"
    pipeline::seed_files(root, &r.run_id, seeds)?;
    let msgs = [
        llm::ChatMessage::system(RAW_SYSTEM.to_string()),
        llm::ChatMessage::user(spec.task.clone()),
    ];
    // JSON 模式：系统提示要求"只输出一个 JSON 对象"，与 plan/generate 的调用方式一致
    let out = llm::chat(&cfg.llm, cfg.llm_fallback.as_ref(), &msgs, true).await?;
    r.tokens_in = out.usage.prompt_tokens;
    r.tokens_out = out.usage.completion_tokens;
    let files = parse_raw_files(&out.content)?;
    for (rel, body) in &files {
        let safe = generate::safe_rel_path(rel)?;
        let path = proj.join(&safe);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, body).map_err(|e| format!("写文件失败 {safe}: {e}"))?;
    }
    r.files = generate::file_listing(proj);
    // 用**同一套仪器**量（验证 + 规约）：否则判据没法算，也没法和 harness 比
    let flag = exec::new_cancel_flag();
    let vrep = verify::run(cfg, &r.run_id, proj, &flag);
    fill_verify_metrics(r, Some(&vrep));
    let lint_out = lint::run_lint(proj, cfg);
    if let Some(l) = &lint_out.report {
        r.lint_errors = l.errors();
        r.lint_warnings = l.warnings();
        r.lint_by_rule = l.by_rule.clone();
    }
    // 也落一份运行记录：评估的每一轮都要能在 `harness logs/context` 里查到
    // （而且判据求值统一从运行记录里读验证/规约结论 —— 两个臂用同一条路）
    let dir = workspace::run_dir(root, &r.run_id)?;
    let rec = RunRecord {
        run_id: r.run_id.clone(),
        task: spec.task.trim().to_string(),
        created_at: workspace::now_iso(),
        model: out.model.clone(),
        status: "evaluated".into(),
        dir: dir.to_string_lossy().to_string(),
        verify: Some(vrep.clone()),
        lint: lint_out.report.clone(),
        usage: out.usage.clone(),
        ..Default::default()
    };
    let _ = workspace::save(root, &rec);
    r.elapsed_ms = t0.elapsed().as_millis();
    Ok(())
}

/// 跑一个臂的一轮：raw 走自己的极简路径，其余三个臂**全部走同一份管道实现**。
// 评估臂的完整上下文（套件/任务/轮次/目录/覆盖/记录），拆了更难对齐论文口径
#[allow(clippy::too_many_arguments)]
async fn run_arm_inner(
    cfg: &AppConfig,
    spec: &TaskSpec,
    arm: Arm,
    round: u32,
    suite_dir: &Path,
    kb_dir_override: Option<PathBuf>,
    root: &Path,
    proj: &Path,
    r: &mut EvalRun,
) -> Result<(), String> {
    let seeds: Vec<(String, String)> = {
        let base = suite_dir.parent().unwrap_or(suite_dir);
        spec.seed
            .iter()
            .map(|(rel, src)| (rel.clone(), base.join(src).to_string_lossy().to_string()))
            .collect()
    };
    // 知识库目录：评估**只认任务自己声明的夹具**，用独立目录（不碰用户自己的 kb.json）。
    // 没夹具的任务指向一个空目录 —— 评估必须可复现，不能因为"用户机器上恰好有知识库"而变好变坏。
    // SAFETY：评估单线程顺序跑任务，改这个环境变量前没有并发读它的地方。
    let kb_dir = kb_dir_override.clone().unwrap_or_else(empty_kb_dir);
    unsafe { std::env::set_var("HARNESS_KB_DIR", &kb_dir) };

    if !arm.uses_harness() {
        return run_raw_arm(cfg, spec, root, proj, r, &seeds).await;
    }
    // 消融臂 / 没声明夹具的任务：关掉知识库注入（管道读的就是这个开关）
    let mut c2 = cfg.clone();
    if !(arm.uses_kb() && kb_dir_override.is_some()) {
        c2.kb.enabled = false;
    }
    run_pipeline_arm(&c2, spec, arm, round, root, proj, r, &seeds).await
}

/// 真正跑管道并把指标搬进 `EvalRun`。
#[allow(clippy::too_many_arguments)]
async fn run_pipeline_arm(
    cfg: &AppConfig,
    spec: &TaskSpec,
    arm: Arm,
    _round: u32,
    root: &Path,
    proj: &Path,
    r: &mut EvalRun,
    seeds: &[(String, String)],
) -> Result<(), String> {
    let t0 = Instant::now();
    let flag = exec::new_cancel_flag();
    let rec = pipeline::run(
        cfg,
        flag,
        pipeline::RunOpts {
            task: spec.task.clone(),
            run_id: Some(r.run_id.clone()),
            seed: seeds.to_vec(),
            // 消融臂：把修复轮数压成 0（管道里 0 = 关掉循环）
            max_repair_rounds: if arm.uses_repair() {
                spec.max_repair_rounds
            } else {
                Some(0)
            },
        },
        &EvalSink { verbose: false },
    )
    .await?;
    r.elapsed_ms = t0.elapsed().as_millis();
    // 指标一律从**运行记录**里取 —— 与 GUI 看到的是同一份数据
    let (tin, tout) = (rec.usage.prompt_tokens, rec.usage.completion_tokens);
    r.tokens_in = tin;
    r.tokens_out = tout;
    r.repair_tokens = rec.repair.iter().map(|x| x.usage_tokens).sum();
    r.repair_rounds = rec.repair.len() as u32;
    r.repair_applied = rec.repair.iter().map(|x| x.applied.len() as u32).sum();
    r.issues_before = rec.repair.first().map(|x| x.before).unwrap_or(0);
    r.issues_after = rec
        .repair
        .last()
        .map(|x| x.after)
        .unwrap_or_else(|| pipeline::issue_count(&rec));
    if let Some(g) = &rec.generation {
        r.files = g.files.iter().map(|f| f.path.clone()).collect();
    }
    // 知识库注入情况（v2 的教训：先确认知识进没进去，再谈效果）
    if let Some(k) = rec.kb.first() {
        r.kb_hits = k.hits.len();
        r.kb_chars = k.injected_chars;
        r.kb_reason = k.reason.clone();
    }
    fill_verify_metrics(r, rec.verify.as_ref());
    if let Some(l) = &rec.lint {
        r.lint_errors = l.errors();
        r.lint_warnings = l.warnings();
        r.lint_by_rule = l.by_rule.clone();
    }
    let _ = (proj, root);
    Ok(())
}

/// 评估专用的**空**知识库目录（没有夹具的任务用它）：确定"没有来源"，而不是"用了用户的"。
fn empty_kb_dir() -> PathBuf {
    let d = std::env::temp_dir().join("darkhorse-eval-kb-empty");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn fill_verify_metrics(r: &mut EvalRun, v: Option<&verify::VerifyReport>) {
    let Some(v) = v else {
        // 没跑到验证（跑挂了/被取消）：一律按"没做到"记，并由上层标成无效轮
        r.syntax_ok = false;
        r.tests_ran = false;
        r.tests_ok = false;
        return;
    };
    r.syntax_ok = v
        .checks
        .iter()
        .filter(|c| c.kind == "syntax")
        .all(|c| c.status != "failed");
    let tests: Vec<&verify::CheckResult> = v.checks.iter().filter(|c| c.kind == "test").collect();
    r.tests_ran = tests.iter().any(|c| c.status != "skipped");
    r.tests_ok = tests.iter().any(|c| c.status == "passed");
}

// ============================================================
// 统计
// ============================================================

/// Wilson 区间：小样本的比例区间，比正态近似稳（pass@1 这种量经常只有几轮）。
pub fn wilson(k: usize, n: usize) -> (f64, f64) {
    if n == 0 {
        return (0.0, 0.0);
    }
    let p = k as f64 / n as f64;
    let z = 1.96_f64;
    let z2 = z * z;
    let denom = 1.0 + z2 / n as f64;
    let center = p + z2 / (2.0 * n as f64);
    let half = z * ((p * (1.0 - p) / n as f64) + z2 / (4.0 * (n as f64).powi(2))).sqrt();
    (
        ((center - half) / denom).max(0.0),
        ((center + half) / denom).min(1.0),
    )
}

fn ln_fact(n: u64) -> f64 {
    let mut s = 0.0;
    for i in 2..=n {
        s += (i as f64).ln();
    }
    s
}

/// Fisher 精确检验（双侧）—— 小样本下只有这个能用（n 小、又是二元判据）。
pub fn fisher_two_sided(table: [[u64; 2]; 2]) -> f64 {
    let [[a, b], [c, d]] = table;
    let n = a + b + c + d;
    if n == 0 {
        return 1.0;
    }
    let r1 = a + b;
    let r2 = c + d;
    let c1 = a + c;
    // 枚举下界：左上格 x 必须满足 c1 - x ≤ r2（右下行够放），即 x ≥ c1 - r2 = a - d。
    // （这里原来写成 r1 - d，对两臂不等的表会漏掉若干张表，p 值直接算错 —— 单测抓到的。）
    let lo = a.saturating_sub(d);
    let hi = r1.min(c1);
    let base = ln_fact(r1) + ln_fact(r2) + ln_fact(c1) + ln_fact(n - c1) - ln_fact(n);
    let prob = |x: u64| {
        (base - ln_fact(x) - ln_fact(r1 - x) - ln_fact(c1 - x) - ln_fact(r2 - (c1 - x))).exp()
    };
    let p_obs = prob(a);
    let mut tot = 0.0;
    for x in lo..=hi {
        let p = prob(x);
        if p <= p_obs * (1.0 + 1e-9) {
            tot += p;
        }
    }
    tot.min(1.0)
}

// ============================================================
// 汇总与报告
// ============================================================

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ArmSummary {
    pub runs: usize,
    pub valid: usize,
    pub invalid: usize,
    pub accepted: usize,
    pub pass_at_1: f64,
    pub ci_low: f64,
    pub ci_high: f64,
    /// 判据名 → (通过轮数, 有效轮数)：**哪一条判据本臂从未通过，一眼能看见**
    pub checks: BTreeMap<String, (usize, usize)>,
    pub tokens_mean: f64,
    pub elapsed_ms_mean: f64,
    pub tests_ran_rounds: usize,
    pub repair_rounds_total: u32,
    pub repair_improved_total: usize,
    /// 知识注入：有效轮里有几轮真的注入了内容（0 就意味着"知识没进去"）
    pub kb_rounds_with_hits: usize,
    pub kb_chars_mean: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FisherRow {
    pub a: String,
    pub b: String,
    pub p_value: f64,
    pub note: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct EvalReport {
    pub suite: String,
    pub started_at: String,
    pub elapsed_ms: u128,
    pub rounds: u32,
    pub temperature: f32,
    /// 评估期间生效的 [kb] 参数（""= 用配置原值）
    pub kb_settings: String,
    pub model: String,
    pub tasks: Vec<String>,
    pub arms: Vec<String>,
    pub runs: Vec<EvalRun>,
    pub summary: BTreeMap<String, ArmSummary>,
    pub fisher: Vec<FisherRow>,
    /// 诚实清单：这一轮有什么没做到
    pub notes: Vec<String>,
}

pub fn summarize(
    runs: &[EvalRun],
    arms: &[String],
    tasks: &[String],
) -> BTreeMap<String, ArmSummary> {
    let mut out = BTreeMap::new();
    for arm in arms {
        let mine: Vec<&EvalRun> = runs.iter().filter(|r| &r.arm == arm).collect();
        let valid: Vec<&&EvalRun> = mine.iter().filter(|r| r.ok).collect();
        let accepted = valid.iter().filter(|r| r.acceptance).count();
        let n = valid.len();
        let (lo, hi) = wilson(accepted, n);
        let mut checks: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for r in &valid {
            for c in &r.checks {
                let e = checks.entry(c.name.clone()).or_insert((0, 0));
                e.1 += 1;
                if c.pass {
                    e.0 += 1;
                }
            }
        }
        let s = ArmSummary {
            runs: mine.len(),
            valid: n,
            invalid: mine.len() - n,
            accepted,
            pass_at_1: if n == 0 {
                0.0
            } else {
                accepted as f64 / n as f64
            },
            ci_low: lo,
            ci_high: hi,
            checks,
            tokens_mean: if n == 0 {
                0.0
            } else {
                valid
                    .iter()
                    .map(|r| (r.tokens_in + r.tokens_out + r.repair_tokens) as f64)
                    .sum::<f64>()
                    / n as f64
            },
            elapsed_ms_mean: if n == 0 {
                0.0
            } else {
                valid.iter().map(|r| r.elapsed_ms as f64).sum::<f64>() / n as f64
            },
            tests_ran_rounds: valid.iter().filter(|r| r.tests_ran).count(),
            repair_rounds_total: valid.iter().map(|r| r.repair_rounds).sum(),
            repair_improved_total: valid
                .iter()
                .filter(|r| r.issues_after < r.issues_before)
                .count(),
            kb_rounds_with_hits: valid.iter().filter(|r| r.kb_hits > 0).count(),
            kb_chars_mean: if n == 0 {
                0.0
            } else {
                valid.iter().map(|r| r.kb_chars as f64).sum::<f64>() / n as f64
            },
        };
        out.insert(arm.clone(), s);
    }
    let _ = tasks;
    out
}

fn md_table(report: &EvalReport) -> String {
    let mut s = String::new();
    s.push_str(&format!(
        "# Harness 评估报告\n\n- 任务集：`{}`（{} 个任务）\n- 臂：{}\n- 每臂轮数：{}\n- 模型：{} · temperature {}\n- 耗时：{:.1} 分钟\n\n",
        report.suite,
        report.tasks.len(),
        report.arms.join(" / "),
        report.rounds,
        report.model,
        report.temperature,
        report.elapsed_ms as f64 / 60000.0
    ));
    s.push_str("## pass@1（无效轮不计入分母）\n\n");
    s.push_str("| 臂 | 有效轮 | 无效轮 | 通过 | pass@1 | 95% CI (Wilson) | tokens 均值 | 耗时均值 | 注入知识的轮数 | 注入字符均值 |\n|---|---|---|---|---|---|---|---|---|---|\n");
    for arm in &report.arms {
        if let Some(a) = report.summary.get(arm) {
            s.push_str(&format!(
                "| `{}` | {} | {} | {} | **{:.0}%** | {:.0}% ~ {:.0}% | {:.0} | {:.0}s | {}/{} | {:.0} |\n",
                arm,
                a.valid,
                a.invalid,
                a.accepted,
                a.pass_at_1 * 100.0,
                a.ci_low * 100.0,
                a.ci_high * 100.0,
                a.tokens_mean,
                a.elapsed_ms_mean / 1000.0,
                a.kb_rounds_with_hits,
                a.valid,
                a.kb_chars_mean
            ));
        }
    }
    s.push_str("\n## 逐条判据通过率（**从未通过的判据也要写出来**）\n\n");
    s.push_str("| 判据 | ");
    for arm in &report.arms {
        s.push_str(&format!("`{arm}` | "));
    }
    s.push_str("\n|---|");
    for _ in &report.arms {
        s.push_str("---|");
    }
    s.push('\n');
    let mut names: Vec<String> = Vec::new();
    for arm in &report.arms {
        if let Some(a) = report.summary.get(arm) {
            for k in a.checks.keys() {
                if !names.contains(k) {
                    names.push(k.clone());
                }
            }
        }
    }
    for name in names {
        s.push_str(&format!("| {name} | "));
        for arm in &report.arms {
            let cell = report
                .summary
                .get(arm)
                .and_then(|a| a.checks.get(&name))
                .map(|(p, n)| format!("{p}/{n}"))
                .unwrap_or_else(|| "—".into());
            s.push_str(&format!("{cell} | "));
        }
        s.push('\n');
    }
    s.push_str(
        "\n## 两两比较（Fisher 精确检验，双侧）\n\n| A | B | p | 说明 |\n|---|---|---|---|\n",
    );
    for f in &report.fisher {
        s.push_str(&format!(
            "| `{}` | `{}` | {:.4} | {} |\n",
            f.a, f.b, f.p_value, f.note
        ));
    }
    s.push_str("\n## 诚实清单\n\n");
    for n in &report.notes {
        s.push_str(&format!("- {n}\n"));
    }
    s.push('\n');
    s
}

// ============================================================
// CLI
// ============================================================

fn arg_value(args: &[String], key: &str) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == key {
            return it.next().cloned();
        }
        if let Some(rest) = a.strip_prefix(&format!("{key}=")) {
            return Some(rest.to_string());
        }
    }
    None
}

const USAGE: &str = r#"harness eval —— Agent 评估：量化 Harness 的效果

用法：
  darkhorse-harness eval --suite eval/tasks [--arms raw,harness,harness_nokb,harness_norepair]
                         [--rounds 1] [--only t01,t03] [--out eval-report.json] [--md eval-report.md]
                         [--kb-root <dir>] [--kb-settings "top_k=6,token_budget=4000,min_score=0.2"]
                         [--recheck]   # 判据改了之后，按既有产物重算（不重跑模型）

  每个任务一个 eval/tasks/<id>.toml：任务描述 + 机械可判的验收判据（见 doc/需求-Harness-v0.7-Agent评估.md）。
  退出码：0 = 全部通过；1 = 有失败；2 = 用法/内部错误。
"#;

pub async fn cli(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return 0;
    }
    let suite = arg_value(args, "--suite").unwrap_or_else(|| "eval/tasks".into());
    let suite_dir = PathBuf::from(&suite);
    let rounds: u32 = arg_value(args, "--rounds")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let arms: Vec<String> = arg_value(args, "--arms")
        .unwrap_or_else(|| "raw,harness,harness_nokb,harness_norepair".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let only: Vec<String> = arg_value(args, "--only")
        .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();
    let out_path = arg_value(args, "--out").unwrap_or_else(|| "eval-report.json".into());
    let md_path = arg_value(args, "--md").unwrap_or_else(|| "eval-report.md".into());
    // 评估**默认单独一个知识库目录**：不污染用户自己的 kb.json
    let kb_root = arg_value(args, "--kb-root")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("darkhorse-eval-kb"));
    // --recheck：判据改了之后，用**既有产物**重算一遍（不重跑模型）
    if args.iter().any(|a| a == "--recheck") {
        return recheck(&suite, &arms, &out_path, &md_path);
    }

    // 知识库参数：默认用配置原值；给了 --kb-settings 就临时改（跑完还原）。
    // 为什么要它能改：默认 top_k=4/budget=1200 装不下约定文档，"知识进不去"会被误读成"知识没用"。
    let kb_settings = arg_value(args, "--kb-settings").unwrap_or_default();

    let mut parsed_arms = Vec::new();
    for a in &arms {
        match Arm::parse(a) {
            Ok(x) => parsed_arms.push(x),
            Err(e) => {
                eprintln!("{e}");
                return 2;
            }
        }
    }

    // 引擎不再自己读配置文件：配置由调用方注入，脚本场景就是默认值 + 环境变量。
    let mut cfg = AppConfig::default();
    config::apply_env_overrides(&mut cfg);
    if cfg.llm.api_key.trim().is_empty() {
        eprintln!(
            "没有 API Key（配置表单的 ai.api_key 或环境变量 DEEPSEEK_API_KEY），评估要真调模型"
        );
        return 2;
    }
    // 实验参数在内存里覆盖（跑完还原）—— 不再去动任何文件。
    let kb_saved = if kb_settings.trim().is_empty() {
        std::collections::BTreeMap::new()
    } else {
        apply_kb_settings(&mut cfg, &kb_settings)
    };
    let mut specs = match load_suite(&suite_dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    if !only.is_empty() {
        specs.retain(|s| {
            only.iter()
                .any(|o| &s.id == o || s.id.starts_with(o.as_str()))
        });
    }
    if specs.is_empty() {
        eprintln!("筛选后没有任务（--only {only:?}）");
        return 2;
    }

    let t0 = Instant::now();
    let mut report = EvalReport {
        suite: suite.clone(),
        started_at: workspace::now_iso(),
        rounds,
        temperature: cfg.llm.temperature,
        kb_settings: kb_settings.clone(),
        model: cfg.llm.model.clone(),
        tasks: specs.iter().map(|s| s.id.clone()).collect(),
        arms: parsed_arms.iter().map(|a| a.label().to_string()).collect(),
        ..Default::default()
    };

    println!(
        "评估开始：{} 个任务 × {} 个臂 × {} 轮 = {} 次运行（模型 {}）",
        specs.len(),
        parsed_arms.len(),
        rounds,
        specs.len() * parsed_arms.len() * rounds as usize,
        cfg.llm.model
    );

    // 任务级知识库缓存：夹具每任务只建一次（内容不会变，别每轮重建）
    let mut kb_cache: BTreeMap<String, Option<PathBuf>> = BTreeMap::new();
    for round in 1..=rounds {
        for spec in &specs {
            if !kb_cache.contains_key(&spec.id) {
                let built = if spec.kb.trim().is_empty() {
                    None
                } else {
                    // 夹具路径按**任务集的父目录**解析（默认 eval/），这样任务文件里写
                    // `kb = "kb/orderkit"` 就指到 eval/kb/orderkit —— 与放置位置一致。
                    let kb_base = suite_dir.parent().unwrap_or(&suite_dir);
                    let src = kb_base.join(&spec.kb);
                    let dir = kb_root.join(&spec.id);
                    match build_kb(&cfg, &src, &dir) {
                        Ok(_) => Some(dir),
                        Err(e) => {
                            println!("  ⚠️ {}：知识库夹具建失败：{e}", spec.id);
                            None
                        }
                    }
                };
                kb_cache.insert(spec.id.clone(), built);
            }
            let kb_dir = kb_cache.get(&spec.id).cloned().flatten();
            for arm in &parsed_arms {
                let r = run_once(&cfg, spec, *arm, round, &suite_dir, kb_dir.clone()).await;
                let mark = if !r.ok {
                    format!("✗ 无效（{}）", exec::clip(&r.error, 60))
                } else if r.acceptance {
                    "✓ 通过".into()
                } else {
                    let failed: Vec<String> = r
                        .checks
                        .iter()
                        .filter(|c| !c.pass)
                        .map(|c| c.name.clone())
                        .collect();
                    format!("✗ 未过（{}）", exec::clip(&failed.join("；"), 80))
                };
                println!(
                    "  [{}] {:<22} 轮{} · {:<15} {} · {:.0}s   «{}»",
                    round,
                    spec.id,
                    r.round,
                    r.arm,
                    mark,
                    r.elapsed_ms as f64 / 1000.0,
                    spec.title
                );
                report.runs.push(r);
            }
            // 每任务落一次盘：长评估不能因为中途崩掉而全丢
            save_report(&report, &out_path, &md_path);
        }
    }
    report.elapsed_ms = t0.elapsed().as_millis();

    // 汇总 + 诚实清单
    let mut notes = Vec::new();
    let invalid_total = report.runs.iter().filter(|r| !r.ok).count();
    if invalid_total > 0 {
        notes.push(format!(
            "本轮有 {invalid_total} 个**无效轮**（跑挂了），已经排除出分母；逐轮原因见 JSON 的 runs[].error"
        ));
    }
    let no_tests = report.runs.iter().filter(|r| r.ok && !r.tests_ran).count();
    if no_tests > 0 {
        notes.push(format!(
            "有 {no_tests} 轮没跑到单元测试（沙箱/环境跳过）—— 这些轮在这条判据上必然失败，属于**环境**而不是模型能力"
        ));
    }
    notes.push("所有臂共用同一份任务描述与判据；`raw` 臂只调一次模型、不重试、不给诊断".into());
    notes.push(format!(
        "每臂每任务 {} 轮：只够看大效应；置信区间按 Wilson 给（小样本）",
        rounds
    ));
    report.notes = notes;
    report.summary = summarize(&report.runs, &report.arms.clone(), &report.tasks.clone());

    // 两两比较（与 recheck 共用同一份实现）
    recompute_fisher(&mut report);

    save_report(&report, &out_path, &md_path);
    if !kb_saved.is_empty() {
        restore_kb_settings(&mut cfg, &kb_saved);
        println!("  [kb] 参数已还原到配置原值");
    }
    println!("\n报告已写入 {out_path}");
    println!("              {md_path}");

    if report
        .summary
        .get("harness")
        .map(|a| a.accepted)
        .unwrap_or(0)
        > 0
    {
        0
    } else {
        1
    }
}

const KB_KEYS: [&str; 6] = [
    "enabled",
    "top_k",
    "token_budget",
    "per_source_limit",
    "min_score",
    "chunk_chars",
];

/// 临时改 `kb` 参数（返回原值，跑完还原）。
///
/// **在内存里改**，不再动用户的配置文件。以前那套是"先把原值写进备份文件、
/// 再改配置、跑完还原、下次启动还来自愈"—— 全是为了防"Ctrl-C 打断把实验值
/// 留在用户配置里"。配置入口废掉之后这些防护一个都不需要了：没有文件可污染。
fn apply_kb_settings(
    cfg: &mut AppConfig,
    spec: &str,
) -> std::collections::BTreeMap<String, String> {
    let mut saved = std::collections::BTreeMap::new();
    let mut pairs: Vec<(String, String)> = Vec::new();
    for pair in spec.split(',').filter(|p| p.contains('=')) {
        let (k, v) = pair.split_once('=').unwrap();
        let k = k.trim();
        if !KB_KEYS.contains(&k) {
            println!("  ⚠️ kb 里没有 {}（跳过）", k);
            continue;
        }
        if let Some(old) = config::flat_value(cfg, &format!("kb.{k}")) {
            saved.insert(k.to_string(), old);
        }
        pairs.push((format!("kb.{k}"), v.trim().to_string()));
    }
    config::apply_flat(cfg, &pairs);
    println!("  [kb] 实验参数：{spec}");
    saved
}

fn restore_kb_settings(cfg: &mut AppConfig, saved: &std::collections::BTreeMap<String, String>) {
    let pairs: Vec<(String, String)> = saved
        .iter()
        .map(|(k, v)| (format!("kb.{k}"), v.clone()))
        .collect();
    config::apply_flat(cfg, &pairs);
}

/// 判据修正后按既有产物重算（模型输出一个字节都不动）。
///
/// 用途：判据写错/写窄时，改判据 + 重算，而不是重跑模型 ——
/// 重跑会引入新的随机性，还会把"判据修正"和"模型变化"两个变量混在一起。
fn recheck(suite: &str, arms: &[String], out_path: &str, md_path: &str) -> i32 {
    let Ok(text) = std::fs::read_to_string(out_path) else {
        eprintln!("读不到报告：{out_path}（--recheck 要在原报告上重算）");
        return 2;
    };
    let mut report: EvalReport = match serde_json::from_str(&text) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("解析报告失败：{e}");
            return 2;
        }
    };
    let specs = match load_suite(Path::new(suite)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let root = Some(config::runs_root(&AppConfig::default()));
    let mut rechecked = 0usize;
    let mut missing = 0usize;
    for r in report.runs.iter_mut() {
        if r.run_id.is_empty() {
            continue; // 本来就没跑起来
        }
        let Some(spec) = specs.iter().find(|s| s.id == r.task_id) else {
            continue;
        };
        let Some(root) = root.as_ref() else { break };
        let proj = workspace::project_dir(root, &r.run_id).unwrap_or_default();
        if !proj.is_dir() {
            missing += 1;
            continue;
        }
        // verify / lint 的结论也从那次 run 的记录里读回来（不重跑验证）
        let rec = workspace::load(root, &r.run_id).ok();
        let vrep = rec.as_ref().and_then(|x| x.verify.clone());
        let lint = rec.as_ref().and_then(|x| x.lint.clone());
        match evaluate(spec, &proj, vrep.as_ref(), lint.as_ref()) {
            Ok((checks, accept)) => {
                r.checks = checks;
                r.acceptance = accept;
                rechecked += 1;
            }
            Err(e) => {
                eprintln!("重算 {} 失败：{e}", r.task_id);
                r.ok = false;
                r.error = e;
            }
        }
    }
    report.summary = summarize(&report.runs, &report.arms.clone(), &report.tasks.clone());
    let mut notes = report.notes.clone();
    notes.push(format!(
        "本报告是**判据修正后重算**的结果（recheck）：重算了 {rechecked} 轮，         产物缺失跳过 {missing} 轮；**模型输出未变**，变的只有判据"
    ));
    report.notes = notes;
    recompute_fisher(&mut report);
    save_report(&report, out_path, md_path);
    println!("重算完成：{rechecked} 轮（模型输出未变）→ {out_path}");
    let _ = arms;
    0
}

/// 重算两两 Fisher（recheck 之后也要更新）
fn recompute_fisher(report: &mut EvalReport) {
    let mut fisher = Vec::new();
    if let Some(h) = report.summary.get("harness") {
        for (other, note) in [
            ("raw", "完整 harness vs 裸模型：差值就是 harness 的净效果"),
            ("harness_nokb", "关掉知识库注入后掉多少 = 知识库的贡献"),
            ("harness_norepair", "关掉自我纠正后掉多少 = 修复循环的贡献"),
        ] {
            if let Some(o) = report.summary.get(other) {
                if h.valid == 0 || o.valid == 0 {
                    continue;
                }
                let table = [
                    [h.accepted as u64, (h.valid - h.accepted) as u64],
                    [o.accepted as u64, (o.valid - o.accepted) as u64],
                ];
                fisher.push(FisherRow {
                    a: "harness".into(),
                    b: other.into(),
                    p_value: fisher_two_sided(table),
                    note: note.into(),
                });
            }
        }
    }
    report.fisher = fisher;
}

fn save_report(report: &EvalReport, out_path: &str, md_path: &str) {
    // 报告是这一趟评估的**唯一产物**：写不进去必须喊出来，不能静默丢掉。
    // （踩过：输出目录 D:/tmp 不存在，`let _ = fs::write(...)` 把整份报告吞了，
    //   命令行还照样打印"报告已写入 …"。）
    for (path, body) in [
        (
            out_path,
            serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".into()),
        ),
        (md_path, md_table(report)),
    ] {
        let p = Path::new(path);
        if let Some(dir) = p.parent()
            && !dir.as_os_str().is_empty()
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            eprintln!("✗ 建报告目录失败 {}: {e}", dir.display());
            continue;
        }
        if let Err(e) = std::fs::write(p, body) {
            eprintln!("✗ 写报告失败 {}: {e}", p.display());
        }
    }
}

/// 为评估任务建一份**独立**的知识库（走 `harness kb index` 同一条路：注册表 + index_source）。
fn build_kb(cfg: &AppConfig, src: &Path, dir: &Path) -> Result<(), String> {
    if !src.is_dir() {
        return Err(format!("知识库夹具目录不存在：{}", src.display()));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("建知识库目录失败: {e}"))?;
    // SAFETY：同上（单线程 CLI 路径）。
    unsafe { std::env::set_var("HARNESS_KB_DIR", dir) };

    let canon = kb::tidy_path(&std::fs::canonicalize(src).unwrap_or_else(|_| src.to_path_buf()));
    let entry = kb::KbEntry {
        id: kb::entry_id("local", &canon),
        kind: "local".into(),
        path: canon.to_string_lossy().to_string(),
        label: "eval".into(),
        added_at: workspace::now_iso(),
        ..Default::default()
    };
    let flag = exec::new_cancel_flag();
    let mut quiet = |_p: kb::index::Progress| {};
    let st = kb::index::index_source(dir, &entry, &cfg.kb, &flag, &mut quiet)?;
    let mut saved = entry.clone();
    saved.doc_count = st.doc_count;
    saved.chunk_count = st.chunk_count;
    saved.indexed_at = st.indexed_at.clone();
    saved.index_files = st.scanned - st.skipped_files.len();
    saved.skipped_files = st.skipped;
    saved.elapsed_ms = st.elapsed_ms;
    let mut reg = kb::Registry::load(dir).map_err(|e| format!("读注册表失败：{e}"))?;
    reg.upsert(saved, dir)?;
    unsafe { std::env::remove_var("HARNESS_KB_DIR") };
    Ok(())
}

// ============================================================
// 测试
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn task_spec() -> TaskSpec {
        TaskSpec {
            id: "t-test".into(),
            title: "测试用".into(),
            task: "写点东西".into(),
            kb: String::new(),
            seed: std::collections::BTreeMap::new(),
            files: vec!["hello.py".into()],
            require_regex: vec![r"return (True|False), ".into()],
            forbid_regex: vec![r"raise ValueError".into()],
            forbid_rules: vec!["HX201".into()],
            max_repair_rounds: None,
            min_match: std::collections::BTreeMap::new(),
            tests_must_pass: false,
            max_lint_errors: Some(0),
            require_layers: vec!["api".into()],
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dh-eval-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("api")).unwrap();
        d
    }

    /// 判据的正例：全部满足 → acceptance = true
    #[test]
    fn checks_pass_on_a_conforming_product() {
        let d = tmpdir("good");
        let mut f = std::fs::File::create(d.join("hello.py")).unwrap();
        writeln!(f, "def ok():\n    return True, 1, \"\"").unwrap();
        let mut g = std::fs::File::create(d.join("api/x.py")).unwrap();
        writeln!(g, "value = 1").unwrap();
        let report = lint::LintReport::default();
        let v = verify::VerifyReport::default();
        let (checks, accept) = evaluate(&task_spec(), &d, Some(&v), Some(&report)).unwrap();
        assert!(accept, "合规产物应当通过：{checks:?}");
        assert_eq!(checks.len(), 6, "6 条判据都要有结论");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 判据的反例：缺文件 / 出现禁止形态 / 缺目录 → 每条都要能定位到原因
    #[test]
    fn checks_fail_on_a_nonconforming_product() {
        let d = tmpdir("bad");
        let mut f = std::fs::File::create(d.join("hello.py")).unwrap();
        writeln!(f, "def bad():\n    raise ValueError('nope')").unwrap();
        let mut lint_report = lint::LintReport::default();
        lint_report.by_rule.insert("HX201".into(), 2);
        lint_report.counts.insert("error".into(), 2);
        let v = verify::VerifyReport::default();
        let (checks, accept) = evaluate(&task_spec(), &d, Some(&v), Some(&lint_report)).unwrap();
        assert!(!accept, "不合规产物必须判失败");
        let failed: Vec<String> = checks
            .iter()
            .filter(|c| !c.pass)
            .map(|c| format!("{}（{}）", c.name, c.detail))
            .collect();
        // 缺 api/ 是存在的（tmpdir 建了 api，所以这里应当是 4 条失败）
        assert!(failed.iter().any(|f| f.contains("禁止")), "{failed:?}");
        assert!(failed.iter().any(|f| f.contains("HX201")), "{failed:?}");
        assert!(failed.iter().any(|f| f.contains("形态")), "{failed:?}");
        assert!(
            failed.iter().any(|f| f.contains("规约 error")),
            "{failed:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 没有层目录时"目录存在"判据必须失败 —— v1 实验的教训（"没有向上边"不等于"分层对了"）
    #[test]
    fn missing_layer_is_caught() {
        let d = tmpdir("layer");
        let _ = std::fs::remove_dir_all(d.join("api"));
        writeln!(std::fs::File::create(d.join("hello.py")).unwrap(), "x = 1").unwrap();
        let spec = TaskSpec {
            require_regex: vec![],
            forbid_regex: vec![],
            forbid_rules: vec![],
            ..task_spec()
        };
        let (checks, accept) =
            evaluate(&spec, &d, Some(&verify::VerifyReport::default()), None).unwrap();
        assert!(!accept);
        assert!(checks.iter().any(|c| c.name.contains("api/") && !c.pass));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// "至少 N 处"判据：出现一次 ≠ 真落地
    #[test]
    fn min_match_requires_real_uptake() {
        let d = tmpdir("minmatch");
        writeln!(
            std::fs::File::create(d.join("hello.py")).unwrap(),
            "def a():
    return True, 1, \"\"

def b():
    return 1"
        )
        .unwrap();
        let mut spec = task_spec();
        spec.require_regex = vec![];
        spec.forbid_regex = vec![];
        spec.forbid_rules = vec![];
        spec.require_layers = vec![]; // 这条测的是 min_match，别让"层里没文件"搅进来
        spec.min_match.insert(r"return (True|False), ".into(), 2);
        let (checks, accept) =
            evaluate(&spec, &d, Some(&verify::VerifyReport::default()), None).unwrap();
        assert!(!accept, "只有 1 处三元组 return，2 处的要求不该通过");
        assert!(
            checks
                .iter()
                .any(|c| c.kind == "min_match" && c.detail.contains("实际 1 处")),
            "要能说出实际命中几次：{checks:?}"
        );
        spec.min_match.insert(r"return (True|False), ".into(), 1);
        let (checks2, accept2) =
            evaluate(&spec, &d, Some(&verify::VerifyReport::default()), None).unwrap();
        assert!(accept2, "降到 1 处就该过：{checks2:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn wilson_interval_is_sane() {
        let (lo, hi) = wilson(0, 10);
        assert!(lo <= 0.0001 && hi > 0.2, "{lo} {hi}");
        let (lo2, hi2) = wilson(10, 10);
        assert!(lo2 < 0.8 && hi2 >= 0.999, "{lo2} {hi2}");
        assert_eq!(wilson(0, 0), (0.0, 0.0));
    }

    #[test]
    fn fisher_matches_known_values() {
        // 10/10 vs 0/10：极端分裂，p 应该很小
        let p = fisher_two_sided([[10, 0], [0, 10]]);
        assert!(p < 1e-4, "p={p}");
        // 两臂完全一样 → p = 1
        let p2 = fisher_two_sided([[3, 7], [3, 7]]);
        assert!((p2 - 1.0).abs() < 1e-9, "p={p2}");
        // 3/10 vs 6/10 这种中等差异在小样本下不显著
        let p3 = fisher_two_sided([[3, 7], [6, 4]]);
        assert!(p3 > 0.05, "p={p3}");
    }

    #[test]
    fn raw_arm_parses_files_json() {
        let raw = r#"```json
{"files": [{"path": "a/b.py", "content": "x = 1\n"}]}
```"#;
        let files = parse_raw_files(raw).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "a/b.py");
        assert!(parse_raw_files("{\"files\": []}").is_err());
        assert!(parse_raw_files("完全不是 JSON").is_err());
    }

    #[test]
    fn suite_loads_and_every_task_has_machine_checks() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../eval/tasks");
        if !dir.is_dir() {
            return; // 没带任务集时跳过（例如打包分发）
        }
        let specs = load_suite(&dir).expect("任务集应当能加载");
        assert!(specs.len() >= 6, "至少要 6 个任务，实际 {}", specs.len());
        for s in &specs {
            assert!(!s.task.trim().is_empty(), "{} 没有任务描述", s.id);
            let n = s.files.len()
                + s.require_regex.len()
                + s.forbid_regex.len()
                + s.forbid_rules.len()
                + s.min_match.len()
                + s.require_layers.len()
                + usize::from(s.tests_must_pass);
            assert!(n >= 2, "{} 的判据太少（{n}），等于没判据", s.id);
            for pat in s
                .require_regex
                .iter()
                .chain(s.forbid_regex.iter())
                .chain(s.min_match.keys())
            {
                rx(pat).unwrap_or_else(|e| panic!("{} 的正则写错了：{e}", s.id));
            }
            // seed / kb 指向的文件必须在（否则要跑一次真任务才发现，白烧 token）
            let base = dir.parent().unwrap_or(&dir);
            for (rel, src) in &s.seed {
                assert!(
                    base.join(src).is_file(),
                    "{} 预置的 {} → {} 源文件不存在",
                    s.id,
                    rel,
                    src
                );
            }
            if !s.kb.trim().is_empty() {
                assert!(
                    base.join(&s.kb).is_dir(),
                    "{} 的知识库夹具目录不存在：{}",
                    s.id,
                    s.kb
                );
            }
        }
    }
}
