//! **模型缓存**：自检 + 按需下载 + 逐文件校验 —— 记忆的嵌入模型与语音的 whisper 共用这一份。
//!
//! ## 为什么要抽出来
//!
//! 发行形态是"一个可执行文件 + `global/` + `projects/` + `plugins/`"，所以**只有单 exe** 不是
//! 边缘情况，是主路径之一。于是"某份权重没装，怎么把它补上"这件事会一而再地出现：
//! 先是记忆的嵌入模型（96MB），现在是语音的 whisper（453MB）。
//! 两处各写一套的代价不是多敲几百行 —— 是**纪律会漂**：`.part` 原子落盘、换源必须拒绝未知字节、
//! 幂等跳过，这三条只要有一处忘了，坏表现都是"看起来装好了、其实拿到的是半截或别人的字节"。
//! 所以：**规格（文件/体积/sha256/来源）各归各的 JSON，机制只有这一份**。
//!
//! ## 三条纪律（改这个文件时不许绕过）
//!
//! 1. **先 `.part` 再改名**：半截文件**永远**不会看起来像装好了。校验不过就删 `.part`，
//!    并把"拿到了什么"与"要的是什么"两个哈希都打出来。
//! 2. **换源不许改口径**：兜底源拿到的字节若与清单不符 → **拒绝**。那时我们不知道拿到的是什么，
//!    宁可失败也别让"未知字节"变成"已安装"。
//! 3. **幂等**：文件在且哈希对就跳过 —— 断网重跑不会重下几百 MB，下到一半被杀也不留残骸。
//!
//! ## 与"降级"的关系
//!
//! 缺模型时调用方**照常工作**（记忆退化为纯词法、语音按钮说清"要先下模型"）。
//! 这个模块只负责把"用户自己去模型站找文件"变成"一条命令 / 一个按钮"。

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// 规格里的一条来源（`{file}` 会被替换成文件名）。
#[derive(Debug, Clone, Deserialize)]
pub struct Source {
    pub tag: String,
    /// 含 `{file}` 占位符的 URL 模板。
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpecFile {
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
    #[serde(default)]
    pub optional: bool,
}

impl Source {
    pub fn url_for(&self, file: &str) -> String {
        self.url.replace("{file}", file)
    }
}

/// 一份模型的规格（由调用方各自的 `*-spec.json` 反序列化而来，编译进二进制）。
#[derive(Debug, Clone, Deserialize)]
pub struct ModelSpec {
    pub model: String,
    pub dir_name: String,
    /// 向量维度。只有嵌入模型用得上；别的模型写 0（字段留着是为了规格形状只有一种）。
    pub dim: u64,
    pub sources: Vec<Source>,
    pub files: Vec<SpecFile>,
}

impl ModelSpec {
    /// 必需文件（`optional` 的不算判据）。
    pub fn required_files(&self) -> Vec<&SpecFile> {
        self.files.iter().filter(|f| !f.optional).collect()
    }

    /// 必需文件的总字节数（给界面报"要下多少"）。
    pub fn required_bytes(&self) -> u64 {
        self.required_files().iter().map(|f| f.bytes).sum()
    }
}

pub fn sha256_file(p: &Path) -> Result<String, String> {
    let data = std::fs::read(p).map_err(|e| format!("读 {} 失败: {e}", p.display()))?;
    Ok(hex(&Sha256::digest(&data)))
}

pub fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

/// 自检结论。**要能分辨"没装"与"装坏了"** —— 两者给用户的下一步不一样。
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    /// 必需文件齐且哈希对
    Ready {
        dir: PathBuf,
        files: usize,
        bytes: u64,
    },
    /// 一个都没有（干净的单 exe）
    Absent {
        dir: PathBuf,
        /// 规格里的模型名（说人话要用）
        model: String,
        /// 要下多少（界面上的按钮得先告诉用户代价）
        need_bytes: u64,
    },
    /// 有文件但不完整/对不上哈希（拿证据说话）
    Broken { dir: PathBuf, problems: Vec<String> },
    /// 没配置模型目录（宿主没设）
    NoRoot,
}

impl Status {
    pub fn is_ready(&self) -> bool {
        matches!(self, Status::Ready { .. })
    }

    /// 一句话人话，`label` 是这个能力的名字（"语义检索" / "语音转写"）。
    ///
    /// 自带规格里的名字与体积（而不是去够外面的全局），于是同一份文案逻辑能给两个模块用。
    pub fn line_for(&self, label: &str) -> String {
        match self {
            Status::Ready { files, bytes, .. } => {
                format!(
                    "{label}已就绪（{files} 个文件 / {:.1} MB）",
                    *bytes as f64 / 1e6
                )
            }
            Status::Absent {
                model, need_bytes, ..
            } => format!(
                "{label}未启用：模型未安装（{model} / {:.1} MB）—— 可一键下载",
                *need_bytes as f64 / 1e6
            ),
            Status::Broken { problems, .. } => format!(
                "{label}未启用：模型文件不完整（{}）—— 重新下载即可修好",
                problems.join("；")
            ),
            Status::NoRoot => format!("{label}未启用：未配置模型目录"),
        }
    }
}

/// 用**指定规格**自检（测试与"检查另一份模型"都用它）。
pub fn status_with(spec: &ModelSpec, dir: &Path) -> Status {
    let mut problems = Vec::new();
    let mut present = 0usize;
    let mut bytes = 0u64;
    for f in spec.required_files() {
        let p = dir.join(&f.name);
        if !p.is_file() {
            problems.push(format!("缺 {}", f.name));
            continue;
        }
        present += 1;
        bytes += std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        match sha256_file(&p) {
            Ok(h) if h == f.sha256 => {}
            Ok(h) => problems.push(format!(
                "{} 哈希不符（{}… ≠ {}…）",
                f.name,
                &h[..8],
                &f.sha256[..8]
            )),
            Err(e) => problems.push(e),
        }
    }
    if problems.is_empty() {
        Status::Ready {
            dir: dir.to_path_buf(),
            files: present,
            bytes,
        }
    } else if present == 0 {
        Status::Absent {
            dir: dir.to_path_buf(),
            model: spec.model.clone(),
            need_bytes: spec.required_bytes(),
        }
    } else {
        Status::Broken {
            dir: dir.to_path_buf(),
            problems,
        }
    }
}

/// 一次下载的进度（界面按它显示"第几个文件 / 已下多少"）。
#[derive(Debug, Clone)]
pub struct Progress {
    pub file: String,
    /// 第几个文件（1-based）
    pub index: usize,
    pub of: usize,
    /// 这个文件已下 / 共下 字节
    pub done: u64,
    pub total: u64,
    /// 当前来源 tag（modelscope / hf-mirror …）
    pub tag: String,
}

/// 一次按需下载的结果。
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub fetched: Vec<String>,
    pub skipped: Vec<String>,
    pub bytes: u64,
    pub from: Vec<String>,
}

/// 下载到指定目录（可注入规格 —— 测试用它跑小文件，不碰几百 MB 的权重）。
pub async fn fetch_into<F>(spec: &ModelSpec, dir: &Path, mut on: F) -> Result<Report, String>
where
    F: FnMut(Progress),
{
    std::fs::create_dir_all(dir).map_err(|e| format!("建目录 {} 失败: {e}", dir.display()))?;
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| format!("建 HTTP 客户端失败: {e}"))?;
    let total_n = spec.files.len();
    let mut rep = Report::default();

    for (i, f) in spec.files.iter().enumerate() {
        let dest = dir.join(&f.name);
        // 幂等：在且哈希对就跳过（断网重跑不重下）
        if dest.is_file() && sha256_file(&dest).map(|h| h == f.sha256).unwrap_or(false) {
            rep.skipped.push(f.name.clone());
            on(Progress {
                file: f.name.clone(),
                index: i + 1,
                of: total_n,
                done: f.bytes,
                total: f.bytes,
                tag: "already".into(),
            });
            continue;
        }
        let part = dir.join(format!("{}.part", f.name));
        let mut last_err: Option<String> = None;
        for s in &spec.sources {
            let url = s.url_for(&f.name);
            match download_one(&client, &url, &part, f, i + 1, total_n, &s.tag, &mut on).await {
                Ok(bytes) => {
                    // 校验通过才改名 —— 半截文件永远不叫这个名字
                    std::fs::rename(&part, &dest)
                        .map_err(|e| format!("改名 {} 失败: {e}", dest.display()))?;
                    rep.fetched.push(f.name.clone());
                    rep.bytes += bytes;
                    rep.from.push(s.tag.clone());
                    last_err = None;
                    break;
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&part);
                    last_err = Some(format!("[{}] {e}", s.tag));
                }
            }
        }
        if let Some(e) = last_err {
            return Err(format!(
                "取模型失败（{}）：{e} —— 文件没落地（半截的 .part 已删）",
                f.name
            ));
        }
    }
    Ok(rep)
}

#[allow(clippy::too_many_arguments)]
async fn download_one<F>(
    client: &reqwest::Client,
    url: &str,
    part: &Path,
    f: &SpecFile,
    index: usize,
    of: usize,
    tag: &str,
    on: &mut F,
) -> Result<u64, String>
where
    F: FnMut(Progress),
{
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("请求失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {} {url}", resp.status().as_u16()));
    }
    let mut file =
        std::fs::File::create(part).map_err(|e| format!("建 {} 失败: {e}", part.display()))?;
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    let mut last_emit = 0u64;
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("读响应失败: {e}"))? {
        hasher.update(&chunk);
        file.write_all(&chunk)
            .map_err(|e| format!("写盘失败: {e}"))?;
        done += chunk.len() as u64;
        // 每 ~1MB 报一次：够"在动"，又不至于把事件通道打满
        if done - last_emit >= 1_048_576 {
            last_emit = done;
            on(Progress {
                file: f.name.clone(),
                index,
                of,
                done,
                total: f.bytes,
                tag: tag.to_string(),
            });
        }
    }
    file.flush().map_err(|e| format!("落盘失败: {e}"))?;
    drop(file);
    on(Progress {
        file: f.name.clone(),
        index,
        of,
        done,
        total: f.bytes,
        tag: tag.to_string(),
    });
    let got = hex(&hasher.finalize());
    if got != f.sha256 {
        return Err(format!(
            "哈希不符：拿到 {got}…，清单要 {}…（{} 字节；不接受未知字节）",
            &f.sha256[..8],
            done
        ));
    }
    if done != f.bytes {
        return Err(format!("体积不符：拿到 {done}，清单写 {}", f.bytes));
    }
    Ok(done)
}
