//! 提问通道（`ask_user`，v0.8）：引擎的 `Asker` 契约在 ruyix 的落地。
//!
//! 引擎只声明"能问人"，**等待与超时由这里负责**（引擎没有计时器）。三条纪律：
//! - 问到人就给答案；拿不到人（超时 / 被取消 / 窗口发不出事件）一律 `Err` —— fail-closed，
//!   **绝不替用户猜一个答案**（那比不问更糟：模型会把它当成许可）。
//! - 幂等：同一个 `ask_id` 只认第一次回答；超时之后迟到的回答不会命中（id 已从表里摘掉）。
//! - 取消：`agent_cancel` 会把待答问题一起清掉（丢弃发送端 → 挂起的循环立刻拿到 Canceled），
//!   否则用户取消后循环还要挂到超时。

use engine::agent::{AskAnswer, AskErr, AskFut, AskSpec, Asker};
use harness_engine as engine;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tokio::sync::oneshot;

/// 待答问题表：`ask_id → 回话通道`。放在 `AgentState` 里（不是 asker 里），
/// 这样 `agent_cancel` 与 `agent_ask_answer` 两条命令都能摸到同一份表。
pub type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<(String, Option<usize>)>>>>;

/// 用户的回答：文本 + 所选选项下标（自由文本为 `None`）
pub type Reply = (String, Option<usize>);

pub struct RuyixAsker {
    app: AppHandle,
    pending: Pending,
}

impl RuyixAsker {
    pub fn new(app: AppHandle, pending: Pending) -> Self {
        Self { app, pending }
    }
}

impl Asker for RuyixAsker {
    fn ask<'a>(&'a self, id: &'a str, spec: &'a AskSpec) -> AskFut<'a> {
        let (tx, rx) = oneshot::channel();
        if let Ok(mut m) = self.pending.lock() {
            m.insert(id.to_string(), tx);
        }
        // 问题卡：question / why / options / default_index 原样给前端 ——
        // `why`（这个答案会决定什么）是审计与反社会工程学的硬要求，必须展示。
        let payload = serde_json::json!({
            "id": id,
            "question": spec.question,
            "why": spec.why,
            "options": spec.options,
            "default_index": spec.default_index,
            "timeout_secs": spec.timeout_secs,
        });
        if let Err(e) = self.app.emit("agent://ask", payload) {
            // 发不出去（窗口没了）：立刻按"没人可问"收场，别把循环挂在那里
            if let Ok(mut m) = self.pending.lock() {
                m.remove(id);
            }
            let msg = format!("提问事件发不出去: {e}");
            return Box::pin(async move { Err(AskErr::Failed(msg)) });
        }
        let timeout_secs = spec.timeout_secs;
        let pending_after = Arc::clone(&self.pending);
        let id_owned = id.to_string();
        Box::pin(async move {
            let got: Result<Reply, AskErr> = if timeout_secs == 0 {
                // 0 = 无限等（桌面场景用户就在旁边）；取消仍能把它叫醒
                rx.await.map_err(|_| AskErr::Canceled)
            } else {
                match tokio::time::timeout(Duration::from_secs(timeout_secs), rx).await {
                    Ok(Ok(v)) => Ok(v),
                    // 发送端被丢了：取消或窗口重开
                    Ok(Err(_)) => Err(AskErr::Canceled),
                    Err(_) => Err(AskErr::Timeout),
                }
            };
            if let Ok(mut m) = pending_after.lock() {
                m.remove(&id_owned);
            }
            got.map(|(text, option_index)| AskAnswer {
                text,
                option_index,
                ts: engine::workspace::now_iso(),
            })
        })
    }
}

/// 把用户的回答投给挂起的循环。返回 `false` = 这个 `ask_id` 已经不在表里
/// （超时 / 已取消 / 已答过）—— 前端据此提示"这个提问已经失效"，而不是假装成功。
pub fn deliver(pending: &Pending, ask_id: &str, text: &str, option_index: Option<usize>) -> bool {
    let tx = pending.lock().ok().and_then(|mut m| m.remove(ask_id));
    match tx {
        Some(tx) => tx.send((text.to_string(), option_index)).is_ok(),
        None => false,
    }
}

/// 清空待答问题（取消时调）：丢弃发送端 → 每个挂起的 `ask` 都拿到 `Canceled`
pub fn drop_all(pending: &Pending) -> usize {
    let mut n = 0;
    if let Ok(mut m) = pending.lock() {
        n = m.len();
        m.clear();
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deliver_is_idempotent_and_reports_misses() {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (tx, _rx) = oneshot::channel();
        pending.lock().unwrap().insert("ask-1".into(), tx);
        assert!(
            deliver(&pending, "ask-1", "答案", Some(1)),
            "第一次该投递成功"
        );
        // 第二次：表里已经没有它了（超时之后迟到的回答不许再命中）
        assert!(!deliver(&pending, "ask-1", "又一次", None));
        assert!(!deliver(&pending, "ask-9", "不存在的 id", None));
        assert_eq!(drop_all(&pending), 0, "已经空了");
    }

    #[test]
    fn drop_all_releases_every_waiter() {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (_tx1, rx1) = oneshot::channel::<Reply>();
        let (_tx2, rx2) = oneshot::channel::<Reply>();
        {
            let mut m = pending.lock().unwrap();
            m.insert("ask-1".into(), _tx1);
            m.insert("ask-2".into(), _tx2);
        }
        assert_eq!(drop_all(&pending), 2);
        // 发送端被丢弃后，接收端不可能再等到"答案值"（挂起的 ask 会拿到 Closed → Canceled），
        // 于是循环立刻醒，而不是挂到超时
        assert!(
            rx1.is_empty() && rx2.is_empty(),
            "丢弃发送端后不该有任何答案值"
        );
        assert!(drop_all(&pending) == 0, "清空后表是空的，重复清不报错");
    }
}
