#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""量一下 U52 那条门禁**真的**扫到了多少：把 `#[cfg(test)]` 门禁的**项**（按花括号配对）挖掉，
产出一份"去测试"的源码副本（保留行号），再交给同一套扫描逻辑。

用途只是**测量**（ui-smoke 里那条的实现是"砍掉第一个 #[cfg(test)] 之后的全部"，会连带把
生产代码一起砍掉 —— 这活正是被它砍掉的那部分）。
"""
import io
import os
import sys

SRC = sys.argv[1]          # 仓库根
OUT = sys.argv[2]          # 输出目录


def skip_item(text, start):
    """从 `#[cfg(test)]` 处（start）出发，返回它门禁的那个**项**的结束位置（不含）。"""
    n = len(text)
    i = start
    depth = 0            # () [] {} 合计深度
    seen_open = False    # 见过第一个 `{`（项体开始）
    in_code = True
    # 先跳到属性结束：`#[cfg(test)]` 自身
    j = text.find("]", i)
    if j < 0:
        return n
    i = j + 1
    while i < n:
        c = text[i]
        if c == "\n" or c in " \t\r":
            i += 1
            continue
        # 属性 / 注释（项前面可能还有别的属性与 doc 注释）
        if c == "#" and text.startswith("#[", i):
            j = text.find("]", i)
            if j < 0:
                return n
            i = j + 1
            continue
        if text.startswith("//", i):
            j = text.find("\n", i)
            i = n if j < 0 else j
            continue
        if text.startswith("/*", i):
            depth_c = 1
            i += 2
            while i < n and depth_c:
                if text.startswith("/*", i):
                    depth_c += 1
                    i += 2
                elif text.startswith("*/", i):
                    depth_c -= 1
                    i += 2
                else:
                    i += 1
            continue
        break
    # 到这里 i 指向项的正文；扫到它的结尾
    while i < n:
        c = text[i]
        if text.startswith("//", i):
            j = text.find("\n", i)
            i = n if j < 0 else j
            continue
        if text.startswith("/*", i):
            depth_c = 1
            i += 2
            while i < n and depth_c:
                if text.startswith("/*", i):
                    depth_c += 1
                    i += 2
                elif text.startswith("*/", i):
                    depth_c -= 1
                    i += 2
                else:
                    i += 1
            continue
        if text.startswith("r#", i) or (c == "r" and i + 1 < n and text[i + 1] == '"'):
            hashes = 0
            k = i + 1
            while k < n and text[k] == "#":
                hashes += 1
                k += 1
            if k < n and text[k] == '"':
                closer = '"' + "#" * hashes
                j = text.find(closer, k + 1)
                i = n if j < 0 else j + len(closer)
                continue
        if c == '"':
            i += 1
            while i < n:
                if text[i] == "\\":
                    i += 2
                    continue
                if text[i] == '"':
                    i += 1
                    break
                i += 1
            continue
        if c == "'":
            # 字符字面量 vs 生命周期
            if i + 1 < n and text[i + 1] == "\\":
                j = text.find("'", i + 2)
                i = n if j < 0 else j + 1
                continue
            if i + 2 < n and text[i + 2] == "'":
                i += 3
                continue
            i += 2
            continue
        if c in "([{":
            depth += 1
            if c == "{":
                seen_open = True
            i += 1
            continue
        if c in ")]}":
            depth -= 1
            i += 1
            if seen_open and depth == 0:
                return i
            continue
        if c == ";" and depth == 0:
            return i + 1
        i += 1
    return n


def clean(text):
    out = list(text)
    hits = 0
    start = 0
    while True:
        k = text.find("#[cfg(test)]", start)
        if k < 0:
            break
        end = skip_item(text, k)
        for p in range(k, end):
            if out[p] != "\n":
                out[p] = " "
        hits += 1
        start = end
    return "".join(out), hits


total_hits = 0
scanned = 0
for base in ["src-tauri/src", "crates/harness-engine/src"]:
    for dp, dn, fn in os.walk(os.path.join(SRC, *base.split("/"))):
        for f in fn:
            if not f.endswith(".rs") or f.endswith("tests.rs"):
                continue
            p = os.path.join(dp, f)
            t = io.open(p, encoding="utf-8", newline="").read().replace("\r\n", "\n")
            c, hits = clean(t)
            total_hits += hits
            scanned += 1
            rel = os.path.relpath(p, SRC)
            dst = os.path.join(OUT, rel)
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            io.open(dst, "w", encoding="utf-8", newline="").write(c)

print("扫了 %d 个文件，挖掉 %d 个 #[cfg(test)] 项 → 产出到 %s" % (scanned, total_hits, OUT))
