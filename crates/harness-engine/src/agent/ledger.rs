//! 去重账本 `ContextLedger`（v1.2 P2 · LSC 迁移）
//!
//! 见 `doc/v1.2/架构-上下文账本与重基线调度-v1.2.md` §3/§4/§5。一句话口径：
//!
//! > **纯工具 + 资源版本未变 ⇒ 同一次调用不许执行第二次**；
//! > 命中时不执行，但必须把**上次的真实结果**还回去、**标注**它，并记 trace。
//!
//! 三件套（对应上游 `src/lsc/ledger.py` 的三条机制，逐条可测）：
//!
//! | 机制 | 做法 | 边界 |
//! |---|---|---|
//! | **规范化** | 路径归一（`./a` → `a`、分隔符统一、Windows 折大小写）、省缺省参数（`limit=None` ≡ 不传）、命令去掉首尾空白与 `2>nul` 这类无副作用重定向 | 只做**语义等价**的变换；模棱两可 ⇒ **不归一**（宁可多执行一次） |
//! | **包含关系** | 整文件读 ⊇ 该文件的任何区间读（同一版本）；`grep` 的包含键是 `(pattern, path)` —— 与上游同形 | 只有**版本一致**时才允许"用整份顶替片段"；覆盖它的候选里取**跨度最大**的那个。**跨路径作用域的包含（全仓 grep ⊇ 子目录 grep）刻意不做**：上游也没有，而我们的黄金测试要求**决策逐条对齐** —— 把判据改宽一档，"对齐"这条硬标准就作废了（收益见于"先全仓再缩范围"那种少见的顺序） |
//! | **版本有效期** | 文件/目录 `(mtime, size)`、暂存内容哈希、仓库 `git HEAD + 工作树脏否`，外加本 run 的**写入代次** | 纯工具白名单内的调用才可去重；白名单外**一律执行**；版本拿不到 ⇒ **执行**（fail-safe） |
//!
//! ## 两条不许动的纪律
//!
//! 1. **宁可多执行一次，也不许"看起来一样就跳过"** —— 去重是性能优化，不是正确性优化；
//!    方向反了会**静默给出错答案**（最难查的一类）。
//! 2. **命中必须让模型知道**：返回上次结果 + 一行标注。不标注的话模型会怀疑"是不是没执行成功"
//!    从而再试一次 —— 那正是这个机制要消灭的循环。
//!
//! ## 与"侧存"的分工
//!
//! P2 的**结果侧存在内存里**（有界，`cap_bytes`，挤掉的条目等于没记过 ⇒ 下次真执行，fail-safe）。
//! P3 把它换成 `<便携根>/projects/<键>/ctx/<run-id>/` 的 capsule 侧存 + sha256 校验之后，
//! 这里不再有上限（"历史不删"是记忆层的公理）。

use super::capsule::{Capsule, Ref};
use super::*;
use std::collections::BTreeMap;
use std::path::Path;

// ============================================================================
// 版本向量
// ============================================================================

/// 一个资源的版本标签。
///
/// 刻意的取舍：**不改 sha256** —— 每次调用去哈希整个文件是纯开销，而 `(mtime, size)`
/// 变了一定变（反过来"同秒同大小覆写"会漏，但那种改写只有我们自己会做，而我们的写入
/// 走 [`Ver::Overlay`] 与 [`Ver::Dir`] 里的写入代次，照样能发现）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ver {
    /// 磁盘文件：`(mtime_ms, size)`
    File(u64, u64),
    /// 本会话写过（暂存或已落盘）：内容哈希（**写入也要能让旧条失效**）
    Overlay(u64),
    /// 目录：自身 mtime + 本 run 的写入代次（子树里加文件不会改父目录 mtime，所以补一代次）
    Dir(u64, u64),
    /// 文件/目录不存在 —— 不参与去重（下次它可能就出现了）
    Missing,
    /// 资源没被世界版本跟踪（glob / 查询串这类）：恒为同一版
    Untracked,
}

/// 一次调用的资源版本向量。**拿不到版本 ⇒ `unknown = true` ⇒ 永不去重**（fail-safe）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VersionVec {
    /// 仓库级：`git HEAD + 工作树脏否`（`None` = 不是仓库 / 拿不到）
    pub repo: Option<(String, bool)>,
    /// 路径级：规范化路径 → 版本
    pub paths: Vec<(String, Ver)>,
    /// 任一资源版本不可判 ⇒ 整条不可复用
    pub unknown: bool,
}

impl VersionVec {
    pub fn get(&self, path: &str) -> Option<Ver> {
        self.paths.iter().find(|(p, _)| p == path).map(|(_, v)| *v)
    }

    /// 给**非文件资源**盖章（v1.5 检索）：版本不一定是"某个路径的 stat" ——
    /// 记忆库在 WAL 模式下主库文件 mtime 不可靠，所以用"事件序号哈希"这类**状态指纹**。
    /// 语义与路径版一致：`fresh_for` 只比相等，所以只要指纹能在该变的时候变，判定就是对的。
    pub(crate) fn set_state(&mut self, name: &str, v: Ver) {
        self.set(name, v);
    }

    fn set(&mut self, path: &str, v: Ver) {
        match self.paths.iter_mut().find(|(p, _)| p == path) {
            Some(slot) => slot.1 = v,
            None => self.paths.push((path.to_string(), v)),
        }
    }

    /// 一条记录的版本**是否仍然有效**：它跟踪过的每一个资源都要和现在一致，
    /// 且现在这条快照本身可判（`unknown` ⇒ 不新鲜）。
    ///
    /// `Ver::Missing` **不特殊化**：`Missing == Missing` 算新鲜（"文件不存在"这个答案，
    /// 在文件仍然不存在时没变）；文件一旦出现，版本就变成 `File(..)` ⇒ 自动失效。
    fn fresh_for(&self, was: &VersionVec) -> bool {
        if self.unknown || was.unknown {
            return false;
        }
        for (p, v) in &was.paths {
            match (v, self.get(p)) {
                (Ver::Untracked, _) => {}
                (_, Some(now)) => {
                    if now != *v {
                        return false;
                    }
                }
                // 旧记录跟踪了这个资源，这次的快照里却没有它 ⇒ 保守判失效
                _ => return false,
            }
        }
        if was.repo.is_some() && self.repo != was.repo {
            return false;
        }
        true
    }
}

/// 取一次调用的版本快照（**唯一的 IO 点**）。
///
/// - 文件 / 目录：`stat` 的 `(mtime_ms, size)`；
/// - 在 `overlay` 里的路径（本会话写过）：内容哈希；
/// - `write_gen` = 本 run 的写入代次（`ctx.changes.len()`），目录读与它绑定；
/// - 仓库级版本只对 `execute` 取（要真调 git，`read` 不需要）；
/// - 资源一个都认不出来（且没有仓库版本）⇒ `unknown = true`（**宁可执行**）。
pub fn snapshot(
    proj: &Path,
    call: &LedgerCall,
    overlay: &BTreeMap<String, String>,
    write_gen: usize,
    repo: &Option<(String, bool)>,
) -> VersionVec {
    let mut v = VersionVec {
        repo: None,
        paths: Vec::new(),
        unknown: false,
    };
    for res in &call.resources {
        if let Some(content) = overlay.get(res) {
            v.set(res, Ver::Overlay(context::digest(content)));
            continue;
        }
        let p = proj.join(res);
        let Ok(md) = std::fs::metadata(&p) else {
            v.set(res, Ver::Missing);
            continue;
        };
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        if md.is_dir() {
            v.set(res, Ver::Dir(mtime, write_gen as u64));
        } else {
            v.set(res, Ver::File(mtime, md.len()));
        }
    }
    // 仓库级版本：`execute`（命令的答案依赖整个工作树）与**检索**（文件层的答案同样依赖它）。
    // 非 git 仓库 ⇒ `repo = None` ⇒ 下面判 `unknown` ⇒ 永不命中（fail-safe：宁可多跑一次）。
    if matches!(call.tool, Tool::Execute | Tool::Search) {
        v.repo = repo.clone();
    }
    if v.paths.is_empty() && v.repo.is_none() {
        // 没有可判版本的资源：不许去重（fail-safe）
        v.unknown = true;
    }
    v
}

// ============================================================================
// 账本
// ============================================================================

/// 上次那条结果正文放在哪儿。
///
/// - `Inline`：P2 的形态（内存里有界，超限整条挤掉）；
/// - `Stored`：P3 的形态（capsule 侧存，账本只留引用 —— 于是 §5 那条内存上限自然退场）。
///
/// 两种形态的**命中语义完全一样**（都要"逐字节还回去"），区别只在正文住在哪儿。
#[derive(Clone, Debug)]
pub enum Body {
    Inline(String),
    Stored(Ref),
}

impl Body {
    /// 占内存的字节数（`Stored` 是 0 —— 正文不在内存里，这一条决定了 P3 之后上限退场）
    pub fn mem_len(&self) -> usize {
        match self {
            Body::Inline(s) => s.len(),
            Body::Stored(_) => 0,
        }
    }

    /// 取回正文：`Inline` 直接给；`Stored` 走 capsule 读回（**sha256 校验**，不符就报错）
    pub fn text(&self, capsule: Option<&mut Capsule>) -> Result<String, String> {
        match self {
            Body::Inline(s) => Ok(s.clone()),
            Body::Stored(r) => match capsule {
                Some(c) => c.get(r),
                None => Err(format!(
                    "这条结果存在 capsule 里（{}），但本次 run 没有挂侧存 —— 取不回正文",
                    r.file
                )),
            },
        }
    }
}

/// 一条账本记录（只增不改）。`body` = 上次的真结果（P2 在内存，P3 在 capsule 侧存）。
#[derive(Clone, Debug)]
pub struct LedgerEntry {
    pub tool: Tool,
    pub norm: String,
    pub digest: u64,
    pub span: Option<SpanKey>,
    pub versions: VersionVec,
    /// 第一次执行它的轮次（标注行要写"第 N 轮"）
    pub step: u32,
    pub body: Body,
    pub bytes: usize,
    /// 被复用了几次（trace / 小结用）
    pub hits: u32,
}

/// 版本向量的 JSON 形态（写进 capsule 索引，供人翻账：这条结果是什么时候、基于哪一版）
pub fn versions_json(v: &VersionVec) -> String {
    let paths: BTreeMap<&str, String> = v
        .paths
        .iter()
        .map(|(p, ver)| (p.as_str(), format!("{ver:?}")))
        .collect();
    serde_json::json!({ "repo": v.repo, "paths": paths, "unknown": v.unknown }).to_string()
}

/// 一次召回的结果：正文 + 它是第几轮执行的 + 正文是不是从 capsule 读回来的。
#[derive(Clone, Debug)]
pub struct Recall {
    pub text: String,
    pub step: u32,
    pub from_capsule: bool,
}

/// 一次执行的分类（**仪器**那一半：与策略无关，两臂都能量）。
///
/// 语义与上游 `src/lsc/engine.py` 的计数逐条对应：
/// `Unique`↔`unique_exec` · `Redundant`↔`redundant_exec`（**纯浪费**，LSC 要把它打到 0）·
/// `Stale`↔`stale_exec`（版本变了，**必要**的重执行）· `Effectful`↔"每次执行都是新动作"。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecKind {
    Effectful,
    Unique,
    Redundant,
    Stale,
}

/// 执行审计（上游 `executed: digest -> versions` 那一张表）。
#[derive(Clone, Debug, Default)]
pub struct ExecAudit {
    seen: BTreeMap<u64, VersionVec>,
    /// 已执行过的**调用包含键**（工具 + 跨度 + 版本）。
    ///
    /// 为什么它不能靠账本的 entries：**对照臂根本不往账本里记**（`dedup` 关着就没人记
    /// 结果，也没有理由记）—— 那样审计就问不出"这次执行账本会不会拦下它"，
    /// 而那个反事实正是"重复执行次数下降"的**基线读数**。这张表只存键与版本（不含结果），
    /// 几十 KB 级别，且与 `dedup` 开关无关。
    subs: Vec<(Tool, SpanKey, VersionVec)>,
    pub unique: u64,
    pub redundant: u64,
    pub stale: u64,
    pub effectful: u64,
}

impl ExecAudit {
    /// 审计一次**执行**（A/B 口径）：**如果账本被查过，这次执行会不会被拦下**。
    ///
    /// 会 ⇒ 这次执行就是**浪费**：包含"同 digest 重复"与**被既有记录覆盖**
    /// （模型先整份读、再读一个窗口 —— digest 不同但内容已被覆盖）两类。
    ///
    /// 与 [`ExecAudit::classify`] 的区别：后者的 digest 口径是**上游**的（黄金测试对的就是它），
    /// 数不出包含关系省下的那些。处理臂里这条路径对纯调用走不到（都被拦在 precheck 了），
    /// 所以两臂读数同源可比。
    pub fn classify_exec(
        &mut self,
        call: &LedgerCall,
        versions: &VersionVec,
        pure: bool,
    ) -> ExecKind {
        if !pure {
            self.effectful += 1;
            return ExecKind::Effectful;
        }
        let covered = call.span.as_ref().is_some_and(|k| {
            self.subs
                .iter()
                .any(|(t, k0, v0)| t == &call.tool && k0.covers(k) && v0 == versions)
        });
        if let Some(k) = call.span.clone() {
            self.subs.push((call.tool, k, versions.clone()));
        }
        if covered {
            self.seen.insert(call.digest, versions.clone());
            self.redundant += 1;
            return ExecKind::Redundant;
        }
        self.classify(call.digest, versions, true)
    }
    /// 记一次**真的执行了**的调用并分类（`pure = false` ⇒ 不纯动作，直接算一次新动作）。
    pub fn classify(&mut self, digest: u64, versions: &VersionVec, pure: bool) -> ExecKind {
        if !pure {
            self.effectful += 1;
            return ExecKind::Effectful;
        }
        let kind = match self.seen.get(&digest) {
            Some(was) if was == versions => {
                self.redundant += 1;
                ExecKind::Redundant
            }
            Some(_) => {
                self.stale += 1;
                ExecKind::Stale
            }
            None => {
                self.unique += 1;
                ExecKind::Unique
            }
        };
        self.seen.insert(digest, versions.clone());
        kind
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub entries: usize,
    pub bytes: usize,
    /// 被拦下的重复执行（真省下来的那些）
    pub blocked: u64,
    pub unique: u64,
    pub redundant: u64,
    pub stale: u64,
    pub effectful: u64,
}

/// 去重账本：`by_digest`（精确命中）+ `order`（包含关系候选，按记录顺序）。
///
/// 不再派生 `Clone`：挂了 capsule 之后它持有侧存句柄（有计数与 IO 语义），
/// "复制一份账本"在这个语义下没有意义。
#[derive(Debug)]
pub struct ContextLedger {
    entries: Vec<LedgerEntry>,
    by_digest: BTreeMap<u64, usize>,
    /// 结果侧存上限（字节）。挤掉最老的 ⇒ 那条等于没记过（**fail-safe：下次真执行**）。
    /// **只对 `Body::Inline` 生效**：挂了 capsule 之后正文不驻留内存，这条上限自然退场。
    cap_bytes: usize,
    bytes: usize,
    pub audit: ExecAudit,
    blocked: u64,
    /// 结果侧存（`agent.ctx.capsule` 开时才挂）
    capsule: Option<Capsule>,
    /// 侧存写失败次数（写不进去就退化成内存，并且要如实报出来）
    capsule_errors: u64,
    /// 召回时读不回来 / sha256 不符的次数（**必须为 0**；非 0 说明侧存坏了，不许静默）
    recall_errors: u64,
}

impl ContextLedger {
    pub fn new(cap_bytes: usize) -> Self {
        Self {
            entries: Vec::new(),
            by_digest: BTreeMap::new(),
            cap_bytes,
            bytes: 0,
            audit: ExecAudit::default(),
            blocked: 0,
            capsule: None,
            capsule_errors: 0,
            recall_errors: 0,
        }
    }

    /// 挂上 capsule 侧存（P3）。挂上之后新记的结果走 `Body::Stored`。
    pub fn attach_capsule(&mut self, c: Capsule) {
        self.capsule = Some(c);
    }

    pub fn capsule(&self) -> Option<&Capsule> {
        self.capsule.as_ref()
    }

    pub fn capsule_mut(&mut self) -> Option<&mut Capsule> {
        self.capsule.as_mut()
    }

    /// 侧存写失败（调用方会退化为内存，这里只记账）
    pub fn note_capsule_error(&mut self) {
        self.capsule_errors += 1;
    }

    /// 查一条可复用的记录。命中条件（两件都要）：① 规范化键一致（或**包含关系**覆盖）
    /// ② 它跟踪过的资源版本**都没变**。任一不满足 ⇒ `None`（调用方照常执行）。
    pub fn lookup(&self, call: &LedgerCall, now: &VersionVec) -> Option<&LedgerEntry> {
        if now.unknown {
            return None;
        }
        if let Some(&i) = self.by_digest.get(&call.digest)
            && let Some(e) = self.entries.get(i)
            && now.fresh_for(&e.versions)
        {
            return Some(e);
        }
        // 包含关系：候选里取**跨度最大**且新鲜的那个
        let key = call.span.as_ref()?;
        let mut best: Option<&LedgerEntry> = None;
        for e in &self.entries {
            if e.tool != call.tool {
                continue;
            }
            let Some(es) = &e.span else { continue };
            if !es.covers(key) || !now.fresh_for(&e.versions) {
                continue;
            }
            if best
                .is_none_or(|b| es.span() > b.span.as_ref().map(|s| s.span()).unwrap_or(isize::MIN))
            {
                best = Some(e);
            }
        }
        best
    }

    /// 记一条**真的执行了**的调用（结果侧存 + 版本向量）。命中（被拦下）的那些**不记** ——
    /// 与上游一致：账本只记"执行过什么"，复用不产生新记录。
    pub fn record(&mut self, call: LedgerCall, body: Body, versions: VersionVec, step: u32) {
        // 版本不可判的**不许入账**：留一条"永远不新鲜"的记录只占内存
        if versions.unknown {
            return;
        }
        let bytes = body.mem_len();
        let e = LedgerEntry {
            tool: call.tool,
            norm: call.norm.clone(),
            digest: call.digest,
            span: call.span.clone(),
            versions,
            step,
            body,
            bytes,
            hits: 0,
        };
        self.bytes += bytes;
        self.entries.push(e);
        self.by_digest.insert(call.digest, self.entries.len() - 1);
        self.evict_if_needed();
    }

    /// 结果侧存有界：挤掉最老的（**同时从索引里摘掉** —— 留着索引却没有结果，
    /// 就会在命中时给不出内容，那比"没记过"更坏）。
    fn evict_if_needed(&mut self) {
        if self.cap_bytes == 0 {
            return;
        }
        let mut dropped = 0usize;
        while self.bytes > self.cap_bytes && dropped < self.entries.len() {
            let b = self.entries[dropped].bytes;
            self.bytes = self.bytes.saturating_sub(b);
            dropped += 1;
        }
        if dropped == 0 {
            return;
        }
        self.entries.drain(..dropped);
        self.by_digest.clear();
        for (i, e) in self.entries.iter().enumerate() {
            self.by_digest.insert(e.digest, i);
        }
    }

    /// 审计一次**执行**（本仓库口径，用于 A/B 读数）：**如果账本被查过，这次执行会不会被拦下**。
    ///
    /// 会 ⇒ 这次执行就是**浪费**（`仍重复执行`），包含"同 digest 重复"与**被既有记录覆盖**
    /// （模型先整份读、再读一个窗口 —— digest 不同但内容已被覆盖）两类。
    ///
    /// 为什么不用 `classify` 的 digest 口径：那是**上游**的口径（黄金测试对的就是它），
    /// 它数不出包含关系省下的那些；而 A/B 要量的正是"对照臂白跑了多少次"。
    /// 处理臂里这条路径对纯调用根本走不到（都被拦在 precheck 了）⇒ 两臂读数同源可比。
    pub fn audit_exec(&mut self, call: &LedgerCall, versions: &VersionVec, pure: bool) -> ExecKind {
        self.audit.classify_exec(call, versions, pure)
    }

    /// 审计一次**不纯**的执行（每次都是新动作，永远算"唯一"）
    pub fn audit_effectful(&mut self) -> ExecKind {
        self.audit.classify(0, &VersionVec::default(), false)
    }

    /// **召回**：命中就取回正文（一次调用把"查键 + 取正文 + 计数"做完）。
    ///
    /// 为什么单独开一条而不是让调用方 `lookup` 完再自己取：取正文可能是 IO（capsule），
    /// 而"命中计数"必须与"取到了正文"绑在一起 —— 只计数却没取到，读数就是假的。
    /// capsule 读失败 ⇒ 返回 `None`（**fail-safe：调用方照常执行**），并把失败计进 `recall_errors`。
    pub fn recall(&mut self, call: &LedgerCall, now: &VersionVec) -> Option<Recall> {
        let (i, from_capsule) = {
            let e = self.lookup(call, now)?;
            let i = self.entries.iter().position(|x| std::ptr::eq(x, e))?;
            (i, matches!(e.body, Body::Stored(_)))
        };
        let text = match self.entries[i].body.text(self.capsule.as_mut()) {
            Ok(t) => t,
            Err(_) => {
                self.recall_errors += 1;
                return None;
            }
        };
        self.blocked += 1;
        self.entries[i].hits += 1;
        Some(Recall {
            text,
            step: self.entries[i].step,
            from_capsule,
        })
    }

    /// 侧存统计（落盘条数 / 召回次数 / 校验失败次数）+ 写失败次数
    pub fn capsule_stats(&self) -> Option<(usize, usize, usize, u64, usize)> {
        self.capsule.as_ref().map(|c| {
            let (puts, recalls, corrupted) = c.stats();
            (
                puts,
                recalls,
                corrupted,
                self.capsule_errors,
                c.disk_bytes(),
            )
        })
    }

    /// 这次 run 有没有召回过（P3 的读数）
    pub fn recalled(&self) -> u64 {
        self.entries
            .iter()
            .filter(|e| matches!(e.body, Body::Stored(_)))
            .map(|e| e.hits as u64)
            .sum()
    }

    /// 记一次"命中"（账本没执行，但我们要知道省了几次 —— 也是 P2 的验收读数）。
    pub fn note_hit_for(&mut self, call: &LedgerCall, now: &VersionVec) {
        self.blocked += 1;
        if let Some(i) = self.lookup_index(call, now)
            && let Some(e) = self.entries.get_mut(i)
        {
            e.hits += 1;
        }
    }

    /// 命中记录在 `entries` 里的下标（`entries` 会被挤掉最老的，所以**每轮现算**，
    /// 不跨轮保存下标 —— 存了就会指向错的那条）。
    fn lookup_index(&self, call: &LedgerCall, now: &VersionVec) -> Option<usize> {
        let hit = self.lookup(call, now)?;
        self.entries.iter().position(|e| std::ptr::eq(e, hit))
    }

    /// 改结果侧存上限（字节）。**0 = 不限**。改完立刻按新上限挤一次。
    pub fn set_cap(&mut self, cap: usize) {
        self.cap_bytes = cap;
        self.evict_if_needed();
    }

    pub fn stats(&self) -> Stats {
        Stats {
            entries: self.entries.len(),
            bytes: self.bytes,
            blocked: self.blocked,
            unique: self.audit.unique,
            redundant: self.audit.redundant,
            stale: self.audit.stale,
            effectful: self.audit.effectful,
        }
    }

    /// 一行小结（进 trace；也就是 P2 的验收读数）
    pub fn render_stats(&self) -> String {
        let kb = |n: usize| format!("{:.1}KB", n as f64 / 1024.0);
        let s = self.stats();
        // 侧存形态下 `s.bytes` 是**内存**里的字节（恒为 0）—— 直接报它，用户会看到
        // 「侧存 28 条/0.0KB」以为存了个空（真报障，2026-09-27）。所以：挂了 capsule 就报
        // **磁盘**占用，而且两个形态都**标明**是哪一个 —— 数字一样、含义不同，不标是另一种误导。
        let stored = match self.capsule_stats() {
            Some((_, _, _, _, disk)) => format!("{} 条/{}（磁盘）", s.entries, kb(disk)),
            None => format!("{} 条/{}（内存）", s.entries, kb(s.bytes)),
        };
        let line = format!(
            "去重账本：拦截重复 {} · 唯一执行 {} · 仍重复执行 {} · 版本失效重执行 {} · 不纯执行 {} · 侧存 {}",
            s.blocked, s.unique, s.redundant, s.stale, s.effectful, stored
        );
        if let Some((puts, recalls, corrupted, werrs, _)) = self.capsule_stats() {
            return format!(
                "{line}（capsule 落盘 {puts} 条 · 召回 {recalls} 次 · 校验失败 {corrupted}                  · 写失败 {werrs}；召回即磁盘读，**不重跑工具**）"
            );
        }
        line
    }
}

// ============================================================================
// 预检（唯一的决策点：**执行之前**问账本）
// ============================================================================

/// 预检结论：这条动作该不该执行、命中时复用哪一段文本。
#[derive(Clone, Debug)]
pub struct Plan {
    pub call: LedgerCall,
    /// 执行**前**取的版本快照。执行之后再取就晚了 —— 执行本身可能改文件（写、构建）。
    pub versions: VersionVec,
    /// 命中 ⇒ 不执行，直接把这段文本还回去（含标注行）
    pub reuse: Option<Reuse>,
    /// 账上怎么称呼它（规范化后的形状，进 trace）
    pub brief: String,
    pub tool: &'static str,
}

#[derive(Clone, Debug)]
pub struct Reuse {
    pub text: String,
    /// 上一次执行它是第几轮（标注行要写"第 N 轮"）
    pub step: u32,
    /// 正文是从 capsule 侧存读回来的（trace 要标出来 —— "召回不重跑"这条判据的可见证据）
    pub from_capsule: bool,
}

/// 仓库级版本（`git HEAD + 工作树脏否`）。不是仓库 / 拿不到 ⇒ `None`
/// （那么只靠路径版本；路径也认不出 ⇒ 整条不可判 ⇒ 老实执行）。
pub fn repo_version(proj: &Path) -> Option<(String, bool)> {
    let head = crate::gitops::head(proj).ok()?;
    let clean = crate::gitops::is_clean(proj).unwrap_or(false);
    Some((head, clean))
}

/// 执行**之前**的预检：逐条动作给出 [`Plan`]（`None` = 这条不参与去重）。
///
/// 这是唯一"要不要跳过执行"的决策点。它只改账本的命中计数，**不碰任何别的状态** ——
/// 波次调度、并发、结果回灌都照旧，所以批里那几条并发读不会被这个机制改形状。
///
/// `consult = false`：只建计划**不问账本**（`agent.ctx.dedup` 关着）。这条路径留给
/// **仪器**（审计）用 —— 对照臂也要能量出"重复执行了几次"，否则"下降 ≥50%"没有基线。
pub(super) fn precheck(
    ctx: &mut Ctx<'_>,
    actions: &[Action],
    repo: &Option<(String, bool)>,
    consult: bool,
) -> Vec<Option<Plan>> {
    actions
        .iter()
        .map(|a| {
            let call = classify(a)?;
            let mut versions = snapshot(
                ctx.project_root(),
                &call,
                &ctx.overlay,
                ctx.changes().len(),
                repo,
            );
            // 分层检索：各层的"变了没有"看的是**各自的存储**（侧存索引 / 记忆库事件序号），
            // 不是项目里的某个路径 —— 盖章在这儿做（这里是唯一能同时看到 `ctx` 与快照的地方）。
            if let Action::Search(spec) = a {
                search::stamp_version(&mut versions, spec, ctx);
            }
            let brief = call.norm.clone();
            let tool = call.tool.name();
            let reuse = if consult {
                // 一次调用做完"查键 + 取正文 + 计数"：侧存形态要读盘（可能读不回来），
                // 取不到正文就**不算命中**（照常执行）—— 计数与取正文必须绑在一起。
                ctx.progress_mut()
                    .dedup_recall(&call, &versions)
                    .map(|r| Reuse {
                        text: r.text,
                        step: r.step,
                        from_capsule: r.from_capsule,
                    })
            } else {
                None
            };
            Some(Plan {
                call,
                versions,
                reuse,
                brief,
                tool,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
