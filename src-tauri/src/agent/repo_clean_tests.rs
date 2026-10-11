//! **A3/A4 的可重复执行版本**（v1.0.0 P3 的旗舰判据）。
//!
//! 判据（预注册，跑之前钉死）：
//!
//! ① 跑完一轮"agent 会做的事"之后，**用户项目里新增的文件恰好只有用户自己要的那一次写回**；
//!    项目里不出现 `.ruyix`、不出现 `target/`、不出现 `verify/`；
//! ② 那些状态全部出现在**便携根**的 `projects/<key>/` 里：
//!    `stage/`（暂存）· `backups/`（写前备份）· `sessions/`（会话）· `env/`（安装留痕）·
//!    `proc/`（托管进程日志）· `verify/target`（验证产物）· `project.toml`（自证）。
//!
//! 为什么值得一条真跑的测试，而不是"看代码改对了"：这七类写入里有五类由**引擎**（另一个 crate）
//! 写，漏掉一处不会报错 —— 只会让用户仓库里悄悄多出一个目录。这种失败静默、致命、且正好是这一版
//! 存在的理由，所以判据必须是"真跑一次、真数一遍"。
//!
//! 与 `scripts/repo-clean-scenario.mjs` 的分工：那个脚本给**真机**用（快照 + 人工跑一轮 + 对比 +
//! 自检，连 git 状态一起看）；这里是它的**机器版**，锁进 `cargo test`，每次提交都会跑。

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ruyix-clean-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
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

    /// 目录下的**文件**相对路径集合（只关心"多出来什么"，不关心内容）
    fn files(dir: &Path) -> BTreeSet<String> {
        fn walk(root: &Path, cur: &Path, out: &mut BTreeSet<String>) {
            let Ok(rd) = std::fs::read_dir(cur) else {
                return;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(root, &p, out);
                } else if let Ok(rel) = p.strip_prefix(root) {
                    out.insert(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        let mut out = BTreeSet::new();
        walk(dir, dir, &mut out);
        out
    }

    /// 一轮 agent 的全部落点，逐个走真代码路径（不是"读代码觉得对"）
    #[test]
    fn one_agent_round_leaves_the_project_clean() {
        let root = tmp("root");
        let proj = tmp("proj");
        crate::paths::set_test_root(&root);
        let p = crate::paths::current();
        let proj_s = proj.to_string_lossy().to_string();

        // 一个真实的小 crate：verify 会**真跑一次 cargo check**（判据 ② 的 verify 那一格要真产物）
        write(
            &proj.join("Cargo.toml"),
            "[package]\nname = \"clean-probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        );
        write(
            &proj.join("src/main.rs"),
            "fn main() { println!(\"hi\"); }\n",
        );
        write(&proj.join("keep.txt"), "old\n");
        let before = files(&proj);

        // ---- ① 确认模式：引擎先把改动写进暂存区（这里直接造出引擎会造的那份目录结构），
        //      再由宿主 `stage::apply` 写回用户勾选的文件（= 用户在差异面板点了确认）----
        let stage_id = "agent-scenario-1";
        let stage_files = p
            .project_bucket(&proj_s, "stage")
            .join(stage_id)
            .join("files");
        write(&stage_files.join("keep.txt"), "new\n");
        write(&stage_files.join("added.txt"), "brand new\n");
        let applied = crate::agent::stage::apply(
            &proj,
            stage_id,
            &["keep.txt".into(), "added.txt".into()],
            true,
        )
        .expect("写回应当成功");
        assert_eq!(applied.applied, vec!["keep.txt", "added.txt"]);

        // ---- ② 会话存档 ----
        let sess = crate::agent::sessions::Session {
            id: "20260924-235959".to_string(),
            title: "场景会话".to_string(),
            created_at: harness_engine::workspace::now_iso(),
            updated_at: harness_engine::workspace::now_iso(),
            messages: Vec::new(),
        };
        crate::agent::sessions::save(&sess, &proj_s).expect("会话应当落盘");

        // ---- ③ 环境留痕（安装动作写的那一行）----
        let rec = crate::agent::env_setup::record_path(&proj);
        write(&rec, "{\"tool\":\"probe\"}\n");

        // ---- ④ 托管进程：日志按 pid 落在项目桶的 proc/ ----
        let state_dir = p.project_dir(&proj_s);
        let spec = harness_engine::proc::StartSpec {
            cmd: if cfg!(windows) {
                "ping -n 20 127.0.0.1".into()
            } else {
                "sleep 20".into()
            },
            ready_cmd: None,
            ready_timeout_secs: Some(10),
            keep_alive: true,
        };
        let started =
            harness_engine::proc::start(&proj, &spec, 4, 10, &state_dir).expect("托管进程应当起来");
        let _ = harness_engine::proc::stop(&proj, &started.info.handle);

        // ---- ⑤ 验证：真跑一次（产物必须落在项目桶的 verify/，不是项目里）----
        let mut cfg = harness_engine::config::AppConfig::default();
        cfg.sandbox.mode = "off".to_string(); // 本机可能没有 docker：判据要可重复
        cfg.project_state_root = state_dir.to_string_lossy().to_string();
        let flag: harness_engine::exec::CancelFlag =
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let report = harness_engine::verify::run(&cfg, "scenario", &proj, &flag);
        assert!(
            report.checks.iter().any(|c| c.kind == "syntax"),
            "验证应当至少跑了语法层：{report:?}"
        );

        // ---- 项目桶自证 ----
        assert!(
            p.stamp_project(&proj_s).is_some(),
            "应当在项目桶里写 project.toml 自证"
        );

        // ================= 判定 ①：项目里只多了用户要的那一个文件 =================
        let after = files(&proj);
        let added: Vec<String> = after.difference(&before).cloned().collect();
        assert_eq!(
            added,
            vec!["added.txt".to_string()],
            "用户项目里多出来的文件必须只有那一次写回（其余全是残留）"
        );
        for bad in [".ruyix", "target", "verify"] {
            assert!(
                !after.iter().any(|f| f.starts_with(bad)),
                "用户项目里出现了 {bad}：{after:?}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(proj.join("keep.txt")).unwrap(),
            "new\n",
            "写回的内容要真的生效"
        );

        // ================= 判定 ②：状态全在便携根的项目桶里 =================
        let bucket = p.project_dir(&proj_s);
        assert!(
            bucket.starts_with(root.join("projects")),
            "项目桶必须在便携根里：{}",
            bucket.display()
        );
        for (what, path) in [
            ("暂存", stage_files.join("keep.txt")),
            ("自证", bucket.join("project.toml")),
            ("会话", bucket.join("sessions").join("20260924-235959.json")),
            ("环境留痕", bucket.join("env").join("installs.jsonl")),
        ] {
            assert!(path.is_file(), "{what}应当在这里：{}", path.display());
        }
        assert!(
            !applied.backup_dir.clone().unwrap_or_default().is_empty(),
            "覆盖既有文件应当产生备份"
        );
        let backups: Vec<_> = std::fs::read_dir(bucket.join("backups"))
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        assert_eq!(backups.len(), 1, "应当恰好有一个备份批次");
        assert_eq!(
            std::fs::read_to_string(backups[0].join("keep.txt")).unwrap(),
            "old\n",
            "备份里应当是覆盖前的内容"
        );
        let proc_logs: Vec<_> = std::fs::read_dir(bucket.join("proc"))
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        assert!(!proc_logs.is_empty(), "托管进程日志应当落在这里");
        assert!(
            bucket.join("verify").join("target").is_dir(),
            "cargo 的 target 必须落在项目桶的 verify/ 里（绝对路径注入）"
        );

        // 收尾：别把临时目录攒下来（顺带证明"删文件夹即净"）
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&proj);
    }
}
