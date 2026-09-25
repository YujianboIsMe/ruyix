//! 看一个 whisper 权重文件里到底有什么 —— 并**亲自试加载一次**。
//!
//! 为什么要有这个探针（2026-09-25）：格式与张量命名都是**外部事实**，而且两条路都踩过坑：
//!   · whisper.cpp 官方发布的是**老 GGML `.bin`**：11 个 hparams + 文件内 mel filters + 无
//!     score 的词表；而 candle 的 `ggml_file` 是按 **llama.cpp** 布局写的（7 个 hparams +
//!     token/score 词表）⇒ 读不了；
//!   · candle 的量化 whisper 走 `quantized_var_builder::from_gguf`（只吃 **GGUF**），且按
//!     **HF 命名**取张量（`model.encoder.blocks.0.self_attn.q_proj`），whisper.cpp 是
//!     `encoder.blocks.0.attn.query` ⇒ 光有 GGUF 还不够，命名也得对得上。
//! 所以这里既数张量，也真做一次 `VarBuilder::from_gguf` + `Whisper::load`：
//! 缺哪个张量就报哪个名字，比读文档可靠。
//!
//! 用法：
//!   cargo run -q -p harness-engine --example voice_inspect -- <模型文件> [--try-load]

use candle_core::{Device, quantized::gguf_file};
use candle_transformers::models::whisper::{Config, quantized_model};

fn whisper_small_cfg() -> Config {
    // openai/whisper-small 的形状（也是 whisper.cpp 在这枚文件里写的 hparams）
    Config {
        num_mel_bins: 80,
        max_source_positions: 1500,
        d_model: 768,
        encoder_attention_heads: 12,
        encoder_layers: 12,
        vocab_size: 51865,
        max_target_positions: 448,
        decoder_attention_heads: 12,
        decoder_layers: 12,
        suppress_tokens: vec![],
    }
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let path = args
        .get(1)
        .ok_or_else(|| "用法: voice_inspect <模型文件> [--try-load]".to_string())?;
    let try_load = args.iter().any(|a| a == "--try-load");

    let mut f = std::fs::File::open(path).map_err(|e| format!("打开 {path} 失败: {e}"))?;
    let mut magic = [0u8; 4];
    use std::io::Read;
    f.read_exact(&mut magic).map_err(|e| e.to_string())?;
    println!("文件: {path}");
    println!(
        "魔数: {:?}（GGUF={:?}；whisper.cpp 老格式={:?}）",
        magic, b"GGUF", b"lmgg"
    );

    if &magic != b"GGUF" {
        println!("\n不是 GGUF ⇒ candle 的量化 whisper 走不了这条路");
        println!(
            "（whisper.cpp 老 .bin 的布局：11 个 hparams + 文件内 mel filters + 无 score 词表，"
        );
        println!(" 与 candle `ggml_file` 期望的 llama.cpp 布局不同，所以连读都读不进来）");
        return Ok(());
    }

    // 必须 rewind：上面读魔数已经把文件指针推了 4 字节，直接往下读会让 candle
    // 把 version 当魔数（实测报 "unknown magic 0x00000003" —— 那是探针自己的错，不是文件的错）
    use std::io::Seek;
    f.rewind().map_err(|e| e.to_string())?;
    let content = gguf_file::Content::read(&mut f).map_err(|e| format!("读 GGUF 失败: {e}"))?;
    let mut meta: Vec<&String> = content.metadata.keys().collect();
    meta.sort();
    println!("\nGGUF 元数据 {} 项（前 25）:", meta.len());
    for k in meta.iter().take(25) {
        println!("   {k}");
    }
    let mut names: Vec<&String> = content.tensor_infos.keys().collect();
    names.sort();
    println!("\n张量 {} 个，前 15:", names.len());
    for n in names.iter().take(15) {
        println!("   {n}");
    }
    let hf = names.iter().any(|n| n.starts_with("model.encoder"));
    let wcpp = names.iter().any(|n| n.starts_with("encoder."));
    println!("\n命名判定: HF 风格(model.encoder.*)={hf} · whisper.cpp 风格(encoder.*)={wcpp}");
    println!(
        "   ⇒ candle 的量化 whisper {}",
        if hf {
            "命名对得上，可以试着加载"
        } else {
            "**命名对不上**（它按 HF 名字取张量）"
        }
    );

    if try_load {
        println!("\n── 真试一次加载（缺哪个张量就报哪个名字）──");
        let device = Device::Cpu;
        match candle_transformers::quantized_var_builder::VarBuilder::from_gguf(path, &device) {
            Ok(vb) => match quantized_model::Whisper::load(&vb, whisper_small_cfg()) {
                Ok(_) => println!("   加载成功 ✓（这枚文件可以直接用）"),
                Err(e) => println!("   加载失败: {e}"),
            },
            Err(e) => println!("   VarBuilder::from_gguf 失败: {e}"),
        }
    }
    Ok(())
}
