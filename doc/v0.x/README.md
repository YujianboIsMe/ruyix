# doc/v0.x —— 0.x 时代存档

从仓库建立到 **v0.14** 为止的**版本专指**文档（需求 / bug / release / 重构 / 问题记录 / 版本计划 /
每日 prompt 快照）。2026-09-24 一次性归档，`git mv` 迁移，逐文件历史保留。跟过去说拜拜。

## 留在 doc/ 根的是「跨版本」文档

- 常青设计文档：[编码规范.md](../编码规范.md)、[editor.md](../editor.md)、[config.md](../config.md)、
  [command.md](../command.md)、[capability.md](../capability.md)、[ui.md](../ui.md)、[menu.md](../menu.md)、
  [navigator.md](../navigator.md)、[outline_area.md](../outline_area.md)、[project.md](../project.md)、
  [execute.md](../execute.md)、[git.md](../git.md)、[mcp-a2a.md](../mcp-a2a.md)、[icon.md](../icon.md)、
  [logo.md](../logo.md)
- 图片素材目录：[preview/](../preview/)（`tools/logo/build_logo.py` 的默认输出目录，别搬）
- **当前版本**（v1.0.0，尚未开工）的三份：[需求-便携形态与零残留-v1.0.0.md](../需求-便携形态与零残留-v1.0.0.md)、
  [架构-便携根与项目状态搬迁-v1.0.0.md](../架构-便携根与项目状态搬迁-v1.0.0.md)、[排期-1.0.0.md](../排期-1.0.0.md)
  —— v1.0.0 是「在做」不是「过去」，所以不进来；等 v1.x 的文档攒够了再建 `doc/v1.x/`。

## 归档时同步改了什么

跨版本文件里指向这些文档的路径统一改成 `doc/v0.x/` 前缀（仓库根相对写法）；归档目录内部的
markdown 相对链接退一级（`(execute.md)` → `(../execute.md)`）。改完做了一遍**链接体检**：把仓库里
所有 markdown 的 `doc/` 引用逐个 stat 一遍 —— **本次迁移新引入的断链 0 条**（顺带修掉两处：
`config.md` / `execute.md` 里指向 `bug-运行目录错误-v0.0.4.md` 的旧相对链接）。体检剩余 4 处
都不是这次弄坏的：`doc/rag.md`（该文档早已删除）、`tools/lint/harness_lint/conventions/testing.md`
里的 harness 仓路径、`ui/help-*.md` 里两处把扩展名当路径读的假阳性。

## 两条**故意不改**的

- prompt 快照里的「@doc/xxx.md」式引用是当时的原文 —— 改了就是伪造记录，按原样保留。
- 「需求-Harness-*.md」是 harness 仓的文档，从未迁进本仓：`crates/harness-engine/src/eval.rs`、
  `tools/lint/harness_lint/graph.py` 里对它的引用在搬家**之前**就已经是死的（不是这次弄坏的）。
