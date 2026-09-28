//! 会话附件（v1.3 多模态输入）：把用户粘贴 / 拖进来 / 选中的截图**落盘**，再把字节交给引擎。
//!
//! **为什么必须落盘，而不是把 base64 一路传下去**：
//! ① 截图是**事实**（用户报 UI bug 的现场）。会话重开之后它还得在 —— 只活在内存里的图，
//!    重启一次就没了，而"当时那张图长什么样"恰恰是之后复盘时最想回看的东西；
//! ② 落盘位置是**便携根的项目桶**（`<便携根>/projects/<项目 key>/shots/`），一个字节都不进
//!    用户仓库 —— 与 stage / backups / sessions 同一条纪律（v1.0.0 的旗舰约束）；
//! ③ 引擎那侧只收 base64（零文件系统假设），落盘与"交给引擎的字节"分成两件事，各归各的闸。
//!
//! **谁缩图**：前端。浏览器里现成就有完整图像编解码器（canvas），把图缩到最长边 ≤ 1600px
//! 只要几行 JS；宿主/引擎这边为此引入一个 `image` 依赖换不来什么。所以宿主收到的已经是
//! 缩好的图，它只做**闸**与**落盘**（实测：9.6 MB base64 的原图往返要 84 秒，缩过之后就正常了）。
//!
//! **三道闸（顺序即"在能执行之前"）**：
//! 1. 张数与总体积：一条消息最多 [`MAX_PER_MESSAGE`] 张、合计 [`MAX_TOTAL_BYTES`] ——
//!    只看单张限额拦不住"一次发 6 张 9 MB"；
//! 2. 声明的 mime 必须在白名单里（png / jpeg / webp）—— 不接受的类型直接说清楚，不静默丢；
//! 3. **按字节嗅探**真类型（magic bytes）：与声明不符时**以字节为准**，认不出来一律拒。
//!    这条是"执行之前的闸"：`gif` / `svg` 这类即便改名成 `.png` 也进不来，而一张被
//!    误标成 jpeg 的 png 照收不误（声明是人写的，字节是事实）。

use base64::Engine as _;
use harness_engine as engine;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 项目桶内的截图目录（与 `stage` / `backups` / `sessions` 同级）
pub const SHOT_DIR: &str = "shots";
/// 一条消息最多几张
pub const MAX_PER_MESSAGE: usize = 6;
/// 单张解出来的字节上限（硬拒；正常的 1600px JPEG 在 0.2~1 MB 量级）
pub const MAX_BYTES: usize = 10 * 1024 * 1024;
/// 一条消息全部图的字节上限
pub const MAX_TOTAL_BYTES: usize = 20 * 1024 * 1024;
/// 白名单（声明侧）。真正认下来的是 [`sniff`] 的结论。
pub const ALLOWED_MIME: &[&str] = &["image/png", "image/jpeg", "image/webp"];

/// 前端发来的一张图（前端已缩过；这里只是"待检的字节"）
#[derive(Debug, Clone, Deserialize)]
pub struct Attachment {
    /// 用户那边的原始文件名（可空；只用来给文件起个可读的名，**不参与路径构造**）
    #[serde(default)]
    pub name: String,
    pub mime: String,
    /// 裸 base64；容忍 `data:<mime>;base64,` 前缀与换行（前端两种来源都可能给）
    pub data_base64: String,
}

/// 落盘之后的那张图（**会话里存的就是这个**：路径 + 元数据，不存字节）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredShot {
    /// 绝对路径（UI 拿它去 `read_file_base64` 渲染缩略图）
    pub path: String,
    /// 磁盘上的文件名
    pub name: String,
    /// **按字节嗅探出来的**类型
    pub mime: String,
    pub bytes: u64,
}

/// 一次落盘的两个产物：给会话存的一份（路径）与给模型的一份（字节）
#[derive(Debug, Clone)]
pub struct Ingested {
    pub shots: Vec<StoredShot>,
    pub parts: Vec<engine::llm::ImagePart>,
}

/// 截图目录：`<便携根>/projects/<项目 key>/shots`
pub fn shots_dir(project_root: &str) -> PathBuf {
    crate::paths::current().project_bucket(project_root, SHOT_DIR)
}

/// 检查 + 落盘 + 拆出"给模型的那份字节"。
///
/// 失败一律 `Err`，且**整条消息都不落**（要么全收下、要么当面说清是哪一张为什么不行）——
/// 半数收下会让用户以为图发出去了。
pub fn store(project_root: &str, items: &[Attachment]) -> Result<Ingested, String> {
    if items.is_empty() {
        return Ok(Ingested {
            shots: Vec::new(),
            parts: Vec::new(),
        });
    }
    if items.len() > MAX_PER_MESSAGE {
        return Err(format!(
            "一条消息最多 {MAX_PER_MESSAGE} 张图（收到 {} 张）",
            items.len()
        ));
    }
    // ---- 第一遍：全部检查（检查期间不碰盘）----
    struct Checked {
        mime: &'static str,
        bytes: Vec<u8>,
        b64: String,
        name: String,
    }
    let mut checked: Vec<Checked> = Vec::with_capacity(items.len());
    let mut total = 0usize;
    for (i, it) in items.iter().enumerate() {
        let n = i + 1;
        let declared = norm_mime(&it.mime);
        if !ALLOWED_MIME.contains(&declared.as_str()) {
            return Err(format!(
                "第 {n} 张图的类型 {} 不在白名单（{}）",
                it.mime.trim(),
                ALLOWED_MIME.join(" / ")
            ));
        }
        let b64 = strip_data_url(&it.data_base64);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.as_bytes())
            .map_err(|e| format!("第 {n} 张图的 base64 解不开（{e}）"))?;
        if bytes.is_empty() {
            return Err(format!("第 {n} 张图是空的"));
        }
        if bytes.len() > MAX_BYTES {
            return Err(format!(
                "第 {n} 张图 {} MB，超过单张上限 {} MB",
                bytes.len() / (1024 * 1024),
                MAX_BYTES / (1024 * 1024)
            ));
        }
        total += bytes.len();
        if total > MAX_TOTAL_BYTES {
            return Err(format!(
                "这些图合计 {} MB，超过上限 {} MB",
                total / (1024 * 1024),
                MAX_TOTAL_BYTES / (1024 * 1024)
            ));
        }
        let Some(mime) = sniff(&bytes) else {
            return Err(format!(
                "第 {n} 张图认不出真实格式（只收 PNG / JPEG / WebP；字节不会骗人，声明说是 {}）",
                it.mime.trim()
            ));
        };
        checked.push(Checked {
            mime,
            bytes,
            b64,
            name: it.name.clone(),
        });
    }
    // ---- 第二遍：写盘（目录建不出来就是硬失败，不静默退化）----
    let dir = shots_dir(project_root);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("截图目录建不出来 {}: {e}", dir.display()))?;
    let mut shots = Vec::with_capacity(checked.len());
    let mut parts = Vec::with_capacity(checked.len());
    for c in checked {
        let path = unique_path(&dir, &c.name, c.mime);
        std::fs::write(&path, &c.bytes)
            .map_err(|e| format!("截图写不进去 {}: {e}", path.display()))?;
        shots.push(StoredShot {
            path: crate::clean_path(&path),
            name: path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            mime: c.mime.to_string(),
            bytes: c.bytes.len() as u64,
        });
        parts.push(engine::llm::ImagePart {
            mime: c.mime.to_string(),
            data_base64: c.b64,
        });
    }
    Ok(Ingested { shots, parts })
}

/// 声明侧归一：小写、去空白、`image/jpg` 归到 `image/jpeg`
fn norm_mime(m: &str) -> String {
    let m = m.trim().to_ascii_lowercase();
    if m == "image/jpg" {
        "image/jpeg".into()
    } else {
        m
    }
}

/// 容忍 `data:<mime>;base64,` 前缀与换行 —— 前端可能直接给 `FileReader.readAsDataURL` 的结果
fn strip_data_url(s: &str) -> String {
    let s = s.trim();
    let s = match s.find(";base64,") {
        Some(i) if s.starts_with("data:") => &s[i + ";base64,".len()..],
        _ => s,
    };
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// **按字节嗅探**图片类型（magic bytes）。认不出来 = `None`（调用方拒）。
fn sniff(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

fn ext_for(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => "png",
    }
}

/// 文件名里只留"看得懂且安全"的字符：字母数字 / 中日韩 / `-` `_`。
/// **路径分隔符、`..`、空格、其他符号全部变成 `_`** —— 名字来自用户侧，不能进路径构造。
fn sanitize(name: &str) -> String {
    let stem = Path::new(name.trim())
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let kept: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = kept.trim_matches('_');
    if trimmed.is_empty() {
        "shot".into()
    } else {
        trimmed.chars().take(40).collect()
    }
}

/// `20260928-153000-shot.png`；同秒撞了加 `-2`、`-3`（截图连拍是常态，不撞才怪）
fn unique_path(dir: &Path, name: &str, mime: &str) -> PathBuf {
    let stamp = engine::workspace::new_run_id("shot");
    let stem = format!("{stamp}-{}", sanitize(name));
    let ext = ext_for(mime);
    let mut p = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{stem}-{n}.{ext}"));
        n += 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ruyix-shots-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn png_bytes() -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(&[0u8; 32]);
        v
    }

    /// 前端发来的 JSON 形状：**嵌套字段不做 camelCase 转换**（Tauri 只转命令参数名），
    /// 所以字段名必须与 Rust 侧一字不差 —— 写错的话症状是"用户点了发送才报一个看不懂的错"。
    /// 这条把形状钉在测试里，不靠"我读代码时是对的"。
    #[test]
    fn the_json_shape_the_ui_sends_deserializes() {
        let raw = r#"[{"name":"屏幕截图.png","mime":"image/png","data_base64":"QUJD"}]"#;
        let items: Vec<Attachment> = serde_json::from_str(raw).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "屏幕截图.png");
        assert_eq!(items[0].mime, "image/png");
        assert_eq!(items[0].data_base64, "QUJD");
        // 文件名可以缺（前端没有必须给文件名的义务）
        let bare: Vec<Attachment> =
            serde_json::from_str(r#"[{"mime":"image/png","data_base64":"QUJD"}]"#).unwrap();
        assert_eq!(bare[0].name, "");
    }

    fn att(name: &str, mime: &str, bytes: &[u8]) -> Attachment {
        Attachment {
            name: name.into(),
            mime: mime.into(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }

    fn root(d: &Path) -> String {
        let p = d.join("proj");
        std::fs::create_dir_all(&p).unwrap();
        crate::paths::set_test_root(d.join("root"));
        p.to_string_lossy().into_owned()
    }

    /// 落盘位置在**便携根的项目桶**里（不进用户仓库），且字节一字不差 —— 这是本模块存在的理由
    #[test]
    fn a_shot_lands_in_the_project_bucket_byte_for_byte() {
        let d = tmp("land");
        let proj = root(&d);
        let bytes = png_bytes();
        let got = store(&proj, &[att("屏幕截图.png", "image/png", &bytes)]).unwrap();
        assert_eq!(got.shots.len(), 1);
        let shot = &got.shots[0];
        let dir = shots_dir(&proj);
        assert!(
            Path::new(&shot.path).starts_with(&dir),
            "必须落在 {dir:?} 之下：{}",
            shot.path
        );
        assert_eq!(
            std::fs::read(&shot.path).unwrap(),
            bytes,
            "落盘字节要一字不差"
        );
        assert_eq!(shot.bytes, bytes.len() as u64);
        // 用户仓库里一个字节都不许出现。判据是**目录里一个条目都没有**（比"没有某个
        // 特定目录名"更强：嵌套写入也得先多出一层目录才藏得进去）。
        // 不写那句字面量断言还有个具体原因：`paths::tests::no_global_path_construction_outside_this_file`
        // 会扫全仓宿主源码（含测试）里的路径字面量，而那条纪律本身是对的 —— 路径只许在 paths.rs 里拼。
        assert_eq!(
            std::fs::read_dir(&proj).unwrap().count(),
            0,
            "项目里不该多出东西"
        );
        // 给模型的那份是**裸** base64（不带 data: 前缀），mime 按字节认定
        assert_eq!(got.parts.len(), 1);
        assert_eq!(got.parts[0].mime, "image/png");
        assert!(!got.parts[0].data_base64.starts_with("data:"));
        assert_eq!(
            got.parts[0].data_base64,
            att("x", "image/png", &bytes).data_base64
        );
    }

    /// 三道闸各拒一次：白名单外的声明、按字节认不出的格式、超单张体积。
    /// 关键在"**整条消息都不落**"：半收下会让用户以为图发出去了。
    #[test]
    fn the_three_gates_refuse_and_leave_nothing_behind() {
        let d = tmp("gates");
        let proj = root(&d);
        // ① 声明侧白名单
        let e = store(&proj, &[att("a.gif", "image/gif", &png_bytes())]).unwrap_err();
        assert!(e.contains("白名单"), "{e}");
        // ② 字节侧嗅探：声明说是 png，字节是 GIF
        let gif = b"GIF89a\x01\x00\x01\x00".to_vec();
        let e = store(&proj, &[att("a.png", "image/png", &gif)]).unwrap_err();
        assert!(e.contains("认不出真实格式"), "{e}");
        // ③ 声明与字节不符（标 jpeg 的 png）⇒ 以字节为准，照收
        let ok = store(&proj, &[att("a.jpg", "image/jpeg", &png_bytes())]).unwrap();
        assert_eq!(ok.parts[0].mime, "image/png", "字节才是事实");
        // ④ 体积（先解密再量，量的是**解出来的字节**而不是 base64 长度）
        let big = vec![0u8; MAX_BYTES + 1];
        let mut big_png = png_bytes();
        big_png.extend_from_slice(&big);
        let e = store(&proj, &[att("big.png", "image/png", &big_png)]).unwrap_err();
        assert!(e.contains("超过单张上限"), "{e}");
        // ⑤ 空
        let e = store(&proj, &[att("empty.png", "image/png", &[])]).unwrap_err();
        assert!(e.contains("空的"), "{e}");
        // ⑥ base64 解不开
        let bad = Attachment {
            name: "x.png".into(),
            mime: "image/png".into(),
            data_base64: "!!!not base64!!!".into(),
        };
        assert!(store(&proj, &[bad]).unwrap_err().contains("解不开"));
        // ⑦ 目录里除了那次成功的一张，什么都不该多
        let n = std::fs::read_dir(shots_dir(&proj)).unwrap().count();
        assert_eq!(n, 1, "被拒的消息一张都不许落盘（当前 {n}）");
    }

    /// 一次能发几张：上限内全落，超一张就整条拒绝；合计体积也有一道闸
    #[test]
    fn the_per_message_limits_are_enforced_on_the_whole_batch() {
        let d = tmp("count");
        let proj = root(&d);
        let many: Vec<Attachment> = (0..MAX_PER_MESSAGE)
            .map(|i| att(&format!("s{i}.png"), "image/png", &png_bytes()))
            .collect();
        assert_eq!(store(&proj, &many).unwrap().shots.len(), MAX_PER_MESSAGE);
        let mut over = many.clone();
        over.push(att("extra.png", "image/png", &png_bytes()));
        let e = store(&proj, &over).unwrap_err();
        assert!(e.contains("最多 6 张"), "{e}");
        // 合计体积：单张都在限内，凑起来超过 MAX_TOTAL_BYTES
        let chunk = vec![0u8; MAX_TOTAL_BYTES / 4];
        let heavy: Vec<Attachment> = (0..5)
            .map(|i| {
                let mut v = png_bytes();
                v.extend_from_slice(&chunk);
                att(&format!("h{i}.png"), "image/png", &v)
            })
            .collect();
        let e = store(&proj, &heavy).unwrap_err();
        assert!(e.contains("合计"), "{e}");
    }

    /// 用户给的**文件名不能进路径构造**：分隔符 / `..` 一律变成 `_`，且仍在 shots 目录里
    #[test]
    fn a_hostile_file_name_cannot_escape_the_shots_dir() {
        let d = tmp("name");
        let proj = root(&d);
        let got = store(
            &proj,
            &[att("../../../etc/passwd.png", "image/png", &png_bytes())],
        )
        .unwrap();
        let p = Path::new(&got.shots[0].path);
        assert!(
            p.starts_with(shots_dir(&proj)),
            "逃出 shots 了：{}",
            p.display()
        );
        assert!(!got.shots[0].path.contains(".."));
        // 分隔符 / 点号一律不残留（`file_stem` 先吃掉一段，剩下的由字符白名单兜住）
        for name in [
            "../../../etc/passwd.png",
            "C:\\Windows\\evil.png",
            "a/b.png",
        ] {
            let s = sanitize(name);
            assert!(!s.contains(['/', '\\', '.']), "{name} -> {s}");
        }
        assert_eq!(sanitize(""), "shot");
        assert_eq!(sanitize("屏幕 截图 (1).png"), "屏幕_截图__1");
        // base64 侧的宽容：`data:` 前缀与换行都吃掉（前端两种来源都可能给）
        assert_eq!(strip_data_url("data:image/png;base64,QUJD"), "QUJD");
        assert_eq!(strip_data_url("QUJD\r\n"), "QUJD");
    }

    /// 前端两种给法都认：`data:` URL（FileReader 的结果）与裸 base64；连拍不覆盖
    #[test]
    fn data_urls_are_accepted_and_bursts_do_not_overwrite() {
        let d = tmp("url");
        let proj = root(&d);
        let bytes = png_bytes();
        let raw = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let as_url = Attachment {
            name: "s.png".into(),
            mime: "image/png".into(),
            data_base64: format!("data:image/png;base64,{raw}"),
        };
        let got = store(&proj, &[as_url]).unwrap();
        assert_eq!(std::fs::read(&got.shots[0].path).unwrap(), bytes);
        // 连拍两张（同秒、同名）→ 两条不同路径
        let a = store(&proj, &[att("shot.png", "image/png", &bytes)]).unwrap();
        let b = store(&proj, &[att("shot.png", "image/png", &bytes)]).unwrap();
        assert_ne!(a.shots[0].path, b.shots[0].path, "同秒连拍不许覆盖");
    }

    /// 会话重开之后还能按路径把**同一张图**取回来（缩略图与"再发给模型"都靠它）：
    /// 落盘 → `read_file_base64` 那条路读出来的 base64 必须与当时发出去的那份一致
    #[test]
    fn a_shot_survives_a_round_trip_through_the_disk() {
        let d = tmp("readback");
        let proj = root(&d);
        let bytes = png_bytes();
        let got = store(&proj, &[att("s.png", "image/png", &bytes)]).unwrap();
        let reread = base64::engine::general_purpose::STANDARD
            .encode(std::fs::read(&got.shots[0].path).unwrap());
        assert_eq!(
            reread, got.parts[0].data_base64,
            "取回来的字节要与发出去的那份一致"
        );
        // 会话里存的是路径 + 元数据（**不存字节**），四个字段够 UI 渲染缩略图与说明
        let json = serde_json::to_string(&got.shots).unwrap();
        assert!(
            json.contains("\"path\"") && json.contains("\"name\"") && json.contains("\"mime\"")
        );
        assert!(!json.contains(&reread[..40]), "字节不该进会话文件：{json}");
    }
}
