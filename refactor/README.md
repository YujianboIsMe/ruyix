# refactor/ —— 一次性重构工具（可审计 / 可重放，不是日常工具）

这里的脚本**都已经跑完了**，留着是为了"这次重构是怎么做出来的"可复查、可重放。
日常没人需要跑它们；判据永远是仓库自己的门禁（`cargo test` / `cargo fmt --check` /
`cargo clippy` / `node scripts/ui-smoke.js` / `node scripts/check-style.js`）。

```
mainjs-split/            # ui/scripts/main.js 4332 → 1053 行的拆分（7 个模块）；见它的 README
host-tests-split.py      # src-tauri/src/main.rs 的 1 拆 2：测试搬进 src-tauri/src/tests.rs
main-split-nav.py        # 1 拆 N 第二刀：导航闸门/外链出口 → src-tauri/src/nav.rs
main-split-mem.py        # 1 拆 N 第三刀：项目记忆 → src-tauri/src/mem_cmds.rs
main-split-proc.py       # 1 拆 N 第四刀：托管进程/运行目标 → src-tauri/src/proc_cmds.rs
main-split-highlight.py  # 1 拆 N 第五刀：语法高亮（三段）→ src-tauri/src/highlight.rs
main-split-pty.py        # 1 拆 N 第六刀：PTY 终端 + 终端目标 → src-tauri/src/pty_cmds.rs
main-split-by-domain.py  # 1 拆 N 第七刀：Tauri 命令段按域 → fs_cmds.rs / project_cmds.rs / plugin_cmds.rs
agent-split.py           # 引擎 agent.rs 3975 行按域 → agent/{types,gate,connect,prompt,action,tools}.rs（子模块 + use super::*）
u52-scan-scope.py        # 量 U52 到底扫到了多少（把内联测试模块挖掉，看剩下多少生产代码）—— ISSUE-1 的证据
tr-snapshot.js           # 翻译表快照 diff：大改 ui/scripts/errors.js 时抓"有没有把别的句子翻歪"
```

对应的记录：`doc/重构-main.js拆分-完成记录.md`、`doc/v1.2/bugs.md` ISSUE-1。
