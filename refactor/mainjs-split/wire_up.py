#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""接线（拆分收尾的第一步）：在 ui/index.html 里按 CRLF 插入 7 个模块的 <script>。

**只做接线这一步**：7 个模块文件本身是 `git mv refactor/mainjs-split/*.js ui/scripts/*.js`
（历史里是 R 记录），脚本不再负责拷贝 —— 拷贝那半只在"还没落位"的那一天有意义，
留着只会让脚本在模块已经就位之后报"源文件找不到"。

坑（交接单里踩过）：index.html 是 **CRLF**，用 LF 锚点会一条也匹配不上 —— 脚本会
"成功地什么都没改"。所以整份文件按 `\\r\\n` 切分后按行插入，插在含
`scripts/external.js` 的那一行之后：`main.js` 必须仍**最后**加载（经典脚本共享一个全局
作用域，`initApp` 裸名调用各模块的函数）。
"""
import io
import os
import sys

# 脚本住在 refactor/mainjs-split/ ⇒ 仓库根要上溯三层
ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
MODS = ["titlebar", "menus", "contextmenu", "commandbar", "navigator", "editor", "terminal"]
CRLF = chr(13) + chr(10)


def main():
    p = os.path.join(ROOT, "ui", "index.html")
    raw = io.open(p, encoding="utf-8", newline="").read()
    if "scripts/titlebar.js" in raw:
        print("index.html 已接线（跳过）")
        return 0
    lines = raw.split(CRLF)
    hit = [k for k, l in enumerate(lines) if "scripts/external.js" in l]
    if len(hit) != 1:
        print("!! external.js 那一行没找到或多于一行：%r" % hit)
        return 1
    i = hit[0]
    lines[i + 1:i + 1] = ['  <script src="scripts/%s.js"></script>' % n for n in MODS]
    io.open(p, "w", encoding="utf-8", newline="").write(CRLF.join(lines))
    print("index.html 在 external.js 之后插入 %d 行（main.js 仍最后加载）" % len(MODS))
    return 0


if __name__ == "__main__":
    sys.exit(main())
