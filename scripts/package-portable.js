// scripts/package-portable.js —— 一条命令产出便携包（v1.0.0 R11 / A9）
//
// 零依赖 Node（照 check-style.js 的风格）。做四件事：
//   1. `cargo tauri build --no-bundle` —— 出**一个可执行文件**（不做安装器：便携形态不需要）；
//   2. 组包：`dist/ruyix/` = exe + 预置 `global/ projects/ plugins/`（含 README）+ 随包 README/LICENSE；
//   3. 写 `SHA256SUMS.txt`（exe 与随包文档逐个 sha256）；
//   4. 打 zip 到 `dist/`，**再解压到临时目录自校验**：逐项存在 + 校验和一致（判据不能只靠"命令没报错"）。
//
// 用法：
//   node scripts/package-portable.js              # 构建 + 组包 + 打 zip + 自校验
//   node scripts/package-portable.js --no-build   # 跳过构建（用已有 target/release/ruyix.exe）
//   node scripts/package-portable.js --mode pure  # 纯净模式：无内置解析器、无预装插件
//                                               #（产物 target-pure/release/ruyix.exe，zip 名带 -pure）
//
// 中间目录用 `RUYIX_HOME` 指向临时目录跑一次自检式启动？**不**：组包不动开发机的家 —— 只组装文件，
// 不运行 exe（要验"解压即用"就手工解压跑一次，那件事由 `scripts/portable-root-probe.mjs` 覆盖）。

"use strict";

const fs = require("fs");
const path = require("path");
const crypto = require("crypto");
const { spawnSync } = require("child_process");

const ROOT = path.resolve(__dirname, "..");
const DIST = path.join(ROOT, "dist");
const NAME = "ruyix";
const VERSION = (() => {
  const conf = JSON.parse(fs.readFileSync(path.join(ROOT, "src-tauri", "tauri.conf.json"), "utf8"));
  return conf.version;
})();
const STAGE = path.join(DIST, NAME);
const ZIP = path.join(
  DIST,
  `${NAME}-${VERSION}-win-x64${MODE === "pure" ? "-pure" : ""}.zip`
);
const NO_BUILD = process.argv.includes("--no-build");
// 两种编译模式（见 doc/highlight-plugins.md）：
//   preinstalled（默认）＝ 内置高亮解析器 + 预装高亮插件（首启物化到 plugins/highlight/）
//   pure               ＝ 一个解析器都不编、一个插件都不预装（plugins/ 空着交给用户）
const MODE = (() => {
  const i = process.argv.indexOf("--mode");
  const m = i >= 0 ? process.argv[i + 1] : "preinstalled";
  if (m !== "preinstalled" && m !== "pure") {
    console.error(`[package] --mode 只认 preinstalled / pure，收到 ${m}`);
    process.exit(2);
  }
  return m;
})();
// 纯净模式用独立 target 目录：两套特性的编译缓存互相顶掉的话，切模式每次都要全量重编
const TARGET_DIR = MODE === "pure" ? "target-pure" : "target";

const log = (m) => console.log(`[package] ${m}`);
const fail = (m) => {
  console.error(`[package] 失败：${m}`);
  process.exit(1);
};

function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { cwd: ROOT, stdio: "inherit", shell: false, ...opts });
  if (r.error) fail(`${cmd} 起不来：${r.error.message}`);
  if (r.status !== 0) fail(`${cmd} ${args.join(" ")} 退出码 ${r.status}`);
}

function sha256(file) {
  return crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function copyFile(from, to) {
  fs.mkdirSync(path.dirname(to), { recursive: true });
  fs.copyFileSync(from, to);
}

// ---------------------------------------------------------------- 1. 构建
if (NO_BUILD) {
  log("跳过构建（--no-build）");
} else {
  if (MODE === "pure") {
    // 纯净模式绕开 tauri CLI（它主要管打包），直接 cargo：少一个会漂移的中间层
    log("cargo build --release --no-default-features --features custom-protocol（纯净模式）");
    run("cargo", [
      "build",
      "-p",
      "ruyix",
      "--release",
      "--no-default-features",
      "--features",
      "custom-protocol",
      "--target-dir",
      TARGET_DIR,
    ]);
  } else {
    log("cargo tauri build --no-bundle ...（便携形态不做安装器）");
    run("cargo", ["tauri", "build", "--no-bundle"]);
  }
}

const exe = path.join(ROOT, TARGET_DIR, "release", `${NAME}.exe`);
if (!fs.existsSync(exe)) fail(`找不到 ${exe}`);
log(`exe = ${exe}（${(fs.statSync(exe).size / 1048576).toFixed(1)} MB）`);

// ---------------------------------------------------------------- 2. 组包
fs.rmSync(STAGE, { recursive: true, force: true });
fs.mkdirSync(STAGE, { recursive: true });
copyFile(exe, path.join(STAGE, `${NAME}.exe`));

// 预置目录与说明：用户拿到 zip 时就是这份形态（也可以只拷 exe，首启会自己长出来）。
// 两种模式的差别只在 exe 本身与 plugins/ 里有没有预装插件 —— 布局一个字不差。
//
// 说明文本与程序首启写的那份是**同一批文件**（`src-tauri/templates/*.md`，
// Rust 侧用 `include_str!` 读进去）。两处各写一份文案，迟早会漂移。
const PRESET = [
  ["global/README.md", "templates/global-README.md"],
  ["projects/README.md", "templates/projects-README.md"],
  ["plugins/README.md", "templates/plugins-README.md"],
  // GPU 加速插件：目录空着就是纯 CPU，所以包里预置的是一份**说明书**，用户往里拷 DLL 即可。
  ["plugins/gpu-asr/README.md", "templates/gpu-asr-README.md"],
];
// 忘了登记就是漏发：`templates/` 里每个 .md 都必须被 PRESET 用到（对比"目录列举"而不是写死个数）。
// 写死个数只会让加模板的人去改数字，抓不到"加了文件却没进包"。
{
  const tplDir = path.join(ROOT, "src-tauri", "templates");
  const used = new Set(PRESET.map((p) => p[1]));
  for (const f of fs.readdirSync(tplDir)) {
    if (!f.endsWith(".md")) continue;
    if (!used.has(path.posix.join("templates", f))) {
      fail(`templates/${f} 没有被 PRESET 用到 —— 它不会进发行包（要么加进 PRESET，要么删掉）`);
    }
  }
}
for (const [rel, tpl] of PRESET) {
  const from = path.join(ROOT, "src-tauri", tpl);
  if (!fs.existsSync(from)) fail(`模板缺失：${from}`);
  fs.mkdirSync(path.dirname(path.join(STAGE, rel)), { recursive: true });
  fs.copyFileSync(from, path.join(STAGE, rel));
}
for (const d of ["global/logs", "global/cache", "global/webview", "global/runs"]) {
  fs.mkdirSync(path.join(STAGE, d), { recursive: true });
}
copyFile(path.join(ROOT, "README.md"), path.join(STAGE, "README.md"));
copyFile(path.join(ROOT, "LICENSE"), path.join(STAGE, "LICENSE"));

// ---------------------------------------------------------------- 3. 校验和
const sumLines = [];
for (const rel of [`${NAME}.exe`, "README.md", "LICENSE", ...PRESET.map((p) => p[0])]) {
  sumLines.push(`${sha256(path.join(STAGE, rel))}  ${rel}`);
}
fs.writeFileSync(path.join(STAGE, "SHA256SUMS.txt"), sumLines.join("\n") + "\n", "utf8");
log(`校验和已写（${sumLines.length} 项）`);

// ---------------------------------------------------------------- 4. 打 zip + 自校验
fs.rmSync(ZIP, { force: true });
// 项目记忆是**核心模块**（不可插件化）：模型随包走 `global/memory/model/`。
// 模型文件不进 git（95.8MB 的二进制进历史就再也清不掉），所以这里是**取模型步骤**：
// 从 ModelScope 取（HF 兜底）→ 逐文件 sha256 落 model.json → 打包前校验。
if (process.argv.includes("--no-model")) {
  log("跳过取模型（--no-model）：包里将没有向量腿，记忆退化为纯词法检索");
} else {
  log("取嵌入模型 → global/memory/model/（记忆是核心模块，随包）");
  run("node", [
    path.join(ROOT, "scripts", "fetch-embed-model.mjs"),
    "--out",
    path.join(STAGE, "global", "memory", "model"),
  ]);
}

log(`打 zip → ${ZIP}`);
run("powershell", [
  "-NoProfile",
  "-Command",
  `Compress-Archive -Path '${STAGE}' -DestinationPath '${ZIP}' -Force`,
]);

// 自校验：解压到临时目录，逐项比对校验和（"命令没报错"不是判据，"解压后逐项对得上"才是）
const tmp = path.join(process.env.TEMP || process.env.TMP || ".", `ruyix-pack-check-${process.pid}`);
fs.rmSync(tmp, { recursive: true, force: true });
run("powershell", [
  "-NoProfile",
  "-Command",
  `Expand-Archive -Path '${ZIP}' -DestinationPath '${tmp}' -Force`,
]);
const unzipped = path.join(tmp, NAME);
let bad = [];
for (const line of fs.readFileSync(path.join(unzipped, "SHA256SUMS.txt"), "utf8").trim().split("\n")) {
  const [sum, rel] = line.split(/\s{2}/);
  const f = path.join(unzipped, rel);
  if (!fs.existsSync(f)) bad.push(`${rel} 缺失`);
  else if (sha256(f) !== sum) bad.push(`${rel} 校验和不一致`);
}
for (const d of ["global", "projects", "plugins"]) {
  if (!fs.existsSync(path.join(unzipped, d))) bad.push(`${d}/ 缺失`);
}
fs.rmSync(tmp, { recursive: true, force: true });
if (bad.length) fail(`解压自校验未过：${bad.join("；")}`);

const size = fs.statSync(ZIP).size;
log(`完成：${ZIP}（${(size / 1048576).toFixed(1)} MB，解压自校验通过）`);
log(`解压即用：双击 ${NAME}\\${NAME}.exe；卸载 = 删掉那个文件夹。`);
