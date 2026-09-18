//! 本地知识库：把「项目自己的常识」喂进上下文。
//!
//! ## 这一版真正的难点不在检索，在上下文
//!
//! 注入是零和的：检索进来的每一段都挤掉别的东西（可能是它正要写的文件，也可能是
//! 它自己刚输出的规划）。所以本模块的设计主线是**预算与优先级**，不是召回率：
//!
//! 1. **必须有显式预算**，且"检索到的知识"是**最先被裁**的那一档（见 `retrieve`）；
//! 2. **"相关"不等于"有用"**：同一份文档相邻的 chunk 会一起上榜，于是 top-5 是同一段话
//!    的五份拷贝 —— 花了 5 份预算只换来 1 份信息。所以有去重 + MMR 式多样性；
//! 3. **上下文腐烂**：塞得越多，模型对中间内容的注意力越差。所以默认 top-k 很小（3~5），
//!    并按预算而不是条数截断；
//! 4. **工作区当前内容 > 知识库里的历史副本**（硬规则，命中工作区文件的片段一律不注入）；
//! 5. **检索回来的是数据，不是指令**：注入块有明确边界、每条标来源与信任级别，
//!    且**只进 user 段落，绝不进 system prompt**；
//! 6. **注入必须可观测**：注入了什么、多少字符、命中哪些 chunk、为什么被裁，全部进 trace
//!    与 `run.json`（`KbInjection`）。
//!
//! ## Provider 抽象
//!
//! 检索层只认 [`KbDoc`]，"知识从哪来"（本地目录 / 以后的 git、飞书、语雀）与
//! "怎么检索、怎么注入"彻底解耦。所以 `kind` 从现在就是枚举，前期只实现 `local`。
//!
//! ## 两个实现取舍（与需求文档的偏差，已在文档里记明）
//!
//! - 索引用 SQLite FTS5，因此引入了 `rusqlite`（bundled）——Rust 没有 Python 那种
//!   标准库 `sqlite3`，"零依赖"在 Rust 侧只能换成"依赖可编译、永远能跑"；
//! - 中文分词用**二元切分（bigram）**而不是 jieba：纯 Rust、确定性强、可单测，
//!   且查询是"规划步骤文本"这类用词与文档高度重合的短句，bigram 的精度够用。
//!   分词只在一个函数里（`index::tokenize`），换 jieba-rs 是局部改动。

pub mod cli;
// `commands`（AppHandle 包装层）不随引擎迁入：ruyix 侧由 src-tauri/src/agent/ 桥承接（融合计划 D1）
pub mod index;
pub mod local;
pub mod retrieve;

pub use crate::config::KbConfig;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub use crate::exec::CancelFlag;

/// 一份被索引的文档（Provider 无关）。
///
/// 比需求文档 §9.3 的字段多了 `mtime_ms` / `size`：增量索引与"陈旧检测"都需要它们，
/// 而且必须是**索引当时**的值 —— 否则没法回答"源文件有没有变过"。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct KbDoc {
    /// 相对来源根的路径（posix 分隔符）
    pub path: String,
    pub title: String,
    pub text: String,
    pub fetched_at: String,
    /// 内容版本：local 是内容 hash，git 是 commit
    pub rev: String,
    pub mtime_ms: u64,
    pub size: u64,
}

/// 一次拉取的结果。
///
/// 比需求文档 §9.3 的 `Vec<KbDoc>` 多带了 `skipped`：**"哪些文件没进库、为什么"
/// 必须能被说出来**，否则检索质量出问题时第一个要问的东西就丢了。
/// （`fetch` 的返回值因此从 `Vec<KbDoc>` 变成 `KbScan`——这是刻意的偏差。）
#[derive(Debug, Default)]
pub struct KbScan {
    pub docs: Vec<KbDoc>,
    pub skipped: Vec<(String, String)>,
    pub cancelled: bool,
}

/// 来源抽象：local 是遍历目录，将来的 git/feishu/yuque 是拉取。
///
/// 检索层只认 [`KbDoc`]，所以"知识从哪来"与"怎么检索、怎么注入"彻底解耦。
/// 索引层需要的两个动作都放这里（`fetch` 读内容、`walk_stats` 只 stat），
/// 远端 provider 把 `walk_stats` 实现成"拉元数据列表"即可，不用改索引层。
pub trait KbSource {
    fn kind(&self) -> &'static str;
    fn label(&self) -> String;
    /// 拉取/扫描出文档列表（含被跳过项与理由）。`cancel` 置位后应尽快返回已拿到的部分。
    fn fetch(&self, cancel: &CancelFlag) -> Result<KbScan, String>;
    /// 只做遍历与过滤（不读内容）：陈旧检测与规模统计用它。
    fn walk_stats(
        &self,
        cancel: &CancelFlag,
    ) -> Result<(Vec<local::FileStat>, Vec<(String, String)>), String>;
}

/// 按 `kind` 造来源。**这里就是"Provider 路线图"的接口**：
/// 前期只有 `local`，加 git/feishu/yuque 时在这里多一个分支 + 一个新 provider。
pub fn source_for(entry: &KbEntry, cfg: &KbConfig) -> Result<Box<dyn KbSource>, String> {
    match entry.kind.as_str() {
        "local" => Ok(Box::new(local::LocalSource::new(
            Path::new(&entry.path),
            &entry.label,
            cfg.max_file_bytes,
        ))),
        other => Err(format!(
            "暂不支持的来源类型：{other}（v0.6 只实现了 local）"
        )),
    }
}

// ---------------------------------------------------------------- 信任级别

/// 注入内容的信任级别（需求 §2.5）。
///
/// 检索回来的文本是**数据不是指令**：知识库文档里完全可能写着"忽略之前的指令"。
/// 级别既是给模型看的提示，也是排序的 tie-breaker。
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    /// 工作区里的当前代码（最高：它就是"当下的事实"）
    Workspace,
    /// 项目文档（仓库内）
    ProjectDoc,
    /// 个人笔记
    Note,
    /// 其它外部文本
    External,
}

impl Trust {
    pub fn level(self) -> u8 {
        match self {
            Trust::Workspace => 3,
            Trust::ProjectDoc => 2,
            Trust::Note => 1,
            Trust::External => 0,
        }
    }

    /// 注入块里显示的名字（中文直接写死：注入文本是给模型看的中文 prompt，
    /// 不跟着 UI 语言走 —— 否则同一份 prompt 有两种形态，trace 会更难比对）。
    pub fn label(self) -> &'static str {
        match self {
            Trust::Workspace => "工作区代码",
            Trust::ProjectDoc => "项目文档",
            Trust::Note => "笔记",
            Trust::External => "外部",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Trust::Workspace => "workspace",
            Trust::ProjectDoc => "project_doc",
            Trust::Note => "note",
            Trust::External => "external",
        }
    }
}

/// 按来源根的"指纹"判级别：有没有 `.git`/包清单（项目）、有没有 `.obsidian`（笔记库）。
///
/// 判不准就落到 `External` —— 信任级别宁可保守。
///
/// 默认值也刻意选 `External`：信任级别是**安全属性**，没判出来时不能默认给高信任。
impl Default for Trust {
    fn default() -> Self {
        Trust::External
    }
}

pub fn classify_source(root: &Path) -> Trust {
    let has = |names: &[&str]| names.iter().any(|n| root.join(n).exists());
    if has(&[".obsidian"]) {
        return Trust::Note;
    }
    if has(&[
        ".git",
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "pom.xml",
    ]) {
        return Trust::ProjectDoc;
    }
    Trust::External
}

// ---------------------------------------------------------------- 注册表

/// 一条来源注册记录（`kb.json`）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct KbEntry {
    pub id: String,
    /// 前期只有 `local`（枚举定死，后期加 git/feishu/yuque 不改这个字段）
    pub kind: String,
    pub path: String,
    pub label: String,
    pub added_at: String,
    /// 索引统计（来自 SQLite，缓存在这里是为了 GUI 不必每次开库）
    #[serde(default)]
    pub doc_count: usize,
    #[serde(default)]
    pub chunk_count: usize,
    /// 最近一次索引完成时间（空 = 从未索引成功过）
    #[serde(default)]
    pub indexed_at: String,
    #[serde(default)]
    pub index_files: usize,
    /// 上次索引时被内容 hash 跳过的文件数（增量的证据）
    #[serde(default)]
    pub skipped_files: usize,
    #[serde(default)]
    pub elapsed_ms: u128,
}

/// Windows 的 `canonicalize` 返回 `\\?\D:\...` 前缀：功能上没问题，
/// 但注册表与 UI 里显示的是**给人看的路径**，得先擦干净。
pub fn tidy_path(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => p.to_path_buf(),
    }
}

/// 来源 id：稳定、可读、由路径唯一决定（同一目录重复添加不会产生第二份索引）。
///
/// 路径先过 [`tidy_path`] 再算：否则 `\\?\D:\x` 与 `D:\x` 会算出两个 id，
/// 于是"同一个目录"在列表里出现两次、各自带一份索引。
pub fn entry_id(kind: &str, path: &Path) -> String {
    let tidy = tidy_path(path);
    let name = tidy
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string());
    let slug = crate::plan::slugify(&name);
    let h = index::content_hash(tidy.to_string_lossy().to_lowercase().as_bytes());
    format!("{kind}-{slug}-{}", &h[..8])
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Registry {
    pub entries: Vec<KbEntry>,
}

impl Registry {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join("kb.json")
    }

    /// 读注册表。文件不存在 = 空注册表（不是错误）；**解析失败要报错** ——
    /// 用户手改坏了必须能看见，而不是"悄悄变成没有知识库"。
    pub fn load(dir: &Path) -> Result<Registry, String> {
        let path = Self::path(dir);
        if !path.exists() {
            return Ok(Registry::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("读取知识库注册表失败 {}: {e}", path.display()))?;
        serde_json::from_str::<Registry>(&text)
            .map_err(|e| format!("解析知识库注册表失败 {}: {e}", path.display()))
    }

    pub fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建知识库目录失败: {e}"))?;
        let path = Self::path(dir);
        let text =
            serde_json::to_string_pretty(self).map_err(|e| format!("序列化注册表失败: {e}"))?;
        std::fs::write(&path, text).map_err(|e| format!("写入注册表失败 {}: {e}", path.display()))
    }

    pub fn find(&self, id: &str) -> Option<&KbEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// 按路径去重（Windows 路径大小写不敏感，统一小写比较）。
    pub fn find_by_path(&self, path: &Path) -> Option<&KbEntry> {
        let key = norm_path_key(path);
        self.entries.iter().find(|e| norm_path_key(Path::new(&e.path)) == key)
    }

    /// 加入（同路径覆盖）并落盘。
    pub fn upsert(&mut self, entry: KbEntry, dir: &Path) -> Result<(), String> {
        let key = norm_path_key(Path::new(&entry.path));
        match self
            .entries
            .iter()
            .position(|e| norm_path_key(Path::new(&e.path)) == key)
        {
            Some(i) => self.entries[i] = entry,
            None => self.entries.push(entry),
        }
        self.save(dir)
    }

    pub fn remove(&mut self, id: &str, dir: &Path) -> Result<Option<KbEntry>, String> {
        let hit = self.entries.iter().position(|e| e.id == id);
        let removed = hit.map(|i| self.entries.remove(i));
        if removed.is_some() {
            self.save(dir)?;
        }
        Ok(removed)
    }
}

fn norm_path_key(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/").to_lowercase()
}

// ---------------------------------------------------------------- Engine

/// 一次检索的运行环境：配置 + 生效来源 + 不可用原因。
///
/// 刻意**不缓存 sqlite 连接**：检索一次开一次库（毫秒级），换来的是
/// "索引进程不会和检索进程抢文件锁"（本机在 Code-Rag 上被 qdrant_local 的锁坑过）。
#[derive(Clone, Debug)]
pub struct Engine {
    pub cfg: KbConfig,
    pub dir: PathBuf,
    pub enabled: bool,
    /// 生效来源：注册表 + 配置里的 `roots`（按路径去重）
    pub sources: Vec<KbEntry>,
    /// 不可用/降级原因（空 = 一切正常）。**不静默降级**：这个字段一定会进 UI 与 trace。
    pub reason: String,
}

impl Engine {
    pub fn from_config(cfg: &crate::config::AppConfig) -> Engine {
        let dir = crate::config::kb_dir(&cfg.kb);
        let mut reason = String::new();

        let mut sources = match Registry::load(&dir) {
            Ok(r) => r.entries,
            Err(e) => {
                reason = format!("注册表不可用：{e}");
                Vec::new()
            }
        };
        let known: Vec<String> = sources
            .iter()
            .map(|e| norm_path_key(Path::new(&e.path)))
            .collect();
        for raw in &cfg.kb.roots {
            let p = Path::new(raw.trim());
            if raw.trim().is_empty() || known.contains(&norm_path_key(p)) {
                continue;
            }
            sources.push(ephemeral_entry(p));
        }

        let enabled = cfg.kb.enabled;
        if !enabled {
            reason = "知识库未启用（config.toml 的 [kb] enabled = false）".into();
        } else if sources.is_empty() {
            reason = "没有添加任何知识库来源".into();
        }

        Engine {
            cfg: cfg.kb.clone(),
            dir,
            enabled,
            sources,
            reason,
        }
    }

    /// 能真的去检索吗（enabled 且有来源）。注意：**不代表有命中**。
    pub fn available(&self) -> bool {
        self.enabled && !self.sources.is_empty()
    }

    /// 给 GUI 的视图（含每个来源的陈旧检测）。
    pub fn status(&self, with_stale: bool) -> KbStatus {
        let mut sources = Vec::new();
        for e in &self.sources {
            let stats = index::stats(&self.dir, &e.id).unwrap_or_default();
            let mut view = KbSourceView {
                id: e.id.clone(),
                kind: e.kind.clone(),
                path: e.path.clone(),
                label: e.label.clone(),
                added_at: e.added_at.clone(),
                exists: Path::new(&e.path).is_dir(),
                doc_count: stats.doc_count,
                chunk_count: stats.chunk_count,
                indexed_at: if stats.indexed_at.is_empty() {
                    e.indexed_at.clone()
                } else {
                    stats.indexed_at.clone()
                },
                index_files: e.index_files,
                skipped_files: e.skipped_files,
                from_config: e.id.starts_with("cfg-"),
                ..Default::default()
            };
            if with_stale {
                view.stale = index::quick_stale(&self.dir, e, &self.cfg);
                view.stale_flag = view.stale.is_stale();
            }
            sources.push(view);
        }
        KbStatus {
            enabled: self.enabled,
            available: self.available(),
            reason: self.reason.clone(),
            dir: self.dir.to_string_lossy().to_string(),
            top_k: self.cfg.top_k,
            token_budget: self.cfg.token_budget,
            per_source_limit: self.cfg.per_source_limit,
            min_score: self.cfg.min_score,
            sources,
        }
    }
}

/// `config.roots` 里的来源：与注册表条目同构，但**不落盘**（用户没在 GUI 里加它）。
pub fn ephemeral_entry(path: &Path) -> KbEntry {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    let id = format!(
        "cfg-{}",
        &entry_id("local", path)[6..] // local-<slug>-<hash8> → cfg-<slug>-<hash8>
    );
    KbEntry {
        id,
        kind: "local".into(),
        path: path.to_string_lossy().to_string(),
        label: name,
        added_at: String::new(),
        ..Default::default()
    }
}

// ---------------------------------------------------------------- GUI 视图

#[derive(Serialize, Clone, Debug, Default)]
pub struct KbStatus {
    pub enabled: bool,
    pub available: bool,
    pub reason: String,
    pub dir: String,
    pub top_k: usize,
    pub token_budget: usize,
    pub per_source_limit: usize,
    pub min_score: f64,
    pub sources: Vec<KbSourceView>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct KbSourceView {
    pub id: String,
    pub kind: String,
    pub path: String,
    pub label: String,
    pub added_at: String,
    /// 来源来自配置而不是 GUI 注册表
    pub from_config: bool,
    /// 来源目录还在不在（目录被删/移动必须显式说出来）
    pub exists: bool,
    pub doc_count: usize,
    pub chunk_count: usize,
    pub indexed_at: String,
    pub index_files: usize,
    pub skipped_files: usize,
    /// `stale` 的布尔版（UI 上一个红/绿点就够，不必自己解析文案）
    pub stale_flag: bool,
    pub stale: StaleView,
}

/// 陈旧检测结果。
///
/// 诚实边界：这是**基于 mtime + size 的快速扫描**（只 stat，不读内容），
/// 所以它只回答"看起来变了几个文件"；真正决定要不要重建的是内容 hash（`kb_reindex`）。
#[derive(Serialize, Clone, Debug, Default)]
pub struct StaleView {
    pub checked: bool,
    pub changed: usize,
    pub missing: usize,
    pub added: usize,
    pub note: String,
    pub detail: Vec<String>,
}

impl StaleView {
    pub fn is_stale(&self) -> bool {
        self.checked && (self.changed + self.missing + self.added) > 0
    }

    pub fn summary(&self) -> String {
        if !self.checked {
            return self.note.clone();
        }
        let n = self.changed + self.missing + self.added;
        if n == 0 {
            "与索引一致".to_string()
        } else {
            format!(
                "已陈旧（{} 个文件有更新 / {} 个已删除 / {} 个新增）",
                self.changed, self.missing, self.added
            )
        }
    }
}
