//! mel 滤波器组：**表来自 whisper 自己的 assets，不是我们算的**。
//!
//! 为什么直接编一张表（103KB）而不是在代码里算 librosa 那套公式：
//! 我按 librosa（Slaney 归一化）实现过一版，与 whisper 发布的 `mel_filters.npz` 比，
//! 第一个滤波器的系数就差了 0.5%（0.024747 vs 0.024863）—— 形状对、数值不等于对，
//! 而这种东西**错了不会报错**，只会让识别率悄悄变差、且极难定位。
//! 这份表的来源是权威的：`openai/whisper` 包里的 `whisper/assets/mel_filters.npz`
//! （MIT；由 librosa 的 `filters.mel(sr=16000, n_fft=400, n_mels=128, htf=False, norm='slaney')`
//! 生成），取出 `mel_128` 一维展开成 f32 LE 存成 `mel-filters.bin`。
//!
//! 布局：`[n_mels][1 + n_fft/2]` = `[128][201]`，行主序 —— 与
//! `candle_transformers::models::whisper::audio::pcm_to_mel` 期望的 `filters[j * 201 + k]` 一致。

/// 支持的 mel 通道数。**两套都编进来**：large-v3 系列是 128，small/base 是 80。
///
/// 为什么要两套：模型目录是用户可换的（自带权重也能跑），而通道数对不上时模型会直接崩或
/// 悄悄乱码 —— 所以按 `config.json` 的 `num_mel_bins` 挑表，挑不到就**明确报错**。
pub const SUPPORTED_MELS: [usize; 2] = [80, 128];

/// `1 + N_FFT/2`，N_FFT = 400
pub const N_FFT_BINS: usize = 201;

/// 默认（large-v3 系列）
pub const N_MELS: usize = 128;

/// whisper 的 mel 滤波器（f32 LE）—— 见模块头注释里的来源与许可证。
const MEL_128_BIN: &[u8] = include_bytes!("mel-filters.bin");
const MEL_80_BIN: &[u8] = include_bytes!("mel-80.bin");

fn decode(bytes: &[u8], n_mels: usize) -> Vec<f32> {
    assert_eq!(
        bytes.len(),
        n_mels * N_FFT_BINS * 4,
        "mel 表体积不对（应为 {n_mels}×{N_FFT_BINS} 个 f32）—— 换表时别忘了同步"
    );
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// 取出指定通道数的滤波器表（解码一次后缓存）。
///
/// **不支持的通道数直接报错**，不给"凑合一个尺寸差不多的表"的机会 —— 那种错会一路走到
/// 识别率变差，且没有任何症状。
pub fn filters_for(n_mels: usize) -> Result<Vec<f32>, String> {
    static C80: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    static C128: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    match n_mels {
        80 => Ok(C80.get_or_init(|| decode(MEL_80_BIN, 80)).clone()),
        128 => Ok(C128.get_or_init(|| decode(MEL_128_BIN, 128)).clone()),
        other => Err(format!(
            "内置的 mel 滤波器表只有 {SUPPORTED_MELS:?} 通道，模型要 {other} 通道 —— 请换一个模型或用同尺寸的表"
        )),
    }
}

/// 默认表（128 通道）。
pub fn filters() -> Vec<f32> {
    filters_for(N_MELS).expect("128 通道表是编进来的，取不到属于构建期缺陷")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 表的**形状**：逐行看峰值落在哪一列 —— 低通道只有 1 列非零、高通道铺开到 9 列。
    /// 这些基准值全部取自 `mel_filters.npz` 的 `mel_128`（不是我们算的），所以这条判据同时钉住了
    /// "有没有转置/错位"与"表还是那份表"。
    #[test]
    fn 滤波器表的位置与峰值必须与_whisper_发布的表一致() {
        let f = filters();
        assert_eq!(f.len(), N_MELS * N_FFT_BINS);
        let row = |i: usize| &f[i * N_FFT_BINS..(i + 1) * N_FFT_BINS];
        // (行, 峰所在列, 峰值, 该行非零个数)
        let cases: [(usize, usize, f32, usize); 8] = [
            (0, 1, 0.012373987, 1),
            (1, 1, 0.030392565, 1),
            (8, 5, 0.023663169, 1),
            (16, 10, 0.03820676, 1),
            (32, 19, 0.021493567, 1),
            (64, 43, 0.018091518, 2),
            (96, 92, 0.008922962, 4),
            (127, 195, 0.005041602, 9),
        ];
        for (i, col, peak, nz) in cases {
            let r = row(i);
            let (mut mx, mut mc) = (f32::MIN, 0usize);
            for (k, v) in r.iter().enumerate() {
                if *v > mx {
                    mx = *v;
                    mc = k;
                }
            }
            assert_eq!(mc, col, "第 {i} 行峰值列不对（转置了？）");
            assert!(
                (mx - peak).abs() < 1e-6,
                "第 {i} 行峰值不对：{mx} vs {peak}"
            );
            assert_eq!(
                r.iter().filter(|v| **v != 0.0).count(),
                nz,
                "第 {i} 行非零个数不对（滤波器宽度变了）"
            );
        }
        // 全表非零：0 行是特例（直流分量不进任何滤波器），别拿它当代表
        // 实测非零总数（whisper 发布的 mel_128 就是这么多：低通道只压 1 个 bin，高通道铺到 9 个）
        assert_eq!(f.iter().filter(|v| **v != 0.0).count(), 394);
        // 80 通道那套（small/base 用）也要在同一份 npz 口径下：抽查前几行的峰值列
        let f80 = filters_for(80).unwrap();
        assert_eq!(f80.len(), 80 * N_FFT_BINS);
        let row80 = |i: usize| &f80[i * N_FFT_BINS..(i + 1) * N_FFT_BINS];
        for (i, col, peak) in [
            (0, 1, 0.024862595),
            (20, 20, 0.013890394),
            (40, 43, 0.014735566),
            (79, 192, 0.0031647119),
        ]
        .map(|(a, b, c)| (a as usize, b as usize, c as f32))
        {
            let r = row80(i);
            let (mut mx, mut mc) = (f32::MIN, 0usize);
            for (k, v) in r.iter().enumerate() {
                if *v > mx {
                    mx = *v;
                    mc = k;
                }
            }
            assert_eq!(mc, col, "80 表第 {i} 行峰值列不对");
            assert!(
                (mx - peak).abs() < 1e-5,
                "80 表第 {i} 行峰值不对：{mx} vs {peak}"
            );
        }
        // 不支持的通道数必须报错（不许"凑合一个"）
        assert!(filters_for(96).is_err());
        assert!(
            f.iter().all(|v| v.is_finite() && *v >= 0.0),
            "系数必须非负且有限"
        );
    }
}
