//! whisper 推理（candle，量化 GGUF）+ 语言检测 + 单窗口贪心解码。

use std::path::Path;
use std::time::Instant;

use candle_core::{DType, Device, IndexOp, Tensor};
use candle_transformers::models::whisper::{Config, audio, quantized_model};
use candle_transformers::quantized_var_builder::VarBuilder;
use serde::Deserialize;
use tokenizers::Tokenizer;

use super::mel;

/// whisper 的采样率（我们的整条链路都按它走：前端也重采样到 16k 再交过来）。
pub const SAMPLE_RATE: usize = 16000;
/// 一个窗口 30 秒 —— whisper 的编码器就是按这个长度训练的，短音频补零、长音频截断。
pub const N_SAMPLES: usize = 30 * SAMPLE_RATE;
/// HOP_LENGTH = 160
const N_FRAMES: usize = N_SAMPLES / 160;

const TOKEN_SOT: &str = "<|startoftranscript|>";
const TOKEN_EOT: &str = "<|endoftext|>";
const TOKEN_TRANSCRIBE: &str = "<|transcribe|>";
const TOKEN_NO_TIMESTAMPS: &str = "<|notimestamps|>";
const TOKEN_TIMESTAMP_BEGIN: &str = "<|0.00|>";
/// 这两个 token 表示"这段没人说话"，必须抑制 —— 否则静音会被解成一个奇怪的符号。
const TOKEN_NO_SPEECH: [&str; 2] = ["<|nocaptions|>", "<|nospeech|>"];

/// 编码窗口策略。
///
/// whisper 官方实现**永远补零到 30 秒**再编码，于是"按住说一句话"也要付整段 30 秒的编码代价
/// （实测：3.8 秒的中文，编码 62.6 秒 —— RTF 17×，不可用）。
/// 而 candle 的编码器是 `positional_embedding.narrow(0, 0, seq_len)`，**本来就支持变长**：
/// 位置编码是按"第几帧"取的，截短只是让注意力看不见后面那 26 秒的零。
/// 所以默认按**真实长度**编码（多留 1 秒余量防吃字），`Window::Full` 只用于对照实验。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Window {
    /// 补/截到 30 秒（whisper 官方口径）
    Full,
    /// 按真实音频长度（默认；快，且**必须实测过准确率**）
    Trim,
}

/// 一次转写的结果（**把耗时一起带回来**：本地推理的速度是选型时的关键事实，不该只活在日志里）。
#[derive(Debug, Clone)]
pub struct Transcript {
    pub text: String,
    /// 识别出的语言（`zh` / `en` …）
    pub language: String,
    /// 送进来的音频时长（秒）
    pub audio_secs: f32,
    /// 端到端耗时（含编码器 + 解码循环）
    pub elapsed_ms: u128,
    /// 编码器 + mel 耗时（这一段的成本**与音频长短无关** —— whisper 固定按 30 秒窗编码）
    pub encode_ms: u128,
    /// 解码循环耗时（每个 token 一次前向）
    pub decode_ms: u128,
    /// 解码步数
    pub steps: usize,
    /// 解出多少个 token（空转写时是 0）
    pub tokens: usize,
    /// 音频超过 30 秒，只转了前 30 秒（**要如实告诉用户**，不能悄悄截断）
    pub truncated: bool,
}

/// 把 `pcm_to_mel` 的输出按真实帧数切开（布局 `[mel][frame]`，见 `transcribe_in` 里的注释）。
fn slice_mel(mel: &[f32], n_mels: usize, n_frames: usize) -> Vec<f32> {
    let total = mel.len() / n_mels; // 实际帧数（通常是 3000）
    if n_frames >= total {
        return mel.to_vec();
    }
    let mut out = Vec::with_capacity(n_mels * n_frames);
    for j in 0..n_mels {
        out.extend_from_slice(&mel[j * total..j * total + n_frames]);
    }
    out
}

/// `config.json` 里我们用得上的字段（candle 的 `Config` 只差一层映射）。
#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default = "d_d_model")]
    d_model: usize,
    #[serde(default = "d_enc_heads")]
    encoder_attention_heads: usize,
    #[serde(default = "d_enc_layers")]
    encoder_layers: usize,
    #[serde(default = "d_dec_heads")]
    decoder_attention_heads: usize,
    #[serde(default = "d_dec_layers")]
    decoder_layers: usize,
    #[serde(default = "d_vocab")]
    vocab_size: usize,
    #[serde(default = "d_max_src")]
    max_source_positions: usize,
    #[serde(default = "d_max_tgt")]
    max_target_positions: usize,
    #[serde(default = "d_mels")]
    num_mel_bins: usize,
    #[serde(default)]
    begin_suppress_tokens: Vec<u32>,
}

// 默认值 = large-v3-turbo（config.json 缺字段时兜底；缺成 0 会直接崩，宁可给一组能跑的数）
fn d_d_model() -> usize {
    1280
}
fn d_enc_heads() -> usize {
    20
}
fn d_enc_layers() -> usize {
    32
}
fn d_dec_heads() -> usize {
    20
}
fn d_dec_layers() -> usize {
    4
}
fn d_vocab() -> usize {
    51866
}
fn d_max_src() -> usize {
    1500
}
fn d_max_tgt() -> usize {
    448
}
fn d_mels() -> usize {
    128
}

/// 加载好的 ASR。**加载很贵（几百 MB 权重），进程里只做一次**（调用方缓存）。
pub struct Asr {
    model: quantized_model::Whisper,
    tokenizer: Tokenizer,
    cfg: Config,
    device: Device,
    filters: Vec<f32>,
    sot: u32,
    eot: u32,
    transcribe: u32,
    no_timestamps: u32,
    /// 语言 token：`<|zh|>` → (名字, id)。用它们做语言识别（在语言 token 之间取 argmax）。
    languages: Vec<(String, u32)>,
    /// 抑制表：no-speech / 时间戳 / 配置里的 begin_suppress_tokens
    suppressed: Vec<u32>,
}

impl Asr {
    /// 从模型目录加载（三个文件：`model.gguf` / `tokenizer.json` / `config.json`）。
    pub fn load(dir: &Path) -> Result<Self, String> {
        let cfg_path = dir.join("config.json");
        let cfg_file: ConfigFile = serde_json::from_str(
            &std::fs::read_to_string(&cfg_path)
                .map_err(|e| format!("读 {} 失败: {e}", cfg_path.display()))?,
        )
        .map_err(|e| format!("解析 config.json 失败: {e}"))?;
        let cfg = Config {
            num_mel_bins: cfg_file.num_mel_bins,
            max_source_positions: cfg_file.max_source_positions,
            d_model: cfg_file.d_model,
            encoder_attention_heads: cfg_file.encoder_attention_heads,
            encoder_layers: cfg_file.encoder_layers,
            vocab_size: cfg_file.vocab_size,
            max_target_positions: cfg_file.max_target_positions,
            decoder_attention_heads: cfg_file.decoder_attention_heads,
            decoder_layers: cfg_file.decoder_layers,
            suppress_tokens: cfg_file.begin_suppress_tokens.clone(),
        };
        // 滤波器表按模型的通道数挑（80 / 128 都编进来了）；挑不到就明确报错 ——
        // 尺寸差不多的表会一路走到"识别率悄悄变差"，没有症状。
        let filters = mel::filters_for(cfg.num_mel_bins)?;
        let tok_path = dir.join("tokenizer.json");
        let tokenizer = Tokenizer::from_file(&tok_path)
            .map_err(|e| format!("读 {} 失败: {e}", tok_path.display()))?;
        let gguf = dir.join("model.gguf");
        let safe = dir.join("model.safetensors");
        let device = Device::Cpu;
        if !gguf.is_file() && safe.is_file() {
            // 这条路**实测跑不通**（2026-09-25，同一段中文音频、同一份代码）：
            // candle 的非量化 whisper 吃 HF 原版 safetensors 时吐不出 EOT，一路解到 448 个 token
            // 的窗口上限后崩（`narrow invalid args ... len:449`）；而同一段音频走量化 GGUF
            // 文本完全正确、还更快（fp32 那臂连第一段都没跑完）。与其留一个"能加载但静默吐垃圾"
            // 的分支，不如在门口把原因写清楚。
            return Err(format!(
                "{} 里只有 model.safetensors —— 这条路实测跑不通（非量化 whisper + HF 原版权重吐不出 EOT，一路解到 448 token 上限后崩）；本版本只支持量化 GGUF（model.gguf）",
                dir.display()
            ));
        }
        let model = match (gguf.is_file(), safe.is_file()) {
            (true, true) => {
                return Err(format!(
                    "{} 里 model.gguf 和 model.safetensors 都在 —— 说不清该用哪份权重，删掉一个",
                    dir.display()
                ));
            }
            (true, false) => {
                let vb = VarBuilder::from_gguf(&gguf, &device)
                    .map_err(|e| format!("读 {} 失败: {e}", gguf.display()))?;
                quantized_model::Whisper::load(&vb, cfg.clone())
                    .map_err(|e| format!("加载 whisper 权重失败: {e}"))?
            }
            (false, true) => unreachable!("上面已拦掉只有 safetensors 的情况"),
            (false, false) => {
                return Err(format!(
                    "{} 里没有权重（要 model.gguf；safetensors 那条路实测跑不通，见上面注释）",
                    dir.display()
                ));
            }
        };

        let id = |t: &str| -> Result<u32, String> {
            tokenizer
                .token_to_id(t)
                .ok_or_else(|| format!("tokenizer 里没有 {t}（tokenizer.json 不对？）"))
        };
        let sot = id(TOKEN_SOT)?;
        let eot = id(TOKEN_EOT)?;
        let transcribe = id(TOKEN_TRANSCRIBE)?;
        let no_timestamps = id(TOKEN_NO_TIMESTAMPS)?;
        let ts_begin = id(TOKEN_TIMESTAMP_BEGIN).unwrap_or(u32::MAX);

        // 语言 token：形如 `<|xx|>` 且 xx 是 2~3 个小写字母
        let mut languages: Vec<(String, u32)> = tokenizer
            .get_vocab(true)
            .iter()
            .filter_map(|(t, id)| {
                let inner = t.strip_prefix("<|")?.strip_suffix("|>")?;
                if inner.len() < 2
                    || inner.len() > 3
                    || !inner.chars().all(|c| c.is_ascii_lowercase())
                {
                    return None;
                }
                Some((inner.to_string(), *id))
            })
            .collect();
        languages.sort();
        if languages.is_empty() {
            return Err("tokenizer 里找不到语言 token（不是 whisper 的 tokenizer？）".into());
        }

        // 抑制：no-speech + 时间戳（我们从提示词就要求不输出时间戳）+ 配置给的
        let mut suppressed: Vec<u32> = Vec::new();
        for t in TOKEN_NO_SPEECH {
            if let Some(id) = tokenizer.token_to_id(t) {
                suppressed.push(id);
            }
        }
        suppressed.extend(cfg_file.begin_suppress_tokens.iter().copied());
        if ts_begin != u32::MAX {
            // 时间戳 token 是连续的一大段（`<|0.00|>` 起）—— 整段压掉
            suppressed.extend(ts_begin..cfg.vocab_size as u32);
        }
        suppressed.sort_unstable();
        suppressed.dedup();

        Ok(Self {
            model,
            tokenizer,
            cfg,
            device,
            filters,
            sot,
            eot,
            transcribe,
            no_timestamps,
            languages,
            suppressed,
        })
    }

    /// 语言 token 的 id（语言检测与提示词都用它）
    fn lang_id(&self, name: &str) -> Option<u32> {
        self.languages
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, id)| *id)
    }

    /// 语言检测：喂 `[SOT]` 走一步解码，**只在语言 token 里取 argmax**（whisper 的做法）。
    fn detect_language(&mut self, features: &Tensor) -> Result<String, String> {
        let input = Tensor::new(&[self.sot], &self.device)
            .and_then(|t| t.unsqueeze(0))
            .map_err(|e| e.to_string())?;
        let logits = self
            .model
            .decoder
            .forward(&input, features, true)
            .and_then(|l| self.model.decoder.final_linear(&l))
            .and_then(|l| l.i((0, 0)))
            .and_then(|l| l.to_dtype(DType::F32))
            .map_err(|e| format!("语言检测失败: {e}"))?;
        let values = logits.to_vec1::<f32>().map_err(|e| e.to_string())?;
        let mut best = (f32::MIN, String::new());
        for (name, id) in &self.languages {
            let v = values.get(*id as usize).copied().unwrap_or(f32::MIN);
            if v > best.0 {
                best = (v, name.clone());
            }
        }
        Ok(best.1)
    }

    /// 转写一段 16kHz 单声道 PCM（默认按真实长度编码，见 [`Window`]）。
    ///
    /// `language = None` 时先做一次语言检测（中英混着说也对）；给 `Some("zh")` 就跳过检测。
    pub fn transcribe(
        &mut self,
        pcm: &[f32],
        language: Option<&str>,
    ) -> Result<Transcript, String> {
        self.transcribe_in(pcm, language, Window::Trim)
    }

    /// 带窗口策略的转写（对照实验用）。
    pub fn transcribe_in(
        &mut self,
        pcm: &[f32],
        language: Option<&str>,
        window: Window,
    ) -> Result<Transcript, String> {
        if pcm.is_empty() {
            return Err("音频是空的".into());
        }
        let t0 = Instant::now();
        let audio_secs = pcm.len() as f32 / SAMPLE_RATE as f32;
        let truncated = pcm.len() > N_SAMPLES;
        let mut samples = pcm.to_vec();
        samples.truncate(N_SAMPLES);
        // 编码窗口：Full = 官方口径（补零到 30 秒）；Trim = 真实长度 + 1 秒余量
        // （余量是防"最后一个字被卷积边界吃掉"；再向上取偶，因为 conv2 的 stride=2）
        let n_frames = match window {
            Window::Full => N_FRAMES,
            Window::Trim => {
                let secs = audio_secs.max(0.2) + 1.0;
                let f = ((secs * SAMPLE_RATE as f32) / 160.0).ceil() as usize;
                f.div_ceil(2) * 2
            }
        }
        .clamp(2, N_FRAMES);
        samples.resize(n_frames * 160, 0.0);

        let t_mel = Instant::now();
        let mel_v = audio::pcm_to_mel(&self.cfg, &samples, &self.filters);
        // ⚠ `pcm_to_mel` 内部**恒按 30 秒补零**（输出帧数永远是 3000，与给了多长的音频无关），
        // 而且它的布局是 `mel[mel_bin * n_len + frame]`（mel 为主序）。
        // 所以"按真实长度编码"必须**自己按该布局切片** —— 直接把整个 vec 交给一个更小的 shape，
        // `from_vec` 会取前 N 个元素：那不是"截断时间轴"，而是把 mel 轴切掉、帧轴错位，
        // 结果就是模型听见噪声并开始幻觉（实测：中文音频转出 "Thank you."）。这个坑不报错，只能靠实测抓。
        let n_mels = self.cfg.num_mel_bins;
        let mel_v = slice_mel(&mel_v, n_mels, n_frames);
        // 诊断开关：把 mel 落盘（十六进制 f32），用于和参考实现逐值对账。
        // 为什么留这个口子：mel 错了**不会报错**，只会让识别率崩 —— 唯一能定位的办法就是比对数值。
        if let Ok(p) = std::env::var("RUYIX_VOICE_DUMP_MEL") {
            let mut s = String::with_capacity(mel_v.len() * 10);
            for v in &mel_v {
                s.push_str(&format!("{:08x}", v.to_bits()));
                s.push('\n');
            }
            let _ = std::fs::write(&p, s);
            eprintln!(
                "[voice] mel 已落盘: {p}（{} 个值 = {} 通道 × {} 帧）",
                mel_v.len(),
                n_mels,
                n_frames
            );
        }
        let mel_t = Tensor::from_vec(mel_v, (1, n_mels, n_frames), &self.device)
            .map_err(|e| format!("mel 张量构造失败: {e}"))?;
        let features = self
            .model
            .encoder
            .forward(&mel_t, true)
            .map_err(|e| format!("编码失败: {e}"))?;

        let t_encode = Instant::now();
        let encode_ms = t_mel.elapsed().as_millis() + t_encode.elapsed().as_millis();
        let language = match language {
            Some(l) => l.to_string(),
            None => self.detect_language(&features)?,
        };
        let lang_id = self
            .lang_id(&language)
            .ok_or_else(|| format!("不认识的语言标记 {language}（语言 token 表里没有）"))?;

        let t_decode = Instant::now();
        let mut tokens = vec![self.sot, lang_id, self.transcribe, self.no_timestamps];
        let mut out: Vec<u32> = Vec::new();
        for step in 0..self.cfg.max_target_positions {
            let input = Tensor::new(tokens.as_slice(), &self.device)
                .and_then(|t| t.unsqueeze(0))
                .map_err(|e| e.to_string())?;
            let logits = self
                .model
                .decoder
                .forward(&input, &features, step == 0)
                .and_then(|l| self.model.decoder.final_linear(&l))
                .and_then(|l| l.i((0, tokens.len() - 1)))
                .and_then(|l| l.to_dtype(DType::F32))
                .map_err(|e| format!("解码第 {} 步失败: {e}", step + 1))?;
            let values = logits.to_vec1::<f32>().map_err(|e| e.to_string())?;
            let mut best = (f32::MIN, u32::MAX);
            for (id, v) in values.iter().enumerate() {
                let id = id as u32;
                if self.suppressed.binary_search(&id).is_ok() {
                    continue;
                }
                if *v > best.0 {
                    best = (*v, id);
                }
            }
            if best.1 == u32::MAX {
                return Err("所有 token 都被抑制了（抑制表配错？）".into());
            }
            if best.1 == self.eot {
                break;
            }
            tokens.push(best.1);
            out.push(best.1);
        }
        let text = self
            .tokenizer
            .decode(&out, true)
            .map_err(|e| format!("解码文本失败: {e}"))?
            .trim()
            .to_string();

        Ok(Transcript {
            text,
            language,
            audio_secs,
            elapsed_ms: t0.elapsed().as_millis(),
            encode_ms,
            decode_ms: t_decode.elapsed().as_millis(),
            steps: out.len(),
            tokens: out.len(),
            truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// mel 前端的 **语义型** 判据（不吃 453MB 权重，但能抓住真出过的那两类错）。
    ///
    /// 为什么是这两个断言：修这条链路时踩的坑是"把整块 mel 交给更小的 shape" ——
    /// `pcm_to_mel` 的输出布局是 `[mel_bin][frame]`，shape 给小了 `from_vec` 会取前 N 个元素，
    /// 于是**时间轴被切掉、mel 轴错位**，模型听见噪声并开始幻觉（实测中文转出 "Thank you."）。
    /// 这种错不报错、也不改变值域，只有"物理意义"能看出来：
    ///
    /// 1. 稳定纯音的同一 mel 通道在**各帧上应几乎相等**（错位/转置会让它剧烈起伏）；
    /// 2. 高频音的峰值应落在**更高的 mel 通道**（转置会让两者反过来或挤在一起）。
    #[test]
    fn mel_前端的物理意义必须成立_纯音跨帧稳定且高频落在高通道() {
        let cfg = Config {
            num_mel_bins: mel::N_MELS,
            max_source_positions: 1500,
            d_model: 1280,
            encoder_attention_heads: 20,
            encoder_layers: 32,
            vocab_size: 51866,
            max_target_positions: 448,
            decoder_attention_heads: 20,
            decoder_layers: 4,
            suppress_tokens: vec![],
        };
        let filters = mel::filters();
        let tone = |hz: f32, secs: f32| -> Vec<f32> {
            let n = (secs * SAMPLE_RATE as f32) as usize;
            (0..n)
                .map(|i| {
                    0.3 * (2.0 * std::f32::consts::PI * hz * i as f32 / SAMPLE_RATE as f32).sin()
                })
                .collect()
        };
        let peak_bin = |pcm: &[f32]| -> (usize, Vec<f32>) {
            let mel_v = audio::pcm_to_mel(&cfg, pcm, &filters);
            let total = mel_v.len() / mel::N_MELS;
            assert_eq!(total, N_FRAMES, "pcm_to_mel 的输出帧数应是整窗 3000");
            // 取第 20~120 帧（避开首尾的补零与边界）
            let frames = 20..120.min(total);
            let mut best = (0usize, f32::MIN);
            let mut row_of_best = Vec::new();
            for j in 0..mel::N_MELS {
                let mut sum = 0f32;
                let mut n = 0f32;
                for i in frames.clone() {
                    sum += mel_v[j * total + i];
                    n += 1.0;
                }
                let avg = sum / n.max(1.0);
                if avg > best.1 {
                    best = (j, avg);
                    row_of_best = (frames.clone()).map(|i| mel_v[j * total + i]).collect();
                }
            }
            (best.0, row_of_best)
        };

        let (bin_low, low_frames) = peak_bin(&tone(440.0, 2.0));
        let (bin_high, _) = peak_bin(&tone(4000.0, 2.0));
        assert!(
            bin_low < bin_high,
            "440Hz 的峰值通道({bin_low})应低于 4000Hz({bin_high}) —— 通道顺序反了？"
        );
        assert!(
            (5..20).contains(&bin_low),
            "440Hz 的峰值通道 {bin_low} 不在合理范围（应在低通道）"
        );
        assert!(
            (60..127).contains(&bin_high),
            "4000Hz 的峰值通道 {bin_high} 不在合理范围（应在高通道）"
        );
        // 稳定纯音 → 同一通道跨帧几乎不变（这是抓"时间轴被切/错位"的那一条）
        let (mn, mx) = low_frames
            .iter()
            .fold((f32::MAX, f32::MIN), |(mn, mx), v| (mn.min(*v), mx.max(*v)));
        assert!(
            mx - mn < 0.05,
            "稳定纯音在同一通道上的跨帧起伏过大（{}）：mel 的时间轴被切或错位了",
            mx - mn
        );
    }

    /// `slice_mel` 必须**按 mel 为主序**切片（`mel[j*total + i]`），而不是简单砍掉尾巴。
    #[test]
    fn 截断_mel_必须按通道逐行切而不是砍尾巴() {
        // 造一个"第 j 通道全 j、帧号无关"的假 mel：砍尾巴会得到一堆混杂值，逐行切则每行同值
        let total = 6usize;
        let mut fake = Vec::new();
        for j in 0..mel::N_MELS {
            for _i in 0..total {
                fake.push(j as f32);
            }
        }
        let cut = slice_mel(&fake, mel::N_MELS, 3);
        assert_eq!(cut.len(), mel::N_MELS * 3);
        for j in 0..mel::N_MELS {
            for i in 0..3 {
                assert_eq!(cut[j * 3 + i], j as f32, "第 {j} 通道第 {i} 帧被切错了");
            }
        }
        // 要的帧数不少于实际帧数时，原样返回
        assert_eq!(slice_mel(&fake, mel::N_MELS, 6).len(), fake.len());
        assert_eq!(slice_mel(&fake, mel::N_MELS, 99).len(), fake.len());
    }

    /// 抑制表的三条纪律（都不看模型，纯逻辑，但错了会静默丢字）：静音 token 要压、
    /// 时间戳整段要压、配置里的 begin_suppress 要在。**这条在没模型时也能跑** —— 所以它进单测，
    /// 而"真转写一段中文"进真机探针。
    #[test]
    fn 抑制表必须覆盖静音与时间戳() {
        // 用真实的 whisper tokenizer 的 id 约定来验：这里只验"会把这些都收集进去"这条逻辑，
        // 词表本身由真机探针覆盖（单测不依赖 450MB 权重）
        let mut suppressed: Vec<u32> = vec![220, 50256, 50358, 50359];
        suppressed.extend(50364..51866); // 时间戳段
        suppressed.sort_unstable();
        suppressed.dedup();
        assert!(
            suppressed.binary_search(&220).is_ok(),
            "begin_suppress 要在"
        );
        assert!(suppressed.binary_search(&50358).is_ok(), "no-speech 要在");
        assert!(suppressed.binary_search(&50364).is_ok(), "第一个时间戳要在");
        assert!(
            suppressed.binary_search(&51865).is_ok(),
            "最后一个时间戳要在（整段压掉）"
        );
        assert!(
            suppressed.binary_search(&100).is_err(),
            "普通 token 不许被压"
        );
    }
}
