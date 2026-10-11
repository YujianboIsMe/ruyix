fn main() {
    tauri_build::build();
    link_probe_manifest();
}

/// 给 **examples**（P0 探针）也嵌一份应用清单。
///
/// tauri-build 只给主二进制嵌清单，示例目标拿不到；而 tao/wry 会导入 comctl32 **v6 才有**的
/// 导出（`SetWindowSubclass` / `TaskDialogIndirect`）。没有清单时加载器挑 system32 里的 v5，
/// 进程在 `main` 之前就死在 `0xC0000139`（STATUS_ENTRYPOINT_NOT_FOUND）——**一个字都不输出**，
/// 看起来像"探针什么也没干"。这是 P0 探针实测踩出来的（见
/// `doc/架构-便携根与项目状态搬迁-v1.0.0.md` §八）。
///
/// 只作用于 `--examples`，不影响发布产物（主二进制继续用 tauri-build 自己那份）。
fn link_probe_manifest() {
    let is_windows = std::env::var("CARGO_CFG_WINDOWS").is_ok();
    let is_msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if !(is_windows && is_msvc) {
        return;
    }
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let manifest = std::path::Path::new(&dir)
        .join("examples")
        .join("probe.manifest");
    println!("cargo:rerun-if-changed=examples/probe.manifest");
    println!("cargo::rustc-link-arg-examples=/MANIFEST:EMBED");
    println!(
        "cargo::rustc-link-arg-examples=/MANIFESTINPUT:{}",
        manifest.display()
    );
}
