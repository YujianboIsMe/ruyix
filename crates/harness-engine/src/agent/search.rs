//! 分层检索（v1.5「五服」）：`read` 的第二种形状。
//!
//! **它不是第五个原语**：不写盘、不改状态、结果由资源当前状态决定 ⇒ 它是**读**。
//! 需求与形状见 `doc/v1.5/需求-分层搜索-五服-v1.5.md`；这里只记三条硬规矩与实现取舍。
//!
//! ## 三条硬规矩
//!
//! 1. **一次一层**：结果块只含一个 `scope` 的条目，逐条标来源与时间。层与层的新鲜度、
//!    权威级别不同（磁盘上现在写的是什么 vs 三个月前记过什么），混在一起模型分不清 ——
//!    要跨层比较是**它**的事，引擎不替它合并。
//! 2. **权威序 文件 > 记忆**：记忆类各层的结果都带来源标注，冲突**并列**给模型。
//!    引擎**不裁决、不覆盖**（需求 §4）。
//! 3. **准入门**：五层没有一层允许"任选路径" —— 文件层过 `safe_rel_path`，记忆两层按
//!    记忆库的 scope 取，会话层只看本 run 的侧存。这是准入，不是敏感词黑名单
//!    （黑名单永远漏，准入不做就进不来）。第 5 服（知识库）**不是**操作系统文件：
//!    全盘搜索没有 owner（准入只能退化成黑名单），也给不出可判定的版本。
//!
//! ## 写盘 = 零
//!
//! 本模块**没有任何写路径**：不碰 `apply_write`、不碰写入白名单、不碰暂存与备份。
//! 唯一的写是侧存自身的**校验计数**（`Capsule` 的内部状态）。判据在
//! `agent/tests/search.rs::search_never_writes_anything` 里钉着。
//!
//! ## 版本与去重
//!
//! 四层的"变了没有"看各自的存储（见 [`stamp_version`]）：文件层看仓库版本、记忆两层看
//! 记忆库的**事件序号**（WAL 下主库文件 mtime 不可靠）、会话层看侧存索引文件。拿不到
//! 版本 ⇒ `unknown` ⇒ **每次都真执行**（fail-safe：宁可多跑一次，也不许拿旧结果冒充新的）。

use super::*;
use crate::exec::clip;
use crate::mem;
use std::path::PathBuf;

/// 一次检索最多几条（与需求 §5 的表一致）
pub(crate) const MAX_HITS: usize = 50;
/// 结果块最多多少字节（超了截断并**说明**截住了）
const MAX_BYTES: usize = 6000;
/// 单条命中裁剪到多少字符
const HIT_CLIP: usize = 200;
/// 单个文件最多读多少字节去找。超过就**跳过并记一行**（二进制/巨型文件不是"里面没有"，
/// 是"没搜" —— 这两件事在模型眼里必须可分）
const FILE_BYTES_CAP: u64 = 512 * 1024;

/// 默认不搜的目录：生成物、依赖、VCS 内部。不跳过的话，检索一个 `target/` 就把预算烧光。
/// **名单会写进结果**：`没搜过` 必须让模型看得见，不能让它把"没搜到"读成"没有"。
///
/// 刻意**不含** IDE 自己的状态目录名：那种字面量只许在宿主的 `paths.rs` 里出现
/// （v1.0.0 P1 纪律，宿主有一条跨仓扫描的判据在守它），而 v1.0.0 之后项目目录里
/// 本来也不再有任何 IDE 状态（正是"零残留"那一条）。
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
];

#[allow(dead_code)]
fn mtime_ms(md: &std::fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn ts_note(secs: i64) -> String {
    chrono::DateTime::from_timestamp(secs, 0)
        .map(|d| {
            d.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| format!("ts={secs}"))
}

/// 这一层的**版本盖章**（`precheck` 取完快照后调用）。
///
/// 为什么不能像 `read` 那样"把资源当路径 stat"：
/// - 记忆库是 **WAL** 模式，小写入落 `mem.db-wal`，主库文件的 `(mtime,size)` **不动** ⇒
///   拿它当版本会**假新鲜**（库里变了而账本说没变 ⇒ 拿旧结果冒充新的）。所以用**事件序号**
///   做指纹（序号变 ⇒ 指纹变 ⇒ 旧记录失效；WAL 与它无关）。
/// - 侧存索引是普通追加文件 ⇒ `(mtime,size)` 可靠。
/// - 文件层走仓库版本（`snapshot` 里 `Tool::Search` 也取 `repo`）；不是仓库 ⇒ `unknown`。
///
/// 拿不到 ⇒ `unknown = true` ⇒ **每次都真执行**。
pub(crate) fn stamp_version(v: &mut ledger::VersionVec, spec: &SearchSpec, ctx: &Ctx<'_>) {
    match spec.scope {
        SearchScope::Files => { /* 仓库版本已经在 `snapshot` 里取过了 */ }
        // 会话层：正文来自**本会话的留痕**（本 run + 前面几个 run，新 → 旧）。
        // 版本 = 这一组留痕的 (目录名, 字节数, mtime) 组合指纹 —— 本 run 的那份还在长 ⇒ 版本必变
        // ⇒ 下一次重跑（与 v1.5 会话层"自失效"那条同向：拿旧结果冒充新的更坏）。
        SearchScope::Session => {
            let dirs = session_dirs(ctx);
            if dirs.is_empty() {
                v.unknown = true;
            } else {
                v.set_state(
                    "search:session",
                    ledger::Ver::Overlay(context::digest(&transcript::version_of(&dirs))),
                );
            }
        }
        SearchScope::ProjectMem | SearchScope::GlobalMem => match mem::current() {
            Some(m) => match mem::head_seq(m) {
                Ok(seq) => v.set_state(
                    "search:mem",
                    ledger::Ver::Overlay(context::digest(&seq.to_string())),
                ),
                Err(_) => v.unknown = true,
            },
            None => v.unknown = true,
        },
    }
}

/// 一次分层检索。`Err` = **这一层查不了**（理由必须具体：哪条准入、库在不在）。
pub(crate) fn run(ctx: &mut Ctx<'_>, spec: &SearchSpec) -> Result<String, String> {
    let limit = spec.max_hits.unwrap_or(MAX_HITS).clamp(1, MAX_HITS);
    match spec.scope {
        SearchScope::Files => files(ctx, spec, limit),
        SearchScope::Session => session(ctx, spec, limit),
        SearchScope::ProjectMem => memory(spec, limit, false),
        SearchScope::GlobalMem => memory(spec, limit, true),
    }
}

// ---------------------------------------------------------------- 第 4 服：项目文件

fn files(ctx: &Ctx<'_>, spec: &SearchSpec, limit: usize) -> Result<String, String> {
    // 准入（唯一的入口检查）：限定子树必须过 jail —— 绝对路径 / `..` / `.git` 全拒
    let sub = match spec.path.as_deref() {
        Some(p) => Some(safe_rel_path(p)?),
        None => None,
    };
    let base = ctx.project_root().to_path_buf();
    let root = match &sub {
        Some(s) if !s.is_empty() && s != "." => base.join(s),
        _ => base.clone(),
    };
    if !root.is_dir() {
        return Err(format!(
            "read 的 search 带了个 path，但它不是目录（{}）—— 限定子树要给目录，不限定就别给",
            root.display()
        ));
    }

    let needle = spec.q.to_lowercase();
    let mut hits: Vec<String> = Vec::new();
    let mut not_searched: Vec<String> = Vec::new();
    let mut bytes = 0usize;
    let mut truncated = false;
    let mut stack: Vec<PathBuf> = vec![root.clone()];

    'walk: while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        entries.sort(); // 稳定顺序：同一棵树搜两次结果逐条一致（账本与判据都靠它）
        for p in entries {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let Ok(md) = std::fs::metadata(&p) else {
                continue;
            };
            let rel = p
                .strip_prefix(&base)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            if md.is_dir() {
                if SKIP_DIRS.contains(&name) {
                    continue;
                }
                stack.push(p);
                continue;
            }
            if !md.is_file() {
                continue;
            }
            if md.len() > FILE_BYTES_CAP {
                not_searched.push(format!("{rel}（{} KB，超大）", md.len() / 1024));
                continue;
            }
            let Ok(raw) = std::fs::read(&p) else { continue };
            if raw.iter().take(1024).any(|b| *b == 0) {
                continue; // 二进制：静默跳过（不是"里面没有"，但它本来就不是给字面检索的）
            }
            let Ok(text) = String::from_utf8(raw) else {
                not_searched.push(format!("{rel}（非 UTF-8）"));
                continue;
            };
            for (i, line) in text.lines().enumerate() {
                if !line.to_lowercase().contains(&needle) {
                    continue;
                }
                let hit = format!("{rel}:{}: {}", i + 1, clip(line.trim(), HIT_CLIP));
                bytes += hit.len() + 1;
                hits.push(hit);
                if hits.len() >= limit || bytes >= MAX_BYTES {
                    truncated = true;
                    break 'walk;
                }
            }
        }
    }

    let where_ = sub.clone().unwrap_or_else(|| ".".to_string());
    let mut out = format!(
        "**项目文件**（根 `{where_}`，词 `{}`，字面匹配、大小写不敏感）：命中 {} 条。\n",
        spec.q,
        hits.len()
    );
    if hits.is_empty() {
        out.push_str("（这个范围内没有出现这个词。）\n");
    } else {
        for h in &hits {
            out.push_str(&format!("- {h}\n"));
        }
    }
    if truncated {
        out.push_str(&format!(
            "⚠ 结果**截断**（上限 {limit} 条 / {MAX_BYTES} 字节）—— 缩小 path、或换个更具体的词再搜。\n"
        ));
    }
    out.push_str(&format!(
        "已跳过目录：{}（生成物与依赖，不是项目源码）。\n",
        SKIP_DIRS.join(", ")
    ));
    if !not_searched.is_empty() {
        out.push_str(&format!(
            "**没搜**的文件（不是\"没有\"）：{}\n",
            not_searched.join("、")
        ));
    }
    out.push_str("（权威序：这是**磁盘上现在写的**，比记忆层的任何说法都硬。）\n");
    Ok(out)
}

// ---------------------------------------------------------------- 第 1 服：会话内

/// 会话层一次最多搜几个 run 的留痕（本 run 也算在内）。
///
/// 8 是拍的：会话的"最近几轮"通常够用，再往前的边际价值低，而留痕是按内容扫的（有成本）。
const SESSION_MAX_RUNS: usize = 8;

/// 本会话要搜的留痕目录：**本 run 在最前**，然后是前面几个 run（新 → 旧），最多 [`SESSION_MAX_RUNS`] 份。
///
/// 新 → 旧 是刻意的：越近的话越可能有用。用户 2026-10-09 的场景（"第 1 个 run 与第 3 个 run
/// 关于某字段有没有冲突"）两个都要看到，顺序只决定先说哪个。
fn session_dirs(ctx: &Ctx<'_>) -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    if let Some(t) = ctx.progress().transcript() {
        out.push(("本 run".to_string(), t.dir().to_path_buf()));
    }
    {
        let root = ctx.state_root();
        for id in ctx
            .session_runs
            .iter()
            .take(SESSION_MAX_RUNS.saturating_sub(out.len()))
        {
            let d = root.join("ctx").join(id);
            if d.join(transcript::FILE).is_file() {
                out.push((format!("run {id}"), d));
            }
        }
    }
    out
}

/// 第 1 服：**本会话的留痕**（提问 / 每一次工具调用与结果 / 回答），本 run + 前面几个 run。
///
/// 为什么不是 capsule（原先的形状）：capsule 只留"被折掉 / 可复用"的那部分结果、且一 run 一目录
/// ⇒ 用户那个场景（"我一个会话里第 1 个 run 与第 3 个 run 关于【user 表 password 字段长度】
/// 有没有冲突"）**一个来源都搜不到** —— 提问与回答压根没进过 capsule，工具调用只留了被折掉的那点。
/// 留痕（`transcript.jsonl`）才是这一层该读的正文。
fn session(ctx: &mut Ctx<'_>, spec: &SearchSpec, limit: usize) -> Result<String, String> {
    let dirs = session_dirs(ctx);
    if dirs.is_empty() {
        return Err(
            "会话层检索不了：这一 run 没有留痕（没建起来），也没有可搜的历史 run。这一层本次没有读数。"
                .into(),
        );
    }
    let mut hits: Vec<(String, transcript::Hit)> = Vec::new();
    for (label, d) in &dirs {
        if hits.len() >= limit {
            break;
        }
        for h in transcript::search_in(d, &spec.q, limit - hits.len()) {
            hits.push((label.clone(), h));
        }
    }
    let mut out = format!(
        "**会话内**（本会话的留痕：提问 / 工具调用与结果 / 回答；词 `{}`）：命中 {} 条，查了 {} 份留痕。\n",
        spec.q,
        hits.len(),
        dirs.len()
    );
    out.push_str(
        "（这是**过去说过的话**，权威级别最低 —— 与磁盘不一致时以磁盘为准。还在你上下文里的\
         内容你直接看得到，不必搜：检索是给**离开上下文的东西**用的。）\n",
    );
    for (label, h) in &hits {
        out.push_str(&format!(
            "- {} · {} · 留痕第 {} 行: {}\n",
            label,
            h.label,
            h.line,
            clip(&h.text, HIT_CLIP)
        ));
    }
    if hits.is_empty() {
        out.push_str(
            "（没有命中：这些留痕里没有出现过这个词 —— **没搜到不等于没发生过**，换个更具体的词再问一次。）\n",
        );
    }
    Ok(out)
}

// ---------------------------------------------------------------- 第 2/3 服：记忆

fn memory(spec: &SearchSpec, limit: usize, global: bool) -> Result<String, String> {
    let Some(m) = mem::current() else {
        return Err(
            "记忆库没安装 / 没打开（宿主启动时 `Memory::open` 失败或没装）—— 这一层本次没有读数。\
             这不是\"没有记忆\"，是\"记忆库不可用\"。"
                .into(),
        );
    };
    let scope = if global {
        mem::GLOBAL_SCOPE.to_string()
    } else {
        mem::scope()
    };
    let hits = mem::retrieve::now(m, &scope, &spec.q, limit, None)?;
    let legs = if mem::embed::is_available() {
        "词法 + 向量".to_string()
    } else {
        format!("词法（向量腿不可用：{}）", mem::embed::unavailable_reason())
    };

    let title = if global {
        "全局记忆"
    } else {
        "项目记忆"
    };
    let mut out = format!(
        "**{title}**（scope `{scope}`，词 `{}`，腿 = {legs}）：命中 {} 条。\n",
        spec.q,
        hits.len()
    );
    out.push_str(if global {
        "（跨项目事实 —— 命中可能来自**别的项目**；每条都带来源，别把它当本项目现状。）\n"
    } else {
        "（记忆是「我曾经相信」；磁盘上的文件是「现在是什么」。两者不一致时以文件为准，\
          并且**把冲突说出来**，不要替用户裁决。）\n"
    });
    for h in &hits {
        let prov = if h.prov.is_empty() {
            "无来源".to_string()
        } else {
            h.prov.join(" / ")
        };
        out.push_str(&format!(
            "- `{}` = {}（记于 {}，来源 {}，状态 {}{}）\n",
            h.key,
            clip(h.value.trim(), HIT_CLIP),
            ts_note(h.updated_ts),
            clip(&prov, HIT_CLIP),
            h.status,
            if h.contested() {
                "，⚠ 有争议"
            } else {
                ""
            }
        ));
    }
    if hits.is_empty() {
        let live = mem::retrieve::now(m, &scope, "", 1000, None)
            .map(|v| v.len())
            .unwrap_or(0);
        out.push_str(&format!(
            "（没有命中：这一层当前有 {live} 条活着的信念。换个说法可能有用 —— 但别把\"没搜到\"\
             当成\"没有这件事\"。）\n"
        ));
    }
    Ok(out)
}
