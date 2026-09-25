//! **模型运行时兜底**：自检 + 按需下载 + 逐文件校验（切片 4）。
//!
//! ## 治的是什么病
//!
//! 发行形态是"一个可执行文件 + `global/` + `projects/` + `plugins/`"，所以**只有单 exe** 不是
//! 边缘情况，是主路径之一。此前只有**打包期**的取模型步骤（`scripts/fetch-embed-model.mjs`），
//! 于是单 exe 用户拿到的是"静默降级"：语义检索关掉了，界面上没有任何出口把它打开。
//!
//! ## 关键一招：规格编进二进制
//!
//! 96MB 权重既不进 git、也不该硬编码在代码里 —— 但"这份模型**是什么**"（文件名 / 体积 /
//! sha256 / 来源 URL）只有几百字节，用 `include_str!` 编进来。于是 **exe 自己知道该取什么、
//! 怎么校验**，权重落在外面（`<便携根>/global/memory/model/`）。同一份 `model-spec.json`
//! 既喂运行时兜底、也喂打包期步骤 —— 谁都不许另抄一遍（抄一遍就会漂）。
//!
//! ## 三条纪律
//!
//! 1. **先 `.part` 再改名**：半截文件**永远**不会看起来像装好了。校验不过就删掉 `.part`
//!    并把两个哈希都打出来（"拿到了什么"与"要的是什么"）。
//! 2. **换源不许改口径**：兜底源（hf）拿到的字节若与清单不符 → **拒绝**，不静默接受。
//!    那时我们不知道拿到的是什么，宁可失败也别让"未知字节"变成"已安装"。
//! 3. **幂等**：文件在且哈希对就跳过 —— 断网重跑不会重下 96MB，下载到一半被杀也能续。
//!
//! ## 与"降级"的关系
//!
//! 缺模型时记忆**照常工作**（退化成纯词法检索，注入块里说清楚）。这个模块只是把
//! "怎么让它变完整"从"用户自己去 ModelScope 找"变成"一条命令 / 一个按钮"。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::embed;

/// 规格（`model-spec.json`，编译进二进制）。
const SPEC_JSON: &str = include_str!("model-spec.json");

#[derive(Debug, Clone, Deserialize)]
pub struct ModelSpec {
    pub model: String,
    pub dir_name: String,
    pub dim: u64,
    pub sources: Vec<Source>,
    pub files: Vec<SpecFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Source {
    pub tag: String,
    /// 含 `{file}` 占位符的 URL 模板（与打包期脚本同一份口径）。
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

/// 编进二进制的规格（解析失败是**构建期**的问题，不该等到运行时才炸）。
pub fn spec() -> &'static ModelSpec {
    static S: OnceLock<ModelSpec> = OnceLock::new();
    S.get_or_init(|| {
        serde_json::from_str(SPEC_JSON).unwrap_or_else(|e| {
            panic!("mem::fetch: model-spec.json 解析失败（它是编译进来的，属于构建期缺陷）：{e}")
        })
    })
}

/// 必需文件（`optional` 的不算判据）。
pub fn required_files() -> Vec<&'static SpecFile> {
    spec().files.iter().filter(|f| !f.optional).collect()
}

/// 模型目录：`<模型根>/<dir_name>`（与 `embed::model_dir` 同一口径）。
pub fn target_dir() -> Result<PathBuf, String> {
    embed::model_root()
        .map(|r| r.join(&spec().dir_name))
        .ok_or_else(|| "未配置模型目录（宿主未设、也没有 RUYIX_MEM_MODEL_DIR）".to_string())
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
    },
    /// 有文件但不完整/对不上哈希（拿证据说话）
    Broken {
        dir: PathBuf,
        problems: Vec<String>,
    },
    NoRoot,
}

impl Status {
    pub fn is_ready(&self) -> bool {
        matches!(self, Status::Ready { .. })
    }

    /// 一句话人话（面板与状态栏都用它；**必须说清下一步**）。
    pub fn line(&self) -> String {
        match self {
            Status::Ready { files, bytes, .. } => {
                format!(
                    "语义检索已就绪（{files} 个文件 / {:.1} MB）",
                    *bytes as f64 / 1e6
                )
            }
            Status::Absent { .. } => {
                format!(
                    "语义检索未启用：模型未安装（{} / {:.1} MB）—— 可一键下载",
                    spec().model,
                    required_bytes() as f64 / 1e6
                )
            }
            Status::Broken { problems, .. } => {
                format!(
                    "语义检索未启用：模型文件不完整（{}）—— 重新下载即可修好",
                    problems.join("；")
                )
            }
            Status::NoRoot => "语义检索未启用：未配置模型目录".into(),
        }
    }
}

/// 必需文件的总字节数（给界面报"要下多少"）。
pub fn required_bytes() -> u64 {
    required_files().iter().map(|f| f.bytes).sum()
}

fn sha256_file(p: &Path) -> Result<String, String> {
    let data = std::fs::read(p).map_err(|e| format!("读 {} 失败: {e}", p.display()))?;
    Ok(hex(&Sha256::digest(&data)))
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

/// 自检：必需文件在不在、哈希对不对。**这是唯一的判据来源**（不猜、不看目录大小）。
pub fn status() -> Status {
    let Ok(dir) = target_dir() else {
        return Status::NoRoot;
    };
    status_in(&dir)
}

/// 指定目录的自检（"检查另一个目录"用它；测试用 [`status_with`] 注入小规格）。
pub fn status_in(dir: &Path) -> Status {
    status_with(spec(), dir)
}

/// 用**指定规格**自检（测试与"检查别的模型"用）。
pub fn status_with(spec: &ModelSpec, dir: &Path) -> Status {
    let mut problems = Vec::new();
    let mut present = 0usize;
    let mut bytes = 0u64;
    for f in spec.files.iter().filter(|f| !f.optional) {
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
    /// 当前来源 tag（modelscope / huggingface）
    pub tag: String,
}

/// 一次兜底下载的结果。
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub fetched: Vec<String>,
    pub skipped: Vec<String>,
    pub bytes: u64,
    pub from: Vec<String>,
}

/// 下载到指定目录（可注入规格 —— 测试用它跑小文件，不碰 96MB）。
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
        // 幂等：在且哈希对就跳过（断网重跑不重下 96MB）
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
                "取模型失败（{}）：{e} —— 文件没落地（半截的 .part 已删），记忆仍按词法模式工作",
                f.name
            ));
        }
    }
    Ok(rep)
}

/// 用编译进来的规格下载到默认目录。
pub async fn fetch<F>(on: F) -> Result<Report, String>
where
    F: FnMut(Progress),
{
    let dir = target_dir()?;
    fetch_into(spec(), &dir, on).await
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
        return Err(format!("HTTP {} {}", resp.status().as_u16(), url));
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

// ============================================================ 判据

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    /// 极小的 HTTP/1.1 桩：按路径返回字节；`corrupt` 时**故意改一个字节**（模拟换源拿到别的字节）。
    fn stub(files: Vec<(String, Vec<u8>)>, corrupt: bool) -> String {
        let l = TcpListener::bind("127.0.0.1:0").expect("绑端口");
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in l.incoming() {
                let Ok(mut c) = conn else { continue };
                let mut buf = [0u8; 4096];
                let n = c.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .trim_start_matches('/')
                    .to_string();
                let name = path.split('?').next().unwrap_or("").to_string();
                let body = files
                    .iter()
                    .find(|(f, _)| f == &name)
                    .map(|(_, b)| {
                        let mut v = b.clone();
                        if corrupt && !v.is_empty() {
                            v[0] ^= 0xff;
                        }
                        v
                    })
                    .unwrap_or_default();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = c.write_all(head.as_bytes());
                let _ = c.write_all(&body);
                let _ = c.flush();
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    fn tmp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let d =
            std::env::temp_dir().join(format!("ruyix-fetch-{tag}-{}-{nanos}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    fn tiny_spec(base: &str, body: &[u8]) -> ModelSpec {
        ModelSpec {
            model: "test/tiny".into(),
            dir_name: "tiny".into(),
            dim: 4,
            sources: vec![Source {
                tag: "stub".into(),
                url: format!("{base}/{{file}}"),
            }],
            files: vec![SpecFile {
                name: "w.bin".into(),
                bytes: body.len() as u64,
                sha256: hex(&Sha256::digest(body)),
                optional: false,
            }],
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("建 runtime")
            .block_on(f)
    }

    /// 规格：编译进来的那份必须自洽（哈希形状 / 体积 / 必需文件数）。
    #[test]
    fn 编进二进制的规格必须自洽() {
        let s = spec();
        assert_eq!(s.dim, 512, "维度要与 embed::DIM 一致");
        assert_eq!(s.dim as usize, embed::DIM);
        assert!(s.sources.len() >= 2, "要有兜底源：{:?}", s.sources);
        assert!(s.sources[0].url.contains("{file}"), "URL 模板要有占位符");
        let req = required_files();
        assert!(
            req.len() >= 3,
            "必需文件至少三个（config/tokenizer/safetensors）"
        );
        let bytes: u64 = req.iter().map(|f| f.bytes).sum();
        assert!(bytes > 90_000_000, "必需文件总量看着不对：{bytes}");
        for f in &s.files {
            assert_eq!(
                f.sha256.len(),
                64,
                "{} 的 sha256 不是 64 位十六进制",
                f.name
            );
            assert!(
                f.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                "{} 的 sha256 有非十六进制字符",
                f.name
            );
        }
    }

    /// 自检要能分辨三种状态：没装 / 装了但坏 / 就绪。
    #[test]
    fn 自检要能分辨没装_装坏了_就绪() {
        let dir = tmp("status");
        let body = b"hello-model".to_vec();

        // ① 一个都没有 → Absent（干净的单 exe）
        let st = status_in(&dir);
        assert!(matches!(st, Status::Absent { .. }), "{st:?}");
        assert!(st.line().contains("未安装"), "{}", st.line());

        // ② 有文件但哈希对不上 → Broken（拿证据说话，不说"没装"）
        std::fs::write(dir.join("config.json"), b"garbage").unwrap();
        let st = status_in(&dir);
        match &st {
            Status::Broken { problems, .. } => {
                assert!(
                    problems.iter().any(|p| p.contains("config.json")),
                    "{problems:?}"
                );
            }
            other => panic!("应当 Broken，实际 {other:?}"),
        }
        assert!(st.line().contains("不完整"), "{}", st.line());

        // ③ 齐了且哈希对 → Ready
        for f in required_files() {
            std::fs::write(dir.join(&f.name), &body).ok();
        }
        // 用真实哈希写不进去（body 不是真文件）——所以这里直接断言"不会假装就绪"
        assert!(!status_in(&dir).is_ready(), "哈希不对就不许说就绪");
    }

    /// 下载成功：**先 .part 再改名**，落盘后哈希与体积都对，且不带 .part 残留。
    #[test]
    fn 下载成功要原子落盘且不留半截文件() {
        let body = b"weights-bytes-0123456789".to_vec();
        let base = stub(vec![("w.bin".into(), body.clone())], false);
        let dir = tmp("ok");
        let spec = tiny_spec(&base, &body);
        let mut seen = Vec::new();
        let rep = block_on(fetch_into(&spec, &dir, |p| seen.push(p))).expect("应当下成功");
        assert_eq!(rep.fetched, vec!["w.bin".to_string()]);
        assert_eq!(std::fs::read(dir.join("w.bin")).unwrap(), body);
        assert!(!dir.join("w.bin.part").exists(), "半截文件不许留下");
        assert!(seen.iter().any(|p| p.done == body.len() as u64), "要报进度");
        assert!(status_with(&spec, &dir).is_ready(), "下完就该就绪");
    }

    /// **校验不过 = 不落地**：这是"宁可失败也别让未知字节变成已安装"的落点。
    #[test]
    fn 哈希不符时必须失败且不许留下任何文件() {
        let body = b"weights-bytes-0123456789".to_vec();
        let base = stub(vec![("w.bin".into(), body.clone())], true); // 服务端改了字节
        let dir = tmp("bad");
        let spec = tiny_spec(&base, &body); // 清单要的是**原**字节
        let err = block_on(fetch_into(&spec, &dir, |_| {})).expect_err("必须失败");
        assert!(err.contains("哈希不符"), "{err}");
        assert!(!dir.join("w.bin").exists(), "坏文件不许叫成品名");
        assert!(!dir.join("w.bin.part").exists(), "半截 .part 必须清掉");
        assert!(
            matches!(status_with(&spec, &dir), Status::Absent { .. }),
            "仍然算没装"
        );
    }

    /// 幂等：文件在且哈希对就跳过（断网重跑不重下）。
    #[test]
    fn 已有且哈希对就跳过() {
        let body = b"weights-bytes-0123456789".to_vec();
        let base = stub(vec![("w.bin".into(), body.clone())], false);
        let dir = tmp("idem");
        let spec = tiny_spec(&base, &body);
        block_on(fetch_into(&spec, &dir, |_| {})).expect("第一次");
        let rep = block_on(fetch_into(&spec, &dir, |_| {})).expect("第二次");
        assert_eq!(rep.skipped, vec!["w.bin".to_string()], "第二次应跳过");
        assert!(rep.fetched.is_empty(), "不该重下");
    }

    /// 兜底源拿到别的字节 → 拒绝（不许静默接受未知字节）。
    #[test]
    fn 换源字节不同时必须拒绝而不是接受() {
        let want = b"primary-bytes".to_vec();
        let other = b"different-bytes".to_vec();
        let base = stub(vec![("w.bin".into(), other)], false);
        let dir = tmp("fallback");
        let spec = tiny_spec(&base, &want);
        let err = block_on(fetch_into(&spec, &dir, |_| {})).expect_err("必须失败");
        assert!(err.contains("哈希不符"), "{err}");
        assert!(!dir.join("w.bin").exists());
    }
}
