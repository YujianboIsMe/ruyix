//! `ContextLedger` 的黄金测试：上游 `experiments/export_decision_trace.py` 产出的
//! `ledger_golden_seed{1,3}.json` 两份夹具，共 143 / 151 个事件 —— **决策逐条对齐** +
//! 四类计数逐项相等。
//!
//! 从 `agent/ledger.rs` 末尾搬出来的（2026-09-28）：测试单独成文件（`doc/编码规范.md`），
//! 内容是**逐字搬**的 —— 里面的 JSON 片段是数据，手工 dedent 会改数据。

    /// 侧存形态下小计要报**磁盘**占用并标明形态（老形态报内存 ⇒ 恒 0.0KB，看着像没存）。
    #[test]
    fn the_stats_line_labels_whether_the_bytes_are_memory_or_disk() {
        let l = ContextLedger::new(1 << 20);
        let line = l.render_stats();
        assert!(line.contains("（内存）"), "没挂侧存时按内存计：{line}");
        assert!(line.contains("条/"), "{line}");
    }

    use super::*;

    fn read_call(path: &str, offset: Option<usize>, limit: Option<usize>) -> LedgerCall {
        classify(&Action::Read(ReadSpec {
            path: path.to_string(),
            offset,
            limit,
        }))
        .expect("read 永远纯")
    }

    fn exec_call(cmd: &str) -> Option<LedgerCall> {
        classify(&Action::Execute(cmd.to_string(), None))
    }

    /// 版本向量的手搓（等价于"世界告诉我这些资源的版本"）
    fn versions(pairs: &[(&str, Ver)], repo: Option<(&str, bool)>) -> VersionVec {
        VersionVec {
            repo: repo.map(|(h, d)| (h.to_string(), d)),
            paths: pairs.iter().map(|(p, v)| (p.to_string(), *v)).collect(),
            unknown: false,
        }
    }

    /// 判据 4：**规范化把等价的调用映到同一个键**（`./a` ≡ `a`、缺省参数 ≡ 显式缺省）。
    #[test]
    fn normalization_maps_equivalent_calls_to_one_key() {
        let a = read_call("ui/main.js", None, None);
        // 三种拼法：`./` 前缀、反斜杠、重复分隔符
        for spelling in ["ui/./main.js", "ui\\main.js", "ui//main.js"] {
            let b = read_call(spelling, None, None);
            assert_eq!(
                a.digest, b.digest,
                "「{spelling}」该与「ui/main.js」同一个键"
            );
        }
        // 项目根的三种拼法都该落到同一个键（read "." 是模型探索的第一步）
        let root = read_call(".", None, None);
        for spelling in ["./", "./.", ".\\", "  .  "] {
            assert_eq!(
                root.digest,
                read_call(spelling, None, None).digest,
                "「{spelling}」该与「.」同一个键"
            );
        }
        // 省缺省参数这一维在 ruyix 侧由**类型**一次性归一（`limit: null` 经 serde
        // 就是 `None`，与不传同一个值）—— 字符串层不需要再归一，所以这里只钉住
        // "整份读 ≠ 窗口读" 这条不许被归一的边界（归一过头 = 少给内容）。
        let whole = read_call("a.rs", None, None);
        let win = read_call("a.rs", Some(1), Some(40));
        assert_ne!(whole.digest, win.digest, "整份读与窗口读是**不同**的调用");
        assert_ne!(
            win.digest,
            read_call("a.rs", Some(2), Some(40)).digest,
            "不同窗口是不同的调用"
        );
        assert_ne!(
            whole.digest,
            read_call("b.rs", None, None).digest,
            "不同文件是不同的调用"
        );
        // 命令里的**路径**也要归一 —— 这条是**黄金测试**抓出来的：上游 `canonical_args`
        // 会把参数里的路径归一，我们第一版没有 ⇒ 两个 `git_diff` 事件对不上（差一次去重）。
        let d1 = exec_call("git diff -- ./src/a.py").unwrap();
        assert_eq!(
            d1.digest,
            exec_call("git diff -- src/a.py").unwrap().digest,
            "命令里的 `./` 前缀不该造出一个新键"
        );
        assert_eq!(d1.resources, vec!["src/a.py".to_string()]);
        assert_eq!(
            exec_call("git grep -n \"x\" -- \"./src\"").unwrap().digest,
            exec_call("git grep -n \"x\" -- src").unwrap().digest,
            "带引号的路径与不带的是同一个资源"
        );
        // 命令：尾随空白 / `2>nul` / 命令名大小写都归一
        let g = exec_call("git grep -n foo -- src/a.rs").unwrap();
        for spelling in [
            "git grep -n foo -- src/a.rs 2>nul",
            "  git   grep  -n  foo --  src/a.rs  ",
            "GIT grep -n foo -- src/a.rs",
        ] {
            let h = exec_call(spelling).unwrap_or_else(|| panic!("「{spelling}」该是纯命令"));
            assert_eq!(g.digest, h.digest, "「{spelling}」该与基础形状同键");
        }
    }

    /// 判据 5：**整文件读 ⊇ 区间读**（同一版本下，整份顶替片段）。
    #[test]
    fn whole_file_read_subsumes_range_read_under_same_version() {
        let mut l = ContextLedger::new(1 << 20);
        let whole = read_call("a.rs", None, None);
        let v = versions(&[("a.rs", Ver::File(1, 10))], None);
        l.record(whole, Body::Inline("全文".into()), v.clone(), 3);

        let hit = l.lookup(&read_call("a.rs", Some(10), Some(20)), &v);
        assert!(hit.is_some(), "整份读过之后，区间读该被它覆盖");
        assert_eq!(hit.unwrap().step, 3);

        // 反向不成立：区间读**不**覆盖整份读（那会少给内容）
        let mut l2 = ContextLedger::new(1 << 20);
        l2.record(
            read_call("a.rs", Some(10), Some(20)),
            Body::Inline("片段".into()),
            v.clone(),
            1,
        );
        assert!(
            l2.lookup(&read_call("a.rs", None, None), &v).is_none(),
            "片段不许顶替整份 —— 那会让模型少看到内容"
        );
        // 包含关系的方向性：更小的窗口被覆盖，更大的不
        let mut l3 = ContextLedger::new(1 << 20);
        l3.record(
            read_call("a.rs", Some(1), Some(100)),
            Body::Inline("100 行".into()),
            v.clone(),
            1,
        );
        assert!(
            l3.lookup(&read_call("a.rs", Some(11), Some(20)), &v)
                .is_some()
        );
        assert!(
            l3.lookup(&read_call("a.rs", Some(11), Some(500)), &v)
                .is_none()
        );
        // grep：包含键与上游同形（`grep::<pattern>::<path>`）——
        // **同一 (pattern, path)** 靠精确 digest 命中；换路径**不算**覆盖（与上游口径一致：
        // 跨路径作用域的包含不做，见模块头的"包含关系"一行）。
        let mut l4 = ContextLedger::new(1 << 20);
        let all = exec_call("git grep -n delta").unwrap();
        let rv = versions(&[], Some(("HEAD1", false)));
        l4.record(all, Body::Inline("全仓命中".into()), rv.clone(), 2);
        let same = exec_call("git grep -n delta").unwrap();
        assert!(l4.lookup(&same, &rv).is_some(), "同一条命令该命中");
        let other_path = exec_call("git grep -n delta -- src/core").unwrap();
        assert!(
            l4.lookup(&other_path, &rv).is_none(),
            "换了路径范围就是另一次调用（上游也不覆盖它）"
        );
        // 但"全仓 grep"的**记录**仍然是可复用的：紧接一次同命令的 grep 命中它
        assert!(
            l4.lookup(&exec_call("git grep -n delta").unwrap(), &rv)
                .is_some()
        );
    }

    /// 判据 6：**纯工具 + 版本未变**才允许复用；版本一变必须重执行。
    #[test]
    fn dedup_requires_pure_tool_and_unchanged_version() {
        let mut l = ContextLedger::new(1 << 20);
        let call = read_call("a.rs", None, None);
        let v1 = versions(&[("a.rs", Ver::File(1, 10))], None);
        l.record(
            call.clone(),
            Body::Inline("v1 的内容".into()),
            v1.clone(),
            1,
        );
        assert!(l.lookup(&call, &v1).is_some(), "版本没变 ⇒ 复用");

        let v2 = versions(&[("a.rs", Ver::File(2, 10))], None);
        assert!(l.lookup(&call, &v2).is_none(), "mtime 变了 ⇒ 必须重执行");
        let v3 = versions(&[("a.rs", Ver::File(1, 11))], None);
        assert!(l.lookup(&call, &v3).is_none(), "size 变了 ⇒ 必须重执行");

        // 写了一笔（覆盖层内容哈希）也算版本变化
        let v4 = versions(&[("a.rs", Ver::Overlay(7))], None);
        assert!(l.lookup(&call, &v4).is_none());

        // 不纯的动作**根本不进账本**（write / 后台 / 句柄 / connect / 构建命令）
        assert!(classify(&Action::Execute("cargo build".into(), None)).is_none());
        assert!(classify(&Action::Execute("npm run build".into(), None)).is_none());
        assert!(classify(&Action::Execute("git commit -m x".into(), None)).is_none());
        assert!(classify(&Action::Execute("dir > out.txt".into(), None)).is_none());
        assert!(classify(&Action::Execute("dir | tee log.txt".into(), None)).is_none());
        assert!(classify(&Action::Execute("git status && rm -rf x".into(), None)).is_none());
        for name in KNOWN_EFFECTFUL {
            let cmd = format!("{name} --version");
            assert!(
                classify(&Action::Execute(cmd.clone(), None)).is_none(),
                "「{cmd}」不该进账本"
            );
        }
        // 白名单内的只读命令进账本
        for cmd in [
            "dir",
            "git status",
            "git grep -n x -- src",
            "findstr /n x src\\a.rs",
            "where cargo",
            "git grep -n x -- src | head -20",
        ] {
            assert!(
                classify(&Action::Execute(cmd.into(), None)).is_some(),
                "「{cmd}」该是纯命令"
            );
        }
    }

    /// 判据 7：**版本拿不到 ⇒ 永不去重**（fail-safe）。宁可多跑一次，不许给错答案。
    #[test]
    fn unknown_version_never_dedups() {
        let mut l = ContextLedger::new(1 << 20);
        let call = read_call("a.rs", None, None);
        let mut unknown = versions(&[("a.rs", Ver::File(1, 10))], None);
        unknown.unknown = true;

        // 拿不到版本的那次**不记账**
        l.record(
            call.clone(),
            Body::Inline("内容".into()),
            unknown.clone(),
            1,
        );
        assert_eq!(l.stats().entries, 0, "版本不可判的条目不该入账");

        // 记过一条之后，来一次"版本不可判"的查询也不许命中
        let good = versions(&[("a.rs", Ver::File(1, 10))], None);
        l.record(call.clone(), Body::Inline("内容".into()), good, 1);
        assert!(l.lookup(&call, &unknown).is_none(), "查不到版本 ⇒ 执行");

        // 文件不存在（Missing）也一样：下次它可能出现
        let missing = versions(&[("a.rs", Ver::Missing)], None);
        assert!(l.lookup(&call, &missing).is_none());

        // 资源一个都认不出来的 execute（没有路径、也不是仓库）⇒ 不可判
        let bare = exec_call("dir").unwrap();
        assert!(bare.resources.is_empty());
        let snap = snapshot(Path::new("."), &bare, &BTreeMap::new(), 0, &None);
        assert!(snap.unknown, "无资源可判 ⇒ unknown（宁可执行）");
    }

    /// 判据 8：**命中返回逐字节一致的结果，并且被标注**（模型必须知道这是复用）。
    #[test]
    fn dedup_hit_returns_byte_identical_result_and_marks_it() {
        let mut l = ContextLedger::new(1 << 20);
        let call = read_call("a.rs", None, None);
        let body = "--- a.rs ---\nfn main() {}\n（尾部有记号 UNIQ-42）";
        let v = versions(&[("a.rs", Ver::File(9, 42))], None);
        l.record(call.clone(), Body::Inline(body.to_string()), v.clone(), 7);

        let hit = l.lookup(&call, &v).expect("该命中");
        assert_eq!(
            hit.body.text(None).unwrap(),
            body,
            "复用的必须是**逐字节相同**的原文"
        );
        assert_eq!(hit.step, 7, "标注行要能说出\"第 N 轮已执行过\"");

        // 标注行（架构文档 §4 的原话）
        let note = LedgerCall::reuse_note(hit.step);
        assert!(note.contains("第 7 轮已执行过同一纯调用"));
        assert!(note.contains("资源版本未变"));
        assert!(note.contains("本次直接复用，未重跑"));
        let returned = format!("{}{note}", hit.body.text(None).unwrap());
        assert!(
            returned.starts_with(body),
            "标注是**尾部追加**的，正文一字不动"
        );
    }

    /// 结果侧存有界：挤掉的条目**同时从索引里摘掉**（否则命中时给不出内容）。
    #[test]
    fn result_store_eviction_is_fail_safe() {
        let mut l = ContextLedger::new(64);
        let v = versions(&[("a.rs", Ver::File(1, 1))], None);
        for i in 0..5 {
            let c = read_call(&format!("f{i}.rs"), None, None);
            let vs = versions(&[(format!("f{i}.rs").as_str(), Ver::File(1, 1))], None);
            l.record(c, Body::Inline("x".repeat(40)), vs, i as u32);
        }
        assert!(l.stats().entries <= 2, "上限该把老的挤掉：{:?}", l.stats());
        let old = read_call("f0.rs", None, None);
        assert!(l.lookup(&old, &v).is_none(), "被挤掉 = 没记过 ⇒ 下次真执行");
    }

    /// 仪器（审计）：唯一 / 仍重复 / 版本失效 三类分开数 —— 这是 P2 的验收读数。
    #[test]
    fn audit_counts_redundant_and_stale_separately() {
        let mut a = ExecAudit::default();
        let v1 = versions(&[("a.rs", Ver::File(1, 1))], None);
        let v2 = versions(&[("a.rs", Ver::File(2, 1))], None);
        let d = read_call("a.rs", None, None).digest;
        assert_eq!(a.classify(d, &v1, true), ExecKind::Unique);
        assert_eq!(a.classify(d, &v1, true), ExecKind::Redundant);
        assert_eq!(a.classify(d, &v2, true), ExecKind::Stale);
        assert_eq!(a.classify(0, &v1, false), ExecKind::Effectful);
        assert_eq!((a.unique, a.redundant, a.stale, a.effectful), (1, 1, 1, 1));
    }

    /// 快照（唯一的 IO 点）：文件变了/自己写了/目录写了，版本都要动。
    #[test]
    fn snapshot_sees_disk_changes_and_our_own_writes() {
        let d = std::env::temp_dir().join(format!("ruyix-ledger-snap-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("a.rs");
        std::fs::write(&f, "one").unwrap();

        let call = read_call("a.rs", None, None);
        let empty = BTreeMap::new();
        let first = snapshot(&d, &call, &empty, 0, &None);
        assert_eq!(first.get("a.rs"), Some(Ver::File(first_file_mtime(&f), 3)));

        // 内容变了（改大小 ⇒ 一定变）
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&f, "one-two-three").unwrap();
        let second = snapshot(&d, &call, &empty, 0, &None);
        assert_ne!(first, second, "文件变了 ⇒ 版本必须变");

        // 我们自己的写入（覆盖层）也算版本变化
        let mut ov = BTreeMap::new();
        ov.insert("a.rs".to_string(), "我在本会话里写过它".to_string());
        let third = snapshot(&d, &call, &ov, 0, &None);
        assert_ne!(second, third);

        // 目录读带写入代次：本 run 写过任何文件 ⇒ 目录版本也变
        let dir_call = read_call(".", None, None);
        let g0 = snapshot(&d, &dir_call, &empty, 0, &None);
        let g1 = snapshot(&d, &dir_call, &empty, 3, &None);
        assert_ne!(g0, g1, "写过东西之后，目录读的旧结果不再可信");

        let _ = std::fs::remove_dir_all(&d);
    }

    fn first_file_mtime(p: &Path) -> u64 {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    // ------------------------------------------------------------------ 黄金测试

    /// A/B 读数的口径：**被既有记录覆盖**的执行也算"浪费"（digest 不同、但内容已被覆盖）。
    ///
    /// 上游的 digest 口径数不出这一类（黄金测试对的就是那个口径），而 A/B 要量的正是
    /// "对照臂白跑了多少次" —— 真 run 里的主要浪费恰恰是"先整份读、再读一个窗口"。
    #[test]
    fn the_audit_counts_a_containment_covered_execution_as_wasted() {
        let vf = |n: u64| versions(&[("a.rs", Ver::File(n, 100))], None);
        let whole = read_call("a.rs", None, None);
        let win = read_call("a.rs", Some(10), Some(20));

        // 第一次整份读 ⇒ 唯一执行；再读一个窗口（digest 不同但被覆盖）⇒ 浪费
        let mut a = ExecAudit::default();
        assert_eq!(a.classify_exec(&whole, &vf(1), true), ExecKind::Unique);
        assert_eq!(a.classify_exec(&win, &vf(1), true), ExecKind::Redundant);
        // 不纯的执行永远算新动作
        assert_eq!(a.classify_exec(&win, &vf(1), false), ExecKind::Effectful);

        // 只读过窄窗口时，远离它的一段是真新信息 ⇒ 唯一
        let mut b = ExecAudit::default();
        assert_eq!(b.classify_exec(&win, &vf(1), true), ExecKind::Unique);
        let far = read_call("a.rs", Some(500), Some(20));
        assert_eq!(b.classify_exec(&far, &vf(1), true), ExecKind::Unique);

        // 同 digest、版本变了 ⇒ 版本失效重执行（**必要**的，不算浪费）
        let mut c = ExecAudit::default();
        assert_eq!(c.classify_exec(&win, &vf(1), true), ExecKind::Unique);
        assert_eq!(c.classify_exec(&win, &vf(2), true), ExecKind::Stale);
    }

    /// **黄金测试**（P2 的硬标准）：把上游 `experiments/export_decision_trace.py` 导出的
    /// 决策轨迹喂给 Rust 端口，**逐条**核对决策，再核对四类计数。
    ///
    /// 只比决策、不比字节（需求 §5）：轨迹里 143 / 151 个事件，每一个都要落在同一条决策上；
    /// 计数逐项相等。夹具是**生成物**（在 context-algorithm 根目录复跑那个脚本即可再生）。
    #[test]
    fn golden_trace_decisions_align_with_upstream() {
        for (name, doc) in [
            (
                "seed1",
                include_str!("../../../tests/fixtures/ledger_golden_seed1.json"),
            ),
            (
                "seed3",
                include_str!("../../../tests/fixtures/ledger_golden_seed3.json"),
            ),
        ] {
            replay_golden(name, doc);
        }
    }

    /// 轨迹里那一动作 → ruyix 的动作（`write` 也建出来：它是**不纯**的那些的代表）
    fn golden_action(r: &serde_json::Value) -> Action {
        let tool = r["tool"].as_str().unwrap_or_default();
        let args = &r["args"];
        match tool {
            "read" => Action::Read(ReadSpec {
                path: args["path"].as_str().unwrap_or_default().to_string(),
                offset: args["offset"].as_u64().map(|v| v as usize),
                limit: args["limit"].as_u64().map(|v| v as usize),
            }),
            "execute" => {
                Action::Execute(args["cmd"].as_str().unwrap_or_default().to_string(), None)
            }
            "write" => Action::Write(WriteSpec {
                path: args["path"].as_str().unwrap_or_default().to_string(),
                body: WriteBody::Content(String::new()),
            }),
            other => panic!("轨迹里有没映射过的形状：{other}"),
        }
    }

    /// 轨迹给的资源版本 → 版本向量。
    ///
    /// 上游的版本是一个整数，Rust 的版本标签是 `(mtime, size)` —— 这里把整数塞进第一位：
    /// 版本标签在本模块里**只用于相等判定**（"还是不是同一版"），这个映射足够且诚实。
    /// `unknown` 恒为 false：轨迹本身**就是**版本来源，不存在"拿不到版本"。
    fn golden_versions(resources: &serde_json::Value) -> VersionVec {
        let mut v = VersionVec {
            repo: None,
            paths: Vec::new(),
            unknown: false,
        };
        if let Some(obj) = resources.as_object() {
            for (k, val) in obj {
                let n = val.as_u64().unwrap_or(1);
                v.paths.push((k.clone(), Ver::File(n, 0)));
            }
        }
        v
    }

    fn replay_golden(name: &str, doc: &str) {
        let doc: serde_json::Value = serde_json::from_str(doc).expect("夹具该是合法 JSON");
        let events = doc["events"].as_array().expect("夹具该有 events");
        // 0 = 结果侧存不限 —— 上游没有容量上限，容量一挤就会多出"重执行"的决策，
        // 那对不上不是端口错了、是**夹具与端口的容量假设不同**（P2 内存侧存有上限是刻意的，
        // 但黄金测试要量的是决策逻辑，不是容量策略）。
        let mut l = ContextLedger::new(0);
        let mut mismatches: Vec<String> = Vec::new();

        for (n, e) in events.iter().enumerate() {
            let step = e["step"].as_u64().unwrap_or(0) as u32;
            let kind = e["kind"].as_str().unwrap_or("?");
            let action = golden_action(&e["ruyix"]);
            let versions = golden_versions(&e["resources"]);
            let want_dedup = e["decision"].as_str() == Some("dedup");
            let call = classify(&action);
            let where_ = format!("第 {n} 个事件（step {step} {kind} {}）", e["tool"]);

            match (&call, want_dedup) {
                (Some(c), true) => {
                    if l.lookup(c, &versions).is_none() {
                        mismatches.push(format!(
                            "{where_}：上游**命中**了（复用 {}），我们没命中",
                            e["reused_ref"].as_str().unwrap_or("-")
                        ));
                    } else {
                        l.note_hit_for(c, &versions);
                    }
                }
                (Some(c), false) => {
                    if l.lookup(c, &versions).is_some() {
                        mismatches.push(format!("{where_}：上游**执行**了，我们却想复用"));
                    }
                    l.audit.classify(c.digest, &versions, true);
                    l.record(
                        c.clone(),
                        Body::Inline(format!("<body {n}>")),
                        versions.clone(),
                        step,
                    );
                }
                (None, true) => mismatches.push(format!(
                    "{where_}：上游把它当**纯工具**命中了，我们的白名单不认"
                )),
                (None, false) => {
                    l.audit.classify(0, &versions, false); // 不纯：每次执行都算一次新动作
                }
            }
        }

        let up = &doc["counts"];
        let s = l.stats();
        let up_n = |k: &str| up[k].as_u64().unwrap_or(0);
        // **先报逐条的对齐结果**：计数对不上时，错在哪几条比"差 2"有用得多
        assert!(
            mismatches.is_empty(),
            "{name}：逐条决策对齐失败（{} 处）：\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
        // 上游的 `unique_exec` 把**不纯的执行**也算进去了（"每次执行都是新动作"）⇒ 对账时要加上
        assert_eq!(
            s.unique + s.effectful,
            up_n("unique_exec"),
            "{name}：唯一执行数对不上（我们 纯 {} + 不纯 {}）",
            s.unique,
            s.effectful
        );
        assert_eq!(
            s.redundant,
            up_n("redundant_exec"),
            "{name}：仍重复执行数对不上"
        );
        assert_eq!(
            s.stale,
            up_n("stale_exec"),
            "{name}：版本失效重执行数对不上"
        );
        // 上游把命中拆成两个计数：本轮调用的命中（`blocked_dup`）与"为目标事实而重发的调用"
        // 的命中（`served_dedup`）。我们只有**一个** `拦截重复`（省的执行次数就是它）⇒
        // 对账时把轨迹里 probe 那一半数出来加上去。
        let probe_hits = events
            .iter()
            .filter(|e| e["kind"] == "fact_probe" && e["decision"] == "dedup")
            .count() as u64;
        assert_eq!(
            s.blocked,
            up_n("blocked_dup") + probe_hits,
            "{name}：拦截重复数对不上（上游拆成 blocked_dup {} + served_dedup {probe_hits}）",
            up_n("blocked_dup")
        );
        assert!(
            mismatches.is_empty(),
            "{name}：逐条决策对齐失败（{} 处）：\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
        assert!(
            up_n("blocked_dup") > 20,
            "{name}：夹具里被拦下的重复太少（{}），这个黄金测试就没验到什么",
            up_n("blocked_dup")
        );
    }
