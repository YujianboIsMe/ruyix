"""DSML 泄露 A/B 探针 —— 四臂对照，判"泄露跟什么有关"。

一次性实验脚本（不是产品代码），但**结论写进了 `doc/v0.x/问题-DSML标记泄露.md`**，所以要能重跑。

用法：
    python scripts/dsml-ab-probe.py
读 `~/.ruyix/code/ai.toml` 里的 api_url / model / api_key（**不回显 key**），直打官方端点。
逐轮原始值写到 `%TEMP%/dsml_ab_raw.json`（含每轮完整 content 与 usage）。

四臂（同一份 messages、temperature=0，只差额外参数）：
    A: response_format=json_object               （现状：引擎一直在发的）
    B: 什么都不加                                  （纯提示词要求 JSON）
    C: tools=[四原语函数]                          （换成工具调用协议）
    D: tools + response_format=json_object         （两者叠加 —— "冲突"就是问这条）

预注册判读规则（跑之前写死，别事后改）：
    A 明显多于 B           -> json_mode 是放大器
    A ≈ B，都漏            -> 与 json_mode 无关，是"没声明 tools"的协议错位
    D 高而 C 低            -> 工具调用声明 + JSON 模式 两者叠加才是病灶
    C 基本走 tool_calls    -> 换协议可行
    四臂都不漏             -> 本 N 下未复现（低发生率，不能当"没问题"）

2026-09-21 实测（N=8）：A 2/8 带标记（都只剩尾巴）、B 1/8（整段信封）、C/D 0/8 且 6/8 走
tool_calls —— 结论见文档第 2 节。
"""
import json, re, os, urllib.request, urllib.error, concurrent.futures as cf, datetime

CFG = os.path.expanduser(r"~\.ruyix\code\ai.toml")
SRC = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                   "crates", "harness-engine", "src", "agent.rs")
OUT = os.path.join(os.environ.get("TEMP", "."), "dsml_ab_raw.json")
N = 8
CONC = 4

# ---- 1. 配置（只取需要字段，不回显 key）----
cfg = {}
for line in open(CFG, encoding="utf-8"):
    if "=" in line and not line.strip().startswith("#"):
        k, _, v = line.partition("=")
        cfg[k.strip()] = v.strip().strip('"')
KEY = cfg.get("api_key") or ""
BASE = (cfg.get("api_url") or "https://api.deepseek.com/v1").rstrip("/")
MODEL = cfg.get("model") or "deepseek-chat"
assert KEY, "ai.toml 里没读到 api_key"
print(f"端点: {BASE}  模型: {MODEL}  key: 已读到（{len(KEY)} 字符，不回显）")

# ---- 2. 引擎真实系统提示词（从源码里原样取出，别手抄）----
src = open(SRC, encoding="utf-8").read()
m = re.search(r'pub const AGENT_SYSTEM: &str = r#"(.*?)"#;', src, re.S)
assert m, "没取到 AGENT_SYSTEM"
SYSTEM = m.group(1)
print(f"系统提示词: {len(SYSTEM)} 字符")

# ---- 3. 上下文：仿那几次真跑（已读 package.json、发现 node_modules 不在）----
HIST = [
    {"role": "user", "content": "帮我启动 cloud-shop-admin 后端（8083）和 cloud-shop-admin-web 前端（5173）并验证能访问，项目根 D:\\Projects\\Java\\cloud-shop。"},
    {"role": "assistant", "content": '{"actions":[{"tool":"read","args":{"path":"cloud-shop-admin-web/package.json"}}]}'},
    {"role": "user", "content": '{"ok": true, "result": {"path":"cloud-shop-admin-web/package.json","content":"{}"}}'},
    {"role": "assistant", "content": '{"actions":[{"tool":"execute","args":{"cmd":"dir /b cloud-shop-admin-web\\\\node_modules"}}]}'},
    {"role": "user", "content": '{"ok": false, "error": "命令退出码 1：找不到文件"}'},
]
MESSAGES = [{"role": "system", "content": SYSTEM}] + HIST

TOOLS = [
    {"type": "function", "function": {"name": "read", "description": "读项目内文件", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}}},
    {"type": "function", "function": {"name": "write", "description": "写项目内文件（整份内容或锚点补丁）", "parameters": {"type": "object", "properties": {"path": {"type": "string"}, "content": {"type": "string"}, "edits": {"type": "array", "items": {"type": "object"}}}, "required": ["path"]}}},
    {"type": "function", "function": {"name": "execute", "description": "在项目根执行命令", "parameters": {"type": "object", "properties": {"cmd": {"type": "string"}, "timeout_secs": {"type": "integer"}}, "required": ["cmd"]}}},
    {"type": "function", "function": {"name": "connect", "description": "连外部能力（MCP/远端 agent）", "parameters": {"type": "object", "properties": {"action": {"type": "string"}, "server": {"type": "string"}, "tool": {"type": "string"}}, "required": ["action"]}}},
]

ARMS = {
    "A_json_object": {"response_format": {"type": "json_object"}},
    "B_plain": {},
    "C_tools": {"tools": TOOLS},
    "D_tools_json_object": {"tools": TOOLS, "response_format": {"type": "json_object"}},
}

RE_MARK = re.compile("</?[|\uff5c]{2}\\s*DSML[|\uff5c]{2}[^>]{0,80}>", re.I)
RE_TAG2 = re.compile("</?[|\uff5c]{2}[^>\"\n]{0,32}[|\uff5c]{2}[^>\"\n]{0,32}>")


def call(arm, extra, i):
    body = {"model": MODEL, "messages": MESSAGES, "temperature": 0, "max_tokens": 1024, "stream": False}
    body.update(extra)
    req = urllib.request.Request(
        BASE + "/chat/completions",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {KEY}"},
    )
    try:
        with urllib.request.urlopen(req, timeout=300) as r:
            obj = json.loads(r.read().decode())
    except urllib.error.HTTPError as e:
        return {"arm": arm, "round": i, "http_error": e.code, "detail": e.read().decode()[:300]}
    except Exception as e:
        return {"arm": arm, "round": i, "http_error": "exc", "detail": str(e)[:300]}

    msg = obj["choices"][0]["message"]
    content = msg.get("content") or ""
    tc = msg.get("tool_calls") or []
    marks = RE_MARK.findall(content) + [x for x in RE_TAG2.findall(content) if x not in RE_MARK.findall(content)]
    first_brace = min([p for p in [content.find("{"), content.find("[")] if p >= 0], default=-1)
    head = content[:first_brace] if first_brace > 0 else ""
    parsed, bare_args, valid = None, False, False
    try:
        seg = content[content.find("{"): content.rfind("}") + 1] if "{" in content else ""
        parsed = json.loads(seg)
        if isinstance(parsed, dict):
            valid = any(k in parsed for k in ("tool", "actions", "calls", "final", "plan"))
            bare_args = not valid
        elif isinstance(parsed, list):
            valid = all(isinstance(x, dict) and "tool" in x for x in parsed)
            bare_args = not valid
    except Exception:
        pass
    return {
        "arm": arm, "round": i,
        "finish_reason": obj["choices"][0].get("finish_reason"),
        "content_len": len(content),
        "markup_count": len(marks),
        "head_tags": bool(RE_MARK.search(head) or RE_TAG2.search(head)),
        "tail_only": len(marks) > 0 and not (RE_MARK.search(head) or RE_TAG2.search(head)),
        "json_parsed": parsed is not None,
        "valid_action": valid,
        "bare_args": bare_args,
        "tool_calls": len(tc),
        "tool_names": [t.get("function", {}).get("name") for t in tc],
        "usage": obj.get("usage"),
        "content": content,
    }


jobs = [(a, e, i) for a, e in ARMS.items() for i in range(N)]
rows = []
with cf.ThreadPoolExecutor(max_workers=CONC) as ex:
    for r in ex.map(lambda t: call(*t), jobs):
        rows.append(r)

json.dump({"when": datetime.datetime.now().isoformat(), "model": MODEL, "n": N, "rows": rows},
          open(OUT, "w", encoding="utf-8"), ensure_ascii=False, indent=1)
print(f"\n逐轮原始值已写盘: {OUT}\n")

hdr = f"{'臂':22} {'轮':>3} {'fr':>10} {'len':>5} {'标记':>4} {'只尾':>4} {'JSON':>5} {'合法动作':>8} {'裸参数':>6} {'tool_calls':>10}"
print(hdr); print("-" * len(hdr))
for r in sorted(rows, key=lambda x: (x["arm"], x["round"])):
    if "http_error" in r:
        print(f"{r['arm']:22} {r['round']:>3}  HTTP {r['http_error']}: {r.get('detail','')[:60]}")
        continue
    print(f"{r['arm']:22} {r['round']:>3} {str(r['finish_reason']):>10} {r['content_len']:>5} "
          f"{r['markup_count']:>4} {str(r['tail_only']):>5} {str(r['json_parsed']):>5} "
          f"{str(r['valid_action']):>8} {str(r['bare_args']):>6} {r['tool_calls']:>10}")

print(f"\n===== 汇总（每臂 N={N}）=====")
for a in ARMS:
    rs = [r for r in rows if r["arm"] == a and "http_error" not in r]
    if not rs:
        print(f"{a:22} 全部请求失败")
        continue
    f = lambda k: sum(1 for r in rs if r[k] is True)
    tok = sum((r["usage"] or {}).get("total_tokens", 0) for r in rs)
    marked = sum(1 for r in rs if r["markup_count"] > 0)
    called = sum(1 for r in rs if r["tool_calls"] > 0)
    print(f"{a:22} n={len(rs)} 带标记={marked} 只尾标记={f('tail_only')} 合法动作={f('valid_action')} "
          f"裸参数={f('bare_args')} tool_calls={called} tokens={tok}")
