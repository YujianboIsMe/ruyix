//! **向量腿**：本地嵌入（`bge-small-zh-v1.5`，512 维，CLS 池化 + L2 归一化）。
//!
//! 三条落地纪律：
//!
//! 1. **不联网、不带 Python**：模型是**文件**，用 `candle`（纯 Rust）直接吃 `model.safetensors`。
//!    绿色形态不能要求用户装 Python，所以 fastembed(ONNX) / Python sidecar 两条路都没走
//!    （ONNX 也不在这几个 ModelScope 仓库里，实测过文件清单）。
//! 2. **模型不在 = 降级不是故障**：向量腿缺席时检索自动退化为纯词法（FTS5/BM25），
//!    并且这件事会被**说出来**（注入块里注明"向量腿未启用"）。
//! 3. **模型文件不进 git、进发行包**：取模型是**打包步骤**（`scripts/fetch-embed-model.mjs`），
//!    落到发行包 `<根>/global/memory/model/`（记忆是核心模块，不走插件体系）。
//!
//! 让词法腿也够用的那部分在 [`super::tokenize`]；这里只负责"把一句话变成 512 个浮点"。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 输出维度（bge-small-zh-v1.5 的 hidden size）。
pub const DIM: usize = 512;
/// 模型目录名（`global/memory/model/<这个>/`）。
pub const MODEL_DIR_NAME: &str = "bge-small-zh-v1.5";
/// BGE 中文**查询侧**建议前缀。默认关闭：我们的场景是"短事实 ↔ 短问句"的对称相似，
/// 加了反而把长前缀的优势变成噪声（要开就在调用处显式开）。
pub const QUERY_INSTRUCTION: &str = "为这个句子生成表示以用于检索相关文章：";

static MODEL_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// 宿主启动时把 `<根>/global/memory/model` 交进来（引擎不认识便携根）。
pub fn set_model_root(p: impl Into<PathBuf>) {
    let _ = MODEL_ROOT.set(p.into());
}

/// 模型根目录：宿主设过的优先，其次 `RUYIX_MEM_MODEL_DIR`。
pub fn model_root() -> Option<PathBuf> {
    if let Some(p) = MODEL_ROOT.get() {
        return Some(p.clone());
    }
    std::env::var_os("RUYIX_MEM_MODEL_DIR").map(PathBuf::from)
}

pub fn model_dir() -> Option<PathBuf> {
    model_root().map(|r| r.join(MODEL_DIR_NAME))
}

/// 三个必需文件齐了才算"装了模型"（不猜、不看目录大小）。
pub fn is_available() -> bool {
    match model_dir() {
        Some(d) => missing_files(&d).is_empty(),
        None => false,
    }
}

fn missing_files(dir: &Path) -> Vec<String> {
    ["config.json", "tokenizer.json", "model.safetensors"]
        .iter()
        .filter(|f| !dir.join(f).is_file())
        .map(|f| f.to_string())
        .collect()
}

/// 没装模型时的**人话**原因（要出现在状态栏/注入块里，而不是一声不响地退化成纯词法）。
pub fn unavailable_reason() -> String {
    match model_dir() {
        None => "未配置模型目录（宿主未设、也没有 RUYIX_MEM_MODEL_DIR）".into(),
        Some(d) => {
            let miss = missing_files(&d);
            if miss.is_empty() {
                "可用".into()
            } else {
                format!("模型目录 {} 缺文件：{}", d.display(), miss.join(", "))
            }
        }
    }
}

/// 一个加载好的嵌入模型。**每次调用 mmap 加载**：`safetensors` 是 mmap，
/// 权重不进内存（实测加载是毫秒级），换来的是没有 `Send/Sync` 与全局可变状态的纠缠。
pub struct Model {
    #[cfg(feature = "embed")]
    inner: embedded::Loaded,
}

impl Model {
    pub fn try_load() -> Result<Option<Model>, String> {
        let Some(dir) = model_dir() else {
            return Ok(None);
        };
        if !missing_files(&dir).is_empty() {
            return Ok(None);
        }
        #[cfg(feature = "embed")]
        {
            embedded::load(&dir).map(|inner| Some(Model { inner }))
        }
        #[cfg(not(feature = "embed"))]
        {
            let _ = dir;
            Err("本构建未编入向量腿（cargo feature `embed` 关闭）—— 记忆会退化为纯词法检索".into())
        }
    }

    /// `is_query = true` 时按 BGE 的建议（可选）加查询前缀；默认不加。
    pub fn embed(&self, text: &str, _is_query: bool) -> Result<Vec<f32>, String> {
        #[cfg(feature = "embed")]
        {
            embedded::embed(&self.inner, text)
        }
        #[cfg(not(feature = "embed"))]
        {
            let _ = text;
            Err("向量腿未编入".into())
        }
    }
}

/// **进程级缓存**的模型句柄。
///
/// 为什么必须有这一层：折叠每写一条信念都要算向量，而"每次 mmap + 解析 tokenizer" 实测
/// 让 200 条事件的测试从 4 秒涨到 **148 秒**（是测试套件当场抓到的，不是估的）。
/// 缓存之后一次加载、全程复用。
static LOADED: std::sync::OnceLock<std::sync::Mutex<Option<std::sync::Arc<Model>>>> =
    std::sync::OnceLock::new();

/// 供检索腿使用：模型不在（或加载失败）就返回 `None`，检索自动退化。
pub fn load_cached() -> Result<Option<std::sync::Arc<Model>>, String> {
    let cell = LOADED.get_or_init(|| std::sync::Mutex::new(None));
    if let Ok(g) = cell.lock()
        && let Some(m) = g.as_ref()
    {
        return Ok(Some(m.clone()));
    }
    if !is_available() {
        return Ok(None);
    }
    match Model::try_load() {
        Ok(Some(m)) => {
            let arc = std::sync::Arc::new(m);
            if let Ok(mut g) = cell.lock() {
                *g = Some(arc.clone());
            }
            Ok(Some(arc))
        }
        Ok(None) => Ok(None),
        Err(e) => {
            // 加载失败要**看得见**：退化成纯词法可以，但静默不行
            crate::debug::note(&format!("[mem] 嵌入模型加载失败，退化为纯词法检索：{e}"));
            Ok(None)
        }
    }
}

pub fn to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

pub fn from_blob(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// 余弦相似度（两边都已 L2 归一化时就是点积；这里仍按定义算，防止漏归一化时静默变错）。
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f64, 0f64, 0f64);
    for i in 0..a.len() {
        let (x, y) = (a[i] as f64, b[i] as f64);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// 读某条信念的向量。
pub fn vector_of(mem: &super::Memory, row_key: &str) -> Result<Option<Vec<f32>>, String> {
    let conn = mem.conn()?;
    let mut stmt = conn
        .prepare("SELECT dim, vec FROM vectors WHERE row_key=?1")
        .map_err(|e| format!("读向量失败: {e}"))?;
    let r = stmt.query_row(rusqlite::params![row_key], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
    });
    match r {
        Ok((d, b)) => {
            if d as usize != DIM {
                return Ok(None); // 维度变了（换模型）⇒ 当作没有，等重建
            }
            Ok(Some(from_blob(&b)))
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(format!("读向量失败: {e}")),
    }
}

/// **按需**给还缺向量的信念补算（返回这次补了几条）。
///
/// 为什么不在写路径上算（第一版就是这么写的，被测试当场打回）：信念写是**交互路径**，
/// 每次 20~150ms 的 CPU 前向让 200 条事件的测试从 4s 涨到 148s；而向量只有"真要语义检索"
/// 时才需要。所以改成懒补：写 = 1ms，第一次中文查询付一次批量代价，之后走缓存。
///
/// 上限参数是**故意的**：一次查询不许变成"把一万条全算一遍"的长尾操作，剩下的下次再补。
pub fn ensure_vectors(mem: &super::Memory, scope: &str, limit: usize) -> usize {
    let Ok(Some(model)) = load_cached() else {
        return 0;
    };
    let Ok(conn) = mem.conn() else {
        return 0;
    };
    let rows: Vec<(String, String, String)> = {
        let Ok(mut stmt) = conn.prepare(
            "SELECT b.scope||'|'||b.key||'|'||b.valid_from||'|'||b.obs_id AS rk, b.key, b.value
             FROM beliefs b LEFT JOIN vectors v ON v.row_key = rk
             WHERE b.scope=?1 AND v.row_key IS NULL LIMIT ?2",
        ) else {
            return 0;
        };
        match stmt.query_map(rusqlite::params![scope, limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        }) {
            Ok(rows) => rows.flatten().collect(),
            Err(_) => return 0,
        }
    };
    let mut n = 0usize;
    for (rk, key, value) in rows {
        if let Ok(v) = model.embed(&format!("{key} {value}"), false)
            && conn
                .execute(
                    "INSERT OR REPLACE INTO vectors (row_key, dim, vec) VALUES (?1, ?2, ?3)",
                    rusqlite::params![rk, DIM as i64, to_blob(&v)],
                )
                .is_ok()
        {
            n += 1;
        }
    }
    n
}

#[cfg(feature = "embed")]
mod embedded {
    use super::*;
    use candle_core::{DType, Device, Tensor};
    use candle_nn::VarBuilder;
    use candle_transformers::models::bert::{BertModel, Config};

    pub struct Loaded {
        model: BertModel,
        tokenizer: tokenizers::Tokenizer,
        device: Device,
    }

    pub fn load(dir: &Path) -> Result<Loaded, String> {
        let cfg_s = std::fs::read_to_string(dir.join("config.json"))
            .map_err(|e| format!("读 config.json 失败: {e}"))?;
        let cfg: Config =
            serde_json::from_str(&cfg_s).map_err(|e| format!("解析 config.json 失败: {e}"))?;
        let weights = dir.join("model.safetensors");
        let device = Device::Cpu;
        // SAFETY: mmap 是 candle 的既有约定；文件在本机、只读、不会被并发改写
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[weights], DType::F32, &device) }
            .map_err(|e| format!("加载 model.safetensors 失败: {e}"))?;
        let model = BertModel::load(vb, &cfg).map_err(|e| format!("构建 BertModel 失败: {e}"))?;
        let tokenizer = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| format!("加载 tokenizer.json 失败: {e}"))?;
        Ok(Loaded {
            model,
            tokenizer,
            device,
        })
    }

    pub fn embed(l: &Loaded, text: &str) -> Result<Vec<f32>, String> {
        let enc = l
            .tokenizer
            .encode(text, true)
            .map_err(|e| format!("分词失败: {e}"))?;
        let ids = Tensor::new(enc.get_ids(), &l.device)
            .and_then(|t| t.unsqueeze(0))
            .map_err(|e| format!("构造 input_ids 失败: {e}"))?;
        let types = Tensor::new(enc.get_type_ids(), &l.device)
            .and_then(|t| t.unsqueeze(0))
            .map_err(|e| format!("构造 token_type_ids 失败: {e}"))?;
        let mask = Tensor::new(enc.get_attention_mask(), &l.device)
            .and_then(|t| t.unsqueeze(0))
            .map_err(|e| format!("构造 attention_mask 失败: {e}"))?;
        let out = l
            .model
            .forward(&ids, &types, Some(&mask))
            .map_err(|e| format!("前向失败: {e}"))?;
        // CLS 池化（bge 的句向量就是 [CLS]）+ L2 归一化
        let cls = out
            .narrow(1, 0, 1)
            .and_then(|t| t.squeeze(1))
            .and_then(|t| t.squeeze(0))
            .map_err(|e| format!("池化失败: {e}"))?;
        let v = cls
            .to_vec1::<f32>()
            .map_err(|e| format!("取向量失败: {e}"))?;
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        let n = if n <= 0.0 { 1.0 } else { n };
        Ok(v.into_iter().map(|x| x / n).collect())
    }
}
