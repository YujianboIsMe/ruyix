//! 本地语音转写（ASR）：**纯 Rust**（candle 的 whisper），不引入任何 C++ 组件。
//!
//! ## 为什么是 candle 而不是 whisper.cpp
//!
//! 本项目的发行形态是"一个可执行文件 + `global/` + `projects/` + `plugins/`"：
//! whisper.cpp 要么静态编进 exe（引 cmake + C++ 工具链），要么多带一个 sidecar 文件，
//! 两条都破坏这个形态；而 candle 已经在依赖里（项目记忆的嵌入模型就是它），`cargo build`
//! 一条命令就位。另外音频解码也不用进 Rust —— WebView 自带 WebAudio，前端把 opus 解成
//! 16kHz 单声道 PCM 再交过来，这一侧只吃浮点数组。
//!
//! ## 模型：candle 原生格式的量化 GGUF（不是 whisper.cpp 那批）
//!
//! 这一段踩了两个坑，都记在这里，免得下次再踩：
//! 1. whisper.cpp 官方发布的是**老 GGML `.bin`**（11 个 hparams + 文件内 mel filters +
//!    无 score 词表），而 candle 的 `ggml_file` 是按 **llama.cpp 布局**写的（7 个 hparams +
//!    token/score 词表）⇒ 读不进来；
//! 2. 就算换成社区 **GGUF**，张量名也对不上：whisper.cpp 系是 `enc.`/`dec.` 或
//!    `encoder.blocks.0.attn.query`，而 candle 按 **HF/transformers 命名**取
//!    （`model.encoder.layers.0.self_attn.q_proj`）。
//!
//! 所以用的是 candle 原生格式的那批：`Demonthos/candle-quantized-whisper-large-v3-turbo`
//! （GGUF v2、587 张量、权重 q4_k + 归一化/偏置 f32、命名逐字对得上 candle 的取法）。
//! 选 large-v3-turbo 而不是 small 的原因：**small 没有 candle 原生格式的量化版**，
//! 而 turbo 是多语种、中文够用、比 large-v3 快数倍。
//!
//! ## mel 滤波器
//!
//! 表直接编进二进制（见 [`mel`]）—— 不自己算公式，因为算出来的**形状对、数值不等于对**，
//! 而这种错不会报错，只会让识别率悄悄变差。

pub mod asr;
pub mod fetch;
pub mod mel;

pub use asr::{Asr, Transcript};
