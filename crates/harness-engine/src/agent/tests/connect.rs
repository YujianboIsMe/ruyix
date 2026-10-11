//! Connect 原语：三种形态路由到宿主、失败与"没接任何外部能力"的措辞、
//! 连接清单进提示词的渲染（对应 `connect.rs`）。

use super::*;

/// connect：list / call / send 三形态各自把请求原样送达宿主，工具名与简报可读
#[test]
fn connect_routes_three_forms_to_host() {
    let rec = Recorder {
        calls: std::sync::Mutex::new(Vec::new()),
        fail: false,
    };
    let (tool, brief, r) = block_on(connect_step(&rec, ConnectAction::List));
    assert_eq!(tool, "connect");
    assert_eq!(brief, "connect list");
    assert!(r.unwrap().contains("mcp fs：已连接"), "清单要带连接状态");

    let (_, brief, r) = block_on(connect_step(
        &rec,
        ConnectAction::Call {
            server: "fs".into(),
            tool: "read_file".into(),
            arguments: serde_json::json!({"path": "a"}),
        },
    ));
    assert_eq!(brief, "connect fs/read_file");
    assert_eq!(r.unwrap(), "call 返回");

    let (_, brief, _) = block_on(connect_step(
        &rec,
        ConnectAction::Send {
            agent: "翻译".into(),
            text: "你好".into(),
        },
    ));
    assert_eq!(brief, "connect 翻译（委托 2 字）");

    let got = rec.calls.lock().unwrap();
    assert_eq!(got.len(), 2, "list 不该走 call");
    assert_eq!(got[0].action, "call");
    assert_eq!(got[0].server, "fs");
    assert_eq!(got[0].tool, "read_file");
    assert_eq!(got[0].arguments["path"], "a");
    assert_eq!(got[1].action, "send");
    assert_eq!(got[1].agent, "翻译");
    assert_eq!(got[1].text, "你好");
}

/// 连不上 = Err（提示词那侧记失败轮）；没接外部能力的宿主清单为空
#[test]
fn connect_failure_and_no_connector() {
    let rec = Recorder {
        calls: std::sync::Mutex::new(Vec::new()),
        fail: true,
    };
    let (_, _, r) = block_on(connect_step(
        &rec,
        ConnectAction::Call {
            server: "fs".into(),
            tool: "x".into(),
            arguments: serde_json::Value::Null,
        },
    ));
    assert!(r.unwrap_err().contains("连不上 fs"));

    let none = NoConnector;
    let (_, _, r) = block_on(connect_step(&none, ConnectAction::List));
    assert_eq!(r.unwrap(), "（当前没有可连接的外部能力）");
    let (_, _, r) = block_on(connect_step(
        &none,
        ConnectAction::Send {
            agent: "翻译".into(),
            text: "你好".into(),
        },
    ));
    assert!(r.is_err(), "没有连接器时 send 必须失败，不能假装成功");
}

/// 清单注入提示词：空清单不出现，非空逐行列出种类/名字/工具
#[test]
fn connect_note_renders_targets() {
    assert!(connect_note(&[]).is_none());
    let note = connect_note(&[
        ConnectTarget {
            kind: "mcp".into(),
            name: "fs".into(),
            detail: "已连接".into(),
            tools: vec!["read_file(读文件)".into()],
        },
        ConnectTarget {
            kind: "a2a".into(),
            name: "translator".into(),
            detail: "http://127.0.0.1:9999".into(),
            tools: vec![],
        },
    ])
    .unwrap();
    assert!(
        note.contains("mcp fs：已连接；工具：read_file(读文件)"),
        "{note}"
    );
    assert!(
        note.contains("a2a translator：http://127.0.0.1:9999"),
        "{note}"
    );
}
