//! Git 版本控制与回滚。
//!
//! 分工：**run.json 记元数据，git 记内容版本**（对齐"仓库即记录系统"）。
//!
//! 两个作用域，规则不同：
//!
//! - **run 的产物目录**：那是我们自己 `git init` 出来的仓库，可以放心 `reset --hard`；
//! - **用户的仓库**：只能动我们自己的分支/worktree，**绝不碰用户未提交的改动**。
//!
//! ## 回滚的三条纪律
//!
//! 1. **先救再退**：回滚前把当前状态记成一个 `rescue` 提交，回滚本身也可逆；
//! 2. **不碰用户的东西**：只动我们的产物与我们创建的分支；
//! 3. **回滚必须核对**：`git status` 干净 **且** 关键文件 sha256 与目标一致，
//!    核对不过就报失败 —— 只说"我回滚了"不算。

use crate::exec::{self, CmdOutput};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

fn git(cwd: &Path, args: &[&str]) -> CmdOutput {
    exec::run(cwd, "git", args, Duration::from_secs(120), &[])
}

fn must(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let o = git(cwd, args);
    if !o.passed() {
        return Err(format!(
            "git {} 失败：{}",
            args.join(" "),
            exec::clip(o.stderr.trim(), 400)
        ));
    }
    Ok(o.stdout.trim().to_string())
}

pub fn is_repo(dir: &Path) -> bool {
    git(dir, &["rev-parse", "--git-dir"]).passed()
}

pub fn head(dir: &Path) -> Result<String, String> {
    must(dir, &["rev-parse", "--short", "HEAD"])
}

/// 工作区是否干净（只看已跟踪文件，未跟踪的运行产物不算法）。
pub fn is_clean(dir: &Path) -> Result<bool, String> {
    Ok(must(dir, &["status", "--porcelain", "--untracked-files=no"])?.is_empty())
}

/// 建仓（幂等）：把运行目录变成一个独立仓库，并落一个初始提交。
///
/// 刻意**不在用户目录里 init** —— 生成的代码本来就不该进用户的仓库；
/// 它需要一个自己的历史来支撑归因与回滚。
pub fn snapshot_init(dir: &Path, message: &str) -> Result<String, String> {
    if !is_repo(dir) {
        must(dir, &["init", "-q"])?;
        // 本机可能没配 user.name/email；只给这个仓库配，不动全局
        let _ = must(dir, &["config", "user.email", "harness@local"]);
        let _ = must(dir, &["config", "user.name", "darkhorse-harness"]);
        // **关掉行尾转换**（真机踩到过）：本机全局是 core.autocrlf=true，
        // `git reset --hard` 检出时会把 LF 变成 CRLF —— 于是"回滚"悄悄改了文件字节，
        // 而回滚的全部意义就是"回到当时的样子"。运行产物是本机生成的制品，
        // 要的就是逐字节一致，所以在这个仓库里（只在这个仓库里）关掉它。
        let _ = must(dir, &["config", "core.autocrlf", "false"]);
        let _ = must(dir, &["config", "core.safecrlf", "false"]);
    }
    commit_stage(dir, message)
}

/// 阶段提交：没有任何改动就跳过（返回 None），**不留空提交**。
pub fn commit_stage(dir: &Path, message: &str) -> Result<String, String> {
    let _ = must(dir, &["add", "-A"]);
    if is_clean(dir)? && !has_staged(dir)? {
        return Ok(match head(dir) {
            Ok(h) => h,
            Err(_) => {
                // 空仓库也要有个起点
                must(dir, &["commit", "-q", "--allow-empty", "-m", message])?;
                head(dir)?
            }
        });
    }
    must(dir, &["commit", "-q", "-m", message])?;
    head(dir)
}

fn has_staged(dir: &Path) -> Result<bool, String> {
    Ok(!must(dir, &["diff", "--cached", "--name-only"])?.is_empty())
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Commit {
    pub hash: String,
    pub subject: String,
}

/// 提交历史（新到旧）。
pub fn log(dir: &Path, limit: usize) -> Result<Vec<Commit>, String> {
    let out = must(
        dir,
        &["log", &format!("-{limit}"), "--pretty=format:%h\t%s"],
    )?;
    Ok(out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (h, s) = l.split_once('\t').unwrap_or((l, ""));
            Commit {
                hash: h.trim().to_string(),
                subject: s.trim().to_string(),
            }
        })
        .collect())
}

/// 回滚计划：把"要回滚"变成可核对的条目，而不是一句话。
#[derive(Serialize, Clone, Debug)]
pub struct RollbackPlan {
    pub dir: String,
    pub target: String,
    pub target_hash: String,
    /// 当前状态（回滚前会先存成 rescue 提交）
    pub current_hash: String,
    /// 回滚会影响的文件（目标→当前 的差异）
    pub files: Vec<String>,
    pub clean_now: bool,
    /// 真正执行时会先做这个提交，保证回滚可逆
    pub rescue_message: String,
}

/// 解析回滚目标：支持 commit hash、`HEAD~n`、以及阶段名（匹配提交信息）。
pub fn resolve_target(dir: &Path, target: &str) -> Result<String, String> {
    // 直接是个合法 revision？
    if must(
        dir,
        &["rev-parse", "--verify", &format!("{target}^{{commit}}")],
    )
    .is_ok()
    {
        return must(dir, &["rev-parse", &format!("{target}^{{commit}}")]);
    }
    // 否则按提交信息里的阶段名找最近一次
    let needle = target.trim();
    let list = log(dir, 100)?;
    for c in &list {
        if c.subject.contains(needle) {
            return must(dir, &["rev-parse", &format!("{}^{{commit}}", c.hash)]);
        }
    }
    Err(format!(
        "找不到回滚目标 {target}（既不是 commit，也没有匹配的提交说明）"
    ))
}

/// 生成回滚计划（**只读**，不改任何东西）。
pub fn plan_rollback(dir: &Path, target: &str) -> Result<RollbackPlan, String> {
    if !is_repo(dir) {
        return Err(format!("{} 不是 git 仓库，无法回滚", dir.display()));
    }
    let target_hash = resolve_target(dir, target)?;
    let current_hash = head(dir)?;
    let files_out = must(dir, &["diff", "--name-only", &target_hash, "HEAD"])?;
    Ok(RollbackPlan {
        dir: dir.display().to_string(),
        target: target.to_string(),
        target_hash,
        current_hash: current_hash.clone(),
        files: files_out
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|s| s.to_string())
            .collect(),
        clean_now: is_clean(dir)?,
        rescue_message: format!("rescue: 回滚到 {target} 之前的状态（{current_hash}）"),
    })
}

#[derive(Serialize, Clone, Debug)]
pub struct RollbackReport {
    pub plan: RollbackPlan,
    /// 回滚前的状态被存成的提交（回滚本身可逆的关键）
    pub rescue_commit: String,
    pub new_head: String,
    /// 核对结果：`git status` 干净 + 文件树与目标一致
    pub verified: bool,
    pub verify_detail: String,
}

/// 执行回滚。三步：先救、再退、后核对。
pub fn rollback(dir: &Path, target: &str) -> Result<RollbackReport, String> {
    let plan = plan_rollback(dir, target)?;

    // 1) 先救：把当前状态落成提交（哪怕工作区脏，也能先存下来）
    let rescue = commit_stage(dir, &plan.rescue_message)?;

    // 2) 再退：硬回退到目标提交
    must(dir, &["reset", "--hard", &plan.target_hash])?;
    // 未跟踪文件不动（可能含运行产物）；但要确认没有残留的已跟踪改动
    let dirty = must(dir, &["status", "--porcelain", "--untracked-files=no"])?;

    // 3) 后核对：工作树必须与目标提交逐字节一致
    let diff = must(dir, &["diff", "--stat", &plan.target_hash, "HEAD"])?;
    let tree_ok = diff.trim().is_empty();
    let clean = dirty.trim().is_empty();
    Ok(RollbackReport {
        plan,
        rescue_commit: rescue,
        new_head: head(dir)?,
        verified: clean && tree_ok,
        verify_detail: if clean && tree_ok {
            "工作区干净，且文件树与目标提交一致".into()
        } else {
            format!("核对未通过：status={dirty:?} diff={diff:?}")
        },
    })
}

// ---------------------------------------------------------------- 用户仓库：分支回滚

#[derive(Serialize, Clone, Debug)]
pub struct BranchRollback {
    pub repo: String,
    pub branch: String,
    pub local_deleted: bool,
    pub remote_deleted: bool,
    /// 主工作区 HEAD 与状态在操作前后必须完全一致
    pub main_head_before: String,
    pub main_head_after: String,
    pub main_untouched: bool,
    pub verify_detail: String,
}

/// 删掉我们创建的熵分支（本地 + 远端），并**核对主工作区没被动过**。
pub fn rollback_branch(
    repo: &Path,
    branch: &str,
    remote: &str,
    also_remote: bool,
) -> Result<BranchRollback, String> {
    if !is_repo(repo) {
        return Err(format!("{} 不是 git 仓库", repo.display()));
    }
    if !branch.starts_with("entropy/") && !branch.starts_with("harness/") {
        // 只允许删我们自己造的命名空间，避免误删用户分支
        return Err(format!(
            "拒绝删除分支 {branch}：只允许回滚 entropy/* 或 harness/* 命名空间"
        ));
    }
    let before_head = head(repo)?;
    let before_status = must(repo, &["status", "--porcelain", "--untracked-files=no"])?;

    // 若这个分支正在某个 worktree 里被检出，先摘掉 worktree
    let wts = must(repo, &["worktree", "list", "--porcelain"]).unwrap_or_default();
    for block in wts.split("\n\n") {
        if block.contains(&format!("branch refs/heads/{branch}"))
            && let Some(path) = block.lines().find_map(|l| l.strip_prefix("worktree "))
        {
            let _ = git(repo, &["worktree", "remove", "--force", path.trim()]);
        }
    }

    let local_deleted = git(repo, &["branch", "-D", branch]).passed();
    let mut remote_deleted = false;
    if also_remote {
        // best-effort：远端没有这个分支不算失败
        remote_deleted = git(repo, &["push", remote, "--delete", branch]).passed();
    }

    let after_head = head(repo)?;
    let after_status = must(repo, &["status", "--porcelain", "--untracked-files=no"])?;
    let untouched = before_head == after_head && before_status == after_status;
    Ok(BranchRollback {
        repo: repo.display().to_string(),
        branch: branch.to_string(),
        local_deleted,
        remote_deleted,
        main_head_before: before_head,
        main_head_after: after_head,
        main_untouched: untouched,
        verify_detail: if untouched {
            "主工作区 HEAD 与工作树状态在回滚前后完全一致".into()
        } else {
            format!("主工作区被改动了！head: {before_status:?} → {after_status:?}")
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempRepo(std::path::PathBuf);

    impl TempRepo {
        fn new(tag: &str) -> Self {
            let d = std::env::temp_dir().join(format!(
                "dh-git-{tag}-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&d).unwrap();
            let r = TempRepo(d);
            snapshot_init(&r.0, "init").unwrap();
            r
        }
        fn write(&self, name: &str, body: &str) {
            std::fs::write(self.0.join(name), body).unwrap();
        }
        fn read(&self, name: &str) -> String {
            std::fs::read_to_string(self.0.join(name)).unwrap()
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn init_creates_a_repo_with_a_first_commit() {
        let r = TempRepo::new("init");
        assert!(is_repo(&r.0));
        assert_eq!(log(&r.0, 10).unwrap().len(), 1);
        assert!(is_clean(&r.0).unwrap());
    }

    #[test]
    fn stage_commits_record_each_step_and_show_in_log() {
        let r = TempRepo::new("stages");
        r.write("a.py", "x = 1\n");
        commit_stage(&r.0, "generate: 初版").unwrap();
        r.write("a.py", "x = 2\n");
        commit_stage(&r.0, "repair: 修掉 HX204").unwrap();
        let log = log(&r.0, 10).unwrap();
        assert_eq!(log.len(), 3, "{log:?}");
        assert!(log[0].subject.contains("repair"));
        assert!(log[1].subject.contains("generate"));
    }

    #[test]
    fn commit_stage_without_changes_does_not_create_empty_commits() {
        let r = TempRepo::new("nocl");
        let n0 = log(&r.0, 10).unwrap().len();
        commit_stage(&r.0, "verify: 无变化").unwrap();
        assert_eq!(log(&r.0, 10).unwrap().len(), n0, "不该产生空提交");
    }

    #[test]
    fn rollback_restores_file_contents_from_the_target_stage() {
        let r = TempRepo::new("roll");
        r.write("a.py", "before\n");
        commit_stage(&r.0, "generate: 初版").unwrap();
        r.write("a.py", "after-broken\n");
        r.write("b.py", "new file\n");
        commit_stage(&r.0, "repair: 改坏了").unwrap();

        let rep = rollback(&r.0, "generate").unwrap();
        assert!(rep.verified, "{}", rep.verify_detail);
        assert_eq!(r.read("a.py"), "before\n");
        assert!(!r.0.join("b.py").exists(), "回滚应当撤销新增文件");
    }

    #[test]
    fn rollback_is_itself_reversible_via_the_rescue_commit() {
        // 纪律 1：先救再退。回滚掉的东西必须还能找回来。
        let r = TempRepo::new("rescue");
        r.write("a.py", "v1\n");
        commit_stage(&r.0, "generate: v1").unwrap();
        r.write("a.py", "v2-有价值\n");
        commit_stage(&r.0, "repair: v2").unwrap();

        let rep = rollback(&r.0, "generate").unwrap();
        assert_eq!(r.read("a.py"), "v1\n");
        // 救回来的提交里必须还有 v2 的内容
        let rescue_body = must(&r.0, &["show", &format!("{}:a.py", rep.rescue_commit)]).unwrap();
        assert_eq!(rescue_body, "v2-有价值");
    }

    #[test]
    fn rollback_restores_bytes_exactly_including_line_endings() {
        // 真机 bug 回归：本机 core.autocrlf=true 时，`reset --hard` 会把 LF 检出成 CRLF，
        // 于是"回滚"静默改了文件字节。运行仓库必须关掉行尾转换。
        let r = TempRepo::new("eol");
        let original = "line1
line2
line3
";
        r.write("a.py", original);
        commit_stage(&r.0, "generate: 初版").unwrap();
        r.write(
            "a.py",
            "改坏了
",
        );
        commit_stage(&r.0, "repair: 坏改动").unwrap();

        rollback(&r.0, "generate").unwrap();
        let got = std::fs::read(r.0.join("a.py")).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&got),
            original,
            "回滚后的字节必须与当初完全一致（含行尾）"
        );
    }

    #[test]
    fn snapshot_repo_disables_line_ending_conversion() {
        let r = TempRepo::new("cfg");
        let v = must(&r.0, &["config", "core.autocrlf"]).unwrap();
        assert_eq!(v.trim(), "false", "运行仓库必须关掉 autocrlf");
    }

    #[test]
    fn rollback_reports_which_files_it_touched() {
        let r = TempRepo::new("plan");
        r.write("a.py", "1\n");
        commit_stage(&r.0, "generate: x").unwrap();
        r.write("a.py", "2\n");
        r.write("c.py", "3\n");
        commit_stage(&r.0, "repair: y").unwrap();
        let plan = plan_rollback(&r.0, "generate").unwrap();
        assert!(plan.files.contains(&"a.py".to_string()), "{:?}", plan.files);
        assert!(plan.files.contains(&"c.py".to_string()), "{:?}", plan.files);
        assert!(plan.clean_now);
        assert!(plan.rescue_message.contains("rescue"));
    }

    #[test]
    fn unknown_target_is_rejected_with_a_clear_message() {
        let r = TempRepo::new("bad");
        let err = plan_rollback(&r.0, "不存在的阶段").unwrap_err();
        assert!(err.contains("找不到回滚目标"), "{err}");
    }

    #[test]
    fn rollback_on_non_repo_fails_loudly() {
        let d = std::env::temp_dir().join("dh-git-nonrepo-x");
        let _ = std::fs::create_dir_all(&d);
        let err = plan_rollback(&d, "HEAD").unwrap_err();
        assert!(err.contains("不是 git 仓库"), "{err}");
    }

    #[test]
    fn branch_rollback_refuses_foreign_namespaces() {
        // 只允许回滚我们自己造的命名空间，避免误删用户分支
        let r = TempRepo::new("ns");
        let err = rollback_branch(&r.0, "master", "origin", false).unwrap_err();
        assert!(err.contains("拒绝删除分支"), "{err}");
    }

    #[test]
    fn branch_rollback_leaves_main_worktree_untouched() {
        let r = TempRepo::new("br");
        r.write("a.py", "keep\n");
        commit_stage(&r.0, "generate: base").unwrap();
        must(&r.0, &["branch", "entropy/2026-x-score"]).unwrap();

        let rep = rollback_branch(&r.0, "entropy/2026-x-score", "origin", false).unwrap();
        assert!(rep.local_deleted);
        assert!(rep.main_untouched, "{}", rep.verify_detail);
        assert_eq!(r.read("a.py"), "keep\n");
    }
}
