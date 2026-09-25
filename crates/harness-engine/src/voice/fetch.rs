//! 语音模型的**运行时兜底**：自检 + 按需下载。
//!
//! 机制在 [`crate::modelstore`]（`.part` 原子落盘 / 换源不许改口径 / 幂等），本文件只负责
//! "这份模型是什么、放哪、它自己那句人话"。
//!
//! **按需**在这里是硬要求：这是一份 **453MB** 的权重（比记忆的嵌入模型大近 5 倍），
//! 不能像记忆那样在启动时就去够 —— 用户点了「下载语音模型」才开始下。缺模型时录音按钮
//! 仍然可见（需求要它出现），但会说话：说清"要下多少、怎么下"。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::modelstore;

/// 规格（`voice-spec.json`，编译进二进制）。
const SPEC_JSON: &str = include_str!("voice-spec.json");

/// 这条路的人话标签。
const LABEL: &str = "语音转写";

/// 模型目录根（宿主设为 `<便携根>/global/voice/model`；测试可用环境变量覆盖）。
static ROOT: Mutex<Option<PathBuf>> = Mutex::new(None);

/// 宿主启动时设一次。
pub fn set_model_root(p: impl Into<PathBuf>) {
    if let Ok(mut g) = ROOT.lock() {
        *g = Some(p.into());
    }
}

/// 模型根：优先宿主设的，其次环境变量（探针/测试用），都没有就返回 `None`（= NoRoot）。
pub fn model_root() -> Option<PathBuf> {
    let from_host = ROOT.lock().ok().and_then(|g| g.as_ref().cloned());
    if from_host.is_some() {
        return from_host;
    }
    std::env::var("RUYIX_VOICE_MODEL_DIR")
        .ok()
        .map(PathBuf::from)
}

/// 编进二进制的规格（解析失败是构建期缺陷，不该等到运行时才炸）。
pub fn spec() -> &'static modelstore::ModelSpec {
    static S: OnceLock<modelstore::ModelSpec> = OnceLock::new();
    S.get_or_init(|| {
        serde_json::from_str(SPEC_JSON).unwrap_or_else(|e| {
            panic!("voice::fetch: voice-spec.json 解析失败（它是编译进来的，属于构建期缺陷）：{e}")
        })
    })
}

/// 模型目录：`<模型根>/<dir_name>`。
pub fn target_dir() -> Result<PathBuf, String> {
    model_root()
        .map(|r| r.join(&spec().dir_name))
        .ok_or_else(|| "未配置语音模型目录（宿主未设、也没有 RUYIX_VOICE_MODEL_DIR）".to_string())
}

pub fn required_files() -> Vec<&'static modelstore::SpecFile> {
    spec().required_files()
}

pub fn required_bytes() -> u64 {
    spec().required_bytes()
}

/// 要下多少 MB（界面上的按钮得先告诉用户代价：这是 453MB 级别的东西）。
pub fn required_mb() -> u64 {
    required_bytes().div_ceil(1_000_000)
}

impl modelstore::Status {
    /// 语音这条路的人话。
    pub fn voice_line(&self) -> String {
        self.line_for(LABEL)
    }
}

/// 自检：三个文件在不在、哈希对不对。
pub fn status() -> modelstore::Status {
    let Ok(dir) = target_dir() else {
        return modelstore::Status::NoRoot;
    };
    modelstore::status_with(spec(), &dir)
}

pub fn status_in(dir: &Path) -> modelstore::Status {
    modelstore::status_with(spec(), dir)
}

/// 按需下载到默认目录。失败时补一句"录音还能用在哪"（**降级是设计**：转写不了，录音照样存下来）。
pub async fn fetch<F>(on: F) -> Result<modelstore::Report, String>
where
    F: FnMut(modelstore::Progress),
{
    let dir = target_dir()?;
    modelstore::fetch_into(spec(), &dir, on)
        .await
        .map_err(|e| format!("{e}；录音仍会保存到项目桶，只是暂时转不成文字"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 编进二进制的规格必须自洽（哈希形状 / 体积 / 来源）——
    /// 这条不吃网络也不吃 453MB 权重，是"规格写错"的第一道闸。
    #[test]
    fn 编进二进制的规格必须自洽() {
        let s = spec();
        assert!(
            s.dir_name.contains("whisper"),
            "目录名要能认出是哪个模型：{}",
            s.dir_name
        );
        assert!(s.sources.len() >= 2, "要有兜底源：{:?}", s.sources);
        assert!(s.sources[0].url.contains("{file}"), "URL 模板要有占位符");
        let req = required_files();
        assert_eq!(
            req.len(),
            3,
            "必需文件是 model.gguf / tokenizer.json / config.json"
        );
        let names: Vec<&str> = req.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"model.gguf"), "{names:?}");
        assert!(names.contains(&"tokenizer.json"), "{names:?}");
        assert!(names.contains(&"config.json"), "{names:?}");
        let bytes = s.required_bytes();
        assert!(
            (400_000_000..600_000_000).contains(&bytes),
            "权重总量看着不对：{bytes}（large-v3-turbo q4_k 应在 450MB 上下）"
        );
        for f in &s.files {
            assert_eq!(f.sha256.len(), 64, "{} 的 sha256 不是 64 位", f.name);
            assert!(
                f.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                "{} 的 sha256 有非十六进制字符",
                f.name
            );
        }
    }

    /// 目录键没配时要能说清"没配"，而不是假装"没装"（两者下一步不一样）。
    #[test]
    fn 没配模型目录要说没配而不是没装() {
        // 测试里不设 ROOT、也清掉环境变量
        unsafe { std::env::remove_var("RUYIX_VOICE_MODEL_DIR") };
        *ROOT.lock().unwrap() = None;
        assert!(matches!(status(), modelstore::Status::NoRoot));
        assert!(status().voice_line().contains("未配置模型目录"));
    }
}
