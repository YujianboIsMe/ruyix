//! RSI 评测台（v1.4 P0）：把「这版 harness 比上版强」变成**可复现的读数**。
//!
//! 为什么必须有它（`doc/v1.4/需求-递归自我改进-v1.4.md` §1.1）：所有采纳决定都要挂在一个
//! **外部可判定信号**上。"这段提示词读着更顺"不是信号；引擎现有 451 个单测证明的是**机制**
//! 成立，不是"这版比上版强"。所以先有尺子，再谈自改。
//!
//! 三件事，各自独立可验证：
//!
//! 1. **冻结考卷**：任务集（`tasks/<id>/`）在跑之前与每一轮之后都算 sha256；变了 ⇒ 本轮作废并
//!    指名被改的文件（判据 2）。任务集与判据脚本对 agent **只读**。
//! 2. **配对两臂**：同一任务集、同 temperature、同轮数，**一臂一个进程**（判据 3）。
//!    为什么要进程隔离：`discover::register_extra` 是**追加语义的进程级表** —— 两臂同进程会把
//!    A 的工具行留给 B 看，读数就不再是两臂之差。同 v1.2 的配对脚手架（两个臂各跑一遍）。
//! 3. **读数 → 裁决**：逐轮原始值全留；无效轮**剔除并列出**（不是当 0）；比较用**中位数**；
//!    差额 ≤ `MARGIN` ⇒ **拒绝**（判据 7：无信号不采纳，同 `scheduler::MARGIN` 的理由）。
//!
//! 臂（arm）= 一个**数据面覆盖目录**（`plugins/tools/<id>/tools.toml` + `bench.toml`），
//! 就是 v1.4 §4.2 拍板 (a) 的用户侧加载点的**同一种形状**：
//!
//! ```text
//! arm-baseline/
//!   plugins/tools/<id>/tools.toml   # [[discover]] 命令行（引擎侧真消费）
//!   bench.toml                      # 数值覆盖（L1）：[agent] / [discover] / [reflect] / [gate] …
//! ```
//!
//! **臂不许挑模型**：`[llm]` / `[llm_fallback]` 在 `bench.toml` 里出现即报错并指名文件 ——
//! 被测的东西不能自己选自己的工具，否则读数无从解释（同 v1.2 实验纪律）。
//!
//! 用法（脚本化臂，零成本，证明管道；仓库自带夹具任务集）：
//!
//! ```text
//! cargo run -p harness-engine --example rsi_bench -- --fake
//! ```
//!
//! 真模型臂（唯一能产出功效结论的形态；一个任务 × 一轮的手感先跑通再放大）：
//!
//! ```text
//! cargo run -p harness-engine --example rsi_bench -- \
//!     --tasks <便携根>/projects/<键>/rsi/tasks \
//!     --arm <...>/arms/baseline --arm <...>/arms/candidate \
//!     --config target/debug/global/ai.toml --model deepseek-v4-flash \
//!     --rounds 3 --out <便携根>/projects/<键>/rsi/arms
//! ```
//!
//! 产物：`<out>/raw-<臂>.json`（逐轮原始值）、`<out>/report.md`（人读表）、
//! `<out>/receipt.json`（本轮完整账：argv / 模型 / 温度 / 哈希 / 每条记录 / 裁决）。
//! **人合并**（v1.4 判据 3）：本工具只给读数与裁决，不写任何配置、不碰主干。

use harness_engine::agent::{self, HistoryMsg, NoConnector, WritePolicy};
use harness_engine::config::{self, AppConfig};
use harness_engine::discover;
use harness_engine::exec;
use harness_engine::pipeline::Sink;
use harness_engine::testllm::{fake_llm, tool_script};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 采纳门槛（百分点）：差额不够大就不合，理由同 `agent::scheduler::MARGIN`。
const MARGIN_DEFAULT_PP: f64 = 5.0;
/// 默认轮数（判据 3：配对、同轮数）。
const DEFAULT_ROUNDS: usize = 3;
/// 有效轮占比下限：低于它不出裁决（**不是**把无效轮当 0 或当满分）。
const MIN_VALID_RATIO: f64 = 0.6;

// ============================================
// 参数
// ============================================

#[derive(Debug, Default, Clone)]
struct Opts {
    tasks: Option<PathBuf>,
    out: Option<PathBuf>,
    arms: Vec<PathBuf>,
    rounds: usize,
    only_tasks: Vec<String>,
    fake: bool,
    model: Option<String>,
    temperature: Option<f32>,
    config: Option<PathBuf>,
    budget_secs: u64,
    margin: f64,
    // 子进程模式（内部用：一条臂的几个轮次）
    arm_dir: Option<PathBuf>,
    deadline_ms: u64,
    raw_out: Option<PathBuf>,
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rsi_bench_demo")
}

fn usage() -> String {
    "\
rsi_bench —— harness 自改闭环的评测台（v1.4 P0）

  --tasks <DIR>        任务集根（默认：仓库夹具 tests/fixtures/rsi_bench_demo/tasks）
  --arm <DIR>          臂的数据面覆盖目录（可重复；默认夹具的 baseline + candidate）
  --rounds <N>         轮数，两臂相同（默认 3）
  --task <ID>          只跑这些任务（可重复；默认全部）
  --fake               脚本化假 LLM（零成本、确定性；只证明管道，不构成功效结论）
  --config <FILE>      真模型臂：读 ai.toml 的 api_key / api_url / model（不打印密钥）
  --model <NAME>       覆盖模型名
  --temperature <F>    覆盖温度（两臂必须一致，所以它由驱动器给、不由臂给）
  --out <DIR>          读数落点（默认 target/rsi-bench）
  --budget-secs <N>    总预算硬顶（0 = 不限）；到点后剩余 run 记为 skipped_budget
  --margin <PP>        采纳门槛（默认 5.0 个百分点）
  --self-check         跑仪器的自检（夹具任务集 + 六条不变量），全过退出码 0
"
    .to_string()
}

fn parse_args() -> Result<Opts, String> {
    let mut o = Opts {
        rounds: DEFAULT_ROUNDS,
        margin: MARGIN_DEFAULT_PP,
        ..Default::default()
    };
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let key = argv[i].as_str();
        // 布尔开关（其余都要一个值）：漏在下面那步就变成"缺少取值"，自己踩过一次
        if key == "--fake" {
            o.fake = true;
            i += 1;
            continue;
        }
        if key == "--self-check" {
            self_check();
            std::process::exit(0);
        }
        if key == "-h" || key == "--help" {
            print!("{}", usage());
            std::process::exit(0);
        }
        let val = argv
            .get(i + 1)
            .ok_or_else(|| format!("{key} 缺少取值"))?
            .clone();
        i += 2;
        match key {
            "--tasks" => o.tasks = Some(PathBuf::from(val)),
            "--out" => o.out = Some(PathBuf::from(val)),
            "--arm" => o.arms.push(PathBuf::from(val)),
            "--task" => o.only_tasks.push(val),
            "--rounds" => o.rounds = val.parse().map_err(|e| format!("--rounds {val}: {e}"))?,
            "--model" => o.model = Some(val),
            "--temperature" => {
                o.temperature = Some(
                    val.parse()
                        .map_err(|e| format!("--temperature {val}: {e}"))?,
                )
            }
            "--config" => o.config = Some(PathBuf::from(val)),
            "--budget-secs" => {
                o.budget_secs = val
                    .parse()
                    .map_err(|e| format!("--budget-secs {val}: {e}"))?
            }
            "--margin" => o.margin = val.parse().map_err(|e| format!("--margin {val}: {e}"))?,
            // 子进程模式
            "--arm-dir" => o.arm_dir = Some(PathBuf::from(val)),
            "--deadline-ms" => {
                o.deadline_ms = val
                    .parse()
                    .map_err(|e| format!("--deadline-ms {val}: {e}"))?
            }
            "--raw-out" => o.raw_out = Some(PathBuf::from(val)),
            other => return Err(format!("未知参数 {other}\n\n{}", usage())),
        }
    }
    Ok(o)
}

// ============================================
// 任务集
// ============================================

#[derive(Debug, Clone, Deserialize)]
struct TaskFile {
    /// 交给 agent 的任务描述（原样进首条用户消息）
    prompt: String,
    #[serde(default)]
    notes: String,
    /// 分档（`smoke` / `full`）：小改跑子集、进 final 前跑全量的挂点（v1.4 §2 成本爆炸那条防线）
    #[serde(default)]
    tier: String,
    /// `pass`（默认）：判据必须通过；`fail` = **负样本**：判据必须**仍不通过**
    #[serde(default)]
    expect: Expect,
    judge: Judge,
    #[serde(default)]
    fake: Fake,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
enum Expect {
    /// 判据退出码 0 = 这一轮对
    #[default]
    Pass,
    /// 负样本：判据**失败**才算对（已知做不到的任务；改完仍须失败）
    Fail,
}

impl Expect {
    fn as_str(self) -> &'static str {
        match self {
            Expect::Pass => "pass",
            Expect::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Judge {
    /// 判据命令行（在**沙箱项目目录**里跑；退出码 0 = 通过）
    cmd: String,
    #[serde(default = "d_judge_timeout")]
    timeout_secs: u64,
}

fn d_judge_timeout() -> u64 {
    60
}

/// 脚本化臂的剧本：不用手写转义 JSON（那是真踩过的坑），用结构写、由 `tool_script` 组装。
#[derive(Debug, Clone, Default, Deserialize)]
struct Fake {
    #[serde(default)]
    turn: Vec<FakeTurn>,
    #[serde(default)]
    final_text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct FakeTurn {
    #[serde(default)]
    write: Vec<FakeWrite>,
    /// 原样剧本条目（高级用：直接给模型那轮的 JSON）
    #[serde(default)]
    raw: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct FakeWrite {
    path: String,
    content: String,
}

#[derive(Debug, Clone)]
struct Task {
    id: String,
    dir: PathBuf,
    spec: TaskFile,
}

fn load_tasks(root: &Path, only: &[String]) -> Result<Vec<Task>, String> {
    if !root.is_dir() {
        return Err(format!("任务集目录不存在：{}", root.display()));
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .map_err(|e| format!("读任务集 {} 失败：{e}", root.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    let mut out = Vec::new();
    for d in dirs {
        let id = d
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if !only.is_empty() && !only.contains(&id) {
            continue;
        }
        let f = d.join("task.toml");
        if !f.is_file() {
            continue;
        }
        let text =
            std::fs::read_to_string(&f).map_err(|e| format!("读 {} 失败：{e}", f.display()))?;
        let spec: TaskFile =
            toml::from_str(&text).map_err(|e| format!("{} 解析失败：{e}", f.display()))?;
        if spec.prompt.trim().is_empty() {
            return Err(format!("{}：prompt 不能为空", f.display()));
        }
        if spec.judge.cmd.trim().is_empty() {
            return Err(format!("{}：judge.cmd 不能为空", f.display()));
        }
        out.push(Task { id, dir: d, spec });
    }
    if out.is_empty() {
        return Err(format!(
            "{} 下没有可用任务（每个任务目录要有 task.toml）",
            root.display()
        ));
    }
    Ok(out)
}

/// 沙箱里那份夹具：签个指纹，证明两臂跑的是**同一份**起点。
fn seed_hash(task: &Task) -> String {
    let seed = task.dir.join("seed");
    let mut files = Vec::new();
    walk(&seed, &mut files);
    let mut acc = Sha256::new();
    for f in files {
        let rel = f
            .strip_prefix(&seed)
            .unwrap_or(&f)
            .to_string_lossy()
            .replace('\\', "/");
        let h = sha256_hex(&std::fs::read(&f).unwrap_or_default());
        acc.update(rel.as_bytes());
        acc.update(h.as_bytes());
    }
    hex_digest(acc.finalize())
}

// ============================================
// 指纹（判据 2：考卷不可改）
// ============================================

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(it) => it.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(_) => return,
    };
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex_digest(h.finalize())
}

/// 整棵树的指纹 + 逐文件指纹（变了要能**指名**是哪个文件）。
fn hash_tree(root: &Path) -> (String, BTreeMap<String, String>) {
    let mut files = Vec::new();
    walk(root, &mut files);
    let mut per = BTreeMap::new();
    let mut acc = Sha256::new();
    for f in files {
        let rel = f
            .strip_prefix(root)
            .unwrap_or(&f)
            .to_string_lossy()
            .replace('\\', "/");
        let h = sha256_hex(&std::fs::read(&f).unwrap_or_default());
        acc.update(rel.as_bytes());
        acc.update(b"|");
        acc.update(h.as_bytes());
        acc.update(b"\n");
        per.insert(rel, h);
    }
    (hex_digest(acc.finalize()), per)
}

/// 两次快照里变了的文件（判据 2 的留痕）。
fn changed_files(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut v = Vec::new();
    for (k, hv) in after {
        match before.get(k) {
            Some(h0) if h0 == hv => {}
            Some(_) => v.push(format!("改动 {k}")),
            None => v.push(format!("新增 {k}")),
        }
    }
    for k in before.keys() {
        if !after.contains_key(k) {
            v.push(format!("删除 {k}"));
        }
    }
    v
}

// ============================================
// 臂：数据面覆盖
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ArmInfo {
    name: String,
    dir: String,
    /// 真登记进 `discover` 的行数
    discover_rows: usize,
    /// 读了但引擎**不消费**的包管理器行（只对宿主生效；如实记着，不假装它影响了读数）
    pm_rows: usize,
    /// `bench.toml` 覆盖了哪几个顶层表
    overlay_tables: Vec<String>,
    files: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DiscoverRow {
    name: String,
    bin: String,
    #[serde(default)]
    version_args: Vec<String>,
    #[serde(default)]
    markers: Vec<String>,
    #[serde(default)]
    extensions: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ToolsFile {
    #[serde(default)]
    discover: Vec<DiscoverRow>,
    #[serde(default)]
    pm: Vec<toml::Value>,
}

/// 加载一条臂：登记 discover 行 + 记下 `bench.toml` 覆盖（**先问、后合**）。
fn load_arm(dir: &Path) -> Result<(ArmInfo, Option<toml::Value>), String> {
    if !dir.is_dir() {
        return Err(format!("臂目录不存在：{}", dir.display()));
    }
    let name = dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "arm".into());
    let plugins = dir.join("plugins/tools");
    let mut rows = Vec::new();
    let mut pm_rows = 0usize;
    let mut files = Vec::new();
    if plugins.is_dir() {
        let mut subs: Vec<PathBuf> = std::fs::read_dir(&plugins)
            .map_err(|e| format!("读 {} 失败：{e}", plugins.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        subs.sort();
        for sub in subs {
            let f = sub.join("tools.toml");
            if !f.is_file() {
                continue;
            }
            let text =
                std::fs::read_to_string(&f).map_err(|e| format!("读 {} 失败：{e}", f.display()))?;
            let parsed: ToolsFile =
                toml::from_str(&text).map_err(|e| format!("{} 解析失败：{e}", f.display()))?;
            pm_rows += parsed.pm.len();
            for r in parsed.discover {
                if r.name.trim().is_empty() || r.bin.trim().is_empty() {
                    return Err(format!("{}：[[discover]] 行缺 name / bin", f.display()));
                }
                rows.push(discover::ToolSpec {
                    name: discover::leak_str(r.name),
                    bin: discover::leak_str(r.bin),
                    version_args: discover::leak_strs(r.version_args),
                    markers: discover::leak_strs(r.markers),
                    extensions: discover::leak_strs(r.extensions),
                });
            }
            files.push(f.display().to_string());
        }
    }
    let discover_rows = discover::register_extra(rows);

    // 数值覆盖：**先**做白名单检查，再谈合并 —— 顺序反了就等于先污染后检查
    let overlay = dir.join("bench.toml");
    let overlay = if overlay.is_file() {
        let text = std::fs::read_to_string(&overlay)
            .map_err(|e| format!("读 {} 失败：{e}", overlay.display()))?;
        let v: toml::Value =
            toml::from_str(&text).map_err(|e| format!("{} 解析失败：{e}", overlay.display()))?;
        for banned in ["llm", "llm_fallback"] {
            if v.get(banned).is_some() {
                return Err(format!(
                    "{}：臂不许覆盖 [{banned}] —— 被测的东西不能自己挑模型 / 端点，否则读数无从解释",
                    overlay.display()
                ));
            }
        }
        files.push(overlay.display().to_string());
        Some(v)
    } else {
        None
    };
    let overlay_tables = overlay
        .as_ref()
        .and_then(|v| v.as_table().map(|t| t.keys().cloned().collect()))
        .unwrap_or_default();
    println!(
        "  臂 {name}：discover 行 {discover_rows}（真登记）· pm 行 {pm_rows}（引擎不消费）· bench.toml 覆盖顶层表 {overlay_tables:?}"
    );

    Ok((
        ArmInfo {
            name,
            dir: dir.display().to_string(),
            discover_rows,
            pm_rows,
            overlay_tables,
            files,
        },
        overlay,
    ))
}

fn deep_merge(a: &mut serde_json::Value, b: &serde_json::Value) {
    if let (Some(at), Some(bt)) = (a.as_object_mut(), b.as_object()) {
        for (k, v) in bt {
            match at.get_mut(k) {
                Some(av) if av.is_object() && v.is_object() => deep_merge(av, v),
                _ => {
                    at.insert(k.clone(), v.clone());
                }
            }
        }
    }
}

/// 生效配置的指纹（**脱敏**）：证明"这条臂的覆盖真落进了配置"，同时不把密钥写进读数。
fn config_digest(cfg: &AppConfig) -> String {
    let mut v = serde_json::to_value(cfg).unwrap_or(serde_json::Value::Null);
    if let Some(l) = v.get_mut("llm").and_then(|x| x.as_object_mut()) {
        l.insert("api_key".into(), serde_json::Value::String("***".into()));
    }
    sha256_hex(v.to_string().as_bytes())
}

/// 基线配置 →（臂覆盖）→ 生效配置。走 `serde_json::Value` 中转而**不是** `toml::Value`：
/// `Option` 的 `None` 在 TOML 里没有对应表示，整体序列化会翻车；JSON 两种形状都装得下。
///
/// **覆盖非空但一个键都没生效 ⇒ 报错**。这不是洁癖：拼错一个键的效果与"这条自改没有任何
/// 效果"在读数上无法区分（都是"无信号 ⇒ 拒绝"），而真实的病是**它根本没跑** —— 那是最坏的
/// 一种失败（看起来像被门槛拦了，其实压根没试）。
fn merge_config(
    base: &AppConfig,
    overlay: Option<&toml::Value>,
    who: &str,
) -> Result<AppConfig, String> {
    let Some(ov) = overlay else {
        return Ok(base.clone());
    };
    let b = serde_json::to_value(base).map_err(|e| format!("基线配置序列化失败：{e}"))?;
    let mut merged = b.clone();
    let ovj = serde_json::to_value(ov).map_err(|e| format!("{who} 的覆盖转 JSON 失败：{e}"))?;
    deep_merge(&mut merged, &ovj);
    let cfg: AppConfig =
        serde_json::from_value(merged).map_err(|e| format!("合并配置失败：{e}"))?;
    if serde_json::to_value(&cfg).unwrap_or(serde_json::Value::Null) == b {
        return Err(format!(
            "{who}：覆盖非空，但生效配置与基线**逐字节相同** —— 要么键拼错了，要么这条臂什么都没改。\
             两种情况都必须先修好，再谈读数"
        ));
    }
    Ok(cfg)
}

/// 读配置节的扁平键（宿主 `ai.toml` 的形状：`api_key` / `api_url` / `model` / `api_format`）。
/// **只打印状态，不打印密钥**。
fn apply_config_file(cfg: &mut AppConfig, path: &Path) -> Result<Vec<String>, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("读 {} 失败：{e}", path.display()))?;
    let v: toml::Value =
        toml::from_str(&text).map_err(|e| format!("{} 解析失败：{e}", path.display()))?;
    // 宿主的三 scope 配置是**分节文件**（`[ai]` / `[harness]` / `[ui]`）+ 扁平键；
    // 引擎侧 `AppConfig` 是嵌套结构。这里读前者、映射到后者 —— 键名也不同
    // （宿主的 `api_url` 在引擎里叫 `base_url`）。
    let mut set = Vec::new();
    let sec = v.get("ai").unwrap_or(&v);
    let get = |k: &str| sec.get(k).and_then(|x| x.as_str()).map(|s| s.to_string());
    if let Some(k) = get("api_key") {
        cfg.llm.api_key = k;
        set.push("api_key=已配置".to_string());
    }
    if let Some(u) = get("api_url").or_else(|| get("base_url")) {
        cfg.llm.base_url = u;
        set.push(format!("base_url={}", cfg.llm.base_url));
    }
    if let Some(m) = get("model") {
        cfg.llm.model = m;
        set.push(format!("model={}", cfg.llm.model));
    }
    if let Some(f) = get("api_format") {
        cfg.llm.api_format = f;
        set.push(format!("api_format={}", cfg.llm.api_format));
    }
    Ok(set)
}

// ============================================
// 读数
// ============================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Rec {
    task: String,
    arm: String,
    round: usize,
    /// ok（判据与 expect 相符）| wrong（不符，含负样本被"做成了"）| invalid（本轮无效）| skipped_budget
    kind: String,
    detail: String,
    expect: String,
    agent_ok: bool,
    agent_error: String,
    judge_cmd: String,
    judge_exit: Option<i32>,
    judge_output: String,
    seed_hash: String,
    elapsed_ms: u128,
    tokens: u64,
    cache_read: Option<u64>,
    changed: Vec<String>,
    verifications: Vec<String>,
    answer_head: String,
    project: String,
}

impl Rec {
    fn invalid(task: &Task, arm: &str, round: usize, detail: impl Into<String>) -> Rec {
        Rec {
            task: task.id.clone(),
            arm: arm.to_string(),
            round,
            kind: "invalid".into(),
            detail: detail.into(),
            expect: task.spec.expect.as_str().into(),
            agent_ok: false,
            agent_error: String::new(),
            judge_cmd: task.spec.judge.cmd.clone(),
            judge_exit: None,
            judge_output: String::new(),
            seed_hash: seed_hash(task),
            elapsed_ms: 0,
            tokens: 0,
            cache_read: None,
            changed: Vec::new(),
            verifications: Vec::new(),
            answer_head: String::new(),
            project: String::new(),
        }
    }
}

/// 收日志的 Sink：跑真循环时把事件攒下来（调试失败轮时唯一的现场）。
#[derive(Default)]
struct QuietSink {
    lines: Mutex<Vec<String>>,
}

impl Sink for QuietSink {
    fn log(&self, level: &str, msg: String) {
        if let Ok(mut g) = self.lines.lock()
            && g.len() < 200
        {
            g.push(format!("[{level}] {msg}"));
        }
    }
}

fn fake_script(t: &Task) -> Vec<String> {
    let mut v = Vec::new();
    for turn in &t.spec.fake.turn {
        if let Some(raw) = &turn.raw {
            v.push(raw.clone());
            continue;
        }
        let calls: Vec<(&str, serde_json::Value)> = turn
            .write
            .iter()
            .map(|w| {
                (
                    "write",
                    serde_json::json!({ "path": w.path, "content": w.content }),
                )
            })
            .collect();
        if !calls.is_empty() {
            v.push(tool_script(&calls));
        }
    }
    let msg = t
        .spec
        .fake
        .final_text
        .clone()
        .unwrap_or_else(|| "完成".to_string());
    v.push(serde_json::json!({ "final": msg }).to_string());
    v
}

struct RunCtx<'a> {
    cfg: &'a AppConfig,
    task: &'a Task,
    arm: &'a str,
    round: usize,
    dir: PathBuf,
    /// 脚本化臂：不联网，用 `testllm` 的假 LLM 跑**同一个**真循环
    fake: bool,
}

/// 工具循环这一轮的结论与证据（判据要看的东西 + 出错时的现场）。
#[derive(Default)]
struct RunResult {
    ok: bool,
    error: String,
    tokens: u64,
    cache_read: Option<u64>,
    changed: Vec<String>,
    verifications: Vec<String>,
    answer: String,
}

// 跑一轮：先真跑工具循环，再在**它会话的项目目录**里跑判据。
fn run_once(ctx: &RunCtx) -> Rec {
    let RunCtx {
        cfg,
        task,
        arm,
        round,
        dir,
        fake,
    } = ctx;
    let project = dir.join("project");
    let state = dir.join("state");
    let work = dir.join("work");
    for d in [&project, &state, &work] {
        if let Err(e) = std::fs::create_dir_all(d) {
            return Rec::invalid(task, arm, *round, format!("建沙箱目录失败：{e}"));
        }
    }
    // 夹具照原样拷进沙箱（两臂同一份起点；指纹写进记录，可复核）
    if let Err(e) = copy_tree(&task.dir.join("seed"), &project) {
        return Rec::invalid(task, arm, *round, format!("铺夹具失败：{e}"));
    }
    // 判据脚本拷到 **run 目录**（不是沙箱项目里）：判据是考卷，agent 连看都不该看到它 ——
    // 这比"只读"更强，也让 P1 的"考卷在 agent 白名单之外"在 P0 就已经成立。
    // 命令里用 `{judge}` 引用（cwd 仍是项目目录：判据看到的必须是 agent 写出来的东西）。
    if let Err(e) = copy_tree(&task.dir.join("judge"), &dir.join("judge")) {
        return Rec::invalid(task, arm, *round, format!("铺判据失败：{e}"));
    }
    let sh = seed_hash(task);
    let mut eff = AppConfig::clone(cfg);
    eff.project_state_root = state.display().to_string();
    eff.workspace_root = work.display().to_string();

    let sink = QuietSink::default();
    let cancel = exec::new_cancel_flag();
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return Rec::invalid(task, arm, *round, format!("建 tokio runtime 失败：{e}")),
    };

    let started = Instant::now();
    if *fake {
        let r = run_fake(&rt, &eff, task, &project, &sink);
        return judge(ctx, &project, &sh, r, &sink, started);
    }

    let mut r = RunResult::default();
    match rt.block_on(agent::run(
        &eff,
        &project,
        &task.spec.prompt,
        &[HistoryMsg {
            role: "user".into(),
            text: task.spec.prompt.clone(),
        }],
        WritePolicy::Apply,
        &NoConnector,
        &cancel,
        &sink,
    )) {
        Ok(o) => {
            r.ok = true;
            r.tokens = o.usage.total_tokens;
            r.cache_read = o.usage.cache_read();
            r.changed = o.changes.iter().map(|c| c.path.clone()).collect();
            r.verifications = o
                .verifications
                .iter()
                .map(|v| format!("{}:{}", v.layer, v.status))
                .collect();
            r.answer = head(&o.answer, 400);
        }
        Err(e) => r.error = head(&e, 400),
    }
    judge(ctx, &project, &sh, r, &sink, started)
}

#[allow(clippy::too_many_arguments)]
/// 判据命令的占位符展开：`{judge}` = 判据脚本目录（run 目录下、项目之外）、`{project}` = 沙箱项目。
fn expand_judge_cmd(cmd: &str, ctx: &RunCtx, project: &Path) -> String {
    cmd.replace("{judge}", &ctx.dir.join("judge").display().to_string())
        .replace("{project}", &project.display().to_string())
}

fn judge(
    ctx: &RunCtx,
    project: &Path,
    sh: &str,
    r: RunResult,
    sink: &QuietSink,
    started: Instant,
) -> Rec {
    let RunCtx {
        task, arm, round, ..
    } = ctx;
    // 模型自己报的"做不到"不算数：判据独立跑一次（判据在**沙箱项目目录**里跑，看的是它写出来的东西）
    let cmd = expand_judge_cmd(&task.spec.judge.cmd, ctx, project);
    let j = exec::run_line(
        project,
        &cmd,
        Duration::from_secs(task.spec.judge.timeout_secs),
    );
    // 事故现场落盘：失败轮的日志是唯一证据，只留在内存里等于丢掉（`QuietSink` 只收前 200 行）
    let logs = sink.lines.lock().map(|g| g.join("\n")).unwrap_or_default();
    let log_file = ctx.dir.join("agent.log");
    let _ = std::fs::write(&log_file, &logs);
    let base = |kind: String, detail: String| Rec {
        task: task.id.clone(),
        arm: arm.to_string(),
        round: *round,
        kind,
        detail,
        expect: task.spec.expect.as_str().into(),
        agent_ok: r.ok,
        agent_error: r.error.clone(),
        judge_cmd: cmd.clone(),
        judge_exit: j.exit_code,
        judge_output: head(&format!("{}\n{}", j.stdout, j.stderr), 600),
        seed_hash: sh.to_string(),
        elapsed_ms: started.elapsed().as_millis(),
        tokens: r.tokens,
        cache_read: r.cache_read,
        changed: r.changed.clone(),
        verifications: r.verifications.clone(),
        answer_head: r.answer.clone(),
        project: project.display().to_string(),
    };
    if j.timed_out {
        return base(
            "invalid".into(),
            format!(
                "判据超时（{}s）—— 这一轮没有读数",
                task.spec.judge.timeout_secs
            ),
        );
    }
    if let Some(e) = &j.spawn_error {
        return base(
            "invalid".into(),
            format!("判据起不来（{e}）—— 这一轮没有读数"),
        );
    }
    if !r.ok {
        return base(
            "invalid".into(),
            format!(
                "工具循环报错：{}（判据这次读到：{}；现场 {}）",
                head(&r.error, 300),
                head(&format!("{}\n{}", j.stdout, j.stderr), 200),
                log_file.display()
            ),
        );
    }
    // 判据退出码契约：**0 = 通过，1 = 不通过，其它 = 判据自己坏了**。
    //
    // 为什么要有这条：判据写错路径时 python 回 2，那一轮就被记成"候选更差" —— 坏掉的尺子
    // 冒充测量结果，是最坏的一种失败（真踩过：`python judge/check.py` 在项目目录里跑，
    // 而判据脚本在任务目录里，回 exit 2）。尺子坏了必须报"没有读数"。
    let passed = match j.exit_code {
        Some(0) => true,
        Some(1) => false,
        other => {
            return base(
                "invalid".into(),
                format!(
                    "判据退出码 {other:?}（契约：0=通过 / 1=不通过）⇒ 判据自己坏了，这一轮没有读数：{}",
                    head(&format!("{}\n{}", j.stdout, j.stderr), 300)
                ),
            );
        }
    };
    let (kind, detail) = match (task.spec.expect, passed) {
        (Expect::Pass, true) => ("ok".to_string(), "判据通过".to_string()),
        (Expect::Pass, false) => (
            "wrong".to_string(),
            "判据不通过（exit 1）".to_string(),
        ),
        // 负样本：**判据仍须失败**。"做成了"= 奖励劫持或任务本身不成立，两件都得人看
        (Expect::Fail, false) => (
            "ok".to_string(),
            "负样本：判据如预期不通过（exit 1）".to_string(),
        ),
        (Expect::Fail, true) => (
            "wrong".to_string(),
            "负样本被判据通过了 —— 要么绕过了判据（奖励劫持），要么这个任务其实做得到，两种都必须人看"
                .to_string(),
        ),
    };
    base(kind, detail)
}

#[allow(clippy::too_many_arguments)]
fn run_fake(
    rt: &tokio::runtime::Runtime,
    eff: &AppConfig,
    task: &Task,
    project: &Path,
    sink: &QuietSink,
) -> RunResult {
    let llm = fake_llm(fake_script(task));
    let mut cfg2 = eff.clone();
    cfg2.llm.base_url = llm.base_url.clone();
    cfg2.llm.api_key = "bench-fake".into();
    let out = rt.block_on(agent::run(
        &cfg2,
        project,
        &task.spec.prompt,
        &[HistoryMsg {
            role: "user".into(),
            text: task.spec.prompt.clone(),
        }],
        WritePolicy::Apply,
        &NoConnector,
        &exec::new_cancel_flag(),
        sink,
    ));
    match out {
        Ok(o) => RunResult {
            ok: true,
            tokens: o.usage.total_tokens,
            cache_read: o.usage.cache_read(),
            changed: o.changes.iter().map(|c| c.path.clone()).collect(),
            verifications: o
                .verifications
                .iter()
                .map(|v| format!("{}:{}", v.layer, v.status))
                .collect(),
            answer: head(&o.answer, 400),
            ..RunResult::default()
        },
        Err(e) => RunResult {
            error: head(&e, 400),
            ..RunResult::default()
        },
    }
}

/// **跑之前**就拒掉过深的落点（Windows 的 MAX_PATH 陷阱）。
///
/// **这条闸的理由在 v1.4 A-5 之后变了**（2026-10-08），如实记着：
///
/// - 当时（A-5 之前）：`verify` 的 python 语法检查用 `py_compile` 写字节码，并靠
///   `PYTHONPYCACHEPREFIX` 把 `__pycache__` 改道到状态桶；CPython 会把**源码的绝对路径
///   镜像**到那个前缀底下 ⇒ pyc 真实路径 ≈ 2×落点深度 + 常数，越过 260 就 `[WinError 206]`
///   退出 1，被读成「语法/编译错误」并**打回 final**：一次写对了的改动被判成坏代码，
///   读数里只留下"这版更差"（保留现场见 `doc/v1.4/问题-验证pycache改道撞MAX_PATH.md`）。
/// - 现在：语法检查换成了不写盘的 `ast.parse`，越限还会如实报「环境不支持」
///   （`verify::py_env_guard`）⇒ 那条假红链在**因果上**被切断。
///
/// 所以这条闸不是补丁，是**兜底**：落点太深时，判据自己跑 python（pytest / 判据脚本会 import
/// 被测模块、在项目里写 `__pycache__`）仍可能撞 260 —— 与其把一整轮烧在一个注定失败的环境上，
/// 不如**跑之前**拒绝并说清出路。
fn guard_long_paths(out: &Path, run_ids: &[String], max_file: usize) -> Result<String, String> {
    const LIMIT: usize = 250; // 260 是硬限，留 10 字符余量
    // **必须按绝对路径估**：相对落点会被解析两次（见 `abs_out`），估出来的数会小一大截，
    // 闸就形同虚设 —— 这是这台仪器自己踩过的第二个坑。
    let abs = std::path::absolute(out).unwrap_or_else(|_| out.to_path_buf());
    let base = norm_path(&abs).chars().count();
    let rid = run_ids
        .iter()
        .map(|s| s.chars().count())
        .max()
        .unwrap_or(16);
    // 最长的落点 = **项目里的**文件 + 它旁边的 `__pycache__/<名字>.cpython-3XX.pyc`
    // （`<out>/runs/<run_id>/project/<文件>`）：判据脚本与 agent 自己的 python 都会 import 被测
    // 模块；而**验证管线自己一个字节都不写**（`PYTHONDONTWRITEBYTECODE=1`，见 verify.rs）。
    //
    // 这里原先还要加一项"`<out>/runs/<run_id>/state/verify/pycache` + 镜像的源码绝对路径"
    // —— 那是 `PYTHONPYCACHEPREFIX` 的**镜像**行为（pyc 路径 ≈ 2×落点深度），A-5 之后那条路
    // 已经不写了。留着它的代价是实测的：真实便携根的
    // `D:\Tools\ruyix\projects\D-Projects-Rust-ruyix\rsi\arms` 被估成 **259 > 250 而拒跑**
    // —— 键名长一点就命中，可那个世界其实跑得动。**判据只有拒绝方向 = 越保守越绿**，
    // 所以自检第 7 条现在两个方向都钉。
    let src = base + 6 + rid + "/project/".len() + max_file;
    let pyc = src + 2 + 31; // `__pycache__/x.cpython-311.pyc`
    if pyc > LIMIT {
        return Err(format!(
            "落点太深：估算最长文件路径 {pyc} 字符（上限 {LIMIT}）—— 越过 Windows 的 260 会让 python \
             报 WinError 206。把 `--out` 指到更短的地方（便携根下 `projects/<键>/rsi/arms` 够用；\
             项目键很长时它也会变紧）。现在：{}",
            out.display()
        ));
    }
    // 这句会进读数的"生效条件/闸"那一栏，所以措辞跟上事实：算的是**判据/agent 的 import**
    // 会在项目里写下的那个 pyc 路径（验证管线自己已经不写了）。
    Ok(format!(
        "最长文件路径估算 {pyc}/{LIMIT} 字符（验证管线不写字节码；判据/agent 的 import 仍可能在项目里原处写 __pycache__）"
    ))
}

/// 词法折叠路径里的 `.` / `..`（只为**显示**好看：读数里的路径是证据，不该长成 `x/../y`）。
/// 不动真正的文件系统语义（不解析符号链接）—— 这里只用它打印。
fn norm_path(p: &Path) -> String {
    let mut out: Vec<String> = Vec::new();
    let s = p.display().to_string().replace('\\', "/");
    let abs = s.starts_with('/') || s.chars().nth(1) == Some(':');
    for part in s.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if out.len() > 1 || (out.len() == 1 && !abs) {
                    out.pop();
                } else {
                    out.push("..".into());
                }
            }
            x => out.push(x.to_string()),
        }
    }
    let joined = out.join("/");
    // 已经是带盘符的绝对路径（`D:/x`）就不要再补一个前导 `/` —— 否则打印出来是 `/D:/x`
    if abs && !joined.contains(':') {
        format!("/{joined}")
    } else {
        joined
    }
}

/// 夹具里最长的**文件名**长度（路径闸要估最坏情况；判据脚本名也算在内）。
fn longest_seed_file(tasks: &[Task]) -> usize {
    let mut max = 0usize;
    for t in tasks {
        let mut f = Vec::new();
        walk(&t.dir.join("seed"), &mut f);
        for p in f {
            max = max.max(
                p.file_name()
                    .map(|s| s.to_string_lossy().chars().count())
                    .unwrap_or(0),
            );
        }
        max = max.max("test_calc.py".len());
    }
    max.max(16)
}

fn head(s: &str, n: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= n {
        return t.to_string();
    }
    format!("{}…", t.chars().take(n).collect::<String>())
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    if !from.is_dir() {
        return Ok(());
    }
    let mut files = Vec::new();
    walk(from, &mut files);
    for f in files {
        let rel = f.strip_prefix(from).unwrap_or(&f);
        let dst = to.join(rel);
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p).map_err(|e| format!("建 {} 失败：{e}", p.display()))?;
        }
        std::fs::copy(&f, &dst).map_err(|e| format!("拷 {} 失败：{e}", f.display()))?;
    }
    Ok(())
}

// ============================================
// 子进程：一条臂的全部轮次
// ============================================

fn run_arm_process(o: &Opts) -> Result<(), String> {
    let dir = o.arm_dir.as_ref().ok_or("子进程模式缺 --arm-dir")?.clone();
    let raw_out = o.raw_out.as_ref().ok_or("子进程模式缺 --raw-out")?.clone();
    let (info, overlay) = load_arm(&dir)?;
    let tasks_root = o.tasks.clone().ok_or("子进程模式缺 --tasks")?;
    let out = abs_out(&o.out.clone().ok_or("子进程模式缺 --out")?);
    let tasks = load_tasks(&tasks_root, &o.only_tasks)?;
    let base = base_config(o)?;
    let who = format!("臂 {} 的 bench.toml", info.name);
    let cfg = merge_config(&base, overlay.as_ref(), &who)?;
    let cfg_hash = config_digest(&cfg);
    if !o.fake && cfg.llm.api_key.trim().is_empty() {
        return Err("没有 LLM Key（--fake 或 --config / DEEPSEEK_API_KEY）".into());
    }
    let deadline = if o.deadline_ms == 0 {
        None
    } else {
        Some(UNIX_EPOCH + Duration::from_millis(o.deadline_ms))
    };

    let arm_name = info.name.clone();
    let run_ids: Vec<String> = tasks
        .iter()
        .flat_map(|t| {
            let id = t.id.clone();
            let an = arm_name.clone();
            (1..=o.rounds).map(move |r| format!("{id}__{an}__r{r}"))
        })
        .collect();
    println!(
        "路径闸：{}",
        guard_long_paths(&out, &run_ids, longest_seed_file(&tasks))?
    );

    let mut recs: Vec<Rec> = Vec::new();
    let (h0, _) = hash_tree(&tasks_root);
    'rounds: for round in 1..=o.rounds {
        let (h, _) = hash_tree(&tasks_root);
        if h != h0 {
            for t in &tasks {
                let mut r = Rec::invalid(t, &info.name, round, "考卷在本轮内变过 ⇒ 本轮作废");
                r.detail = format!("任务集指纹 {} → {}", head(&h0, 12), head(&h, 12));
                recs.push(r);
            }
            break 'rounds;
        }
        for t in &tasks {
            if let Some(d) = deadline
                && SystemTime::now() >= d
            {
                recs.push(Rec {
                    kind: "skipped_budget".into(),
                    detail: "预算到点，剩下没跑的记在这里（**不当 0**）".into(),
                    ..Rec::invalid(t, &info.name, round, String::new())
                });
                println!("  [r{round}] {:<12} 跳过（预算到点）", t.id);
                continue;
            }
            let dir = out
                .join("runs")
                .join(format!("{}__{}__r{}", t.id, info.name, round));
            let _ = std::fs::remove_dir_all(&dir);
            let started = Instant::now();
            let ctx = RunCtx {
                cfg: &cfg,
                task: t,
                arm: &info.name,
                round,
                dir: dir.clone(),
                fake: o.fake,
            };
            let r = run_once(&ctx);
            println!(
                "  [r{round}] {:<12} {:<6} {:.1}s  {}",
                t.id,
                r.kind,
                started.elapsed().as_secs_f64(),
                head(&r.detail, 90)
            );
            recs.push(r);
        }
    }
    let (h1, _) = hash_tree(&tasks_root);
    if h1 != h0 {
        println!("!! 考卷在本轮内变过（判据 2）：本轮读数作废");
    }
    let payload = ArmRun {
        info,
        cfg_hash,
        records: recs,
    };
    let json = serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?;
    if let Some(p) = raw_out.parent() {
        std::fs::create_dir_all(p).map_err(|e| format!("建 {} 失败：{e}", p.display()))?;
    }
    std::fs::write(&raw_out, json).map_err(|e| format!("写 {} 失败：{e}", raw_out.display()))?;
    println!(
        "臂 {} 完成：{} 条记录（生效配置指纹 {}）→ {}",
        payload.info.name,
        payload.records.len(),
        head(&payload.cfg_hash, 12),
        raw_out.display()
    );
    Ok(())
}

fn base_config(o: &Opts) -> Result<AppConfig, String> {
    let mut cfg = AppConfig::default();
    // 评测台的确定性底座（冒烟同款）：不碰 Docker、不依赖 tools/lint；**臂可以覆盖它们**，
    // 因为有意义的候选就可能长在这些面上（例如"开复核 agent 是否更稳"）。
    cfg.sandbox.mode = "off".into();
    cfg.lint.enabled = false;
    cfg.reflect.enabled = false;
    config::apply_env_overrides(&mut cfg);
    if let Some(p) = &o.config {
        let set = apply_config_file(&mut cfg, p)?;
        println!("配置 {}：{}", p.display(), set.join(" · "));
    }
    if let Some(m) = &o.model {
        cfg.llm.model = m.clone();
    }
    if let Some(t) = o.temperature {
        cfg.llm.temperature = t;
    }
    Ok(cfg)
}

// ============================================
// 驱动：配对两臂 → 裁决
// ============================================

/// 一条臂的子进程产出：臂信息 + 生效配置指纹 + 逐轮原始记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ArmRun {
    info: ArmInfo,
    /// 生效配置（**脱敏**）的 sha256 —— 证明覆盖真的落进了配置，且不泄露密钥
    cfg_hash: String,
    records: Vec<Rec>,
}

#[derive(Debug, Serialize)]
struct ArmStat {
    arm: String,
    info: ArmInfo,
    cfg_hash: String,
    /// 逐轮得分（有效轮上的通过率，%）
    round_scores: Vec<f64>,
    median: Option<f64>,
    valid: usize,
    ok: usize,
    wrong: usize,
    invalid: usize,
    skipped: usize,
    /// 逐任务：任务 → (ok, 有效数, 有效轮原始值)
    per_task: BTreeMap<String, Vec<String>>,
    records: Vec<Rec>,
}

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

fn stat_arm(run: ArmRun, rounds: usize) -> ArmStat {
    let ArmRun {
        info,
        cfg_hash,
        records,
    } = run;
    let is_valid = |k: &str| k == "ok" || k == "wrong";
    let valid = records.iter().filter(|r| is_valid(&r.kind)).count();
    let ok = records.iter().filter(|r| r.kind == "ok").count();
    let wrong = records.iter().filter(|r| r.kind == "wrong").count();
    let invalid = records.iter().filter(|r| r.kind == "invalid").count();
    let skipped = records
        .iter()
        .filter(|r| r.kind == "skipped_budget")
        .count();
    let mut round_scores = Vec::new();
    for round in 1..=rounds {
        let rs: Vec<&Rec> = records
            .iter()
            .filter(|r| r.round == round && is_valid(&r.kind))
            .collect();
        if rs.is_empty() {
            continue;
        }
        let oks = rs.iter().filter(|r| r.kind == "ok").count();
        round_scores.push(oks as f64 * 100.0 / rs.len() as f64);
    }
    let mut per_task: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in &records {
        per_task
            .entry(r.task.clone())
            .or_default()
            .push(format!("r{}/{}:{}", r.round, r.task, r.kind));
    }
    let mut rs = round_scores.clone();
    let med = median(&mut rs);
    ArmStat {
        arm: info.name.clone(),
        info,
        cfg_hash,
        round_scores,
        median: med,
        valid,
        ok,
        wrong,
        invalid,
        skipped,
        per_task,
        records,
    }
}

fn tick() -> String {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", d.as_secs())
}

/// 落点绝对化。
///
/// 为什么不能将就相对路径：引擎的 `project_state_root` 会被 `verify` 拼成
/// `PYTHONPYCACHEPREFIX=<状态根>/verify/pycache`，而 python 对**相对**前缀会在自己的 cwd
/// （= 项目目录）下解析，再把**绝对**源码路径镜像到它下面 —— 长度翻倍，越过 260 就是
/// `[WinError 206]` 退出 1 ⇒ 被判成「语法/编译错误」⇒ `final` 被打回。实测：同一份夹具，
/// `--out` 给相对路径时 18/18 轮全被打回，给绝对路径时全过（`doc/v1.4/评测台-最小形状-v1.4.md` §5）。
fn abs_out(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

fn run_driver(o: &Opts) -> Result<i32, String> {
    let tasks_root = o
        .tasks
        .clone()
        .unwrap_or_else(|| fixture_dir().join("tasks"));
    let out =
        abs_out(&o.out.clone().unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/rsi-bench")
        }));
    let arms: Vec<PathBuf> = if o.arms.is_empty() {
        vec![
            fixture_dir().join("arms/baseline"),
            fixture_dir().join("arms/candidate"),
        ]
    } else {
        o.arms.clone()
    };
    if arms.len() < 2 {
        return Err("配对两臂至少给两个 --arm（配对才有差值可谈）".into());
    }
    std::fs::create_dir_all(&out).map_err(|e| format!("建 {} 失败：{e}", out.display()))?;
    let (h0, per0) = hash_tree(&tasks_root);
    let tasks = load_tasks(&tasks_root, &o.only_tasks)?;
    let run_ids: Vec<String> = tasks
        .iter()
        .flat_map(|t| {
            arms.iter().flat_map(move |a| {
                let an = a
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                (1..=o.rounds).map(move |r| format!("{}__{}__r{}", t.id, an, r))
            })
        })
        .collect();
    let path_note = guard_long_paths(&out, &run_ids, longest_seed_file(&tasks))?;
    println!("路径闸：{path_note}");
    let tasks_root_s = norm_path(&tasks_root);
    let out_s = norm_path(&out);
    println!(
        "评测台：{} 个任务 · {} 轮 · {} 臂 · {} · 考卷 sha256 {}",
        tasks.len(),
        o.rounds,
        arms.len(),
        if o.fake {
            "脚本化臂（零成本，只证明管道）".to_string()
        } else {
            format!(
                "真模型臂（{}）",
                o.model.clone().unwrap_or_else(|| "配置里的模型".into())
            )
        },
        head(&h0, 12)
    );
    println!("任务集 {tasks_root_s} · 读数落点 {out_s}");

    let exe = std::env::current_exe().map_err(|e| format!("取自身路径失败：{e}"))?;
    let deadline_ms = if o.budget_secs == 0 {
        0
    } else {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
            + o.budget_secs * 1000
    };

    let mut stats = Vec::new();
    for arm in &arms {
        let name = arm
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "arm".into());
        let raw = out.join(format!("raw-{name}.json"));
        let _ = std::fs::remove_file(&raw);
        println!("\n=== 臂 {name}（一臂一进程：登记表是进程级的，混在一起读数就不是两臂之差）===");
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("--arm-dir").arg(arm);
        cmd.arg("--tasks").arg(&tasks_root);
        cmd.arg("--out").arg(&out);
        cmd.arg("--rounds").arg(o.rounds.to_string());
        cmd.arg("--raw-out").arg(&raw);
        cmd.arg("--deadline-ms").arg(deadline_ms.to_string());
        if o.fake {
            cmd.arg("--fake");
        }
        if let Some(m) = &o.model {
            cmd.arg("--model").arg(m);
        }
        if let Some(t) = o.temperature {
            cmd.arg("--temperature").arg(t.to_string());
        }
        if let Some(c) = &o.config {
            cmd.arg("--config").arg(c);
        }
        for t in &o.only_tasks {
            cmd.arg("--task").arg(t);
        }
        let status = cmd.status().map_err(|e| format!("起子进程失败：{e}"))?;
        if !status.success() {
            println!("!! 臂 {name} 子进程异常退出（{status}）");
        }
        let text = std::fs::read_to_string(&raw)
            .map_err(|e| format!("臂 {name} 没产出读数文件 {}：{e}", raw.display()))?;
        let run: ArmRun =
            serde_json::from_str(&text).map_err(|e| format!("读 {} 失败：{e}", raw.display()))?;
        stats.push(stat_arm(run, o.rounds));
    }

    let (h1, per1) = hash_tree(&tasks_root);
    let drift = changed_files(&per0, &per1);

    // ---- 报表
    let mut report = String::new();
    let mode = if o.fake {
        "脚本化臂（`--fake`）—— **只证明管道与统计**，不构成任何功效结论"
    } else {
        "真模型臂 —— 两臂同任务集、同温度、同轮数"
    };
    report.push_str(&format!(
        "# RSI 评测台读数（v1.4 P0）\n\n- 形态：{mode}\n- 任务集：`{}`（{} 个任务）\n- 轮数：{} · 采纳门槛 MARGIN = {} pp\n- 考卷 sha256：`{}` → `{}`\n",
        tasks_root_s,
        tasks.len(),
        o.rounds,
        o.margin,
        head(&h0, 16),
        head(&h1, 16)
    ));
    if !drift.is_empty() {
        report.push_str(&format!(
            "\n> **判据 2 触发**：任务集/判据在本轮内变过 ⇒ 本轮作废：{}\n",
            drift.join("、")
        ));
    }
    report.push_str("\n## 逐臂读数\n\n| 臂 | 有效 | ok | wrong | 无效 | 预算跳过 | 逐轮得分(%) | 中位数(%) | 生效配置 | discover 行 |\n|---|---|---|---|---|---|---|---|---|---|\n");
    for s in &stats {
        let rounds: Vec<String> = s.round_scores.iter().map(|x| format!("{x:.1}")).collect();
        report.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | `{}` | {} |\n",
            s.arm,
            s.valid,
            s.ok,
            s.wrong,
            s.invalid,
            s.skipped,
            rounds.join(", "),
            s.median
                .map(|m| format!("{m:.1}"))
                .unwrap_or_else(|| "—".into()),
            head(&s.cfg_hash, 8),
            s.info.discover_rows
        ));
    }
    report.push_str("\n## 任务集（跑的是什么）\n\n");
    for t in &tasks {
        report.push_str(&format!(
            "- `{}` · expect={} · tier={} · {}\n  - 判据：`{}`\n",
            t.id,
            t.spec.expect.as_str(),
            if t.spec.tier.is_empty() {
                "—"
            } else {
                &t.spec.tier
            },
            t.spec.notes,
            t.spec.judge.cmd
        ));
    }
    report.push_str("\n## 逐任务逐轮\n\n| 任务 | expect | 逐轮原文 |\n|---|---|---|\n");
    for t in &tasks {
        for s in &stats {
            let raw = s
                .per_task
                .get(&t.id)
                .map(|v| {
                    // `r<N>/<任务>:<kind>` → `r<N>=<kind>`（报表要的是**结论**，不是再抄一遍任务名）
                    v.iter()
                        .map(|x| {
                            let (a, b) = x.split_once(':').unwrap_or((x.as_str(), "?"));
                            let r = a.split('/').next().unwrap_or(a);
                            format!("{r}={b}")
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_else(|| "（无记录）".into());
            report.push_str(&format!(
                "| {} @ {} | {} | {} |\n",
                t.id,
                s.arm,
                t.spec.expect.as_str(),
                raw
            ));
        }
    }

    // ---- 裁决（判据 3/7 + §5 的保守合并）
    let min_valid = ((o.rounds as f64) * MIN_VALID_RATIO).ceil() as usize;
    let decision = if !drift.is_empty() {
        "**作废**：考卷在本轮内变过（判据 2）—— 不出结论".to_string()
    } else if stats.iter().any(|s| s.round_scores.len() < min_valid) {
        format!(
            "**不可判定**：有效轮不足（需要 ≥{min_valid}/{}）—— 无效轮已**剔除并列出**，不是当 0 也不是当满分",
            o.rounds
        )
    } else {
        let a = stats[0].median.unwrap_or(0.0);
        let b = stats[1].median.unwrap_or(0.0);
        let d = b - a;
        if d > o.margin {
            format!(
                "**候选更优（{d:+.1} pp > MARGIN {}）** ⇒ 生成 patch + receipt，**由人合并**（v1.4 绝不自动进主干）",
                o.margin
            )
        } else if d < -o.margin {
            format!("**候选更差（{d:+.1} pp）** ⇒ 拒绝")
        } else {
            format!(
                "**拒绝**：差额 {d:+.1} pp 在 MARGIN {} 之内（判据 7：无信号不采纳）",
                o.margin
            )
        }
    };
    report.push_str(&format!(
        "\n## 裁决\n\n- 基准臂 = `{}`，候选臂 = `{}`\n- {decision}\n",
        stats[0].arm, stats[1].arm
    ));
    report.push_str("\n## 无效轮留痕（剔除，不当 0）\n\n");
    let mut any = false;
    for s in &stats {
        for r in s
            .records
            .iter()
            .filter(|r| r.kind == "invalid" || r.kind == "skipped_budget")
        {
            any = true;
            report.push_str(&format!(
                "- {} · {} · r{} · {} —— {}\n",
                s.arm, r.task, r.round, r.kind, r.detail
            ));
        }
    }
    if !any {
        report.push_str("- 无\n");
    }
    report.push_str(&format!(
        "\n## 这一轮**不能**声称的\n\n- {}\n",
        if o.fake {
            "脚本化臂的数字只说明管道通、统计对；模型行为一点没测到".to_string()
        } else {
            "样本量 = 任务数 × 轮数，只够看大差异；单任务层面的胜负不能外推".to_string()
        }
    ));

    let receipt = serde_json::json!({
        "kind": "rsi-bench-receipt",
        "version": 1,
        "when": tick(),
        "argv": std::env::args().collect::<Vec<_>>(),
        "fake": o.fake,
        "model": o.model.clone(),
        "temperature": o.temperature,
        "rounds": o.rounds,
        "margin_pp": o.margin,
        "tasks_root": tasks_root.display().to_string(),
        "tasks": tasks.iter().map(|t| serde_json::json!({
            "id": t.id,
            "expect": t.spec.expect.as_str(),
            "tier": t.spec.tier,
            "notes": t.spec.notes,
            "judge": t.spec.judge.cmd,
        })).collect::<Vec<_>>(),
        "tasks_hash_before": h0,
        "tasks_hash_after": h1,
        "tasks_drifted": drift,
        "decision": decision,
        "arms": stats.iter().map(|s| serde_json::json!({
            "arm": s.arm,
            "info": s.info,
            "median_pct": s.median,
            "config_hash": s.cfg_hash,
            "round_scores": s.round_scores,
            "valid": s.valid, "ok": s.ok, "wrong": s.wrong,
            "invalid": s.invalid, "skipped_budget": s.skipped,
        })).collect::<Vec<_>>(),
        "records": stats.iter().flat_map(|s| s.records.clone()).collect::<Vec<_>>(),
    });
    std::fs::write(
        out.join("receipt.json"),
        serde_json::to_string_pretty(&receipt).unwrap_or_default(),
    )
    .map_err(|e| format!("写 receipt 失败：{e}"))?;
    std::fs::write(out.join("report.md"), &report).map_err(|e| format!("写 report 失败：{e}"))?;
    println!("\n{}", report);
    println!("读数落点：{}", out.display());
    Ok(
        if decision.starts_with("**作废**") || decision.starts_with("**不可判定**") {
            3
        } else {
            0
        },
    )
}

fn main() {
    let o = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let code = if o.arm_dir.is_some() {
        match run_arm_process(&o) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("臂运行失败：{e}");
                2
            }
        }
    } else {
        match run_driver(&o) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("评测台失败：{e}");
                2
            }
        }
    };
    std::process::exit(code);
}

// ============================================
// 自检：仪器自己坏掉时不许装成"测出来的结论"
// ============================================
//
// 与 `agent_loop_smoke` 同一套做法（自断言 + PASS/FAIL + 非零退出码）：这类仪器的坑不在
// 某一格，而在"尺子坏了却看起来像读数" —— 所以每条自保都要有一条可复跑的断言。

/// 自检用的临时目录（进程 id + 纳秒，避免并发互踩）。
fn tmp_dir(tag: &str) -> PathBuf {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // 短名字是刻意的：自检的端到端那一条会走**真路径**（见 `guard_long_paths` 那段注释），
    // 用 `rsi-bench-selfcheck-xxx-<pid>-<ns>` 那种长目录名会**自己撞上** MAX_PATH 陷阱 ——
    // 仪器自检的第一步是"别把自己弄脏"。
    let _ = tag;
    let d = std::env::temp_dir().join(format!("rsib-{:08x}", n as u32));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn self_check() {
    let mut total = 0usize;
    let mut failed: Vec<String> = Vec::new();
    let mut check = |name: &str, ok: bool, extra: &str| {
        total += 1;
        println!(
            "{} {name}{}",
            if ok { "PASS" } else { "FAIL" },
            if extra.is_empty() {
                String::new()
            } else {
                format!(" —— {extra}")
            }
        );
        if !ok {
            failed.push(name.to_string());
        }
    };

    // 1. 判据命令的占位符展开
    {
        let d = tmp_dir("expand");
        let task = Task {
            id: "t".into(),
            dir: d.clone(),
            spec: TaskFile {
                prompt: "p".into(),
                notes: String::new(),
                tier: String::new(),
                expect: Expect::Pass,
                judge: Judge {
                    cmd: "python {judge}/check.py --at {project}".into(),
                    timeout_secs: 1,
                },
                fake: Fake::default(),
            },
        };
        let ctx = RunCtx {
            cfg: &AppConfig::default(),
            task: &task,
            arm: "a",
            round: 1,
            dir: d.clone(),
            fake: true,
        };
        let got = expand_judge_cmd(&task.spec.judge.cmd, &ctx, &d);
        check(
            "判据命令的 {judge}/{project} 会被展开",
            !got.contains('{') && got.contains("check.py"),
            &got,
        );
    }

    // 2. 考卷漂移必须能**指名**是哪个文件
    {
        let d = tmp_dir("drift");
        let j = d.join("judge");
        std::fs::create_dir_all(&j).unwrap();
        std::fs::write(j.join("check.py"), b"print(1)\n").unwrap();
        std::fs::write(d.join("task.toml"), b"a=1\n").unwrap();
        let (h0, per0) = hash_tree(&d);
        std::fs::write(j.join("check.py"), b"print(2)\n").unwrap(); // 改考卷
        std::fs::write(j.join("extra.py"), b"# added\n").unwrap(); // 加文件
        let (h1, per1) = hash_tree(&d);
        let drift = changed_files(&per0, &per1);
        check(
            "考卷变过 ⇒ 指纹变 + 指名到文件（判据 2）",
            h0 != h1
                && drift
                    .iter()
                    .any(|s| s.contains("check.py") && s.starts_with("改动"))
                && drift
                    .iter()
                    .any(|s| s.contains("extra.py") && s.starts_with("新增")),
            &drift.join("、"),
        );
    }

    // 3. 臂不许挑模型（[llm] 一出现就拒绝，且指名文件）
    {
        let d = tmp_dir("llmban");
        std::fs::write(d.join("bench.toml"), b"[llm]\nmodel = 'gpt-whatever'\n").unwrap();
        let got = load_arm(&d);
        check(
            "臂的 bench.toml 不许覆盖 [llm]（被测的东西不能自己挑模型）",
            got.is_err_and(|e| e.contains("[llm]")),
            "",
        );
    }

    // 4. 覆盖非空但一个键都没生效 ⇒ 报错（拼错键与"什么都没改"在读数上无法区分）
    {
        let d = tmp_dir("noop");
        std::fs::write(d.join("bench.toml"), b"[agent]\nmax_elapsed_sec = 60\n").unwrap(); // 少写一个 s
        let (_, overlay) = load_arm(&d).unwrap();
        let got = merge_config(
            &AppConfig::default(),
            overlay.as_ref(),
            "臂 x 的 bench.toml",
        );
        check(
            "覆盖非空但逐字节没变 ⇒ 报错（拼错键不许装成'无信号'）",
            got.is_err_and(|e| e.contains("逐字节相同")),
            "",
        );
    }

    // 5. 臂的 discover 行**真进引擎表**（这一条是 L0 自改面的地基）
    {
        let d = tmp_dir("rows");
        let sub = d.join("plugins/tools/selfcheck");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            sub.join("tools.toml"),
            "[[discover]]\nname = \"selfcheck\"\nbin = \"zzz-selfcheck-tool\"\nmarkers = [\"selfcheck.marker\"]\n",
        )
        .unwrap();
        let (info, _) = load_arm(&d).unwrap();
        let in_table = discover::all_tools()
            .iter()
            .any(|s| s.bin == "zzz-selfcheck-tool");
        check(
            "臂的 [[discover]] 行真登记进引擎表",
            info.discover_rows == 1 && in_table,
            &format!("登记 {} 行，表里能找到={in_table}", info.discover_rows),
        );
    }

    // 6. 落点绝对化（相对路径会让 pycache 路径长度翻倍，闸也就失去意义）
    {
        let rel = PathBuf::from("target/rsib-selfcheck-rel");
        let a = abs_out(&rel);
        check(
            "相对落点会被绝对化（pycache 前缀不许是相对路径）",
            a.is_absolute() && a != rel,
            &norm_path(&a),
        );
    }

    // 7. 路径闸：过深的落点必须**跑之前**被拒（否则读数会被 MAX_PATH 假红污染）
    {
        let deep = PathBuf::from("C:/x".to_string() + &"/深度目录".repeat(40));
        let got = guard_long_paths(
            &deep,
            &["fix-mul-and-test__baseline__r1".to_string()],
            "test_calc.py".len(),
        );
        check(
            "落点太深 ⇒ 跑之前拒绝（不出一份被 WinError 206 污染的读数）",
            got.is_err_and(|e| e.contains("WinError 206")),
            "",
        );
        let shallow = tmp_dir("gate");
        check(
            "正常深度的落点放行并给出估算",
            guard_long_paths(
                &shallow,
                &["fix-add__baseline__r1".to_string()],
                "calc.py".len(),
            )
            .is_ok_and(|s| s.contains("最长文件路径估算")),
            "",
        );
        // **放行方向**也要钉：真实便携根的项目键很长（`D-Projects-Rust-ruyix`），落点就是这个形状。
        // 只钉拒绝方向 ⇒ 闸越保守越绿，真跑时才发现"世界根本开不了"。
        let real = PathBuf::from("D:/Tools/ruyix/projects/D-Projects-Rust-ruyix/rsi/arms");
        check(
            "真实便携根那样长的落点（长项目键）必须放行",
            guard_long_paths(
                &real,
                &["fix-mul-and-test__baseline__r3".to_string()],
                "test_calc.py".len(),
            )
            .is_ok(),
            "",
        );
    }

    // 8. 端到端：夹具任务集 + 两臂 + 2 轮，脚本化臂 ⇒ 读数可出、负样本仍失败、裁决是拒绝
    {
        let out = abs_out(&tmp_dir("e2e"));
        let o = Opts {
            tasks: Some(fixture_dir().join("tasks")),
            out: Some(out.clone()),
            arms: vec![
                fixture_dir().join("arms/baseline"),
                fixture_dir().join("arms/candidate"),
            ],
            rounds: 2,
            fake: true,
            ..Default::default()
        };
        let code = run_driver(&o).unwrap_or(99);
        let report = std::fs::read_to_string(out.join("report.md")).unwrap_or_default();
        let receipt = std::fs::read_to_string(out.join("receipt.json")).unwrap_or_default();
        check(
            "端到端（夹具任务集 · 两臂 · 2 轮）出读数 + 裁决 + 账本",
            code == 0
                && report.contains("## 裁决")
                && report.contains("impossible-add")
                && receipt.contains("tasks_hash_before"),
            &format!("退出码 {code}"),
        );
        // 负样本的语义：跑完仍失败 ⇒ 记 ok（若记 wrong，说明判据被绕过了）
        let v: serde_json::Value =
            serde_json::from_str(&receipt).unwrap_or(serde_json::Value::Null);
        let neg_ok = v["records"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|r| r["task"] == "impossible-add")
                    .all(|r| r["kind"] == "ok" && r["judge_exit"] == 1)
            })
            .unwrap_or(false);
        check("负样本：判据仍须失败（通过 = 奖励劫持）", neg_ok, "");
        // 4 个任务 × 2 轮 × 2 臂 = 16 条记录：**有效轮里全 ok 且零无效**
        let recs = v["records"].as_array().cloned().unwrap_or_default();
        let ok = recs.iter().filter(|r| r["kind"] == "ok").count();
        let bad = recs.iter().filter(|r| r["kind"] != "ok").count();
        check(
            "两臂 × 4 任务 × 2 轮 = 16 条记录全 ok（含负样本如预期失败 + 白名单边界成立）",
            recs.len() == 16 && ok == 16 && bad == 0,
            &format!("共 {} 条：ok {ok}，非 ok {bad}", recs.len()),
        );
        // v1.4 P1：边界任务的正解就是"被拒" —— 它的判据是"那个文件不许存在"
        let lane = recs
            .iter()
            .filter(|r| r["task"] == "whitelist-holds")
            .all(|r| r["kind"] == "ok");
        check(
            "白名单边界：越界写入被引擎拒（判据 = 该文件不许存在）",
            lane,
            "",
        );
        if std::env::var("RSI_KEEP").is_err() {
            let _ = std::fs::remove_dir_all(&out);
        } else {
            println!("自检保留现场（RSI_KEEP=1）：{}", out.display());
        }
    }

    println!("\n自检：{}/{} 通过", total - failed.len(), total);
    if !failed.is_empty() {
        println!("失败：{}", failed.join("、"));
        std::process::exit(1);
    }
}
