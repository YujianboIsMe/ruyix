# main.js 拆分：完成记录（2026-09-27）

**一句话**：`ui/scripts/main.js` 从 **4332 → 1053 行**，7 个模块各自持有 `window.XxxUI`；
门禁 **415/415**（含 U56 startup-scope / U57 startup-real 真浏览器启动探针）、风格 0 error、
两个真浏览器几何探针全通过。前端行为**未改**：这份拆分是搬运，不是重写。

（本文件的前身是同一位置的**交接单** —— 那时只差"①接线 ②删掉 main.js 里两份重复副本"两步。
两步都做完了，交接单改成完成记录，诊断与踩坑原样留着：它们是这次最有价值的部分。）

## 交付物

| 项 | 结果 |
|---|---|
| 模块 | `ui/scripts/{titlebar,menus,contextmenu,commandbar,navigator,editor,terminal}.js`（258 / 198 / 504 / 78 / 739 / 1360 / 364 行） |
| 命名空间 | `window.TitleBarUI / MenusUI / ContextMenuUI / CommandBarUI / NavigatorUI / EditorUI / TerminalUI`（函数本身仍是**全局**：经典脚本，`initApp` 按原次序裸名调用） |
| `main.js` | 1053 行 = `state`（含 `window.state` 导出）+ 标签页 + git 面板 + 弹窗 + 项目语言表 + 运行目标 + 状态栏 + 引导块 |
| `ui/index.html` | 7 个 `<script>` 插在 `external.js` 之后，`main.js` 仍**最后**加载 |
| `scripts/ui-sources.js`（新） | 前端脚本清单：从 index.html 现取真实 `<script src="scripts/*.js">` 顺序，探针与门禁共用 |
| 搬运工具 | 4 个一次性脚本（**已删，2026-09-30**）—— 脚本本体与逐次运行记录都在 git 历史里（`git log --diff-filter=D -- refactor/mainjs-split/`） |

## 两步是怎么做的

### ① 接线

搬运脚本（已删）里的 `wire_up.py`：7 个模块落位（行尾统一 CRLF）+ 在 index.html 里
**按 `\r\n` 切分后按行插入**。**坑**：index.html 是 CRLF，用 LF 锚点会一条也匹配不上 ——
脚本会"成功地什么都没改"。

### ② 删掉"搬了一半"的副本（交接单里判定的唯一实质工作）

交接单点名的两块（`_ctxPath/_ctxIsDir/_ctxIsRoot`、`_welcomeLoadToken`/`loadWelcome`）正是
"顶层声明归属于某个模块"的那一类，所以它们被**同一条规则**一并解决，不需要特例：
凡是顶层声明名出现在某个模块的顶层声明表里，就整块删掉（连同紧邻的注释块）。

**怎么定位块边界**（这是这活里唯一需要真做对的事）：搬运脚本（已删）里的 `split_main.py`
用逐字符扫描器（字符串 / 模板串 / 正则 / 注释 / 括号配对）算出**每一行的行首括号深度**，
深度为 0 且匹配顶层声明正则的行就是一个顶层项的开始，结束行 = 深度回到 0 的那一行。
删之前过 5 条自检：① 每个被删行都能在它的模块里逐行找到（`state.` → `window.state.` 归一后）；
② 被删项的结束行形状像结尾（`}` / `;`）；③ 删除区间不碰任何保留项；④ 新文件里不再出现任何
被搬走的顶层名；⑤ 新文件的顶层项清单与"保留项"逐项、逐序相等。

**别用模糊对齐（difflib）**：试过，会漂。贪婪 LCS 把 `updateProjectMenu` 的收尾三行判成
"主文件独有"而留在原地 —— 结果是主文件里留下**孤立的 `}`**，那是语法错误，不是"少删一行"。
（同样的教训在引擎侧已经写进注释：一次合法的文件切分，不该让门禁报"文件挪了"。）

**append 型编辑的幂等判据**（第三条，也是真踩的）：判据不能写成「旧文本没命中 and 新文本已在」——
对「在旧行后面追加一行」不成立（旧行根本没被改掉，而新文本**包含**旧行）⇒ 第二次跑又插一遍。
判据要写成**新文本已在就跳过**。（当时多插了一份 `require`、多插了一份 `readUiScripts` 助手块。）

## 门禁改了什么，以及为什么

拆分把"门禁按文件名找代码"这件事变成了**假红**：ui-smoke 里 24 处
`read("ui/scripts/main.js")` 与 5 个布局探针都还钉着旧文件名，报的是"文件挪了"。
重定向的原则（与引擎侧 `readEngineAgent` 同一条）：

- **`readUiScripts()`**（ui-smoke）= 按 index.html 真实顺序拼接的**前端全量源码**，清单来自
  `scripts/ui-sources.js`（探针共用同一份）。凡"代码里写了 X / 不许出现 X / 切一段源码出来跑"
  的断言都读它 —— 契约的对象是**模块**，不是文件名。
- **`readUiModule(file)`** = 单个模块。有**两类判据不能读全量**，读了会判错对象：
  「不许出现 X」（命令层里 `invoke("config_set")` / `invoke("project_bucket_delete")` 是
  命令系统的本体，本来就该有 —— U48 / U51 判的是**面板**不许绕开命令层）与「只许有一处调用」
  （`highlightAndRender` 的另一处调用在 command.js 的显式换语言路径上，用户点"重新着色"就该
  重着色 —— U34 判的是**编辑器模块**不许在打字路径上跑全量）。实测：读全量会把这 3 条判红，
  那不是"更严"，是判错了对象。
- **回放沙箱按真页面装配补齐**：模块里读 `window.state`（main.js 顶层 `const` 不挂 window，
  靠显式导出），所以 U22 / U45 / U33 的沙箱补上 `window.state`；U53 还要**显式传 `BackendMsg`**
  —— 真页面里 errors.js 把它挂到 window 上、裸引用才成立，而 `new Function` 里自由变量解析到的
  是本进程的 globalThis，不传就 ReferenceError（红出来的方向还是错的那个）。
- **U56 / U57 一字未改** —— 正是它们抓出这次拆分第一版的病（main.js 里留着 `let` 副本 ⇒
  后加载的脚本整份不执行 ⇒ 白屏）。U56 扫的是 `ui/scripts/*.js` **目录**，7 个新模块自动进覆盖面。

## 实测读数

```
node scripts/check-style.js            42 个文件：0 error, 300 warning
node scripts/ui-smoke.js               415/415（U56 startup-scope ✓ / U57 startup-real ✓ 真浏览器）
node scripts/editor-layout.js          editor-layout: 全部通过
node scripts/session-trace-layout.js   session-trace-layout: 全部通过
```

## 留下的可复现判据

- **U56 startup-scope**：跨文件顶层 `const/let/class` 不许重名（经典脚本共享**一个**全局词法
  作用域 —— 重名 = 后加载的那份在求值前抛 SyntaxError、**整份不执行**）、`window.L` 只许 command.js
  定义、main.js 不许再声明顶层 `L`。
- **U57 startup-real**：真浏览器 + 真 index.html + 真脚本清单：零 SyntaxError，且
  `state` / `showPane` / `L` / `EDITOR_PANES` 真的被赋值（脚本没执行时这几个都是 false）。
- **`readUiScripts()` / `readUiModule(file)`**：门禁的对象是模块，不是文件名 —— 下一次合法的
  文件切分不该把契约判红。

## 边界与后续

- 前端仍**无打包器 / 无 npm**（老规矩）：模块靠 index.html 的 `<script>` 顺序 + 经典脚本共享的
  全局作用域协作，`main.js` 必须最后加载。
- `ui/` 是**编译期内嵌**进 exe 的 ⇒ 改了前端要重编（debug 与 release 两个产物都要）。
- 搬运工具是一次性脚本，**已删**（2026-09-30）。它们的价值是「这次是怎么做的」可复查，而脚本与逐次运行记录都在 git 历史里；留着只会让 `refactor/` 变成第二个垃圾场 —— 判据永远是仓库自己的门禁，不是这些脚本。
