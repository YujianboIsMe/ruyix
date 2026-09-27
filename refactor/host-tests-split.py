#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""main.rs 1 拆 2：把测试代码搬进 src-tauri/src/tests.rs。

- main.rs 只留一个接线点：末尾 `#[cfg(test)] mod tests;`
- 顺带把 `utf16_offset()`（`#[cfg(test)]` 门禁的测试专供参照实现）一并搬过去
  —— 它本来就只为测试编译，留在生产段里是"生产文件里的测试代码"。
- 缩进不在这里处理：正文先按原缩进（差 4 格）落盘，交给 `cargo fmt` 收 —— rustfmt 认识
  多行字符串字面量，手工 dedent 会把字面量内容也削掉（那是数据，静默改掉最恶）。

锚点只认文本 + 命中数断言 + CRLF 感知（见 skill: scripted-source-edits）。
"""
import io
import os
import sys

ROOT = os.path.dirname(os.path.abspath(__file__))
for _ in range(2):  # 脚本住在 refactor/ 下（上一轮的位置）或哪里都行，按标记找仓库根
    if os.path.isdir(os.path.join(ROOT, "src-tauri")):
        break
    ROOT = os.path.dirname(ROOT)
MAIN = os.path.join(ROOT, "src-tauri", "src", "main.rs")
TESTS = os.path.join(ROOT, "src-tauri", "src", "tests.rs")

HEADER = """//! ruyix 宿主（bin crate）的单元测试 —— 从 `main.rs` 拆出来的（3665 行 → 2693 + 972）。
//!
//! **为什么单开一个文件**：生产代码里夹着近千行测试，读代码时一直在中间穿行；
//! 拆开之后 `main.rs` 只剩"能跑起来的那部分"。接线只有一处 —— `main.rs` 末尾的
//! `#[cfg(test)] mod tests;`。
//!
//! **可见性没变**：`use super::*;` 现在指 crate 根（本文件就是 crate 根下的 `tests` 模块），
//! 所以测试照样能摸到 `main.rs` 里的私有项 —— 这也是当初没选 `tests/` 集成测试目录的原因
//! （那一层看不到私有项，得先把接口开出来）。
//!
//! **`include_str!` 的相对基准没变**：它相对**本文件所在目录**（`src/`）解析，与 `main.rs`
//! 同目录 ⇒ 下面那些 `"../../plugins/highlight/…"` 指向的还是仓库根的 `plugins/`。
//!
//! 只搬了一个"生产段里的测试代码"：`utf16_offset()`（逐字符数一遍的朴素换算）本来就是
//! `#[cfg(test)]` 门禁的测试专供参照，顺带去掉它那行属性 —— 整个文件都只在测试构建里编译。
"""

UTF16_OLD_DOC = """/// 不经查表、逐字符数一遍的等价实现。
///
/// 生产路径**不能**用它：它在每个片段上重走一遍整行，单行超长的压缩文件会退化成
/// O(片段数 × 行长)。保留它是给 `slow_reference`（本来就是刻意写慢的参照）和测试用的
/// —— 两份实现互相独立，单位一旦搞错，等价性测试就会红。所以只在测试构建里编译。
"""

UTF16_NEW_DOC = """/// 不经查表、逐字符数一遍的等价实现（`slow_reference` 的偏移换算用它）。
///
/// 生产路径**不能**用它：它在每个片段上重走一遍整行，单行超长的压缩文件会退化成
/// O(片段数 × 行长)。两份实现互相独立，单位一旦搞错，等价性测试就会红。
///
/// 它原先住在 `main.rs` 的生产段、靠 `#[cfg(test)]` 门禁；测试搬进本文件后，门禁由
/// **整个文件**承担，那行属性就去掉了。`#[cfg(feature = "preinstalled")]` 留着 ——
/// 纯净模式（`--no-default-features`）里 arborium 根本没编进来，而用它的 `slow_reference`
/// 被同一条件门禁着（没有这行就是 unresolved import / 死代码）。
"""


def rd(p):
    return io.open(p, encoding="utf-8", newline="").read().replace("\r\n", "\n")


def wr(p, text):
    io.open(p, "w", encoding="utf-8", newline="").write(text.replace("\n", "\r\n"))


def main():
    if os.path.exists(TESTS):
        print("!! %s 已存在 —— 这活已经做过了（要么先删它再跑）" % TESTS)
        return 1
    t = rd(MAIN)
    lines = t.split("\n")

    # ---- 定位：测试模块
    start = None
    for i, l in enumerate(lines):
        if l == "#[cfg(test)]" and i + 1 < len(lines) and lines[i + 1] == "mod tests {":
            start = i
            break
    if start is None:
        print("!! 找不到 `#[cfg(test)]\\nmod tests {`")
        return 1
    close = None
    for i in range(len(lines) - 1, start, -1):
        if lines[i] == "}":
            close = i
            break
    if close is None:
        print("!! 找不到测试模块的收尾 `}`")
        return 1
    if lines[close + 1:].count("") != len(lines) - close - 1:
        print("!! 测试模块的 `}` 之后还有内容（不是文件末尾）：%r" % lines[close + 1:close + 3])
        return 1
    print("测试模块：第 %d 行 `mod tests {` … 第 %d 行 `}`（共 %d 行）"
          % (start + 1, close + 1, close - start + 1))

    # 模块体：去掉两行 use 与前导空行后的正文（末尾那个 `}` 也要去掉）
    body = lines[start + 2:close]          # `mod tests {` 之后一行是 `use super::*;`
    while body and body[0].strip() == "":
        body.pop(0)
    # 去掉 `use super::*;` 与 `#[cfg(feature = "preinstalled")] use arborium::Highlighter;`
    if body[0] != "    use super::*;":
        print("!! 测试模块第一行不是 `use super::*;`：%r" % body[0])
        return 1
    body = body[1:]
    while body and body[0].strip() == "":
        body.pop(0)
    if body[0].strip() != '#[cfg(feature = "preinstalled")]' or "arborium::Highlighter" not in body[1]:
        print("!! `use arborium::Highlighter` 的形状变了：%r / %r" % (body[0], body[1]))
        return 1
    body = body[2:]
    while body and body[0].strip() == "":
        body.pop(0)
    while body and body[-1].strip() == "":
        body.pop()
    # 这句话是"文件末的测试模块"那一版的指针，搬过来之后就成了自指
    old_self_ref = "// （用它们的用例也被同一条件门禁，见文件末的测试模块）"
    if old_self_ref in body:
        body[body.index(old_self_ref)] = "    // （用它们的用例也被同一条件门禁）"
    print("测试正文 %d 行" % len(body))

    # ---- 定位：utf16_offset（`#[cfg(test)]` + `#[cfg(feature = "preinstalled")]` + fn）
    u_start = None
    for i, l in enumerate(lines[:start]):
        if l == "fn utf16_offset(line: &str, byte_off: usize) -> usize {":
            u_start = i
            break
    if u_start is None:
        print("!! 找不到 utf16_offset 的定义")
        return 1
    if (lines[u_start - 1] != '#[cfg(feature = "preinstalled")]'
            or not lines[u_start - 2].startswith("// 只被已门禁的用例用到")
            or lines[u_start - 3] != "#[cfg(test)]"):
        print("!! utf16_offset 的属性形状变了：%r / %r / %r"
              % (lines[u_start - 3], lines[u_start - 2], lines[u_start - 1]))
        return 1
    d = u_start - 4                                        # 属性上面那行是 doc 的最后一行
    while d >= 0 and lines[d].startswith("///"):
        d -= 1
    doc_start = d + 1
    if "\n".join(lines[doc_start:u_start - 3]) + "\n" != UTF16_OLD_DOC:
        print("!! utf16_offset 的文档注释与预期不一致，先看再说：\n%s"
              % "\n".join(lines[doc_start:u_start - 3]))
        return 1
    u_end = None
    for i in range(u_start, start):
        if lines[i] == "}":
            u_end = i
            break
    if u_end is None:
        print("!! 找不到 utf16_offset 的收尾 `}`")
        return 1
    print("utf16_offset：第 %d-%d 行（doc %d 行 + 注释 1 行 + 属性 1 行 + 函数体）"
          % (doc_start + 1, u_end + 1, u_start - 3 - doc_start))

    utf16_block = (UTF16_NEW_DOC.rstrip("\n").split("\n")
                   + lines[u_start - 1:u_start]
                   + lines[u_start:u_end + 1])

    # ---- 写 tests.rs
    tests_src = (HEADER.rstrip("\n").split("\n")
                 + ["", "use super::*;",
                    '#[cfg(feature = "preinstalled")]', "use arborium::Highlighter;", ""]
                 + utf16_block
                 + [""] + body + [""])
    wr(TESTS, "\n".join(tests_src))
    print("写出 %s（%d 行，缩进交给 cargo fmt）" % (TESTS, len(tests_src)))

    # ---- 改 main.rs
    # ① 去掉 utf16_offset（前后各留一个空行）
    del lines[doc_start:u_end + 1]
    delta = u_end - doc_start + 1
    start -= delta
    close -= delta

    # ② 测试段 → 接线
    lines[start:close + 1] = ["#[cfg(test)]", "mod tests;"]
    out = "\n".join(lines)

    # ③ 两处文档指针
    old = "/// 片段语义（切点、排序、去重）与旧实现相同（`slow_reference` + 等价性测试钉住），"
    new = "/// 片段语义（切点、排序、去重）与旧实现相同（`tests.rs` 里的 `slow_reference` 钉住），"
    if out.count(old) != 1:
        print("!! slow_reference 的文档指针没命中（%d 次）" % out.count(old))
        return 1
    out = out.replace(old, new)

    wr(MAIN, out)
    print("改写 %s（%d 行）" % (MAIN, len(out.split("\n"))))

    # ---- 自检：内容守恒（测试条数一致、测试属性不在 main.rs 里了）
    n_before = t.count("#[test]")
    n_main = out.count("#[test]")
    n_tests = "\n".join(tests_src).count("#[test]")
    print("自检：改动前 main.rs 有 %d 个 #[test] / 改动后 main.rs %d 个 / tests.rs %d 个"
          % (n_before, n_main, n_tests))
    if n_main != 0 or n_tests != n_before:
        print("!! 内容守恒自检失败")
        return 1
    if "mod tests {" in "\n".join(tests_src):
        print("!! tests.rs 里还留着 `mod tests {` 外壳（那会变成 tests::tests）")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
