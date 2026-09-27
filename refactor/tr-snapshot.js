// 翻译表快照 diff 工具（ISSUE-1 的回归证据就靠它）：
//   1) node refactor/tr-snapshot.js <仓库根> <errors.js> before.json     # 改动前
//   2) 改 ui/scripts/errors.js
//   3) node refactor/tr-snapshot.js <仓库根> <errors.js> after.json      # 改动后
//   4) diff 两份 json —— 只看"该变的变了"，多出来的变化就是误伤。
// 语料 = 两个源码根下**所有** .rs 字符串字面量里的中文（4420 条），比 U52 的错误文案口径宽，
// 专门用来抓"把别的句子翻歪"。
// 语料回归：把所有 .rs 里的中文字面量喂给 errors.js 的 translate，导出 {字面量: 译文} 快照。
// 用法：node tr_corpus.js <仓库根> <errors.js> <输出.json>
const fs = require("fs");
const path = require("path");
const ROOT = process.argv[2];
const ERRORS_JS = process.argv[3];
const OUT = process.argv[4];

const win = { I18N: { t: (k) => k, getLang: () => "zh-CN" } };
Object.assign(globalThis, { window: win });
new Function(fs.readFileSync(ERRORS_JS, "utf8"))();
const translate = win.BackendMsg.translate;

const files = [];
const walk = (dir) => {
  let ents = [];
  try { ents = fs.readdirSync(path.join(ROOT, dir), { withFileTypes: true }); } catch { return; }
  for (const e of ents) {
    const rel = dir + "/" + e.name;
    if (e.isDirectory()) walk(rel);
    else if (e.name.endsWith(".rs")) files.push(rel);
  }
};
["src-tauri/src", "crates/harness-engine/src"].forEach(walk);

const CJK = /[\u4e00-\u9fff]/;
const lits = new Set();
for (const f of files) {
  const src = fs.readFileSync(path.join(ROOT, f), "utf8").replace(/\r\n/g, "\n");
  for (const m of src.matchAll(/"((?:[^"\\\n]|\\.)*)"/g)) {
    if (CJK.test(m[1])) lits.add(m[1]);
  }
}
const snapshot = {};
for (const l of [...lits].sort()) snapshot[l] = translate(l);
fs.writeFileSync(OUT, JSON.stringify(snapshot, null, 1), "utf8");
console.log(`语料 ${lits.size} 条；未翻译 ${Object.values(snapshot).filter((v, i) => v === Object.keys(snapshot)[i]).length} 条 → ${OUT}`);
