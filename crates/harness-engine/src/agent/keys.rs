//! 调用键：**规范化 + 纯度判定 + 包含关系**（v1.2 P2 的地基）。
//!
//! 去重的第一步是回答"**这是不是同一次调用**"，本模块就干这个：
//!
//! - **规范化**（[`norm_path`] / [`norm_cmd_and_resources`]）：路径折叠 `./`、统一分隔符、
//!   Windows 折大小写；命令串折叠空白、去 `2>nul`、首 token 折小写 —— **命令里的路径一起归一**
//!   （少了这条，`git diff -- ./a` 与 `git diff -- a` 会算成两个键；黄金测试抓出来的）；
//! - **纯度白名单**（[`cmd_is_pure`] + `PURE_CMDS` / `GIT_PURE` / `KNOWN_EFFECTFUL`）：
//!   只有"读类"工具才参与去重 —— 写向文件的重定向、`&&` / `||` / `;` 串联、后台与句柄、
//!   `connect`、控制动作一律判纯失败。**宁可多执行一次，也不许"看起来一样就跳过"**；
//! - **包含关系**（[`SpanKey`]）：整文件读 ⊇ 该文件的任何区间读（`hi = -1` 表"到末尾"）；
//!   跨路径作用域的包含**刻意不做**（上游也没有，而黄金测试要求决策逐条对齐）。
//!
//! 从 `agent/ledger.rs` 拆出的（2026-09-28，那个文件当时 1613 行）。

use super::*;
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
    pub(crate) fn covers(&self, other: &SpanKey) -> bool {
        if self.resource != other.resource || self.lo > other.lo {
            return false;
        }
        if self.hi == -1 {
            return true;
        }
        other.hi != -1 && self.hi >= other.hi
    }

    /// 跨度（候选里取最大的那个）
    pub(crate) fn span(&self) -> isize {
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

    pub(crate) fn new(tool: Tool, norm: String, span: Option<SpanKey>, resources: Vec<String>) -> Self {
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
pub(crate) fn norm_cmd_and_resources(raw: &str) -> (String, Vec<String>) {
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
pub(crate) fn base_cmd(token: &str) -> String {
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
pub(crate) const PURE_CMDS: &[&str] = &[
    // 目录 / 文件内容
    "dir", "ls", "type", "cat", "head", "tail", // 检索
    "findstr", "where", "find", // 只读的 git（子命令白名单在下面）
    "git",
];

/// git 的只读子命令（`git grep` / `git log` / …）。
pub(crate) const GIT_PURE: &[&str] = &["grep", "log", "status", "diff", "show", "ls-files"];

/// 明确**不纯**、永远不进账本的命令（写盘 / 构建 / 安装 / 起服务）。
/// 这张表是"给读代码的人看的"：真正生效的判据是 `PURE_CMDS` 白名单（不在里面就不纯），
/// 所以这里漏一个也不会造成错误去重，只会少一次优化。
#[cfg(test)]
pub(crate) const KNOWN_EFFECTFUL: &[&str] = &[
    "cargo", "npm", "pnpm", "yarn", "mvn", "gradle", "pip", "uv", "go", "make", "cmake", "python",
    "node", "rm", "del", "copy", "move", "ren", "mkdir", "touch", "tee",
];

/// 命令里有没有"写向文件"的重定向或命令连接符（有 ⇒ 不纯）。
/// `2>nul` / `2>&1` 已经在 [`norm_cmd`] 里被剥掉，到这里还剩下的 `>` 都算写。
pub(crate) fn has_side_effects(cmd: &str) -> bool {
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
pub(crate) fn cmd_is_pure(cmd: &str) -> bool {
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
pub(crate) fn is_flag(t: &str) -> bool {
    if t.starts_with('-') {
        return true;
    }
    let rest = t.strip_prefix('/').unwrap_or("");
    !rest.is_empty() && rest.len() <= 2 && rest.chars().all(|c| c.is_ascii_alphanumeric())
}

/// 这个 token 像"项目内的路径"吗？（含分隔符，或带已知的源码扩展名）
pub(crate) fn looks_like_path(t: &str) -> bool {
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
pub(crate) fn grep_span(cmd: &str) -> Option<SpanKey> {
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
pub(crate) fn read_norm(path: &str, offset: Option<usize>, limit: Option<usize>) -> String {
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
pub(crate) fn read_span(path: &str, offset: Option<usize>, limit: Option<usize>) -> SpanKey {
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
