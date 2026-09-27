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
// 调用键：规范化 + 包含关系
// ============================================================================

/// 进账本的两类纯工具。其余动作（write / 后台 / 句柄 / connect / ask / plan / final）
/// **一律不参与**去重 —— 见 [`classify`]。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Read,
    Execute,
}

impl Tool {
    pub fn name(&self) -> &'static str {
        match self {
            Tool::Read => "read",
            Tool::Execute => "execute",
        }
    }
}

/// 包含关系用的键（`(resource, lo, hi)`；`hi == -1` = 到末尾/不设上界）。
///
/// 覆盖判定：旧的 `covers` 新的 ⇔ 同资源 ∧ 旧的下界更靠前 ∧ 旧的上界到末尾或 ≥ 新的上界。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpanKey {
    pub resource: String,
    pub lo: usize,
    /// -1 = 无上界
    pub hi: isize,
}

impl SpanKey {
    fn covers(&self, other: &SpanKey) -> bool {
        if self.resource != other.resource || self.lo > other.lo {
            return false;
        }
        if self.hi == -1 {
            return true;
        }
        other.hi != -1 && self.hi >= other.hi
    }

    /// 跨度（候选里取最大的那个）
    fn span(&self) -> isize {
        if self.hi == -1 {
            isize::MAX
        } else {
            self.hi - self.lo as isize
        }
    }
}

/// 一次调用的规范化键 + 包含关系键 + digest。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerCall {
    pub tool: Tool,
    /// 规范化后的参数（进 trace / 标注行；**不含**正文）
    pub norm: String,
    /// 包含关系键（`read` 的区间 / `grep` 的 pattern+路径）；`None` = 只认精确 digest
    pub span: Option<SpanKey>,
    /// 规范化键的哈希（同一 key ⇒ 同一 digest）
    pub digest: u64,
    /// 这次调用读到的资源（相对路径，规范化后）；版本向量按它取
    pub resources: Vec<String>,
}

impl LedgerCall {
    /// 结果尾部那行**标注**（架构文档 §4 的原话，逐字不动）。
    pub fn reuse_note(step: u32) -> String {
        format!("\n（第 {step} 轮已执行过同一纯调用，资源版本未变；本次直接复用，未重跑）")
    }

    fn new(tool: Tool, norm: String, span: Option<SpanKey>, resources: Vec<String>) -> Self {
        let mut h = format!("{}|{}", tool.name(), norm);
        if let Some(s) = &span {
            h.push_str(&format!("|S:{}:{}:{}", s.resource, s.lo, s.hi));
        }
        h.push_str(&format!("|R:{}", resources.join(",")));
        Self {
            tool,
            norm,
            span,
            digest: context::digest(&h),
            resources,
        }
    }
}

// ---------------------------------------------------------------- 规范化

/// 路径归一（**语义等价**的那几种）：
/// `\` → `/`、折叠重复分隔符、去掉前导 `./`、抹掉尾随 `/`；Windows 上再折大小写。
///
/// 不做的事：不解析 `..`（跨目录的 `..` 在路径封闭检查里有自己的口径，这里动它只会
/// 让"同一个文件"看起来是两个）、不查磁盘（这是纯函数，IO 在 [`snapshot`] 里）。
pub fn norm_path(raw: &str) -> String {
    let s = raw.trim().replace('\\', "/");
    let mut out: Vec<&str> = Vec::new();
    for seg in s.split('/') {
        match seg {
            "" | "." => {}
            other => out.push(other),
        }
    }
    let mut joined = out.join("/");
    if s.starts_with('/') {
        joined.insert(0, '/');
    }
    if joined.is_empty() {
        // `"."` / `"./"` / 全是分隔符 ⇒ 项目根（read "." 的规范化必须落在同一个键上）
        joined.push('.');
    }
    if cfg!(windows) {
        joined = joined.to_lowercase();
    }
    joined
}

/// 命令归一：折叠空白、去首尾空白、去掉 `2>nul` / `2>/dev/null` 这类**无副作用**重定向、
/// 首个 token 折小写（Windows 命令不区分大小写），并把**像路径的 token** 一起归一。
///
/// 路径那一维必须**在这里**归一（不能只归一 `resources`）：键是拿这个串算 digest 的，
/// `git diff -- ./a` 与 `git diff -- a` 若算两个键，同一次调用就会因为一个 `./` 前缀
/// 被重跑一次。这条是被**黄金测试**抓出来的（上游 `canonical_args` 会把参数里的路径
/// 归一，我们的第一版没有 ⇒ 两个 `git_diff` 事件对不上）。
/// 命令归一 + 资源抽取，**一趟走完**。
///
/// 两件事共用同一个"像路径吗"的判据，账上的资源与 digest 里的字节因此永远一致 ——
/// 分两处实现就必然在某次改动里漂移（那会静默少一次去重或多一次去重）。
fn norm_cmd_and_resources(raw: &str) -> (String, Vec<String>) {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut s = collapsed.as_str();
    for tail in [" 2>nul", " 2>NUL", " 2>/dev/null", " 2>&1"] {
        if let Some(stripped) = s.strip_suffix(tail) {
            s = stripped.trim_end();
        }
    }
    let mut out: Vec<String> = Vec::new();
    let mut resources: Vec<String> = Vec::new();
    let mut after_ddash = false;
    for (i, tok) in s.split(' ').enumerate() {
        let bare = tok.trim_matches('"').trim_matches('\'');
        if bare == "--" {
            after_ddash = true;
            out.push(tok.to_string());
            continue;
        }
        if i == 0 {
            // 命令名：折小写（Windows 不区分）—— 参数**不折**，它们的大小写有意义
            out.push(tok.to_lowercase());
            continue;
        }
        if !is_flag(bare) && (looks_like_path(bare) || after_ddash) {
            // 路径：抹掉引号外壳 + 归一（`"./a.py"` 与 `a.py` 是同一个资源）
            let p = norm_path(bare);
            if !resources.contains(&p) {
                resources.push(p.clone());
            }
            out.push(p);
            continue;
        }
        out.push(tok.to_string());
    }
    (out.join(" "), resources)
}

/// Windows 上把 `where` / `where.exe` / `C:\Windows\System32\where.exe` 认成同一个命令。
fn base_cmd(token: &str) -> String {
    let t = token.replace('\\', "/");
    let last = t.rsplit('/').next().unwrap_or(&t);
    for ext in [".exe", ".cmd", ".bat", ".com"] {
        if let Some(stripped) = last.strip_suffix(ext) {
            return stripped.to_lowercase();
        }
    }
    last.to_lowercase()
}

// ============================================================================
// 纯工具白名单（起步集，写进代码而不是提示词：模型不许自己说"我很纯"）
// ============================================================================

/// **永远纯**的命令（只读、无副作用）。git 再看子命令（[`GIT_PURE`]）。
///
/// 起步集按需求文档 §6 拍板 4：白名单外的 `execute` **一律执行**。
/// 扩展只有一处：往这张表加名字（不是加 match 分支）。
const PURE_CMDS: &[&str] = &[
    // 目录 / 文件内容
    "dir", "ls", "type", "cat", "head", "tail", // 检索
    "findstr", "where", "find", // 只读的 git（子命令白名单在下面）
    "git",
];

/// git 的只读子命令（`git grep` / `git log` / …）。
const GIT_PURE: &[&str] = &["grep", "log", "status", "diff", "show", "ls-files"];

/// 明确**不纯**、永远不进账本的命令（写盘 / 构建 / 安装 / 起服务）。
/// 这张表是"给读代码的人看的"：真正生效的判据是 `PURE_CMDS` 白名单（不在里面就不纯），
/// 所以这里漏一个也不会造成错误去重，只会少一次优化。
#[cfg(test)]
const KNOWN_EFFECTFUL: &[&str] = &[
    "cargo", "npm", "pnpm", "yarn", "mvn", "gradle", "pip", "uv", "go", "make", "cmake", "python",
    "node", "rm", "del", "copy", "move", "ren", "mkdir", "touch", "tee",
];

/// 命令里有没有"写向文件"的重定向或命令连接符（有 ⇒ 不纯）。
/// `2>nul` / `2>&1` 已经在 [`norm_cmd`] 里被剥掉，到这里还剩下的 `>` 都算写。
fn has_side_effects(cmd: &str) -> bool {
    if cmd.contains(">>") || cmd.contains("&&") || cmd.contains("||") || cmd.contains(';') {
        return true;
    }
    // `>`（单个）：剥掉过 `2>&1` 之类之后还有就当作重定向到文件
    cmd.contains('>')
}

/// 一段命令是否"纯"：每一段（按 `|` 切开）的首 token 都在白名单里，且不含重定向/连接符。
///
/// 管道**允许**（`git grep x -- src | head -20` 是很常见的形状），但每一段都得是纯命令 ——
/// 右边那半也可能写盘（`| tee f`），所以必须逐段查。
fn cmd_is_pure(cmd: &str) -> bool {
    if has_side_effects(cmd) {
        return false;
    }
    let segments: Vec<&str> = cmd.split('|').collect();
    if segments.is_empty() {
        return false;
    }
    for seg in segments {
        let seg = seg.trim();
        if seg.is_empty() {
            return false;
        }
        let mut toks = seg.split_whitespace();
        let Some(first) = toks.next() else {
            return false;
        };
        let base = base_cmd(first);
        if !PURE_CMDS.contains(&base.as_str()) {
            return false;
        }
        if base == "git" {
            let sub = toks.next().unwrap_or("").to_lowercase();
            if !GIT_PURE.contains(&sub.as_str()) {
                return false;
            }
        }
    }
    true
}

/// 这个 token 是命令行开关吗？`-n` / `--json` / `/n`(Windows 风格短开关) 都算。
///
/// 为什么要认：`/n` 里有个 `/`，不认它就会被当成路径塞进版本向量（`findstr /n x a.rs`
/// 会多出一个不存在的资源）—— 不致命，但账上的资源必须是真的资源。
fn is_flag(t: &str) -> bool {
    if t.starts_with('-') {
        return true;
    }
    let rest = t.strip_prefix('/').unwrap_or("");
    !rest.is_empty() && rest.len() <= 2 && rest.chars().all(|c| c.is_ascii_alphanumeric())
}

/// 这个 token 像"项目内的路径"吗？（含分隔符，或带已知的源码扩展名）
fn looks_like_path(t: &str) -> bool {
    t.contains('/')
        || t.contains('\\')
        || matches!(
            Path::new(t).extension().and_then(|e| e.to_str()),
            Some("rs" | "py" | "js" | "ts" | "md" | "toml" | "json" | "txt" | "css" | "html")
        )
}

/// `git grep -n <pattern> [-- <path>]` 的包含关系键。
///
/// 包含语义与上游一致：**同一 pattern、同一版本**时，`git grep pat`（全仓库/全路径）
/// 覆盖 `git grep pat -- 子路径` —— 前者的结果里已经包含后者要的那些行。
fn grep_span(cmd: &str) -> Option<SpanKey> {
    let toks: Vec<&str> = cmd.split_whitespace().collect();
    if toks.len() < 2 || base_cmd(toks[0]) != "git" || toks[1].to_lowercase() != "grep" {
        return None;
    }
    let mut pattern: Option<String> = None;
    let mut path: Option<String> = None;
    let mut i = 2;
    let mut seen_ddash = false;
    while i < toks.len() {
        let t = toks[i].trim_matches('"').trim_matches('\'');
        if t == "--" {
            seen_ddash = true;
            i += 1;
            if i < toks.len() {
                path = Some(norm_path(toks[i].trim_matches('"').trim_matches('\'')));
            }
            i += 1;
            continue;
        }
        if t.starts_with('-') {
            i += 1;
            continue;
        }
        if pattern.is_none() {
            pattern = Some(t.to_string());
        } else if seen_ddash && path.is_none() {
            path = Some(norm_path(t));
        }
        i += 1;
    }
    let pattern = pattern?;
    Some(SpanKey {
        resource: format!("grep::{}::{}", pattern, path.unwrap_or_else(|| ".".into())),
        lo: 0,
        hi: -1,
    })
}

/// `read` 在账上怎么称呼（形状与 `ReadSpec::brief` 一致，但路径用**归一后**的那个）。
///
/// 这里必须用归一后的路径：键是拿这个串算 digest 的 —— 用原串就等于
/// `read("./a")` 与 `read("a")` 是两个键，规范化白做（这条是被单测抓出来的）。
fn read_norm(path: &str, offset: Option<usize>, limit: Option<usize>) -> String {
    match (offset, limit) {
        (None, None) => path.to_string(),
        (o, l) => format!(
            "{}（{}-{}）",
            path,
            o.unwrap_or(1),
            l.map(|l| format!("+{l}"))
                .unwrap_or_else(|| "末".to_string())
        ),
    }
}

/// `read` 的包含关系键：整份读 = `(path, 0, -1)`；窗口读 = `(path, offset-1, offset-1+limit)`。
///
/// 与 ruyix 的窗口语义对齐（`ReadSpec`: 1-based 起点，`limit = None` 表示到末尾）。
fn read_span(path: &str, offset: Option<usize>, limit: Option<usize>) -> SpanKey {
    match (offset, limit) {
        (None, None) => SpanKey {
            resource: path.to_string(),
            lo: 0,
            hi: -1,
        },
        (o, l) => {
            let lo = o.unwrap_or(1).max(1) - 1;
            SpanKey {
                resource: path.to_string(),
                lo,
                hi: match l {
                    None => -1,
                    Some(l) => (lo + l) as isize,
                },
            }
        }
    }
}

/// 动作 → 账本键。`None` = 这个动作**不参与**去重（不纯 / 不是读类动作）。
pub(super) fn classify(action: &Action) -> Option<LedgerCall> {
    match action {
        Action::Read(spec) => {
            let path = norm_path(&spec.path);
            let span = read_span(&path, spec.offset, spec.limit);
            Some(LedgerCall::new(
                Tool::Read,
                read_norm(&path, spec.offset, spec.limit),
                Some(span),
                vec![path],
            ))
        }
        Action::Execute(cmd, _) => {
            let (norm, resources) = norm_cmd_and_resources(cmd);
            if !cmd_is_pure(&norm) {
                return None;
            }
            let span = grep_span(&norm);
            Some(LedgerCall::new(Tool::Execute, norm, span, resources))
        }
        // 下面这些都**不纯**或不是"读"：
        // · write 改世界；· 后台/句柄是**活的**（第二次 status 的答案本来就不同，
        //   去重它会给出错答案）；· connect 的对端是外部系统，版本向量拿不到 ⇒ fail-safe；
        // · plan / final / ask / findings 是控制动作。
        Action::Write(_)
        | Action::ExecBg(_)
        | Action::Proc(_, _)
        | Action::Connect(_)
        | Action::Plan(_)
        | Action::Final(_)
        | Action::Ask(_)
        | Action::Findings(_) => None,
    }
}

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
    if call.tool == Tool::Execute {
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
            let versions = snapshot(
                ctx.project_root(),
                &call,
                &ctx.overlay,
                ctx.changes().len(),
                repo,
            );
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
mod tests {
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
                include_str!("../../tests/fixtures/ledger_golden_seed1.json"),
            ),
            (
                "seed3",
                include_str!("../../tests/fixtures/ledger_golden_seed3.json"),
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
}
