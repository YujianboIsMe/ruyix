#!/usr/bin/env node
/**
 * P0 探针跑法（可重复）。零依赖，照 `scripts/check-style.js` 的风格。
 *
 * 跑两个探针的**全部臂**，把原始输出打出来，并按**预注册判据**给判定：
 *
 *  探针 ① WebView2 数据目录（`src-tauri/examples/webview_data_probe.rs`）
 *     - 指定臂 `--data-dir <tmp>`：预期 PASS（profile 落在指定目录 + %LOCALAPPDATA% 不新增 + 旧 profile 没被碰）
 *     - 对照臂 `--default`：预期"外溢"（%LOCALAPPDATA%\com.ruyix.code 被创建/被碰）
 *  探针 ② 单实例互斥体（`src-tauri/examples/instance_mutex_probe.rs`）
 *     - 同根并发两份：恰好一个 first_instance=true          判据 P2-A
 *     - 不同根并发两份（root 命名）：两个都 true              判据 P2-B
 *     - 不同根并发两份（global 命名 = 现状）：第二个 false     判据 P2-C（对照臂：证明现状必须改）
 *
 * 用法：`node scripts/p0-probes.mjs [--skip-build]`
 * 退出码 0 = 全部达预期。
 */
import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const ROOT = path.resolve(__dirname, '..');
const EX = path.join(ROOT, 'target', 'debug', 'examples');
const WV_PROBE = path.join(EX, 'webview_data_probe.exe');
const MUTEX_PROBE = path.join(EX, 'instance_mutex_probe.exe');
const PROBE_DATA = path.join(os.tmpdir(), 'ruyix-p0-probe-data');
const MUTEX_ROOT_A = ROOT;
const MUTEX_ROOT_B = path.join(ROOT, 'crates', 'harness-engine'); // 任意另一个"根"

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function start(exe, args) {
  const p = spawn(exe, args, { cwd: ROOT });
  let out = '';
  let err = '';
  p.stdout.on('data', (d) => (out += d));
  p.stderr.on('data', (d) => (err += d));
  const done = new Promise((res) => p.on('close', (code) => res({ code, out, err })));
  return { p, done, text: () => out };
}

function jsonOf(text) {
  // 证据行可能带前缀（探针 ① 打的是 `[probe] 证据 {...}`），所以取"最后一个含 { 的行"里
  // 从第一个 { 到最后一个 } 的子串。
  const lines = text.split(/\r?\n/).reverse();
  for (const line of lines) {
    const a = line.indexOf('{');
    const b = line.lastIndexOf('}');
    if (a >= 0 && b > a) {
      try {
        return JSON.parse(line.slice(a, b + 1));
      } catch {
        /* 继续往上找 */
      }
    }
  }
  return null;
}

const results = [];
function judge(criterion, ok, detail) {
  results.push({ criterion, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${criterion}  ${detail}`);
}

async function main() {
  if (!process.argv.includes('--skip-build')) {
    console.log('[build] cargo build -p ruyix --examples');
    const b = spawnSync('cargo', ['build', '-p', 'ruyix', '--examples'], {
      cwd: ROOT,
      stdio: 'inherit',
      shell: true,
    });
    if (b.status !== 0) {
      console.error('构建失败，退出');
      process.exit(2);
    }
  }

  // ---------------- 探针 ① ----------------
  console.log('\n===== 探针 ① WebView2 数据目录 =====');

  console.log('\n--- 指定臂：--data-dir ' + PROBE_DATA + ' ---');
  fs.rmSync(PROBE_DATA, { recursive: true, force: true });
  const wv = await start(WV_PROBE, ['--data-dir', PROBE_DATA]).done;
  console.log(wv.out.trim());
  if (wv.err.trim()) console.log('[stderr] ' + wv.err.trim());
  const e1 = jsonOf(wv.out);
  if (!e1) {
    judge('W1 指定臂产出证据', false, '没拿到 JSON 证据行');
  } else {
    judge('W1 profile 落在指定目录', e1.profile_created_at_target === true, e1.profile_path || '');
    judge(
      'W2 %LOCALAPPDATA% 无新增顶层目录',
      (e1.new_localappdata_entries || []).length === 0,
      JSON.stringify(e1.new_localappdata_entries),
    );
    judge(
      'W3 旧 profile（com.ruyix.code）没被碰',
      e1.localappdata_identifier_touched === false,
      'touched=' + e1.localappdata_identifier_touched,
    );
  }
  const inside = fs.existsSync(path.join(PROBE_DATA, 'EBWebView'));
  judge('W4 磁盘上确实有 <data-dir>/EBWebView', inside, PROBE_DATA);

  console.log('\n--- 对照臂：不指定（今日形态）---');
  const wv0 = await start(WV_PROBE, ['--default']).done;
  console.log(wv0.out.trim());
  const e0 = jsonOf(wv0.out);
  if (!e0) {
    judge('W5 对照臂产出证据', false, '没拿到 JSON 证据行');
  } else {
    judge(
      'W5 不指定 → 外溢到 %LOCALAPPDATA%\\com.ruyix.code（对照组必须成立）',
      e0.localappdata_identifier_touched === true || e0.profile_created_at_target === true,
      'touched=' + e0.localappdata_identifier_touched,
    );
  }

  // ---------------- 探针 ② ----------------
  console.log('\n===== 探针 ② 单实例互斥体按根区分 =====');

  console.log('\n--- 同根并发两份 ---');
  const a1 = start(MUTEX_PROBE, [MUTEX_ROOT_A, '--hold-secs', '6']);
  await sleep(1200);
  const a2 = await start(MUTEX_PROBE, [MUTEX_ROOT_A, '--hold-secs', '2']).done;
  const a1r = await a1.done;
  console.log(a1r.out.trim());
  console.log(a2.out.trim());
  const ja1 = jsonOf(a1r.out);
  const ja2 = jsonOf(a2.out);
  judge(
    'P2-A 同根：恰好一个 first_instance=true',
    !!ja1 && !!ja2 && ja1.first_instance !== ja2.first_instance,
    `first1=${ja1 && ja1.first_instance} first2=${ja2 && ja2.first_instance} name=${ja1 && ja1.mutex_name}`,
  );

  console.log('\n--- 不同根并发两份（root 命名）---');
  const b1 = start(MUTEX_PROBE, [MUTEX_ROOT_A, '--hold-secs', '6']);
  await sleep(1200);
  const b2 = await start(MUTEX_PROBE, [MUTEX_ROOT_B, '--hold-secs', '2']).done;
  const b1r = await b1.done;
  console.log(b1r.out.trim());
  console.log(b2.out.trim());
  const jb1 = jsonOf(b1r.out);
  const jb2 = jsonOf(b2.out);
  judge(
    'P2-B 不同根：两个都 first_instance=true',
    !!jb1 && !!jb2 && jb1.first_instance === true && jb2.first_instance === true,
    `${jb1 && jb1.mutex_name} vs ${jb2 && jb2.mutex_name}`,
  );

  console.log('\n--- 不同根并发两份（global 命名 = 现状，对照臂）---');
  const c1 = start(MUTEX_PROBE, [MUTEX_ROOT_A, '--hold-secs', '6', '--name-mode', 'global']);
  await sleep(1200);
  const c2 = await start(MUTEX_PROBE, [
    MUTEX_ROOT_B,
    '--hold-secs',
    '2',
    '--name-mode',
    'global',
  ]).done;
  const c1r = await c1.done;
  console.log(c1r.out.trim());
  console.log(c2.out.trim());
  const jc1 = jsonOf(c1r.out);
  const jc2 = jsonOf(c2.out);
  judge(
    'P2-C 对照臂：现状（写死名字）下不同根也撞',
    !!jc1 && !!jc2 && jc1.first_instance === true && jc2.first_instance === false,
    `first1=${jc1 && jc1.first_instance} first2=${jc2 && jc2.first_instance}`,
  );

  // ---------------- 汇总 ----------------
  const bad = results.filter((r) => !r.ok);
  console.log(`\n===== 汇总：${results.length - bad.length}/${results.length} 项达预期 =====`);
  for (const r of bad) console.log(`  FAIL ${r.criterion} ${r.detail}`);
  process.exit(bad.length ? 1 : 0);
}

main().catch((e) => {
  console.error(e);
  process.exit(3);
});
