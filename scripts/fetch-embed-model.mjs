// 取模型步骤（**模型文件不进 git**，这一步负责把它弄到发行包/开发目录里）。
//
// 为什么是"步骤"而不是"把 95.8MB 提交进仓库"：
//   1. 仓库是**源码**：95.8MB 的二进制进去就再也清不掉了（历史里永远躺着）；
//   2. 模型有**来源与校验**：从 ModelScope/HuggingFace 取，逐文件 sha256 落 `model.json`，
//      使"包里这份模型到底是什么"变成可核对的事实（而不是"某个 commit 里那个文件"）；
//   3. 记忆是**核心模块**（不可插件化）：所以它的模型跟着发行包走 `global/memory/model/`，
//      不放在插件体系里 —— 但"怎么来的"仍然是这一步的事。
//
// 用法：
//   node scripts/fetch-embed-model.mjs                      # 默认下到 dist/ruyix/global/memory/model/
//   node scripts/fetch-embed-model.mjs --out D:\Models      # 下到开发机目录（目录下会建 bge-small-zh-v1.5/）
//   node scripts/fetch-embed-model.mjs --force              # 已存在也重下
//   node scripts/fetch-embed-model.mjs --check <dir>        # 只校验：文件齐不齐、sha256 与 model.json 对不对
//
// 判据（失败即退出码 1，并打印原始证据）：
//   F1 三个必需文件都到（config.json / tokenizer.json / model.safetensors）且大小与来源清单一致；
//   F2 `config.json` 能解析、`tokenizer.json` 能解析（坏文件不许当成"下好了"）；
//   F3 `model.safetensors` 的头部合法（8 字节小端头长 + JSON 头，第 9 个字节是 `{`）；
//   F4 写 `model.json`（来源 URL + 每文件 sha256 + 体积），二次运行**幂等**：文件在且校验过就不重下。
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";

const MODEL = "BAAI/bge-small-zh-v1.5";
const DIR_NAME = "bge-small-zh-v1.5";
const FILES = [
  { name: "config.json", min: 300 },
  { name: "tokenizer.json", min: 100_000 },
  { name: "model.safetensors", min: 50_000_000 },
  { name: "vocab.txt", min: 50_000, optional: true },
];

// 两个来源：ModelScope 优先（本机实测可达、国内快），HuggingFace 兜底
const SOURCES = [
  { tag: "modelscope", url: (f) => `https://www.modelscope.cn/api/v1/models/${MODEL}/repo?Revision=master&FilePath=${f}` },
  { tag: "huggingface", url: (f) => `https://huggingface.co/${MODEL}/resolve/main/${f}` },
];

const args = process.argv.slice(2);
const flag = (n) => args.includes(n);
const val = (n, d) => {
  const i = args.indexOf(n);
  return i >= 0 ? args[i + 1] : d;
};

const OUT = path.resolve(val("--out", path.join(import.meta.dirname, "..", "dist", "ruyix", "global", "memory", "model")));
const DIR = path.join(OUT, DIR_NAME);
const CHECK_ONLY = flag("--check") ? path.resolve(val("--check", DIR)) : null;
const FORCE = flag("--force");

const log = (m) => console.log(`[model] ${m}`);
const bad = [];
const check = (name, ok, detail = "") => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? "  " + detail : ""}`);
  if (!ok) bad.push(name);
};

function sha256(p) {
  return crypto.createHash("sha256").update(fs.readFileSync(p)).digest("hex");
}

/** 用系统 curl 下载 —— Windows 自带 curl，零依赖（不引 node-fetch）。 */
function download(url, dest) {
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  const tmp = dest + ".part";
  execFileSync("curl", ["-sSL", "--fail", "--max-time", "900", "-o", tmp, url], { stdio: ["ignore", "ignore", "pipe"] });
  const size = fs.statSync(tmp).size;
  fs.renameSync(tmp, dest);
  return size;
}

async function fetchAll(target) {
  const manifestPath = path.join(target, "model.json");
  const prev = fs.existsSync(manifestPath) ? JSON.parse(fs.readFileSync(manifestPath, "utf8")) : null;
  const man = { model: MODEL, fetched_at: new Date().toISOString(), files: {} };

  for (const f of FILES) {
    const dest = path.join(target, f.name);
    if (!FORCE && fs.existsSync(dest) && fs.statSync(dest).size >= f.min) {
      const h = sha256(dest);
      const known = prev?.files?.[f.name]?.sha256;
      if (known && known !== h) {
        check(`F1 ${f.name} 与上次记录一致`, false, `sha256 变了：${known.slice(0, 12)} → ${h.slice(0, 12)}`);
      } else {
        man.files[f.name] = { sha256: h, bytes: fs.statSync(dest).size, source: prev?.files?.[f.name]?.source || "cache" };
        log(`${f.name} 已在（${(fs.statSync(dest).size / 1e6).toFixed(1)} MB），跳过`);
        continue;
      }
    }
    let ok = false;
    for (const s of SOURCES) {
      const url = s.url(f.name);
      try {
        log(`下载 ${f.name} ← ${s.tag}`);
        const size = download(url, dest);
        if (size < f.min) {
          log(`${s.tag} 下到的 ${f.name} 只有 ${size} 字节（疑似错误页），换下一个来源`);
          continue;
        }
        man.files[f.name] = { sha256: sha256(dest), bytes: size, source: url };
        log(`${f.name} 完成（${(size / 1e6).toFixed(1)} MB）`);
        ok = true;
        break;
      } catch (e) {
        log(`${s.tag} 失败：${String(e.message || e).slice(0, 120)}`);
      }
    }
    if (!ok && !f.optional) check(`F1 ${f.name} 落地`, false, "两个来源都没成功");
  }
  fs.writeFileSync(manifestPath, JSON.stringify(man, null, 2) + "\n");
  log(`来源与校验写入 ${manifestPath}`);
}

function verify(target) {
  for (const f of FILES) {
    const p = path.join(target, f.name);
    if (!fs.existsSync(p)) {
      if (!f.optional) check(`F1 ${f.name} 存在`, false, p);
      continue;
    }
    const size = fs.statSync(p).size;
    check(`F1 ${f.name} 体积合理`, size >= f.min, `${(size / 1e6).toFixed(1)} MB`);
  }
  for (const j of ["config.json", "tokenizer.json"]) {
    const p = path.join(target, j);
    if (!fs.existsSync(p)) continue;
    try {
      JSON.parse(fs.readFileSync(p, "utf8"));
      check(`F2 ${j} 可解析`, true, "");
    } catch (e) {
      check(`F2 ${j} 可解析`, false, String(e.message).slice(0, 80));
    }
  }
  const sp = path.join(target, "model.safetensors");
  if (fs.existsSync(sp)) {
    const fd = fs.openSync(sp, "r");
    const head = Buffer.alloc(9);
    fs.readSync(fd, head, 0, 9, 0);
    fs.closeSync(fd);
    const hlen = head.readBigUInt64LE(0);
    check("F3 safetensors 头部合法", head[8] === 0x7b && hlen > 2n, `头长=${hlen} 首字节=${String.fromCharCode(head[8])}`);
  }
  const mp = path.join(target, "model.json");
  if (fs.existsSync(mp)) {
    const man = JSON.parse(fs.readFileSync(mp, "utf8"));
    for (const [name, rec] of Object.entries(man.files)) {
      const p = path.join(target, name);
      if (!fs.existsSync(p)) continue;
      check(`F4 ${name} sha256 未变`, sha256(p) === rec.sha256, rec.sha256.slice(0, 12));
    }
  } else {
    log("没有 model.json（首次下载会生成）");
  }
}

console.log(`[model] ${MODEL} → ${CHECK_ONLY || DIR}`);
if (CHECK_ONLY) {
  verify(CHECK_ONLY);
} else {
  fs.mkdirSync(DIR, { recursive: true });
  await fetchAll(DIR);
  verify(DIR);
}
console.log(bad.length ? `\n=== 取模型：${bad.length} 条未达预期 ===` : "\n=== 取模型：全部达预期 ===");
if (bad.length) {
  for (const b of bad) console.log("  " + b);
  process.exit(1);
}
