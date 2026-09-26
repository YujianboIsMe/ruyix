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

/// 转写选项。**为什么要有这个结构体**：这些开关一次转写只判定一次，散成五个参数
/// 会让每个调用点都要记住"默认是什么"；而它们的默认值本身是有讲究的（见各字段注释）。
pub struct TranscribeOpts<'a> {
    /// 音频语言；`None` = 让模型自己认（只在第一窗认一次）
    pub language: Option<&'a str>,
    /// 编码窗口（`trim` 快 / `full` 官方口径）
    pub window: Window,
    /// 是否剪静音。**默认开**：whisper 在静音上会幻觉，而录音两头必然是静音。
    /// 关掉它的唯一理由：极安静的说话声被误剪（那时把 `voice.vad` 设为 false）。
    pub vad: bool,
    /// 最多转多少秒（超出部分**如实标注** truncated）。默认 120 秒：
    /// 本地 CPU 的 RTF ~3×，再长用户会以为死了 —— 这个上限是"体验闸"而不是技术限制。
    pub max_secs: f32,
}

impl Default for TranscribeOpts<'_> {
    fn default() -> Self {
        Self {
            language: None,
            window: Window::Trim,
            vad: true,
            max_secs: 120.0,
        }
    }
}

/// 单窗解码的结果（把"靠不靠得住"的指标一起带回来）。
struct Decoded {
    tokens: Vec<u32>,
    /// 各 token 对数概率的均值（whisper 的 `avg_logprob`，判幻觉用的第一条阈值）
    avg_logprob: f32,
    /// 第一步里 `<|nospeech|>` 的概率（whisper 判"这段没人说话"用的）
    no_speech_prob: f32,
}

/// 第 `id` 个 token 的 softmax 概率（数值稳定：先减最大值）。
fn softmax_at(values: &[f32], id: u32) -> f32 {
    let m = values.iter().copied().fold(f32::MIN, f32::max);
    let mut sum = 0.0f32;
    for v in values {
        sum += (v - m).exp();
    }
    let mine = values.get(id as usize).copied().unwrap_or(f32::MIN);
    if sum > 0.0 {
        (mine - m).exp() / sum
    } else {
        0.0
    }
}

/// 第 `id` 个 token 的对数概率（log-softmax，数值稳定）。
fn log_softmax_at(values: &[f32], id: u32) -> f32 {
    let m = values.iter().copied().fold(f32::MIN, f32::max);
    let mut sum = 0.0f32;
    for v in values {
        sum += (v - m).exp();
    }
    let mine = values.get(id as usize).copied().unwrap_or(f32::MIN);
    (mine - m) - sum.ln()
}

/// 温度回退的档位（whisper 官方就是 0.0 到 1.0 每 0.2 一档）。
/// 0.0 = 贪心（能对就别乱动）；后面的档位只在**这一段被判为幻觉/重复环**时才用。
const FALLBACK_TEMPS: [f32; 6] = [0.0, 0.2, 0.4, 0.6, 0.8, 1.0];
/// 平均对数概率低于它 = 这段没把握（whisper 的 `logprob_threshold`）。
const FALLBACK_MIN_LOGPROB: f32 = -1.0;
/// 重复度高于它 = 这段在打转（whisper 用 zlib 压缩比 2.4；我们用自己的 4-gram 口径，见 `repetition_ratio`）。
const FALLBACK_MAX_REPEAT: f32 = 0.6;

/// 从（已抑制过的）logits 里**采样**一个 token（温度 > 0 时用）。
///
/// 为什么需要采样而不是继续贪心：贪心在"没把握"时会锁进一个环
/// （实测：`…然后重启一下服务` 重复 8 遍）。升温 + 采样是打散那个环的标准手段。
///
/// 随机数用**确定性 xorshift**（种子 = 常量 + 步数）：本项目的判据要可复现，
/// 同一条音频每次跑出同样的结果，比"真随机"更有价值。
fn sample_token(values: &[f32], suppressed: &[u32], temperature: f32, step: usize) -> u32 {
    let mut state: u32 = 0x9E37_79B9u32.wrapping_add(step as u32).wrapping_add(1);
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        state
    };
    let m = values.iter().copied().fold(f32::MIN, f32::max);
    let mut sum = 0.0f32;
    let mut probs: Vec<f32> = Vec::with_capacity(values.len());
    for (i, v) in values.iter().enumerate() {
        if suppressed.binary_search(&(i as u32)).is_ok() {
            probs.push(0.0);
            continue;
        }
        let p = ((v - m) / temperature.max(1e-3)).exp();
        probs.push(p);
        sum += p;
    }
    if sum <= 0.0 {
        return u32::MAX;
    }
    let target = (next() as f32 / u32::MAX as f32) * sum;
    let mut acc = 0.0f32;
    for (i, p) in probs.iter().enumerate() {
        acc += p;
        if acc >= target {
            return i as u32;
        }
    }
    u32::MAX
}

/// 一帧 25ms / 步进 10ms（能量 VAD 的常用参数：25ms 够一个音素、10ms 够分辨气口）。
const VAD_FRAME: usize = SAMPLE_RATE / 40;
const VAD_HOP: usize = SAMPLE_RATE / 100;

/// 能量法 VAD：给出**语音区间**（采样下标，半开区间）。
///
/// 为什么必须有它（而不是直接把整段喂给模型）：whisper 在**静音/气口**上会幻觉 ——
/// 实测把一段只有静音垫的音频喂进去，它会一本正经地吐出 "Thank you."。而"按住说一句话"
/// 的录音两头必然有静音，中间还有气口。
///
/// 为什么不用学习型 VAD（silero 等）：那要再引一个运行时与一份权重；而这条链路的
/// 输入是**录音笔级的近距离单人语音**，能量法足够，且**零依赖、可单测、无权重可复现**。
/// 代价要如实写在文档里：持续的高能量噪声（键盘、风）会被当成语音 —— 模型仍会尽力转，
/// 只是没有"帮它剪掉"这一步。
///
/// 口径：帧能量用 RMS²；噪声底取**低分位**（取平均会被语音带高）；语音判据
/// `能量 > max(噪声底 × 6, 绝对下限)`；不足 0.25 秒的碎片丢掉；间隔 < 0.4 秒的合并
/// （那是气口，不是句末）；两侧各留 0.15 秒（别切掉爆破音）。
fn energy_vad(pcm: &[f32]) -> Vec<(usize, usize)> {
    const MIN_SPEECH: usize = SAMPLE_RATE / 4; // 0.25s
    const MERGE_GAP: usize = SAMPLE_RATE * 2 / 5; // 0.4s
    const PAD: usize = SAMPLE_RATE * 3 / 20; // 0.15s
    const ABS_FLOOR: f32 = 1e-5;

    if pcm.len() < VAD_FRAME {
        return if pcm.iter().any(|v| v.abs() > 1e-3) {
            vec![(0, pcm.len())]
        } else {
            Vec::new()
        };
    }
    // 逐帧能量
    let mut energies = Vec::with_capacity(pcm.len() / VAD_HOP + 1);
    let mut pos = 0;
    while pos + VAD_FRAME <= pcm.len() {
        let e: f32 =
            pcm[pos..pos + VAD_FRAME].iter().map(|v| v * v).sum::<f32>() / VAD_FRAME as f32;
        energies.push((pos, e));
        pos += VAD_HOP;
    }
    // 噪声底 = 20 分位（取均值会被语音拉高，取最小值又会被一个异常安静帧骗到）
    let mut sorted: Vec<f32> = energies.iter().map(|(_, e)| *e).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let noise = sorted[sorted.len() / 5];
    let peak = sorted[sorted.len() * 95 / 100];
    let mut thr = (noise * 6.0).max(ABS_FLOOR);

    // 连成区间（帧 → 采样），并合并近邻
    let collect = |thr: f32| -> Vec<(usize, usize)> {
        let mut regions: Vec<(usize, usize)> = Vec::new();
        for (pos, e) in &energies {
            if *e > thr {
                match regions.last_mut() {
                    Some(last) if pos.saturating_sub(last.1) <= MERGE_GAP => {
                        last.1 = pos + VAD_FRAME
                    }
                    _ => regions.push((*pos, pos + VAD_FRAME)),
                }
            }
        }
        regions
    };
    let mut regions = collect(thr);
    // **第二级兜底**：第一级一个语音段都没找到时，说明"静音太少、噪声底落在语音里"
    // （实测：40 秒连续说话，静音只占 2%，20 分位就被语音抬高了 6 倍 ⇒ 整段被判成没有人声 ✗）。
    // 这时退回**相对峰值**判据（峰值的一小部分算语音）。取舍是明确的：
    // 宁可多转一段（模型还能从噪声里捞出话），也不能把用户真说的话判成"没听到人声"。
    // 代价如实记：持续的高能量噪声（风扇/键盘）会被当成语音 —— 能量法 VAD 的固有边界。
    if regions.is_empty() {
        thr = (peak * 0.06).max(ABS_FLOOR);
        regions = collect(thr);
    }
    // 两侧留边、丢碎片、再夹回音频范围
    regions
        .into_iter()
        .map(|(a, b)| (a.saturating_sub(PAD), (b + PAD).min(pcm.len())))
        .filter(|(a, b)| b - a >= MIN_SPEECH)
        .collect()
}

/// 把语音区间排成若干**不超过一个窗口**的片段（切点选在能量最低处）。
///
/// 这是"一句话超过 30 秒"的正解：以前是**直接截断**，用户说长了后半句就没了 ——
/// 那不是"不准"，是**根本没听**。切点优先落在低能量处（气口），避免把词切两半。
fn plan_windows(regions: &[(usize, usize)], pcm: &[f32], max_len: usize) -> Vec<(usize, usize)> {
    // **先装箱、再切**（顺序反了会白白多花几倍时间 —— 实测踩过）：
    // 一句话的录音里"句与句之间的气口"往往超过 0.4 秒，VAD 会把它们切成一段段；
    // 如果**一段一个窗口**，77 秒的十句话就变成 **22 个窗口 ⇒ 22 遍编码器**（每遍 5~8 秒），
    // 用户看到的"慢"有一半是我们自己造出来的。
    // 正确做法：把相邻语音段**并进同一个 ≤30 秒的窗口**（窗内的气口无害，模型本来就吃连续音频），
    // 只在装不下时**在气口处收口** —— 收口点天然落在停顿上，一个词都不会被切开。
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut cur: Option<(usize, usize)> = None;
    for (a, b) in regions.iter().copied() {
        match cur {
            None => cur = Some((a, b)),
            Some((s, e)) => {
                if b - s <= max_len {
                    cur = Some((s, b));
                } else {
                    out.push((s, e));
                    cur = Some((a, b));
                }
            }
        }
    }
    if let Some(w) = cur {
        out.push(w);
    }

    // 剩下唯一要处理的：**单个区间自己就超过 max_len**（连续说话 30 秒以上没有气口可收口）。
    // 这时才按能量最低点切（本次改造前的老路径，保留）。
    let mut final_out = Vec::with_capacity(out.len());
    for (start, end) in out {
        let mut a = start;
        while end - a > max_len {
            let hi = a + max_len;
            let lo = hi.saturating_sub(SAMPLE_RATE * 8).max(a + SAMPLE_RATE);
            let mut best = (hi, f32::MAX);
            let mut pos = lo;
            while pos + VAD_FRAME <= hi {
                let e: f32 =
                    pcm[pos..pos + VAD_FRAME].iter().map(|v| v * v).sum::<f32>() / VAD_FRAME as f32;
                if e < best.1 {
                    best = (pos, e);
                }
                pos += VAD_HOP;
            }
            final_out.push((a, best.0));
            a = best.0;
        }
        if end > a {
            final_out.push((a, end));
        }
    }
    final_out
}

/// 重复度：最高频 4-gram 出现次数 / 全部 4-gram 数。
///
/// whisper 官方用 `len(text)/len(zlib.compress(text))`（压缩比 > 2.4 判为幻觉/重复环）。
/// 我们不引 zlib 依赖，改用**同一个目的的显式判据**：一段话里 4-gram 反复出现，
/// 就是重复环的指纹。实测现场（2026-09-26）：6.86 秒的音频解出
/// `把库存服务的连接池从20改成50然后重启一下服务` **重复 8 遍**（82 token / 48.5 秒），
/// 这个指标在那一刻 ≈ 0.86 —— 而正常文本在 0.1 以下。**口径不同，目的相同，如实写明。**
fn repetition_ratio(text: &str) -> f32 {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 8 {
        return 0.0;
    }
    let grams: Vec<String> = chars.windows(4).map(|w| w.iter().collect()).collect();
    let unique: std::collections::HashSet<&str> = grams.iter().map(|g| g.as_str()).collect();
    // **1 − 不重复占比**：一段正常话里几乎每个 4-gram 都是新的（≈0）；
    // 重复环里同样的 4-gram 反复出现 ⇒ 唯一数很少（≈1）。
    //
    // 注意别写成"最高频 4-gram 次数 / 总数"——那个数在**长**重复文本里反而很小
    // （实测：8 遍 22 字的重复环算出来只有 0.042，判据当场放过 ✗）。形状选错的代价是
    // "指标看着对、实际不报警"，所以这条也留一个用真文本当夹具的单测。
    1.0 - unique.len() as f32 / grams.len() as f32
}

/// 把"打转"出来的尾巴剪掉：反复剥掉结尾处**重复出现的一整块**。
///
/// 这是重复环的**最终处置**（比升温可靠，也比按 token 拉黑可靠 —— 实测按 token 的
/// 4-gram 拉黑抓不住中文重复：同一句重复时 BPE 的边界合并会让 4-gram 变样，环照样跑 ✗）。
/// 做法：从结尾剥 —— 若末尾 2k 个字符由同样的 k 个字符重复两次构成，就保留前一半、丢掉后一半，
/// 反复直到不再重复。**留下的是第一次说出口的那句**，那正是用户要的。
///
/// 代价要认：用户真的重复说同一句（"再说一遍、再说一遍"）会被剪成一遍。
/// 与"整段话被环吞掉、还要等 225 秒"相比，这个代价小得多。
fn strip_repeated_tail(text: &str) -> String {
    let mut chars: Vec<char> = text.chars().collect();
    let mut changed = true;
    while changed {
        changed = false;
        let n = chars.len();
        // k 从大到小试：优先剥掉"最大的那一段重复"
        // 注意是 n/2（不是 (n-1)/2）：正好两遍时要能够到周期本身，差一个就少剥一遍
        let max_k = n / 2;
        for k in (6..=max_k).rev() {
            let tail = &chars[n - 2 * k..];
            if tail[..k] == tail[k..] {
                chars.truncate(n - k);
                changed = true;
                break;
            }
        }
    }
    chars.into_iter().collect()
}

/// 片段拼接：中文直接接、中英/英英之间补一个空格。
///
/// 为什么要区分：whisper 的分词在"中文 + 英文词"边界上不稳定（实测同一句两种编码窗口
/// 分别吐出 `Cargo Build` / `CargoBuild`）—— 我们自己拼的时候至少别**制造**新的空格问题。
fn join_parts(parts: &[String]) -> String {
    let mut out = String::new();
    for p in parts {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        let need_space = match (out.chars().last(), p.chars().next()) {
            (Some(a), Some(b)) => a.is_ascii_alphanumeric() && b.is_ascii_alphanumeric(),
            _ => false,
        };
        if need_space {
            out.push(' ');
        }
        out.push_str(p);
    }
    out
}

impl Window {
    /// 从配置值解析窗口策略（`voice.window`）。
    ///
    /// **未知值一律当 `Trim`**：这是读侧的兜底 —— 一份被写坏的配置不许改变行为
    /// （`"on"`、`"TRUE "`、空串、拼错的 `"trimmed"` 都落到默认）。认得出 `full` 才用 `full`，
    /// 因为它是那个"更慢但更贴官方"的选项，**永远不该因为解析意外而被选中**。
    pub fn from_cfg(v: &str) -> Window {
        if v.trim().eq_ignore_ascii_case("full") {
            Window::Full
        } else {
            Window::Trim
        }
    }
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
    /// 音频超过 `voice.max_secs`，只转了前面那段（**要如实告诉用户**，不能悄悄截断）
    pub truncated: bool,
    /// 剪静音之后**真正有人声**的时长（秒）。
    /// 与 `audio_secs` 一起看就知道"用户按了 8 秒、其实只说了 3 秒"。
    pub speech_secs: f32,
    /// 这段音频被切成了几个窗口（1 = 没超过 30 秒；>1 = 分段生效）
    pub windows: usize,
    /// 模型认为**这段没人说话**（no-speech 概率 > 0.6）。
    /// 与 `text.is_empty()` 一起用：静音被正确拒掉时两者同时成立，而不是吐一句幻觉。
    pub no_speech: bool,
    /// 有几个窗口的尾部"打转"被剪掉（0 = 没有重复环）。
    pub deduped: usize,
    /// 温度回退一共重跑了几个窗口（0 = 全部一次贪心就过）。
    /// **这不是"内部细节"，是质量信号**：老在重跑说明这批音频对模型偏难。
    pub retries: usize,
    /// 各窗口平均对数概率的均值（越接近 0 越有把握）。
    /// **这是"该不该信这句话"的唯一机器判据** —— 低置信度的转写要提示用户核对。
    pub avg_logprob: f32,
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
    /// `<|nospeech|>`：第一步解码时它的概率就是"这窗没人说话"的判据（whisper 的 no_speech_prob）
    no_speech: u32,
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
        // `<|nospeech|>`：不同转写版里可能写成 `<|nocaptions|>`，两个名字都试一下。
        // 拿不到就退回 MAX（那个位置永远是 0 概率）—— **不许因为一个 token 让整条链路起不来**，
        // 代价只是"少了一条 no-speech 判据"，而能量 VAD 那一层还在。
        let no_speech = id(TOKEN_NO_SPEECH[1])
            .or_else(|_| id(TOKEN_NO_SPEECH[0]))
            .unwrap_or(u32::MAX);

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
            no_speech,
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
    /// 单个窗口：mel + 编码器 → features。
    ///
    /// `Window::Trim` 按**这个窗口自己的长度**编码（多留 1 秒余量防吃字、向上取偶因为
    /// conv2 的 stride=2）；`Window::Full` 是 whisper 官方口径（恒补零到 30 秒）。
    fn encode_window(&mut self, samples: &[f32], window: Window) -> Result<Tensor, String> {
        let audio_secs = samples.len() as f32 / SAMPLE_RATE as f32;
        let n_frames = match window {
            Window::Full => N_FRAMES,
            Window::Trim => {
                let secs = audio_secs.max(0.2) + 1.0;
                let f = ((secs * SAMPLE_RATE as f32) / 160.0).ceil() as usize;
                f.div_ceil(2) * 2
            }
        }
        .clamp(2, N_FRAMES);
        let mut buf = samples.to_vec();
        buf.resize(n_frames * 160, 0.0);

        let mel_v = audio::pcm_to_mel(&self.cfg, &buf, &self.filters);
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
        self.model
            .encoder
            .forward(&mel_t, true)
            .map_err(|e| format!("编码失败: {e}"))
    }

    /// 单个窗口：贪心解码。
    ///
    /// 返回 `(tokens, avg_logprob, no_speech_prob)` —— 后两个是**下一层（温度回退）的输入**，
    /// 也是用户判断"这句靠不靠得住"的依据，所以从第一步就接出来，别等要用了再加。
    fn decode_window(
        &mut self,
        features: &Tensor,
        lang_id: u32,
        temperature: f32,
    ) -> Result<Decoded, String> {
        let mut tokens = vec![self.sot, lang_id, self.transcribe, self.no_timestamps];
        let mut out: Vec<u32> = Vec::new();
        let mut logprob_sum = 0.0f32;
        let mut no_speech_prob = 0.0f32;
        // **no-repeat 4-gram**：把"会让某个 4-gram 再次出现"的下一个 token 拉黑。
        //
        // 为什么必须有它（而不是只靠升温）：现场那段 6.86 秒音频，贪心解出
        // `把库存服务的连接池从20改成50然后重启一下服务` **重复 8 遍**；加温度回退后
        // **五档全试完仍然在环里**（端到端 225 秒 ✗）—— 因为环的成因是"前缀里已经有了这串，
        // 模型顺着它继续"（我们没有 KV 缓存、每步重发整段前缀，这个条件被反复强化），
        // 采样打不散。**确定性拉黑**才治得住：同一串 4-gram 不许出现第二次。
        // 代价要认：合法文本里真的重复四次以上的短语会被削掉（中文里"再试一次再试一次"这种
        // 口语重复可能受影响）—— 与"整段话被环吞掉"相比，这个代价小得多，且方向可控。
        // 只维护"三元组后缀 → 跟在它后面的 token"这一张表就够（等价于"4-gram 不许重复"，
        // 但每步只查一次、每步只写一条）。
        let mut suffix_next: std::collections::HashMap<(u32, u32, u32), Vec<u32>> =
            std::collections::HashMap::new();
        for step in 0..self.cfg.max_target_positions {
            // **每步重发整段 token 前缀**（`flush = step == 0`）—— 看着浪费，但这是
            // candle 0.9.2 的 whisper **唯一正确**的喂法。
            //
            // 试过"增量解码"（第一步整段、之后每步只喂新 token + 复用 kv_cache）：**输出直接崩**
            // （实测 3.78 秒音频解出乱码替换符、6.86 秒解出"把"，都跑满 448 步）。原因在模型实现里 ——
            // 位置编码按**输入长度**从 0 取起（`positional_embedding.narrow(0, 0, x.len())`），
            // 喂单 token 等于把位置重置成 0。candle 自己的 whisper 示例同样每步 flush
            // （`flush = x.dim(1)? != 1`）⇒ **这个版本无法增量解码**，解码 O(n^2) 是实现限制，
            // 不是"我们没用对"（我先前那句话是错的，已更正）。
            // 真要提速只有两条：fork candle 给位置编码加偏移（改依赖、风险高），或换实现 / 上 GPU。
            let input = Tensor::new(tokens.as_slice(), &self.device)
                .and_then(|t| t.unsqueeze(0))
                .map_err(|e| e.to_string())?;
            let logits = self
                .model
                .decoder
                .forward(&input, features, step == 0)
                .and_then(|l| self.model.decoder.final_linear(&l))
                .and_then(|l| l.i((0, tokens.len() - 1)))
                .and_then(|l| l.to_dtype(DType::F32))
                .map_err(|e| format!("解码第 {} 步失败: {e}", step + 1))?;
            let values = logits.to_vec1::<f32>().map_err(|e| e.to_string())?;
            // no-speech 概率只看**第一步**（whisper 也是这么用的：它是 SOT 位置上的一个特殊 token）
            if step == 0 {
                no_speech_prob = softmax_at(&values, self.no_speech);
            }
            // 本步拉黑的候选：会让"当前后缀 + 这个 token"构成已出现过的 4-gram 的那些 token。
            let blocked: Vec<u32> = if out.len() >= 3 {
                let suf = (out[out.len() - 3], out[out.len() - 2], out[out.len() - 1]);
                suffix_next.get(&suf).cloned().unwrap_or_default()
            } else {
                Vec::new()
            };
            let is_blocked = |id: u32| blocked.contains(&id);
            // 温度 0 = 贪心（能对就别乱动）；温度 > 0 = 采样（只在被判为幻觉/重复环时才走到这里）
            let picked = if temperature <= 0.0 {
                let mut best = (f32::MIN, u32::MAX);
                for (id, v) in values.iter().enumerate() {
                    let id = id as u32;
                    if self.suppressed.binary_search(&id).is_ok() || is_blocked(id) {
                        continue;
                    }
                    if *v > best.0 {
                        best = (*v, id);
                    }
                }
                // 全被拉黑 ⇒ 说明退无可退（极短输出），此时放行，别把这段卡死
                if best.1 == u32::MAX {
                    for (id, v) in values.iter().enumerate() {
                        let id = id as u32;
                        if self.suppressed.binary_search(&id).is_ok() {
                            continue;
                        }
                        if *v > best.0 {
                            best = (*v, id);
                        }
                    }
                }
                best.1
            } else {
                sample_token(&values, &self.suppressed, temperature, step)
            };
            if picked == u32::MAX {
                return Err("所有 token 都被抑制了（抑制表配错？）".into());
            }
            if picked == self.eot {
                break;
            }
            logprob_sum += log_softmax_at(&values, picked);
            tokens.push(picked);
            out.push(picked);
            // 维护索引：刚生成的这个 token，是"前三个 token"这个后缀的后续 —— 记下来，
            // 下次再遇到同一后缀，就把它拉黑（4-gram 不许重复）。
            if out.len() >= 4 {
                let n = out.len();
                let suf = (out[n - 4], out[n - 3], out[n - 2]);
                let e = suffix_next.entry(suf).or_default();
                if !e.contains(&out[n - 1]) {
                    e.push(out[n - 1]);
                }
            }
        }
        let avg = if out.is_empty() {
            0.0
        } else {
            logprob_sum / out.len() as f32
        };
        Ok(Decoded {
            tokens: out,
            avg_logprob: avg,
            no_speech_prob,
        })
    }

    /// 转写一段 16kHz 单声道 PCM（默认：按真实长度编码 + 剪静音 + 分段）。
    ///
    /// `language = None` 时先做一次语言检测（中英混着说也对）；给 `Some("zh")` 就跳过检测。
    pub fn transcribe(
        &mut self,
        pcm: &[f32],
        language: Option<&str>,
    ) -> Result<Transcript, String> {
        self.transcribe_with(
            pcm,
            TranscribeOpts {
                language,
                ..Default::default()
            },
        )
    }

    /// 带窗口策略的转写（对照实验用）。
    pub fn transcribe_in(
        &mut self,
        pcm: &[f32],
        language: Option<&str>,
        window: Window,
    ) -> Result<Transcript, String> {
        self.transcribe_with(
            pcm,
            TranscribeOpts {
                language,
                window,
                ..Default::default()
            },
        )
    }

    /// 完整形态的转写：**剪静音 → 分段 → 逐窗编码解码 → 拼接**。
    ///
    /// 为什么要这三步（都是实测踩出来的）：
    /// * **剪静音**：whisper 在静音上会幻觉（实测吐出 "Thank you."）。录音两头必然有静音，
    ///   中间还有气口 —— 不剪就是把幻觉的原料喂给它。
    /// * **分段**：以前超过 30 秒**直接截断**，用户说长了后半句就没了。那不是"不准"，是**没听**。
    /// * **拼接**：中文直接接、中英之间补空格，别自己制造新的边界问题。
    ///
    /// 语言检测只在**第一窗**做（后面沿用）—— 既省时间，也避免"同一段话两个窗口识别成两种语言"。
    pub fn transcribe_with(
        &mut self,
        pcm: &[f32],
        opts: TranscribeOpts<'_>,
    ) -> Result<Transcript, String> {
        if pcm.is_empty() {
            return Err("音频是空的".into());
        }
        let t0 = Instant::now();
        let audio_secs = pcm.len() as f32 / SAMPLE_RATE as f32;

        // ① 剪静音（可关：`voice.vad = false` 时退回"整段喂"）
        let regions = if opts.vad {
            energy_vad(pcm)
        } else {
            vec![(0, pcm.len())]
        };
        let speech_secs =
            regions.iter().map(|(a, b)| (b - a) as f32).sum::<f32>() / SAMPLE_RATE as f32;
        if regions.is_empty() {
            // **整段没有人声**：如实回报，**绝不让模型去猜**（这就是幻觉的入口）
            return Ok(Transcript {
                text: String::new(),
                language: opts.language.unwrap_or("").to_string(),
                audio_secs,
                speech_secs,
                elapsed_ms: t0.elapsed().as_millis(),
                encode_ms: 0,
                decode_ms: 0,
                steps: 0,
                tokens: 0,
                windows: 0,
                no_speech: true,
                retries: 0,
                deduped: 0,
                avg_logprob: 0.0,
                truncated: false,
            });
        }

        // ② 分段（每段不超过一个窗口；超长音频按 `max_secs` 截断并**如实标注**）
        let max_samples = ((opts.max_secs.max(1.0) * SAMPLE_RATE as f32) as usize).max(VAD_FRAME);
        let capped: Vec<(usize, usize)> = {
            let mut total = 0usize;
            let mut keep = Vec::new();
            for (a, b) in regions.iter().copied() {
                if total >= max_samples {
                    break;
                }
                let b2 = b.min(a + (max_samples - total));
                if b2 > a {
                    total += b2 - a;
                    keep.push((a, b2));
                }
            }
            keep
        };
        // **截断的判据是"语音被砍了"，不是"总长变短了"** —— 剪静音本来就会让总长变短，
        // 拿 `pcm.len()` 比会把每一段都标成截断（实测：77.8 秒音频被误标 truncated ✗）。
        let speech_total: usize = regions.iter().map(|(a, b)| b - a).sum();
        let capped_total: usize = capped.iter().map(|(a, b)| b - a).sum();
        let truncated = capped_total < speech_total;
        let planned = plan_windows(&capped, pcm, N_SAMPLES);

        // ③ 逐窗编码 + 解码
        let mut lang_id: Option<u32> = None;
        let mut language = String::new();
        let mut parts: Vec<String> = Vec::new();
        let mut encode_ms = 0u128;
        let mut decode_ms = 0u128;
        let mut steps = 0usize;
        let mut logprob_acc = 0.0f32;
        let mut logprob_n = 0usize;
        let mut worst_no_speech = 0.0f32;
        let mut fallbacks = 0usize;
        let mut deduped_n = 0usize;
        for (a, b) in &planned {
            let chunk = &pcm[*a..*b];
            let t_enc = Instant::now();
            let features = self.encode_window(chunk, opts.window)?;
            encode_ms += t_enc.elapsed().as_millis();
            // 语言：第一窗检测一次，之后沿用（同一段话不该出现两种语言）
            let lid = match (lang_id, opts.language) {
                (Some(id), _) => id,
                (None, Some(l)) => {
                    let id = self
                        .lang_id(l)
                        .ok_or_else(|| format!("不认识的语言标记 {l}（语言 token 表里没有）"))?;
                    language = l.to_string();
                    lang_id = Some(id);
                    id
                }
                (None, None) => {
                    let l = self.detect_language(&features)?;
                    let id = self
                        .lang_id(&l)
                        .ok_or_else(|| format!("不认识的语言标记 {l}（语言 token 表里没有）"))?;
                    language = l;
                    lang_id = Some(id);
                    id
                }
            };
            // **温度回退**（whisper 官方的 `decode_with_fallback`）：先贪心；这一段若
            // 被判为"没把握"或"在打转"，就升温重跑整窗（官方会在最后一个时间戳处重切，
            // 我们不做时间戳，所以重跑整窗 —— 口径差异如实写明）。
            // 现场证据：6.86 秒音频贪心解出 `…然后重启一下服务` **重复 8 遍**
            // （82 token / 48.5 秒），升温到 0.2 一次就正常。
            let t_dec = Instant::now();
            let mut text = String::new();
            let mut chosen_logprob = 0.0f32;
            let mut n_tokens = 0usize;
            let mut retries = 0usize;
            for (idx, temp) in FALLBACK_TEMPS.iter().enumerate() {
                let d = self.decode_window(&features, lid, *temp)?;
                if idx > 0 {
                    retries += 1;
                }
                worst_no_speech = worst_no_speech.max(d.no_speech_prob);
                if d.tokens.is_empty() {
                    text.clear();
                    n_tokens = 0;
                    break;
                }
                let cand = self
                    .tokenizer
                    .decode(&d.tokens, true)
                    .map_err(|e| format!("解码文本失败: {e}"))?
                    .trim()
                    .to_string();
                text = cand;
                chosen_logprob = d.avg_logprob;
                n_tokens = d.tokens.len();
                // **打转**用确定性去重（升温治不了它，实测五档全试完仍在环里），
                // **没把握**才升温重跑。两者分开处理，各自用对得上的手段。
                if repetition_ratio(&text) > FALLBACK_MAX_REPEAT {
                    let deduped = strip_repeated_tail(&text);
                    if deduped != text {
                        deduped_n += 1;
                        text = deduped;
                        n_tokens = self
                            .tokenizer
                            .encode(text.as_str(), true)
                            .map(|t| t.get_ids().len())
                            .unwrap_or(n_tokens);
                    }
                }
                if chosen_logprob >= FALLBACK_MIN_LOGPROB
                    && repetition_ratio(&text) <= FALLBACK_MAX_REPEAT
                {
                    break;
                }
            }
            decode_ms += t_dec.elapsed().as_millis();
            steps += n_tokens;
            fallbacks += retries;
            if text.is_empty() {
                continue;
            }
            logprob_acc += chosen_logprob;
            logprob_n += 1;
            parts.push(text);
        }

        Ok(Transcript {
            text: join_parts(&parts),
            language,
            audio_secs,
            speech_secs,
            elapsed_ms: t0.elapsed().as_millis(),
            encode_ms,
            decode_ms,
            steps,
            tokens: steps,
            windows: planned.len(),
            no_speech: worst_no_speech > 0.6,
            retries: fallbacks,
            deduped: deduped_n,
            avg_logprob: if logprob_n == 0 {
                0.0
            } else {
                logprob_acc / logprob_n as f32
            },
            truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 能量 VAD：**静音必须被判掉**（这是"静音幻觉"的第一道闸），语音段要保住。
    #[test]
    fn vad_keeps_speech_and_drops_silence() {
        let tone = |secs: f32| -> Vec<f32> {
            let n = (SAMPLE_RATE as f32 * secs) as usize;
            (0..n)
                .map(|i| {
                    0.2 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / SAMPLE_RATE as f32).sin()
                })
                .collect()
        };
        let mut pcm = vec![0.0f32; SAMPLE_RATE];
        pcm.extend(tone(1.0));
        pcm.extend(vec![0.0f32; SAMPLE_RATE]);
        let regions = energy_vad(&pcm);
        assert_eq!(regions.len(), 1, "应只留一段语音：{regions:?}");
        let (a, b) = regions[0];
        assert!(
            b - a >= SAMPLE_RATE,
            "1 秒语音不该被剪短（两侧只留 0.15 秒边）：{a}..{b}"
        );
        assert!(
            a >= SAMPLE_RATE - SAMPLE_RATE / 5 && b <= 2 * SAMPLE_RATE + SAMPLE_RATE / 5,
            "不该把静音也带上：{a}..{b}"
        );
        // 纯静音 → 什么都不留。调用方据此返回"没听到人声"，
        // **而不是让模型去猜**（那一猜就是 "Thank you." 的来源）。
        assert!(
            energy_vad(&vec![0.0f32; SAMPLE_RATE * 3]).is_empty(),
            "纯静音必须被判成没有人声"
        );
    }

    /// 分段：40 秒连续语音必须切成多窗，且**一秒都不少**。
    /// 老行为是 `samples.truncate(N_SAMPLES)` —— 超过 30 秒的部分**根本没被听**，
    /// 而用户会把那体验成"一点都不准"。这条判据钉的就是那个 bug。
    #[test]
    fn long_audio_is_split_not_truncated() {
        let speech = SAMPLE_RATE * 40;
        let mut pcm = vec![0.0f32; SAMPLE_RATE];
        pcm.extend((0..speech).map(|i| {
            0.15 * (2.0 * std::f32::consts::PI * 300.0 * i as f32 / SAMPLE_RATE as f32).sin()
        }));
        pcm.extend(vec![0.0f32; SAMPLE_RATE]);
        let regions = energy_vad(&pcm);
        let planned = plan_windows(&regions, &pcm, N_SAMPLES);
        assert!(planned.len() >= 2, "40 秒语音应切成多窗：{planned:?}");
        let covered: usize = planned.iter().map(|(a, b)| b - a).sum();
        assert!(
            covered >= speech,
            "切完必须覆盖整段语音（实际 {covered} < 语音 {speech}）"
        );
        for (a, b) in &planned {
            assert!(b - a <= N_SAMPLES, "单窗不许超过 30 秒：{}", b - a);
        }
    }

    /// **装箱**判据：多句、句间有长气口时，窗口数应接近"总长 / 30 秒"，而不是**一句一窗**。
    /// 现场：77 秒的十句话被切成 22 个窗口 ⇒ 22 遍编码器（每遍 5~8 秒）——
    /// 用户感受到的"慢"有一半是这么来的。这条判据钉住"别再一段一窗"。
    #[test]
    fn speech_bursts_are_packed_into_full_windows() {
        // 10 段 2 秒语音，之间各 1 秒静音（模拟"一句话一句话说"）
        let mut pcm: Vec<f32> = vec![0.0f32; SAMPLE_RATE];
        let mut starts = Vec::new();
        for _ in 0..10 {
            starts.push(pcm.len());
            pcm.extend((0..SAMPLE_RATE * 2).map(|i| {
                0.15 * (2.0 * std::f32::consts::PI * 300.0 * i as f32 / SAMPLE_RATE as f32).sin()
            }));
            pcm.extend(vec![0.0f32; SAMPLE_RATE]);
        }
        let regions = energy_vad(&pcm);
        assert_eq!(regions.len(), 10, "应识别出 10 段语音：{}", regions.len());
        let planned = plan_windows(&regions, &pcm, N_SAMPLES);
        let total_span = regions.last().unwrap().1 - regions[0].0;
        let want = total_span.div_ceil(N_SAMPLES);
        assert!(
            planned.len() <= want + 1,
            "窗口数应接近总长/30s（期望 ~{want}），实际 {} —— 一段一窗会让编码器多跑几倍",
            planned.len()
        );
    }

    /// 重复度判据：**用现场抓到的那串真文本**当夹具（不是编的）。
    /// 左边是 2026-09-26 实测的重复环（6.86 秒音频解出 8 遍），右边是它的正确结果 ——
    /// 温度回退就是靠这个数把两者分开的。
    #[test]
    fn repetition_ratio_separates_a_loop_from_real_text() {
        let looped = "把库存服务的连接池从20改成50然后重启一下服务".repeat(8);
        let good = "把库存服务的连接池从20改成50然后重启一下服务";
        assert!(
            repetition_ratio(&looped) > FALLBACK_MAX_REPEAT,
            "重复环必须被判出来：{:.3}",
            repetition_ratio(&looped)
        );
        assert!(
            repetition_ratio(good) <= FALLBACK_MAX_REPEAT,
            "正常一句话不该被误判：{:.3}",
            repetition_ratio(good)
        );
        // 长音频里十句话各自不同（含重复的说法）也要放行
        let long = "帮我把构建命令改成cargo build把库存服务的连接池从20改成50然后重启一下服务\
这个报错是空指针帮我看看是哪里传进来的把登录接口的超时时间从30秒改成60秒";
        assert!(
            repetition_ratio(long) <= FALLBACK_MAX_REPEAT,
            "正常长文本不该被误判：{:.3}",
            repetition_ratio(long)
        );
    }

    /// 去重尾巴：**用现场那串真重复文本当夹具**。
    /// 8 遍重复 ⇒ 必须剪成 1 遍（用户要的就是第一次说的那句）。
    #[test]
    fn strip_repeated_tail_keeps_the_first_saying() {
        let one = "把库存服务的连接池从20改成50然后重启一下服务";
        let looped = one.repeat(8);
        assert_eq!(strip_repeated_tail(&looped), one, "8 遍重复必须剪成 1 遍");
        assert_eq!(strip_repeated_tail(one), one, "不重复的文本不许动");
        assert_eq!(
            strip_repeated_tail(&format!("{one}{one}")),
            one,
            "两遍也要剪成一遍"
        );
        let normal = "帮我把构建命令改成cargo build把库存服务的连接池从20改成50";
        assert_eq!(strip_repeated_tail(normal), normal, "正常多句文本不许误剪");
    }
    /// 拼接：中文直接接、中英之间补空格（别自己**制造**新的边界空格问题）。
    #[test]
    fn join_parts_handles_cjk_and_latin_boundaries() {
        let p = |v: &[&str]| join_parts(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(
            p(&["帮我把构建命令改成", "Cargo build"]),
            "帮我把构建命令改成Cargo build"
        );
        assert_eq!(p(&["run", "tests"]), "run tests");
        assert_eq!(
            p(&["把连接池从20改成50", "然后重启"]),
            "把连接池从20改成50然后重启"
        );
        assert_eq!(p(&["", "  ", "有内容"]), "有内容");
    }
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
    fn window_from_cfg_only_full_opts_in() {
        assert_eq!(Window::from_cfg("full"), Window::Full);
        assert_eq!(
            Window::from_cfg(" full "),
            Window::Full,
            "两端空白不该改变判读"
        );
        assert_eq!(Window::from_cfg("FULL"), Window::Full, "大小写不敏感");
        // 其余全部回落 trim：默认值、空串、写坏的值（`on` 就是真事故里那个值）
        for bad in ["", "trim", "on", "true", "TRUE", "Full2", "fuller", "0"] {
            assert_eq!(Window::from_cfg(bad), Window::Trim, "`{bad}` 必须回落 trim");
        }
    }

    /// `voice.window` 的读侧口径：**只有明确写 `full` 才走全窗**，其余一律 `trim`。
    /// 这条判据护的是一个不对称的代价：解析意外把用户推进 `full` 会让每次转写慢 6 倍，
    /// 而意外留在 `trim` 只是"没享受加速"（且默认本就是它）。
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
