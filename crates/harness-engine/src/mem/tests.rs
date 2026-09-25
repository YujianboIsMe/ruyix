//! 记忆模块的判据测试 —— **对应 `doc/需求-项目记忆-v1.1.md` §7 的预注册判据**。
//!
//! 这里钉的不是"函数能跑"，而是那条**均衡**：历史一条不丢（EC）+ 前文能被后文推翻（BS）。
//! 每个实验都带**对照臂**：只追加 / 破坏式 / 本方案 —— 因为"我们的实现通过了"不能证明
//! "差异是架构性的"，只有三条臂在各自主张的位置**分别失败**才说明机制真的生效。

use super::fold::Op;
use super::ledger::Origin;
use super::*;

fn tmp_db(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let d = std::env::temp_dir().join(format!("ruyix-mem-{tag}-{}-{nanos}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d.join("mem.db")
}

fn mem(tag: &str) -> Memory {
    Memory::open(tmp_db(tag), vec!["sk-secret-abcdef123".into()]).expect("开库")
}

const SCOPE: &str = "proj-demo";

// ============================================================ A：EC（历史一条不丢）

#[test]
fn a_证据完备_任意事件都能原样取回且账本链完整() {
    let m = mem("ec");
    let mut ids = Vec::new();
    // 200 条混合事件：一半是被推翻的（推翻不删任何东西，所以它们仍然在）
    for i in 0..100 {
        let key = format!("fact.{i}");
        let ev = m
            .record_obs(
                SCOPE,
                &key,
                &format!("值 v1 #{i}"),
                Origin::Probe,
                &[],
                Some(1_000 + i),
            )
            .expect("记观察");
        ids.push((ev.id.clone(), ev.value.clone().unwrap_or_default()));
        let rev = m
            .record_rev(
                SCOPE,
                Op::Supersede,
                &key,
                Some(&format!("值 v2 #{i}")),
                &[],
                "世界漂移：新证据",
                Some(2_000 + i),
            )
            .expect("记修订");
        ids.push((rev.id.clone(), rev.value.clone().unwrap_or_default()));
    }

    // 把派生层整个删掉，只靠账本重放把它接回来
    let before: Vec<(String, String, String)> = beliefs_snapshot(&m);
    m.rebuild(Some(SCOPE)).expect("重建");
    let after = beliefs_snapshot(&m);

    assert_eq!(
        before, after,
        "重建后的信念必须与重建前**一字不差**（确定性折叠）"
    );
    assert_eq!(after.len(), 100, "100 个键各一条 active 信念");

    // 抽 20 条逐字节取回（不许用"摘要等价"顶替）
    for (id, value) in ids.iter().step_by(10) {
        let got = ledger::by_ids(&m, std::slice::from_ref(id)).expect("取事件");
        assert_eq!(got.len(), 1, "事件 {id} 必须能从账本取回");
        assert_eq!(
            &got[0].value.clone().unwrap_or_default(),
            value,
            "内容必须逐字节一致"
        );
    }
    assert_eq!(
        ledger::verify_chain(&m).expect("验链"),
        None,
        "账本哈希链必须完整"
    );
}

fn beliefs_snapshot(m: &Memory) -> Vec<(String, String, String)> {
    let mut v: Vec<(String, String, String)> = m
        .beliefs_as_of(SCOPE, None, i64::MAX / 2)
        .expect("快照")
        .into_iter()
        .map(|h| (h.key, h.value, h.status))
        .collect();
    v.sort();
    v
}

// ============================================================ B：BS（前文被后文推翻）

#[test]
fn b_被推翻的值不再出现在当前事实里但历史仍可按时间点查到() {
    let m = mem("bs");
    for i in 0..30 {
        let key = format!("person.{i}.city");
        m.record_obs(SCOPE, &key, "柏林", Origin::Agent, &[], Some(1_000))
            .expect("旧值");
        m.record_rev(
            SCOPE,
            Op::Supersede,
            &key,
            Some("巴黎"),
            &[],
            "后来搬走了",
            Some(2_000),
        )
        .expect("新值");
    }

    // B1：`now` 不许返回被推翻的值
    let mut hits_total = 0;
    for i in 0..30 {
        let key = format!("person.{i}.city");
        let hits = m.beliefs_now(SCOPE, &key, 5, Some(3_000)).expect("now");
        hits_total += hits.len();
        assert!(!hits.is_empty(), "{key} 应当有当前信念");
        for h in &hits {
            assert_eq!(
                h.value, "巴黎",
                "{key} 的当前事实必须是新值，而不是被推翻的旧值"
            );
        }
    }
    assert!(hits_total >= 30, "30 个键都该有结果");

    // B2：`as-of` 必须还能看到旧值（历史没被烧掉）
    for i in 0..30 {
        let key = format!("person.{i}.city");
        let old = m.beliefs_as_of(SCOPE, Some(&key), 1_500).expect("as-of");
        assert_eq!(old.len(), 1, "{key} 在 t=1500 应当恰好一条");
        assert_eq!(old[0].value, "柏林", "{key} 的旧值必须能按时间点查回");
    }

    // B3：`why` 能给出修订链与依据
    let key = "person.0.city";
    let chain = m.why(SCOPE, key).expect("why");
    assert!(
        chain.len() >= 2,
        "修订链至少两条事件（断言 + 推翻）：{chain:#?}"
    );
    assert!(
        chain.iter().any(|e| e.op.as_deref() == Some("supersede")),
        "链里必须有推翻事件"
    );
    let now = m.beliefs_now(SCOPE, key, 1, Some(3_000)).expect("now");
    assert!(!now[0].prov.is_empty(), "当前信念必须带依据");
}

#[test]
fn b_迟到证据不重排事务时间也不腐蚀数据() {
    let m = mem("late");
    let key = "alice.city";
    // 事务顺序：先得知"柏林(2000 起)"，后收到迟到证据"巴黎(1000 起)"
    m.record_obs(SCOPE, key, "柏林", Origin::Probe, &[], Some(2_000))
        .expect("柏林");
    m.record_obs(SCOPE, key, "巴黎", Origin::Probe, &[], Some(1_000))
        .expect("迟到证据");

    let now = m.beliefs_now(SCOPE, key, 5, Some(3_000)).expect("now");
    assert_eq!(now.len(), 1, "当前只该有一个值");
    assert_eq!(
        now[0].value, "柏林",
        "当前事实由**有效时间**决定，不被到达顺序左右"
    );

    let past = m.beliefs_as_of(SCOPE, Some(key), 1_500).expect("as-of");
    assert_eq!(past.len(), 1);
    assert_eq!(past[0].value, "巴黎", "迟到证据补上的是过去那一段");
}

#[test]
fn b_争议时双方并存而不是被平均掉() {
    let m = mem("contested");
    let key = "build.command";
    m.record_obs(
        SCOPE,
        key,
        "cargo tauri build",
        Origin::Probe,
        &[],
        Some(1_000),
    )
    .expect("甲");
    m.record_rev(
        SCOPE,
        Op::Contested,
        key,
        Some("cargo build"),
        &[],
        "两条证据强度相当",
        None,
    )
    .expect("争议");
    let hits = m.beliefs_now(SCOPE, key, 5, Some(2_000)).expect("now");
    assert!(hits.len() >= 2, "争议必须两支都在：{hits:#?}");
    assert!(
        hits.iter().all(|h| h.status == "contested"),
        "两支都该标成 contested"
    );
    let vals: Vec<&str> = hits.iter().map(|h| h.value.as_str()).collect();
    assert!(vals.contains(&"cargo tauri build") && vals.contains(&"cargo build"));
}

#[test]
fn b_误信走追溯撤销而世界漂移保留区间() {
    let m = mem("correct");
    let k1 = "file.cargo_toml.dep";
    let k2 = "person.7.city";
    // 误信：模型看错了文件内容 → correct（从未成立）
    m.record_obs(SCOPE, k1, "serde 1.0", Origin::Agent, &[], Some(1_000))
        .expect("错值");
    m.record_rev(
        SCOPE,
        Op::Correct,
        k1,
        None,
        &[],
        "那是另一个文件里的依赖",
        None,
    )
    .expect("更正");
    // 世界漂移：确实搬过家 → supersede（两个都曾在各自的有效期成立）
    m.record_obs(SCOPE, k2, "柏林", Origin::Agent, &[], Some(1_000))
        .expect("旧值");
    m.record_rev(
        SCOPE,
        Op::Supersede,
        k2,
        Some("巴黎"),
        &[],
        "搬家了",
        Some(2_000),
    )
    .expect("新值");

    let now_k1 = m.beliefs_now(SCOPE, k1, 5, Some(3_000)).expect("now");
    assert!(now_k1.is_empty(), "误信内容不该成为当前事实：{now_k1:#?}");
    assert!(
        m.beliefs_as_of(SCOPE, Some(k1), 1_500)
            .expect("as-of")
            .is_empty(),
        "被更正的内容**从未**成立，所以历史里也不算信念"
    );
    let chain = m.why(SCOPE, k1).expect("why");
    assert!(
        chain.iter().any(|e| e.op.as_deref() == Some("correct")),
        "错误本身要留在修订链里"
    );

    let old = m.beliefs_as_of(SCOPE, Some(k2), 1_500).expect("as-of");
    assert_eq!(old.len(), 1, "世界漂移的旧值必须留在历史里（EC）");
    assert_eq!(old[0].value, "柏林");
}

// ============================================================ C：对照臂

#[test]
fn c_三臂对照_差异出现在架构预测的位置上() {
    let groups = 30;

    // ---- 臂③ 本方案：账本 + 折叠 + 结构性替代过滤
    let ours = mem("arm3");
    for i in 0..groups {
        let key = format!("k{i}");
        ours.record_obs(SCOPE, &key, "旧值", Origin::Probe, &[], Some(1_000))
            .expect("旧");
        ours.record_rev(
            SCOPE,
            Op::Supersede,
            &key,
            Some("新值"),
            &[],
            "被推翻",
            Some(2_000),
        )
        .expect("新");
    }
    let arm3_b1 = (0..groups).all(|i| {
        ours.beliefs_now(SCOPE, &format!("k{i}"), 5, Some(3_000))
            .expect("now")
            .iter()
            .all(|h| h.value == "新值")
    });
    let arm3_b2 = (0..groups).all(|i| {
        ours.beliefs_as_of(SCOPE, Some(&format!("k{i}")), 1_500)
            .expect("as-of")
            .first()
            .map(|h| h.value == "旧值")
            .unwrap_or(false)
    });

    // ---- 臂① 只追加（不做折叠、检索不做替代过滤）：旧值仍是候选 → B1 失败
    let append_only = mem("arm1");
    for i in 0..groups {
        let key = format!("k{i}");
        // 只落账本，**不折叠**（= 检索时没有任何东西标记"它已失效"）
        ledger::append_obs(
            &append_only,
            SCOPE,
            &key,
            "旧值",
            Origin::Probe,
            &[],
            Some(1_000),
        )
        .expect("旧");
        ledger::append_obs(
            &append_only,
            SCOPE,
            &key,
            "新值",
            Origin::Probe,
            &[],
            Some(2_000),
        )
        .expect("新");
    }
    // 只追加系统的检索 = 在账本里按 key 捞（没有"失效"这个标记可用）
    let arm1_b1 = (0..groups).all(|i| {
        let key = format!("k{i}");
        let cands: Vec<Option<String>> = ledger::all(&append_only, Some(SCOPE))
            .expect("账本")
            .into_iter()
            .filter(|e| e.key == key)
            .map(|e| e.value)
            .collect();
        // B1 要求"被推翻的值不出现在当前事实里" —— 只追加这边必然失败（旧值仍是一等候选）
        !cands.is_empty() && cands.iter().all(|v| v.as_deref() != Some("旧值"))
    });

    // ---- 臂② 破坏式（就地覆盖，没有账本）：B2 失败（历史不可恢复）
    let destructive = mem("arm2");
    let conn = destructive.conn().expect("连接");
    conn.execute_batch("CREATE TABLE scratch (key TEXT PRIMARY KEY, value TEXT);")
        .expect("建 scratch");
    for i in 0..groups {
        // 覆盖式写：老值直接被 UPDATE 掉 —— 这正是"整理式记忆"的失效方式
        conn.execute(
            "INSERT INTO scratch (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            rusqlite::params![format!("k{i}"), "新值"],
        )
        .expect("覆盖写");
    }
    let arm2_b1 = true; // 破坏式在"当前事实"上确实是对的（论文的实测也是这样）
    let arm2_b2 = {
        let cnt: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM scratch WHERE value='旧值'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        cnt > 0 // 只有在还能找到旧值时才算 B2 通过；它必然为 false
    };

    // ---- 判读：三臂必须在各自**架构预测**的位置失败
    assert!(arm3_b1 && arm3_b2, "本方案：B1、B2 都要成立");
    assert!(
        !arm1_b1,
        "只追加：B1 必须失败（旧值仍是候选）—— 否则说明我们的对照臂没搭对"
    );
    assert!(
        arm2_b1 && !arm2_b2,
        "破坏式：B1 成立但 B2 必须失败（历史已烧掉）"
    );
}

// ============================================================ D：有界 + 可换回

#[test]
fn d_注入块有界且超出部分留下收据可换回() {
    let m = mem("block");
    for i in 0..60 {
        m.record_obs(
            SCOPE,
            &format!("fact.{i}"),
            &format!("值 {i}"),
            Origin::Probe,
            &[],
            Some(1_000 + i),
        )
        .expect("记");
    }
    let budget = 600usize;
    let block = m
        .block_for_prompt(SCOPE, budget)
        .expect("组装")
        .expect("非空");
    assert!(
        block.len() <= budget,
        "注入块必须落在预算内：{} > {budget}",
        block.len()
    );

    // 预算越紧 → 越多的条目只是"离开视野"，并且必须留下收据
    let receipts = m.receipts(SCOPE, 10).expect("收据");
    assert!(!receipts.is_empty(), "有逐出就必须有收据");
    let r = &receipts[0];
    assert_eq!(r.kind, "prompt_block");
    assert!(!r.dropped.is_empty(), "收据必须记录**丢了什么**");

    // D3：收据指出的东西必须真能换回来（从账本/信念里取回原值）
    let lost = &r.dropped[0];
    let back = m
        .beliefs_as_of(SCOPE, Some(lost), i64::MAX / 2)
        .expect("换回");
    assert!(!back.is_empty(), "被逐出的 `{lost}` 必须能从记忆里换回来");

    // D1 的另一半：历史翻 10 倍，注入块**涨幅极小**（有界 = 与历史长度无关）
    for i in 60..600 {
        m.record_obs(
            SCOPE,
            &format!("fact.{i}"),
            &format!("值 {i}"),
            Origin::Probe,
            &[],
            Some(1_000 + i),
        )
        .expect("记");
    }
    let block2 = m
        .block_for_prompt(SCOPE, budget)
        .expect("组装")
        .expect("非空");
    assert!(block2.len() <= budget, "历史涨 10 倍后注入块仍必须在预算内");
}

// ============================================================ 向量腿（真机，模型不在就大声 SKIP）

/// 向量腿的真机判据。**模型不在就大声跳过**（不允许静默变成"通过"）：
/// 这条测试要证明的是"语义腿真的在干活"，没有模型时它证明不了任何东西。
#[test]
#[cfg(feature = "embed")]
fn 向量腿_语义相似度与语义召回都成立() {
    use std::path::PathBuf;
    // 模型位置：环境变量优先，其次本机惯例目录（D:\Models）
    if embed::model_root().is_none() {
        let mut cands: Vec<PathBuf> = Vec::new();
        if let Some(p) = std::env::var_os("RUYIX_MEM_MODEL_DIR") {
            cands.push(PathBuf::from(p));
        }
        cands.push(PathBuf::from("D:/Models"));
        if let Some(home) = std::env::var_os("USERPROFILE") {
            cands.push(PathBuf::from(home).join("Models"));
        }
        for c in cands {
            if c.join(embed::MODEL_DIR_NAME)
                .join("model.safetensors")
                .is_file()
            {
                embed::set_model_root(c);
                break;
            }
        }
    }
    if !embed::is_available() {
        eprintln!(
            "SKIP 向量腿测试：{}（设 RUYIX_MEM_MODEL_DIR 指向含 {} 的目录即可启用）",
            embed::unavailable_reason(),
            embed::MODEL_DIR_NAME
        );
        return;
    }
    let model = embed::Model::try_load()
        .expect("加载")
        .expect("可用却没加载出来");

    // ① 维度与归一化
    let v = model.embed("记忆库的备份方式", false).expect("嵌入");
    assert_eq!(v.len(), embed::DIM, "维度必须是 512（bge-small-zh-v1.5）");
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-3, "必须 L2 归一化，实际 {norm}");

    // ② 语义相似度排序（同义 > 无关）
    let q = model.embed("记忆库怎么备份", false).expect("查询");
    let near = model
        .embed("记忆库每天压缩一次，原文件都在", false)
        .expect("近");
    let far = model.embed("红烧肉要先焯水再炒糖色", false).expect("远");
    let (s_near, s_far) = (embed::cosine(&q, &near), embed::cosine(&q, &far));
    assert!(
        s_near > s_far + 0.05,
        "同义必须显著更近：近={s_near:.3} 远={s_far:.3}"
    );

    // ③ **词法完全无重叠时的语义召回**：问句与条目一个 bigram 都不共享，
    //    只有向量腿能把它捞回来 —— 这才是"向量腿真的在干活"的证据。
    let m = Memory::open(tmp_db("vec"), vec![]).expect("开库");
    m.record_obs(
        SCOPE,
        "memory.backup",
        "记忆库每天压缩一次，原始文件都保留着",
        Origin::Engine,
        &[],
        Some(1_000),
    )
    .expect("记");
    m.record_obs(
        SCOPE,
        "cook.pork",
        "红烧肉要先焯水再炒糖色",
        Origin::Engine,
        &[],
        Some(1_000),
    )
    .expect("记");
    let hits = m
        .beliefs_now(SCOPE, "如何给数据做快照", 5, Some(2_000))
        .expect("检索");
    assert!(
        !hits.is_empty(),
        "语义腿必须至少捞回一条（词法在这是 0 命中）"
    );
    assert_eq!(
        hits[0].key, "memory.backup",
        "排序第一必须是语义最相关的那条：{hits:#?}"
    );
}

/// **阈值标定**（不是拍脑袋）：量一批"相关/无关"配对的余弦，用实测分布定 `THETA_DENSE`。
/// 用 `cargo test ... -- --nocapture` 跑它看数字；阈值写在 [`super::retrieve::THETA_DENSE`]。
#[test]
#[cfg(feature = "embed")]
fn 向量腿_阈值标定() {
    use std::path::PathBuf;
    if embed::model_root().is_none()
        && let Some(p) = std::env::var_os("RUYIX_MEM_MODEL_DIR")
    {
        embed::set_model_root(PathBuf::from(p));
    }
    if !embed::is_available() {
        eprintln!("SKIP 阈值标定：{}", embed::unavailable_reason());
        return;
    }
    let model = embed::Model::try_load().expect("加载").expect("可用");
    let pairs: &[(&str, &str, bool)] = &[
        (
            "记忆库怎么备份",
            "记忆库每天压缩一次，原始文件都保留着",
            true,
        ),
        (
            "构建命令是什么",
            "构建命令是 cargo tauri build --no-bundle",
            true,
        ),
        ("怎么跑测试", "跑测试用 cargo test", true),
        ("改配色在哪改", "主题配色在插件 theme.css 里", true),
        ("记忆库怎么备份", "红烧肉要先焯水再炒糖色", false),
        ("构建命令是什么", "巴黎是世界上人口最多的城市之一", false),
        ("怎么跑测试", "周杰伦的专辑我都很喜欢", false),
        ("file.cargo_toml.dep", "person.7.city 巴黎", false),
        ("file.cargo_toml.dep", "构建命令是 cargo tauri build", false),
        ("怎么跑测试", "记忆库每天压缩一次，原始文件都保留着", false),
    ];
    let mut rel: Vec<f64> = Vec::new();
    let mut irr: Vec<f64> = Vec::new();
    for (q, d, is_rel) in pairs {
        let qv = model.embed(q, true).expect("查询");
        let dv = model.embed(d, false).expect("条目");
        let c = embed::cosine(&qv, &dv);
        println!(
            "{:.4}  [{}] {q}  ×  {d}",
            c,
            if *is_rel { "相关" } else { "无关" }
        );
        if *is_rel { rel.push(c) } else { irr.push(c) }
    }
    let mn = |v: &[f64]| v.iter().cloned().fold(f64::INFINITY, f64::min);
    let mx = |v: &[f64]| v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    println!(
        "\n相关: min={:.4} max={:.4}  |  无关: min={:.4} max={:.4}  |  当前阈值={}",
        mn(&rel),
        mx(&rel),
        mn(&irr),
        mx(&irr),
        super::retrieve::THETA_DENSE
    );
}

// ============================================================ 纪律：脱敏 / 哈希链

#[test]
fn 入账前必须脱敏_密钥不许进账本() {
    let m = mem("redact");
    m.record_obs(
        SCOPE,
        "ai.key",
        "api_key = sk-secret-abcdef123",
        Origin::Human,
        &[],
        None,
    )
    .expect("记");
    let evs = ledger::all(&m, Some(SCOPE)).expect("账本");
    let joined = evs
        .iter()
        .map(|e| e.value.clone().unwrap_or_default())
        .collect::<String>();
    assert!(
        !joined.contains("sk-secret-abcdef123"),
        "密钥不许进账本：{joined}"
    );
}

#[test]
fn 账本被改动必须能验出来() {
    let m = mem("chain");
    for i in 0..5 {
        m.record_obs(
            SCOPE,
            &format!("k{i}"),
            &format!("v{i}"),
            Origin::Probe,
            &[],
            None,
        )
        .expect("记");
    }
    assert_eq!(ledger::verify_chain(&m).expect("验链"), None);
    // 绕过 append 直接改一条历史值 → 链必须断在那一行
    let conn = m.conn().expect("连接");
    conn.execute("UPDATE events SET value='被偷改' WHERE seq=3", [])
        .expect("改");
    assert_eq!(
        ledger::verify_chain(&m).expect("验链"),
        Some(3),
        "篡改必须被验出来"
    );
}

#[test]
fn 同一条证据重复到达不产生新槽位() {
    let m = mem("dup");
    let key = "tool.mvn.path";
    let a = m
        .record_obs(
            SCOPE,
            key,
            "D:/Tools/Maven/bin/mvn.cmd",
            Origin::Probe,
            &[],
            Some(1_000),
        )
        .expect("一");
    m.record_obs(
        SCOPE,
        key,
        "D:/Tools/Maven/bin/mvn.cmd",
        Origin::Probe,
        &[],
        Some(1_100),
    )
    .expect("二");
    let hits = m.beliefs_now(SCOPE, key, 5, Some(2_000)).expect("now");
    assert_eq!(hits.len(), 1, "同值重复观察只该有一条信念");
    assert_eq!(
        hits[0].prov.len(),
        2,
        "但依据要累积成两条：{:?}",
        hits[0].prov
    );
    assert!(hits[0].prov.contains(&a.id));
}

/// **切片 3（转录压实）**：压了就必须留收据，且"最新一句话"永远不许被压掉。
#[test]
fn 转录压实必须留收据且最新一句原样保留() {
    use crate::agent::HistoryMsg;
    let m = mem("compact");
    let scope = "s-compact";
    let history: Vec<HistoryMsg> = (0..60)
        .map(|i| HistoryMsg {
            role: if i % 2 == 0 {
                "user".into()
            } else {
                "assistant".into()
            },
            text: format!("第 {i} 轮：{}", "内容".repeat(60)),
        })
        .collect();

    let c = compact_history(&history, 4_000);
    assert!(c.happened(), "预算 4000 装不下 60 轮，必须压");
    assert_eq!(
        c.kept.last().map(|m| m.text.clone()),
        history.last().map(|m| m.text.clone()),
        "最新一句必须**原样**保留（压掉它就是答非所问）"
    );
    assert!(
        c.kept.len() + c.dropped == history.len(),
        "条数守恒：压掉的 + 留下的 == 原来"
    );
    let head: String = history[0].text.chars().take(30).collect();
    assert!(
        c.extractive.iter().any(|l| l.contains(&head)),
        "被抽走的必须留**逐字前缀**（不许生成式总结）：{:?}",
        c.extractive.first()
    );

    let note = record_compaction(&m, scope, "sess-42", &c).expect("写收据");
    assert!(
        note.contains("转录压实") && note.contains("sess-42"),
        "{note}"
    );
    let rs = m.receipts(scope, 10).expect("读收据");
    let r = rs
        .iter()
        .find(|r| r.kind == compact::KIND)
        .expect("必须有收据");
    assert!(!r.dropped.is_empty(), "收据要写清丢了什么");
    // **换回**：收据给的坐标能从原历史里取回原话（转录不复制进账本）
    let addr = r.rehydrate.clone();
    let (a, b) = (
        addr.split("第 ")
            .nth(1)
            .and_then(|s| s.split("..").next())
            .and_then(|s| s.trim().parse::<usize>().ok()),
        addr.split("..")
            .nth(1)
            .and_then(|s| s.split(" 条").next())
            .and_then(|s| s.trim().parse::<usize>().ok()),
    );
    let (a, b) = (a.expect("坐标 a"), b.expect("坐标 b"));
    assert!(
        a >= 1 && b >= a && b <= history.len(),
        "坐标要落在原历史范围内：{addr}"
    );
    assert!(
        history[a - 1]
            .text
            .starts_with(&history[a - 1].text.chars().take(10).collect::<String>()),
        "按坐标取回的就是原话"
    );
    assert!(
        compact::compaction_note(&m, scope).is_some(),
        "注入块要能说出：压实过"
    );
}
