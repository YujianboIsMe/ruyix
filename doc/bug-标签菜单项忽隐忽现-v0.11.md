# BUG：标签右键菜单的【关闭左侧】"消失"了
## 归属版本：v0.11 · 状态：**已修复**（2026-09-24）· 报告：用户

> 用户原话：*"编辑区标签的右键菜单里没有【关闭左侧】子菜单。"*

## 1. 根因：不是没有，是**按标签位置被藏掉了**

代码里这一项一直都在 —— `ui/index.html` 有 `data-tab-action="left"`，
`ui/main.js::closePlan` 有 `case "left": return ids.slice(0, i)`。真错在**可见性规则**：

```js
row.style.display = closePlan(ids, targetId, mode).length ? "" : "none";   // 旧版
```

右键**最左**那个标签时，"左侧"的计划为空 ⇒ 这一项被 `display:none` 整条藏掉 ⇒ 用户看到的
就是"菜单里没有这一项"，于是报成缺功能。真 DOM 复现（`scripts/tab-menu-layout.js`，修复前）：

```
中间标签： close others right left all copy-path copy-full-path
最左标签： close others right left(藏) all copy-path copy-full-path      ← 就是这一条
```

## 2. 改法：**恒在 + 置灰禁用**

| 落点 | 内容 |
|---|---|
| `ui/main.js` | 可见性从"藏/显"改成"可用/禁用"：`row.style.display = ""` 恒定，干不了的加 `.context-menu-item--disabled`（同时给 `aria-disabled`）；点击处理里禁用项直接 return |
| `ui/styles.css` | `.context-menu-item--disabled { opacity: .4; pointer-events: none }`（注释写明**别改回 `display:none`**） |

适用面：单标签时的 others/right/left、伪标签（会话/配置/服务）的复制项 —— 全都**留在菜单里**。
关闭策略本身（`closePlan`）没动。

## 3. 判据

- `scripts/tab-menu-layout.js`（**真 index.html + 真 styles.css + main.js 真源码切片**，
  派发真的 `contextmenu` 事件读 computed display）：七项恒在 / 最左标签的【关闭左侧】是禁用态 /
  右键不改激活标签 / 点禁用项什么都不发生 / 点【关闭左侧】只关左边的 / CSS 里没有藏 `.context-menu*` 的规则。
  ```
  中间标签： close others right left all copy-path copy-full-path
  最左标签： close others right left(灰) all copy-path copy-full-path
    ok  右键中间标签：七项全在，且都不是禁用态
    ok  右键最左标签：七项仍然全在（不消失）
    ok  最左标签上【关闭左侧】是**禁用**态（没东西可关，但不许消失）
    ok  右键不改激活标签（仍是 t1）
    ok  点禁用项（left）什么都不发生
    ok  点【关闭左侧】只关它左边的那些（t1）
    ok  ui/styles.css 里没有把 .context-menu* 藏掉的规则
  ```
- ui-smoke：**U45**（回放，判据同步改成"恒在+禁用"）+ **U46**（跑上面那张探针）。
- 全量：ui-smoke **310/310**、check-style PASS、clippy 0、fmt 干净、引擎 351 / 宿主 121 不变。

## 4. 教训（一条 UX 规矩，写下来免得再犯）

**菜单项的位置是用户记住的东西**：会因为上下文而"忽隐忽现"的项，一定会被当成"功能没了"
（本单就是——逻辑没坏，只是那一项没出现在那个位置）。干不了就**置灰禁用**，不要藏；
只有当这一项在这个上下文里**语义根本不成立**时，才谈得上隐藏。

> 附带：文件树右键菜单（`showContextMenu`）对项目根目录也是"隐藏删除/重命名"的老做法，
> 属于同一类；那处**没动**（没在本次报告范围里），要不要一起改由用户定。
