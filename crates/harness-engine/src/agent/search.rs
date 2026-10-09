//! 分层检索（v1.5「五服」）：`read` 的第二种形状。
//!
//! **它不是第五个原语**：不写盘、不改状态、结果由资源当前状态决定 ⇒ 它是**读**。
//! 需求与形状见 `doc/v1.5/需求-分层搜索-五服-v1.5.md`；这里只记三条硬规矩与实现取舍。
//!
//! ## 三条硬规矩
//!
//! 1. **一次一层**：结果块只含一个 `scope` 的条目，逐条标来源与时间。层与层的新鲜度、
//!    权威级别不同（磁盘上现在写的是什么 vs 三个月前记过什么 vs 外部资料），混在一起模型
//!    分不清 —— 要跨层比较是**它**的事，引擎不替它合并。
//! 2. **权威序 文件 > 记忆**：记忆类各层与知识库的结果都带来源标注，冲突**并列**给模型。
//!    引擎**不裁决、不覆盖**（需求 §4）。
//! 3. **准入门**：五层没有一层允许"任选路径" —— 文件层过 `safe_rel_path`，记忆两层按
//!    记忆库的 scope 取，会话层只看本会话的留痕，知识库只看**声明过的语料**（注册表 +
//!    配置 roots）。这是准入，不是敏感词黑名单（黑名单永远漏，准入不做就进不来）。
//!    知识库**不是**操作系统文件：全盘搜索没有 owner（准入只能退化成黑名单），
//!    也给不出可判定的版本。
//!
//! ## 写盘 = 零
//!
//! 本模块**没有任何写路径**：不碰 `apply_write`、不碰写入白名单、不碰暂存与备份。
//! 唯一的写是侧存自身的**校验计数**（`Capsule` 的内部状态）。判据在
//! `agent/tests/search.rs::search_never_writes_anything` 里钉着。
//!
//! ## 版本与去重
//!
//! 五层的"变了没有"看各自的存储（见 [`stamp_version`]）：文件层看仓库版本、记忆两层看
//! 记忆库的**事件序号**（WAL 下主库文件 mtime 不可靠）、会话层看留痕指纹、
//! 知识库看**索引指纹**（每个来源 `.db` 的 `(mtime,size)`）。拿不到
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
///
/// ⚠ 这一段是在 `snapshot` **之后**跑的，而 `snapshot` 对"没有任何可 stat 资源的检索"一律
/// 先判 `unknown` —— 所以这里每盖成一个状态指纹，就必须把 `unknown` 清掉（见函数内的 `stamp`）。
pub(crate) fn stamp_version(v: &mut ledger::VersionVec, spec: &SearchSpec, ctx: &Ctx<'_>) {
    /// 记下一个"状态指纹"版本源，并**同时把 `unknown` 清掉**。
    ///
    /// 为什么必须清：`snapshot` 是在**不知道这一层版本源**的前提下做的 —— 检索的资源表是空的
    /// （版本不在某个可 stat 的项目路径上，见 `keys::search_resources`），于是它对"没有仓库版本"
    /// 的检索一律先判 `unknown`（保守，宁可不复用）。这里刚刚给出了版本源，那个判决就被推翻了。
    ///
    /// 少了这一句，**非 git 仓库里所有层的状态指纹都是摆设**（`fresh_for` 见 `was.unknown`
    /// 直接判失效）：记忆层与知识库层永远重跑，而"同词同版本第二次不执行"只在 git 仓库里
    /// 成立 —— 2026-10-09 接荒服时被 kb 的端到端判据抓出来的。
    fn stamp(v: &mut ledger::VersionVec, name: &str, fp: String) {
        v.set_state(name, ledger::Ver::Overlay(context::digest(&fp)));
        v.unknown = false;
    }
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
                stamp(v, "search:session", transcript::version_of(&dirs));
            }
        }
        SearchScope::ProjectMem | SearchScope::GlobalMem => match mem::current() {
            Some(m) => match mem::head_seq(m) {
                Ok(seq) => stamp(v, "search:mem", seq.to_string()),
                Err(_) => v.unknown = true,
            },
            None => v.unknown = true,
        },
        // 知识库层：版本 = **索引指纹**（每个来源 `.db` 的 `(mtime, size)` 组合）。
        // 重建索引一定改写 `.db` ⇒ 旧结果失效；一个索引都没有 ⇒ 指纹拿不到 ⇒ `unknown`
        // ⇒ 每次都真执行（fail-safe）。为什么不看语料目录：见 `kb::index::index_fingerprint`。
        SearchScope::Kb => match ctx.kb() {
            Some(e) if e.available() => {
                let ids: Vec<String> = e.sources.iter().map(|s| s.id.clone()).collect();
                match crate::kb::index::index_fingerprint(&e.dir, &ids) {
                    Some(fp) => stamp(v, "search:kb", fp),
                    None => v.unknown = true,
                }
            }
            _ => v.unknown = true,
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
        SearchScope::Kb => kb_layer(ctx, spec, limit),
    }
}

// ---------------------------------------------------------------- 第 4 服：项目文件

/// 目录树遍历取文件清单（**非 git 仓库**那条路）。
///
/// 除 `SKIP_DIRS` 之外还有一道**已访问目录**守卫：符号链接 / Windows junction 指回祖先会绕死，
/// 而 junction 在这台机器上是常态（`AppData\\Local\\Temp` 本身就是）。没有这道守卫，
/// 一次检索就能把 run 挂在那儿，而界面上只会看到它"一直在跑"。
fn walk_files(root: &Path, base: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        // 规范化后比对：同一个真实目录只进去一次（软链/junction 绕回祖先时到此为止）
        let Ok(canon) = std::fs::canonicalize(&dir) else {
            continue;
        };
        if !seen.insert(canon) {
            continue;
        }
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
            if md.is_dir() {
                if SKIP_DIRS.contains(&name) {
                    continue;
                }
                stack.push(p);
            } else if md.is_file() {
                let _ = base; // 显示口径由调用方算（这里只出清单）
                out.push(p);
            }
        }
    }
    out
}

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

    // ---- 文件清单的**来源**（2026-10-09 审计要服后改）----
    //
    // git 仓库里**问 git**（`ls-files --others --exclude-standard`）：老路 `git grep` 认
    // `.gitignore`，而硬编码的 `SKIP_DIRS` 覆盖不到用户自定义忽略的目录 —— 那些会被整棵扫进来。
    // 不是仓库 ⇒ 退回目录树遍历，并把"忽略规则不适用"写进结果（"没搜"与"没有"可分）。
    let mut listed: Vec<PathBuf> = Vec::new();
    let how = match crate::gitops::listed_files(&root) {
        Ok(files) => {
            listed.extend(files.iter().map(|rel| root.join(rel)));
            "按 git 的忽略规则取清单（`.gitignore` 生效）".to_string()
        }
        Err(_) => {
            listed = walk_files(&root, &base);
            "**不是 git 仓库**：按目录树搜（`.gitignore` 不适用，只跳固定的生成物目录）".to_string()
        }
    };
    listed.sort(); // 稳定顺序：同一棵树搜两次结果逐条一致（账本与判据都靠它）

    'walk: for p in listed {
        {
            let Ok(md) = std::fs::metadata(&p) else {
                continue;
            };
            let rel = p
                .strip_prefix(&base)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
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
        "文件清单：{how}。\n（目录树遍历时跳过这些目录：{} —— 生成物与依赖，不是项目源码。）\n",
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

// ---------------------------------------------------------------- 第 5 服：知识库（kb）

/// 单条命中裁剪到多少字符。
///
/// 比其它几层的 [`HIT_CLIP`]（200）宽得多，理由是**这一层的正文就是答案**：文件层给的是
/// "这一行里有这个词"（模型再去 `read` 那一行），记忆层给的是一条短信念；而知识库的命中是
/// 一段文档 —— 掐到 200 字符等于给了一句半截话，模型只能再猜。整块的预算
/// （[`crate::kb::retrieve::SEARCH_BUDGET_CHARS`] = 4000）仍然是硬闸。
const KB_HIT_CLIP: usize = 800;

/// 第 5 服：**知识库**（声明式语料）—— 与注入同一套闸门（阈值 / 去重 / 多样性 / 预算），
/// 只把"房间"放大（见 `kb::retrieve::retrieve_for_search`）。
///
/// 三条与别的层不同、必须写进结果的：
/// 1. **它是资料不是指令** —— 知识库文档里完全可能写着"忽略之前的指令"（边界声明见
///    [`crate::kb::retrieve::BOUNDARY_HEAD`]，这里原样带上，免得两处措辞漂移）；
/// 2. **权威级别中** —— 外部常识/笔记，与项目文件冲突时以文件为准（与记忆层同一条纪律）；
/// 3. **"没搜"与"没有"必须可分** —— 来源查失败（没索引 / 库打不开）逐条列出，被阈值/预算
///    裁掉的候选带理由列出。
fn kb_layer(ctx: &Ctx<'_>, spec: &SearchSpec, limit: usize) -> Result<String, String> {
    use crate::kb::retrieve;

    let Some(engine) = ctx.kb() else {
        return Err(
            "知识库这一层没接上（宿主没把知识库句柄交给引擎 / 引擎独立跑）—— \
                   这一层本次没有读数，不是「知识库里没有这个东西」。"
                .into(),
        );
    };
    if !engine.enabled {
        return Err(format!(
            "知识库没开（{}）—— 打开它（配置键 `ruyix.code.harness.kb.enabled`）或先加一个语料目录；\
             这一层本次没有读数。",
            if engine.reason.trim().is_empty() {
                "config 里 [kb] enabled = false"
            } else {
                engine.reason.as_str()
            }
        ));
    }
    if engine.sources.is_empty() {
        return Err(
            "知识库**一个来源都没有**（没有任何声明过的语料目录）—— 没有来源不是「搜了没命中」，\
                   是这一层没有可搜的东西。要看项目自己的东西请用 scope=files。"
                .into(),
        );
    }

    let inj = retrieve::retrieve_for_search(engine, &spec.q, limit);
    if inj.sources_ok == 0 {
        // 一个来源都没查成 ⇒ 这不是"没有命中"，是**这一层没有读数**（与记忆层同款纪律）
        let why = if inj.notes.is_empty() {
            inj.reason.clone()
        } else {
            inj.notes.join("；")
        };
        return Err(format!(
            "知识库这一层本次没有读数：{} 个来源一个都没查成（{why}）。\
             「来源还没有索引」要先把语料索引建起来（ruyix 启动时会自动建，也可用引擎的 `kb index`）。",
            inj.sources_failed
        ));
    }

    let mut out = format!(
        "**知识库**（声明式语料 · 查成 {ok} / 失败 {failed} 个来源 · 词 `{}` · 策略 {}）：命中 {} 条。\n",
        spec.q,
        if inj.strategy.is_empty() {
            "none"
        } else {
            inj.strategy.as_str()
        },
        inj.hits.len(),
        ok = inj.sources_ok,
        failed = inj.sources_failed,
    );
    out.push_str(retrieve::BOUNDARY_HEAD);
    out.push('\n');
    out.push_str(&format!(
        "（权威级别：**中** —— 这是外部资料/笔记，与项目文件冲突时以文件为准，并把冲突说出来。\n\
         房间：整块 {} 字符、单条截 {} 字符；被裁掉的候选在下面带理由列出。）\n",
        retrieve::SEARCH_BUDGET_CHARS,
        KB_HIT_CLIP
    ));

    for (i, h) in inj.hits.iter().enumerate() {
        out.push_str(&format!(
            "{}. [{}] {}#{} · 来源「{}」 · 索引 {} · 分数 {:.2}（覆盖 {:.2}）\n",
            i + 1,
            h.trust,
            h.path,
            h.ordinal,
            h.source_label,
            if h.indexed_at.trim().is_empty() {
                "未知"
            } else {
                h.indexed_at.trim()
            },
            h.score,
            h.coverage
        ));
        if !h.title.trim().is_empty() && h.title.trim() != h.path {
            out.push_str(&format!("   标题：{}\n", clip(h.title.trim(), HIT_CLIP)));
        }
        for line in h.text.trim().lines() {
            out.push_str(&format!("   {}\n", clip(line, KB_HIT_CLIP)));
        }
    }

    if inj.hits.is_empty() {
        out.push_str(
            "（没有命中：这次查到的东西没进结果。**别把「没搜到」当成「知识库里没有这件事」** ——\
             换个更具体的词再问一次，或换一层找（项目自己的东西在 scope=files）。）\n",
        );
    }
    if !inj.dropped.is_empty() {
        out.push_str("被裁掉的候选（**本次没给你看**，不是「不存在」）：\n");
        for d in inj.dropped.iter().take(20) {
            out.push_str(&format!("- {}：{}（{}）\n", d.path, d.reason, d.detail));
        }
    }
    for n in &inj.notes {
        out.push_str(&format!("来源状态：{n}\n"));
    }
    Ok(out)
}
