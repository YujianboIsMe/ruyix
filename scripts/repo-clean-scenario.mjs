#!/usr/bin/env node
/**
 * A3/A4「用户仓库零写入」场景 —— 快照 + 判定，可重复跑。
 *
 * 为什么要有它：v1.0.0 最值钱的判据是"跑完一轮 agent，用户仓库里没有 `.ruyix`、
 * `git status` 只剩用户自己的改动"（需求 A3/A4）。这条判据必须**跑真场景**才算数，
 * 而"跑"之前要先把"看什么"钉死 —— 就是这个脚本。
 *
 * 场景（在 before 与 after 之间人工或用 CDP 驱动，见 doc/排期-1.0.0.md P0）：
 *   1. 打开项目
 *   2. 让 agent 干一件会产生暂存的活（确认模式）→ 差异面板出现
 *   3. 确认写回（点 data-confirm）
 *   4. 起一个托管服务（execute background）→ 看服务面板
 *   5. 跑一次验证（交付前门禁）→ 看 验证/复核 区
 *   6. 收尾：关掉服务、切走标签
 *
 * 用法：
 *   node scripts/repo-clean-scenario.mjs --project <项目根> --phase before
 *   ...（跑场景）...
 *   node scripts/repo-clean-scenario.mjs --project <项目根> --phase after
 *   # 判定在 after 这一跑里自动给出
 *   node scripts/repo-clean-scenario.mjs --selftest     # 自检判定逻辑（正例 + 反例）
 *
 * 纪律：**快照写在项目之外**（默认 %TEMP%/ruyix-repo-clean-scenario）——
 * 一个用来证明"零写入"的工具，自己先不能往仓库里写。
 */
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const OUT_DEFAULT = path.join(os.tmpdir(), 'ruyix-repo-clean-scenario');
const HEAVY = new Set(['.git', 'node_modules', 'target', '.venv', 'venv', '__pycache__', 'dist']);

function arg(name, def = null) {
  const i = process.argv.indexOf('--' + name);
  if (i < 0) return def;
  const v = process.argv[i + 1];
  return v && !v.startsWith('--') ? v : true;
}

function git(cwd, args) {
  const r = spawnSync('git', ['-c', 'core.quotepath=false', ...args], {
    cwd,
    encoding: 'utf8',
  });
  return { code: r.status, out: (r.stdout || '').replace(/\r\n/g, '\n') };
}

/** git status --porcelain 的原始行（含未跟踪） */
function statusLines(project) {
  const r = git(project, ['status', '--porcelain']);
  if (r.code !== 0) return null;
  return r.out.split('\n').filter((l) => l.length > 0);
}

/** 项目里所有叫 .ruyix 的目录（跳过重型目录，但绝不跳过 .ruyix 自身） */
function findRuyixDirs(project) {
  const hits = [];
  const walk = (dir, depth) => {
    let entries;
    try {
      entries = fs.readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of entries) {
      if (!e.isDirectory()) continue;
      const full = path.join(dir, e.name);
      if (e.name === '.ruyix') {
        hits.push(path.relative(project, full));
        continue; // 不往里走
      }
      if (HEAVY.has(e.name) || depth > 6) continue;
      walk(full, depth + 1);
    }
  };
  walk(project, 0);
  return hits;
}

function snapshot(project) {
  return {
    project,
    at: new Date().toISOString(),
    head: git(project, ['rev-parse', 'HEAD']).out.trim(),
    status: statusLines(project),
    ruyix_dirs: findRuyixDirs(project),
  };
}

function key(project) {
  return project.replace(/[\\/:]/g, '_');
}

function writeSnap(outDir, project, phase, snap) {
  fs.mkdirSync(outDir, { recursive: true });
  const file = path.join(outDir, `${key(project)}.${phase}.json`);
  fs.writeFileSync(file, JSON.stringify(snap, null, 2), 'utf8');
  return file;
}

function verdict(before, after) {
  const b = new Set(before.status || []);
  const a = new Set(after.status || []);
  const added = [...a].filter((x) => !b.has(x));
  const removed = [...b].filter((x) => !a.has(x));
  const lines = [];
  let ok = true;

  lines.push(`  git status: before ${b.size} 项 → after ${a.size} 项`);
  if (added.length) {
    ok = false;
    lines.push('  ❌ 多出来的（= IDE 写进用户仓库的东西）：');
    for (const x of added) lines.push('      ' + x);
  } else {
    lines.push('  ✅ 没有多出任何一条（用户的改动集合没被扩大）');
  }
  if (removed.length) {
    lines.push('  ℹ 少了的（用户自己在这期间改回/提交了？）：');
    for (const x of removed) lines.push('      ' + x);
  }
  if ((after.ruyix_dirs || []).length) {
    ok = false;
    lines.push('  ❌ 项目里出现 .ruyix：' + after.ruyix_dirs.join(', '));
  } else {
    lines.push('  ✅ 项目里没有 .ruyix');
  }
  return { ok, lines };
}

function runScenario(project, phase, outDir) {
  if (outDir.startsWith(project)) {
    console.error('拒绝：快照目录不能放在被观测的项目里（那本身就是一次写入）');
    process.exit(2);
  }
  const snap = snapshot(project);
  if (snap.status === null) {
    console.error(`${project} 不是 git 仓库（A3/A4 判据需要 git status）`);
    process.exit(2);
  }
  const file = writeSnap(outDir, project, phase, snap);
  console.log(`[${phase}] 快照 → ${file}`);
  console.log(`  HEAD=${snap.head.slice(0, 8)}  git status ${snap.status.length} 项`);
  console.log('  --- git status --porcelain（原始）---');
  for (const l of snap.status) console.log('    ' + l);
  console.log(`  --- find -name .ruyix：${snap.ruyix_dirs.length} 处 ---`);
  for (const d of snap.ruyix_dirs) console.log('    ' + d);

  if (phase !== 'after') {
    console.log('\n接下来跑场景（打开项目 → agent 暂存 → 确认写回 → 起服务 → 验证 → 收尾），');
    console.log('然后用 --phase after 收尾判定。');
    return;
  }
  const beforeFile = path.join(outDir, `${key(project)}.before.json`);
  if (!fs.existsSync(beforeFile)) {
    console.error('\n找不到 before 快照，无法判定：' + beforeFile);
    process.exit(2);
  }
  const before = JSON.parse(fs.readFileSync(beforeFile, 'utf8'));
  const v = verdict(before, snap);
  console.log('\n===== A3/A4 判定 =====');
  for (const l of v.lines) console.log(l);
  console.log(v.ok ? '\nPASS  A3/A4：用户仓库零写入' : '\nFAIL  A3/A4：仓库被写了');
  process.exit(v.ok ? 0 : 1);
}

/** 自检：判定逻辑对"没写"放行、对"写了 .ruyix"报警（正例 + 反例） */
function selftest() {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'ruyix-scenario-selftest-'));
  const proj = path.join(tmp, 'demo');
  const out = path.join(tmp, 'snaps');
  fs.mkdirSync(proj, { recursive: true });
  const run = (args) => spawnSync('git', args, { cwd: proj, encoding: 'utf8' });
  run(['init', '-q']);
  run(['config', 'user.email', 'probe@local']);
  run(['config', 'user.name', 'probe']);
  fs.writeFileSync(path.join(proj, 'a.txt'), 'hello\n');
  run(['add', 'a.txt']);
  run(['commit', '-qm', 'init']);

  console.log('--- 自检 1/2：场景没写任何东西（期望 PASS）---');
  const snap0 = snapshot(proj);
  writeSnap(out, proj, 'before', snap0);
  const v1 = verdict(snap0, snapshot(proj));
  for (const l of v1.lines) console.log(l);

  console.log('\n--- 自检 2/2：IDE 往仓库写了 .ruyix（期望 FAIL）---');
  fs.mkdirSync(path.join(proj, '.ruyix', 'stage'), { recursive: true });
  fs.writeFileSync(path.join(proj, '.ruyix', 'stage', 'x.json'), '{}\n');
  const v2 = verdict(snap0, snapshot(proj));
  for (const l of v2.lines) console.log(l);

  fs.rmSync(tmp, { recursive: true, force: true });
  const ok = v1.ok === true && v2.ok === false;
  console.log(ok ? '\nPASS  自检：正例放行、反例报红' : '\nFAIL  自检：判定逻辑有问题');
  process.exit(ok ? 0 : 1);
}

const projectArg = arg('project');
if (process.argv.includes('--selftest')) {
  selftest();
} else if (typeof projectArg === 'string') {
  const phase = arg('phase', 'before');
  const outDir = arg('out', OUT_DEFAULT);
  runScenario(path.resolve(projectArg), phase, typeof outDir === 'string' ? outDir : OUT_DEFAULT);
} else {
  console.error('用法：node scripts/repo-clean-scenario.mjs --project <项目根> --phase before|after [--out <目录>]');
  console.error('   或：node scripts/repo-clean-scenario.mjs --selftest');
  process.exit(2);
}
