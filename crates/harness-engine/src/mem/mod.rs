//! **项目记忆**（v1.1 核心模块，不可插件化）：证据无损 + 信念可推翻。
//!
//! 设计来源：`doc/需求-项目记忆-v1.1.md`（对照 MemFold 论文的 EC/BS/BC 三条公理）。
//! 一句话：**同一个存储既当证据又当信念时，三条公理才互相打架** —— 所以这里劈三层：
//!
//! | 层 | 表 | 谁承担 | 规则 |
//! |---|---|---|---|
//! | **账本**（证据） | [`ledger`] 的 `events` | EC：历史一条不丢 | 只追加、哈希链、**永不删**；推翻也是一条事件 |
//! | **折叠**（信念） | `beliefs` | BS：前文可被后文推翻 | 账本前缀的确定性折叠；`beliefs` 只是**可重建的缓存** |
//! | **视图**（注意力） | [`retrieve::block_for_prompt`] + `receipts` | BC：上下文有界 | 有界块；逐出只出视图，且**签发收据** |
//!
//! 两条硬纪律（可证伪，见 `doc/需求-项目记忆-v1.1.md` §7）：
//!
//! 1. **检索先结构性过滤，再打分**：`status != active` 或有效期不覆盖查询时刻的槽位
//!    **不进候选**（不是降权）—— 把被推翻的旧值留在候选里只会制造偶发幻觉；
//! 2. **入账前脱敏**：走 [`Memory::record_obs`] / [`Memory::record_rev`] 的内容一律先过
//!    [`crate::observe::redact`]（账本是**长期**存储，比日志更该脱敏）。
//!
//! 落地位置：`<便携根>/global/memory/mem.db`（**强制随发行包**，用户的仓库里一个字都不写）。
//! 作用域：`scope` = 项目 key（项目级事实）或 [`GLOBAL_SCOPE`]（机器级事实，如工具链路径）。
//!
//! 与 git 的分工（论文对照讨论的结论）：**代码事实归 git**（它本来就有账本与修订代数），
//! 这里只装"仓库记不住的东西" + 引擎自己的决策。

pub mod compact;
pub mod embed;
pub mod fetch;
pub mod fold;
pub mod ledger;
pub mod retrieve;
pub mod tokenize;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

pub use compact::{Compaction, compact_history, record_compaction};
pub use ledger::{Event, EventKind, Origin};
pub use retrieve::{Hit, Receipt};

/// 机器级作用域（工具链、跨项目偏好）；其余 scope 一律是项目 key。
pub const GLOBAL_SCOPE: &str = "_global";

/// 一个未关闭的有效期用 `None` 表示（"至今"）。
pub type Interval = (Option<i64>, Option<i64>);

pub const SCHEMA: &str = r#"
PRAGMA journal_mode=WAL;
PRAGMA foreign_keys=ON;

-- 账本：只追加，永不更新/删除。hash 链 = 前一条的 hash + 本条内容（防篡改，可选但便宜）
CREATE TABLE IF NOT EXISTS events (
  seq        INTEGER PRIMARY KEY AUTOINCREMENT,
  id         TEXT    NOT NULL UNIQUE,
  ts         INTEGER NOT NULL,               -- 事务时间：引擎何时得知
  scope      TEXT    NOT NULL,
  kind       TEXT    NOT NULL,               -- obs | rev
  key        TEXT,                           -- 槽位键
  value      TEXT,
  valid_from INTEGER,                        -- 有效时间：内容何时在世界成立
  valid_to   INTEGER,                        -- NULL = 至今
  op         TEXT,                           -- rev 算子：supersede|correct|refine|merge|split|contested
  prov       TEXT    NOT NULL DEFAULT '[]',  -- 依据事件 id（JSON 数组）
  reason     TEXT    NOT NULL DEFAULT '',
  origin     TEXT    NOT NULL,               -- probe | agent | human | engine
  prev_hash  TEXT    NOT NULL,
  hash       TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS events_scope_key ON events(scope, key, seq);

-- 折叠结果（**可丢弃、可重建**；删掉它 EC 不受影响，重放账本即可回来）
CREATE TABLE IF NOT EXISTS beliefs (
  scope      TEXT    NOT NULL,
  key        TEXT    NOT NULL,
  valid_from INTEGER NOT NULL,               -- 默认取事件时间（世界时间未知时）
  valid_to   INTEGER,                        -- NULL = 至今
  value      TEXT    NOT NULL,
  status     TEXT    NOT NULL,               -- active | superseded | revoked | contested
  prov       TEXT    NOT NULL DEFAULT '[]',
  revs       TEXT    NOT NULL DEFAULT '[]',  -- 修订链：事件 id 数组
  obs_id     TEXT    NOT NULL,
  updated_ts INTEGER NOT NULL,
  PRIMARY KEY (scope, key, valid_from, obs_id)
);
CREATE INDEX IF NOT EXISTS beliefs_now ON beliefs(scope, status, valid_from, valid_to);

-- 检索用的视图（视图之二：可丢弃，重建走 rebuild_index）
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(scope UNINDEXED, row_key UNINDEXED, text);

-- 向量（同样可丢弃；模型不在时这张表空着，检索自动退化为纯词法）
CREATE TABLE IF NOT EXISTS vectors (
  row_key TEXT PRIMARY KEY,
  dim     INTEGER NOT NULL,
  vec     BLOB    NOT NULL
);

-- 收据：每次压缩/逐出"丢了什么、怎么换回来"
CREATE TABLE IF NOT EXISTS receipts (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  ts         INTEGER NOT NULL,
  scope      TEXT    NOT NULL,
  kind       TEXT    NOT NULL,               -- prompt_block | compact | prune | reindex
  covered    TEXT    NOT NULL DEFAULT '[]',  -- 覆盖了哪些事件/信念
  dropped    TEXT    NOT NULL DEFAULT '[]',  -- 离开了视野的内容（或路径）
  rehydrate  TEXT    NOT NULL DEFAULT '',    -- 怎么换回来（路径 / 事件 id 前缀）
  note       TEXT    NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS receipts_scope ON receipts(scope, ts);

CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
"#;

/// 记忆库。**每次调用开一个连接**：SQLite 自己管文件锁（WAL），
/// 省掉 `Connection` 不是 `Send`/`Sync` 带来的满仓 `Mutex` 与生命周期开销。
#[derive(Debug, Clone)]
pub struct Memory {
    path: PathBuf,
    secrets: Vec<String>,
}

impl Memory {
    /// 打开（不存在则建目录 + 建表）。`secrets` 是入账前要抹掉的串（API key 等）。
    pub fn open(path: impl AsRef<Path>, secrets: Vec<String>) -> Result<Self, String> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("建记忆目录失败 {}: {e}", dir.display()))?;
        }
        let m = Memory { path, secrets };
        {
            let conn = m.conn()?;
            conn.execute_batch(SCHEMA)
                .map_err(|e| format!("建记忆表失败: {e}"))?;
        }
        Ok(m)
    }

    /// 只读打开（不建表）：给"文件还不存在时也别报错"的探查路径用。
    pub fn exists(path: impl AsRef<Path>) -> bool {
        path.as_ref().is_file()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn conn(&self) -> Result<rusqlite::Connection, String> {
        let c = rusqlite::Connection::open(&self.path)
            .map_err(|e| format!("打开记忆库失败 {}: {e}", self.path.display()))?;
        c.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        Ok(c)
    }

    /// 入账前脱敏（复用 observe 的写入层口径，见 `doc/需求-项目记忆-v1.1.md` §8 待拍板③）。
    pub(crate) fn redact(&self, s: &str) -> String {
        crate::observe::redact(s, &self.secrets)
    }

    // ---------------------------------------------------------------- 写：观察

    /// 记一条**观察**（原始证据）。`key` 为空表示这是一条"无槽位"的纯证据（如工具输出）。
    ///
    /// 这是唯一该被用来"告诉记忆库一件事"的入口：它先脱敏、再落账本、然后折叠。
    pub fn record_obs(
        &self,
        scope: &str,
        key: &str,
        value: &str,
        origin: Origin,
        prov: &[String],
        valid_from: Option<i64>,
    ) -> Result<Event, String> {
        let value = self.redact(value);
        let ev = ledger::append_obs(self, scope, key, &value, origin, prov, valid_from)?;
        if !key.trim().is_empty() {
            fold::apply(self, &ev)?;
        }
        Ok(ev)
    }

    /// 记一条**修订决策**（一等公民）。`op` 是 [`fold::Op`] 的字符串形式。
    ///
    /// **决策本身就是记录**（裁决器非确定 ⇒ 重放必须靠它）：所以 `reason` 与 `prov` 是必填语义，
    /// 哪怕 `prov` 为空也要写空数组，而不是省略。
    #[allow(clippy::too_many_arguments)]
    pub fn record_rev(
        &self,
        scope: &str,
        op: fold::Op,
        key: &str,
        value: Option<&str>,
        prov: &[String],
        reason: &str,
        at: Option<i64>,
    ) -> Result<Event, String> {
        let value = value.map(|v| self.redact(v));
        let reason = self.redact(reason);
        let ev = ledger::append_rev(self, scope, op, key, value.as_deref(), prov, &reason, at)?;
        fold::apply(self, &ev)?;
        Ok(ev)
    }

    // ---------------------------------------------------------------- 读：折叠 + 检索

    /// 当前信念（`now`）：先**结构性过滤**（active + 有效期覆盖 `at`），再打分。
    ///
    /// `query` 为空 = 只按效用（频次/最近/重要）取前 `limit` 条 —— 用于组装注入块。
    pub fn beliefs_now(
        &self,
        scope: &str,
        query: &str,
        limit: usize,
        at: Option<i64>,
    ) -> Result<Vec<Hit>, String> {
        retrieve::now(self, scope, query, limit, at)
    }

    /// 某时刻的信念（`as-of`）：**包含**当时成立、后来被推翻的槽位。
    pub fn beliefs_as_of(
        &self,
        scope: &str,
        key: Option<&str>,
        at: i64,
    ) -> Result<Vec<Hit>, String> {
        retrieve::as_of(self, scope, key, at)
    }

    /// 一条信念的完整修订链与依据（`why`）：一次调用回答"你凭什么这么认为"。
    pub fn why(&self, scope: &str, key: &str) -> Result<Vec<Event>, String> {
        retrieve::why(self, scope, key)
    }

    /// 组装注入给模型的**有界**信念块；超预算的部分**离开视野并签发收据**（不删）。
    pub fn block_for_prompt(
        &self,
        scope: &str,
        budget_chars: usize,
    ) -> Result<Option<String>, String> {
        retrieve::block_for_prompt(self, scope, budget_chars)
    }

    /// 收据：每次压缩/逐出丢了什么、怎么换回来（`loss accounting`）。
    pub fn receipts(&self, scope: &str, limit: usize) -> Result<Vec<Receipt>, String> {
        retrieve::receipts(self, scope, limit)
    }

    pub fn add_receipt(
        &self,
        scope: &str,
        kind: &str,
        covered: &[String],
        dropped: &[String],
        rehydrate: &str,
        note: &str,
    ) -> Result<(), String> {
        retrieve::add_receipt(self, scope, kind, covered, dropped, rehydrate, note)
    }

    /// 从账本重建折叠（`beliefs` + FTS + 向量）—— 证明 EC：**删掉派生层也不丢东西**。
    pub fn rebuild(&self, scope: Option<&str>) -> Result<usize, String> {
        fold::rebuild(self, scope)
    }

    /// 统计（给面板/自检用）。
    pub fn stats(&self, scope: &str) -> Result<Stats, String> {
        let conn = self.conn()?;
        let count = |sql: &str| -> Result<i64, String> {
            conn.query_row(sql, rusqlite::params![scope], |r| r.get::<_, i64>(0))
                .map_err(|e| format!("统计失败: {e}"))
        };
        Ok(Stats {
            events: count("SELECT COUNT(*) FROM events WHERE scope=?")?,
            beliefs_active: count("SELECT COUNT(*) FROM beliefs WHERE scope=? AND status='active'")?,
            beliefs_all: count("SELECT COUNT(*) FROM beliefs WHERE scope=?")?,
            receipts: count("SELECT COUNT(*) FROM receipts WHERE scope=?")?,
            vectors: conn
                .query_row(
                    "SELECT COUNT(*) FROM vectors v JOIN beliefs b ON b.scope||'|'||b.key||'|'||b.valid_from = v.row_key WHERE b.scope=?",
                    rusqlite::params![scope],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0),
        })
    }
}

/// 进程级安装点：宿主启动时把记忆库交进来（引擎不认识便携根）。
///
/// 为什么用进程级而不是层层传参：记忆是**横切**能力（主循环要用、step 子 agent 要用、
/// 宿主命令要用），把它塞进每个签名只会让调用链记住一个与业务无关的路径。
static INSTALLED: std::sync::OnceLock<Memory> = std::sync::OnceLock::new();
static SCOPE: std::sync::OnceLock<std::sync::RwLock<String>> = std::sync::OnceLock::new();

/// 注入块预算（字符）。**故意是常量而不是配置项**：先把它跑出真实收益，再决定要不要
/// 变成用户可调的旋钮 —— 现在就加配置键等于把一个还没被用过的参数固化进配置界面。
pub const PROMPT_BLOCK_BUDGET_CHARS: usize = 1200;

/// 宿主启动时调用一次。重复调用是**无害**的（后到的不覆盖，返回 Err 说明清楚）。
pub fn install(m: Memory) -> Result<(), String> {
    INSTALLED
        .set(m)
        .map_err(|_| "记忆库已经安装过了（install 只该在启动时调一次）".to_string())
}

pub fn current() -> Option<&'static Memory> {
    INSTALLED.get()
}

/// 当前项目作用域（宿主在切换项目 / 开跑前设置）。默认机器级。
pub fn set_scope(s: &str) {
    let lock = SCOPE.get_or_init(|| std::sync::RwLock::new(GLOBAL_SCOPE.to_string()));
    if let Ok(mut g) = lock.write() {
        *g = if s.trim().is_empty() {
            GLOBAL_SCOPE.to_string()
        } else {
            s.to_string()
        };
    }
}

pub fn scope() -> String {
    SCOPE
        .get_or_init(|| std::sync::RwLock::new(GLOBAL_SCOPE.to_string()))
        .read()
        .map(|g| g.clone())
        .unwrap_or_else(|_| GLOBAL_SCOPE.to_string())
}

/// 给首条用户消息用的有界信念块（项目作用域 + 机器级事实）。任何错误都**降级为 None**：
/// 记忆是辅助，不该有能力把一次 run 弄挂。
pub fn prompt_block_for_current() -> Option<String> {
    let m = current()?;
    let sc = scope();
    let mut out = m
        .block_for_prompt(&sc, PROMPT_BLOCK_BUDGET_CHARS)
        .ok()
        .flatten();
    if let Ok(Some(g)) = m.block_for_prompt(GLOBAL_SCOPE, PROMPT_BLOCK_BUDGET_CHARS / 2) {
        out = Some(match out {
            Some(s) => format!("{s}\n{g}"),
            None => g,
        });
    }
    // 有压实就直说：收据在账本里，说的是"注意力丢了什么"（切片 3）
    if let Some(n) = compact::compaction_note(m, &sc) {
        out = Some(match out {
            Some(s) => format!("{s}\n\n{n}"),
            None => n,
        });
    }
    out
}

/// 把一次命令发现的结论**记成观察**（origin = probe：探针自己就是证据）。
///
/// 这是 `discover` 的升级点：以前它是 300 秒 TTL 缓存（过期即遗忘），现在是账本里的信念 ——
/// **可解释**（为什么我认为 mvn 在那个路径）、**可复验**（重探一次就有新证据）、
/// **可被推翻**（装到别处就 supersede 掉旧的，而旧路径仍能按时间点查回来）。
pub fn record_discovery(tools: &[crate::discover::Found]) {
    let Some(m) = current() else {
        return;
    };
    for t in tools {
        if t.name.trim().is_empty() {
            continue;
        }
        let value = if t.available {
            let v = t.version.trim();
            if t.path.trim().is_empty() {
                if v.is_empty() {
                    "(可用)".to_string()
                } else {
                    v.to_string()
                }
            } else if v.is_empty() {
                t.path.clone()
            } else {
                format!("{} ({})", t.path, v.chars().take(60).collect::<String>())
            }
        } else {
            "未安装".to_string()
        };
        // 失败不打扰 run：记忆坏了不该让一次 run 挂掉
        let _ = m.record_obs(
            GLOBAL_SCOPE,
            &format!("tool.{}", t.name),
            &value,
            ledger::Origin::Probe,
            &[],
            None,
        );
    }
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub events: i64,
    pub beliefs_active: i64,
    pub beliefs_all: i64,
    pub receipts: i64,
    pub vectors: i64,
}
