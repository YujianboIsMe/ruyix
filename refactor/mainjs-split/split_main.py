#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""把 main.js 里已经搬进 7 个模块的顶层声明整块删掉，产出拆分版 main.js。

做法（确定性，不依赖模糊对齐）：
  1. 用逐字符扫描器（字符串 / 模板串 / 正则 / 注释 / 括号配对）算出**每一行行首的括号深度**；
  2. 行首深度为 0 且匹配顶层声明正则的行 = 一个顶层项的开始；该项的结束行 = 深度回到 0 的那一行；
  3. 归属表来自 7 个模块文件的顶层声明名；
  4. 被搬走的项**连同它前面紧邻的注释块**一起删（注释随代码搬走了）；
  5. 自检四条（见 main()）+ 收尾由 node --check / ui-smoke U56/U57 判。

用法：
  python refactor/split_main.py --dry      # 只报告 + 写预览，不改 ui/scripts/main.js
  python refactor/split_main.py --write
"""
import io
import os
import re
import sys

# 脚本住在 refactor/mainjs-split/ ⇒ 仓库根要上溯三层
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
MAIN = os.path.join(ROOT, "ui", "scripts", "main.js")
MODS = ["titlebar", "menus", "contextmenu", "commandbar", "navigator", "editor", "terminal"]
# 归属表从**模块的正式位置**读：拆分做完之后 refactor/ 里不再留副本，
# 否则这个脚本重跑一次就会说"模块文件找不到"（踩过）。
MODDIR = os.path.join(ROOT, "ui", "scripts")

DECL_RE = re.compile(
    r"^(?:async\s+)?function\s+(\w+)|^(let|const|var)\s+(\w+)|^window\.(\w+)\s*=[^=]"
)
REGEX_START = set("(,=:[!&|?{};+-*%~^<>")


def rd(path):
    """读文件并把 CRLF 归一成 LF（写回时统一用 CRLF）。"""
    return io.open(path, encoding="utf-8", newline="").read().replace("\r\n", "\n")


def regex_allowed(text, i):
    """位置 i 的 `/` 是正则开头还是除号：看上一个有效字符。"""
    p = i - 1
    while p >= 0 and text[p] in " \t":
        p -= 1
    if p < 0:
        return True
    c = text[p]
    if c == "\n":
        return True
    return c in REGEX_START


def depth_at_line_start(text):
    """返回两个等长列表：每行**行首**深度、每行**行尾**深度。只数括号，认字符串/正则/注释。

    换行一律由循环顶部那一条分支记账（跳读会把行号弄丢，第一版就栽在这），
    因此这里用**模式机**而不是“跳过一段”：每个模式每次只前进一个字符。
    """
    n = len(text)
    depth = 0
    stack = []
    mode = "code"  # code | "'" | '"' | '`' | regex | linecomment | blockcomment
    starts = [0]
    ends = []
    i = 0
    while i < n:
        c = text[i]
        if c == "\n":
            ends.append(depth)
            starts.append(depth)
            if mode == "linecomment":
                mode = "code"
            i += 1
            continue
        if mode == "linecomment" or mode == "blockcomment":
            if mode == "blockcomment" and c == "*" and text.startswith("*/", i):
                mode = "code"
                i += 2
                continue
            i += 1
            continue
        if mode == "code":
            if c == "/" and text.startswith("//", i):
                mode = "linecomment"
                i += 2
                continue
            if c == "/" and text.startswith("/*", i):
                mode = "blockcomment"
                i += 2
                continue
            if c in "\"'`":
                mode = c
                i += 1
                continue
            if c == "/" and regex_allowed(text, i):
                mode = "regex"
                i += 1
                continue
            if c in "{([":
                stack.append(c)
                depth += 1
                i += 1
                continue
            if c in ")]":
                if stack and stack[-1] == {"}": "{", ")": "(", "]": "["}[c]:
                    stack.pop()
                    depth -= 1
                i += 1
                continue
            if c == "}":
                if stack and stack[-1] == "${":
                    stack.pop()
                    depth -= 1
                    mode = "`"
                elif stack:
                    stack.pop()
                    depth -= 1
                i += 1
                continue
            i += 1
            continue
        # 字符串 / 模板串 / 正则内部
        if c == "\\" and i + 1 < n and text[i + 1] != "\n":
            i += 2
            continue
        if mode == "`" and c == "$" and i + 1 < n and text[i + 1] == "{":
            stack.append("${")
            depth += 1
            mode = "code"
            i += 2
            continue
        if mode == "regex" and c == "[":
            # 字符类里的 / 不是结尾
            i += 1
            while i < n and text[i] != "]":
                if text[i] == "\\":
                    i += 1
                i += 1
            i += 1
            continue
        if mode == "regex" and c == "/":
            mode = "code"
            i += 1
            continue
        if mode != "regex" and c == mode:
            mode = "code"
            i += 1
            continue
        i += 1
    ends.append(depth)
    if mode not in ("code", "linecomment"):
        raise RuntimeError("扫描结束时仍在字符串/正则/块注释里（mode=%r）" % mode)
    if depth != 0:
        raise RuntimeError("扫描结束括号不平衡：depth=%d stack=%r" % (depth, stack))
    return starts, ends


def top_items(text, lines):
    starts, ends = depth_at_line_start(text)
    assert len(starts) == len(lines), (len(starts), len(lines))
    items = []
    i = 0
    while i < len(lines):
        if starts[i] != 0:
            i += 1
            continue
        m = DECL_RE.match(lines[i])
        if not m:
            i += 1
            continue
        name = m.group(1) or m.group(3) or m.group(4)
        kind = "function" if m.group(1) else ("assign" if m.group(4) else "var")
        # 结束行 = 从这里开始、行尾深度回到 0 的第一行
        j = i
        last = None
        while j < len(lines):
            if ends[j] == 0:
                last = j
                if kind == "function":
                    if lines[j].rstrip().endswith("}"):
                        break
                else:
                    body = re.sub(r"//.*$", "", lines[j]).rstrip()
                    if body.endswith(";") or body.endswith("}"):
                        break
            j += 1
        if last is None:
            raise RuntimeError("找不到 %s 的结尾" % name)
        items.append((i, last, kind, name))
        i = last + 1
    return items, starts, ends


def comment_block_start(lines, i):
    """从项的首行向上吃掉紧邻的空行/注释行，返回注释块起始行（注释随代码一起搬走）。"""
    start = i
    p = i - 1
    while p >= 0:
        s = lines[p].strip()
        if s == "" or s.startswith("//") or s.startswith("/*") or s.startswith("*") or s.endswith("*/"):
            start = p
            p -= 1
            continue
        break
    return start


def module_decls():
    """{名字: 模块名}，7 个模块的顶层声明（含 window.X = ... 这种顶层赋值）。"""
    own = {}
    for m in MODS:
        for ln in rd(os.path.join(MODDIR, m + ".js")).split("\n"):
            mm = DECL_RE.match(ln)
            if mm:
                name = mm.group(1) or mm.group(3) or mm.group(4)
                if not name.endswith("UI"):
                    own.setdefault(name, m)
    return own


def norm(s):
    """归一：模块里把裸 `state` 写成了 `window.state`。"""
    return s.replace("window.state", "state").strip()


def main():
    text = rd(MAIN)
    lines = text.split("\n")
    items, starts, ends = top_items(text, lines)
    ownmod = module_decls()
    modbody = {m: "\n".join(norm(x) for x in
                            rd(os.path.join(MODDIR, m + ".js")).split("\n")) for m in MODS}

    kill = [it for it in items if it[3] in ownmod]
    keepm = [it for it in items if it[3] not in ownmod]
    if not kill:
        print("main.js 里已经没有归模块的顶层声明 —— 这活已经做完了，什么都不动")
        return 0
    print("顶层项 %d（归模块 %d / 留下 %d）" % (len(items), len(kill), len(keepm)))
    print("留下的：%s" % ", ".join(it[3] for it in keepm))
    print("归模块的：%s" % ", ".join(it[3] for it in kill))

    # 自检 1：每个被删项的代码行都能在它的模块里逐行找到
    bad = []
    for (a, b, kind, name) in kill:
        body = modbody[ownmod[name]]
        for k in range(a, b + 1):
            s = norm(lines[k])
            if s == "" or s.startswith("//") or s.startswith("*") or s.startswith("/*") or s.endswith("*/"):
                continue
            if s not in body:
                bad.append((name, k + 1, lines[k].strip()))
    if bad:
        print("!! 自检 1 失败：以下被删行在模块里找不到")
        for name, ln, txt in bad[:40]:
            print("   [%s] line %d: %s" % (name, ln, txt[:90]))
        return 2

    # 自检 2：被删项的结束行看起来像结尾
    for (a, b, kind, name) in kill:
        tail = lines[b].rstrip()
        if not (tail == "}" or tail.endswith("};") or tail.endswith(";")):
            print("!! 自检 2 失败 [%s] line %d: %r" % (name, b + 1, tail[:80]))
            return 2

    # 合并成删除区间（相邻的项并成一块，并把紧邻的注释/空行带上）
    spans = []
    for (a, b, kind, name) in kill:
        s = comment_block_start(lines, a)
        if spans and s <= spans[-1][1] + 1:
            spans[-1][1] = max(spans[-1][1], b)
        else:
            spans.append([s, b])

    # 合块时把开头多吃的空行吐回去（块与块之间的空行交给后面折叠空行的逻辑）
    for sp in spans:
        while sp[0] < sp[1] and lines[sp[0]].strip() == "":
            sp[0] += 1

    # 自检 3：删除区间不许碰到任何保留项的行
    holds = set()
    for (a, b, kind, name) in keepm:
        holds.update(range(a, b + 1))
    for s, e in spans:
        clash = sorted(holds.intersection(range(s, e + 1)))
        if clash:
            print("!! 自检 3 失败：删除区间吃到了保留声明 (line %d)" % (clash[0] + 1))
            return 2

    remove = set()
    for s, e in spans:
        remove.update(range(s, e + 1))
    new, prev_blank = [], False
    for i, l in enumerate(lines):
        if i in remove:
            continue
        blank = l.strip() == ""
        if blank and prev_blank:
            continue
        new.append(l)
        prev_blank = blank

    print("删除 %d 行 / %d 块 ⇒ main.js %d → %d 行" % (len(remove), len(spans), len(lines), len(new)))
    for s, e in spans:
        print("   块 %5d-%-5d (%4d 行) 起于 %s" % (s + 1, e + 1, e - s + 1, lines[s].strip()[:56]))

    # 自检 4：新文件里不许再出现被搬走的顶层名
    txt = "\n".join(new)
    left = []
    for nm in ownmod:
        pats = (r"(?m)^(?:async\s+)?function\s+%s\b", r"(?m)^(?:let|const|var)\s+%s\b",
                r"(?m)^window\.%s\s*=[^=]")
        if any(re.search(p % re.escape(nm), txt) for p in pats):
            left.append(nm)
    if left:
        print("!! 自检 4 失败：新文件里仍有被搬走的声明：%s" % left)
        return 2

    # 自检 5：新文件的顶层项 == 保留项（逐名比对，顺序也一致）
    newitems, _, _ = top_items(txt, new)
    if [it[3] for it in newitems] != [it[3] for it in keepm]:
        print("!! 自检 5 失败：新文件顶层项与预期不符")
        print("   实际：%s" % [it[3] for it in newitems])
        print("   预期：%s" % [it[3] for it in keepm])
        return 2

    out = "\r\n".join(new)
    if "--write" in sys.argv:
        io.open(MAIN, "w", encoding="utf-8", newline="").write(out)
        print("已写入 %s（CRLF，%d 行）" % (MAIN, len(new)))
    else:
        prev = os.path.join(ROOT, "refactor", "_main_split_preview.js")
        io.open(prev, "w", encoding="utf-8", newline="").write(out)
        print("dry-run：预览写到 %s" % prev)
    return 0


if __name__ == "__main__":
    sys.exit(main())
