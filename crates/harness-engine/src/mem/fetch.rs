//! **模型运行时兜底**：记忆嵌入模型的自检 + 按需下载（切片 4）。
//!
//! 治的是什么病：发行形态是"一个可执行文件 + `global/` + `projects/` + `plugins/`"，所以
//! **只有单 exe** 不是边缘情况，是主路径之一。此前只有**打包期**的取模型步骤
//! （`scripts/fetch-embed-model.mjs`），于是单 exe 用户拿到的是"静默降级"：语义检索关掉了，
//! 界面上没有任何出口把它打开。
//!
//! 关键一招：**规格编进二进制**。96MB 权重既不进 git、也不该硬编码在代码里 —— 但"这份模型
//! **是什么**"（文件名 / 体积 / sha256 / 来源 URL）只有几百字节，用 `include_str!` 编进来。
//! 于是 **exe 自己知道该取什么、怎么校验**，权重落在外面（`<便携根>/global/memory/model/`）。
//! 同一份 `model-spec.json` 既喂运行时兜底、也喂打包期步骤 —— 谁都不许另抄一遍（抄一遍就会漂）。
//!
//! **机制在 [`crate::modelstore`]**（先 `.part` 再改名 / 换源不许改口径 / 幂等 —— 三条纪律
//! 由 modelstore 统一实现）。本文件只负责：这份模型**是什么**、放**哪**、以及**它自己**的
//! 那句人话（"语义检索"）。
//!
//! 与"降级"的关系：缺模型时记忆**照常工作**（退化成纯词法检索，注入块里说清楚）。这个模块
//! 只是把"怎么让它变完整"从"用户自己去 ModelScope 找"变成"一条命令 / 一个按钮"。

use std::path::PathBuf;
use std::sync::OnceLock;

use super::embed;
use crate::modelstore;

// 通用机制与类型从 modelstore 转出：**这个模块的老 API 一个都不许变**
// （宿主、示例、单测都按它们写的；类型改了就是三处一起改）
pub use crate::modelstore::{
    Progress, Report, Source, SpecFile, Status, fetch_into, hex, status_with,
};

/// 规格（`model-spec.json`，编译进二进制）。
const SPEC_JSON: &str = include_str!("model-spec.json");

/// 这个能力的名字（所有面向用户的文案都用它，避免一处写"语义检索"、另一处写"向量检索"）。
const LABEL: &str = "语义检索";

/// 编进二进制的规格（解析失败是**构建期**的问题，不该等到运行时才炸）。
pub fn spec() -> &'static modelstore::ModelSpec {
    static S: OnceLock<modelstore::ModelSpec> = OnceLock::new();
    S.get_or_init(|| {
        serde_json::from_str(SPEC_JSON).unwrap_or_else(|e| {
            panic!("mem::fetch: model-spec.json 解析失败（它是编译进来的，属于构建期缺陷）：{e}")
        })
    })
}

/// 必需文件（`optional` 的不算判据）。
pub fn required_files() -> Vec<&'static SpecFile> {
    spec().required_files()
}

/// 模型目录：`<模型根>/<dir_name>`（与 `embed::model_dir` 同一口径）。
pub fn target_dir() -> Result<PathBuf, String> {
    embed::model_root()
        .map(|r| r.join(&spec().dir_name))
        .ok_or_else(|| "未配置模型目录（宿主未设、也没有 RUYIX_MEM_MODEL_DIR）".to_string())
}

/// 必需文件的总字节数（给界面报"要下多少"）。
pub fn required_bytes() -> u64 {
    spec().required_bytes()
}

impl Status {
    /// 记忆这条路的人话（`modelstore::Status::line_for` 的固定 label 版本）。
    pub fn line(&self) -> String {
        self.line_for(LABEL)
    }
}

/// 自检：必需文件在不在、哈希对不对。**这是唯一的判据来源**（不猜、不看目录大小）。
pub fn status() -> Status {
    let Ok(dir) = target_dir() else {
        return Status::NoRoot;
    };
    status_in(&dir)
}

/// 指定目录的自检（"检查另一个目录"用它；测试用 [`status_with`] 注入小规格）。
pub fn status_in(dir: &std::path::Path) -> Status {
    status_with(spec(), dir)
}

/// 用编译进来的规格下载到默认目录；失败时补一句"记忆怎么办"（降级是**设计**，不是事故）。
pub async fn fetch<F>(on: F) -> Result<Report, String>
where
    F: FnMut(Progress),
{
    let dir = target_dir()?;
    fetch_into(spec(), &dir, on)
        .await
        .map_err(|e| format!("{e}；记忆仍按词法模式工作"))
}

#[cfg(test)]
mod tests {
    use super::*;
    // 通用件搬去 modelstore 之后，测试要显式把它们带进来（原来是跟着 `use super::*` 一起来的）
    use crate::modelstore::{ModelSpec, Source, SpecFile};
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
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
