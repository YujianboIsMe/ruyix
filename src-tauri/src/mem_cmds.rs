//! 项目记忆（v1.1）的命令层 —— 2026-09-28 从 `main.rs` 拆出（1 拆 N 的第三刀）。
//!
//! 这里只有**命令层**：作用域解析（[`mem_scope`] / [`mem_root`]）+ 9 个 `mem_*` 命令，
//! 真正的记忆库在 `harness_engine::mem`（账本 → 派生层，`beliefs` / FTS / 向量）。
//! 记忆库没安装时命令要**明说**（"记忆库未安装"），而不是返回空数据 —— 空数据看起来像
//! "没有记忆"，而事实是"没装"。
//!
//! `main.rs` 只留注册（`mem_cmds::mem_*`，见 invoke_handler）。
//!
//! 下面是拆出来时**原封不动**的那段说明：

use tauri::Emitter;

// ---------------------------------------------------------------- 项目记忆（v1.1）

/// 记忆作用域：有项目用项目 key，没项目就是机器级。
fn mem_scope(root_paths: &crate::paths::Paths, project_root: Option<&str>) -> String {
    match project_root.filter(|s| !s.trim().is_empty()) {
        Some(p) => root_paths.project_key(p),
        None => harness_engine::mem::GLOBAL_SCOPE.to_string(),
    }
}

/// 便携根（记忆库与模型都在它下面）。引擎不认识便携根，所以由宿主每次算出来。
fn mem_root() -> crate::paths::Paths {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_default();
    crate::paths::Paths::discover(&exe_dir)
}

/// 模型自检：装着没有 / 装坏了 / 就绪，**带一句人话**（面板与状态栏都用它）。
#[tauri::command]
pub fn mem_model_status() -> serde_json::Value {
    let st = harness_engine::mem::fetch::status();
    serde_json::json!({
        "ready": st.is_ready(),
        "line": st.line(),
        "dir": harness_engine::mem::fetch::target_dir().ok().map(|d| d.display().to_string()),
        "need_bytes": harness_engine::mem::fetch::required_bytes(),
    })
}

/// 一条命令把模型补齐（面板上的〔取模型〕）。**立即返回**，进度走 `mem://model` 事件 ——
/// 96MB 的下载不该把界面卡住，也不该让命令挂在那儿。
#[tauri::command]
pub async fn mem_model_fetch(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let st = harness_engine::mem::fetch::status();
    if st.is_ready() {
        return Ok(serde_json::json!({ "started": false, "line": st.line() }));
    }
    let h = app.clone();
    let h2 = app.clone();
    let start_line = st.line();
    // spawn 是 async move：给它一份自己的，函数出口那份留给返回值
    let line_for_task = start_line.clone();
    tauri::async_runtime::spawn(async move {
        let _ = h.emit(
            "mem://model",
            serde_json::json!({ "phase": "start", "line": line_for_task }),
        );
        let res = harness_engine::mem::fetch::fetch(|p| {
            let _ = h2.emit(
                "mem://model",
                serde_json::json!({
                    "phase": "progress",
                    "file": p.file, "index": p.index, "of": p.of,
                    "done": p.done, "total": p.total, "tag": p.tag,
                }),
            );
        })
        .await;
        match res {
            Ok(rep) => {
                if let Some(m) = harness_engine::mem::current() {
                    let where_ = harness_engine::mem::fetch::target_dir()
                        .map(|d| d.display().to_string())
                        .unwrap_or_default();
                    let _ = m.record_obs(
                        harness_engine::mem::GLOBAL_SCOPE,
                        "mem.embed.model",
                        &where_,
                        harness_engine::mem::Origin::Probe,
                        &[],
                        None,
                    );
                }
                let _ = h.emit(
                    "mem://model",
                    serde_json::json!({
                        "phase": "done",
                        "fetched": rep.fetched.len(), "skipped": rep.skipped.len(), "bytes": rep.bytes,
                    }),
                );
            }
            Err(e) => {
                let _ = h.emit(
                    "mem://model",
                    serde_json::json!({ "phase": "error", "line": e }),
                );
            }
        }
    });
    Ok(serde_json::json!({ "started": true, "line": start_line }))
}

/// 记忆库状态：账本/信念/收据/向量条数 + 向量腿是否可用（**带人话原因**）。
#[tauri::command]
pub fn mem_status(project_root: Option<String>) -> Result<serde_json::Value, String> {
    let rp = mem_root();
    let scope = mem_scope(&rp, project_root.as_deref());
    let m = harness_engine::mem::current().ok_or("记忆库未安装（启动时打开失败）")?;
    let st = m.stats(&scope)?;
    Ok(serde_json::json!({
        "scope": scope,
        "db": rp.memory_dir().join("mem.db").display().to_string(),
        "events": st.events,
        "beliefs_active": st.beliefs_active,
        "beliefs_all": st.beliefs_all,
        "receipts": st.receipts,
        "vectors": st.vectors,
        "embed_available": harness_engine::mem::embed::is_available(),
        "embed_reason": harness_engine::mem::embed::unavailable_reason(),
    }))
}

/// 当前信念（`now`）：**先结构性过滤掉被推翻的**，再打分。query 为空 = 按最近更新取前 N。
#[tauri::command]
pub fn mem_beliefs(
    query: Option<String>,
    limit: Option<usize>,
    project_root: Option<String>,
) -> Result<serde_json::Value, String> {
    let rp = mem_root();
    let scope = mem_scope(&rp, project_root.as_deref());
    let m = harness_engine::mem::current().ok_or("记忆库未安装")?;
    let hits = m.beliefs_now(
        &scope,
        query.as_deref().unwrap_or(""),
        limit.unwrap_or(50).min(500),
        None,
    )?;
    Ok(serde_json::json!({
        "scope": scope,
        "hits": hits.iter().map(|h| serde_json::json!({
            "key": h.key, "value": h.value, "status": h.status,
            "valid_from": h.valid_from, "valid_to": h.valid_to,
            "prov": h.prov.len(), "score": h.score,
        })).collect::<Vec<_>>()
    }))
}

/// `why`：一条信念的完整修订链与依据 —— "你凭什么这么认为"。
#[tauri::command]
pub fn mem_why(key: String, project_root: Option<String>) -> Result<serde_json::Value, String> {
    let rp = mem_root();
    let scope = mem_scope(&rp, project_root.as_deref());
    let m = harness_engine::mem::current().ok_or("记忆库未安装")?;
    let evs = m.why(&scope, &key)?;
    Ok(serde_json::json!({
        "scope": scope,
        "chain": evs.iter().map(|e| serde_json::json!({
            "seq": e.seq, "ts": e.ts, "kind": e.kind.as_str(), "op": e.op,
            "value": e.value, "origin": e.origin.as_str(), "reason": e.reason,
            "prov": e.prov, "id": e.id,
        })).collect::<Vec<_>>()
    }))
}

/// `as-of`：某时刻的信念（**包含**当时成立、后来被推翻的）。
#[tauri::command]
pub fn mem_as_of(
    at: i64,
    key: Option<String>,
    project_root: Option<String>,
) -> Result<serde_json::Value, String> {
    let rp = mem_root();
    let scope = mem_scope(&rp, project_root.as_deref());
    let m = harness_engine::mem::current().ok_or("记忆库未安装")?;
    let hits = m.beliefs_as_of(&scope, key.as_deref(), at)?;
    Ok(serde_json::json!({
        "scope": scope, "at": at,
        "hits": hits.iter().map(|h| serde_json::json!({
            "key": h.key, "value": h.value, "status": h.status,
            "valid_from": h.valid_from, "valid_to": h.valid_to,
        })).collect::<Vec<_>>()
    }))
}

/// 收据：每次压缩/逐出"丢了什么、怎么换回来"（损失核算）。
#[tauri::command]
pub fn mem_receipts(
    limit: Option<usize>,
    project_root: Option<String>,
) -> Result<serde_json::Value, String> {
    let rp = mem_root();
    let scope = mem_scope(&rp, project_root.as_deref());
    let m = harness_engine::mem::current().ok_or("记忆库未安装")?;
    let rs = m.receipts(&scope, limit.unwrap_or(50).min(500))?;
    Ok(serde_json::json!({
        "scope": scope,
        "receipts": rs.iter().map(|r| serde_json::json!({
            "id": r.id, "ts": r.ts, "kind": r.kind,
            "covered": r.covered.len(), "dropped": r.dropped,
            "rehydrate": r.rehydrate, "note": r.note,
        })).collect::<Vec<_>>()
    }))
}

/// 人写一条（`origin=human`）：与探针/引擎决策在账本里同列，但来源可区分 ——
/// 这是"谁说的"这个问题的最小答案。
#[tauri::command]
pub fn mem_record(
    key: String,
    value: String,
    reason: Option<String>,
    project_root: Option<String>,
) -> Result<serde_json::Value, String> {
    let rp = mem_root();
    let scope = mem_scope(&rp, project_root.as_deref());
    let m = harness_engine::mem::current().ok_or("记忆库未安装")?;
    let ev = m.record_obs(
        &scope,
        key.trim(),
        &value,
        harness_engine::mem::Origin::Human,
        &[],
        None,
    )?;
    Ok(serde_json::json!({ "scope": scope, "id": ev.id, "reason": reason }))
}

/// 从账本重放重建派生层（`beliefs`/FTS/向量）—— **EC 的可操作形态**。
#[tauri::command]
pub async fn mem_rebuild(project_root: Option<String>) -> Result<serde_json::Value, String> {
    let rp = mem_root();
    let scope = mem_scope(&rp, project_root.as_deref());
    let m = harness_engine::mem::current()
        .ok_or("记忆库未安装")?
        .clone();
    let n = tauri::async_runtime::spawn_blocking(move || m.rebuild(Some(&scope)))
        .await
        .map_err(|e| format!("重建失败: {e}"))??;
    Ok(serde_json::json!({ "replayed_events": n }))
}
