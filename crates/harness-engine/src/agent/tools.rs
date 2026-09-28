//! 工具执行：**项目目录 + 覆盖层**（路径全部封闭在项目内）。
//!
//! [`Ctx`] 是这条链的共享状态：覆盖层（本会话写过什么，Stage 模式下磁盘没动也要视图一致）、
//! 改动台账、找文件缓存、账本（去重）与上下文布局。步骤执行体（`crate::step_agent`）借的是
//! **同一份** `&mut Ctx` —— 不借同一份就得写 merge 与冲突处理，而收益只有"父能看子"。
//!
//! 四类动作的落地：read（窗口读 / 行内扫描）、write（整份或锚点替换，Stage 模式进暂存区）、
//! execute（前台 / 后台 / 句柄三种生命周期 + 执行前预检）、connect（交给宿主的 [`Connector`]）。
//! 另外还有循环需要的零件：历史折叠、停滞检测文案、计划收尾（`settle_steps`）。
//!
//! 从 `agent.rs` 拆出的（2026-09-28）。

use super::*;

// （原 `agent.rs` 的 banner：工具执行（项目目录 + 覆盖层；路径全部封闭在项目内））

// ============================================
// 工具执行（项目目录 + 覆盖层；路径全部封闭在项目内）
// ============================================

/// 覆盖层：本会话已写/改的内容。read 优先读它（模型改完能读回自己的修改），
/// Stage 策略下磁盘未动也能保持一致的视图。
///
/// 步骤执行体（`crate::step_agent`）借的是**同一份** `&mut Ctx`，不是自己的副本：
/// `emit_step_progress` / `gate_before_final` / `flush_stage` 全依赖这一份 overlay 与
/// changes，各持一份就得写 merge 与冲突处理，而收益只有"父能看到中间态"。
///
/// 它是 `pub` 只因为出现在 `step_agent::run_step` 的签名里；字段与工具方法仍然是
/// crate 内可见 —— 外部拿不到一份可用的上下文，工具循环的唯一入口是 [`run`]。
pub struct Ctx<'a> {
    pub(crate) proj: &'a Path,
    pub(crate) overlay: BTreeMap<String, String>,
    pub(crate) changes: Vec<FileChange>,
    /// 本轮**命令取证**的留痕（依据核对的证据集合之一；复核员会看到它）
    pub(crate) probes: Vec<Probe>,
    pub(crate) policy: WritePolicy,
    pub(crate) backup_dir: Option<PathBuf>,
    /// 本 run 的进展（结论 + 引擎账本 + 读取索引）。主循环与 step 子步骤**共用同一份**
    /// （子步骤借的是同一个 `&mut Ctx`），所以不需要第二套机制。
    pub(crate) progress: Progress,
    /// **项目状态根**（宿主注入；暂存与备份落这里，绝不落进项目）。
    /// `None` = 走兜底（临时目录）—— 引擎单测与"宿主没注入"的情况都走它。
    pub(crate) state_root: Option<PathBuf>,
    /// **写入白名单**（v1.4 P1，宿主注入）。空 = 不启用（老行为一字不变）。
    pub(crate) write_allow: Vec<String>,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(proj: &'a Path, policy: WritePolicy) -> Self {
        Self {
            proj,
            overlay: BTreeMap::new(),
            changes: Vec::new(),
            probes: Vec::new(),
            progress: Progress::new(),
            policy,
            backup_dir: None,
            state_root: None,
            write_allow: Vec::new(),
        }
    }

    /// 项目状态根：注入优先，否则兜底临时目录（**绝不回落进项目**）。
    /// 侧存文件（**只读例外**，v1.2 P3）：把一条**绝对路径**认成"项目桶里的文件"。
    ///
    /// 规矩两条，缺一不可：① **只读**（这里只解析路径，写盘另有闸 —— `apply_write`
    /// 仍走 `safe_rel_path`）；② 规范化之后**必须落在 `state_root` 之内**（`..` 与
    /// 符号链接都逃不出去）。项目内的绝对路径**照旧拒绝**，那条规定没被放宽。
    pub(crate) fn state_file(&self, raw: &str) -> Option<PathBuf> {
        let t = raw.trim();
        let looks_absolute =
            (t.len() >= 2 && t.as_bytes()[1] == b':') || t.starts_with('/') || t.starts_with('\\');
        if !looks_absolute {
            return None;
        }
        let root = self.state_root().canonicalize().ok()?;
        let real = Path::new(t).canonicalize().ok()?;
        if real.starts_with(&root) && real.is_file() {
            Some(real)
        } else {
            None
        }
    }

    /// 注入写入白名单（v1.4 P1）。空列表 = 不启用。
    pub(crate) fn with_write_allow(mut self, allow: Vec<String>) -> Self {
        self.write_allow = allow;
        self
    }

    /// **写入白名单闸**（v1.4 P1）：白名单非空且 `rel` 不匹配任何一条 ⇒ 拒绝。
    ///
    /// 拒绝理由必须**具体**（需求 §1.2 ②：不是提示词礼貌劝阻）：指名路径、说明它属于
    /// "白名单外"这一类、把本 run 的白名单原文列出来，并说明**磁盘与覆盖层都没动** ——
    /// 模型据此一轮就能改正，而不是接着猜。
    pub(crate) fn guard_write(&self, rel: &str) -> Result<(), String> {
        if self.write_allow.is_empty() || self.write_allow.iter().any(|p| allow_matches(p, rel)) {
            return Ok(());
        }
        Err(format!(
            "写 {rel} 被拒：它**不在本 run 的写入白名单内**（白名单 {} 条：{}）。\
             引擎在解析内容与落盘**之前**拒绝 —— 磁盘、备份、覆盖层都没有这次改动。\
             白名单外的改动一律不许：改成白名单内的路径，或者如实报告做不到。",
            self.write_allow.len(),
            self.write_allow.join("、")
        ))
    }

    pub(crate) fn state_root(&self) -> PathBuf {
        self.state_root
            .clone()
            .unwrap_or_else(|| crate::config::fallback_state_root(self.proj))
    }

    /// 宿主注入项目状态根（`<便携根>/projects/<项目 key>`）：暂存与备份都写到那儿。
    pub(crate) fn with_state_root(mut self, dir: PathBuf) -> Self {
        self.state_root = Some(dir);
        self
    }

    /// 本会话改了哪些文件（步骤执行体按"本步写过的路径"筛自己那部分）
    pub(crate) fn changes(&self) -> &[FileChange] {
        &self.changes
    }

    /// 本会话跑过哪些命令、各自输出了什么（依据核对的证据集合）
    pub(crate) fn probes(&self) -> &[Probe] {
        &self.probes
    }

    /// 留一次取证。两端都要裁剪：命令可能特长、输出可能是一份几千行的编译日志，
    /// 而它是要进复核员上下文的。
    /// 进展状态（主循环与子步骤共用）
    pub(crate) fn progress(&self) -> &Progress {
        &self.progress
    }

    pub(crate) fn progress_mut(&mut self) -> &mut Progress {
        &mut self.progress
    }

    /// 记一条结论（`record_findings` 的落点）
    pub(crate) fn record_finding(&mut self, spec: &FindingSpec) -> Result<String, String> {
        self.progress.record(
            &spec.claim,
            &spec.evidence,
            &spec.note,
            spec.supersedes.as_deref(),
        )
    }

    pub(crate) fn note_probe(&mut self, cmd: &str, output: &str) {
        self.probes.push(Probe {
            cmd: clip(cmd, PROBE_CMD_CLIP),
            output: clip(output, PROBE_OUT_CLIP),
        });
    }

    /// 项目根（execute 的工作目录）
    pub(crate) fn project_root(&self) -> &Path {
        self.proj
    }

    /// 本会话的写入策略。子步骤执行体（`crate::step_agent`）借同一份 `Ctx`，
    /// 要靠它决定要不要把"确认模式：execute 看不到你的改动"贴进自己的上下文。
    pub(crate) fn policy(&self) -> WritePolicy {
        self.policy
    }

    /// read：目录给结构树（复用 repair 的列表逻辑），文件给内容；"." = 项目根。
    ///
    /// `spec` 带 `offset`/`limit` 时只取那一段行窗口（[`ReadSpec::is_window`]）——
    /// 让模型不必为读 3000 行文件里中间那 40 行而付整份 token。
    /// 不带窗口时与老行为**逐字一致**（同一次 `clip`）。
    pub(crate) fn tool_read(&self, spec: &ReadSpec) -> Result<String, String> {
        let raw = spec.path.as_str();
        let raw_trim = raw.trim();
        if raw_trim == "." || raw_trim == "./" {
            // 项目根结构：read "." 是模型探索项目的第一步
            // （目录给的本来就是结构树，窗口参数在这里无意义 —— 忽略，不报错换一轮白跑）
            let mut listing = String::from("--- 项目根目录结构 ---\n");
            let mut n = 0usize;
            repair::list_dir(self.proj, "", 0, &mut n, &mut listing);
            return Ok(listing);
        }
        // ---- 唯一的"跳出项目根"只读例外（v1.2 P3）----
        // capsule 侧存与 findings 落盘都在**项目桶**（`<便携根>/projects/<键>/`）里，
        // 不在用户仓库内 —— 模型要能回头读自己那次被折掉的全量结果（需求 §2 G4）。
        if let Some(p) = self.state_file(raw) {
            let body = std::fs::read_to_string(&p).unwrap_or_else(|e| format!("<读取失败：{e}>"));
            return Ok(format!(
                "--- {}（侧存，只读）---
{body}",
                p.display()
            ));
        }
        let rel = safe_rel_path(raw)?;
        if let Some(c) = self.overlay.get(&rel) {
            let body = read_body(&rel, c, spec)?;
            return Ok(format!("（{rel} 的当前内容 = 本会话已暂存的修改）\n{body}"));
        }
        let p = self.proj.join(&rel);
        if p.is_dir() {
            Ok(repair::gather_context(
                self.proj,
                std::slice::from_ref(&rel),
                READ_CLIP,
            ))
        } else if p.is_file() {
            let content =
                std::fs::read_to_string(&p).unwrap_or_else(|e| format!("<读取失败：{e}>"));
            let body = read_body(&rel, &content, spec)?;
            Ok(format!("--- {rel} ---\n{body}"))
        } else {
            Err(format!(
                "{rel} 不存在（项目根目录用 read \".\" 看整体结构）"
            ))
        }
    }

    /// write 的整份形态（老形状，逐字保留）。生产路径走 [`Ctx::apply_write`]，
    /// 这条是测试与调用方直连的入口。
    #[cfg(test)]
    pub(crate) fn tool_write(&mut self, path: &str, content: &str) -> Result<String, String> {
        self.apply_write(&WriteSpec::content(path, content))
    }

    /// write 的锚点形态：只传改动，不重发全文
    #[cfg(test)]
    pub(crate) fn tool_edit(
        &mut self,
        path: &str,
        edits: Vec<AgentEdit>,
    ) -> Result<String, String> {
        self.apply_write(&WriteSpec {
            path: path.to_string(),
            body: WriteBody::Edits(edits),
        })
    }

    /// write 的**唯一入口**：两种参数形状都在 [`resolve_write`] 里归约成"最终整份内容"，
    /// 再走同一条记账/落盘通道。
    ///
    /// 为什么不直接调 [`repair::apply_edits`]：那个函数**自己写盘**，会绕过覆盖层与
    /// 备份/暂存记账 —— 确认模式下磁盘本来就不该动（那是"read 看到新内容、execute 看到旧内容"
    /// 那场 65 轮误侦察的根）。共用的只是**匹配规则**（[`repair::replace_unique`]），
    /// 落盘通道仍然只有 [`Ctx::commit_with`] 一条。
    pub(crate) fn apply_write(&mut self, spec: &WriteSpec) -> Result<String, String> {
        let rel = safe_rel_path(&spec.path)?;
        // 白名单闸：**在解析内容与落盘之前**（拒绝时磁盘与覆盖层都不动）
        self.guard_write(&rel)?;
        let before = self.before_of(&rel);
        let after = resolve_write(&rel, &spec.body, before.as_deref())?;
        let len = after.len();
        let reply = match &spec.body {
            WriteBody::Content(_) => write_ok_text(&rel, len, self.policy),
            WriteBody::Edits(edits) => write_edits_ok_text(&rel, edits.len(), len, self.policy),
        };
        self.commit_with(rel, before, after)?;
        Ok(reply)
    }

    /// 记录变更 + 更新覆盖层；Apply 策略同步落盘。
    /// `before` 由调用方给：它往往已经为了别的目的读过一遍当前内容，这里再读一次是白付一次 I/O。
    pub(crate) fn commit_with(
        &mut self,
        rel: String,
        before: Option<String>,
        after: String,
    ) -> Result<(), String> {
        if self.policy == WritePolicy::Apply {
            let bdir = self.ensure_backup_dir(before.is_some());
            flush_write_disk(self.proj, bdir.as_deref(), &rel, before.as_deref(), &after)?;
        }
        self.record(rel, before, after);
        Ok(())
    }

    /// 取被覆盖前的原内容（覆盖层优先：同 run 内的二次写以首次记录为准）。
    /// 只读 `&self` —— 所以并发波的线程里也能取。
    pub(crate) fn before_of(&self, rel: &str) -> Option<String> {
        self.overlay
            .get(rel)
            .cloned()
            .or_else(|| std::fs::read_to_string(self.proj.join(rel)).ok())
    }

    /// 备份目录（Apply 模式覆盖已有文件时才需要）。`need` 为假则返回当前值。
    /// 必须在**主线程**调（它改 `Ctx`），线程里只读 [`Ctx::backup_dir`] 的克隆。
    pub(crate) fn ensure_backup_dir(&mut self, need: bool) -> Option<PathBuf> {
        if need {
            // 备份落在**项目状态根**里（v1.0.0 起不在用户仓库里）
            let d = self
                .state_root()
                .join("backups")
                .join(format!("agent-{}", crate::workspace::now_compact()));
            let _ = std::fs::create_dir_all(&d);
            self.backup_dir.get_or_insert(d);
        }
        self.backup_dir.clone()
    }

    /// 当前备份目录的只读访问（并发波里要用它落备份，但线程不许改 `Ctx`）
    pub(crate) fn backup_dir_path(&self) -> Option<PathBuf> {
        self.backup_dir.clone()
    }

    /// 写入的**记账阶段**：只动内存（overlay / changes），磁盘已由 [`flush_write_disk`] 落过了。
    /// 所以并发波里写完磁盘后，可以在主线程按声明顺序补这一步。
    pub(crate) fn record(&mut self, rel: String, before: Option<String>, after: String) {
        if let Some(existing) = self.changes.iter_mut().find(|c| c.path == rel) {
            existing.after = after.clone();
        } else {
            self.changes.push(FileChange {
                path: rel.clone(),
                kind: if before.is_some() {
                    "modify".into()
                } else {
                    "add".into()
                },
                before,
                after: after.clone(),
            });
        }
        self.overlay.insert(rel, after);
    }

    /// Stage 策略收尾：把覆盖层写到暂存目录，返回目录路径
    pub(crate) fn flush_stage(&self) -> Result<PathBuf, String> {
        // 暂存落在**项目状态根**里（v1.0.0 起不在用户仓库里）
        let dir = self
            .state_root()
            .join("stage")
            .join(format!("agent-{}", crate::workspace::now_compact()));
        for c in &self.changes {
            let to = dir.join("files").join(&c.path);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("创建暂存目录失败：{e}"))?;
            }
            std::fs::write(&to, &c.after).map_err(|e| format!("暂存 {} 失败：{e}", c.path))?;
        }
        let manifest = serde_json::to_string_pretty(&self.changes)
            .map_err(|e| format!("序列化暂存清单失败：{e}"))?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建暂存目录失败：{e}"))?;
        std::fs::write(dir.join("manifest.json"), manifest)
            .map_err(|e| format!("写暂存清单失败：{e}"))?;
        Ok(dir)
    }
}

/// 白名单模式匹配（v1.4 P1）。语法见 `config::AgentConfig::write_allow`。
///
/// 单独成函数、单独测：白名单是**判据的一部分**（判错了就等于没有闸），
/// 所以它必须是纯函数、可枚举地测，而不是埋在路径解析里。
pub(crate) fn allow_matches(pat: &str, rel: &str) -> bool {
    let p: Vec<&str> = pat.split('/').filter(|s| !s.is_empty()).collect();
    let r: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    if p.is_empty() {
        return false;
    }
    fn go(p: &[&str], r: &[&str]) -> bool {
        match p.split_first() {
            None => r.is_empty(),
            // 整段 `**`：吃掉 0..n 段（`plugins/**` 也能匹配 `plugins` 本身）
            Some((&"**", rest)) => (0..=r.len()).any(|k| go(rest, &r[k..])),
            Some((seg, rest)) => match r.split_first() {
                Some((head, tail)) if seg_match(seg, head) => go(rest, tail),
                _ => false,
            },
        }
    }
    go(&p, &r)
}

/// 段内通配（`*` = 任意字符但不跨 `/`，`?` = 一个字符）。经典双指针回溯。
fn seg_match(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// execute 的破坏性模式拒绝清单。这是绊线不是沙箱 —— 真正的隔离在 verify 的 docker 沙箱；
/// Agent 的 execute 面向用户自己的项目目录，只拦"不可逆的系统级破坏"。
pub(crate) fn execute_allowed(cmd: &str) -> Result<(), String> {
    let c = cmd.to_ascii_lowercase();
    for pat in [
        "rm -rf /",
        "rm -rf /*",
        "rm -rf ~",
        "mkfs",
        "shutdown",
        "reboot",
        "del /s /q",
        "rd /s /q",
        "format c:",
    ] {
        if c.contains(pat) {
            return Err(format!(
                "命令被拒绝（破坏性模式 {pat:?}）。execute 的作用域是当前项目，不允许不可逆的系统级操作。"
            ));
        }
    }
    Ok(())
}

/// shell **内置**命令：`where` / `command -v` 查不到，但绝不是"命令不存在"。
/// 表而不是分支 —— 与 `discover::TOOLS` 同一哲学：无界的那一类靠数据长，不靠加 if。
pub(crate) const SHELL_BUILTINS: &[&str] = &[
    // cmd.exe
    "cd", "chdir", "dir", "echo", "set", "type", "copy", "move", "del", "erase", "md", "mkdir",
    "rd", "rmdir", "cls", "title", "ver", "date", "time", "exit", "call", "shift", "if", "for",
    "goto", "pause", "pushd", "popd", "assoc", "ftype", "color", "prompt", "rem",
    // sh / bash
    "export", "source", "test", "true", "false", "pwd", "unset", "alias", "read", "umask", "local",
    "return", "printf", "ulimit", "trap", "exec", "eval", "wait", "kill", "jobs", "bg", "fg", ".",
];

/// "把执行交给外壳"的启动器：**输出脱离捕获**（stdout / stderr 一律拿不到），
/// 真控制台里还会把"找不到"变成桌面弹窗。
pub(crate) const LAUNCHERS: &[&str] = &["start", "explorer", "rundll32", "open", "xdg-open"];

/// 命令文本里出现这些就拒：PowerShell 的外壳转发同样脱离捕获。
pub(crate) const SHELL_ESCAPES: &[&str] = &["start-process", "invoke-item"];

/// execute 的**执行前闸门**：把"确定是垃圾"的命令挡在 shell 之外，并回一条可读的纠正。
///
/// 为什么要有：execute 是模型唯一的写入口，一次瞎试就是一轮。实测 run
/// `agent-20260920-152312` 里模型为"mvn / java 到底存不存在"空转 5~6 轮；而 `\`、
/// `\admin-run\` 这类**根本不是命令**的字符串也会被原样交给 cmd —— 模型只看到一行它
/// 读不懂的报错（GBK 乱码，见 [`exec::decode_output`]），于是换个更离谱的继续试。
///
/// **保守原则**：只拦三类，拿不准一律放行。闸门误杀一条合法命令的代价，
/// 远大于放过去一条垃圾命令。
///
/// 与 discover 的关系：同一张工具表，两种用法 —— 探测结果既喂上下文
/// （[`discover::render_note`]），也在这里当**验证器**。
pub(crate) fn preflight_execute(proj: &Path, cmd: &str) -> Result<(), String> {
    // ① 启动器 / 外壳转发：必然拿不到输出，真控制台还会弹窗
    if let Some(l) = launcher_of(cmd) {
        return Err(format!(
            "❌ 这条命令没有执行：`{l}` 会把执行交给外壳 —— stdout / stderr 一律拿不到，\
             出错还会在桌面上弹窗。请直接运行程序本身（例：`mvn -v`、`java -jar app.jar`）；\
             确实需要新窗口时，把这条命令写进最终答复让用户手动跑。"
        ));
    }
    let Some(tok) = first_token(cmd) else {
        return Err("❌ 空命令。".into());
    };
    // ② 路径式写法，但本机与项目里都没有这个文件
    if path_like(&tok) {
        if path_exists(proj, &tok) {
            return Ok(());
        }
        return Err(format!(
            "❌ 这条命令没有执行：`{tok}` 是路径写法，但本机与项目里都没有这个文件。\n\
             项目根：{}\n\
             要跑程序就写命令名（例：`mvn -v`）；要跑项目里的脚本就写**存在的**相对路径。\n{}",
            proj.display(),
            available_hint(proj)
        ));
    }
    // ③ 本机没有这个命令（先排除 shell 内置与项目内脚本）
    if !SHELL_BUILTINS.contains(&tok.as_str())
        && !path_exists(proj, &tok)
        && !discover::is_available(&tok)
    {
        return Err(format!(
            "❌ 这条命令没有执行：本机没有 `{tok}`。别猜工具名，先看这份实测清单。\n{}",
            available_hint(proj)
        ));
    }
    Ok(())
}

/// 极简命令行切分：按空白切，引号内的空白不算分隔符。
/// 够闸门用（不做转义与变量展开）—— 但**必须**认引号，否则
/// `"C:\Program Files\Git\bin\bash.exe"` 会被切成两半而误判。
pub(crate) fn tokens(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut q: Option<char> = None;
    for ch in cmd.chars() {
        match q {
            Some(qc) => {
                if ch == qc {
                    q = None;
                } else {
                    cur.push(ch);
                }
            }
            None if ch == '"' || ch == '\'' => q = Some(ch),
            None if ch.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            None => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// 首个**有意义的** token：跳过前置环境赋值（`FOO=bar prog`）与纯操作符（`& prog`）。
pub(crate) fn first_token(cmd: &str) -> Option<String> {
    tokens(cmd).into_iter().find(|t| {
        !matches!(t.as_str(), "&" | "&&" | "|" | "||" | "(" | ")")
            && !(t.contains('=') && !t.starts_with('-') && !path_like(t))
    })
}

/// 带路径分隔符 = 模型在写路径（而不是命令名）
pub(crate) fn path_like(tok: &str) -> bool {
    tok.contains('\\') || tok.contains('/')
}

/// 这个路径（项目相对 / 绝对）真的指向一个可执行文件吗。
/// 覆盖三件事：绝对路径、项目内相对路径、以及项目内 `gradlew` / `mvnw`
/// 这类**不带扩展名**、靠 `.cmd` / `.bat` 落地的包装脚本。
pub(crate) fn path_exists(proj: &Path, tok: &str) -> bool {
    let p = Path::new(tok);
    if p.file_name().is_none() {
        return false; // 纯分隔符 / 盘根：不是可执行的东西
    }
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        proj.join(p)
    };
    if abs.is_file() {
        return true;
    }
    if p.extension().is_none() {
        for ext in [".cmd", ".bat", ".exe", ".ps1", ".sh"] {
            if proj.join(format!("{tok}{ext}")).is_file() {
                return true;
            }
        }
    }
    false
}

/// 首 token（含 `cmd /C start …` 这种包一层的情况）是启动器就返回它。
pub(crate) fn launcher_of(cmd: &str) -> Option<String> {
    let low = cmd.to_ascii_lowercase();
    if let Some(e) = SHELL_ESCAPES.iter().find(|e| low.contains(**e)) {
        return Some((*e).to_string());
    }
    let toks = tokens(cmd);
    let mut i = 0;
    if toks.len() >= 2
        && matches!(
            toks[0].to_ascii_lowercase().as_str(),
            "cmd" | "cmd.exe" | "sh" | "bash" | "bash.exe"
        )
        && matches!(toks[1].to_ascii_lowercase().as_str(), "/c" | "-c")
    {
        i = 2;
    }
    let t = toks.get(i)?;
    let base = t.rsplit(['\\', '/']).next().unwrap_or(t.as_str());
    let name = base.to_ascii_lowercase();
    let name = name
        .strip_suffix(".exe")
        .unwrap_or(name.as_str())
        .to_string();
    LAUNCHERS.contains(&name.as_str()).then_some(name)
}

/// 拒绝命令时附上"那有什么" —— 光说"这条不存在"治不了空转。
pub(crate) fn available_hint(proj: &Path) -> String {
    let names = discover::available_names(proj);
    if names.is_empty() {
        return "（本机没探到可用命令；先看看上下文里的『本机命令』段）".to_string();
    }
    format!("本机可用：{}", names.join(" / "))
}

pub(crate) fn run_shell(proj: &Path, cmd: &str, timeout: Duration) -> exec::CmdOutput {
    #[cfg(target_os = "windows")]
    return exec::run(proj, "cmd", &["/C", cmd], timeout, &[]);
    #[cfg(not(target_os = "windows"))]
    return exec::run(proj, "sh", &["-c", cmd], timeout, &[]);
}

pub(crate) fn tool_execute(proj: &Path, cmd: &str, timeout_secs: Option<u64>) -> String {
    // 闸门在破坏性模式之前：先判"这条命令有没有意义"，再判"它危不危险"。
    if let Err(e) = preflight_execute(proj, cmd) {
        return e;
    }
    if let Err(e) = execute_allowed(cmd) {
        return format!("❌ {e}");
    }
    let t = Duration::from_secs(
        timeout_secs
            .unwrap_or(EXEC_DEFAULT_TIMEOUT_SECS)
            .clamp(EXEC_MIN_TIMEOUT_SECS, EXEC_MAX_TIMEOUT_SECS),
    );
    let out = run_shell(proj, cmd, t);
    let mut s = format!(
        "exit={} 耗时 {}ms{}",
        out.exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".into()),
        out.duration_ms,
        if out.timed_out {
            "（超时被杀）"
        } else {
            ""
        }
    );
    if let Some(e) = &out.spawn_error {
        s.push_str(&format!("\n启动失败: {e}"));
    }
    if !out.stdout.trim().is_empty() {
        s.push_str(&format!("\nstdout:\n{}", clip(&out.stdout, EXEC_CLIP)));
    }
    if !out.stderr.trim().is_empty() {
        s.push_str(&format!("\nstderr:\n{}", clip(&out.stderr, EXEC_CLIP)));
    }
    s
}

/// 把一次后台启动的结果渲染给模型。**每个出口都带证据**：判据命中的那行 / 退出码 +
/// 日志尾 + 判据最后结果 / 判据未命中 + 日志尾。
///
/// 另外两件必须说清的事：① 日志在哪、后续怎么操作（不然模型还得猜）；
/// ② **如实记账** —— 同一就绪判据已经有别的进程在跑，就直说。实测那 17 轮空转里最毒的一环
/// 就是"上一轮的进程还占着端口，于是每次重启拿到的都是假信号"。引擎不该猜模型的意图，
/// 但把事实摆出来，模型自己就会先 stop 再起。
pub(crate) fn render_start(proj: &Path, out: &crate::proc::StartOutcome) -> String {
    use crate::proc::StartKind;
    let i = &out.info;
    let secs = out.waited_ms as f64 / 1000.0;
    let (mut s, tail) = match &out.kind {
        StartKind::Ready { evidence } => (
            format!(
                "✓ 后台已就绪 handle={} pid={} 用时 {secs:.1}s\n就绪判据命中：{evidence}",
                i.handle, i.pid
            ),
            None,
        ),
        StartKind::Exited {
            code,
            tail,
            last_probe,
        } => (
            format!(
                "✗ 进程已退出（没等到就绪）handle={} pid={} 用时 {secs:.1}s exit={}\n\
                 就绪判据最后一次：{}",
                i.handle,
                i.pid,
                code.map(|c| c.to_string()).unwrap_or_else(|| "-".into()),
                last_probe.clone().unwrap_or_else(|| "（没跑过）".into())
            ),
            Some(tail.clone()),
        ),
        StartKind::NotReady {
            evidence,
            tail,
            hint,
        } => (
            format!(
                "⚠ 进程还活着，但就绪判据在窗口内没命中 handle={} pid={} 用时 {secs:.1}s\n\
                 判据最后一次：{evidence}\n\
                 （判据没命中 ≠ 启动失败：慢启动很常见。看下面的日志尾，分辨它在启动还是卡住了。）{}",
                i.handle,
                i.pid,
                match hint {
                    Some(h) => format!("\n{h}"),
                    None => String::new(),
                }
            ),
            Some(tail.clone()),
        ),
        StartKind::Started => (
            format!(
                "✓ 后台已启动（没给就绪判据，不等它）handle={} pid={}",
                i.handle, i.pid
            ),
            None,
        ),
    };
    if let Some(t) = tail
        && !t.trim().is_empty()
    {
        s.push_str(&format!("\n日志尾部：\n{t}"));
    }
    s.push_str(&format!(
        "\n日志文件（可直接 read，或用 execute {{\"op\":\"log\",\"handle\":\"{}\"}}）：{}",
        i.handle, i.log
    ));
    s.push_str(&format!(
        "\n查状态 / 读日志 / 停掉：execute {{\"op\":\"status\"|\"log\"|\"stop\",\"handle\":\"{}\"}}",
        i.handle
    ));
    if i.keep_alive {
        s.push_str(
            "\n（已声明 keep_alive：本次 run 结束**不**收它 —— 用户在【服务】面板能看到、能按 pid 停掉；IDE 退出时一并收。）",
        );
    } else {
        s.push_str(&format!(
            "\n（**未声明 keep_alive**：本次 run 一结束引擎就会把它收掉。用户要的是“服务跑着”、而不是“验证一下”时，\
             现在就 execute {{\"op\":\"stop\",\"handle\":\"{}\"}} 停掉、再带 \"keep_alive\":true 重启；\
             否则你 final 里的“已启动”在用户看到时已经是空的。）",
            i.handle
        ));
    }

    let others: Vec<crate::proc::ProcInfo> = crate::proc::listing_for(proj)
        .into_iter()
        .filter(|p| !crate::proc::is_dead(&p.state) && p.handle != i.handle)
        .collect();
    if !others.is_empty() {
        s.push_str(&format!(
            "\n其他托管进程：{}",
            crate::proc::render_listing(&others)
        ));
        if let Some(rc) = &i.ready_cmd {
            let same = others
                .iter()
                .filter(|p| p.ready_cmd.as_deref() == Some(rc.as_str()))
                .count();
            if same > 0 {
                s.push_str(&format!(
                    "\n⚠ 上面有 {same} 个用的是**同一个就绪判据**。如果你是在反复重启同一个服务：\
                     先 stop 掉旧的再起 —— 重复起一个已经跑着的服务通常只会撞端口占用，\
                     而那个失败是假的。"
                ));
            }
        }
    }
    s
}

/// 后台启动的入口。
///
/// **`Err` = "这次请求本身没能执行"**（开关关着 / 闸门拒了 / 判据在启动前就已命中 /
/// 到并发上限 / 起不来），`Ok` = "真的跑起来了，结果好坏都写在文本里"。
/// 与 [`parse_action`] 的分界一致：句柄不存在等同于参数不合法，不该伪装成一次成功的工具调用。
pub(crate) fn tool_exec_bg(
    proj: &Path,
    cfg: &AppConfig,
    spec: &crate::proc::StartSpec,
) -> Result<String, String> {
    if !cfg.proc.enabled {
        return Err(
            "后台启动已关闭（ruyix.code.harness.proc.enabled = false）。请改用前台 execute，\
                    或在设置里打开 —— 关着的时候去用 start / Start-Process 那类花招只会更糟：\
                    输出拿不到、进程还脱离掌控。"
                .into(),
        );
    }
    // 闸门对后台同样生效：跑多久不改变"它是不是一条命令"
    preflight_execute(proj, &spec.cmd)?;
    execute_allowed(&spec.cmd)?;
    if let Some(rc) = &spec.ready_cmd {
        preflight_execute(proj, rc).map_err(|e| format!("就绪判据没通过闸门：\n{e}"))?;
        execute_allowed(rc).map_err(|e| format!("就绪判据被拒绝：{e}"))?;
    }
    let out = crate::proc::start(
        proj,
        spec,
        cfg.proc.max,
        cfg.proc.ready_timeout_secs,
        &crate::config::project_state_root(cfg, proj),
    )?;
    Ok(render_start(proj, &out))
}

/// 托管进程的句柄操作：查状态 / 读日志尾 / 停掉（连子进程树）。
pub(crate) fn tool_proc(proj: &Path, op: ProcOp, handle: &str) -> Result<String, String> {
    match op {
        ProcOp::Status => {
            let i = crate::proc::status(proj, handle)?;
            let ready = i.ready_cmd.clone().unwrap_or_else(|| "（无）".into());
            let mut s = format!(
                "handle={} {} pid={} 已跑 {:.1}s\n命令：{}\n就绪判据：{ready}\n日志：{}",
                i.handle,
                i.state,
                i.pid,
                i.elapsed_ms as f64 / 1000.0,
                clip(&i.cmd, 160),
                i.log
            );
            if crate::proc::is_dead(&i.state) {
                s.push_str(&format!(
                    "\n进程已退出 —— 退出码在上面 state 里；日志尾部用 \
                     execute {{\"op\":\"log\",\"handle\":\"{}\"}} 看。",
                    i.handle
                ));
            } else {
                s.push_str(&format!(
                    "\n还在跑。读日志尾：execute {{\"op\":\"log\",\"handle\":\"{}\"}}；\
                     停掉：execute {{\"op\":\"stop\",\"handle\":\"{}\"}}",
                    i.handle, i.handle
                ));
            }
            Ok(s)
        }
        ProcOp::Log => {
            let i = crate::proc::status(proj, handle)?;
            let tail = crate::proc::log_tail(proj, handle, crate::proc::LOG_TAIL_LINES)?;
            let what = if tail.trim().is_empty() {
                "（空 —— 进程可能还没吐东西）"
            } else {
                "（末尾若干行）"
            };
            Ok(format!(
                "handle={} {} pid={} 日志{what}：\n{tail}\n—— 全文在 {}",
                i.handle, i.state, i.pid, i.log
            ))
        }
        ProcOp::Stop => {
            let i = crate::proc::stop(proj, handle)?;
            Ok(format!(
                "✓ 已停止 handle={} pid={}（连子进程树一起杀，端口会立刻释放）\n日志留着：{}",
                i.handle, i.pid, i.log
            ))
        }
    }
}

/// 执行一次 connect：看清单 / 调 MCP 工具 / 委托远端 Agent。
/// "连不上"（目标不存在、服务器起不来）走 Err → 提示词那侧记为失败轮；
/// 外部系统的业务错误是信息不是故障，包成 Ok 交回模型自己判断。
/// connect 的**调用摘要**（不含执行）：并发路径要先把摘要定好，再去 join 那些 future。
pub(crate) fn connect_brief(a: &ConnectAction) -> String {
    match a {
        ConnectAction::List => "connect list".into(),
        ConnectAction::Call { server, tool, .. } => format!("connect {server}/{tool}"),
        ConnectAction::Send { agent, text } => {
            format!("connect {agent}（委托 {} 字）", text.chars().count())
        }
    }
}

/// connect 的**执行**（不含摘要）：单动作与批并发两条路径共用同一个实现。
pub(crate) fn connect_future<'a>(
    conn: &'a dyn Connector,
    action: ConnectAction,
) -> ConnectFuture<'a, String> {
    Box::pin(connect_run(conn, action))
}

pub(crate) async fn connect_run(
    conn: &dyn Connector,
    action: ConnectAction,
) -> Result<String, String> {
    match action {
        ConnectAction::List => conn
            .list()
            .await
            .map(|ts| connect_note(&ts).unwrap_or_else(|| "（当前没有可连接的外部能力）".into())),
        ConnectAction::Call {
            server,
            tool: name,
            arguments,
        } => conn
            .call(ConnectRequest {
                action: "call".into(),
                server,
                tool: name,
                arguments,
                ..Default::default()
            })
            .await
            .map(connect_outcome_text),
        ConnectAction::Send { agent, text } => conn
            .call(ConnectRequest {
                action: "send".into(),
                agent,
                text,
                ..Default::default()
            })
            .await
            .map(connect_outcome_text),
    }
}

/// 单动作路径：摘要 + 执行（与并发路径同一份实现，不许各写一遍）
pub(crate) async fn connect_step(
    conn: &dyn Connector,
    action: ConnectAction,
) -> (String, String, Result<String, String>) {
    let brief = connect_brief(&action);
    (
        "connect".to_string(),
        brief,
        connect_future(conn, action).await,
    )
}

pub(crate) fn connect_outcome_text(outcome: ConnectOutcome) -> String {
    if outcome.is_error {
        format!("（外部系统返回错误）{}", outcome.text)
    } else {
        outcome.text
    }
}

/// 历史裁剪：只留 user/assistant、非空文本，最多 n 条（最近的），保持原顺序。
pub fn tail_history(history: &[HistoryMsg], n: usize) -> Vec<HistoryMsg> {
    let filtered: Vec<HistoryMsg> = history
        .iter()
        .filter(|m| (m.role == "user" || m.role == "assistant") && !m.text.trim().is_empty())
        .cloned()
        .collect();
    if filtered.len() <= n {
        filtered
    } else {
        filtered[filtered.len() - n..].to_vec()
    }
}

/// 阶梯的**字节代理**：`content` 字节 + 每张图的固定配额（`IMAGE_PROXY_BYTES`）。
///
/// 为什么图的 base64 不按字面量：**base64 字节不是 token**。厂商按**像素**计价一张图
/// （1600px 的截图约 1.1k~1.6k token），而同样这张图的 base64 有 30 万字节 —— 直接量它
/// 会让调度器以为阶梯已经 7.5 万 token、于是每一轮都判"超预算"把历史压掉（p4 的判序是
/// 预算 → 下限 → DP，预算越界是**强制**压）。一张常量配额既不虚报规模，也不会污染
/// "每轮增长"的读数（增长仍是纯文本的增长）。
pub(crate) fn msgs_bytes(msgs: &[ChatMessage]) -> usize {
    msgs.iter()
        .map(|m| m.content.len() + m.images.len() * IMAGE_PROXY_BYTES)
        .sum()
}

/// 一轮工具调用在 `msgs` 里的两个落点，以及它折叠后各自替换成什么。
///
/// 为什么记下标、而不给消息加个"这是第几轮"的字段：`ChatMessage` 是 `{role, content}` 且
/// 全仓共用（`llm.rs`），为工具循环的记账给它加字段会污染每一个调用方。
/// 下标 + **幂等**的摘要文本就够：同一轮反复折叠写进去的是同一串，不会漂移。
pub(crate) struct RoundSlot {
    /// 模型那半（assistant，含它发出的调用 JSON）在 msgs 里的下标
    pub(crate) assistant: usize,
    /// 结果那半（user）在 msgs 里的下标
    pub(crate) result: usize,
    /// 折叠后 assistant 那半替换成什么（只留"调了什么"，不留 content 正文）
    pub(crate) call_digest: String,
    /// 折叠后 result 那半替换成什么（一行事实）
    pub(crate) outcome_digest: String,
}

/// 一轮调用的形状摘要（**不带正文**）：折叠后模型仍认得出自己那一轮调了什么。
/// 停滞守卫触发时回灌给模型的那条**硬指令**。
///
/// 措辞刻意"指名要什么"：① 已确认的事实（引用 findings id）② 卡在哪 ③ 需要用户给什么。
/// 只喊"你必须 final"会让模型把同一段探索换个说法再写一遍 —— 那正是它绕不出来的地方。
pub(crate) fn stall_instruction(rounds: usize) -> String {
    format!(
        "（停滞守卫：连续 {rounds} 轮既没有新结论、也没有文件变更。本轮**不再接受任何工具调用**，\
         按两步收尾：\n\
         1) 先调 record_findings 记下你**已经确认的事实**（每条 claim + 证据指针 path:line 或 命令+退出码）。\
         这一步**不需要**完整答案，只需要事实 —— 卡住时你一定至少确认过一些东西；\n\
         2) 再用 final 交付 **当前最佳答案**（允许不完整），并写清**还缺哪条信息、需要用户提供什么**。\n\
         硬性要求：final 必须走工具调用形状 {{\"final\":\"…\"}} —— **不要直接把正文写进 content**，\
         协议不认正文（那会被判『无法解析』而白烧一轮）。）"
    )
}

/// 硬终止时交给用户的诚实报告（模型不听守卫，就由引擎把账本说清楚）。
pub(crate) fn stall_report(cfg: &AppConfig, ctx: &Ctx<'_>, step: usize) -> String {
    let mut s = format!(
        "（停滞守卫：连续 {} 轮没有新结论、也没有文件变更，本 run 已停止 —— \
         再跑下去只会换着法子重读同一批文件。本 run 共 {} 轮。",
        cfg.agent.stall_rounds, step
    );
    let items = ctx.progress().in_prompt();
    if items.is_empty() {
        s.push_str("\n期间**没有记下任何确认的事实**（record_findings 为空），所以给不出结论。");
    } else {
        s.push_str("\n已确认的事实：");
        for f in items.iter().take(12) {
            s.push_str(&format!("\n- {}", f.line()));
        }
    }
    let ledger = ctx.progress().ledger();
    if !ledger.is_empty() {
        s.push_str("\n已做过的事（引擎账本，尾部 12 条）：");
        for l in ledger.iter().rev().take(12).rev() {
            s.push_str(&format!("\n- {l}"));
        }
    }
    s.push_str("\n\n请直接告诉我下一步该怎么走，或补上我缺的那条信息。）");
    s
}

/// 引擎账本的一行：**只记形状与结局，不记内容**。
///
/// - `read`：`r12 read ui/scripts/session.js (296-334) ✓` —— 只到"读过这一段"为止；
/// - `execute`：`r13 execute cargo test ✓` / `✗ 退出码 101` —— 退出码是**结局**，
///   不是内容，所以留；命令自身的输出不留（要去 findings 里写结论，或重新精确读）。
///
/// 为什么这么做：把结果首行塞进账本看着有用，实际是**绕过折叠** —— 旧结果的头 100 字符
/// 会永远在场、账本也随轮数线性涨。这一条是被单测抓出来的（`old_tool_results_are_folded_out_of_the_request`）。
pub(crate) fn ledger_line_for(
    step: usize,
    tool: &str,
    brief: &str,
    res: &Result<String, String>,
) -> String {
    let verdict = if res.is_ok() { "✓" } else { "✗" };
    let mut line = format!("r{step} {tool} {brief} {verdict}");
    if tool == "execute" {
        // 退出码/失败原因：从结果文本里认（引擎的执行回灌里带着它）
        let text = match res {
            Ok(t) => t.as_str(),
            Err(e) => e.as_str(),
        };
        if let Some(seg) = text
            .lines()
            .find(|l| l.contains("退出码") || l.contains("exit"))
        {
            line.push_str(&format!(" {}", clip(seg.trim(), 60)));
        } else if res.is_err() {
            line.push_str(&format!(" {}", clip(text, 60)));
        }
    }
    line
}

pub(crate) fn call_shape(actions: &[Action]) -> String {
    let one = |a: &Action| match a {
        Action::Read(s) => format!("read {}", s.brief()),
        Action::Write(s) => s.brief(),
        Action::Execute(c, _) => format!("execute {}", clip(c, 60)),
        Action::ExecBg(s) => format!("execute bg {}", clip(&s.cmd, 60)),
        Action::Proc(op, h) => format!("execute {} {h}", proc_op_name(*op)),
        Action::Connect(_) => "connect".to_string(),
        Action::Plan(p) => format!("plan {} 步", p.len()),
        Action::Ask(_) => "ask_user".to_string(),
        Action::Findings(v) => format!("record_findings {} 条", v.len()),
        Action::Final(_) => "final".to_string(),
    };
    if actions.len() == 1 {
        one(&actions[0])
    } else {
        actions.iter().map(one).collect::<Vec<_>>().join("；")
    }
}

/// 一条工具结果折成一行：调了什么、成没成、头一行是什么。
///
/// 取头一行对四种能力刚好都是最有信息量的那行：read 的表头（`--- src/a.rs ---` 或
/// 窗口表头）、write 的"已写入/已改"、execute 输出的第一行（往往就是那行 error）。
pub(crate) fn outcome_line(tool: &str, brief: &str, res: &Result<String, String>) -> String {
    match res {
        Ok(t) => {
            let head = t.lines().next().unwrap_or("").trim();
            format!("{tool} {brief} ✓ {}", clip(head, 100))
        }
        Err(e) => format!("{tool} {brief} ✗ {}", clip(e, 100)),
    }
}

/// 把"超出保留窗口"的老轮次折叠成一行事实，返回 `(折叠了几轮, 省下多少字节)`。
///
/// 幂等：已折叠的轮次再折一次写进去的是同一串文本，所以每轮都调它没有副作用。
/// 它管的是**单次 run 内**工具循环的历史；跨会话的输入历史归 [`tail_history`]，两回事。
pub(crate) fn fold_history(
    msgs: &mut [ChatMessage],
    rounds: &[RoundSlot],
    keep: usize,
) -> (usize, usize) {
    if rounds.len() <= keep {
        return (0, 0);
    }
    let (mut folded, mut saved) = (0usize, 0usize);
    for r in &rounds[..rounds.len() - keep] {
        let mut hit = false;
        for (idx, digest) in [(r.assistant, &r.call_digest), (r.result, &r.outcome_digest)] {
            if msgs[idx].content != *digest {
                saved += msgs[idx].content.len().saturating_sub(digest.len());
                msgs[idx].content = digest.clone();
                hit = true;
            }
        }
        if hit {
            folded += 1;
        }
    }
    (folded, saved)
}

/// plan 步骤 → 大纲区的任务列表
pub(crate) fn plan_to_outline(task: &str, steps: Vec<PlanStep>) -> Plan {
    Plan {
        project_name: "agent".into(),
        language: String::new(),
        summary: clip(task, 120),
        entry: String::new(),
        test_command: String::new(),
        steps,
    }
}

/// 步骤声明的文件里，有多少已经在本 run 落地（overlay = 本 run 写过的相对路径 → 内容）
pub(crate) fn produced_files(s: &PlanStep, overlay: &BTreeMap<String, String>) -> usize {
    s.files
        .iter()
        .filter(|f| overlay.contains_key(f.as_str()))
        .count()
}

/// 步骤完成度 → 推给 UI（全部文件落地 = done ✅；部分 = running ⛏️）
///
/// **只在 `execute_plan` 关着的时候用**（开着时状态由派发逻辑报，两条通道同时跑只会互相矛盾）。
/// 它刻意不查"文件是否本来就存在"：run 中途我们没法知道模型的意图，能诚实说的只有
/// "本 run 写出来几个"。所以它只会把状态**往前推**（never downgrade）——
/// 拿"声明文件本来就存在"当满足是收尾（[`settle_steps`]）才做的判断。
pub(crate) fn emit_step_progress(
    sink: &dyn Sink,
    steps: &[PlanStep],
    overlay: &BTreeMap<String, String>,
) {
    let total = steps.len();
    for (i, s) in steps.iter().enumerate() {
        if s.files.is_empty() {
            continue;
        }
        let produced = produced_files(s, overlay);
        let st = if produced == s.files.len() {
            "done"
        } else if produced > 0 {
            "running"
        } else {
            continue; // 还没动过，不用发事件
        };
        sink.step(
            i + 1,
            total,
            &StepOutcome {
                step_id: s.id,
                title: s.title.clone(),
                status: st.into(),
                files: s.files.clone(),
                ..Default::default()
            },
        );
    }
}

/// 子步骤失败后交回模型的那一轮 —— 这是 `execute_plan` 下模型唯一的干预通道。
///
/// 为什么不闷头继续跑下一步：第 1 步就崩了的话后面全白跑。把"哪一步、为什么、已经产出
/// 什么"如实回灌，让模型自己选：重排计划 / 自己动手补齐 / 直接交付（并说明为何没做）。
pub(crate) fn step_failure_feedback(
    step: &PlanStep,
    index: usize,
    total: usize,
    report: &StepReport,
) -> String {
    let reason = report.error.clone().unwrap_or_else(|| "未说明原因".into());
    let produced = if report.files.is_empty() {
        "无".to_string()
    } else {
        report.files.join("、")
    };
    let msg = format!(
        "步骤 {index}/{total}「{}」执行失败：{reason}\n\
         本步已产出的文件：{produced}（已留在覆盖层里，不回滚）。\n\
         请决定下一步：① 调整计划后继续（重新输出 plan —— 注意新计划会从第 1 步重新执行）\
         ② 你自己动手补齐（read / write / execute）\
         ③ 直接交付（final，并说明这一步为何没做）。",
        step.title.trim()
    );
    format!(
        "{{\"ok\": false, \"error\": {}}}",
        serde_json::to_string(&msg).unwrap_or_else(|_| "\"\"".into())
    )
}

/// run 收尾：把计划里每个步骤都落到终态。
///
/// 沙漏（⌛）的语义是**等待执行** —— run 已经结束还显示等待，就是在骗人。
/// 步骤状态原先只由 [`emit_step_progress`] 在 write 轮里按「声明文件是否落地」推，
/// 于是两类步骤会永远停在初始态：① 计划里没声明文件的步骤（「编译验证」这类
/// 没有文件级判据的步骤）；② 声明了文件、这次却没写的步骤 —— 实测 run
/// `agent-20260920-091436` 只暂存了 `cloud-shop-admin/pom.xml`，而计划第 3 步
/// 「同步文档」声明的文档文件零产出，界面永远停在 2/3 + ⌛。
///
/// `delivered` = 本次交付了 final（按契约，模型在 final 里断言"全部完成后"），
/// 否则是轮次上限 / 用户取消 / 致命错误收场 —— 那种场合一步都不能算完成。
///
/// **这个推断分支只在没有引擎事实可用时才走**（`execute_plan` 关着，或步骤还没轮到派发）。
/// 它的判据是 `PlanStep::files` —— 模型**动手前**自己写的产出清单，会漂移，所以口径要宽：
///
/// * `missing` 只认「本 run 没写 **且** 磁盘上也没有」。声明了却本来就存在、无需重写的文件
///   （实测 run `agent-20260920-142405` 第 7 步声明 `vite.config.js`，那是前一天就有的文件）
///   不算缺 —— 否则界面会指着一个静静躺在那儿的文件说"缺它"。
/// * 「有产出但对不上声明」落 `partial`（未对齐），不再冒充 `skipped`。
///   `skipped`（跳过）的意思是"这步一件没做"，而实测里 6 个 `skipped` 有 4 个其实做了
///   一半以上、只是改名或少写 —— 用一个比事实更重的词去描述，和沙漏是同一种骗人。
pub(crate) fn settle_steps(
    sink: &dyn Sink,
    steps: &[PlanStep],
    overlay: &BTreeMap<String, String>,
    // 项目根：用来把"声明了、本 run 没写"再分成「真缺」和「本来就在」
    proj: &Path,
    delivered: bool,
    // 每个步骤**已执行过**的终态（`execute_plan` 下由派发逻辑写）。有它的步骤直接照抄：
    // 引擎知道"这一步跑完了 / 失败了"这个事实，比"声明文件是否落地"的推断准得多。
    executed: &[Option<(String, String)>],
) {
    let total = steps.len();
    for (i, s) in steps.iter().enumerate() {
        if let Some(Some((status, notes))) = executed.get(i) {
            sink.step(
                i + 1,
                total,
                &StepOutcome {
                    step_id: s.id,
                    title: s.title.clone(),
                    status: status.clone(),
                    notes: notes.clone(),
                    files: s.files.clone(),
                    ..Default::default()
                },
            );
            continue;
        }
        // 声明了、本 run 却没写的文件（两种成因，别混成一句"缺失"）：
        let unwritten: Vec<&str> = s
            .files
            .iter()
            .filter(|f| !overlay.contains_key(f.as_str()))
            .map(String::as_str)
            .collect();
        // 真缺 = 没写、磁盘上也没有。已经躺在项目里的文件不算缺（那只是"无需重写"）。
        let missing: Vec<&str> = unwritten
            .iter()
            .copied()
            .filter(|f| !proj.join(f).exists())
            .collect();
        // 本 run 真写出来的声明文件（partial 的说明里要列清楚"已经写了哪些"）
        let produced: Vec<&str> = s
            .files
            .iter()
            .filter(|f| overlay.contains_key(f.as_str()))
            .map(String::as_str)
            .collect();
        // 四种情形分开判，别让"没交付"一刀切把已做完的步骤降级：
        //   ① 真缺、且一件没写 → skipped，把缺什么写清楚（这是"跳过"的正当用法）
        //   ② 真缺、但有产出 → partial「未对齐」：做了，只是产出与它自己事前列的清单不一致
        //   ③ 计划里根本没声明文件（「编译验证」这类没有文件级判据的步骤）→ 只有交付才算完成
        //   ④ 声明的文件全部满足（写过 or 本来就在）→ done，与交付与否无关
        //      （取消前已经做完的步骤不该被降级）
        // 注意 ④ 不留 note：声明文件本来就在是**正常**的，给它挂个 ⚠ 只会让用户学会忽略提示。
        let (status, notes) = if !missing.is_empty() && produced.is_empty() {
            // 一件没写：这才是"跳过"
            let why = if delivered {
                "本次未产出声明的文件："
            } else {
                "本次未完成；未产出："
            };
            ("skipped", format!("{why}{}", missing.join("、")))
        } else if !missing.is_empty() {
            // 有产出但没对齐：做了事，只是产出与它自己事前列的清单不一致
            (
                "partial",
                format!(
                    "产出与声明不一致（已写 {}；还缺 {}）",
                    produced.join("、"),
                    missing.join("、")
                ),
            )
        } else if s.files.is_empty() {
            if delivered {
                ("done", String::new())
            } else {
                ("skipped", "本次未完成".into())
            }
        } else {
            ("done", String::new())
        };
        sink.step(
            i + 1,
            total,
            &StepOutcome {
                step_id: s.id,
                title: s.title.clone(),
                status: status.into(),
                notes,
                files: s.files.clone(),
                ..Default::default()
            },
        );
    }
}
