/**
 * 前端一方脚本的源码清单 —— 从 `ui/index.html` 里**真的** `<script src="scripts/*.js">` 取，
 * 按页面的加载顺序。
 *
 * 为什么要有这个文件：`ui/scripts/main.js` 拆成 7 个模块（titlebar / menus / contextmenu /
 * commandbar / navigator / editor / terminal）之后，"把前端源码拼起来跑一遍"的探针
 * （面板回放 / 布局探针 / 宽行探针 / 标签菜单探针…）若各自硬编码 `main.js`，一次合法的文件切分
 * 就会让它们集体失灵 —— 而它们的判据本来与文件名无关（同一个道理见 ui-smoke 的 readEngineAgent：
 * 契约的对象是模块，不是文件名）。
 *
 * 顺手的好处：清单从 index.html 现取，"页面漏挂某个脚本"在探针侧就可见了
 * （真浏览器那条端到端判据是 U57 / scripts/startup-probe.js）。
 */
const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");

/** index.html 里按顺序被引用的 `ui/scripts/*.js`（同一文件重复 include 只算一次）。 */
function uiScriptFiles() {
  const html = fs.readFileSync(path.join(ROOT, "ui", "index.html"), "utf8");
  const seen = new Set();
  const out = [];
  for (const m of html.matchAll(/<script\s+src="scripts\/([\w.-]+\.js)"/g)) {
    if (seen.has(m[1])) continue;
    seen.add(m[1]);
    out.push(m[1]);
  }
  return out;
}

/** 相对仓库根的路径，如 `ui/scripts/titlebar.js`。 */
function uiScriptPaths() {
  return uiScriptFiles().map((f) => "ui/scripts/" + f);
}

/** 全量源码（行尾归一 LF，按页面顺序用换行拼）。 */
function uiScriptSource() {
  return uiScriptPaths()
    .map((p) => fs.readFileSync(path.join(ROOT, p), "utf8").replace(/\r\n/g, "\n"))
    .join("\n");
}

module.exports = { uiScriptFiles, uiScriptPaths, uiScriptSource };
