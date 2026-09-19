//! 会话 Agent 工具循环的暂存产物（`WritePolicy::Stage`，确认模式）。
//!
//! 引擎把 Agent 的 write/edit 收集到 `<项目>/.ruyix/stage/agent-<ts>/`
//! （`files/<相对路径>` + `manifest.json`）。确认模式下这些改动**不落盘**，
//! 由人看完差异、勾选后才经 [`apply`] 写进项目 —— 与 apply.rs 同一套安全约定：
//! 路径封闭（拒绝绝对路径 / `..` / `.git` / `.ruyix` 自身）、写前备份、绝不默认写。
//! `preview` 的 `before` 取项目**当前**内容（不是暂存时的快照）—— 用户在确认前
//! 手改过文件的话，看到的是真实差异。

use super::apply::{
    self, ApplyResult, FileChange, KIND_ADD, KIND_MODIFY, KIND_SAME, Preview, Skipped,
};
use std::path::{Path, PathBuf};

/// stage_id 形如 agent-20260919-153000：只允许 [A-Za-z0-9_.-]，防路径逃逸
fn stage_dir(project_root: &Path, stage_id: &str) -> Result<PathBuf, String> {
    if stage_id.is_empty()
        || !stage_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(format!("非法暂存 id: {stage_id}"));
    }
    Ok(project_root.join(".ruyix").join("stage").join(stage_id))
}

/// 暂存路径安全：继承 apply 的封闭规则，另拒绝写进 `.ruyix` 自身
/// （否则 Agent 可以覆盖自己的备份/暂存区，等于撤销安全网）
fn is_safe_stage_rel(rel: &str) -> bool {
    apply::is_safe_rel(rel) && rel != ".ruyix" && !rel.starts_with(".ruyix/")
}

pub fn preview(project_root: &Path, stage_id: &str) -> Result<Preview, String> {
    let stage = stage_dir(project_root, stage_id)?;
    let files = stage.join("files");
    if !project_root.is_dir() {
        return Err(format!("项目目录不存在: {}", project_root.display()));
    }
    if !files.is_dir() {
        return Err(format!(
            "暂存目录不存在（可能已被确认或清理）: {}",
            stage.display()
        ));
    }

    let mut changes = Vec::new();
    for (rel, after) in apply::generated_files(&files)? {
        if !is_safe_stage_rel(&rel) {
            continue;
        }
        let target = project_root.join(&rel);
        let (kind, before) = match apply::read_text(&target) {
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
    if changes.is_empty() {
        return Err("暂存目录里没有可写回的文件".into());
    }

    let same_count = changes.iter().filter(|c| c.kind == KIND_SAME).count();
    Ok(Preview {
        run_id: stage_id.to_string(),
        sandbox_dir: files.to_string_lossy().to_string(),
        project_root: project_root.to_string_lossy().to_string(),
        changes,
        dirty: apply::is_dirty(project_root),
        same_count,
    })
}

/// 把勾选的暂存文件写进项目（写前备份被覆盖的原文）。
pub fn apply(
    project_root: &Path,
    stage_id: &str,
    paths: &[String],
    backup: bool,
) -> Result<ApplyResult, String> {
    let stage = stage_dir(project_root, stage_id)?;
    let files = stage.join("files");
    if !files.is_dir() {
        return Err(format!("暂存目录不存在: {}", stage.display()));
    }
    if !project_root.is_dir() {
        return Err(format!("项目目录不存在: {}", project_root.display()));
    }
    let staged = apply::generated_files(&files)?;

    let mut applied = Vec::new();
    let mut skipped = Vec::new();
    let mut backup_dir: Option<PathBuf> = None;
    for rel in paths {
        if !is_safe_stage_rel(rel) {
            skipped.push(Skipped {
                path: rel.clone(),
                reason: "非法路径（绝对路径 / .. / .git / .ruyix）".to_string(),
            });
            continue;
        }
        let Some((_, content)) = staged.iter().find(|(p, _)| p == rel) else {
            skipped.push(Skipped {
                path: rel.clone(),
                reason: "暂存目录里没有这个文件".to_string(),
            });
            continue;
        };
        let target = project_root.join(rel);
        if let Some(cur) = apply::read_text(&target)
            && cur == *content
        {
            skipped.push(Skipped {
                path: rel.clone(),
                reason: "项目内容已一致，无需写入".to_string(),
            });
            continue;
        }
        if backup && target.exists() {
            let dir = backup_dir.get_or_insert_with(|| {
                let d = project_root.join(".ruyix").join("backups").join(format!(
                    "{stage_id}-{}",
                    harness_engine::workspace::now_compact()
                ));
                let _ = std::fs::create_dir_all(&d);
                d
            });
            let to = dir.join(rel);
            if let Some(parent) = to.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Some(cur) = apply::read_text(&target) {
                std::fs::write(&to, cur).map_err(|e| format!("备份 {rel} 失败：{e}"))?;
            }
        }
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&target, content)
            .map_err(|e| format!("写入 {rel} 失败：{e}（之前的文件已写入，见备份目录）"))?;
        applied.push(rel.clone());
    }
    Ok(ApplyResult {
        applied,
        skipped,
        backup_dir: backup_dir.map(|d| d.to_string_lossy().to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ruyix-stage-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn seed_stage(root: &Path, id: &str, files: &[(&str, &str)]) {
        let s = stage_dir(root, id).unwrap().join("files");
        for (rel, content) in files {
            let p = s.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
    }

    #[test]
    fn stage_id_rejects_path_escape() {
        assert!(stage_dir(Path::new("x"), "../evil").is_err());
        assert!(stage_dir(Path::new("x"), "agent-ok-1").is_ok());
    }

    /// preview 的 before 取项目当前内容（不是暂存快照）—— 确认前手改过文件能看到真实差异
    #[test]
    fn preview_shows_live_before_and_kinds() {
        let root = dir("pv");
        std::fs::write(root.join("keep.txt"), "old").unwrap();
        seed_stage(
            &root,
            "agent-t1",
            &[("keep.txt", "new"), ("fresh.txt", "hi")],
        );
        let pv = preview(&root, "agent-t1").unwrap();
        assert_eq!(pv.changes.len(), 2);
        let keep = pv.changes.iter().find(|c| c.path == "keep.txt").unwrap();
        assert_eq!(keep.kind, KIND_MODIFY);
        assert_eq!(keep.before.as_deref(), Some("old"));
        let fresh = pv.changes.iter().find(|c| c.path == "fresh.txt").unwrap();
        assert_eq!(fresh.kind, KIND_ADD);
        assert!(fresh.before.is_none());
    }

    /// 落盘：只写勾选的；一致跳过；覆盖先备份；.ruyix 自身拒绝
    #[test]
    fn apply_writes_selected_with_backup_and_blocks_ruyix() {
        let root = dir("ap");
        std::fs::write(root.join("keep.txt"), "old").unwrap();
        seed_stage(
            &root,
            "agent-t2",
            &[
                ("keep.txt", "new"),
                ("fresh.txt", "hi"),
                (".ruyix/backups/x", "evil"),
            ],
        );
        let res = apply(
            &root,
            "agent-t2",
            &[
                "keep.txt".into(),
                "fresh.txt".into(),
                ".ruyix/backups/x".into(),
            ],
            true,
        )
        .unwrap();
        assert_eq!(res.applied, vec!["keep.txt", "fresh.txt"]);
        assert_eq!(res.skipped.len(), 1);
        assert_eq!(
            std::fs::read_to_string(root.join("keep.txt")).unwrap(),
            "new"
        );
        assert!(res.backup_dir.is_some());
        let backup = PathBuf::from(res.backup_dir.clone().unwrap());
        assert_eq!(
            std::fs::read_to_string(backup.join("keep.txt")).unwrap(),
            "old"
        );

        // 内容一致时再写 → 跳过
        let again = apply(&root, "agent-t2", &["keep.txt".into()], true).unwrap();
        assert!(again.applied.is_empty());
        assert_eq!(again.skipped[0].reason, "项目内容已一致，无需写入");
        let _ = std::fs::remove_dir_all(&root);
    }
}
