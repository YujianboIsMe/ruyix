#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""main.rs 1 拆 N —— 第三刀：项目记忆（v1.1）→ src-tauri/src/mem_cmds.rs。

搬走：mem_scope / mem_root + 9 个 `mem_*` tauri 命令 + 那段"项目记忆（v1.1）"说明。
留在 main.rs：invoke_handler 的 9 条注册（改成 `mem_cmds::mem_*`）。

自检：锚点命中数 / 顶层项名字集合守恒 / 命令条数 == 9；改完跑 `cargo check -p ruyix --bins`。
"""
import io
import os
import re

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
MAIN = os.path.join(ROOT, "src-tauri", "src", "main.rs")
MEM = os.path.join(ROOT, "src-tauri", "src", "mem_cmds.rs")

CMDS = ["mem_status", "mem_model_status", "mem_model_fetch", "mem_beliefs", "mem_why",
        "mem_as_of", "mem_receipts", "mem_rebuild", "mem_record"]

HEADER = """\
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

"""


def rd(p):
    with io.open(p, encoding="utf-8", newline="") as f:
        return f.read()


def wr(p, t):
    with io.open(p, "w", encoding="utf-8", newline="") as f:
        f.write(t)


def sub(t, old, new, n=1, tag=""):
    nl = "\r\n" if "\r\n" in t else "\n"
    o, w = old.replace("\n", nl), new.replace("\n", nl)
    c = t.count(o)
    if c != n:
        raise SystemExit("!! %s 命中 %d 次（期望 %d）\n--- 锚点 ---\n%s" % (tag, c, n, old[:200]))
    return t.replace(o, w), c


def items(src):
    out = set()
    for m in re.finditer(r"(?m)^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?"
                         r"(fn|enum|struct|const|static|type|trait|union|mod)\s+([A-Za-z_]\w*)", src):
        if m.group(1) != "mod":
            out.add(m.group(1) + " " + m.group(2))
    return out


def main():
    main_t = rd(MAIN)
    nl = "\r\n" if "\r\n" in main_t else "\n"
    if "mod mem_cmds;" in main_t:
        print("mem_cmds.rs 已经拆过了 —— 这活已经做完")
        return 0
    if os.path.exists(MEM):
        raise SystemExit("!! mem_cmds.rs 已存在但 main.rs 没接线，先弄清状态")

    before = items(main_t)
    lines = main_t.split(nl)

    starts = [i for i, l in enumerate(lines)
              if l.startswith("// ---") and "项目记忆（v1.1）" in l]
    ends = [i for i, l in enumerate(lines)
            if l.strip() == 'Ok(serde_json::json!({ "replayed_events": n }))']
    if len(starts) != 1 or len(ends) != 1:
        raise SystemExit("!! 段落边界锚点命中 %d / %d 次（各应为 1）" % (len(starts), len(ends)))
    s, e = starts[0], ends[0]
    if lines[e + 1] != "}" or lines[e + 2].strip() != "":
        raise SystemExit("!! 段尾形状变了：%r" % (lines[e:e + 3],))
    e = e + 1                       # 含 mem_rebuild 的收尾 `}`

    block = lines[s:e + 1]
    n_cmd = sum(1 for l in block if re.match(r"^(?:async )?fn mem_(?!scope|root)\w*", l))
    if n_cmd != len(CMDS):
        raise SystemExit("!! 块里命令 %d 条（期望 %d）" % (n_cmd, len(CMDS)))
    for k in ("fn mem_scope(", "fn mem_root("):
        if sum(1 for l in block if l.startswith(k)) != 1:
            raise SystemExit("!! 块里 %r 不是恰好 1 次" % k)

    # 1) mem_cmds.rs：加可见性（只给命令加 pub，内部 helper 保持私有）+ 需要 Emitter
    body = []
    n_pub = 0
    for l in block:
        if re.match(r"^(?:async )?fn mem_(?!scope|root)\w*", l):
            body.append("pub " + l)
            n_pub += 1
        else:
            body.append(l)
    if n_pub != len(CMDS):
        raise SystemExit("!! 只给 %d 条命令加了 pub" % n_pub)
    wr(MEM, HEADER.replace("\n", nl) + "use tauri::Emitter;\n\n" + nl.join(body) + nl)

    # 2) main.rs：删块 + 注册加前缀 + mod 声明
    rest = lines[:s] + lines[e + 1:]
    while rest[s - 1].strip() == "" and rest[s].strip() == "":
        del rest[s]
    out = nl.join(rest)
    out, _ = sub(out, "mod mcp;\nmod nav;\n", "mod mcp;\nmod mem_cmds;\nmod nav;\n", 1, "mod 声明")
    reg_old = "".join("            %s,\n" % c for c in CMDS)
    reg_new = "".join("            mem_cmds::%s,\n" % c for c in CMDS)
    out, _ = sub(out, reg_old, reg_new, 1, "invoke_handler 注册")
    wr(MAIN, out)

    after = items(rd(MAIN)) | items(rd(MEM))
    if after != before:
        raise SystemExit("!! 顶层项集合变了：丢 %s / 多 %s"
                         % (sorted(before - after), sorted(after - before)))
    print("main.rs %d → %d 行；mem_cmds.rs %d 行；命令 %d 条；顶层项 %d 个（守恒 ✓）"
          % (len(lines), len(rd(MAIN).split(nl)), len(rd(MEM).split(nl)), n_pub, len(before)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
