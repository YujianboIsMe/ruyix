// 语法高亮插件化：**两种编译模式的真机判据**（跑真 exe，不跑 mock）。
//
// 用法：
//   node scripts/highlight-modes-probe.mjs              # 需要时自动构建纯净版（首次约 2~5 分钟）
//   node scripts/highlight-modes-probe.mjs --no-build   # 两枚 exe 都在了，只跑判据
//
// ## 判据（**预注册，跑之前钉死**）
//
// 预装模式（默认特性，target/debug/ruyix.exe）：
//   A1 首启**物化**预装插件：`<根>/plugins/highlight/ruyix-builtin/plugin.toml` 与 `theme.css` 都出现；
//   A2 `theme.css` 里 `.tok-*` 规则 ≥ 30 条（"配色确实在插件里"，不是空壳）；
//   A3 `<根>/global/logs/plugins.jsonl` 有 `"event":"load"`、`"mode":"preinstalled"`、`"id":"ruyix-builtin"`；
//   A4 **幂等且不覆盖**：往里写一句哨兵后再跑一次，哨兵必须还在（用户改过的主题比我们的默认值重要）。
//
// 纯净模式（--no-default-features --features custom-protocol，target-pure/debug/ruyix.exe）：
//   B1 `<根>/plugins/` 下**没有任何** plugin.toml（一个解析器、一个插件都不预装）；
//   B2 JSONL 里 `"mode":"pure"` 且 `"builtin":false`（程序自己承认"我没有内置解析器"）；
//   B3 程序照常起来又照常被杀掉（**降级不是崩**）。
//
// 退出码 0 = 全部达预期；任何一条不达预期都会打印原始证据并 FAIL。
//
// 为什么必须跑真 exe：这两条差异（物化 / 不物化）**只存在于编译期特性里** ——
// 单测只能验"这个函数在 pure 特性下返回空"，验不了"这枚二进制里真的没有 arborium"。
import { execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const ROOT = path.resolve(import.meta.dirname, "..");
const NO_BUILD = process.argv.includes("--no-build");
// **不要**用系统临时目录：那是"从 zip 里直接双击"的判定区，根会被拒（弹【必须解压后试用】）。
const SCRATCH = path.join(path.parse(ROOT).root, "ruyix-mode-probe");

const PRE_EXE = path.join(ROOT, "target", "debug", "ruyix.exe");
const PURE_EXE = path.join(ROOT, "target-pure", "debug", "ruyix.exe");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const results = [];
function check(arm, name, ok, detail = "") {
  results.push({ arm, name, ok, detail });
  console.log(`${ok ? "PASS" : "FAIL"}  [${arm}] ${name}${detail ? "  " + detail : ""}`);
}

function killAll() {
  try {
    execFileSync("taskkill", ["/F", "/IM", "ruyix.exe"], { stdio: "ignore" });
  } catch {
    /* 没有进程在跑 = 正常 */
  }
}

function build(what, args) {
  console.log(`\n[build] ${what} …`);
  const r = spawn("cargo", args, { cwd: ROOT, stdio: "inherit", shell: true });
  return new Promise((res) =>
    r.on("exit", (code) => {
      if (code !== 0) {
        console.error(`构建失败（${what}），退出码 ${code}`);
        process.exit(1);
      }
      res();
    })
  );
}

/** 把 exe 拷进一个空目录（= 便携根），起一次，等它把该写的写完，再杀掉。 */
async function runOnce(exe, root) {
  fs.mkdirSync(root, { recursive: true });
  const dest = path.join(root, "ruyix.exe");
  fs.copyFileSync(exe, dest);
  const p = spawn(dest, [], { cwd: root, stdio: "ignore" });
  await sleep(4000);
  killAll();
  await sleep(400);
  try {
    p.kill();
  } catch {
    /* 已经没了 */
  }
}

const read = (p) => (fs.existsSync(p) ? fs.readFileSync(p, "utf8") : "");

async function armPreinstalled() {
  const root = path.join(SCRATCH, "preinstalled");
  fs.rmSync(root, { recursive: true, force: true });
  await runOnce(PRE_EXE, root);

  const dir = path.join(root, "plugins", "highlight", "ruyix-builtin");
  const manifest = dir + path.sep + "plugin.toml";
  const theme = dir + path.sep + "theme.css";
  check("A", "A1 物化了预装插件（plugin.toml + theme.css）",
    fs.existsSync(manifest) && fs.existsSync(theme),
    `plugin.toml=${fs.existsSync(manifest)} theme.css=${fs.existsSync(theme)}`);

  const css = read(theme);
  const rules = (css.match(/\.tok-[a-z0-9-]+ \{/g) || []).length;
  check("A", "A2 配色确实在插件里（.tok-* 规则 ≥ 30）", rules >= 30, `实际 ${rules} 条`);

  const log = read(path.join(root, "global", "logs", "plugins.jsonl"));
  check("A", "A3 留痕里记着这次加载", log.includes('"event":"load"') && log.includes('"mode":"preinstalled"'),
    log.split("\n")[0]?.slice(0, 120) || "（没有 plugins.jsonl）");
  check("A", "A3b 留痕点名了 ruyix-builtin", log.includes('"id":"ruyix-builtin"'), "");

  // A4：用户改过的主题**不许**被覆盖
  fs.appendFileSync(theme, "\n/* 哨兵：用户改的 */\n");
  await runOnce(PRE_EXE, root);
  check("A", "A4 再跑一次不动用户改过的文件", read(theme).includes("哨兵：用户改的"), "");

  console.log(`\n[证据] ${path.join(root, "global", "logs", "plugins.jsonl")} 尾行：`);
  const tail = read(path.join(root, "global", "logs", "plugins.jsonl")).trim().split("\n").slice(-4);
  for (const l of tail) console.log("   " + l.slice(0, 160));
}

async function armPure() {
  const root = path.join(SCRATCH, "pure");
  fs.rmSync(root, { recursive: true, force: true });
  await runOnce(PURE_EXE, root);

  const pluginRoot = path.join(root, "plugins");
  const found = [];
  const walk = (d) => {
    if (!fs.existsSync(d)) return;
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (e.name === "plugin.toml") found.push(p);
    }
  };
  walk(pluginRoot);
  check("B", "B1 一个插件都没预装", found.length === 0, `找到 ${found.length} 个 plugin.toml`);

  const log = read(path.join(root, "global", "logs", "plugins.jsonl"));
  check("B", "B2 程序承认自己是纯净模式（mode=pure, builtin=false）",
    log.includes('"mode":"pure"') && log.includes('"builtin":false'),
    log.trim().split("\n")[0]?.slice(0, 140) || "（没有 plugins.jsonl）");
  check("B", "B3 降级不是崩（根目录建起来了）",
    fs.existsSync(path.join(root, "global")) && fs.existsSync(path.join(root, "projects")), "");
}

console.log(`[mode-probe] 预装 exe: ${PRE_EXE}\n[mode-probe] 纯净 exe: ${PURE_EXE}`);
if (!NO_BUILD) {
  if (!fs.existsSync(PRE_EXE)) await build("预装模式（默认特性）", ["build", "-p", "ruyix"]);
  if (!fs.existsSync(PURE_EXE)) {
    await build("纯净模式（--no-default-features --features custom-protocol）", [
      "build", "-p", "ruyix", "--no-default-features", "--features", "custom-protocol",
      "--target-dir", "target-pure",
    ]);
  }
}
for (const p of [PRE_EXE, PURE_EXE]) {
  if (!fs.existsSync(p)) {
    console.error(`缺 ${p}（先跑一次 cargo build，或去掉 --no-build）`);
    process.exit(1);
  }
}

killAll();
await sleep(300);
await armPreinstalled();
await armPure();
killAll();

const failed = results.filter((r) => !r.ok);
console.log(`\n=== 模式探针：${results.length - failed.length}/${results.length} 条达预期 ===`);
if (failed.length) {
  console.log("未达预期：");
  for (const f of failed) console.log(`  [${f.arm}] ${f.name}  ${f.detail}`);
}
// 清理（留着也会被下一次跑掉，但别在用户盘上攒垃圾）
try {
  fs.rmSync(SCRATCH, { recursive: true, force: true });
  console.log(`[mode-probe] 已清理 ${SCRATCH}`);
} catch (e) {
  console.log(`[mode-probe] 清理失败（下次跑会覆盖）：${e.message}`);
}
process.exit(failed.length ? 1 : 0);
