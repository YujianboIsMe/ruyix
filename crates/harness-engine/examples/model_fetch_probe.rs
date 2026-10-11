//! 运行时取模型的**真机探针**（`cargo run -p harness-engine --example model_fetch_probe`）
//!
//! 单测里的下载是对着**本地 HTTP 桩**跑的（几字节的假文件），证明的是机制；
//! 这里补的是**真源**：用编译进二进制的 `model-spec.json` 里那两条 URL 模板，从 ModelScope
//! 真取一次最小的两个文件（`config.json` 776B + `vocab.txt` 110KB ≈ 110KB），逐字节校验 sha256。
//!
//! **不下载 96MB 的权重** —— 那是用户机器上的一次性动作，不属于探针；机制由那条路径覆盖：
//! 同一个 `fetch_into`、同一份清单、同一套校验。
//!
//! 判据（任何一条不成立即退出码 1，并打印原始证据）：
//!   1. 两个小文件都从**真源**取到（HTTP 200 + 字节数对）；
//!   2. sha256 与清单一致（换源拿到别的字节就会在这里红）；
//!   3. 落盘后自检认为这两个文件**就绪**（status_with）；
//!   4. 不留 `.part`（原子落盘的反面）；
//!   5. 再来一次**幂等**（不重下）。
//!
//! 需要网络。跑法：
//!   cargo run -q -p harness-engine --example model_fetch_probe

use harness_engine::mem::fetch::{self, Status};

fn main() {
    let spec = fetch::spec();
    // 只挑最小的两个必需文件：够验通"真源 URL + 真哈希 + 原子落盘"，又不往网上搬 96MB
    let mut small = spec.clone();
    small
        .files
        .retain(|f| f.name == "config.json" || f.name == "vocab.txt");
    assert_eq!(small.files.len(), 2, "清单里应当有这两个小文件");

    let dir = std::env::temp_dir().join(format!("ruyix-model-fetch-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);

    let mut bad = 0;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("建 runtime");
    let spec_for_call = small.clone();
    let dir_for_call = dir.clone();
    let rep = rt.block_on(async move {
        fetch::fetch_into(&spec_for_call, &dir_for_call, |p| {
            println!(
                "  · 取 {} ({}/{}) {:.1}/{:.1} KB [{}]",
                p.file,
                p.index,
                p.of,
                p.done as f64 / 1e3,
                p.total as f64 / 1e3,
                p.tag
            );
        })
        .await
    });

    match rep {
        Ok(r) => {
            println!("PASS  F1 真源取到 {:?}（{} 字节）", r.fetched, r.bytes);
            if r.fetched.len() != 2 {
                println!("FAIL  F1 应当取到两个文件，实际 {:?}", r.fetched);
                bad += 1;
            }
        }
        Err(e) => {
            println!("FAIL  F1 取模型失败：{e}");
            bad += 1;
        }
    }

    // ②③ 校验 + 自检（status_with 逐个重算 sha256）
    match fetch::status_with(&small, &dir) {
        Status::Ready { files, bytes, .. } => {
            println!("PASS  F2/F3 sha256 与清单一致，自检就绪（{files} 个文件 / {bytes} 字节）");
        }
        other => {
            println!("FAIL  F2/F3 自检不是就绪：{other:?}");
            bad += 1;
        }
    }

    // ④ 不留半截文件
    let parts: Vec<String> = std::fs::read_dir(&dir)
        .map(|d| {
            d.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.ends_with(".part"))
                .collect()
        })
        .unwrap_or_default();
    if parts.is_empty() {
        println!("PASS  F4 没有 .part 残留（半截文件不许看起来像装好了）");
    } else {
        println!("FAIL  F4 留下半截文件：{parts:?}");
        bad += 1;
    }

    // ⑤ 幂等
    let spec2 = small.clone();
    let dir2 = dir.clone();
    let again = rt.block_on(async move { fetch::fetch_into(&spec2, &dir2, |_| {}).await });
    match again {
        Ok(r) if r.fetched.is_empty() && r.skipped.len() == 2 => {
            println!("PASS  F5 二次运行幂等（跳过 {:?}，没重下）", r.skipped);
        }
        Ok(r) => {
            println!("FAIL  F5 二次运行该全跳过，实际 fetched={:?}", r.fetched);
            bad += 1;
        }
        Err(e) => {
            println!("FAIL  F5 二次运行失败：{e}");
            bad += 1;
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
    println!(
        "\n=== 取模型真机探针：{} ===",
        if bad == 0 {
            "全部达预期".to_string()
        } else {
            format!("{bad} 项不通过")
        }
    );
    std::process::exit(if bad == 0 { 0 } else { 1 });
}
