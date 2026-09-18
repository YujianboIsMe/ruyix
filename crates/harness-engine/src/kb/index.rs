//! 索引层：分词 → 切分 → SQLite FTS5 → 增量与陈旧检测。
//!
//! ## 三个关键决定
//!
//! 1. **中文用二元切分（bigram）预分词**，不是 jieba。FTS5 的 `unicode61` 压根不切中文，
//!    所以必须自己先把内容切好再入库；而 bigram（相邻两字成词）是纯 Rust、零字典依赖、
//!    完全确定性的做法，且对"用词与文档高度重合"的 Agent 查询（规划步骤文本、规则 ID）
//!    精度足够。真要有词粒度需求，只需换掉 [`tokenize`] 一个函数。
//! 2. **增量按内容 hash**：hash 没变就不动这份文档的 chunk（省的是重建整个库的时间，
//!    也让"重建"这件事变得可以随手点）。hash 用 FNV-1a 64 —— 我们要的是"变了没有"，
//!    不是抗碰撞，所以不引 sha/md5 依赖（诚实边界：它不是密码学哈希）。
//! 3. **一个来源一个库文件**：删来源就是删文件，不会有孤儿数据；陈旧检测、来源统计
//!    各自独立，互不影响。
//!
//! ## FTS5 的同步方式
//!
//! 不用触发器（那是多写入方场景的解法）。这里写入方只有索引器一个，所以
//! `chunks`（业务表）与 `chunks_fts`（索引表）显式成对维护，删除一律用
//! `DELETE FROM chunks_fts WHERE rowid = ?` —— **不用 FTS5 的 `'delete'` 特殊命令**
//! （本机在 Code-Rag 上验证过它在内置 sqlite3 上不可靠）。

use super::{KbConfig, KbDoc, KbEntry, StaleView};
use crate::exec::{CancelFlag, is_cancelled};
use rusqlite::{Connection, params};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

// ---------------------------------------------------------------- 哈希

/// FNV-1a 64：够用的内容指纹（**不是密码学哈希**，别拿它当安全边界）。
pub fn content_hash(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

// ---------------------------------------------------------------- 分词

fn is_cjk(c: char) -> bool {
    // 中日韩统一表意文字 + 扩展 A + 兼容区 + 假名/谚文（够用即可，不追求全覆盖）
    matches!(c as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF |
        0x3040..=0x30FF | 0xAC00..=0xD7AF)
}

/// 把文本切成检索用的 token 序列。
///
/// - ASCII 字母/数字：整段小写成一个 token（`snake_case` 会被下划线切开，
///   与 jieba + unicode61 的行为一致）；
/// - 中文：**相邻两字成词**（长度 1 的单独成 token）；
/// - 其它字符（标点、空白）是分隔符。
pub fn tokenize(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut ascii = String::new();
    let mut cjk: Vec<char> = Vec::new();

    fn flush_ascii(buf: &mut String, out: &mut Vec<String>) {
        if !buf.is_empty() {
            out.push(std::mem::take(buf));
        }
    }
    fn flush_cjk(buf: &mut Vec<char>, out: &mut Vec<String>) {
        if buf.is_empty() {
            return;
        }
        if buf.len() == 1 {
            out.push(buf[0].to_string());
        } else {
            for w in buf.windows(2) {
                out.push(w.iter().collect());
            }
        }
        buf.clear();
    }

    for c in text.chars() {
        if is_cjk(c) {
            flush_ascii(&mut ascii, &mut out);
            cjk.push(c);
        } else if c.is_ascii_alphanumeric() {
            flush_cjk(&mut cjk, &mut out);
            ascii.push(c.to_ascii_lowercase());
        } else {
            flush_cjk(&mut cjk, &mut out);
            flush_ascii(&mut ascii, &mut out);
        }
    }
    flush_cjk(&mut cjk, &mut out);
    flush_ascii(&mut ascii, &mut out);
    out
}

/// 入库/查询用的预分词文本（空格连接）。查询侧同样用它，才能和库里的 token 对上。
pub fn tokenize_join(text: &str) -> String {
    let mut toks = tokenize(text);
    toks.dedup();
    toks.join(" ")
}

/// 覆盖率的计量单位：**中文按单字、ASCII 按词**。
///
/// 为什么不用 bigram 当单位：bigram 会切出跨词项（"预算裁剪" → 算裁），
/// 那个 bigram 天然不在任何文档里 —— 拿它算覆盖率会把**完全对题的文档**也压低。
/// 召回用 bigram（它召回好），质量信号用字级覆盖（它稳）。
pub fn coverage_units(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut ascii = String::new();
    for c in text.chars() {
        if is_cjk(c) {
            if !ascii.is_empty() {
                out.push(std::mem::take(&mut ascii));
            }
            out.push(c.to_string());
        } else if c.is_ascii_alphanumeric() {
            ascii.push(c.to_ascii_lowercase());
        } else if !ascii.is_empty() {
            out.push(std::mem::take(&mut ascii));
        }
    }
    if !ascii.is_empty() {
        out.push(ascii);
    }
    out
}

fn unique_units(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    coverage_units(text)
        .into_iter()
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

/// 去重但保序（FTS5 的 MATCH 里重复项只是浪费，不影响结果）。
pub fn unique_tokens(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    tokenize(text)
        .into_iter()
        .filter(|t| seen.insert(t.clone()))
        .collect()
}

// ---------------------------------------------------------------- 切分

/// 一个 chunk：**切分不能撕开语义单元**（标题、代码块、函数体）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Chunk {
    pub ordinal: usize,
    pub title: String,
    pub text: String,
}

/// 代码/文档的"可切分点"标记（顶层定义开头）。
const ITEM_KEYS: &[&str] = &[
    "def ",
    "class ",
    "fn ",
    "pub fn ",
    "pub struct ",
    "pub enum ",
    "impl ",
    "func ",
    "async fn ",
    "export ",
    "function ",
    "void ",
    "struct ",
    "interface ",
    "const ",
    "async def ",
    "test ",
];

fn looks_like_heading(t: &str) -> bool {
    let hs = t.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hs) && t.chars().nth(hs).map(|c| c == ' ').unwrap_or(false)
}

fn looks_like_item(t: &str) -> bool {
    ITEM_KEYS.iter().any(|k| t.starts_with(k))
}

/// 按行切分，`target` 字符左右一块。
///
/// 规则（都被单测钉着）：
/// - **绝不切开```代码围栏**：围栏内的行永远留在同一块里；
/// - 只在"好边界"处切：空行、标题行、顶层定义行；
/// - 一块就是 1.5~2 倍目标也不切（宁可长一点也不撕开语义）；
/// - 每块带标题：最近的 markdown 标题（没有就用文件标题）。
pub fn chunk_text(text: &str, target: usize) -> Vec<Chunk> {
    let target = target.max(200);
    let mut out: Vec<Chunk> = Vec::new();
    let mut buf = String::new();
    let mut heading = String::new();
    let mut fence: Option<char> = None;
    let file_title = super::local::title_of(text);

    fn flush(buf: &mut String, out: &mut Vec<Chunk>, heading: &str, file_title: &str) {
        let body = buf.trim_end();
        if !body.trim().is_empty() {
            out.push(Chunk {
                ordinal: out.len(),
                title: if heading.trim().is_empty() {
                    file_title.to_string()
                } else {
                    heading.to_string()
                },
                text: body.to_string(),
            });
        }
        buf.clear();
    }

    for line in text.lines() {
        let t = line.trim_start();
        // 围栏开关：只有同一种标记才能关掉自己打开的围栏
        if t.starts_with("```") || t.starts_with("~~~") {
            let c = t.chars().next().unwrap_or('`');
            fence = match fence {
                None => Some(c),
                Some(open) if open == c => None,
                other => other,
            };
        }
        let in_fence = fence.is_some();
        if !in_fence
            && buf.len() >= target
            && (line.trim().is_empty() || looks_like_heading(t) || looks_like_item(t))
        {
            flush(&mut buf, &mut out, &heading, &file_title);
        }
        if !in_fence && looks_like_heading(t) {
            heading = t.trim_start_matches('#').trim().to_string();
            // 标题本身就是块的开头：标题前的东西先收尾
            if !buf.trim().is_empty() {
                let saved = std::mem::take(&mut heading);
                flush(&mut buf, &mut out, &saved, &file_title);
                heading = saved;
            }
        }
        buf.push_str(line);
        buf.push('\n');
        // 硬上限：到了两倍目标，只要不在围栏里就必须切
        if fence.is_none() && buf.len() >= target * 2 {
            flush(&mut buf, &mut out, &heading, &file_title);
        }
    }
    flush(&mut buf, &mut out, &heading, &file_title);
    out
}

// ---------------------------------------------------------------- 存储

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS files(
    path TEXT PRIMARY KEY, hash TEXT NOT NULL, mtime_ms INTEGER NOT NULL,
    size INTEGER NOT NULL, chunks INTEGER NOT NULL, indexed_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS chunks(
    id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT NOT NULL, ordinal INTEGER NOT NULL,
    title TEXT NOT NULL, text TEXT NOT NULL, tokens TEXT NOT NULL, hash TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_chunks_path ON chunks(path);
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(tokens);
"#;

#[derive(Default, Clone, Debug)]
pub struct StoreStats {
    pub doc_count: usize,
    pub chunk_count: usize,
    pub indexed_at: String,
}

/// 打开（必要时创建）某个来源的索引库。
pub fn open(dir: &Path, id: &str) -> Result<Connection, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建知识库目录失败: {e}"))?;
    let path = dir.join(format!("{id}.db"));
    let conn =
        Connection::open(&path).map_err(|e| format!("打开索引库失败 {}: {e}", path.display()))?;
    conn.execute_batch(SCHEMA).map_err(|e| {
        format!("初始化索引库失败（若报 no such module: fts5，说明这个 SQLite 没有 FTS5）：{e}")
    })?;
    Ok(conn)
}

fn open_existing(dir: &Path, id: &str) -> Result<Connection, String> {
    let path = dir.join(format!("{id}.db"));
    if !path.exists() {
        return Err(format!("这个来源还没有索引（{}）", path.display()));
    }
    Connection::open(&path).map_err(|e| format!("打开索引库失败 {}: {e}", path.display()))
}

pub fn stats(dir: &Path, id: &str) -> Result<StoreStats, String> {
    let conn = match open_existing(dir, id) {
        Ok(c) => c,
        Err(_) => return Ok(StoreStats::default()),
    };
    let docs: i64 = conn
        .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
        .unwrap_or(0);
    let chunks: i64 = conn
        .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))
        .unwrap_or(0);
    let indexed_at: String = conn
        .query_row("SELECT value FROM meta WHERE key = 'indexed_at'", [], |r| {
            r.get(0)
        })
        .unwrap_or_default();
    Ok(StoreStats {
        doc_count: docs.max(0) as usize,
        chunk_count: chunks.max(0) as usize,
        indexed_at,
    })
}

fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO meta(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map(|_| ())
    .map_err(|e| format!("写 meta 失败: {e}"))
}

fn delete_doc(tx: &rusqlite::Transaction<'_>, path: &str) -> Result<(), String> {
    let ids: Vec<i64> = {
        let mut stmt = tx
            .prepare("SELECT id FROM chunks WHERE path = ?1")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![path], |r| r.get::<_, i64>(0))
            .map_err(|e| e.to_string())?;
        rows.filter_map(|r| r.ok()).collect()
    };
    for id in ids {
        // 用普通 DELETE（FTS5 的 'delete' 特殊命令在 Python/内置 sqlite3 上有坑）
        tx.execute("DELETE FROM chunks_fts WHERE rowid = ?1", params![id])
            .map_err(|e| format!("清 FTS 行失败: {e}"))?;
    }
    tx.execute("DELETE FROM chunks WHERE path = ?1", params![path])
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn insert_chunk(tx: &rusqlite::Transaction<'_>, path: &str, c: &Chunk) -> Result<(), String> {
    let tokens = tokenize_join(&c.text);
    tx.execute(
        "INSERT INTO chunks(path, ordinal, title, text, tokens, hash)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            path,
            c.ordinal as i64,
            c.title,
            c.text,
            tokens,
            content_hash(c.text.as_bytes())
        ],
    )
    .map_err(|e| format!("写 chunk 失败: {e}"))?;
    let rowid = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO chunks_fts(rowid, tokens) VALUES(?1, ?2)",
        params![rowid, tokens],
    )
    .map_err(|e| format!("写 FTS 索引失败: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------- 索引

#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub phase: String,
    pub done: usize,
    pub total: usize,
    pub msg: String,
}

#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    pub scanned: usize,
    pub added: usize,
    pub updated: usize,
    pub skipped: usize,
    pub removed: usize,
    pub chunk_count: usize,
    pub doc_count: usize,
    pub skipped_files: Vec<(String, String)>,
    pub indexed_at: String,
    pub elapsed_ms: u128,
    pub cancelled: bool,
}

impl IndexStats {
    pub fn summary(&self) -> String {
        format!(
            "{} 篇文档 / {} 个 chunk（新增 {} · 更新 {} · 未变 {} · 删除 {}{}）· {} ms",
            self.doc_count,
            self.chunk_count,
            self.added,
            self.updated,
            self.skipped,
            self.removed,
            if self.cancelled { " · 已取消" } else { "" },
            self.elapsed_ms
        )
    }
}

/// 建/更新一个来源的索引（增量：内容 hash 没变就跳过）。
pub fn index_source(
    dir: &Path,
    entry: &KbEntry,
    cfg: &KbConfig,
    cancel: &CancelFlag,
    progress: &mut dyn FnMut(Progress),
) -> Result<IndexStats, String> {
    let t0 = Instant::now();
    let src = super::source_for(entry, cfg)?;
    crate::observe::log(
        "info",
        "kb",
        format!(
            "索引来源「{}」（类型 {}）：{}",
            src.label(),
            src.kind(),
            entry.path
        ),
    );
    progress(Progress {
        phase: "scan".into(),
        total: 0,
        done: 0,
        msg: format!("扫描 {}", entry.path),
    });
    let scan = src.fetch(cancel)?;
    let total = scan.docs.len();
    let mut st = IndexStats {
        scanned: total,
        skipped_files: scan.skipped.clone(),
        cancelled: scan.cancelled,
        ..Default::default()
    };

    let mut conn = open(dir, &entry.id)?;
    // 旧索引状态：先读出来（statement 用完即弃，避免与后面的写操作借冲突）
    let existing: HashMap<String, (String, u64, u64)> = {
        let mut stmt = conn
            .prepare("SELECT path, hash, mtime_ms, size FROM files")
            .map_err(|e| format!("读索引状态失败: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?.max(0) as u64,
                    r.get::<_, i64>(3)?.max(0) as u64,
                ))
            })
            .map_err(|e| format!("读索引状态失败: {e}"))?;
        let mut m = HashMap::new();
        for row in rows.flatten() {
            m.insert(row.0, (row.1, row.2, row.3));
        }
        m
    };

    let now = crate::workspace::now_iso();
    let mut seen: HashSet<String> = HashSet::new();
    {
        let tx = conn
            .transaction()
            .map_err(|e| format!("开启事务失败: {e}"))?;
        for (i, doc) in scan.docs.iter().enumerate() {
            if is_cancelled(cancel) {
                st.cancelled = true;
                break;
            }
            seen.insert(doc.path.clone());
            let unchanged = existing
                .get(&doc.path)
                .map(|(h, _, _)| h == &doc.rev)
                .unwrap_or(false);
            if unchanged {
                st.skipped += 1;
            } else {
                delete_doc(&tx, &doc.path)?;
                let chunks = chunk_text(&doc.text, cfg.chunk_chars);
                for c in &chunks {
                    insert_chunk(&tx, &doc.path, c)?;
                }
                write_file_row(&tx, doc, chunks.len(), &now)?;
                if existing.contains_key(&doc.path) {
                    st.updated += 1;
                } else {
                    st.added += 1;
                }
                progress(Progress {
                    phase: "index".into(),
                    done: i + 1,
                    total,
                    msg: format!("{}（{} chunk）", doc.path, chunks.len()),
                });
            }
        }
        // 源文件消失了 → 从索引里删掉。不删就会留下"幽灵命中"：
        // 检索命中一个已经不存在的文件，比命中不了更糟。
        if !st.cancelled {
            let gone: Vec<String> = existing
                .keys()
                .filter(|p| !seen.contains(*p))
                .cloned()
                .collect();
            for p in gone {
                delete_doc(&tx, &p)?;
                tx.execute("DELETE FROM files WHERE path = ?1", params![p])
                    .map_err(|e| e.to_string())?;
                st.removed += 1;
            }
        }
        tx.commit().map_err(|e| format!("提交索引事务失败: {e}"))?;
    }

    meta_set(&conn, "indexed_at", &now)?;
    meta_set(&conn, "kind", &entry.kind)?;
    meta_set(&conn, "label", &entry.label)?;
    meta_set(&conn, "path", &entry.path)?;
    let s = stats(dir, &entry.id)?;
    st.doc_count = s.doc_count;
    st.chunk_count = s.chunk_count;
    st.indexed_at = now;
    st.elapsed_ms = t0.elapsed().as_millis();
    progress(Progress {
        phase: "done".into(),
        done: total,
        total,
        msg: st.summary(),
    });
    Ok(st)
}

fn write_file_row(
    tx: &rusqlite::Transaction<'_>,
    doc: &KbDoc,
    chunks: usize,
    now: &str,
) -> Result<(), String> {
    tx.execute(
        "INSERT INTO files(path, hash, mtime_ms, size, chunks, indexed_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(path) DO UPDATE SET hash = excluded.hash, mtime_ms = excluded.mtime_ms,
             size = excluded.size, chunks = excluded.chunks, indexed_at = excluded.indexed_at",
        params![
            doc.path,
            doc.rev,
            doc.mtime_ms as i64,
            doc.size as i64,
            chunks as i64,
            now
        ],
    )
    .map(|_| ())
    .map_err(|e| format!("写文件行失败: {e}"))
}

// ---------------------------------------------------------------- 检索

#[derive(Debug, Clone, Default)]
pub struct RawHit {
    pub path: String,
    pub ordinal: usize,
    pub title: String,
    pub text: String,
    /// 归一化前的 BM25 原始分（越大越相关）
    pub bm25: f64,
    /// 查询词命中比例（0~1）：这是**绝对的**信号，用来兜住"只命中一个词却排第一"
    pub coverage: f64,
}

#[derive(Debug, Clone, Default)]
pub struct SearchOutcome {
    pub hits: Vec<RawHit>,
    /// and（所有词都命中）/ or（降级为任一命中）/ none（一个都没命中）
    pub strategy: String,
    pub tokens: usize,
}

/// 在一个来源里检索。
///
/// **先 AND 后 OR**：AND 精度高但召回差（长查询很容易全落空），OR 召回了大量
/// 只沾一个词的噪声。所以先按"所有词都要有"召，空了再降级 —— 且**把用了哪条路记进
/// 结果**（strategy），否则"这次召回到的是噪声"永远查不出来。
pub fn search(dir: &Path, id: &str, query: &str, limit: usize) -> Result<SearchOutcome, String> {
    let tokens = unique_tokens(query);
    if tokens.is_empty() {
        return Ok(SearchOutcome {
            strategy: "none".into(),
            tokens: 0,
            hits: Vec::new(),
        });
    }
    let conn = open_existing(dir, id)?;
    let limit = limit.max(1) as i64;
    let units = unique_units(query);
    let mut last = SearchOutcome {
        strategy: "none".into(),
        tokens: tokens.len(),
        hits: Vec::new(),
    };
    for all_terms in [true, false] {
        let expr = tokens
            .iter()
            .map(|t| format!("\"{t}\""))
            .collect::<Vec<_>>()
            .join(if all_terms { " AND " } else { " OR " });
        let mut stmt = conn
            .prepare(
                "SELECT c.path, c.ordinal, c.title, c.text, c.tokens, bm25(chunks_fts)
                 FROM chunks_fts JOIN chunks c ON c.id = chunks_fts.rowid
                 WHERE chunks_fts MATCH ?1
                 ORDER BY bm25(chunks_fts) ASC LIMIT ?2",
            )
            .map_err(|e| format!("检索语句准备失败: {e}"))?;
        let rows = stmt
            .query_map(params![expr, limit], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, f64>(5)?,
                ))
            })
            .map_err(|e| format!("检索失败: {e}"))?;
        let mut hits = Vec::new();
        for row in rows.flatten() {
            // 覆盖率在**字/词**这一级算：bigram 负责召回，字级覆盖负责"够不够对题"
            let doc_units: HashSet<String> = unique_units(&row.3).into_iter().collect();
            let hit_n = units.iter().filter(|u| doc_units.contains(*u)).count();
            hits.push(RawHit {
                path: row.0,
                ordinal: row.1.max(0) as usize,
                title: row.2,
                text: row.3,
                bm25: -row.5,
                coverage: hit_n as f64 / (units.len() as f64).max(1.0),
            });
        }
        if !hits.is_empty() {
            last.strategy = if all_terms { "and" } else { "or" }.into();
            last.hits = hits;
            return Ok(last);
        }
    }
    Ok(last)
}

// ---------------------------------------------------------------- 陈旧检测

/// 快速陈旧检测：**只 stat，不读内容**。
///
/// 诚实边界：它回答"看起来变了几个文件"，不回答"内容真的变了没有"
/// （mtime/size 都不变的内容改动抓不到）。真正决定要不要重建的是内容 hash。
pub fn quick_stale(dir: &Path, entry: &KbEntry, cfg: &KbConfig) -> StaleView {
    let mut view = StaleView {
        checked: false,
        note: "还没有索引，无从比较（先点「重建」）".into(),
        ..Default::default()
    };
    if !db_path(dir, &entry.id).exists() {
        return view;
    }
    let Ok(conn) = open_existing(dir, &entry.id) else {
        return view;
    };
    drop(conn);
    // 复用 indexed_files：陈旧检测与索引统计看的必须是**同一份**索引状态
    let Ok(files_indexed) = indexed_files(dir, &entry.id) else {
        return view;
    };
    let existing: HashMap<String, (u64, u64)> = files_indexed
        .into_iter()
        .map(|(p, (_hash, m, s))| (p, (m, s)))
        .collect();

    let Ok(src) = super::source_for(entry, cfg) else {
        view.note = "来源类型不支持，无法比较陈旧".into();
        return view;
    };
    let cancel: CancelFlag = crate::exec::new_cancel_flag();
    let (files, _) = match src.walk_stats(&cancel) {
        Ok(v) => v,
        Err(e) => {
            view.note = format!("无法扫描来源：{e}");
            return view;
        }
    };
    view.checked = true;
    view.note = "基于 mtime + size 的快速扫描（不做内容 hash）".into();
    let on_disk: HashSet<&String> = files.iter().map(|f| &f.path).collect();
    for f in &files {
        match existing.get(&f.path) {
            Some((m, s)) if *m == f.mtime_ms && *s == f.size => {}
            Some(_) => {
                view.changed += 1;
                push_detail(&mut view, &f.path, "mtime 或大小变了");
            }
            None => {
                view.added += 1;
                push_detail(&mut view, &f.path, "索引里没有（新增）");
            }
        }
    }
    for p in existing.keys() {
        if !on_disk.contains(p) {
            view.missing += 1;
            push_detail(&mut view, p, "源文件已删除");
        }
    }
    view
}

fn push_detail(view: &mut StaleView, path: &str, why: &str) {
    if view.detail.len() < 10 {
        view.detail.push(format!("{path}：{why}"));
    } else if view.detail.len() == 10 {
        view.detail.push("…（其余从略）".into());
    }
}

/// 给 CLI/命令层用：把 walk 出来的候选文件数报出来（索引规模的可核对数字）
pub fn candidate_count(entry: &KbEntry, cfg: &KbConfig) -> Result<usize, String> {
    let src = super::source_for(entry, cfg)?;
    let cancel: CancelFlag = crate::exec::new_cancel_flag();
    Ok(src.walk_stats(&cancel)?.0.len())
}

/// 暴露给测试与调试：索引里有哪些文件（path → (hash, mtime, size)）
pub fn indexed_files(dir: &Path, id: &str) -> Result<HashMap<String, (String, u64, u64)>, String> {
    let conn = open_existing(dir, id)?;
    let mut stmt = conn
        .prepare("SELECT path, hash, mtime_ms, size FROM files")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?.max(0) as u64,
                r.get::<_, i64>(3)?.max(0) as u64,
            ))
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.flatten().map(|(p, h, m, s)| (p, (h, m, s))).collect())
}

/// 索引库的物理路径（GUI 里"打开目录"用）
pub fn db_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.db"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::new_cancel_flag;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "dh-kb-idx-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        fn write(&self, rel: &str, content: &str) {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        fn rm(&self, rel: &str) {
            let _ = std::fs::remove_file(self.0.join(rel));
        }
        fn store(&self) -> PathBuf {
            self.0.join("kb-store")
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn entry_for(root: &Path) -> KbEntry {
        KbEntry {
            id: super::super::entry_id("local", root),
            kind: "local".into(),
            path: root.to_string_lossy().to_string(),
            label: "t".into(),
            added_at: "now".into(),
            ..Default::default()
        }
    }

    fn index(d: &TempDir, cfg: &KbConfig) -> IndexStats {
        let e = entry_for(&d.0);
        index_source(&d.store(), &e, cfg, &new_cancel_flag(), &mut |_p| {}).unwrap()
    }

    // ---------------- 分词

    #[test]
    fn tokenizer_bigrams_chinese_and_keeps_ascii_words() {
        let t = tokenize("上下文预算 harness_kb 检索");
        assert!(t.contains(&"harness".to_string()), "{t:?}");
        assert!(t.contains(&"kb".to_string()), "{t:?}");
        assert!(t.contains(&"预算".to_string()), "{t:?}");
        assert!(t.contains(&"上下".to_string()), "{t:?}");
        // 二元切分：4 个汉字 → 3 个 bigram
        assert_eq!(tokenize("上下文预").len(), 3);
        // 单字也能检索
        assert!(tokenize("表").contains(&"表".to_string()));
    }

    #[test]
    fn tokenizer_drops_punctuation_and_markdown_noise() {
        let t = unique_tokens("### 标题：**加粗**、括号（x）");
        assert!(
            !t.iter()
                .any(|x| x.contains('*') || x.contains('#') || x.contains('：')),
            "{t:?}"
        );
        assert!(t.contains(&"标题".to_string()), "{t:?}");
    }

    #[test]
    fn bigram_queries_still_recall_the_right_document() {
        // 两侧分词必须同源，但**不能要求每个 bigram 都命中**：
        // "预算裁剪" 会切出跨词的 "算裁"，它天然不在文档里。
        // 所以这里断言的是"能召回，且相关文档排在前面"，不是"每个 token 都在"。
        let q = unique_tokens("预算裁剪");
        assert!(q.contains(&"算裁".to_string()), "bigram 会跨词：{q:?}");
        let doc = tokenize_join("按预算而不是条数截断，超预算必被裁掉");
        assert!(
            doc.split_whitespace().any(|x| x == "预算"),
            "两侧分词同源，'预算' 必须能对上"
        );

        // 真库上验一遍召回：同主题的文档必须排在无关文档之前
        let d = TempDir::new("recall");
        d.write(
            "doc/预算.md",
            "# 预算\n\n按预算而不是条数截断，超预算必被裁掉。\n",
        );
        d.write("doc/无关.md", "# 无关\n\n今天天气不错，出去走走。\n");
        let cfg = KbConfig::default();
        index(&d, &cfg);
        let e = entry_for(&d.0);
        let out = search(&d.store(), &e.id, "预算裁剪", 10).unwrap();
        assert!(!out.hits.is_empty(), "同主题文档必须被召回");
        assert_eq!(out.hits[0].path, "doc/预算.md", "{:?}", out.hits);
        assert!(
            out.hits[0].coverage >= 0.75,
            "对题文档的字级覆盖应当很高，实际 {}",
            out.hits[0].coverage
        );
    }

    // ---------------- 切分

    fn fences_balanced(t: &str) -> bool {
        let n = t
            .lines()
            .filter(|l| l.trim_start().starts_with("```"))
            .count();
        n % 2 == 0
    }

    #[test]
    fn chunker_never_splits_a_code_fence() {
        let body = "填充内容。".repeat(80); // 撑到超过两块
        let text = format!(
            "# 标题\n\n{body}\n\n```python\ndef f():\n    # ## 这不是标题\n    return 1\n```\n\n{body}\n"
        );
        let chunks = chunk_text(&text, 200);
        assert!(
            chunks.len() >= 2,
            "这份文本必须被切成多块：{}",
            chunks.len()
        );
        for c in &chunks {
            assert!(fences_balanced(&c.text), "块里出现了半截围栏：\n{}", c.text);
        }
        let holder = chunks
            .iter()
            .find(|c| c.text.contains("def f():"))
            .expect("函数体必须还在");
        assert!(
            holder.text.contains("return 1"),
            "代码块被切断了：{}",
            holder.text
        );
        assert!(
            holder.text.contains("## 这不是标题"),
            "围栏内的伪标题不许当切分点"
        );
    }

    #[test]
    fn chunker_keeps_every_line() {
        let text = "# A\n\n一\n\n二\n\n三\n\n```\ncode\n```\n\n四\n";
        let chunks = chunk_text(text, 200);
        let all: String = chunks
            .iter()
            .map(|c| c.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        for line in ["一", "二", "三", "code", "四", "# A"] {
            assert!(all.contains(line), "丢了行 {line}: {all}");
        }
    }

    #[test]
    fn chunker_titles_come_from_headings_then_file_title() {
        let body = "正文。".repeat(200);
        let text = format!("# 文档标题\n\n{body}\n\n## 第二节\n\n{body}\n");
        let chunks = chunk_text(&text, 200);
        assert!(
            chunks.iter().any(|c| c.title == "第二节"),
            "{:?}",
            chunks.iter().map(|c| &c.title).collect::<Vec<_>>()
        );
        assert!(chunks.iter().all(|c| !c.title.is_empty()));
        // 空文档：不产出空块
        assert!(chunk_text("   \n\n  ", 200).is_empty());
    }

    // ---------------- 索引 / 检索 / 增量

    #[test]
    fn fts5_is_available_and_chinese_queries_hit() {
        // 这条同时是"这个 SQLite 到底有没有 FTS5"的探针：没有 FTS5 的话
        // open() 会直接报错，而不是悄悄退化成"什么都搜不到"。
        let d = TempDir::new("fts");
        d.write(
            "doc/约定.md",
            "# 开发约定\n\n注入是零和的，必须有显式预算，超预算的片段一律裁掉并标注原因。\n",
        );
        d.write("doc/其它.md", "# 其它\n\n这里讲的是完全不相干的东西。\n");
        let cfg = KbConfig::default();
        let st = index(&d, &cfg);
        assert_eq!(st.added, 2);
        assert!(st.chunk_count >= 2, "{st:?}");

        let e = entry_for(&d.0);
        let out = search(&d.store(), &e.id, "预算", 10).unwrap();
        assert_eq!(out.strategy, "and");
        assert!(!out.hits.is_empty(), "中文查询必须能命中");
        assert!(
            out.hits[0].path.contains("约定"),
            "命中的应该是讲预算的那份：{:?}",
            out.hits.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
        assert!(out.hits[0].bm25 > 0.0, "BM25 原始分要为正（越大越相关）");
        assert!(out.hits[0].coverage > 0.0);
    }

    #[test]
    fn search_falls_back_to_or_and_says_so() {
        let d = TempDir::new("or");
        d.write("a.md", "# A\n\n只有预算这一个词。\n");
        d.write("b.md", "# B\n\n只有裁剪这一个词。\n");
        let cfg = KbConfig::default();
        index(&d, &cfg);
        let e = entry_for(&d.0);
        // 两个词分别落在不同文档 → AND 命中 0 条 → 降级 OR
        let out = search(&d.store(), &e.id, "预算 裁剪", 10).unwrap();
        assert_eq!(out.strategy, "or", "降级必须被记录下来");
        assert_eq!(out.hits.len(), 2);
        assert!(out.hits.iter().all(|h| h.coverage < 1.0));
    }

    #[test]
    fn incremental_index_skips_unchanged_and_drops_removed_files() {
        let d = TempDir::new("incr");
        d.write("a.md", "# A\n\n内容一\n");
        d.write("b.md", "# B\n\n专属标记乙乙丙丙\n");
        let cfg = KbConfig::default();
        let first = index(&d, &cfg);
        assert_eq!(first.added, 2);
        assert_eq!(first.skipped, 0);

        // 什么都没改 → 全部按 hash 跳过（这就是"重建可以随手点"的原因）
        let second = index(&d, &cfg);
        assert_eq!(second.skipped, 2, "{second:?}");
        assert_eq!(second.added, 0);
        assert_eq!(second.updated, 0);
        assert_eq!(
            second.chunk_count, first.chunk_count,
            "跳过的文档不许产生重复 chunk"
        );

        // 改一个、删一个
        d.write("a.md", "# A\n\n内容一改过了\n");
        d.rm("b.md");
        let third = index(&d, &cfg);
        assert_eq!(third.updated, 1, "{third:?}");
        assert_eq!(third.removed, 1, "{third:?}");

        let e = entry_for(&d.0);
        let files = indexed_files(&d.store(), &e.id).unwrap();
        assert!(
            !files.contains_key("b.md"),
            "删掉的源文件不许留在索引里（幽灵命中）"
        );
        assert_eq!(files.len(), 1);
        // 删掉的文档不许还能被搜到（用只在它里面出现的词来验，避免 OR 降级误伤）
        let out = search(&d.store(), &e.id, "乙乙丙丙", 10).unwrap();
        assert!(out.hits.is_empty(), "{:?}", out.hits);
    }

    #[test]
    fn stale_detection_reports_changed_added_and_missing() {
        let d = TempDir::new("stale");
        d.write("a.md", "# A\n\n一\n");
        d.write("b.md", "# B\n\n二\n");
        let cfg = KbConfig::default();
        index(&d, &cfg);
        let e = entry_for(&d.0);

        let fresh = quick_stale(&d.store(), &e, &cfg);
        assert!(fresh.checked);
        assert!(!fresh.is_stale(), "{fresh:?}");
        assert!(fresh.summary().contains("一致"), "{}", fresh.summary());

        d.write("c.md", "# C\n\n新文件\n"); // 新增
        d.rm("b.md"); // 删除
        d.write("a.md", "# A\n\n一改过内容\n"); // 内容改（mtime/size 都变）
        let stale = quick_stale(&d.store(), &e, &cfg);
        assert_eq!(stale.added, 1, "{stale:?}");
        assert_eq!(stale.missing, 1, "{stale:?}");
        assert!(stale.changed >= 1, "mtime+size 变了就该报：{stale:?}");
        assert!(stale.is_stale());
        assert!(stale.summary().contains("已陈旧"), "{}", stale.summary());
        assert!(
            stale.note.contains("mtime"),
            "必须写明这是快速扫描：{}",
            stale.note
        );
        assert!(!stale.detail.is_empty());
    }

    #[test]
    fn never_indexed_source_says_so_instead_of_claiming_fresh() {
        let d = TempDir::new("never");
        d.write("a.md", "# A\n");
        let e = entry_for(&d.0);
        let v = quick_stale(&d.store(), &e, &KbConfig::default());
        assert!(!v.checked);
        assert!(!v.is_stale());
        assert!(v.summary().contains("还没有索引"), "{}", v.summary());
    }

    #[test]
    fn cancelled_index_keeps_what_it_already_wrote() {
        let d = TempDir::new("cancel");
        for i in 0..5 {
            d.write(&format!("f{i}.md"), "# t\n\n内容\n");
        }
        let cfg = KbConfig::default();
        let flag = new_cancel_flag();
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
        let e = entry_for(&d.0);
        let st = index_source(&d.store(), &e, &cfg, &flag, &mut |_p| {}).unwrap();
        assert!(st.cancelled);
        assert_eq!(st.added, 0, "取消后不该写任何文档");
        // 取消态下不许执行"删除消失文件"的清理（否则会把整库清空）
        assert_eq!(st.removed, 0);
    }

    #[test]
    fn empty_files_are_registered_so_staleness_does_not_cry_wolf() {
        // 空文件如果被"跳过"，陈旧检测每次都会把它算成"新增" —— 一个永远亮着的
        // 假警报，比没有警报更糟。所以空文件照样登记（只是 0 个 chunk）。
        let d = TempDir::new("empty");
        d.write("a.md", "# A\n\n有内容\n");
        d.write("b.md", "");
        let cfg = KbConfig::default();
        let st = index(&d, &cfg);
        assert_eq!(st.added, 2, "{st:?}");
        let e = entry_for(&d.0);
        let v = quick_stale(&d.store(), &e, &cfg);
        assert_eq!(v.added, 0, "空文件不该被反复报成新增：{v:?}");
        assert!(!v.is_stale(), "{v:?}");
        // 但它搜不到任何东西（没有 chunk 可命中）
        let out = search(&d.store(), &e.id, "内容", 10).unwrap();
        assert!(out.hits.iter().all(|h| h.path != "b.md"), "{:?}", out.hits);
    }
}
