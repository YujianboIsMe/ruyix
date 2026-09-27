# main.js 拆分：一次性工具（2026-09-27）

这四个脚本把 `ui/scripts/main.js`（4332 行）拆成 7 个模块 + 1053 行的 main.js，并把这活
留下的坑写进判据。**活已经做完了**，留在这里是为了可审计 / 可重放，不是日常工具。

| 脚本 | 干什么 | 幂等 |
|---|---|---|
| `split_main.py` | 重建拆分版 main.js：逐字符扫描器（字符串 / 模板串 / 正则 / 注释 / 括号配对）算每一行的行首深度 ⇒ 顶层项边界；删掉归模块的声明（连同紧邻注释块），过 5 条自检才写盘 | 已拆过 ⇒ 什么都不动 |
| `wire_up.py` | 7 个模块落位 `ui/scripts/`（行尾统一 CRLF）+ 在 index.html 的 `external.js` 之后按 CRLF 插 7 行（main.js 仍**最后**加载） | 已接线 ⇒ 跳过 |
| `redirect_asserts.py` | ui-smoke 的 24 处 `read("ui/scripts/main.js")` 与 5 个布局探针 → 改读 `scripts/ui-sources.js` 的**前端全量源码**；回放沙箱按真页面装配补 `window.state` | 锚点 + 命中数断言，改过就跳过 |
| `fix_scope.py` | 把**文件作用域**的判据收回模块（U34 → editor.js / U48 → navigator.js / U51 → terminal.js）+ U53 回放显式传 `BackendMsg` | 同上 |

```bash
python refactor/mainjs-split/split_main.py --dry     # 只报告 + 写预览，不碰 ui/
python refactor/mainjs-split/split_main.py --write
python refactor/mainjs-split/wire_up.py
python refactor/mainjs-split/redirect_asserts.py
python refactor/mainjs-split/fix_scope.py
```

## 三条踩过的坑（重放时会再撞上）

1. **别用 difflib 对齐**。贪婪 LCS 在这份文件上会漂：`updateProjectMenu` 的收尾三行被判成
   "主文件独有" 而留在原地 —— 结果是主文件里留下**孤立的 `}`**，那是语法错误，不是"少删一行"。
   扫描器算行首括号深度才对（`split_main.py::depth_at_line_start`）。
2. **index.html 是 CRLF**。LF 锚点一条也匹配不上 —— 脚本会"成功地什么都没改"。
3. **append 型编辑的幂等判据**：`旧文本没命中 and 新文本已在` 对"在旧行后面追加一行"不成立
   （旧行没被改掉，而新文本包含旧行）⇒ 第二次跑又插一遍。判据要写成**新文本已在就跳过**。
   （这条真踩了：多插了一份 require、多插了一份 `readUiScripts` 助手块。）

## 判据（收尾必须全绿）

```bash
node scripts/check-style.js        # 0 error
node scripts/ui-smoke.js           # 415/415（含 U56 startup-scope / U57 startup-real 真浏览器）
node scripts/editor-layout.js      # 真浏览器几何
node scripts/session-trace-layout.js
```
