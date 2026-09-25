//! **检索**：结构性过滤 → 打分 → 组装（论文 §4.4）。
//!
//! 默认模式 `now` 的第一步就是**结构性过滤**：`status != active/contested` 或有效期不覆盖查询时刻的
//! 槽位**根本不进候选** —— 不是降权。这一条是 BS 的落点，也是"前文被后文推翻"在检索侧的全部含义：
//! 被推翻的值不是"排得靠后"，而是**不在当前事实里**；但它仍能被 `as_of` 查到（EC 不受损）。
//!
//! 打分是**混合**的：词法腿（FTS5/BM25）+ 语义腿（本地嵌入余弦），用**加权 RRF** 融合。
//! 权重与"长问句走 OR"这类参数来自技能 `code-search-retrieval-tuning` 的实测结论，不是拍脑袋。

use super::fold::Belief;
use super::ledger::{self, Event};
use super::{GLOBAL_SCOPE, Memory};
use rusqlite::params;

/// RRF 常数（越大越平滑）。
pub const RRF_K: f64 = 60.0;
/// 语义腿权重。
pub const W_DENSE: f64 = 1.0;
/// 词法腿权重。技能实测：BM25 腿等权会用它自己的噪声顶掉精确的语义命中，0.1 量级最优。
pub const W_LEX: f64 = 0.1;
/// 语义腿的**弃答下限**（不是"相关性分类器"）。这个数是量出来的（
/// `mem::tests::向量腿_阈值标定` 会打印实测余弦，`-- --nocapture` 可复跑）：
///
/// | | 实测范围 |
/// |---|---|
/// | 明显无关（不同话题） | 0.180 ~ 0.290 |
/// | 改写 / 同义（"如何给数据做快照" × "记忆库每天压缩一次…"） | ≈ 0.45 |
/// | 直白相关 | 0.59 ~ 0.735 |
///
/// **关键教训**：绝对余弦切不开"改写"和"跨话题但都短"的两类（0.45 vs 0.41 挨着），
/// 所以这里只用一个**低位的弃答下限**（把明显无关的挡掉），排序交给 RRF 融合，
/// 而"符号查询别用语义腿"交给结构性规则 [`super::tokenize::has_cjk`]。
/// 拿一个高阈值当相关性判据会把 paraphrase 召回一起掐死 —— 这是实测踩出来的。
pub const THETA_DENSE: f64 = 0.30;

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub row_key: String,
    pub key: String,
    pub value: String,
    pub status: String,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    pub score: f64,
    pub prov: Vec<String>,
    pub revs: Vec<String>,
    pub updated_ts: i64,
}

impl Hit {
    pub fn contested(&self) -> bool {
        self.status == "contested"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub id: i64,
    pub ts: i64,
    pub scope: String,
    pub kind: String,
    pub covered: Vec<String>,
    pub dropped: Vec<String>,
    pub rehydrate: String,
    pub note: String,
}

fn b_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<Belief> {
    let prov: String = r.get("prov")?;
    let revs: String = r.get("revs")?;
    Ok(Belief {
        scope: r.get("scope")?,
        key: r.get("key")?,
        valid_from: r.get("valid_from")?,
        valid_to: r.get("valid_to")?,
        value: r.get("value")?,
        status: r.get("status")?,
        prov: serde_json::from_str(&prov).unwrap_or_default(),
        revs: serde_json::from_str(&revs).unwrap_or_default(),
        obs_id: r.get("obs_id")?,
        updated_ts: r.get("updated_ts")?,
    })
}

const BCOLS: &str = "scope,key,valid_from,valid_to,value,status,prov,revs,obs_id,updated_ts";

fn hit(b: Belief, score: f64) -> Hit {
    Hit {
        row_key: b.row_key(),
        key: b.key,
        value: b.value,
        status: b.status,
        valid_from: b.valid_from,
        valid_to: b.valid_to,
        score,
        prov: b.prov,
        revs: b.revs,
        updated_ts: b.updated_ts,
    }
}

/// 当前信念（`now`）。`query` 为空 = 只要"当前活着的事实"（按最近更新排序），用于组装注入块。
pub fn now(
    mem: &Memory,
    scope: &str,
    query: &str,
    limit: usize,
    at: Option<i64>,
) -> Result<Vec<Hit>, String> {
    let at = at.unwrap_or_else(|| chrono::Utc::now().timestamp());
    let conn = mem.conn()?;
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {BCOLS} FROM beliefs
             WHERE scope=?1 AND status IN ('active','contested')
               AND valid_from<=?2 AND (valid_to IS NULL OR valid_to>?2)
             ORDER BY updated_ts DESC"
        ))
        .map_err(|e| format!("检索失败: {e}"))?;
    let cands: Vec<Belief> = stmt
        .query_map(params![scope, at], b_of)
        .map_err(|e| format!("检索失败: {e}"))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("检索失败: {e}"))?;
    if cands.is_empty() {
        return Ok(Vec::new());
    }

    let toks = super::tokenize::query_tokens(query);
    if toks.is_empty() {
        return Ok(cands.into_iter().take(limit).map(|b| hit(b, 0.0)).collect());
    }

    // ---- 词法腿（FTS5/BM25）；BM25 越小越好 → 名次由 ORDER BY 给出
    let mut lex_rank: Vec<String> = Vec::new();
    if let Some(expr) = super::tokenize::match_expr(&toks) {
        let mut s2 = conn
            .prepare("SELECT row_key FROM fts WHERE fts MATCH ?1 AND scope=?2 ORDER BY bm25(fts) LIMIT 200")
            .map_err(|e| format!("词法检索失败: {e}"))?;
        if let Ok(rows) = s2.query_map(params![expr, scope], |r| r.get::<_, String>(0)) {
            for r in rows.flatten() {
                lex_rank.push(r);
            }
        }
    }

    // ---- 语义腿（本地嵌入余弦）；模型不在 ⇒ 自动退化为纯词法
    let mut den_rank: Vec<String> = Vec::new();
    let mut dense_ok = false;
    // 结构性守卫：**全 ASCII 的查询（符号 / 路径 / 命令）不走语义腿**。
    // 这不是省算力，是正确性：跨话题的短中文条目在 bge 上也能拿到 0.4~0.6 的余弦，
    // 让它们参与排序会把"精确的符号命中"顶掉（实测：查 file.cargo_toml.dep 时
    // `person.7.city = 巴黎` 被排到第 1）。
    if super::tokenize::has_cjk(query) {
        // 按需补向量：只在**这次真的要语义检索**时给缺向量的信念算一遍（上限一次 400 条）
        let _ = super::embed::ensure_vectors(mem, scope, 400);
        if let Some(model) = super::embed::load_cached()?
            && let Ok(qv) = model.embed(query, true)
        {
            dense_ok = true;
            let mut scored: Vec<(String, f64)> = Vec::new();
            for b in &cands {
                if let Some(v) = super::embed::vector_of(mem, &b.row_key())? {
                    let c = super::embed::cosine(&qv, &v);
                    if c >= THETA_DENSE {
                        scored.push((b.row_key(), c));
                    }
                }
            }
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            den_rank = scored.into_iter().map(|(k, _)| k).collect();
        }
    }

    // ---- 加权 RRF 融合
    let mut fused: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for (i, k) in den_rank.iter().enumerate() {
        *fused.entry(k.clone()).or_default() += W_DENSE / (RRF_K + (i + 1) as f64);
    }
    for (i, k) in lex_rank.iter().enumerate() {
        *fused.entry(k.clone()).or_default() += W_LEX / (RRF_K + (i + 1) as f64);
    }
    if fused.is_empty() {
        // 两条腿都没捞到 ⇒ **弃答**，而不是拿"看起来相关"的东西顶上
        return Ok(Vec::new());
    }
    let mut out: Vec<Hit> = cands
        .into_iter()
        .filter_map(|b| fused.get(&b.row_key()).map(|s| hit(b, *s)))
        .collect();
    let _ = dense_ok;
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(limit);
    Ok(out)
}

/// `as-of`：某时刻的信念 —— **包含**当时成立、后来被推翻的槽位（这是 EC 的读侧形态）。
///
/// `revoked`（= `correct` 追溯撤销的"从未成立"内容）不返回：它从来没成立过，
/// 所以它不属于"当时的信念"；但它仍在账本与 `why` 里。
pub fn as_of(mem: &Memory, scope: &str, key: Option<&str>, at: i64) -> Result<Vec<Hit>, String> {
    let conn = mem.conn()?;
    let sql = format!(
        "SELECT {BCOLS} FROM beliefs
         WHERE scope=?1 AND status!='revoked'
           AND valid_from<=?2 AND (valid_to IS NULL OR valid_to>?2)
           {k}
         ORDER BY key, valid_from",
        k = if key.is_some() { "AND key=?3" } else { "" }
    );
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("as-of 查询失败: {e}"))?;
    let rows = match key {
        Some(k) => stmt.query_map(params![scope, at, k], b_of),
        None => stmt.query_map(params![scope, at], b_of),
    }
    .map_err(|e| format!("as-of 查询失败: {e}"))?;
    let bs: Vec<Belief> = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("as-of 查询失败: {e}"))?;
    Ok(bs.into_iter().map(|b| hit(b, 0.0)).collect())
}

/// `why`：一条信念的完整修订链与依据 —— 一次调用回答"你凭什么这么认为"。
pub fn why(mem: &Memory, scope: &str, key: &str) -> Result<Vec<Event>, String> {
    let mut evs = ledger::all(mem, Some(scope))?
        .into_iter()
        .filter(|e| e.key == key)
        .collect::<Vec<_>>();
    // 依据事件也要能看到（它们可能挂在别的键上）
    let refs: Vec<String> = evs.iter().flat_map(|e| e.prov.clone()).collect();
    let extra = ledger::by_ids(mem, &refs)?;
    let mut seen: Vec<String> = evs.iter().map(|e| e.id.clone()).collect();
    evs.extend(extra.into_iter().filter(|e| {
        if seen.contains(&e.id) {
            false
        } else {
            seen.push(e.id.clone());
            true
        }
    }));
    evs.sort_by_key(|e| e.seq);
    Ok(evs)
}

/// 组装注入给模型的有界信念块；超预算的**离开视野并签发收据**（不删任何东西）。
pub fn block_for_prompt(
    mem: &Memory,
    scope: &str,
    budget_chars: usize,
) -> Result<Option<String>, String> {
    let hits = now(mem, scope, "", 40, None)?;
    if hits.is_empty() {
        return Ok(None);
    }
    let header = "[项目记忆 · 当前信念] 以下是这台机器/这个项目上**已记录**的事实（不是全部历史，每条都带依据条数）。\n\
                  - 与仓库里读到的内容冲突时**以仓库为准**，并把修正记下来（record_rev/correct）。\n\
                  - 想知道某条的历史或依据（它曾经是什么、凭什么），用 `mem_why` / `mem_as_of`。\n\
                  - 这张表里**没有**的东西不等于不存在，不要拿它的缺失去推翻仓库事实。\n";
    let mut out = String::from(header);
    let mut kept: Vec<String> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    for h in &hits {
        let mark = if h.contested() {
            "（有争议，双方并存）"
        } else {
            ""
        };
        let line = format!(
            "- {} = {}{}  [依据 {} 条]\n",
            h.key,
            h.value,
            mark,
            h.prov.len()
        );
        if out.len() + line.len() > budget_chars {
            dropped.push(h.key.clone());
            continue;
        }
        out.push_str(&line);
        kept.push(h.key.clone());
    }
    if !dropped.is_empty() {
        add_receipt(
            mem,
            scope,
            "prompt_block",
            &kept,
            &dropped,
            "mem_why(<key>)",
            &format!("注入块预算 {budget_chars} 字符；超出的条目只是离开视野，账本未动"),
        )?;
    }
    Ok(Some(out))
}

pub fn add_receipt(
    mem: &Memory,
    scope: &str,
    kind: &str,
    covered: &[String],
    dropped: &[String],
    rehydrate: &str,
    note: &str,
) -> Result<(), String> {
    let conn = mem.conn()?;
    conn.execute(
        "INSERT INTO receipts (ts, scope, kind, covered, dropped, rehydrate, note) VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            chrono::Utc::now().timestamp(),
            scope,
            kind,
            serde_json::to_string(covered).unwrap_or_else(|_| "[]".into()),
            serde_json::to_string(dropped).unwrap_or_else(|_| "[]".into()),
            rehydrate,
            note
        ],
    )
    .map_err(|e| format!("写收据失败: {e}"))?;
    Ok(())
}

pub fn receipts(mem: &Memory, scope: &str, limit: usize) -> Result<Vec<Receipt>, String> {
    let conn = mem.conn()?;
    let mut stmt = conn
        .prepare("SELECT id,ts,scope,kind,covered,dropped,rehydrate,note FROM receipts WHERE scope=?1 ORDER BY ts DESC LIMIT ?2")
        .map_err(|e| format!("读收据失败: {e}"))?;
    let rows = stmt
        .query_map(params![scope, limit as i64], |r| {
            let cov: String = r.get("covered")?;
            let dro: String = r.get("dropped")?;
            Ok(Receipt {
                id: r.get("id")?,
                ts: r.get("ts")?,
                scope: r.get("scope")?,
                kind: r.get("kind")?,
                covered: serde_json::from_str(&cov).unwrap_or_default(),
                dropped: serde_json::from_str(&dro).unwrap_or_default(),
                rehydrate: r.get("rehydrate")?,
                note: r.get("note")?,
            })
        })
        .map_err(|e| format!("读收据失败: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("读收据失败: {e}"))
}

/// 机器级作用域的便捷入口（工具链等跨项目事实）。
pub fn global_now(mem: &Memory, query: &str, limit: usize) -> Result<Vec<Hit>, String> {
    now(mem, GLOBAL_SCOPE, query, limit, None)
}
