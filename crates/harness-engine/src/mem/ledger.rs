//! **账本**（证据层）：只追加、哈希链式、永不删。
//!
//! 两条来自论文的纪律，落在这里：
//!
//! 1. **推翻也是一条事件**（`Rev`）：所以"后文推翻前文"不会让任何东西消失 —— 这正是 EC 成立的方式；
//! 2. **决策即记录**：裁决器（LLM 或规则）是非确定的，所以产生决策的那次判断必须作为数据落账
//!    （`op` + `prov` 依据 + `reason` 理由），否则重放不可能一致。
//!
//! 哈希链：`hash = sha256(prev_hash ‖ 本条内容)`。它是**可选**的防篡改（本机单用户场景不承担安全
//! 职责），但便宜且让"账本没被动过"变成可验证的事实。

use super::Memory;
use rusqlite::params;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Obs,
    Rev,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Obs => "obs",
            EventKind::Rev => "rev",
        }
    }
    pub fn parse(s: &str) -> EventKind {
        match s {
            "rev" => EventKind::Rev,
            _ => EventKind::Obs,
        }
    }
}

/// 这条事实**从哪来**。probe = 确定性探针（免费证据，不需要模型裁决）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// 确定性探针：文件系统 / 命令探测 / 环境自检 —— 它自己就是证据。
    Probe,
    /// agent 在跑的途中学会的（要裁决）。
    Agent,
    /// 用户显式给的（配置 / 手写）。
    Human,
    /// 引擎自己的决策（写策略、验证结论、复核发现）。
    Engine,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Probe => "probe",
            Origin::Agent => "agent",
            Origin::Human => "human",
            Origin::Engine => "engine",
        }
    }
    pub fn parse(s: &str) -> Origin {
        match s {
            "probe" => Origin::Probe,
            "agent" => Origin::Agent,
            "human" => Origin::Human,
            _ => Origin::Engine,
        }
    }
}

/// 一条账本事件。`seq` 是便宜的总序（事务序）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub seq: i64,
    pub id: String,
    pub ts: i64,
    pub scope: String,
    pub kind: EventKind,
    pub key: String,
    pub value: Option<String>,
    pub valid_from: Option<i64>,
    pub valid_to: Option<i64>,
    pub op: Option<String>,
    pub prov: Vec<String>,
    pub reason: String,
    pub origin: Origin,
    pub hash: String,
    pub prev_hash: String,
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 内容寻址的 id：同内容同 id（幂等重放友好），并带时间戳前缀便于人眼排序。
fn make_id(
    ts: i64,
    scope: &str,
    kind: &str,
    key: &str,
    value: Option<&str>,
    prev_hash: &str,
) -> String {
    let mut h = Sha256::new();
    for part in [
        ts.to_string(),
        scope.to_string(),
        kind.to_string(),
        key.to_string(),
        value.unwrap_or("\u{0}").to_string(),
        prev_hash.to_string(),
    ] {
        h.update(part.as_bytes());
        h.update([0u8]);
    }
    format!("{ts}-{}", &hex(&h.finalize())[..12])
}

/// 一条事件的**内容指纹**（按固定字段顺序拼）。append 与 verify 必须共用它 ——
/// 否则"只比前后指针"的校验**改值也验不出来**（这个洞是被测试当场抓出来的：改了
/// `events.value` 之后 `verify_chain` 仍然返回"链完整"）。
#[allow(clippy::too_many_arguments)]
fn content_of_parts(
    id: &str,
    ts: i64,
    scope: &str,
    kind: EventKind,
    key: &str,
    value: Option<&str>,
    valid_from: Option<i64>,
    op: Option<&str>,
    prov: &[String],
    reason: &str,
    origin: Origin,
) -> String {
    format!(
        "{id}|{ts}|{scope}|{}|{key}|{}|{}|{}|{}|{reason}|{}",
        kind.as_str(),
        value.unwrap_or(""),
        valid_from
            .map(|v| v.to_string())
            .unwrap_or_else(|| "open".into()),
        op.unwrap_or_default(),
        prov.join(","),
        origin.as_str()
    )
}

fn chain_hash(prev_hash: &str, content: &str) -> String {
    let mut h = Sha256::new();
    h.update(prev_hash.as_bytes());
    h.update([0u8]);
    h.update(content.as_bytes());
    hex(&h.finalize())
}

fn head_hash(conn: &rusqlite::Connection) -> Result<String, String> {
    conn.query_row(
        "SELECT hash FROM events ORDER BY seq DESC LIMIT 1",
        [],
        |r| r.get::<_, String>(0),
    )
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(String::new()),
        other => Err(other),
    })
    .map_err(|e| format!("读账本头失败: {e}"))
}

// 账本一条事件本来就有这么多字段（id/时间/作用域/键/值/有效期/算子/依据/理由/来源）——
// 拆成结构体只是把参数换个地方写，反而让调用点更难看出"落的是什么"。
#[allow(clippy::too_many_arguments)]
fn append(
    mem: &Memory,
    scope: &str,
    kind: EventKind,
    key: &str,
    value: Option<&str>,
    valid_from: Option<i64>,
    op: Option<String>,
    prov: &[String],
    reason: &str,
    origin: Origin,
) -> Result<Event, String> {
    let conn = mem.conn()?;
    let ts = chrono::Utc::now().timestamp();
    let prev_hash = head_hash(&conn)?;
    let id = make_id(ts, scope, kind.as_str(), key, value, &prev_hash);
    let hash = chain_hash(
        &prev_hash,
        &content_of_parts(
            &id,
            ts,
            scope,
            kind,
            key,
            value,
            valid_from,
            op.as_deref(),
            prov,
            reason,
            origin,
        ),
    );
    conn.execute(
        "INSERT INTO events (id, ts, scope, kind, key, value, valid_from, valid_to, op, prov, reason, origin, prev_hash, hash)
         VALUES (?1,?2,?3,?4,?5,?6,?7,NULL,?8,?9,?10,?11,?12,?13)",
        params![
            id, ts, scope, kind.as_str(), key, value, valid_from, op, serde_json::to_string(prov).unwrap_or_else(|_| "[]".into()),
            reason, origin.as_str(), prev_hash, hash
        ],
    )
    .map_err(|e| format!("写账本失败: {e}"))?;
    let seq = conn.last_insert_rowid();
    Ok(Event {
        seq,
        id: id.clone(),
        ts,
        scope: scope.to_string(),
        kind,
        key: key.to_string(),
        value: value.map(|v| v.to_string()),
        valid_from,
        valid_to: None,
        op,
        prov: prov.to_vec(),
        reason: reason.to_string(),
        origin,
        hash,
        prev_hash,
    })
}

pub fn append_obs(
    mem: &Memory,
    scope: &str,
    key: &str,
    value: &str,
    origin: Origin,
    prov: &[String],
    valid_from: Option<i64>,
) -> Result<Event, String> {
    append(
        mem,
        scope,
        EventKind::Obs,
        key,
        Some(value),
        valid_from,
        None,
        prov,
        "",
        origin,
    )
}

// 同 `append`：修订事件的字段本来就多（算子 / 目标槽位 / 新值 / 依据 / 理由 / 有效时间）
#[allow(clippy::too_many_arguments)]
pub fn append_rev(
    mem: &Memory,
    scope: &str,
    op: super::fold::Op,
    key: &str,
    value: Option<&str>,
    prov: &[String],
    reason: &str,
    at: Option<i64>,
) -> Result<Event, String> {
    // `correct` 的失效时刻：撤销意味着"从未成立"，所以不给有效期边界；
    // `supersede` 的有效时间是**世界时间**（可显式给 = 迟到证据的场景），默认取当下。
    let vf = match op {
        super::fold::Op::Correct | super::fold::Op::Contested => None,
        _ => Some(at.unwrap_or_else(|| chrono::Utc::now().timestamp())),
    };
    append(
        mem,
        scope,
        EventKind::Rev,
        key,
        value,
        vf,
        Some(op.as_str().into()),
        prov,
        reason,
        Origin::Engine,
    )
}

fn row_to_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    let prov: String = r.get("prov")?;
    let kind: String = r.get("kind")?;
    let origin: String = r.get("origin")?;
    Ok(Event {
        seq: r.get("seq")?,
        id: r.get("id")?,
        ts: r.get("ts")?,
        scope: r.get("scope")?,
        kind: EventKind::parse(&kind),
        key: r.get::<_, Option<String>>("key")?.unwrap_or_default(),
        value: r.get("value")?,
        valid_from: r.get("valid_from")?,
        valid_to: r.get("valid_to")?,
        op: r.get("op")?,
        prov: serde_json::from_str(&prov).unwrap_or_default(),
        reason: r.get("reason")?,
        origin: Origin::parse(&origin),
        hash: r.get("hash")?,
        prev_hash: r.get("prev_hash")?,
    })
}

const COLS: &str =
    "seq,id,ts,scope,kind,key,value,valid_from,valid_to,op,prov,reason,origin,hash,prev_hash";

/// 账本全量（按 seq = 事务序）。**折叠、重建、审计都走它** —— 这是"证据"的唯一入口。
pub fn all(mem: &Memory, scope: Option<&str>) -> Result<Vec<Event>, String> {
    let conn = mem.conn()?;
    let (sql, args): (String, Vec<String>) = match scope {
        Some(s) => (
            format!("SELECT {COLS} FROM events WHERE scope=?1 ORDER BY seq"),
            vec![s.to_string()],
        ),
        None => (format!("SELECT {COLS} FROM events ORDER BY seq"), vec![]),
    };
    let mut stmt = conn.prepare(&sql).map_err(|e| format!("读账本失败: {e}"))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(args.iter()), row_to_event)
        .map_err(|e| format!("读账本失败: {e}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| format!("读账本失败: {e}"))
}

pub fn by_ids(mem: &Memory, ids: &[String]) -> Result<Vec<Event>, String> {
    let conn = mem.conn()?;
    let mut out = Vec::new();
    for id in ids {
        let mut stmt = conn
            .prepare(&format!("SELECT {COLS} FROM events WHERE id=?1"))
            .map_err(|e| e.to_string())?;
        if let Ok(ev) = stmt.query_row(params![id], row_to_event) {
            out.push(ev);
        }
    }
    Ok(out)
}

/// 链式自检：逐条验 `prev_hash` 与 `hash`。返回第一个断点的 seq（None = 完整）。
pub fn verify_chain(mem: &Memory) -> Result<Option<i64>, String> {
    let evs = all(mem, None)?;
    let mut prev = String::new();
    for ev in &evs {
        // 两件事都要验：① 前后指针连得上；② **内容哈希等于存下来的哈希**
        // （只做①的话，直接 UPDATE events.value 是验不出来的）
        if ev.prev_hash != prev {
            return Ok(Some(ev.seq));
        }
        let recomputed = chain_hash(
            &ev.prev_hash,
            &content_of_parts(
                &ev.id,
                ev.ts,
                &ev.scope,
                ev.kind,
                &ev.key,
                ev.value.as_deref(),
                ev.valid_from,
                ev.op.as_deref(),
                &ev.prov,
                &ev.reason,
                ev.origin,
            ),
        );
        if recomputed != ev.hash {
            return Ok(Some(ev.seq));
        }
        prev = ev.hash.clone();
    }
    Ok(None)
}
