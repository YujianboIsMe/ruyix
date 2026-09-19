//! 产物写回真实项目（差异预览 + 显式确认 + 写前备份）。
//!
//! 引擎（`harness-engine`）生成的产物一律落在自己的沙箱里：
//! `<runs_root>/<run_id>/project/`（见 `workspace::project_dir`）。这是刻意设计 ——
//! 未经人类确认的机器产出不该直接落在真实仓库上。但沙箱与真实项目之间此前
//! **没有任何搬运机制**，于是出现"任务跑完了、项目里啥也没有"。本模块补上这段：
//!
//!   `preview(runs_root, run_id, project_root)` → 逐文件差异（add / modify / same）
//!   `apply(runs_root, run_id, project_root, paths)` → 写回 + 备份
//!
//! 安全约定（三条硬约束，不得绕过）：
//!   1. **绝不默认写**：`preview` 只读，`apply` 必须显式给出要写的文件清单；
//!   2. **写前备份**：被覆盖的原文件先进 `<项目>/.ruyix/backups/<run_id>-<时间戳>/`；
//!   3. **路径封闭**：拒绝绝对路径 / `..` / `.git`，拒绝把产物写进沙箱自身。

use serde::Serialize;
use std::path::{Path, PathBuf};

/// 差异类型：新增 / 修改 / 内容一致（一致的文件无需写入）
pub const KIND_ADD: &str = "add";
pub const KIND_MODIFY: &str = "modify";
pub const KIND_SAME: &str = "same";

/// 备份目录（相对项目根）。放 `.ruyix/` 下：既在项目内便于找回，又不会污染仓库内容。
const BACKUP_ROOT: &str = ".ruyix/backups";

/// 单个文件的差异。before 为 `None` 表示这是个新文件。
#[derive(Debug, Clone, Serialize)]
pub struct FileChange {
    /// 相对项目根的路径（统一 `/` 分隔）
    pub path: String,
    /// add | modify | same
    pub kind: String,
    /// 项目里现在的内容；新文件为 null
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// 沙箱产物里的内容
    pub after: String,
}

/// 预览结果。UI 据此渲染勾选清单，用户确认后才调 [`apply`]。
#[derive(Debug, Clone, Serialize)]
pub struct Preview {
    pub run_id: String,
    /// 产物沙箱目录（只读展示，便于排查）
    pub sandbox_dir: String,
    pub project_root: String,
    pub changes: Vec<FileChange>,
    /// 若项目是 git 仓库且工作树脏，UI 需要提示"建议先提交，便于回滚"
    pub dirty: bool,
    /// 内容一致、无需写入的文件数
    pub same_count: usize,
}

/// 写回结果
#[derive(Debug, Clone, Serialize)]
pub struct ApplyResult {
    pub applied: Vec<String>,
    /// 跳过项（含原因）——包括用户未勾选的以外，主要是内容一致与非法路径
    pub skipped: Vec<Skipped>,
    /// 本次备份落到的目录；没有文件被覆盖时为 null
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Skipped {
    pub path: String,
    pub reason: String,
}

// ============================================
// 沙箱侧：枚举产物
// ============================================

/// 沙箱里有内容的文件（rel 路径，统一 `/`）+ 内容。跳过 `.git` 与非 UTF-8 二进制。
/// （stage.rs 的暂存目录枚举也走这里 —— 同一套跳过规则）
pub(crate) fn generated_files(dir: &Path) -> Result<Vec<(String, String)>, String> {
    let mut out: Vec<(String, String)> = Vec::new();
    collect(dir, Path::new(""), &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn collect(dir: &Path, rel: &Path, out: &mut Vec<(String, String)>) -> Result<(), String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("读取产物目录失败 {}: {e}", dir.display()))?;
    for ent in entries {
        let ent = ent.map_err(|e| format!("读取产物目录项失败: {e}"))?;
        let name = ent.file_name().to_string_lossy().to_string();
        // 沙箱里的 .git 是引擎为了 diff 建的，不是产物
        if rel.as_os_str().is_empty() && name == ".git" {
            continue;
        }
        let child_rel = if rel.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            rel.join(&name)
        };
        let path = ent.path();
        if path.is_dir() {
            collect(&path, &child_rel, out)?;
            continue;
        }
        let bytes =
            std::fs::read(&path).map_err(|e| format!("读取产物失败 {}: {e}", path.display()))?;
        let Ok(text) = String::from_utf8(bytes) else {
            continue; // 二进制产物不参与写回（没法做差异，也不该盲写）
        };
        out.push((to_slash(&child_rel), text));
    }
    Ok(())
}

fn to_slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// 路径是否安全：必须是相对路径、不含 `..`、不指向 `.git`
pub(crate) fn is_safe_rel(rel: &str) -> bool {
    if rel.is_empty() || rel.starts_with('/') || rel.starts_with('\\') {
        return false;
    }
    if rel.contains(':') {
        return false; // Windows 盘符
    }
    !rel.split(['/', '\\']).any(|c| c == ".." || c == ".git")
}

// ============================================
// 预览
// ============================================

pub fn preview(runs_root: &Path, run_id: &str, project_root: &Path) -> Result<Preview, String> {
    let sandbox = harness_engine::workspace::project_dir(runs_root, run_id)?;
    // 先校验目标项目：它不存在时用户什么都做不了，是最该先说的那句话
    if !project_root.is_dir() {
        return Err(format!("项目目录不存在: {}", project_root.display()));
    }
    if !sandbox.is_dir() {
        return Err(format!("这次运行没有产物目录: {}", sandbox.display()));
    }

    let mut changes = Vec::new();
    for (rel, after) in generated_files(&sandbox)? {
        if !is_safe_rel(&rel) {
            continue;
        }
        let target = project_root.join(&rel);
        let (kind, before) = match read_text(&target) {
            None => (KIND_ADD, None),
            Some(cur) if cur == after => (KIND_SAME, Some(cur)),
            Some(cur) => (KIND_MODIFY, Some(cur)),
        };
        changes.push(FileChange {
            path: rel,
            kind: kind.to_string(),
            before,
            after,
        });
    }

    let same_count = changes.iter().filter(|c| c.kind == KIND_SAME).count();
    Ok(Preview {
        run_id: run_id.to_string(),
        sandbox_dir: sandbox.to_string_lossy().to_string(),
        project_root: project_root.to_string_lossy().to_string(),
        changes,
        dirty: is_dirty(project_root),
        same_count,
    })
}

pub(crate) fn read_text(path: &Path) -> Option<String> {
    std::fs::read(path)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
}

/// git 仓库且工作树脏。非 git 仓库一律 false —— 不能因为没装 git / 没建仓就报脏。
pub(crate) fn is_dirty(project_root: &Path) -> bool {
    if !project_root.join(".git").exists() {
        return false;
    }
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(project_root)
        .stdin(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().lines().count() > 0,
        Err(_) => false,
    }
}

// ============================================
// 写回
// ============================================

/// 把沙箱产物写进真实项目。
///
/// `paths` 为要写的文件相对路径清单（来自用户在预览面板的勾选）；留空=一个都不写。
/// 内容一致的自动跳过（写了也没变化，徒增备份噪音）；命不中的路径按原因记进 skipped。
pub fn apply(
    runs_root: &Path,
    run_id: &str,
    project_root: &Path,
    paths: &[String],
    backup: bool,
) -> Result<ApplyResult, String> {
    let sandbox = harness_engine::workspace::project_dir(runs_root, run_id)?
        .canonicalize()
        .unwrap_or_else(|_| harness_engine::workspace::project_dir(runs_root, run_id).unwrap());
    if !sandbox.is_dir() {
        return Err(format!("这次运行没有产物目录: {}", sandbox.display()));
    }
    if !project_root.is_dir() {
        return Err(format!("项目目录不存在: {}", project_root.display()));
    }
    let real_root = project_root
        .canonicalize()
        .map_err(|e| format!("无法解析项目目录 {}: {e}", project_root.display()))?;

    // 硬约束 3：产物绝不能写进沙箱自身（沙箱通常是 <runs_root> 下，不在项目里；
    // 但若有人把 workspace_root 指到项目里，这里必须挡住）
    if sandbox.starts_with(&real_root) || sandbox == real_root {
        return Err(format!(
            "产物目录在项目内部，拒绝写回（会把上一次结果当成项目源码）: {}",
            sandbox.display()
        ));
    }

    let generated = generated_files(&sandbox)?;
    let mut applied = Vec::new();
    let mut skipped = Vec::new();
    let mut backup_dir: Option<PathBuf> = None;

    for rel in paths {
        // 安全判定先行：路径合法性优先于"它存不存在"，别让越界路径钻到后面去
        if !is_safe_rel(rel) {
            skipped.push(Skipped {
                path: rel.clone(),
                reason: "非法路径（绝对路径 / .. / .git）".to_string(),
            });
            continue;
        }
        let Some((_, content)) = generated.iter().find(|(p, _)| p == rel) else {
            skipped.push(Skipped {
                path: rel.clone(),
                reason: "这次运行的产物里没有这个文件".to_string(),
            });
            continue;
        };
        let target = real_root.join(rel);
        if let Some(cur) = read_text(&target) {
            if cur == *content {
                skipped.push(Skipped {
                    path: rel.clone(),
                    reason: "内容与项目现有文件一致".to_string(),
                });
                continue;
            }
            if backup {
                let dir = backup_dir
                    .get_or_insert_with(|| real_root.join(BACKUP_ROOT).join(backup_name(run_id)));
                let dst = dir.join(rel);
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("创建备份目录失败 {}: {e}", parent.display()))?;
                }
                std::fs::copy(&target, &dst).map_err(|e| {
                    format!(
                        "备份原文件失败 {} → {}: {e}",
                        target.display(),
                        dst.display()
                    )
                })?;
            }
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建目录失败 {}: {e}", parent.display()))?;
        }
        std::fs::write(&target, content)
            .map_err(|e| format!("写入失败 {}: {e}", target.display()))?;
        applied.push(rel.clone());
    }

    Ok(ApplyResult {
        applied,
        skipped,
        backup_dir: backup_dir.map(|d| d.to_string_lossy().to_string()),
    })
}

fn backup_name(run_id: &str) -> String {
    // 本地时间戳即可：备份只用于"刚才那次覆盖"的回滚，不需要跨时区语义。
    // 用 SystemTime 算偏移，避免引入 chrono（引擎也不依赖它）。
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86400) as i64;
    let rem = (secs % 86400) as u32;
    format!("{run_id}-{}", civil_date_time(days, rem))
}

///  Julian day number → 公历日期 + 当日秒数 → `YYYYMMDD-HHMMSS`
fn civil_date_time(days_since_epoch: i64, secs_of_day: u32) -> String {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y } as u32;
    let h = secs_of_day / 3600;
    let mi = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    format!("{year:04}{m:02}{d:02}-{h:02}{mi:02}{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ruyix-apply-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(p: &Path, body: &str) {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, body).unwrap();
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    /// 造一次假 run：runs/<id>/project/...
    fn fake_run(root: &Path, id: &str, files: &[(&str, &str)]) -> PathBuf {
        let sandbox = root.join(id).join("project");
        for (rel, body) in files {
            write(&sandbox.join(rel), body);
        }
        write(&sandbox.join(".git/HEAD"), "ref: refs/heads/main\n");
        sandbox
    }

    #[test]
    fn preview_classifies_add_modify_and_same() {
        let runs = temp("runs");
        let proj = temp("proj");
        fake_run(
            &runs,
            "r1",
            &[
                ("pom.xml", "new"),
                ("a.txt", "same-body"),
                ("docs/x.md", "doc"),
            ],
        );
        write(&proj.join("pom.xml"), "old");
        write(&proj.join("a.txt"), "same-body");

        let pv = preview(&runs, "r1", &proj).unwrap();
        let get = |p: &str| pv.changes.iter().find(|c| c.path == p).unwrap().clone();
        assert_eq!(get("pom.xml").kind, KIND_MODIFY);
        assert_eq!(get("pom.xml").before.as_deref(), Some("old"));
        assert_eq!(get("a.txt").kind, KIND_SAME);
        assert_eq!(get("docs/x.md").kind, KIND_ADD);
        assert_eq!(get("docs/x.md").before, None);
        assert_eq!(pv.same_count, 1);
        // .git 不能作为产物出现
        assert!(!pv.changes.iter().any(|c| c.path.starts_with(".git")));
    }

    #[test]
    fn apply_writes_selected_files_and_backs_up() {
        let runs = temp("runs2");
        let proj = temp("proj2");
        fake_run(
            &runs,
            "r2",
            &[("pom.xml", "new"), ("src/A.java", "class A {}")],
        );
        write(&proj.join("pom.xml"), "old");

        let res = apply(
            &runs,
            "r2",
            &proj,
            &["pom.xml".into(), "src/A.java".into()],
            true,
        )
        .unwrap();
        assert_eq!(res.applied.len(), 2);
        assert_eq!(read(&proj.join("pom.xml")), "new");
        assert_eq!(read(&proj.join("src/A.java")), "class A {}");

        let bak = res.backup_dir.as_ref().expect("覆盖了既有文件，应产生备份");
        assert_eq!(read(&Path::new(bak).join("pom.xml")), "old");
        // 新文件没有原文件可备份
        assert!(!Path::new(bak).join("src/A.java").exists());
    }

    #[test]
    fn apply_skips_identical_and_unknown_paths() {
        let runs = temp("runs3");
        let proj = temp("proj3");
        fake_run(&runs, "r3", &[("a.txt", "same")]);
        write(&proj.join("a.txt"), "same");

        let res = apply(
            &runs,
            "r3",
            &proj,
            &["a.txt".into(), "ghost.txt".into()],
            true,
        )
        .unwrap();
        assert!(res.applied.is_empty());
        assert_eq!(res.skipped.len(), 2);
        assert!(res.skipped.iter().any(|s| s.reason.contains("一致")));
        assert!(
            res.skipped
                .iter()
                .any(|s| s.reason.contains("没有这个文件"))
        );
        assert!(res.backup_dir.is_none(), "没覆盖任何文件就不该生成备份目录");
    }

    #[test]
    fn apply_rejects_path_traversal() {
        let runs = temp("runs4");
        let proj = temp("proj4");
        fake_run(&runs, "r4", &[("ok.txt", "x")]);
        // 预览里不该出现走私产物（这里由 is_safe_rel 兜底）
        assert!(is_safe_rel("ok.txt"));
        assert!(!is_safe_rel("../escape.txt"));
        assert!(!is_safe_rel("a/../../escape.txt"));
        assert!(!is_safe_rel("/abs.txt"));
        assert!(!is_safe_rel("C:/win.txt"));
        assert!(!is_safe_rel(".git/config"));

        let res = apply(&runs, "r4", &proj, &["../escape.txt".into()], true).unwrap();
        assert!(res.applied.is_empty());
        assert!(res.skipped[0].reason.contains("非法路径"));
        // 真的没在项目外面写出东西
        assert!(!proj.parent().unwrap().join("escape.txt").exists());
    }

    #[test]
    fn apply_refuses_writing_into_sandbox() {
        // 把 runs 根放到真实项目内部：模拟有人把 workspace_root 指到了项目目录下。
        // 这时"沙箱在项目里"，写回会产生"上一次产物变成项目源码"的污染，必须挡住。
        let proj = temp("proj5");
        let proj = proj.canonicalize().unwrap();
        let runs = proj.join("agent-runs");
        fake_run(&runs, "r5", &[("a.txt", "x")]);

        let err = apply(&runs, "r5", &proj, &["a.txt".into()], true).unwrap_err();
        assert!(err.contains("拒绝写回"), "{err}");
        assert!(!proj.join("a.txt").exists(), "被拒绝后不该写出任何文件");
    }

    #[test]
    fn errors_are_readable() {
        let runs = temp("runs6");
        let proj = temp("proj6");
        let err = preview(&runs, "nope", &proj).unwrap_err();
        assert!(err.contains("没有产物目录"), "{err}");

        let missing = temp("missing-proj");
        let absent = missing.join("absent");
        let err = preview(&runs, "r6", &absent).unwrap_err();
        assert!(err.contains("项目目录不存在"), "{err}");
    }

    #[test]
    fn backup_name_has_timestamp_shape() {
        let n = backup_name("20260919-150658-cloud-shop");
        assert!(n.starts_with("20260919-150658-cloud-shop-"), "{n}");
        // 尾段应是 14 位 YYYYMMDD-HHMMSS + 1 个短横线前导
        let tail = n.rsplit('-').next().unwrap();
        assert_eq!(tail.len(), 6, "{n}");
    }
}
