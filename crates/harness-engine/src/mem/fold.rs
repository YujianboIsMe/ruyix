//! **折叠**（信念层）：账本前缀 → 当前信念。`beliefs` 是可丢弃、可重建的缓存。
//!
//! 修订代数（论文 §4.2 的五个算子 + `contested`），每个算子都有明确的**效果**与**语义**：
//!
//! | 算子 | 什么时候用 | 效果（**都不删任何东西**） |
//! |---|---|---|
//! | `supersede` | **世界漂移**：两个值都曾在各自的有效期成立 | 关闭旧区间（`valid_to = 新值的 valid_from`）+ 装新值 |
//! | `correct` | **误信**：旧内容**从未**成立（抽取错、看错文件） | 追溯撤销（`revoked`），错误与理由逐字留在修订链里 |
//! | `refine` | 粒度变化（两条部分观察锐化为一条，不冲突） | 换载荷 + 溯源取并 |
//! | `merge` | 跨键去重 | 源键关闭 + 溯源并入目标键 |
//! | `split` | 一条信念混淆了不同实体/区间 | 源键关闭 + 在新键装同样的值 |
//! | `contested` | 强度相当的反证：**裁决器不猜** | 双方并存、互指，**不做任何改写** |
//!
//! 最后一条是刻意保留的"诚实的失败方式"：有损整合最常见的做法是把矛盾在摘要里"平均掉"，
//! 而这个代数是明令禁止的 —— **不确定性必须被表示，而不是被插值抹平**。

use super::ledger::Event;
use super::{Memory, ledger::EventKind};
use rusqlite::{Connection, params};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Supersede,
    Correct,
    Refine,
    Merge,
    Split,
    Contested,
}

impl Op {
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Supersede => "supersede",
            Op::Correct => "correct",
            Op::Refine => "refine",
            Op::Merge => "merge",
            Op::Split => "split",
            Op::Contested => "contested",
        }
    }
    pub fn parse(s: &str) -> Op {
        match s {
            "correct" => Op::Correct,
            "refine" => Op::Refine,
            "merge" => Op::Merge,
            "split" => Op::Split,
            "contested" => Op::Contested,
            _ => Op::Supersede,
        }
    }
}

/// 折叠出来的一条槽位信念。`valid_to = None` 表示"至今"。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Belief {
    pub scope: String,
    pub key: String,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    pub value: String,
    pub status: String,
    pub prov: Vec<String>,
    pub revs: Vec<String>,
    pub obs_id: String,
    pub updated_ts: i64,
}

impl Belief {
    /// FTS / 向量表里的行键（表内唯一）：三件套 + 观察 id。
    pub fn row_key(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.scope, self.key, self.valid_from, self.obs_id
        )
    }
}

fn j(v: &[String]) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "[]".into())
}

fn pj(s: &str) -> Vec<String> {
    serde_json::from_str(s).unwrap_or_default()
}

const BCOLS: &str = "scope,key,valid_from,valid_to,value,status,prov,revs,obs_id,updated_ts";

fn row_to_belief(r: &rusqlite::Row<'_>) -> rusqlite::Result<Belief> {
    let prov: String = r.get("prov")?;
    let revs: String = r.get("revs")?;
    Ok(Belief {
        scope: r.get("scope")?,
        key: r.get("key")?,
        valid_from: r.get("valid_from")?,
        valid_to: r.get("valid_to")?,
        value: r.get("value")?,
        status: r.get("status")?,
        prov: pj(&prov),
        revs: pj(&revs),
        obs_id: r.get("obs_id")?,
        updated_ts: r.get("updated_ts")?,
    })
}

/// 某键上的 `active` 槽位（按 valid_from 升序）。
pub(crate) fn active_at(conn: &Connection, scope: &str, key: &str) -> Result<Vec<Belief>, String> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {BCOLS} FROM beliefs WHERE scope=?1 AND key=?2 AND status='active' ORDER BY valid_from"
        ))
        .map_err(|e| format!("查信念失败: {e}"))?;
    let rows = stmt
        .query_map(params![scope, key], row_to_belief)
        .map_err(|e| format!("查信念失败: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("查信念失败: {e}"))
}

fn delete_belief(conn: &Connection, b: &Belief) -> Result<(), String> {
    conn.execute(
        "DELETE FROM beliefs WHERE scope=?1 AND key=?2 AND valid_from=?3 AND obs_id=?4",
        params![b.scope, b.key, b.valid_from, b.obs_id],
    )
    .map_err(|e| format!("改信念失败: {e}"))?;
    conn.execute("DELETE FROM fts WHERE row_key=?1", params![b.row_key()])
        .map_err(|e| format!("清索引失败: {e}"))?;
    conn.execute("DELETE FROM vectors WHERE row_key=?1", params![b.row_key()])
        .map_err(|e| format!("清向量失败: {e}"))?;
    Ok(())
}

fn insert_belief(conn: &Connection, b: &Belief) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO beliefs (scope,key,valid_from,valid_to,value,status,prov,revs,obs_id,updated_ts)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            b.scope, b.key, b.valid_from, b.valid_to, b.value, b.status,
            j(&b.prov), j(&b.revs), b.obs_id, b.updated_ts
        ],
    )
    .map_err(|e| format!("写信念失败: {e}"))?;
    // 检索视图（FTS）：键 + 值一起进，因为"构建命令"这种键本身就是查询词的一部分
    let text = super::tokenize::index_text(&format!("{} {}", b.key, b.value));
    conn.execute("DELETE FROM fts WHERE row_key=?1", params![b.row_key()])
        .map_err(|e| format!("写索引失败: {e}"))?;
    conn.execute(
        "INSERT INTO fts (scope, row_key, text) VALUES (?1, ?2, ?3)",
        params![b.scope, b.row_key(), text],
    )
    .map_err(|e| format!("写索引失败: {e}"))?;
    // **向量不在写路径上算**：信念写入是交互路径，一次 20~150ms 的 CPU 前向没必要花在这里
    // （实测：200 条事件的测试从 4s 涨到 148s）。向量改由检索时按需补（embed::ensure_vectors），
    // 也就是"只有真的要语义检索时才付这个钱"。
    Ok(())
}

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 把一条事件折叠进 `beliefs`。**只读 `events` 之外的输入都不接受** —— 折叠必须是账本的函数。
pub fn apply(mem: &Memory, ev: &Event) -> Result<(), String> {
    let conn = mem.conn()?;
    match ev.kind {
        EventKind::Obs => apply_obs(&conn, ev),
        EventKind::Rev => apply_rev(&conn, ev),
    }
}

fn apply_obs(conn: &Connection, ev: &Event) -> Result<(), String> {
    let scope = &ev.scope;
    let key = &ev.key;
    let value = ev.value.clone().unwrap_or_default();
    let vf = ev.valid_from.unwrap_or(ev.ts);
    let actives = active_at(conn, scope, key)?;

    if actives.is_empty() {
        return insert_belief(
            conn,
            &Belief {
                scope: scope.clone(),
                key: key.clone(),
                valid_from: vf,
                valid_to: None,
                value,
                status: "active".into(),
                prov: vec![ev.id.clone()],
                revs: vec![ev.id.clone()],
                obs_id: ev.id.clone(),
                updated_ts: now_ts(),
            },
        );
    }

    // 同值 → 只是又一条证据：溯源取并、区间拉长（**不产生新槽位**）
    if let Some(b) = actives.iter().find(|b| b.value == value) {
        let mut prov = b.prov.clone();
        if !prov.contains(&ev.id) {
            prov.push(ev.id.clone());
        }
        let mut revs = b.revs.clone();
        if !revs.contains(&ev.id) {
            revs.push(ev.id.clone());
        }
        let nb = Belief {
            valid_from: b.valid_from.min(vf),
            prov,
            revs,
            updated_ts: now_ts(),
            ..b.clone()
        };
        delete_belief(conn, b)?;
        return insert_belief(conn, &nb);
    }

    let earliest = actives.iter().map(|b| b.valid_from).min().unwrap_or(vf);
    if vf >= earliest {
        // **世界漂移**：旧值当时确实成立 → 关闭区间（不删），装新值
        for b in &actives {
            let mut revs = b.revs.clone();
            if !revs.contains(&ev.id) {
                revs.push(ev.id.clone());
            }
            let closed = Belief {
                valid_to: Some(vf),
                status: "superseded".into(),
                revs,
                updated_ts: now_ts(),
                ..b.clone()
            };
            delete_belief(conn, b)?;
            insert_belief(conn, &closed)?;
        }
        insert_belief(
            conn,
            &Belief {
                scope: scope.clone(),
                key: key.clone(),
                valid_from: vf,
                valid_to: None,
                value,
                status: "active".into(),
                prov: vec![ev.id.clone()],
                revs: vec![ev.id.clone()],
                obs_id: ev.id.clone(),
                updated_ts: now_ts(),
            },
        )
    } else {
        // **迟到证据**：这条说的是"过去"（有效时间早于当前信念）→ 给它一个过去的区间，
        // 并让后来的信念关闭它。历史因此比之前更完整，而不是被覆盖。
        insert_belief(
            conn,
            &Belief {
                scope: scope.clone(),
                key: key.clone(),
                valid_from: vf,
                valid_to: Some(earliest),
                value,
                status: "superseded".into(),
                prov: vec![ev.id.clone()],
                revs: actives.iter().map(|b| b.obs_id.clone()).collect(),
                obs_id: ev.id.clone(),
                updated_ts: now_ts(),
            },
        )
    }
}

fn apply_rev(conn: &Connection, ev: &Event) -> Result<(), String> {
    let scope = &ev.scope;
    let key = &ev.key;
    let op = Op::parse(ev.op.as_deref().unwrap_or("supersede"));
    let actives = active_at(conn, scope, key)?;

    match op {
        Op::Supersede => {
            let vf = ev.valid_from.unwrap_or(ev.ts);
            for b in &actives {
                let mut revs = b.revs.clone();
                revs.push(ev.id.clone());
                let closed = Belief {
                    valid_to: Some(vf),
                    status: "superseded".into(),
                    revs,
                    updated_ts: now_ts(),
                    ..b.clone()
                };
                delete_belief(conn, b)?;
                insert_belief(conn, &closed)?;
            }
            if let Some(v) = &ev.value {
                let mut prov = vec![ev.id.clone()];
                if let Some(b) = actives.first() {
                    for p in &b.prov {
                        if !prov.contains(p) {
                            prov.push(p.clone());
                        }
                    }
                }
                insert_belief(
                    conn,
                    &Belief {
                        scope: scope.clone(),
                        key: key.clone(),
                        valid_from: vf,
                        valid_to: None,
                        value: v.clone(),
                        status: "active".into(),
                        prov,
                        revs: vec![ev.id.clone()],
                        obs_id: ev.id.clone(),
                        updated_ts: now_ts(),
                    },
                )?;
            }
            Ok(())
        }
        // 误信：内容**从未**成立 → 追溯撤销；区间与错误内容逐字保留（`revoked`）
        Op::Correct => {
            for b in &actives {
                let mut revs = b.revs.clone();
                revs.push(ev.id.clone());
                let revoked = Belief {
                    status: "revoked".into(),
                    revs,
                    updated_ts: now_ts(),
                    ..b.clone()
                };
                delete_belief(conn, b)?;
                insert_belief(conn, &revoked)?;
            }
            Ok(())
        }
        // 粒度变化：不冲突 → 换载荷、溯源取并、状态不变
        Op::Refine => {
            let v = ev.value.clone().unwrap_or_default();
            for b in &actives {
                let mut prov = b.prov.clone();
                prov.push(ev.id.clone());
                let mut revs = b.revs.clone();
                revs.push(ev.id.clone());
                let refined = Belief {
                    value: v.clone(),
                    prov,
                    revs,
                    updated_ts: now_ts(),
                    ..b.clone()
                };
                delete_belief(conn, b)?;
                insert_belief(conn, &refined)?;
            }
            Ok(())
        }
        // 去重：源键关闭；目标键（`value`）若已有 active，则把溯源并过去
        Op::Merge => {
            let dest = ev.value.clone().unwrap_or_default();
            for b in &actives {
                let mut revs = b.revs.clone();
                revs.push(ev.id.clone());
                let closed = Belief {
                    status: "superseded".into(),
                    revs,
                    updated_ts: now_ts(),
                    ..b.clone()
                };
                delete_belief(conn, b)?;
                insert_belief(conn, &closed)?;
            }
            if !dest.is_empty() {
                let dst = active_at(conn, scope, &dest)?;
                if let Some(d) = dst.first() {
                    let mut prov = d.prov.clone();
                    for b in &actives {
                        for p in &b.prov {
                            if !prov.contains(p) {
                                prov.push(p.clone());
                            }
                        }
                    }
                    let mut revs = d.revs.clone();
                    revs.push(ev.id.clone());
                    let merged = Belief {
                        prov,
                        revs,
                        updated_ts: now_ts(),
                        ..d.clone()
                    };
                    delete_belief(conn, d)?;
                    insert_belief(conn, &merged)?;
                }
            }
            Ok(())
        }
        // split：一条信念混淆了两个实体 → 源键关闭，在新键（`value`）装同样的值
        Op::Split => {
            let dest_key = ev.value.clone().unwrap_or_default();
            for b in &actives {
                let mut revs = b.revs.clone();
                revs.push(ev.id.clone());
                let closed = Belief {
                    status: "superseded".into(),
                    revs: revs.clone(),
                    updated_ts: now_ts(),
                    ..b.clone()
                };
                delete_belief(conn, b)?;
                insert_belief(conn, &closed)?;
                if !dest_key.is_empty() {
                    insert_belief(
                        conn,
                        &Belief {
                            scope: scope.clone(),
                            key: dest_key.clone(),
                            valid_from: b.valid_from,
                            valid_to: b.valid_to,
                            value: b.value.clone(),
                            status: "active".into(),
                            prov: b.prov.clone(),
                            revs: revs.clone(),
                            obs_id: ev.id.clone(),
                            updated_ts: now_ts(),
                        },
                    )?;
                }
            }
            Ok(())
        }
        // 反证强度相当：**双方并存互指**，不做任何改写（"不确定性必须被表示"）
        Op::Contested => {
            let v = ev.value.clone().unwrap_or_default();
            for b in &actives {
                let mut revs = b.revs.clone();
                revs.push(ev.id.clone());
                let c = Belief {
                    status: "contested".into(),
                    revs,
                    updated_ts: now_ts(),
                    ..b.clone()
                };
                delete_belief(conn, b)?;
                insert_belief(conn, &c)?;
            }
            if !v.is_empty() {
                let (vf, vt) = actives
                    .first()
                    .map(|b| (b.valid_from, b.valid_to))
                    .unwrap_or((ev.ts, None));
                insert_belief(
                    conn,
                    &Belief {
                        scope: scope.clone(),
                        key: key.clone(),
                        valid_from: vf,
                        valid_to: vt,
                        value: v,
                        status: "contested".into(),
                        prov: vec![ev.id.clone()],
                        revs: vec![ev.id.clone()],
                        obs_id: ev.id.clone(),
                        updated_ts: now_ts(),
                    },
                )?;
            }
            Ok(())
        }
    }
}

/// 从账本**重放**重建折叠层（`beliefs` + FTS + 向量）。
///
/// 这是 EC 的可证伪形态：把派生层整个删掉，只要账本在，**当前信念必须一字不差地回来**
/// （`mem::tests` 里就是拿它当判据）。
pub fn rebuild(mem: &Memory, scope: Option<&str>) -> Result<usize, String> {
    let conn = mem.conn()?;
    {
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("开事务失败: {e}"))?;
        match scope {
            Some(s) => {
                // 先按 scope 的 row_key 清向量，再清信念/FTS —— 不清的话重建后可能留着旧向量
                tx.execute(
                    "DELETE FROM vectors WHERE row_key IN (SELECT scope||'|'||key||'|'||valid_from||'|'||obs_id FROM beliefs WHERE scope=?1)",
                    params![s],
                )
                .map_err(|e| e.to_string())?;
                tx.execute("DELETE FROM beliefs WHERE scope=?1", params![s])
                    .map_err(|e| e.to_string())?;
                tx.execute("DELETE FROM fts WHERE scope=?1", params![s])
                    .map_err(|e| e.to_string())?;
            }
            None => {
                tx.execute("DELETE FROM beliefs", [])
                    .map_err(|e| e.to_string())?;
                tx.execute("DELETE FROM fts", [])
                    .map_err(|e| e.to_string())?;
                tx.execute("DELETE FROM vectors", [])
                    .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| format!("清派生层失败: {e}"))?;
    }
    let events = super::ledger::all(mem, scope)?;
    let mut n = 0usize;
    for ev in &events {
        match ev.kind {
            EventKind::Obs => apply_obs(&conn, ev)?,
            EventKind::Rev => apply_rev(&conn, ev)?,
        }
        n += 1;
    }
    Ok(n)
}
