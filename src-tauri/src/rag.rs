//! RAG (检索增强生成) 模块
//!
//! 架构：
//!   在线嵌入 API (OpenAI 兼容格式，如 DeepSeek) → 文本编码为向量
//!   qdrant-edge (嵌入式，进程内) → 向量存储与检索
//!
//! 存储路径：
//!   ~/.darkhorse/code/qdrant/     qdrant 数据
//!   ~/.darkhorse/code/rag.toml    全局 RAG 配置（嵌入 API 地址/密钥/维度）
//!   ./.darkhorse/code/rag.toml    项目索引状态

use qdrant_edge::{
    Condition, CountRequest, Distance, EdgeConfig, EdgeShard, EdgeVectorParams, FieldCondition,
    Filter, JsonPath, Match, NamedQuery, Payload, PointId, PointInsertOperations, PointOperations,
    PointStructPersisted, QueryEnum, QueryRequest, ScoringQuery, UpdateOperation, ValueVariants,
    VectorInternal, VectorPersisted, VectorStructPersisted, WithPayloadInterface,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

// ============================================
// 数据结构
// ============================================

/// 搜索结果
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub path: String,
    pub score: f32,
    pub start_line: usize,
    pub end_line: usize,
    pub snippet: String,
}

/// 索引进度
#[derive(Debug, Clone, Serialize)]
pub struct IndexProgress {
    pub phase: String,     // "idle" | "scanning" | "indexing" | "done"
    pub current: usize,
    pub total: usize,
}

/// 索引状态
#[derive(Debug, Clone, Serialize)]
pub struct IndexStatus {
    pub enabled: bool,
    pub api_configured: bool,
    pub api_url: Option<String>,
    pub qdrant_running: bool,
    pub files_indexed: usize,
    pub last_indexed: Option<String>,
    pub progress: Option<IndexProgress>,
    /// 向量库重建说明（旧数据无法加载时非空，提醒用户重新索引）
    pub rebuild_note: Option<String>,
}

/// 项目 rag.toml 配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectRagConfig {
    pub enabled: bool,
    pub model: String,
    pub last_indexed: Option<String>,
    pub files_count: usize,
    #[serde(default)]
    pub files: HashMap<String, String>,
}

/// 全局 rag.toml 配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalRagConfig {
    pub enabled: bool,
    pub permanently_disabled: bool,
    /// 在线嵌入 API 地址（OpenAI 兼容），如 https://api.deepseek.com/v1/embeddings
    pub api_url: Option<String>,
    /// API 密钥（缺省时复用 darkhorse.code.ai.api_key）
    pub api_key: Option<String>,
    /// 嵌入模型名，默认 deepseek-embedding
    pub model: Option<String>,
    /// 向量维度，默认 1024
    pub dim: Option<usize>,
}

// ============================================
// 索引文件扩展名
// ============================================

const DEFAULT_EXTENSIONS: &[&str] = &[
    "java", "js", "ts", "jsx", "tsx", "py", "rs", "go", "c", "cpp", "h", "hpp",
    "cs", "rb", "php", "swift", "kt", "scala", "vue", "svelte", "html", "css",
    "scss", "less", "md", "rst", "txt", "toml", "yaml", "yml", "json", "xml",
    "sql", "sh", "bat", "ps1", "proto", "graphql",
];

const EXCLUDED_DIRS: &[&str] = &[
    "node_modules", ".git", "target", ".venv", "venv", "__pycache__",
    "dist", "build", ".next", "out", "coverage", ".idea", ".vscode",
    ".darkhorse",
];

const EXCLUDED_EXTENSIONS: &[&str] = &[
    "lock", "min.js", "min.css", "map", "pyc", "class", "o", "so",
    "dll", "exe", "bin", "png", "jpg", "jpeg", "gif", "bmp", "ico",
    "svg", "webp", "woff", "woff2", "ttf", "eot", "pdf", "zip",
    "tar", "gz", "bz2", "xz", "7z", "rar",
];

// ============================================
// 嵌入 API 常量
// ============================================

/// 默认嵌入模型名
pub const DEFAULT_EMBED_MODEL: &str = "deepseek-embedding";
/// 默认向量维度
pub const DEFAULT_DIM: usize = 1024;
/// 每批调用嵌入 API 的文件数
const EMBED_BATCH: usize = 10;
/// 单文件最大字符数（超过则跳过，受嵌入 API token 上限约束）
const MAX_FILE_CHARS: usize = 32_000;

// ============================================
// Qdrant 常量与类型
// ============================================

const VECTOR_NAME: &str = "code";

/// 要插入的向量点（与旧 API 兼容）
#[derive(Debug, Clone, Serialize)]
pub struct QdrantPoint {
    pub id: u64,
    pub vector: Vec<f32>,
    pub payload: serde_json::Value,
}

/// 搜索结果命中（与旧 API 兼容）
#[derive(Debug, Clone)]
pub struct QdrantSearchHit {
    pub id: u64,
    pub score: f32,
    pub payload: Option<serde_json::Value>,
}

/// qdrant 启动结果
pub enum StartOutcome {
    /// 正常启动（首次建库，或成功加载旧库）
    Started,
    /// 旧数据无法加载：已自动备份并重建空库，需重新建立索引
    Rebuilt { reason: String },
}

/// rag_reindex 命令返回值
#[derive(Debug, Clone, Serialize)]
pub struct ReindexResult {
    /// 本次索引的文件数
    pub indexed: usize,
    /// 向量库重建说明（旧数据无法加载时非空）
    pub rebuild_note: Option<String>,
}

// ============================================
// QdrantManager — 嵌入式 EdgeShard
// ============================================

pub struct QdrantManager {
    shard: Option<EdgeShard>,
    data_dir: PathBuf,
    dim: usize,
}

impl QdrantManager {
    pub fn new(dim: usize) -> Self {
        let data_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".darkhorse")
            .join("code")
            .join("qdrant");
        Self {
            shard: None,
            data_dir,
            dim,
        }
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    /// 打开或创建嵌入式 shard（懒加载，不使用时占 0 资源）
    pub fn start(&mut self) -> Result<StartOutcome, String> {
        if self.shard.is_some() {
            return Ok(StartOutcome::Started);
        }

        // 维度变化 → 旧向量无法复用，清空数据目录重建（需要重新索引）
        let marker = self.data_dir.join("vector-dim");
        let existing_dim = std::fs::read_to_string(&marker)
            .ok()
            .and_then(|s| s.trim().parse::<usize>().ok());
        if let Some(old) = existing_dim {
            if old != self.dim {
                std::fs::remove_dir_all(&self.data_dir)
                    .map_err(|e| format!("清理旧向量数据失败: {}", e))?;
            }
        }

        let _ = std::fs::create_dir_all(&self.data_dir);

        let config = EdgeConfig {
            vectors: HashMap::from([(
                VECTOR_NAME.to_string(),
                EdgeVectorParams {
                    size: self.dim,
                    distance: Distance::Cosine,
                    on_disk: Some(true),   // mmap 存储，省内存
                    multivector_config: None,
                    datatype: None,
                    quantization_config: None,
                    hnsw_config: None,
                },
            )]),
            ..Default::default()
        };

        // 先尝试加载已有数据，失败时区分两种情况：
        // 1. 无旧数据 → 正常首次建库
        // 2. 有旧数据但加载失败（维度/格式不兼容、损坏等）→ 保留原始错误，
        //    自动备份旧目录后重建空库（向量可从 API 重新生成）
        match EdgeShard::load(&self.data_dir, Some(config.clone())) {
            Ok(s) => {
                self.shard = Some(s);
                let _ = std::fs::write(&marker, self.dim.to_string());
                Ok(StartOutcome::Started)
            }
            Err(load_err) if self.has_old_data() => {
                let backup_dir = self.backup_dir();
                std::fs::rename(&self.data_dir, &backup_dir).map_err(|e| {
                    format!(
                        "旧向量库无法加载（{}），且备份到 {} 失败（{}）。请手动删除 {} 后重试",
                        load_err,
                        backup_dir.display(),
                        e,
                        self.data_dir.display()
                    )
                })?;
                let _ = std::fs::create_dir_all(&self.data_dir);
                self.shard = Some(
                    EdgeShard::new(&self.data_dir, config)
                        .map_err(|e| format!("重建 qdrant shard 失败: {}", e))?,
                );
                let _ = std::fs::write(&marker, self.dim.to_string());
                Ok(StartOutcome::Rebuilt {
                    reason: format!(
                        "旧向量库无法加载（{}），已自动备份到 {} 并重建空库，需要重新建立索引",
                        load_err,
                        backup_dir.display()
                    ),
                })
            }
            Err(_) => {
                // 无旧数据 → 正常首次建库
                self.shard = Some(
                    EdgeShard::new(&self.data_dir, config)
                        .map_err(|e| format!("创建 qdrant shard 失败: {}", e))?,
                );
                let _ = std::fs::write(&marker, self.dim.to_string());
                Ok(StartOutcome::Started)
            }
        }
    }

    /// 数据目录下是否残留旧数据（segments 或 wal 中有任何非隐藏条目）
    fn has_old_data(&self) -> bool {
        ["segments", "wal"].iter().any(|sub| {
            let Ok(entries) = std::fs::read_dir(self.data_dir.join(sub)) else {
                return false;
            };
            entries
                .flatten()
                .any(|e| !e.file_name().to_string_lossy().starts_with('.'))
        })
    }

    /// 旧数据备份目录：qdrant.bak-<时间戳毫秒>（与数据目录同级）
    fn backup_dir(&self) -> PathBuf {
        let mut name = self.data_dir.file_name().unwrap_or_default().to_os_string();
        name.push(format!(
            ".bak-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));
        self.data_dir.with_file_name(name)
    }

    /// 关闭 shard（Drop 时自动 flush）
    pub fn stop(&mut self) {
        self.shard = None;
    }

    /// 检查 shard 是否已加载
    pub fn is_running(&mut self) -> bool {
        self.shard.is_some()
    }

    /// 批量插入/更新向量
    pub fn upsert_points(
        &self,
        points: Vec<QdrantPoint>,
    ) -> Result<(), String> {
        let shard = self.shard.as_ref().ok_or("qdrant 未启动")?;

        let persisted: Vec<PointStructPersisted> = points
            .into_iter()
            .map(|p| {
                let payload = match p.payload {
                    serde_json::Value::Object(map) => Some(Payload(map)),
                    _ => None,
                };
                PointStructPersisted {
                    id: PointId::NumId(p.id),
                    vector: VectorStructPersisted::Named(HashMap::from([(
                        VECTOR_NAME.to_string(),
                        VectorPersisted::Dense(p.vector),
                    )])),
                    payload,
                }
            })
            .collect();

        shard
            .update(UpdateOperation::PointOperation(
                PointOperations::UpsertPoints(PointInsertOperations::PointsList(
                    persisted,
                )),
            ))
            .map_err(|e| format!("插入向量失败: {}", e))
    }

    /// 搜索相似向量
    pub fn search(
        &self,
        vector: Vec<f32>,
        limit: usize,
    ) -> Result<Vec<QdrantSearchHit>, String> {
        let shard = self.shard.as_ref().ok_or("qdrant 未启动")?;

        let query = QueryEnum::Nearest(NamedQuery::new(
            VectorInternal::Dense(vector),
            VECTOR_NAME,
        ));

        let mut request = QueryRequest::new(limit);
        request.query = Some(ScoringQuery::Vector(query));
        request.with_payload = WithPayloadInterface::Bool(true);

        let scored = shard.query(request)
            .map_err(|e| format!("搜索失败: {}", e))?;

        let hits: Vec<QdrantSearchHit> = scored
            .into_iter()
            .map(|s| QdrantSearchHit {
                id: match s.id {
                    PointId::NumId(n) => n,
                    _ => 0,
                },
                score: s.score,
                payload: s.payload.map(|p| serde_json::Value::Object(p.0)),
            })
            .collect();

        Ok(hits)
    }

    /// 删除指定文件的所有向量
    pub fn delete_points(&self, file_path: &str) -> Result<(), String> {
        let shard = self.shard.as_ref().ok_or("qdrant 未启动")?;

        let filter = Filter::new_must(Condition::Field(
            FieldCondition::new_match(
                "path".parse::<JsonPath>().unwrap(),
                Match::new_value(ValueVariants::String(file_path.to_string())),
            ),
        ));

        shard
            .update(UpdateOperation::PointOperation(
                PointOperations::DeletePointsByFilter(filter),
            ))
            .map_err(|e| format!("删除向量失败: {}", e))
    }

    /// 获取 shard 中的向量数量
    pub fn count(&self) -> Result<usize, String> {
        let shard = self.shard.as_ref().ok_or("qdrant 未启动")?;
        shard
            .count(CountRequest::new())
            .map_err(|e| format!("计数失败: {}", e))
    }
}

impl Drop for QdrantManager {
    fn drop(&mut self) {
        // EdgeShard 的 Drop 会自动 flush
    }
}

// ============================================
// EmbeddingClient — 在线嵌入 API
// ============================================

/// 嵌入 API 响应（OpenAI 兼容格式）
#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

/// 在线嵌入客户端：POST OpenAI 兼容的 embeddings 接口
pub struct EmbeddingClient {
    client: reqwest::blocking::Client,
    api_url: String,
    api_key: Option<String>,
    model: String,
    dim: usize,
}

impl EmbeddingClient {
    /// 从全局配置构建；未配置 API 地址时报错
    pub fn from_global(cfg: &GlobalRagConfig) -> Result<Self, String> {
        let raw = cfg
            .api_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or("未配置嵌入 API 地址，请点击菜单栏【智搜】填写")?;

        // 允许填 base_url，自动补全 /embeddings 路径
        let api_url = if raw.trim_end_matches('/').ends_with("/embeddings") {
            raw.to_string()
        } else {
            format!("{}/embeddings", raw.trim_end_matches('/'))
        };

        let api_key = cfg.api_key.clone().filter(|s| !s.trim().is_empty());
        let model = cfg
            .model
            .clone()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_EMBED_MODEL.to_string());
        let dim = cfg.dim.unwrap_or(DEFAULT_DIM);

        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| e.to_string())?;

        Ok(Self { client, api_url, api_key, model, dim })
    }

    pub fn api_url(&self) -> &str {
        &self.api_url
    }

    fn embed_inner(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let body = serde_json::json!({
            "model": self.model,
            "input": texts,
            "encoding_format": "float",
        });

        let mut req = self.client.post(&self.api_url).json(&body);
        if let Some(key) = &self.api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        let resp = req
            .send()
            .map_err(|e| format!("嵌入 API 请求失败: {}", e))?;
        let status = resp.status();
        let text = resp.text().map_err(|e| e.to_string())?;

        if !status.is_success() {
            let mut msg = format!(
                "嵌入 API 返回 {}: {}",
                status,
                text.chars().take(300).collect::<String>()
            );
            if self.api_key.is_none() {
                msg.push_str("（未配置 API Key，请先执行: config add -g darkhorse.code.ai.api_key <你的密钥>）");
            }
            return Err(msg);
        }

        let parsed: EmbeddingResponse = serde_json::from_str(&text)
            .map_err(|e| format!("解析嵌入响应失败: {}", e))?;

        let mut out = Vec::with_capacity(parsed.data.len());
        for d in parsed.data {
            if d.embedding.len() != self.dim {
                return Err(format!(
                    "嵌入 API 返回向量维度 {} 与配置维度 {} 不一致，请检查 API 或修改维度配置",
                    d.embedding.len(),
                    self.dim
                ));
            }
            out.push(d.embedding);
        }

        if out.is_empty() {
            return Err("嵌入 API 返回空结果".to_string());
        }
        Ok(out)
    }

    /// 编码单个文本
    pub fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
        let mut vecs = self.embed_inner(&[text.to_string()])?;
        Ok(vecs.remove(0))
    }

    /// 批量编码（索引时每批一次请求，减少网络往返）
    pub fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        self.embed_inner(texts)
    }
}

// ============================================
// RagManager — 统一入口
// ============================================

pub struct RagManager {
    qdrant: QdrantManager,
    embed: Option<EmbeddingClient>,
    pub config: ProjectRagConfig,
    pub progress: IndexProgress,
    /// 最近一次向量库重建的说明（用于提示用户重新索引）
    pub rebuild_note: Option<String>,
    /// 当前已加载配置所属的项目根目录（切换项目时重新加载 rag.toml）
    config_project: Option<String>,
}

impl RagManager {
    pub fn new() -> Self {
        Self {
            qdrant: QdrantManager::new(DEFAULT_DIM),
            embed: None,
            config: ProjectRagConfig::default(),
            progress: IndexProgress { phase: "idle".to_string(), current: 0, total: 0 },
            rebuild_note: None,
            config_project: None,
        }
    }

    /// 读取项目 rag.toml
    pub fn load_config(&mut self, project_root: &str) -> Result<(), String> {
        let path = PathBuf::from(project_root)
            .join(".darkhorse")
            .join("code")
            .join("rag.toml");
        if path.exists() {
            let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            self.config = toml::from_str(&content).unwrap_or_default();
        }
        Ok(())
    }

    /// 确保 self.config 属于当前项目：切换项目时重新加载其 rag.toml
    fn ensure_project(&mut self, project_root: &str) -> Result<(), String> {
        if self.config_project.as_deref() != Some(project_root) {
            self.config = ProjectRagConfig::default();
            self.load_config(project_root)?;
            self.config_project = Some(project_root.to_string());
        }
        Ok(())
    }

    /// 保存项目 rag.toml
    pub fn save_config(&self, project_root: &str) -> Result<(), String> {
        let dir = PathBuf::from(project_root)
            .join(".darkhorse")
            .join("code");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("rag.toml");
        let toml_str = toml::to_string_pretty(&self.config).map_err(|e| e.to_string())?;
        std::fs::write(&path, toml_str).map_err(|e| e.to_string())
    }

    /// 全局配置中的向量维度
    fn current_dim(&self) -> usize {
        load_global_rag_config().dim.unwrap_or(DEFAULT_DIM)
    }

    /// 启动 qdrant（维度变化时自动重建空库；旧数据无法加载时自动备份重建）
    fn ensure_qdrant(&mut self) -> Result<StartOutcome, String> {
        let dim = self.current_dim();
        if self.qdrant.dim() != dim {
            self.qdrant.stop();
            self.qdrant = QdrantManager::new(dim);
        }
        self.qdrant.start()
    }

    /// 处理向量库重建结果：记录说明并作废旧 hash 记录（强制全量重编）
    fn on_rebuilt(&mut self, project_root: &str, reason: String) {
        self.rebuild_note = Some(reason);
        self.config.files.clear();
        self.config.files_count = 0;
        let _ = self.save_config(project_root);
    }

    /// 构建嵌入客户端（未配置 API 地址时报错）
    fn ensure_embed(&mut self) -> Result<(), String> {
        if self.embed.is_none() {
            let cfg = load_global_rag_config();
            self.embed = Some(EmbeddingClient::from_global(&cfg)?);
        }
        Ok(())
    }

    /// 初始化 RAG：启动 qdrant + 构建嵌入客户端
    pub fn init(&mut self, project_root: &str) -> Result<(), String> {
        self.ensure_project(project_root)?;
        if let StartOutcome::Rebuilt { reason } = self.ensure_qdrant()? {
            self.on_rebuilt(project_root, reason);
        }
        self.ensure_embed()
    }

    /// 全量索引（带进度回调）
    pub fn full_index_with_progress(
        &mut self,
        project_root: &str,
        on_progress: &dyn Fn(usize, usize),
    ) -> Result<usize, String> {
        let root = PathBuf::from(project_root);
        if !root.exists() {
            return Err(format!("项目路径不存在: {}", project_root));
        }

        self.ensure_project(project_root)?;
        if let StartOutcome::Rebuilt { reason } = self.ensure_qdrant()? {
            self.on_rebuilt(project_root, reason);
        }
        self.ensure_embed()?;

        // 向量库为空（首次索引或维度变更后清空）→ 重置 hash 记录，强制全量编码
        if self.qdrant.count()? == 0 {
            self.config.files.clear();
        }

        // 先扫描所有文件
        let mut all_files: Vec<(PathBuf, String)> = Vec::new();
        for entry in walkdir::WalkDir::new(&root)
            .into_iter()
            .filter_entry(|e| !is_excluded(e))
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file() && has_valid_extension(e.path()))
        {
            let rel_path = entry.path()
                .strip_prefix(&root)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .replace('\\', "/");
            all_files.push((entry.path().to_path_buf(), rel_path));
        }

        let total = all_files.len();
        on_progress(0, total);

        let mut new_files: HashMap<String, String> = HashMap::new();
        let mut points: Vec<QdrantPoint> = Vec::new();
        let mut pending: Vec<(String, String, String)> = Vec::new(); // (rel_path, hash, content)
        let mut id_counter: u64 = 0;
        let mut done: usize = 0;

        for (path, rel_path) in &all_files {
            let content = match std::fs::read_to_string(path) {
                Ok(s) => s,
                Err(_) => { done += 1; on_progress(done, total); continue; },
            };

            if content.trim().is_empty() || content.len() > MAX_FILE_CHARS {
                done += 1; on_progress(done, total); continue;
            }

            // hash 相同 → 无需重新编码
            let hash = compute_hash(&content);
            if self.config.files.get(rel_path) == Some(&hash) {
                new_files.insert(rel_path.clone(), hash);
                done += 1; on_progress(done, total); continue;
            }

            pending.push((rel_path.clone(), hash, content));

            // 攒满一批，批量调用嵌入 API
            if pending.len() >= EMBED_BATCH {
                self.flush_pending_batch(&mut pending, &mut points, &mut new_files, &mut id_counter)?;
            }

            done += 1; on_progress(done, total);
        }
        self.flush_pending_batch(&mut pending, &mut points, &mut new_files, &mut id_counter)?;

        // 删除已不存在文件的旧向量
        for rel in self.config.files.keys() {
            if !new_files.contains_key(rel) {
                let _ = self.qdrant.delete_points(rel);
            }
        }

        if !points.is_empty() {
            self.qdrant.upsert_points(points)?;
        }

        let count = new_files.len();
        self.config.files = new_files;
        self.config.files_count = count;
        self.config.last_indexed = Some(chrono_now());
        // 持久化 hash 记录：重启后未变化的文件跳过重新编码
        let _ = self.save_config(project_root);

        Ok(count)
    }

    /// 批量编码 pending 中的文件并累积到 points
    fn flush_pending_batch(
        &mut self,
        pending: &mut Vec<(String, String, String)>,
        points: &mut Vec<QdrantPoint>,
        new_files: &mut HashMap<String, String>,
        id_counter: &mut u64,
    ) -> Result<(), String> {
        if pending.is_empty() {
            return Ok(());
        }
        let texts: Vec<String> = pending.iter().map(|(_, _, c)| c.clone()).collect();
        let embeddings = self
            .embed
            .as_ref()
            .ok_or("嵌入客户端未初始化")?
            .embed_batch(&texts)
            .map_err(|e| format!("嵌入编码失败: {}", e))?;

        for ((rel_path, hash, content), vector) in pending.drain(..).zip(embeddings) {
            let snippet = content.lines().take(3).collect::<Vec<_>>().join("\n");
            let snippet = if snippet.len() > 200 {
                format!("{}...", &snippet[..200])
            } else {
                snippet
            };
            let line_count = content.lines().count().max(1);

            points.push(QdrantPoint {
                id: *id_counter,
                vector,
                payload: serde_json::json!({
                    "path": rel_path,
                    "start_line": 1,
                    "end_line": line_count,
                    "snippet": snippet,
                }),
            });
            *id_counter += 1;
            new_files.insert(rel_path, hash);
        }
        Ok(())
    }

    /// 全量索引（无回调）
    pub fn full_index(&mut self, project_root: &str) -> Result<usize, String> {
        self.full_index_with_progress(project_root, &|_, _| {})
    }

    /// 增量索引单文件
    pub fn index_file(&mut self, project_root: &str, abs_path: &str) -> Result<(), String> {
        self.ensure_project(project_root)?;
        if self.config.files_count == 0 {
            // 还没建过索引，跳过
            return Ok(());
        }
        if let StartOutcome::Rebuilt { reason } = self.ensure_qdrant()? {
            // 向量库刚重建 → 增量索引作废，等待全量重建
            self.on_rebuilt(project_root, reason);
            return Ok(());
        }
        self.ensure_embed()?;

        let p = PathBuf::from(abs_path);
        let content = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
        let root = PathBuf::from(project_root);
        let rel_path = p.strip_prefix(&root).unwrap_or(&p).to_string_lossy().replace('\\', "/");

        // 删除旧向量
        let _ = self.qdrant.delete_points(&rel_path);

        if content.trim().is_empty() || content.len() > MAX_FILE_CHARS {
            config_files_remove(&mut self.config, &rel_path);
            return Ok(());
        }

        let hash = compute_hash(&content);
        let embedding = self.embed.as_ref().unwrap().embed(&content)?;
        let snippet = content.lines().take(3).collect::<Vec<_>>().join("\n");
        let snippet = if snippet.len() > 200 { format!("{}...", &snippet[..200]) } else { snippet };
        let line_count = content.lines().count().max(1);

        self.qdrant.upsert_points(vec![QdrantPoint {
            id: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,
            vector: embedding,
            payload: serde_json::json!({
                "path": rel_path,
                "start_line": 1,
                "end_line": line_count,
                "snippet": snippet,
            }),
        }])?;

        self.config.files.insert(rel_path, hash);
        self.config.files_count = self.config.files.len();
        // 持久化 hash 记录（与 remove_file 保持一致）
        let _ = self.save_config(project_root);
        Ok(())
    }

    /// 删除单个文件索引
    pub fn remove_file(&mut self, project_root: &str, rel_path: &str) -> Result<(), String> {
        self.ensure_project(project_root)?;
        if self.config.files_count == 0 {
            return Ok(());
        }
        if let StartOutcome::Rebuilt { reason } = self.ensure_qdrant()? {
            // 向量库刚重建 → 无需删除，等待全量重建
            self.on_rebuilt(project_root, reason);
            return Ok(());
        }
        self.qdrant.delete_points(rel_path)?;
        config_files_remove(&mut self.config, rel_path);
        let _ = self.save_config(project_root);
        Ok(())
    }

    /// 空闲检查：在线嵌入模式下无需卸载模型，qdrant-edge 常驻内存开销极小
    pub fn idle_check(&mut self) {}

    /// 搜索
    pub fn search(&mut self, project_root: &str, query: &str, top_k: usize) -> Result<Vec<SearchResult>, String> {
        self.ensure_project(project_root)?;
        if !self.qdrant.is_running() || self.embed.is_none() {
            self.init(project_root)?;
        }

        if self.config.files_count == 0 {
            return Err("索引为空，请先点击菜单栏【智搜】建立索引".to_string());
        }

        let embedding = self.embed.as_ref().unwrap().embed(query)?;
        let hits = self.qdrant.search(embedding, top_k)?;

        let results: Vec<SearchResult> = hits
            .into_iter()
            .map(|h| {
                let payload = h.payload.unwrap_or_default();
                SearchResult {
                    path: payload["path"].as_str().unwrap_or("").to_string(),
                    score: h.score,
                    start_line: payload["start_line"].as_u64().unwrap_or(1) as usize,
                    end_line: payload["end_line"].as_u64().unwrap_or(1) as usize,
                    snippet: payload["snippet"].as_str().unwrap_or("").to_string(),
                }
            })
            .collect();

        Ok(results)
    }

    /// 关闭
    pub fn shutdown(&mut self) {
        self.qdrant.stop();
    }

    /// 获取状态
    pub fn status(&mut self) -> IndexStatus {
        IndexStatus {
            enabled: self.config.enabled,
            api_configured: self.embed.is_some(),
            api_url: self.embed.as_ref().map(|e| e.api_url().to_string()),
            qdrant_running: self.qdrant.is_running(),
            files_indexed: self.config.files_count,
            last_indexed: self.config.last_indexed.clone(),
            progress: if self.progress.phase != "idle" {
                Some(self.progress.clone())
            } else {
                None
            },
            rebuild_note: self.rebuild_note.clone(),
        }
    }
}

// ============================================
// 配置辅助
// ============================================

fn config_files_remove(config: &mut ProjectRagConfig, rel_path: &str) {
    config.files.remove(rel_path);
    config.files_count = config.files.len();
}

/// 读取全局 rag 配置
pub fn load_global_rag_config() -> GlobalRagConfig {
    let path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".darkhorse")
        .join("code")
        .join("rag.toml");
    if path.exists() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            return toml::from_str(&content).unwrap_or_default();
        }
    }
    GlobalRagConfig::default()
}

/// 保存全局 rag 配置
pub fn save_global_rag_config(cfg: &GlobalRagConfig) -> Result<(), String> {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".darkhorse")
        .join("code");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("rag.toml");
    let toml_str = toml::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(&path, toml_str).map_err(|e| e.to_string())
}

// ============================================
// 辅助函数
// ============================================

fn is_excluded(entry: &walkdir::DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy();
    if name.starts_with('.') && entry.file_type().is_dir() {
        // 不要排除以 . 开头的文件，只排除文件夹
        if name == "." || name == ".." {
            return true;
        }
        if EXCLUDED_DIRS.contains(&name.as_ref()) {
            return true;
        }
    }
    // 也检查非 . 开头的排除目录
    if entry.file_type().is_dir() && EXCLUDED_DIRS.contains(&name.as_ref()) {
        return true;
    }
    false
}

fn has_valid_extension(path: &std::path::Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        // 没有扩展名的文件（如 Makefile, Dockerfile）也要索引
        // 但排除一些常见的无扩展名二进制
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        return matches!(name, "Makefile" | "Dockerfile" | "LICENSE" | "README");
    };
    let ext_lower = ext.to_lowercase();

    if EXCLUDED_EXTENSIONS.contains(&ext_lower.as_str()) {
        return false;
    }

    DEFAULT_EXTENSIONS.contains(&ext_lower.as_str())
}

fn compute_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())[..12].to_string()
}

fn chrono_now() -> String {
    use std::time::SystemTime;
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // 简易时间戳格式化（精确到天）
    let _days = ts / 86400;
    // 返回固定格式的日期标记
    format!("{}", ts)
}
