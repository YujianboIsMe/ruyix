#!/usr/bin/env node
/**
 * 前端代码风格校验（Google JavaScript / HTML / CSS / JSON Style Guide 的机械可查项）
 *
 * 为什么是自研脚本：本项目禁止 npm 依赖（无打包器），装不了 eslint。
 * Rust 侧不归它管：直接 `cargo fmt --check`（Google 的 Rust 风格即 rustfmt 风格）。
 *
 * 用法：
 *   node scripts/check-style.js           # 全量检查，有 error 则退出码 1
 *   node scripts/check-style.js --quiet   # 只打印有问题的文件
 *   node scripts/check-style.js <path...>  # 只查指定文件
 *
 * 规则：E = error（阻断），W = warning（仅提示）
 *   E1 no-var            禁止 var，用 const / let
 *   E2 no-tabs           禁止制表符
 *   E3 indent-2          缩进为 2 的倍数（注释续行 ` * ` 除外）
 *   E4 no-trailing-space 行尾无多余空白
 *   E5 final-newline     文件以换行结尾
 *   E6 eqeqeq            用 === / !==（与 null 比较例外）
 *   E7 file-name         .js/.css 文件名小写连字符
 *   E8 json-indent       JSON 可解析且缩进 2 空格
 *   W1 line-length       单行 > 100 列（Google 建议 80，存量放宽为提示）
 *
 * 实现要点：先用 stripLiterals 把字符串/模板/正则/注释替换成空白，
 * 再在"代码骨架"上判定 —— 否则 JSDoc 里的 ` * `、正则里的 `==` 全会误报。
 */

const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "..");
const QUIET = process.argv.includes("--quiet");
const CLI_FILES = process.argv.slice(2).filter((a) => !a.startsWith("--"));

/** 第三方 vendored 库保持原样，不校验 */
const VENDORED = new Set(["ui/xterm.js", "ui/xterm.css"]);

const EXTS = [".js", ".css", ".html", ".json"];

function collectFiles() {
  if (CLI_FILES.length) return CLI_FILES.map((f) => path.relative(ROOT, path.resolve(f)).replace(/\\/g, "/"));

  const out = [];
  const walk = (dir) => {
    const abs = path.join(ROOT, dir);
    if (!fs.existsSync(abs)) return;
    for (const name of fs.readdirSync(abs)) {
      if (name === "node_modules" || name.startsWith(".")) continue;
      const rel = path.posix.join(dir, name);
      if (fs.statSync(path.join(ROOT, rel)).isDirectory()) walk(rel);
      else if (EXTS.includes(path.extname(name).toLowerCase())) out.push(rel);
    }
  };
  walk("ui");
  walk("scripts");
  return out;
}

/**
 * 把一行里的字符串/模板/正则/注释替换为等长空白，保留其它字符位置。
 * 返回 { code, block, template }：block/template 是跨行状态，供下一行续用
 */
function stripLiterals(line, state) {
  let out = "";
  let i = 0;
  let block = state.block;
  let template = state.template;
  let prevSignificant = ""; // 用于判断 `/` 是正则还是除号

  const blank = (ch) => (ch === "\t" ? "\t" : " ");

  while (i < line.length) {
    const ch = line[i];
    const next = line[i + 1];

    // 块注释（跨行）
    if (block) {
      if (ch === "*" && next === "/") {
        out += "  ";
        i += 2;
        block = false;
        continue;
      }
      out += blank(ch);
      i += 1;
      continue;
    }

    // 模板字符串（跨行；内容整段视为字符串）
    if (template) {
      if (ch === "\\") {
        out += "  ";
        i += 2;
        continue;
      }
      if (ch === "`") {
        out += " ";
        i += 1;
        template = false;
        continue;
      }
      out += blank(ch);
      i += 1;
      continue;
    }

    // 行注释：本行剩余全是注释
    if (ch === "/" && next === "/") {
      out += " ".repeat(line.length - i);
      break;
    }

    // 块注释开始
    if (ch === "/" && next === "*") {
      out += "  ";
      i += 2;
      block = true;
      continue;
    }

    // 字符串 / 模板开始
    if (ch === '"' || ch === "'" || ch === "`") {
      const quote = ch;
      out += " ";
      i += 1;
      while (i < line.length) {
        if (line[i] === "\\") {
          out += "  ";
          i += 2;
          continue;
        }
        if (line[i] === quote) {
          out += " ";
          i += 1;
          break;
        }
        out += blank(line[i]);
        i += 1;
      }
      if (quote === "`" && line[i - 1] !== "`") template = true; // 未闭合 → 跨行
      prevSignificant = quote;
      continue;
    }

    // 正则字面量：仅当上一个有意义字符表明"这里该是表达式"时
    if (ch === "/" && /^[(,=:[!&|?{};+\-*%~^]$/.test(prevSignificant)) {
      out += " ";
      i += 1;
      let inClass = false;
      while (i < line.length) {
        const c = line[i];
        if (c === "\\") {
          out += "  ";
          i += 2;
          continue;
        }
        if (c === "[") inClass = true;
        else if (c === "]") inClass = false;
        else if (c === "/" && !inClass) {
          out += " ";
          i += 1;
          break;
        }
        out += blank(c);
        i += 1;
      }
      while (i < line.length && /[a-z]/.test(line[i])) {
        out += " ";
        i += 1;
      }
      prevSignificant = "/";
      continue;
    }

    out += blank(ch);
    if (!/\s/.test(ch)) prevSignificant = ch;
    i += 1;
  }

  return { code: out, block, template };
}

function checkFile(rel) {
  const errors = [];
  const warns = [];
  if (VENDORED.has(rel)) return { errors, warns };

  const text = fs.readFileSync(path.join(ROOT, rel), "utf8");
  const rawLines = text.split(/\r?\n/);
  const isJson = rel.endsWith(".json");

  // E8：JSON 单独处理（解析 + 2 空格缩进）
  if (isJson) {
    try {
      JSON.parse(text);
    } catch (e) {
      errors.push(["E8 json-indent", `JSON 解析失败: ${e.message}`]);
    }
    rawLines.forEach((line, i) => {
      const indent = (line.match(/^ */) || [""])[0].length;
      if (line.trim() && indent % 2 !== 0) {
        errors.push(["E8 json-indent", `第 ${i + 1} 行缩进 ${indent} 空格，不是 2 的倍数`]);
      }
    });
    if (text.length > 0 && !text.endsWith("\n")) {
      errors.push(["E5 final-newline", "文件未以换行结尾"]);
    }
    return { errors, warns };
  }

  const isHtml = rel.endsWith(".html");
  let state = { block: false, template: false };
  let inTag = false;

  rawLines.forEach((line, i) => {
    const no = i + 1;
    const wasInTemplate = state.template;
    const startedInTag = inTag;

    const st = stripLiterals(line, state);
    state = { block: st.block, template: st.template };
    const code = st.code;

    // HTML：标签内的属性续行（对齐排版，Google 规范不禁止）跳过缩进检查
    if (isHtml) {
      for (const ch of code) {
        if (ch === "<") inTag = true;
        else if (ch === ">") inTag = false;
      }
    }

    // E1 禁止 var
    if (/\bvar\s+[A-Za-z_$]/.test(code)) {
      errors.push(["E1 no-var", `第 ${no} 行使用了 var，请改用 const / let`]);
    }

    // E2 制表符（看原始行）
    if (line.includes("\t")) {
      errors.push(["E2 no-tabs", `第 ${no} 行含制表符，请用 2 空格`]);
    }

    // E3 缩进为 2 的倍数。
    // 跳过三种"内容非代码"的情形：整行注释/注释续行、跨行模板字符串内部、HTML 标签内的属性续行
    const codeHasCode = /\S/.test(code);
    const indent = (line.match(/^ */) || [""])[0].length;
    if (codeHasCode && !wasInTemplate && !startedInTag && indent % 2 !== 0) {
      errors.push(["E3 indent-2", `第 ${no} 行缩进 ${indent} 空格，不是 2 的倍数`]);
    }

    // E4 行尾空白
    if (/[ \t]+$/.test(line)) {
      errors.push(["E4 no-trailing-space", `第 ${no} 行行尾有多余空白`]);
    }

    // E6 宽松相等（在与 null 比较时是 Google 允许的例外）
    const looseMatches = code.match(/[^=!<>]==[^=]|[^=!]!=[^=]/g) || [];
    for (const m of looseMatches) {
      const idx = code.indexOf(m);
      const around = line.slice(Math.max(0, idx - 8), idx + 12);
      if (/\bnull\b/.test(m) || /\bnull\b/.test(around)) continue;
      errors.push(["E6 eqeqeq", `第 ${no} 行使用了 ${m.trim()}，请用 === / !==`]);
      break;
    }

    // W1 行长度
    if (line.length > 100) {
      warns.push(["W1 line-length", `第 ${no} 行 ${line.length} 列（>100）`]);
    }
  });

  // E5 结尾换行
  if (text.length > 0 && !text.endsWith("\n")) {
    errors.push(["E5 final-newline", "文件未以换行结尾"]);
  }

  // E7 文件名
  const base = path.basename(rel);
  const stem = base.replace(/\.(js|css|json|html)$/, "");
  if (/\.(js|css)$/.test(base) && stem !== stem.toLowerCase()) {
    errors.push(["E7 file-name", `文件名应小写（${base}）`]);
  }
  if (/[A-Z_]/.test(stem)) {
    errors.push(["E7 file-name", `文件名建议小写连字符（${base}）`]);
  }

  return { errors, warns };
}

function main() {
  const files = collectFiles();
  let errorCount = 0;
  let warnCount = 0;
  const byRule = {};
  const details = [];

  for (const rel of files) {
    const { errors, warns } = checkFile(rel);
    errorCount += errors.length;
    warnCount += warns.length;
    for (const [rule] of errors) byRule[rule] = (byRule[rule] || 0) + 1;
    if (errors.length) details.push({ rel, errors });
  }

  if (!QUIET) console.log(`检查 ${files.length} 个文件：${errorCount} error, ${warnCount} warning`);

  for (const { rel, errors } of details) {
    console.log(`\n${rel}`);
    for (const [rule, msg] of errors) console.log(`  E ${rule}  ${msg}`);
  }

  if (!QUIET) {
    const rules = Object.keys(byRule).sort();
    if (rules.length) {
      console.log("");
      for (const rule of rules) console.log(`  ${rule}: ${byRule[rule]}`);
    }
  }

  if (errorCount > 0) {
    console.log(`\nFAIL: ${errorCount} 个 error 需要修复`);
    process.exit(1);
  }
  console.log("\nPASS: 前端风格检查通过");
}

main();
