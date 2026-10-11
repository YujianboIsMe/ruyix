//! **向量腿**的硬证据自检（零联网、零成本、确定性）—— 2026-10-10 那一版的病是
//! "库里 `vectors` 表恒 0 行、检索只走词法腿，而**界面上看不出来**"：注入块里那句
//! "腿 = 词法（向量腿不可用：…缺文件：model.safetensors）" 是唯一线索，没人会去读。
//!
//! 用法（模型根目录就是便携根下那一处）：
//!
//! ```ignore
//! RUYIX_MEM_MODEL_DIR=D:/Tools/ruyix/global/memory/model \
//!   cargo run -q -p harness-engine --example mem_vector_check
//! ```
//!
//! ## 判据（退出码；一条不达预期就是 1）
//!
//! 1. 自检 `Status::Ready`：三个必需文件齐、**哈希都对**（不猜、不看目录大小）；
//! 2. `embed::is_available()` 为真；
//! 3. 嵌入**确定性**：同一段文本嵌两次，余弦 ≥ 0.999（换模型/半截权重会当场露出来）；
//! 4. 一次中文检索之后 **`vectors` 表里有行** —— 说明向量是**真算出来并落盘**的
//!    （此前恒 0 就是这一条不过）；
//! 5. **语义腿真的在排序里起作用**：用一条与信念**字面完全不重合**的中文提问，
//!    仍然命中 —— 纯词法腿在这一问上必然空手（这是"两条腿"与"一条腿"的分水岭）。
//!
//! ## 环境不齐时的退出码 3
//!
//! 模型没装 / 目录没配 ⇒ **大声跳过**（打印人话原因），绝不假装通过 ——
//! 与 `mem_layers_smoke.rs` 同款纪律。

use harness_engine::mem::{self, Memory};

fn main() {
    // ---------- ① 自检 ----------
    let st = mem::fetch::status();
    println!("=== 自检：{}", st.line());
    if let Ok(dir) = mem::fetch::target_dir() {
        println!("=== 目录：{}", dir.display());
    }
    if !st.is_ready() {
        eprintln!(
            "[skip] 向量腿不可用（{}）—— 这一趟不跑，也不假装通过。\n\
             补齐：把模型三个文件放进 <便携根>/global/memory/model/<dir_name>/（打包期 \
             `node scripts/fetch-embed-model.mjs`，或界面上的〔取模型〕）",
            st.line()
        );
        std::process::exit(3);
    }

    // ---------- ② 嵌入可用 ----------
    if !mem::embed::is_available() {
        eprintln!(
            "✗ 自检说就绪、`is_available()` 却说不可用（两处判据必须同源）：{}",
            mem::embed::unavailable_reason()
        );
        std::process::exit(1);
    }
    println!("=== 腿 = 词法 + 向量（is_available = true）");

    // ---------- ③ 确定性 ----------
    let model = match mem::embed::load_cached() {
        Ok(Some(m)) => m,
        Ok(None) => {
            eprintln!("✗ 加载不出模型（文件在、加载失败）—— 看 debug 日志里的 [mem] 那行");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("✗ 加载模型出错：{e}");
            std::process::exit(1);
        }
    };
    let a = model.embed("用户明天有两场面试", false).unwrap_or_default();
    let b = model.embed("用户明天有两场面试", false).unwrap_or_default();
    let same = mem::embed::cosine(&a, &b);
    println!("=== 嵌入维度 {} · 同一文本两次的余弦 {same:.6}", a.len());
    if a.len() != mem::embed::DIM || same < 0.999 {
        eprintln!(
            "✗ 嵌入不确定 / 维度不对（期望 {} 维、余弦 ≥ 0.999）",
            mem::embed::DIM
        );
        std::process::exit(1);
    }

    // ---------- ④⑤ 检索侧：向量真的落盘、且语义腿在排序里起作用 ----------
    let dir = std::env::temp_dir().join(format!("ruyix-vec-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    let db = dir.join("mem.db");
    let m = Memory::open(&db, vec![]).expect("开临时记忆库");
    let scope = "vec-check";

    // 一条**只有语义相关**的信念：提问用同义说法，字面一个词都不重合。
    m.record_obs(
        scope,
        "user.面试",
        "委托人自述：明天（周六）有两场面试，上午一场下午一场",
        mem::Origin::Human,
        &[],
        None,
    )
    .expect("记一条信念");
    m.record_obs(
        scope,
        "unrelated.one",
        "这个模块的日志级别默认是 info",
        mem::Origin::Probe,
        &[],
        None,
    )
    .expect("记一条对照信念");

    let q = "他最近的日程安排是什么";
    println!("=== 检索（字面不重合）：「{q}」");
    let hits = mem::retrieve::now(&m, scope, q, 5, None).unwrap_or_default();
    for h in &hits {
        println!("    - `{}`（{:.4}，状态 {}）", h.key, h.score, h.status);
    }
    let vectors = m.stats(scope).map(|s| s.vectors).unwrap_or(0);
    // 面板上那个数字（`mem_status` / 记忆面板读的就是它）与**库里的真值**分别打一遍：
    // 两者不一致本身就是一条要看的读数（2026-10-10 就是"真值 >0、面板恒 0"）。
    let raw: i64 = rusqlite::Connection::open(&db)
        .and_then(|c| c.query_row("SELECT COUNT(*) FROM vectors", [], |r| r.get(0)))
        .unwrap_or(-1);
    println!("=== `vectors` 表：面板数字 {vectors} · 库里真值 {raw}（这一层以前两边都是 0）");

    let _ = std::fs::remove_dir_all(&dir);

    if vectors == 0 {
        eprintln!("✗ 面板上的向量条数恒 0（`stats().vectors`）—— 检索只剩词法腿，而界面看不出来");
        std::process::exit(1);
    }
    if raw <= 0 {
        eprintln!("✗ 向量根本没落盘（库里真值 {raw}）—— 语义腿不可能起作用");
        std::process::exit(1);
    }
    if hits.is_empty() {
        eprintln!("✗ 字面不重合的提问一条都没命中 —— 纯词法腿必然如此，说明**语义腿没参与排序**");
        std::process::exit(1);
    }
    if !hits.iter().any(|h| h.key == "user.面试") {
        eprintln!("✗ 命中了，但没命中语义最相关的那条（排序不是语义腿给的）");
        std::process::exit(1);
    }
    println!("✓ 通过：三个必需文件齐且哈希对、向量真落盘、语义腿在排序里起作用");
}
