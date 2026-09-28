#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""agent/ledger.rs 1 拆 3：调用键 → agent/keys.rs；内联黄金测试 → agent/ledger/tests.rs。

`ledger.rs` 拆前 1613 行，其实是三层 + 一坨测试：

| 段 | 行 | 去处 |
|---|---|---|
| 「调用键：规范化 + 包含关系」（`Tool`/`SpanKey`/`LedgerCall` + 归一 + **纯度白名单**） | 32-425 | → `agent/keys.rs` |
| 「版本向量」+「账本」（`Ver`/`VersionVec`/`Body`/`ContextLedger`） | 426-986 | 留在 `ledger.rs` |
| 「预检」（`Plan`/`Reuse`/`repo_version`） | 987-1070 | 留在 `ledger.rs` |
| 内联 `#[cfg(test)] mod tests`（黄金测试：143/151 事件逐条决策对齐） | 1071-1613 | → `agent/ledger/tests.rs`（`#[cfg(test)] mod tests;`） |

两点纪律：

1. **测试搬去独立文件**（`doc/编码规范.md` 那条规矩：要么放文件末尾、要么单独成文件）。
   内容是**逐字搬**、不手工 dedent —— 内联模块里的原始字符串字面量（黄金测试的 JSON 片段）
   带 4 空格缩进是**数据**，手工 dedent 会把数据改掉；缩进交给 `cargo fmt`（它重排代码、
   不动字符串内容）。
2. 可见性沿用上一刀的规则：块内私有项 → `pub(crate)`；`keys.rs` 还需要 `use std::path::Path;`
   （`use super::*` 拿到的是 `agent` 的项，拿不到 `ledger.rs` 的 `use`）。
"""
import io
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
AGENTDIR = os.path.join(ROOT, "crates", "harness-engine", "src", "agent")
SRC = os.path.join(AGENTDIR, "ledger.rs")
KEYS = os.path.join(AGENTDIR, "keys.rs")
TDIR = os.path.join(AGENTDIR, "ledger")
TTEST = os.path.join(TDIR, "tests.rs")
AGENT = os.path.join(ROOT, "crates", "harness-engine", "src", "agent.rs")

KEYS_DOC = """//! 调用键：**规范化 + 纯度判定 + 包含关系**（v1.2 P2 的地基）。
//!
//! 去重的第一步是回答"**这是不是同一次调用**"，本模块就干这个：
//!
//! - **规范化**（[`norm_path`] / [`norm_cmd_and_resources`]）：路径折叠 `./`、统一分隔符、
//!   Windows 折大小写；命令串折叠空白、去 `2>nul`、首 token 折小写 —— **命令里的路径一起归一**
//!   （少了这条，`git diff -- ./a` 与 `git diff -- a` 会算成两个键；黄金测试抓出来的）；
//! - **纯度白名单**（[`cmd_is_pure`] + `PURE_CMDS` / `GIT_PURE` / `KNOWN_EFFECTFUL`）：
//!   只有"读类"工具才参与去重 —— 写向文件的重定向、`&&` / `||` / `;` 串联、后台与句柄、
//!   `connect`、控制动作一律判纯失败。**宁可多执行一次，也不许"看起来一样就跳过"**；
//! - **包含关系**（[`SpanKey`]）：整文件读 ⊇ 该文件的任何区间读（`hi = -1` 表"到末尾"）；
//!   跨路径作用域的包含**刻意不做**（上游也没有，而黄金测试要求决策逐条对齐）。
//!
//! 从 `agent/ledger.rs` 拆出的（2026-09-28，那个文件当时 1613 行）。
"""


def rd(p):
    return io.open(p, encoding="utf-8", newline="").read()


def wr(p, t):
    io.open(p, "w", encoding="utf-8", newline="").write(t)


PRIV_RE = re.compile(r"^(async\s+)?(fn|struct|enum|const|static|type|trait)\s+([A-Za-z_]\w*)")
ITEM_RE = re.compile(
    r"^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(fn|struct|enum|const|static|type|trait)\s+([A-Za-z_]\w*)"
)


def items(text):
    return {(m.group(1), m.group(2)) for m in (ITEM_RE.match(l) for l in text.splitlines()) if m}


def bump(body):
    n = 0
    state = None
    for i, l in enumerate(body):
        if PRIV_RE.match(l):
            body[i] = "pub(crate) " + l
            n += 1
        if re.match(r"^(?:pub(?:\([^)]*\))?\s+)?struct\s+\w+", l):
            state = "struct"
        elif re.match(r"^(?:pub(?:\([^)]*\))?\s+)?enum\s+\w+", l):
            state = "enum"
        elif re.match(r"^impl\b", l):
            state = "impl_trait" if " for " in l else "impl"
        elif l.startswith("}"):
            state = None
        elif state == "struct" and re.match(r"^    [a-z_]\w*\s*:", l):
            body[i] = "    pub(crate) " + l.strip()
            n += 1
        elif state == "impl" and re.match(r"^    (?:async )?fn\s+\w+", l):
            body[i] = "    pub(crate) " + l.strip()
            n += 1
    return n


def main():
    t = rd(SRC)
    nl = "\r\n" if "\r\n" in t else "\n"
    lines = t.split(nl)
    if os.path.exists(KEYS):
        print("这活已经做完了（%s 已在）" % os.path.basename(KEYS))
        return 0

    before = items(t)

    # ---- A 区：调用键（banner 三元组 → `// 版本向量` banner 之前）----
    b = [i for i, l in enumerate(lines) if l.strip() == "// 调用键：规范化 + 包含关系"]
    assert len(b) == 1, "!! 调用键 banner 命中 %d" % len(b)
    b = b[0]
    assert re.match(r"^// =+$", lines[b - 1]) and lines[b + 1] == lines[b - 1]
    a_s = b - 1
    v = [i for i, l in enumerate(lines) if l.strip() == "// 版本向量"]
    assert len(v) == 1
    v = v[0]
    a_e = v - 2                      # 版本向量 banner 上一行（空行）之前
    while lines[a_e].strip() == "":
        a_e -= 1
    assert lines[a_e] == "}", "!! A 区结尾不是行首 }：%r" % lines[a_e]

    # ---- B 区：内联测试模块（文件末尾）----
    ct = [i for i, l in enumerate(lines) if l.strip() == "#[cfg(test)]"]
    assert len(ct) == 2, "!! #[cfg(test)] 命中 %d（期望 2：中段那个 + 末尾测试模块）" % len(ct)
    m = ct[-1]
    assert lines[m + 1].strip() == "mod tests {", "!! 末尾不是内联测试模块：%r" % lines[m + 1]
    b_s, b_e = m, len(lines) - 1
    while lines[b_e].strip() == "":      # 文件以换行收尾 ⇒ 末尾有空串
        b_e -= 1
    assert lines[b_e] == "}", "!! 文件末尾不是模块收尾：%r" % lines[b_e]

    # ---- 生成 keys.rs ----
    body = lines[a_s:a_e + 1]
    n1 = bump(body)
    wr(KEYS, (KEYS_DOC + "\nuse super::*;\nuse std::path::Path;\n\n").replace("\n", nl)
       + nl.join(body) + nl)

    # ---- 生成 ledger/tests.rs（正文逐字搬，不 dedent）----
    inner = lines[b_s + 2:b_e]
    os.makedirs(TDIR, exist_ok=True)
    wr(TTEST, ("//! `ContextLedger` 的黄金测试：上游 `experiments/export_decision_trace.py` 产出的\n"
               "//! `ledger_golden_seed{1,3}.json` 两份夹具，共 143 / 151 个事件 —— **决策逐条对齐** +\n"
               "//! 四类计数逐项相等。\n"
               "//!\n"
               "//! 从 `agent/ledger.rs` 末尾搬出来的（2026-09-28）：测试单独成文件（`doc/编码规范.md`），\n"
               "//! 内容是**逐字搬**的 —— 里面的 JSON 片段是数据，手工 dedent 会改数据。\n"
               "\n") + nl.join(inner) + nl)

    # ---- 改 ledger.rs：删 A 区、内联测试正文换成声明 ----
    # 三段拼接：头 + 中段（版本向量/账本/预检）+ 测试模块声明。
    # 第一版写成 `lines[:a_s] + lines[b_s:]` —— 那一刀顺手把**中段整段**也切掉了
    # （ledger.rs 只剩 35 行），是守恒断言当场报出丢了 15 个顶层项才拦住。
    kept = lines[:a_s] + lines[a_e + 1:b_s] + ["#[cfg(test)]", "mod tests;"]
    text = nl.join(kept) + nl
    text = re.sub(r"(?:\r?\n){3,}", nl + nl, text)
    wr(SRC, text)

    # ---- 调用方改路径：搬到 keys.rs 的两类名字（`ledger::X` → `keys::X`）----
    # `LedgerCall` 是 `pub`，但它在**私有模块 keys** 里 ⇒ 旧路径 `agent::ledger::X` 不再解析
    # （E0603 / E0425 抓到）。`classify` 是 `pub(super)`，同理。
    CALLERS = {
        "dispatch.rs": [("ledger::LedgerCall", "keys::LedgerCall", 1)],
        "findings.rs": [("ledger::LedgerCall", "keys::LedgerCall", 5)],
        "tests.rs": [("ledger::LedgerCall", "keys::LedgerCall", 1),
                     ("ledger::classify(", "keys::classify(", 1)],
    }
    for fn, subs in CALLERS.items():
        fp = os.path.join(AGENTDIR, fn)
        s = rd(fp)
        for old, new, n in subs:
            got = s.count(old)
            assert got == n, "!! %s 里 %r 命中 %d（期望 %d）" % (fn, old, got, n)
            s = s.replace(old, new)
        wr(fp, s)

    # findings.rs 用的是**模块名**路径（`use super::ledger;` + `ledger::X`），
    # 所以要把它也拉进来；dispatch.rs / tests.rs 是 `use super::*`（glob 能看到私有子模块）不用改。
    s = rd(os.path.join(AGENTDIR, "findings.rs"))
    assert s.count("use super::ledger;") == 1, "!! findings.rs 的 ledger 导入形状变了"
    s = s.replace("use super::ledger;", "use super::{keys, ledger};", 1)
    wr(os.path.join(AGENTDIR, "findings.rs"), s)

    # 测试文件多了一层目录 ⇒ `include_str!` 的相对路径要 +1 级（否则 E0583 找不到夹具）
    s = rd(TTEST)
    got = s.count('include_str!("../../tests/fixtures/')
    assert got == 2, "!! 夹具路径命中 %d（期望 2）" % got
    s = s.replace('include_str!("../../tests/fixtures/', 'include_str!("../../../tests/fixtures/')
    wr(TTEST, s)

    # ---- agent.rs 登记 keys 子模块 ----
    a = rd(AGENT)
    assert "mod keys;" not in a
    # ⚠️ `mod ledger;` 是 `pub mod ledger;` 的**子串**：先认 pub 形态，否则 replace 会把
    # `pub mod ledger;` 写成 `pub mod keys;` + `mod ledger;` —— 模块一私有，`LedgerEntry`
    # 这类 pub 结构体就不再"对外可达"，dead_code 立刻开始报（第一版真踩：`field norm is
    # never read` + `methods capsule/recalled are never used`）。
    key = "pub mod ledger;" if "pub mod ledger;" in a else "mod ledger;"
    assert a.count(key) == 1, "!! ledger 模块声明形状变了"
    a = a.replace(key, "mod keys;" + nl + "pub(crate) use keys::*;" + nl + key, 1)
    wr(AGENT, a)

    after = items(rd(SRC)) | items(rd(KEYS))
    lost, gained = before - after, after - before
    print("ledger.rs %d → %d 行；keys.rs %d 行；ledger/tests.rs %d 行；提升可见性 %d 项；"
          "顶层项 丢 %d 多 %d"
          % (len(t.splitlines()), len(rd(SRC).splitlines()), len(rd(KEYS).splitlines()),
             len(rd(TTEST).splitlines()), n1, len(lost), len(gained)))
    if lost:
        print("!! 丢掉的顶层项：", sorted(lost))
    if gained:
        print(".. 新增的顶层项：", sorted(gained))
    # 测试正文守恒：块内行数 = 原内联模块正文行数
    print("测试正文 %d 行（原内联模块正文 %d 行）" % (len(inner), b_e - b_s - 1))
    return 1 if (lost or len(inner) != b_e - b_s - 1) else 0


if __name__ == "__main__":
    sys.exit(main())
