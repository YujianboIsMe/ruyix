// scripts/portable-root-probe.mjs —— 便携根的真机判据（A1 / A2 / A5 / A11 / A12）
//
// 零依赖 Node，跑真实的 `ruyix.exe`（不是示例、不是 mock）。五条臂，判据**预注册**在下面，
// 跑完逐条打印原始证据与 PASS/FAIL；退出码 0 = 全达预期。
//
// 预注册判据（跑之前钉死，跑完不改）：
//   A1  空目录只放 exe → 启动后**自己长出** `global/`（含 logs/cache/webview/runs）+ `projects/`
//       + `plugins/` + 三份 README；`--debug` 会话头里有 `便携根` 且等于该目录。
//   A2  会话头有 `可写 : 是` 与 `webview : <根>/global/webview` 两栏。
//   A5  成功启动后：`<根>/global/webview/EBWebView` 存在；`%LOCALAPPDATA%\com.ruyix.code\EBWebView\Local State`
//       的 mtime **逐位不变**（不指定 data dir 时它会变 —— P0 对照臂已证）。
//   A11 两份副本放在两个不同的根：**两个进程能同时在**，各自建自己的 `global/`。
//   A12 根在 `%TEMP%` 下 → 拒绝启动（退出码 2）+ stderr 出现【必须解压后试用】+ 该目录**不长出** global/；
//       根不可写（自己给目录下 DENY 写 ACL）→ 拒绝启动（退出码 2）+ stderr 出现【绿色软件无法安装在系统级目录】；
//       两种情况都要**真的弹出原生框**（用窗口标题证明，不是只看 stderr）。
//
// 用法：node scripts/portable-root-probe.mjs [--keep]
//   跑完自动清理探针目录；`--keep` 留着以便人工查看。

import { execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const KEEP = process.argv.includes("--keep");
const EXE = path.resolve("target", "debug", "ruyix.exe");
const SCRATCH = "D:\\ruyix-probe"; // 非临时目录（临时目录会被 A12 的判据拒掉）
const results = [];
let failed = 0;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function check(arm, ok, detail) {
  results.push({ arm, ok, detail });
  if (!ok) failed += 1;
  console.log(`${ok ? "PASS" : "FAIL"}  ${arm}\n      ${detail}`);
}

function rmrf(p) {
  try {
    fs.rmSync(p, { recursive: true, force: true });
  } catch {
    /* 忽略 */
  }
}

function copyExe(dest) {
  fs.mkdirSync(dest, { recursive: true });
  fs.copyFileSync(EXE, path.join(dest, "ruyix.exe"));
}

/** 跑 exe 若干秒后收掉；返回 {code, stdout, stderr}（code 为 null = 被我们杀掉的） */
function runExe(dir, { secs = 12, env = {} } = {}) {
  return new Promise((resolve) => {
    const p = spawn(path.join(dir, "ruyix.exe"), ["--debug"], {
      cwd: dir,
      env: { ...process.env, ...env },
      stdio: ["ignore", "pipe", "pipe"],
    });
    let out = "",
      err = "";
    p.stdout.on("data", (d) => (out += d.toString("utf8")));
    p.stderr.on("data", (d) => (err += d.toString("utf8")));
    const timer = setTimeout(() => {
      try {
        p.kill();
      } catch {
        /* 已退出 */
      }
    }, secs * 1000);
    p.on("exit", (code) => {
      clearTimeout(timer);
      resolve({ code, stdout: out, stderr: err });
    });
  });
}

/** 窗口标题（证明原生弹框真的出现了）：tasklist /V 的最后一列 */
function windowTitles() {
  try {
    // 注意：tasklist 的输出走**控制台代码页**（中文 Windows = GBK）。按 utf8 解会得到一串乱码，
    // 于是"弹框明明出现了"却被判成没出现 —— 探针的假 FAIL 也是一种错判，得一起修。
    const buf = execFileSync("tasklist", ["/FI", "IMAGENAME eq ruyix.exe", "/V", "/FO", "CSV"]);
    let txt;
    try {
      txt = new TextDecoder("gbk").decode(buf);
    } catch {
      txt = buf.toString("utf8");
    }
    return txt
      .split(/\r?\n/)
      .filter((l) => l.includes("ruyix.exe"))
      .map((l) => l.split('","').slice(-1)[0].replace(/"?\s*$/, ""));
  } catch {
    return [];
  }
}

function killAll() {
  try {
    execFileSync("taskkill", ["/F", "/IM", "ruyix.exe"], { stdio: "ignore" });
  } catch {
    /* 没有在跑 */
  }
}

const localState = path.join(
  process.env.LOCALAPPDATA || "",
  "com.ruyix.code",
  "EBWebView",
  "Local State"
);
const mtimeOf = (p) => {
  try {
    return fs.statSync(p).mtimeMs;
  } catch {
    return null;
  }
};

// ---------------------------------------------------------------- 臂 1：A1 / A2 / A5
async function armCore() {
  const dir = path.join(SCRATCH, "a1");
  rmrf(dir);
  copyExe(dir);
  const lsBefore = mtimeOf(localState);
  const homeBefore = fs.existsSync(path.join(os.homedir(), ".ruyix"))
    ? JSON.stringify(fs.readdirSync(path.join(os.homedir(), ".ruyix")))
    : "(不存在)";

  const r = await runExe(dir, { secs: 14 });
  killAll();
  await sleep(500);

  const want = ["global", "projects", "plugins"];
  const got = fs.readdirSync(dir).sort();
  check("A1 空目录自建布局", want.every((d) => got.includes(d)), `目录 = ${got.join(", ")}`);
  const sub = ["global/logs", "global/cache", "global/webview", "global/runs"];
  check(
    "A1 子目录齐全",
    sub.every((d) => fs.existsSync(path.join(dir, d))),
    sub.map((d) => `${d}=${fs.existsSync(path.join(dir, d))}`).join(" ")
  );
  check(
    "A1 模板 README",
    ["global/README.md", "projects/README.md", "plugins/README.md"].every((f) =>
      fs.existsSync(path.join(dir, f))
    ),
    "三份 README"
  );

  const log = path.join(dir, "global", "logs", "debug.log");
  const head = fs.existsSync(log) ? fs.readFileSync(log, "utf8").split("\n").slice(0, 14) : [];
  const line = (k) => head.find((l) => l.startsWith(k)) || "";
  check(
    "A2 会话头：便携根 = exe 目录",
    line("便携根").includes(dir),
    JSON.stringify(line("便携根").trim())
  );
  check("A2 会话头：可写 + webview", line("可写").includes("是") && line("webview").includes("global\\webview"),
    `${line("可写").trim()} | ${line("webview").trim()}`);
  console.log("      首启新建 : " + line("首启新建").trim());

  const eb = path.join(dir, "global", "webview", "EBWebView");
  check("A5 WebView2 profile 落根内", fs.existsSync(eb), eb);
  const lsAfter = mtimeOf(localState);
  check(
    "A5 %LOCALAPPDATA% 未被碰",
    lsBefore === lsAfter,
    `Local State mtime ${lsBefore} → ${lsAfter}（必须逐位相同）`
  );
  const homeAfter = fs.existsSync(path.join(os.homedir(), ".ruyix"))
    ? JSON.stringify(fs.readdirSync(path.join(os.homedir(), ".ruyix")))
    : "(不存在)";
  check("A5 家目录 ~/.ruyix 未被碰", homeBefore === homeAfter, `${homeBefore} → ${homeAfter}`);
  return dir;
}

// ---------------------------------------------------------------- 臂 2：A12 临时根
async function armTemp() {
  const dir = path.join(os.tmpdir(), "ruyix-probe-temp");
  rmrf(dir);
  copyExe(dir);
  const r = await runExe(dir, { secs: 20, env: { RUYIX_NO_DIALOG: "1" } });
  const after = fs.readdirSync(dir).sort();
  check(
    "A12 临时根：拒绝启动（退出码 2）",
    r.code === 2,
    `exit=${r.code} stderr=${JSON.stringify(r.stderr.slice(0, 220))}`
  );
  check(
    "A12 临时根：文案【必须解压后试用】",
    r.stderr.includes("必须解压后试用"),
    r.stderr.split("\n").slice(0, 3).join(" / ")
  );
  check("A12 临时根：不长出 global/", after.length === 1 && after[0] === "ruyix.exe", after.join(", "));
  return dir;
}

/** 真的弹出原生框了吗：不用 RUYIX_NO_DIALOG 跑，读窗口标题 */
async function armDialog() {
  const dir = path.join(os.tmpdir(), "ruyix-probe-dialog");
  rmrf(dir);
  copyExe(dir);
  const p = spawn(path.join(dir, "ruyix.exe"), [], { cwd: dir, stdio: "ignore" });
  await sleep(3500);
  const titles = windowTitles();
  killAll();
  await sleep(300);
  try {
    p.kill();
  } catch {
    /* 已退出 */
  }
  check(
    "A12 原生弹框真的出现（窗口标题为证）",
    titles.some((t) => t.includes("必须解压后试用")),
    `窗口标题 = ${JSON.stringify(titles)}`
  );
  return dir;
}

// ---------------------------------------------------------------- 臂 3：A12 只读根
async function armReadonly() {
  const dir = path.join(SCRATCH, "a3-ro");
  rmrf(dir);
  copyExe(dir);
  const user = process.env.USERNAME;
  let acl = false;
  try {
    execFileSync("icacls", [dir, "/deny", `${user}:(W)`], { stdio: "ignore" });
    acl = true;
  } catch (e) {
    check("A12 只读根：准备 ACL", false, `icacls 失败：${e.message}`);
    return dir;
  }
  try {
    // 证明这目录现在真的不可写
    let writable = true;
    try {
      fs.writeFileSync(path.join(dir, "probe.tmp"), "x");
      fs.unlinkSync(path.join(dir, "probe.tmp"));
    } catch {
      writable = false;
    }
    if (writable) {
      check("A12 只读根：前置条件（目录必须不可写）", false, "仍可写 → 本臂无效（跳过也不算过）");
      return dir;
    }
    const r = await runExe(dir, { secs: 20, env: { RUYIX_NO_DIALOG: "1" } });
    check(
      "A12 只读根：拒绝启动（退出码 2）",
      r.code === 2,
      `exit=${r.code} stderr=${JSON.stringify(r.stderr.slice(0, 240))}`
    );
    check(
      "A12 只读根：文案【绿色软件无法安装在系统级目录】",
      r.stderr.includes("绿色软件无法安装在系统级目录"),
      r.stderr.split("\n").slice(0, 3).join(" / ")
    );
    let after = null;
    try {
      after = fs.readdirSync(dir).sort();
    } catch {
      // DENY 写的目录可能连列目录都被拒 —— 列不出来同样是"没写进去"的旁证
      after = null;
    }
    check(
      "A12 只读根：不长出任何东西",
      after === null || after.length === 1,
      after === null ? "目录不可列（DENY 写 ACL 的副作用）" : after.join(", ")
    );
  } finally {
    if (acl) execFileSync("icacls", [dir, "/remove:d", user], { stdio: "ignore" });
  }
  return dir;
}

// ---------------------------------------------------------------- 臂 4：A11 两份副本
async function armTwoCopies() {
  const d1 = path.join(SCRATCH, "a1");
  const d2 = path.join(SCRATCH, "a4-two");
  rmrf(d2);
  copyExe(d2);
  const p1 = spawn(path.join(d1, "ruyix.exe"), [], { cwd: d1, stdio: "ignore" });
  await sleep(6000);
  const p2 = spawn(path.join(d2, "ruyix.exe"), [], { cwd: d2, stdio: "ignore" });
  await sleep(9000);
  let n = 0;
  try {
    const txt = execFileSync("tasklist", ["/FI", "IMAGENAME eq ruyix.exe", "/FO", "CSV"], {
      encoding: "utf8",
    });
    n = txt.split(/\r?\n/).filter((l) => l.includes("ruyix.exe")).length;
  } catch {
    /* 一个都没有 */
  }
  const both = fs.existsSync(path.join(d1, "global")) && fs.existsSync(path.join(d2, "global"));
  killAll();
  await sleep(400);
  for (const p of [p1, p2]) {
    try {
      p.kill();
    } catch {
      /* 已退出 */
    }
  }
  check("A11 两份副本同时在跑", n >= 2, `进程数 = ${n}`);
  check("A11 各自建自己的 global/", both, `${d1} / ${d2}`);
  return d2;
}

// ---------------------------------------------------------------- main
if (!fs.existsSync(EXE)) {
  console.error(`[probe] 找不到 ${EXE} —— 先 \`cargo build -p ruyix\``);
  process.exit(2);
}
console.log(`[probe] exe = ${EXE}（${(fs.statSync(EXE).size / 1048576).toFixed(1)} MB）`);
console.log(`[probe] 工作目录 = ${SCRATCH}；系统临时目录 = ${os.tmpdir()}\n`);

killAll();
const keep = [];
try {
  keep.push(await armCore());
  keep.push(await armTemp());
  keep.push(await armDialog());
  keep.push(await armReadonly());
  keep.push(await armTwoCopies());
} catch (e) {
  console.error(`[probe] 抛异常：${e.stack}`);
  failed += 1;
}

if (!KEEP) {
  for (const d of keep) rmrf(d);
  rmrf(SCRATCH);
  console.log(`\n[probe] 已清理探针目录（--keep 可保留）`);
} else {
  console.log(`\n[probe] 保留目录：${keep.join(" ")}`);
}
console.log(`\n[probe] 结论：${results.length - failed}/${results.length} 达预期`);
process.exit(failed ? 1 : 0);
