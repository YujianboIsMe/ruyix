//! **真机探针**：拿真的中文语音跑一遍 candle whisper（量化 GGUF），把文本与**耗时**打出来。
//!
//! 为什么必须有它：单测只能验"逻辑对不对"（抑制表、mel 表形状），而"这枚权重能不能加载、
//! 中文识别得怎样、CPU 上跑多快"全是**外部事实** —— 只有真跑一发才知道。
//! 尤其是速度：选 candle 而不是 whisper.cpp，代价到底是多少倍，只有量出来才算数。
//!
//! 用法（先备好模型目录，或用 `RUYIX_VOICE_MODEL_DIR` 指过去）：
//!
//! ```bash
//! RUYIX_VOICE_MODEL_DIR='<模型根>' \
//!   cargo run --release -q -p harness-engine --example voice_probe -- <wav> [<wav> ...]
//! ```
//!
//! 环境变量 `RUYIX_VOICE_MODEL_DIR` 指向**模型根**（其下是 `candle-whisper-large-v3-turbo/`）。
//! WAV 支持 16 位 PCM（单/多声道会混成单声道），采样率任意 —— 探针内部重采样到 16kHz，
//! 因为真机上"前端交给 Rust 的就是 16k 单声道"（WebAudio 解码 + 重采样）。

use harness_engine::voice::Asr;
use std::time::Instant;

/// 极简 WAV 读取：只支持 16 位 PCM（SAPI / 我们前端导出的就是它）。
fn read_wav_16k_mono(path: &str) -> Result<Vec<f32>, String> {
    let b = std::fs::read(path).map_err(|e| format!("读 {path} 失败: {e}"))?;
    if b.len() < 44 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(format!("{path} 不是 RIFF/WAVE"));
    }
    let find = |tag: &[u8]| -> Option<usize> { b.windows(4).position(|w| w == tag) };
    let f = find(b"fmt ").ok_or("没有 fmt 块")? + 8;
    let (tag, ch, sr, bits) = (
        u16::from_le_bytes([b[f], b[f + 1]]),
        u16::from_le_bytes([b[f + 2], b[f + 3]]),
        u32::from_le_bytes([b[f + 4], b[f + 5], b[f + 6], b[f + 7]]),
        u16::from_le_bytes([b[f + 14], b[f + 15]]),
    );
    if tag != 1 || bits != 16 {
        return Err(format!("只支持 16 位 PCM（tag={tag} bits={bits}）"));
    }
    let d = find(b"data").ok_or("没有 data 块")?;
    let dlen = u32::from_le_bytes([b[d + 4], b[d + 5], b[d + 6], b[d + 7]]) as usize;
    let pcm = &b[d + 8..(d + 8 + dlen).min(b.len())];
    let ch = ch as usize;
    // 交织 → 单声道 f32
    let mut mono: Vec<f32> = Vec::with_capacity(pcm.len() / 2 / ch);
    for frame in pcm.chunks_exact(2 * ch) {
        let mut acc = 0f32;
        for c in 0..ch {
            let s = i16::from_le_bytes([frame[c * 2], frame[c * 2 + 1]]);
            acc += s as f32 / 32768.0;
        }
        mono.push(acc / ch as f32);
    }
    // 线性重采样到 16kHz（探针够用：真机上前端做的是正经重采样）
    let target = harness_engine::voice::asr::SAMPLE_RATE;
    if sr as usize == target {
        return Ok(mono);
    }
    let ratio = target as f64 / sr as f64;
    let out_len = ((mono.len() as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let x = i as f64 / ratio;
        let i0 = x.floor() as usize;
        let frac = (x - i0 as f64) as f32;
        let a = mono.get(i0).copied().unwrap_or(0.0);
        let bnext = mono.get(i0 + 1).copied().unwrap_or(a);
        out.push(a + (bnext - a) * frac);
    }
    println!(
        "  · {path}: {} 声道 / {} Hz / {:.2}s → 重采样到 16k 单声道（{} 点）",
        ch,
        sr,
        mono.len() as f32 / sr as f32,
        out.len()
    );
    Ok(out)
}

fn main() -> Result<(), String> {
    let root = std::env::var("RUYIX_VOICE_MODEL_DIR").map_err(|_| {
        "先设 RUYIX_VOICE_MODEL_DIR（模型根，其下有 candle-whisper-large-v3-turbo/）".to_string()
    })?;
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let lang: Option<String> = argv
        .iter()
        .position(|a| a == "--lang")
        .and_then(|i| argv.get(i + 1).cloned());
    // 编码窗口：默认按真实长度（快）；`--full` 走 whisper 官方口径（补零到 30 秒）做对照
    let window = if argv.iter().any(|a| a == "--full") {
        harness_engine::voice::asr::Window::Full
    } else {
        harness_engine::voice::asr::Window::Trim
    };
    // `--dir <路径>` 直接指定模型目录（换权重做对照时用）；不给就按便携布局拼
    let dir_arg: Option<String> = argv
        .iter()
        .position(|a| a == "--dir")
        .and_then(|i| argv.get(i + 1).cloned());
    let wavs: Vec<String> = argv
        .iter()
        .filter(|a| {
            a.as_str() != "--lang"
                && a.as_str() != "--full"
                && a.as_str() != "--dir"
                && lang.as_deref() != Some(a.as_str())
                && dir_arg.as_deref() != Some(a.as_str())
        })
        .cloned()
        .collect();
    if wavs.is_empty() {
        return Err(
            "用法: voice_probe [--lang zh] [--full] [--dir <模型目录>] <wav> [<wav> ...]".into(),
        );
    }
    let dir = match &dir_arg {
        Some(d) => std::path::PathBuf::from(d),
        None => std::path::Path::new(&root).join("candle-whisper-large-v3-turbo"),
    };
    println!("模型目录: {}", dir.display());

    let t0 = Instant::now();
    let mut asr = Asr::load(&dir)?;
    // 别在打印里写死体积：这个探针可以指向任意模型目录（换臂对照时那行会变成假话）
    println!(
        "加载耗时: {} ms（权重只加载一次，进程内复用）",
        t0.elapsed().as_millis()
    );

    let mut pass = 0usize;
    for w in &wavs {
        let pcm = read_wav_16k_mono(w)?;
        let t = Instant::now();
        let r = asr.transcribe_in(&pcm, lang.as_deref(), window)?; // None = 先做语言检测
        println!("── {w}");
        println!("   语言      : {}", r.language);
        println!("   文本      : {}", r.text);
        println!("   token 数  : {}", r.tokens);
        println!(
            "   分阶段    : 编码 {} ms（固定 30s 窗）· 解码 {} ms（{} 步）",
            r.encode_ms, r.decode_ms, r.steps
        );
        println!(
            "   音频/耗时 : {:.2}s / {} ms（RTF {:.2}×，含语言检测与整段编码）",
            r.audio_secs,
            r.elapsed_ms,
            r.elapsed_ms as f64 / (r.audio_secs.max(0.01) as f64 * 1000.0)
        );
        println!("   墙钟(含准备): {} ms", t.elapsed().as_millis());
        if r.truncated {
            println!("   ⚠ 音频超过 30 秒，只转了前 30 秒");
        }
        if r.text.trim().is_empty() {
            return Err(format!("{w} 转出来是空的 —— 权重/mel 表/提示词有一步不对"));
        }
        pass += 1;
    }
    println!(
        "\nvoice_probe: {pass}/{} 段转出了非空文本（准确率请人眼核对上面的原文）",
        wavs.len()
    );
    Ok(())
}
