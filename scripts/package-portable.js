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
const ZIP = path.join(DIST, `${NAME}-${VERSION}-win-x64.zip`);
const NO_BUILD = process.argv.includes("--no-build");

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
  log("cargo tauri build --no-bundle ...（便携形态不做安装器）");
  run("cargo", ["tauri", "build", "--no-bundle"]);
}

const exe = path.join(ROOT, "target", "release", `${NAME}.exe`);
if (!fs.existsSync(exe)) fail(`找不到 ${exe}`);
log(`exe = ${exe}（${(fs.statSync(exe).size / 1048576).toFixed(1)} MB）`);

// ---------------------------------------------------------------- 2. 组包
fs.rmSync(STAGE, { recursive: true, force: true });
fs.mkdirSync(STAGE, { recursive: true });
copyFile(exe, path.join(STAGE, `${NAME}.exe`));

// 预置目录与说明：用户拿到 zip 时就是这份形态（也可以只拷 exe，首启会自己长出来）
//
// 说明文本与程序首启写的那份是**同一批文件**（`src-tauri/templates/*.md`，
// Rust 侧用 `include_str!` 读进去）。两处各写一份文案，迟早会漂移。
const PRESET = [
  ["global/README.md", "templates/global-README.md"],
  ["projects/README.md", "templates/projects-README.md"],
  ["plugins/README.md", "templates/plugins-README.md"],
];
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
