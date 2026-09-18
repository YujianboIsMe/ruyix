//! 可观测性：执行日志 + trace file + 给 Agent 的调试 context。
//!
//! ## 设计取舍
//!
//! 1. **append-only**。日志与 span 都是逐行追加到 `logs.jsonl` / `trace.jsonl`。
//!    崩溃或被强杀时，已经发生的一切都还在 —— 而"跑挂了"正是最需要日志的时候。
//!    最后一次性写文件的方案，恰恰在那时什么都留不下。
//! 2. **全局 tracer，未初始化时是 no-op**。不把 `AppHandle` 穿进十几个调用点：
//!    观测不该改变被测代码的形状，否则"加观测"就变成了"改被测代码"。
//! 3. **span 用同 id 两行表达**（`status: start` → 落到 `status: done/error`）。
//!    读取时按 id 归并。好处是**没闭合的 start 一眼就是崩溃点**，
//!    而且不需要在每个阶段手工埋 begin/end 配对。
//! 4. **脱敏与截断在写入层做**，不靠调用方自觉。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

/// 单个属性 / 日志消息的上限。超限**必须留下痕迹**（见 `cap`）。
pub const ATTR_CAP: usize = 4000;

// ---------------------------------------------------------------- 脱敏与截断

/// 截断并**显式标注**丢了多少。
///
/// 静默截断是本项目踩过的坑（v0.2：机器可读输出被人为展示阈值腰斩，
/// 结果报成"JSON 解析失败"，把人往完全错的方向带）。所以这里一律留痕。
pub fn cap(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    let dropped_bytes = s.len().saturating_sub(head.len());
    format!("{head}\n… [截断 {dropped_bytes} 字节]")
}

fn is_secret_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+' | '/' | '=')
}

fn matches_at(chars: &[char], i: usize, pat: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    i + p.len() <= chars.len() && chars[i..i + p.len()] == p[..]
}

/// 按**已知形态**打码：`sk-…` / `Bearer …` / `Authorization: …`。
///
/// 刻意不上正则依赖：形态极少，手写扫描足够，且不会把依赖引进来。
fn redact_forms(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if matches_at(&chars, i, "sk-") {
            let mut j = i + 3;
            while j < chars.len() && is_secret_char(chars[j]) {
                j += 1;
            }
            if j - (i + 3) >= 12 {
                out.push_str("[REDACTED]");
                i = j;
                continue;
            }
        }
        if matches_at(&chars, i, "Bearer ") || matches_at(&chars, i, "bearer ") {
            let mut j = i + 7;
            while j < chars.len() && is_secret_char(chars[j]) {
                j += 1;
            }
            if j - (i + 7) >= 8 {
                out.push_str(&chars[i..i + 7].iter().collect::<String>());
                out.push_str("[REDACTED]");
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 脱敏：**两条路径同时走**。
///
/// ① 精确替换当前配置里的密钥 —— 不依赖正则猜对格式，是自己那把钥匙的兜底；
/// ② 形态匹配 —— 覆盖别人的钥匙、以及日志里出现的各种写法。
///
/// 诚实边界：形态匹配只认已知形态。所以精确替换不是可选项，是必需项。
pub fn redact(s: &str, secrets: &[String]) -> String {
    let mut out = redact_forms(s);
    for k in secrets {
        if k.trim().len() >= 8 {
            out = out.replace(k.trim(), "[REDACTED]");
        }
    }
    out
}

// ---------------------------------------------------------------- 数据结构

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct LogRow {
    pub ts_ms: u128,
    pub level: String,
    #[serde(default)]
    pub stage: String,
    pub msg: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SpanRow {
    pub id: u64,
    #[serde(default)]
    pub parent: Option<u64>,
    pub kind: String,
    pub name: String,
    pub ts_ms: u128,
    /// start 行没有这个字段；close 行才有
    #[serde(default)]
    pub end_ms: Option<u128>,
    pub status: String,
    #[serde(default)]
    pub attrs: BTreeMap<String, String>,
}

/// 归并后的 span（同 id 的两行合并成一条）。
#[derive(Serialize, Clone, Debug)]
pub struct Span {
    pub id: u64,
    pub parent: Option<u64>,
    pub kind: String,
    pub name: String,
    pub start_ms: u128,
    pub end_ms: u128,
    pub duration_ms: u128,
    pub status: String,
    pub attrs: BTreeMap<String, String>,
    /// start 之后没有闭合行 —— **多半就是崩溃点**
    pub unclosed: bool,
}

// ---------------------------------------------------------------- Tracer

pub struct Tracer {
    run_id: String,
    dir: PathBuf,
    t0: Instant,
    secrets: Vec<String>,
    next_id: AtomicU64,
    /// 打开中的 span：name → (id, parent), 用于 `span_close_latest`
    open: Mutex<Vec<(String, u64)>>,
}

static TRACER: OnceLock<Mutex<Option<Arc<Tracer>>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Arc<Tracer>>> {
    TRACER.get_or_init(|| Mutex::new(None))
}

/// 挂上 tracer（每次运行开始时调一次）。
pub fn install(t: Arc<Tracer>) {
    *slot().lock().unwrap() = Some(t);
}

/// 摘掉（结束本次观测作用域）。没挂上时所有写入接口都是 no-op。
///
/// 目前只有测试与显式收尾点用；保留为公开 API 是因为它是作用域的一环
/// —— 只挂不摘，长跑进程里会一直往同一个目录追加。
#[allow(dead_code)]
pub fn uninstall() {
    *slot().lock().unwrap() = None;
}

pub fn current() -> Option<Arc<Tracer>> {
    slot().lock().unwrap().clone()
}

/// 这次运行的可观测产物目录（没挂 tracer 时为 None）。
pub fn current_dir() -> Option<PathBuf> {
    current().map(|t| t.dir.clone())
}

impl Tracer {
    pub fn new(dir: &Path, run_id: &str, secrets: Vec<String>) -> Arc<Tracer> {
        Arc::new(Tracer {
            run_id: run_id.to_string(),
            dir: dir.to_path_buf(),
            t0: Instant::now(),
            secrets,
            next_id: AtomicU64::new(1),
            open: Mutex::new(Vec::new()),
        })
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    fn now_ms(&self) -> u128 {
        self.t0.elapsed().as_millis()
    }

    /// 追加一行。
    ///
    /// **刻意不加锁**：每次 append 本来就是"自己 open + write"，锁在这里没有
    /// 正确性作用，却引入了一个"观测阻塞主流程"的风险点。
    /// 观测代码的原则是：**宁可能丢一行日志，也绝不能卡住被测代码。**
    fn append(&self, file: &str, line: &str) {
        let path = self.dir.join(file);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(f, "{line}");
        }
    }

    /// 非阻塞地访问 span 栈，**并且容忍 poisoned**。
    ///
    /// 中毒的锁意味着"某个线程在持锁时 panic 了"。对观测来说最正确的处理是
    /// **继续用**（数据仍然可用），而不是 `unwrap()` 再 panic 一次 ——
    /// 那会顺着调用栈把被观测的流程一起带走。
    fn with_stack<R>(&self, f: impl FnOnce(&mut Vec<(String, u64)>) -> R, fallback: R) -> R {
        match self.open.try_lock() {
            Ok(mut g) => f(&mut g),
            Err(std::sync::TryLockError::Poisoned(p)) => f(&mut p.into_inner()),
            // 竞争就放弃：parent 关系错一点，好过阻塞
            Err(std::sync::TryLockError::WouldBlock) => fallback,
        }
    }

    /// 打开一个 span，返回 id。
    pub fn span_open(&self, kind: &str, name: &str, attrs: BTreeMap<String, String>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let parent = self.with_stack(|v| v.last().map(|(_, i)| *i), None);
        self.with_stack(
            |v| {
                v.push((name.to_string(), id));
            },
            (),
        );
        let row = SpanRow {
            id,
            parent,
            kind: kind.to_string(),
            name: name.to_string(),
            ts_ms: self.now_ms(),
            end_ms: None,
            status: "start".into(),
            attrs: self.scrub_attrs(attrs),
        };
        self.append("trace.jsonl", &to_json(&row));
        id
    }

    /// 关掉最近一个同名 span（阶段收尾用：不用把 id 一路传下去）。
    pub fn span_close(&self, name: &str, status: &str, attrs: BTreeMap<String, String>) {
        let found = self.with_stack(
            |v| {
                let pos = v.iter().rposition(|(n, _)| n == name)?;
                let (name, id) = v.remove(pos);
                Some((name, id, v.last().map(|(_, i)| *i)))
            },
            None,
        );
        let Some((name, id, parent)) = found else {
            return;
        };
        let row = SpanRow {
            id,
            parent,
            kind: String::new(), // 归并时以 start 行为准
            name,
            ts_ms: self.now_ms(),
            end_ms: Some(self.now_ms()),
            status: status.to_string(),
            attrs: self.scrub_attrs(attrs),
        };
        self.append("trace.jsonl", &to_json(&row));
    }

    /// 一次性 span（子进程、LLM 调用这类"有明确起止但不需要配对"的场景）。
    pub fn span_once(
        &self,
        kind: &str,
        name: &str,
        status: &str,
        started_ms: u128,
        attrs: BTreeMap<String, String>,
    ) {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let parent = self.with_stack(|v| v.last().map(|(_, i)| *i), None);
        let row = SpanRow {
            id,
            parent,
            kind: kind.to_string(),
            name: name.to_string(),
            ts_ms: started_ms,
            end_ms: Some(self.now_ms().max(started_ms)),
            status: status.to_string(),
            attrs: self.scrub_attrs(attrs),
        };
        self.append("trace.jsonl", &to_json(&row));
    }

    pub fn log(&self, level: &str, stage: &str, msg: &str) {
        let row = LogRow {
            ts_ms: self.now_ms(),
            level: level.to_string(),
            stage: stage.to_string(),
            msg: cap(&redact(msg, &self.secrets), ATTR_CAP),
        };
        self.append("logs.jsonl", &to_json(&row));
    }

    fn scrub_attrs(&self, attrs: BTreeMap<String, String>) -> BTreeMap<String, String> {
        attrs
            .into_iter()
            .map(|(k, v)| (k, cap(&redact(&v, &self.secrets), ATTR_CAP)))
            .collect()
    }

    /// 便于测试与 CLI：现在的相对时间
    pub fn now(&self) -> u128 {
        self.now_ms()
    }
}

fn to_json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap_or_else(|e| format!("{{\"error\":\"序列化失败：{e}\"}}"))
}

// ---------------------------------------------------------------- 对外便捷接口（全局，未挂载即 no-op）

pub fn log(level: &str, stage: &str, msg: impl AsRef<str>) {
    if let Some(t) = current() {
        t.log(level, stage, msg.as_ref());
    }
}

pub fn stage_open(name: &str, attrs: BTreeMap<String, String>) {
    if let Some(t) = current() {
        let _ = t.span_open("stage", name, attrs);
    }
}

pub fn stage_close(name: &str, status: &str, attrs: BTreeMap<String, String>) {
    if let Some(t) = current() {
        t.span_close(name, status, attrs);
    }
}

pub fn span_once(
    kind: &str,
    name: &str,
    status: &str,
    started_ms: u128,
    attrs: BTreeMap<String, String>,
) {
    if let Some(t) = current() {
        t.span_once(kind, name, status, started_ms, attrs);
    }
}

/// 便捷构造 attrs
pub fn attrs(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// ---------------------------------------------------------------- 读取与归并

pub fn read_spans(dir: &Path) -> Result<Vec<Span>, String> {
    let path = dir.join("trace.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("读 {} 失败：{e}", path.display()))?;
    let mut map: std::collections::BTreeMap<u64, Span> = std::collections::BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let row: SpanRow = match serde_json::from_str(line) {
            Ok(r) => r,
            // 坏行跳过但不炸：日志是"尽量留下证据"，不是"必须完美"
            Err(_) => continue,
        };
        let e = map.entry(row.id).or_insert_with(|| Span {
            id: row.id,
            parent: row.parent,
            kind: row.kind.clone(),
            name: row.name.clone(),
            start_ms: row.ts_ms,
            end_ms: row.ts_ms,
            duration_ms: 0,
            status: "start".into(),
            attrs: BTreeMap::new(),
            unclosed: true,
        });
        if !row.kind.is_empty() {
            e.kind = row.kind.clone();
        }
        if !row.name.is_empty() {
            e.name = row.name.clone();
        }
        if row.parent.is_some() {
            e.parent = row.parent;
        }
        e.attrs.extend(row.attrs.clone());
        match row.end_ms {
            Some(end) => {
                e.end_ms = end;
                e.duration_ms = end.saturating_sub(e.start_ms);
                e.status = row.status.clone();
                e.unclosed = false;
            }
            None => {
                e.start_ms = row.ts_ms;
            }
        }
    }
    Ok(map.into_values().collect())
}

fn level_rank(l: &str) -> u8 {
    match l {
        "debug" => 0,
        "info" | "ok" => 1,
        "warn" => 2,
        "error" => 3,
        _ => 1,
    }
}

pub fn read_logs(dir: &Path, min_level: Option<&str>) -> Result<Vec<LogRow>, String> {
    let path = dir.join("logs.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text =
        std::fs::read_to_string(&path).map_err(|e| format!("读 {} 失败：{e}", path.display()))?;
    let floor = min_level.map(level_rank).unwrap_or(0);
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<LogRow>(l).ok())
        .filter(|r| level_rank(&r.level) >= floor)
        .collect())
}

/// 人读的时间线（CLI 与 context pack 共用）。
pub fn render_timeline(spans: &[Span]) -> String {
    let mut out = String::new();
    for s in spans {
        let mark = if s.unclosed {
            "⚠ 未结束"
        } else {
            match s.status.as_str() {
                "done" | "ok" | "passed" => "✓",
                "error" | "failed" => "✗",
                "skipped" => "⊘",
                _ => "·",
            }
        };
        let dur = if s.unclosed {
            "?".to_string()
        } else {
            format!("{}ms", s.duration_ms)
        };
        out.push_str(&format!(
            "{:>7}  {mark} {:<6} {:<28} {dur}\n",
            format!("{}ms", s.start_ms),
            s.kind,
            cap_head(&s.name, 28)
        ));
        for (k, v) in &s.attrs {
            if v.is_empty() || v == "0" {
                continue;
            }
            out.push_str(&format!("              {k} = {}\n", cap_head(v, 160)));
        }
    }
    out
}

fn cap_head(s: &str, n: usize) -> String {
    let one = s.replace('\n', " ");
    if one.chars().count() <= n {
        one
    } else {
        format!("{}…", one.chars().take(n).collect::<String>())
    }
}

// ---------------------------------------------------------------- 给 Agent 的调试 context

/// 生成一份**自包含**的调试上下文。
///
/// 与 v0.2 的"诊断即指令"同源，但覆盖整次运行：任务、失败点、时间线、**完整**失败证据、
/// 相关文件、上游产出、以及**被排除的检查**。
///
/// 最后一条是刻意的：不把"没跑成的检查"列出来，模型就会把"没验证"当成"没问题"。
pub fn context_pack(run_dir: &Path, rec: &crate::workspace::RunRecord) -> Result<String, String> {
    let mut o = String::new();
    let spans = read_spans(run_dir).unwrap_or_default();

    o.push_str(&format!("# 运行 {} 的调试上下文\n\n", rec.run_id));
    o.push_str(&format!(
        "- 状态：`{}`{}\n- 运行目录：`{}`\n- 模型：{}\n",
        rec.status,
        match &rec.error {
            Some(e) if !e.is_empty() => format!("，错误：{e}"),
            _ => String::new(),
        },
        run_dir.display(),
        rec.model
    ));
    // 隔离状态：回答"这一次到底隔离了没有"
    if let Some(v) = &rec.verify {
        if let Some(iso) = &v.isolation {
            o.push_str(&format!("- 运行时隔离：{}\n", iso.label()));
        }
    }
    o.push_str(&format!("\n## 任务原文\n\n{}\n", rec.task.trim()));

    // ---- 结论与失败点
    o.push_str("\n## 结论与失败点\n\n");
    let mut failures: Vec<String> = Vec::new();
    if let Some(e) = &rec.error {
        if !e.is_empty() {
            failures.push(format!("- 运行级错误：{e}"));
        }
    }
    if let Some(v) = &rec.verify {
        o.push_str(&format!("- 验证结论：{}\n", v.verdict));
        for c in v.checks.iter().filter(|c| c.status == "failed") {
            failures.push(format!(
                "- 检查失败：`{}` / {}（退出码 {}）",
                c.target,
                c.kind,
                c.exit_code
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "无".into())
            ));
        }
    }
    if let Some(l) = &rec.lint {
        let errs: Vec<_> = l
            .diagnostics
            .iter()
            .filter(|d| d.level == "error")
            .collect();
        if !errs.is_empty() {
            o.push_str(&format!("- 规约：{} 条 error\n", errs.len()));
            for d in errs.iter().take(20) {
                failures.push(format!(
                    "- 规约违规 {}：{} @ {}:{}",
                    d.rule,
                    d.message,
                    d.file(),
                    d.line()
                ));
            }
        }
    }
    if failures.is_empty() {
        o.push_str("- 没有发现失败项。\n");
    } else {
        o.push_str(&failures.join("\n"));
        o.push('\n');
    }

    // ---- 时间线
    o.push_str("\n## 时间线（trace）\n\n```\n");
    if spans.is_empty() {
        o.push_str("（没有 trace：这次运行没挂 tracer）\n");
    } else {
        o.push_str(&render_timeline(&spans));
    }
    o.push_str("```\n");

    // ---- 失败证据：**完整**，不截断
    o.push_str("\n## 失败证据（完整输出，未截断）\n\n");
    let mut any = false;
    if let Some(v) = &rec.verify {
        for c in v.checks.iter().filter(|c| c.status == "failed") {
            any = true;
            o.push_str(&format!(
                "### {}（{}）\n\n命令：\n```\n{}\n```\n退出码：{}\n",
                c.target,
                c.kind,
                c.cmd,
                c.exit_code
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "无".into())
            ));
            if !c.stderr.trim().is_empty() {
                o.push_str(&format!("stderr：\n```\n{}\n```\n", c.stderr.trim()));
            }
            if !c.stdout.trim().is_empty() {
                o.push_str(&format!("stdout：\n```\n{}\n```\n", c.stdout.trim()));
            }
        }
    }
    if !any {
        o.push_str("（没有失败检查）\n");
    }

    // ---- 相关文件
    o.push_str("\n## 相关文件\n\n");
    let mut targets: Vec<String> = Vec::new();
    if let Some(v) = &rec.verify {
        for c in v.checks.iter().filter(|c| c.status == "failed") {
            if c.target.ends_with(".py") || c.target.ends_with(".rs") {
                targets.push(c.target.clone());
            }
        }
    }
    if let Some(l) = &rec.lint {
        for d in l.diagnostics.iter().filter(|d| d.level == "error") {
            let f = d.file().to_string();
            if !targets.contains(&f) {
                targets.push(f);
            }
        }
    }
    if targets.is_empty() {
        if let Some(g) = &rec.generation {
            for f in g.files.iter().take(10) {
                targets.push(f.path.clone());
            }
        }
    }
    if targets.is_empty() {
        o.push_str("（没有可定位的文件）\n");
    }
    for rel in targets.iter().take(8) {
        let p = run_dir.join(rel);
        match std::fs::read_to_string(&p) {
            Ok(text) => {
                o.push_str(&format!(
                    "### `{}`\n\n```\n{}\n```\n\n",
                    rel,
                    cap(&text, 6000)
                ));
            }
            Err(e) => o.push_str(&format!("### `{}`\n\n（读不到：{e}）\n\n", rel)),
        }
    }

    // ---- 上游产出
    o.push_str("## 上游产出\n\n");
    if let Some(p) = &rec.plan {
        for s in p.steps.iter().take(12) {
            o.push_str(&format!("- [{}] {}\n", s.kind, s.title));
        }
    } else {
        o.push_str("（没有规划）\n");
    }
    if let Some(g) = &rec.generation {
        o.push_str(&format!("\n本轮生成 {} 个文件：\n", g.files.len()));
        for f in g.files.iter().take(20) {
            o.push_str(&format!("- {}（{} 字节）\n", f.path, f.bytes));
        }
    }

    // ---- 被排除的检查（**不许把"没跑"当"没有"**）
    o.push_str("\n## 被排除 / 跳过的项（不要把「没跑」当成「没问题」）\n\n");
    let mut excluded: Vec<String> = Vec::new();
    if let Some(v) = &rec.verify {
        for c in v.checks.iter().filter(|c| c.status == "skipped") {
            excluded.push(format!("- 跳过检查 `{}`：{}", c.target, c.reason));
        }
    }
    if let Some(l) = &rec.lint {
        // 豁免统计在 JSON 里是嵌套结构（items/total/used/without_reason）
        let total = l
            .suppressions
            .get("total")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let bad = l
            .suppressions
            .get("without_reason")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if total > 0 {
            excluded.push(format!(
                "- 规约豁免 {total} 条（其中无理由 {bad} 条）——豁免不等于修好"
            ));
        }
    }
    if excluded.is_empty() {
        o.push_str("（没有跳过的检查）\n");
    } else {
        o.push_str(&excluded.join("\n"));
        o.push('\n');
    }

    // ---- 知识注入（v0.6）：注入了什么、多少、为什么被裁
    //
    // 与"被排除的检查"同源：**检索被裁掉的项必须写出来**，否则模型会把
    // "没注入"当成"知识库里没有"。最后一条注错了还会误导它编造。
    o.push_str("\n## 知识注入（本地知识库）\n\n");
    if rec.kb.is_empty() {
        o.push_str("- 本次运行没有知识库注入记录（知识库没启用，或这次运行早于 v0.6）。\n");
    } else {
        for inj in &rec.kb {
            o.push_str(&format!("- `{}`：{}\n", inj.stage, inj.summary()));
        }
        let cuts: Vec<String> = rec
            .kb
            .iter()
            .flat_map(|i| {
                i.dropped
                    .iter()
                    .map(move |d| format!("- `{}` 被裁：{} — {}（{}）", i.stage, d.path, d.reason, d.detail))
            })
            .take(30)
            .collect();
        if !cuts.is_empty() {
            o.push_str("\n被裁掉的检索结果（**没注入**，不要当成「知识库里没有」）：\n");
            o.push_str(&cuts.join("\n"));
            o.push('\n');
        }
        let notes: Vec<String> = rec
            .kb
            .iter()
            .flat_map(|i| i.notes.iter().map(|n| format!("- {n}")))
            .take(20)
            .collect();
        if !notes.is_empty() {
            o.push_str("\n检索过程中的异常/降级：\n");
            o.push_str(&notes.join("\n"));
            o.push('\n');
        }
    }

    o.push_str("\n---\n\n> 本文件由 `harness context` 生成：**只汇总证据，不做猜测**。\n");
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "dh-obs-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn cap_marks_how_much_was_dropped() {
        let s = "x".repeat(100);
        let out = cap(&s, 10);
        assert!(out.starts_with("xxxxxxxxxx"));
        assert!(out.contains("[截断"), "必须留下痕迹：{out}");
        assert_eq!(cap("short", 10), "short", "没超限就不该加标记");
    }

    #[test]
    fn redact_masks_key_shaped_strings() {
        let out = redact("key=sk-abcdefghijklmnopqrstuvwxyz tail", &[]);
        assert!(!out.contains("abcdefghijklmnop"), "{out}");
        assert!(out.contains("[REDACTED]"), "{out}");
    }

    #[test]
    fn redact_masks_bearer_tokens() {
        let out = redact("Authorization: Bearer abcdefghijklmnop", &[]);
        assert!(!out.contains("abcdefghijklmnop"), "{out}");
        assert!(out.contains("Bearer [REDACTED]"), "{out}");
    }

    #[test]
    fn redact_masks_the_actual_configured_key_by_exact_match() {
        // 形态匹配只认已知形态；自己那把钥匙靠精确替换兜底 —— 两条路都要走
        let key = "my-very-odd-format-key-1234567890".to_string();
        let out = redact(&format!("使用 {key} 访问"), &[key.clone()]);
        assert!(!out.contains(&key), "{out}");
        assert!(out.contains("[REDACTED]"), "{out}");
    }

    #[test]
    fn short_looking_words_are_not_mangled() {
        // 别把普通文本打成马赛克（误伤会让日志不可读）
        let out = redact("sk-短 和 Bearer 小", &[]);
        assert!(out.contains("sk-短"), "{out}");
        assert!(out.contains("Bearer 小"), "{out}");
    }

    /// 全局单例的测试必须串行 —— `cargo test` 默认多线程，两个测试同时
    /// install/uninstall 会互相踩（这就是当初把它设计成"测试里用实例 API"的原因）。
    static GLOBAL_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn global_tracer_receives_stage_and_log_calls() {
        let _g = GLOBAL_LOCK.lock().unwrap();
        let d = tmp("global");
        install(Tracer::new(&d, "rg", vec![]));
        stage_open("plan", attrs(&[("k", "v")]));
        stage_close("plan", "done", BTreeMap::new());
        log("info", "plan", "走全局路径");
        assert!(current().is_some());
        assert_eq!(current_dir().unwrap(), d);
        uninstall();
        let spans = read_spans(&d).unwrap();
        assert_eq!(spans.len(), 1);
        assert!(
            read_logs(&d, None)
                .unwrap()
                .iter()
                .any(|r| r.msg.contains("全局路径"))
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn no_tracer_means_everything_is_a_noop() {
        let _g = GLOBAL_LOCK.lock().unwrap();
        uninstall();
        // 这些必须不 panic、不写任何东西
        log("info", "test", "不该被记录");
        stage_open("t", attrs(&[("a", "b")]));
        stage_close("t", "done", BTreeMap::new());
        span_once("cmd", "x", "ok", 0, BTreeMap::new());
        assert!(current().is_none());
        assert!(current_dir().is_none());
    }

    #[test]
    fn spans_merge_into_durations_and_unclosed_ones_are_visible() {
        let d = tmp("span");
        // 直接测实例 API：全局单例在并行测试里会互相覆盖（`cargo test` 多线程跑）
        let t = Tracer::new(&d, "r1", vec![]);
        t.span_open("stage", "generate", attrs(&[("task", "写个模块")]));
        std::thread::sleep(std::time::Duration::from_millis(15));
        t.span_close("generate", "done", attrs(&[("files", "2")]));
        t.span_open("stage", "verify", BTreeMap::new()); // 故意不闭合 = 崩溃点

        let spans = read_spans(&d).unwrap();
        assert_eq!(spans.len(), 2, "{spans:?}");
        let g = spans.iter().find(|s| s.name == "generate").unwrap();
        assert_eq!(g.status, "done");
        assert!(g.duration_ms >= 10, "耗时要真实：{:?}", g.duration_ms);
        assert_eq!(g.attrs.get("files").map(|s| s.as_str()), Some("2"));
        let v = spans.iter().find(|s| s.name == "verify").unwrap();
        assert!(v.unclosed, "没闭合的 span 必须能看出是崩溃点");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn logs_are_persisted_and_filterable_by_level() {
        let d = tmp("logs");
        let t = Tracer::new(&d, "r2", vec![]);
        t.log("info", "plan", "开始");
        t.log("warn", "lint", "有点问题");
        t.log("error", "verify", "挂了");

        assert_eq!(read_logs(&d, None).unwrap().len(), 3);
        let warns = read_logs(&d, Some("warn")).unwrap();
        assert_eq!(warns.len(), 2, "{warns:?}");
        assert!(
            warns
                .iter()
                .all(|r| r.level == "warn" || r.level == "error")
        );
        let errs = read_logs(&d, Some("error")).unwrap();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].stage, "verify");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn secrets_never_reach_the_files() {
        // 这是"脱敏在写入层做"的兑现：调用方忘了脱敏也不会漏
        let d = tmp("secret");
        let key = "sk-supersecretkey1234567890".to_string();
        let t = Tracer::new(&d, "r3", vec![key.clone()]);
        t.log("info", "llm", &format!("调用时带了 {key}"));
        t.span_once(
            "llm",
            "plan",
            "ok",
            0,
            attrs(&[("auth", &format!("Bearer {key}"))]),
        );

        let logs = std::fs::read_to_string(d.join("logs.jsonl")).unwrap();
        let trace = std::fs::read_to_string(d.join("trace.jsonl")).unwrap();
        assert!(!logs.contains("supersecret"), "日志里不许出现原文：{logs}");
        assert!(
            !trace.contains("supersecret"),
            "trace 里不许出现原文：{trace}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn timeline_marks_status_and_unclosed() {
        let d = tmp("tl");
        let t = Tracer::new(&d, "r4", vec![]);
        t.span_open("stage", "plan", BTreeMap::new());
        t.span_close("plan", "done", BTreeMap::new());
        t.span_open("stage", "generate", BTreeMap::new());
        let tl = render_timeline(&read_spans(&d).unwrap());
        assert!(tl.contains('✓'), "{tl}");
        assert!(tl.contains("未结束"), "{tl}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
