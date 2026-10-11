//! 运行留痕：每次运行一个目录，`run.json` 记录任务/计划/生成/验证的全过程。
//!
//! 为什么要把过程全落盘而不是只放内存：Harness 的价值在于"可回看、可复现"。
//! 用户第二天想搞清楚"上次那个失败到底为什么"，靠内存状态是拿不回来的。

use crate::exec::clip;
use crate::generate::GenOutcome;
use crate::lint::LintReport;
use crate::llm::Usage;
use crate::plan::Plan;
use crate::repair::RepairRound;
use crate::verify::VerifyReport;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct RunRecord {
    pub run_id: String,
    pub task: String,
    pub created_at: String,
    pub model: String,
    /// planned | generated | verified | failed
    pub status: String,
    pub dir: String,
    #[serde(default)]
    pub plan: Option<Plan>,
    #[serde(default)]
    pub generation: Option<GenOutcome>,
    #[serde(default)]
    pub verify: Option<VerifyReport>,
    /// 规约检查（自定义 lint 规则）的结果
    #[serde(default)]
    pub lint: Option<LintReport>,
    /// 自我纠正循环的逐轮记录
    #[serde(default)]
    pub repair: Vec<RepairRound>,
    /// 知识库注入记录（v0.6）：每个注入点一条 —— 规划、每个生成步骤、每轮修复。
    /// 放这里而不是只进 trace，是为了让 `harness context` 与 UI 都能直接核对
    /// "这次到底注入了什么、为什么被裁"。
    #[serde(default)]
    pub kb: Vec<crate::kb::retrieve::KbInjection>,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct RunSummary {
    pub run_id: String,
    pub task: String,
    pub created_at: String,
    pub status: String,
    pub dir: String,
    pub file_count: usize,
    pub verdict: String,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub total_tokens: u64,
    /// 规约违规数（error + warning）
    #[serde(default)]
    pub lint_issues: u32,
    /// 自我纠正循环跑了几轮
    #[serde(default)]
    pub repair_rounds: u32,
    /// 本次运行注入了多少条知识库片段（0 也可能是"知识库没开"）
    #[serde(default)]
    pub kb_injected: usize,
}

pub const RUN_FILE: &str = "run.json";

/// `20260916-082012-todo-cli` —— 前缀时间戳保证目录名按时间可排序。
pub fn new_run_id(slug: &str) -> String {
    let now = chrono::Local::now();
    format!("{}-{}", now.format("%Y%m%d-%H%M%S"), slug)
}

pub fn now_iso() -> String {
    chrono::Local::now().to_rfc3339()
}

/// 给人和模型看的当前本地时间：`2026-09-22 星期二 13:44 +08:00`。
///
/// 为什么要由引擎直接给，而不是让模型自己跑 `date /t` / `Get-Date`：它们的输出
/// **依赖机器区域设置**（同一台机中文环境给 `2026/09/22 周二`，英文环境给
/// `Tue 09/22/2026`），模型先猜命令、再猜格式，是最容易白烧几轮的活；而"现在几点"
/// 引擎本来就知道 —— 直接说，别让它去探。
///
/// 星期写中文真名而不是 `%A`：chrono 默认 locale 会给出 `Tuesday`，与界面语言对不上。
pub fn now_human() -> String {
    const WD_CN: [&str; 7] = [
        "星期一",
        "星期二",
        "星期三",
        "星期四",
        "星期五",
        "星期六",
        "星期日",
    ];
    let now = chrono::Local::now();
    let wd = WD_CN[chrono::Datelike::weekday(&now).num_days_from_monday() as usize];
    format!(
        "{} {} {} {}",
        now.format("%Y-%m-%d"),
        wd,
        now.format("%H:%M"),
        now.offset()
    )
}

/// 紧凑时间戳（只有数字和 `-`）—— 目录名安全（`now_iso` 含 `:`，Windows 不允许做文件名）
pub fn now_compact() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

pub fn run_dir(root: &Path, run_id: &str) -> Result<PathBuf, String> {
    // run_id 来自我们自己生成（时间戳 + slug），这里再校验一次，避免从前端传进来的
    // 任意字符串被当成路径用
    if run_id.is_empty() || run_id.contains(['/', '\\', ':']) || run_id.contains("..") {
        return Err(format!("非法 run_id: {run_id}"));
    }
    Ok(root.join(run_id))
}

pub fn project_dir(root: &Path, run_id: &str) -> Result<PathBuf, String> {
    Ok(run_dir(root, run_id)?.join("project"))
}

pub fn save(root: &Path, rec: &RunRecord) -> Result<(), String> {
    let dir = run_dir(root, &rec.run_id)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建运行目录失败 {}: {e}", dir.display()))?;
    let text = serde_json::to_string_pretty(rec).map_err(|e| format!("序列化运行记录失败: {e}"))?;
    std::fs::write(dir.join(RUN_FILE), text).map_err(|e| format!("写入运行记录失败: {e}"))?;

    // **一处挂载，之后所有阶段的事件都会自动落盘。**
    // 与"所有检查都从一个出口出去"是同一条纪律：不留漏网点 ——
    // 只要这次运行落过盘，它就一定有日志与 trace。
    let need_install = crate::observe::current()
        .map(|t| t.run_id() != rec.run_id)
        .unwrap_or(true);
    if need_install {
        let dir = run_dir(root, &rec.run_id)?;
        // 脱敏用的密钥由 run 入口注入（`observe::set_secrets`）。
        // **不再自己去读配置文件**：那条路在 IDE 下读的是引擎实验室的 config.toml，
        // 而用户实际用的是 ruyix 配置里的 key —— 拿错了钥匙等于没脱敏。
        let secrets = crate::observe::secrets();
        crate::observe::install(crate::observe::Tracer::new(&dir, &rec.run_id, secrets));
        crate::observe::log("info", "run", format!("开始记录：{}", rec.run_id));
    }

    // 有观测却不告诉用户观测物在哪，等于没有观测。
    if matches!(rec.status.as_str(), "verified" | "failed")
        && let Some(dir) = crate::observe::current_dir()
    {
        crate::observe::log(
            "info",
            "run",
            format!(
                "日志与 trace 已落盘：{}（读法：harness trace {}）",
                dir.display(),
                rec.run_id
            ),
        );
    }

    crate::observe::log("debug", "run", "save: 落 run.json 完成，准备记 git 快照");
    // **git 记内容版本**（run.json 记元数据）。放在这里是因为它是所有阶段的
    // 唯一落盘口 —— 一处接线，漏不掉某个阶段。
    // `commit_stage` 自己会判断"没变化就不提交"，所以高频调用也不会堆出空提交。
    if let Ok(dir) = project_dir(root, &rec.run_id)
        && dir.exists()
    {
        let subject = format!("{} | {}", rec.status, clip(&rec.task, 48));
        let _ = if crate::gitops::is_repo(&dir) {
            crate::gitops::commit_stage(&dir, &subject)
        } else {
            crate::gitops::snapshot_init(&dir, &subject)
        };
    }
    crate::observe::log("debug", "run", "save: 完成");
    Ok(())
}

pub fn load(root: &Path, run_id: &str) -> Result<RunRecord, String> {
    let path = run_dir(root, run_id)?.join(RUN_FILE);
    if !path.exists() {
        return Err(format!("没有找到这次运行（{}）", path.display()));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("读取运行记录失败: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("解析运行记录失败: {e}"))
}

pub fn list(root: &Path) -> Vec<RunSummary> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<RunSummary> = Vec::new();
    for e in entries.flatten() {
        if !e.path().is_dir() {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        let Ok(rec) = load(root, &name) else { continue };
        let (verdict, passed, failed, skipped, files) = match &rec.verify {
            Some(v) => (
                v.verdict.clone(),
                v.passed,
                v.failed,
                v.skipped,
                v.file_count,
            ),
            None => (
                String::new(),
                0,
                0,
                0,
                rec.generation.as_ref().map(|g| g.files.len()).unwrap_or(0),
            ),
        };
        let lint_issues = rec.lint.as_ref().map(|l| l.total()).unwrap_or(0);
        let kb_injected = rec.kb.iter().map(|k| k.hits.len()).sum();
        out.push(RunSummary {
            run_id: rec.run_id,
            task: rec.task,
            created_at: rec.created_at,
            status: rec.status,
            dir: rec.dir,
            file_count: files,
            verdict,
            passed,
            failed,
            skipped,
            total_tokens: rec.usage.total_tokens,
            lint_issues,
            repair_rounds: rec.repair.len() as u32,
            kb_injected,
        });
    }
    // 目录名带时间戳前缀，倒序即"最新在前"
    out.sort_by(|a, b| b.run_id.cmp(&a.run_id));
    out
}

pub fn delete(root: &Path, run_id: &str) -> Result<(), String> {
    let dir = run_dir(root, run_id)?;
    if !dir.exists() {
        return Ok(());
    }
    // 删掉构建产物目录常见失败：target 里可能有正在被占用的文件，先尽力清 target
    let _ = std::fs::remove_dir_all(dir.join("project").join(".ruyix"));
    // Windows 上"目录不是空的 (os error 145)"几乎总是**删除过程中还有人在写**：
    // 观测 tracer 是全局的（可能在往这个 run 目录追加日志）、杀毒/索引服务也可能摸文件。
    // 退一步重试，比把一次正常的删除报成失败更接近事实。
    let mut last = String::new();
    for attempt in 0..3 {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = e.to_string();
                if !dir.exists() {
                    return Ok(()); // 内容是删干净了，只是父目录项还没来得及回收
                }
                std::thread::sleep(std::time::Duration::from_millis(50 * (attempt + 1)));
            }
        }
    }
    Err(format!("删除运行目录失败 {}: {last}", dir.display()))
}

/// 读取运行目录内的某个文件（UI 里点文件树用）。限制在运行目录内 + 512KB。
pub fn read_project_file(root: &Path, run_id: &str, rel: &str) -> Result<String, String> {
    let rel = crate::generate::safe_rel_path(rel)?;
    let base = project_dir(root, run_id)?;
    let target = base.join(&rel);
    let base_c = std::fs::canonicalize(&base).map_err(|e| format!("运行目录不存在: {e}"))?;
    let target_c = std::fs::canonicalize(&target)
        .map_err(|e| format!("文件不存在 {}: {e}", target.display()))?;
    if !target_c.starts_with(&base_c) {
        return Err(format!("越界访问被拒绝: {rel}"));
    }
    let meta = std::fs::metadata(&target_c).map_err(|e| format!("读取文件信息失败: {e}"))?;
    if meta.len() > 512 * 1024 {
        return Err(format!(
            "文件太大（{} 字节），超过 512KB 预览上限",
            meta.len()
        ));
    }
    std::fs::read_to_string(&target_c).map_err(|e| format!("读取文件失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "dh-harness-ws-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sample(run_id: &str, status: &str) -> RunRecord {
        RunRecord {
            run_id: run_id.into(),
            task: "写个加法器".into(),
            created_at: now_iso(),
            model: "deepseek-v4-pro".into(),
            status: status.into(),
            dir: String::new(),
            ..Default::default()
        }
    }

    #[test]
    fn run_id_is_sortable_and_has_slug() {
        let id = new_run_id("todo-cli");
        assert!(id.ends_with("-todo-cli"), "{id}");
        assert!(id.len() > 20);
    }

    #[test]
    fn rejects_path_traversal_run_ids() {
        let d = TempDir::new("trav");
        assert!(run_dir(&d.0, "../../etc").is_err());
        assert!(run_dir(&d.0, "a/b").is_err());
        assert!(run_dir(&d.0, "").is_err());
        assert!(run_dir(&d.0, "20260916-082012-x").is_ok());
    }

    /// **防回归**：`save` 现在还要建 git 快照，它必须快速返回。
    ///
    /// 真机踩到过：一次 GUI 全流程跑完 verify 的所有检查之后就没动静了 ——
    /// 卡在 save 里的 git 那一段（run.json 停在 "generated"，日志停在 veriy 检查之后）。
    /// 这个测试直接在无 GUI 的情况下压 save 的路径。
    #[test]
    fn save_is_fast_even_with_git_snapshots() {
        let d = std::env::temp_dir().join(format!(
            "dh-save-git-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        let proj = project_dir(&d, "r1").unwrap();
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("a.py"),
            "x = 1
",
        )
        .unwrap();

        let mut rec = RunRecord {
            run_id: "r1".into(),
            task: "t".into(),
            created_at: "now".into(),
            model: "m".into(),
            status: "planned".into(),
            dir: proj.to_string_lossy().to_string(),
            ..Default::default()
        };
        let t0 = std::time::Instant::now();
        for status in ["planned", "generated", "linted", "verified"] {
            rec.status = status.into();
            save(&d, &rec).expect("save 必须成功");
            std::fs::write(
                proj.join("a.py"),
                format!(
                    "x = 1  # {status}
"
                ),
            )
            .unwrap();
        }
        let ms = t0.elapsed().as_millis();
        // 抓的是"卡死"（真机 600s 那次），不是做基准。并行跑测试时会慢一些；
        // Windows 上 git 子进程 spawn 开销大，并行实测可超 60s（单跑 9s），留到 180s
        assert!(ms < 180_000, "save 太慢（{ms}ms），八成卡在 git 上了");
        // 内容变过几次，就该有几个提交
        let n = crate::gitops::log(&proj, 20).map(|v| v.len()).unwrap_or(0);
        assert!(n >= 1, "至少要有一次提交");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let d = TempDir::new("rt");
        let rec = sample("20260916-082012-x", "planned");
        save(&d.0, &rec).unwrap();
        let back = load(&d.0, "20260916-082012-x").unwrap();
        assert_eq!(back.task, rec.task);
        assert_eq!(back.status, "planned");
    }

    #[test]
    fn list_returns_newest_first() {
        let d = TempDir::new("list");
        save(&d.0, &sample("20260916-082012-a", "planned")).unwrap();
        save(&d.0, &sample("20260917-090000-b", "generated")).unwrap();
        let l = list(&d.0);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].run_id, "20260917-090000-b");
        assert_eq!(l[1].run_id, "20260916-082012-a");
    }

    #[test]
    fn read_project_file_stays_inside_project() {
        let d = TempDir::new("read");
        let p = project_dir(&d.0, "20260916-082012-x").unwrap();
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("a.txt"), "hello").unwrap();
        assert_eq!(
            read_project_file(&d.0, "20260916-082012-x", "a.txt").unwrap(),
            "hello"
        );
        assert!(read_project_file(&d.0, "20260916-082012-x", "../run.json").is_err());
        assert!(read_project_file(&d.0, "20260916-082012-x", "missing.txt").is_err());
    }

    #[test]
    fn delete_removes_the_run() {
        let d = TempDir::new("del");
        save(&d.0, &sample("20260916-082012-x", "planned")).unwrap();
        assert!(run_dir(&d.0, "20260916-082012-x").unwrap().exists());
        delete(&d.0, "20260916-082012-x").unwrap();
        assert!(!run_dir(&d.0, "20260916-082012-x").unwrap().exists());
        // 再删一次不报错（幂等）
        delete(&d.0, "20260916-082012-x").unwrap();
    }

    #[test]
    fn corrupt_run_json_is_skipped_not_fatal() {
        let d = TempDir::new("corrupt");
        let dir = run_dir(&d.0, "20260916-082012-bad").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(RUN_FILE), "{ not json").unwrap();
        save(&d.0, &sample("20260917-090000-good", "planned")).unwrap();
        let l = list(&d.0);
        assert_eq!(l.len(), 1, "坏记录应被跳过: {l:?}");
        assert_eq!(l[0].run_id, "20260917-090000-good");
    }
}
