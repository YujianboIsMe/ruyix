#!/usr/bin/env bash
# v1.4 P0 验收：① 可复现（同任务集连跑两遍读数逐项一致）② 真模型臂接线（1 任务 × 2 臂 × 1 轮，flash）
set -u
cd /d/Projects/Rust/ruyix || exit 1
EXE="$LOCALAPPDATA/Temp/rsi_bench_run.exe"
cp -f target/debug/examples/rsi_bench.exe "$EXE" || exit 1

echo "########## ① 可复现性：同一任务集连跑两遍（脚本化臂，零成本）"
for tag in a b; do
  out="D:/Projects/Rust/ruyix/target/rsi-rep-$tag"
  rm -rf "$out"
  "$EXE" --fake --rounds 3 --out "$out" > "target/rsi-rep-$tag.log" 2>&1
  echo "  第 $tag 遍：$(grep -c '判据通过\|负样本' "target/rsi-rep-$tag.log") 条有效记录"
done
python - <<'PY'
import json, pathlib
r = {}
for tag in "ab":
    d = json.loads((pathlib.Path("target") / f"rsi-rep-{tag}" / "receipt.json").read_text(encoding="utf-8"))
    r[tag] = {a["arm"]: (a["median_pct"], tuple(a["round_scores"]), a["config_hash"], a["valid"], a["invalid"]) for a in d["arms"]}
    r[tag]["_decision"] = d["decision"][:40]
    r[tag]["_hash"] = d["tasks_hash_before"][:16]
print("PASS 两遍读数逐项一致（中位数/逐轮/生效配置指纹/有效轮/账本裁决）" if r["a"] == r["b"] else f"FAIL 不一致\n {r['a']}\n {r['b']}")
print("   中位数：", {k: v[0] for k, v in r["a"].items() if not k.startswith("_")}, "· 考卷", r["a"]["_hash"])
PY

echo
echo "########## ② 真模型臂接线（flash，1 任务 × 2 臂 × 1 轮；只验「能真跑出读数」，不构成功效结论）"
CFG="D:/Projects/Rust/ruyix/target/debug/global/ai.toml"
if [ ! -f "$CFG" ]; then echo "SKIP 没有 $CFG（改用 DEEPSEEK_API_KEY 环境变量）"; fi
out="D:/Projects/Rust/ruyix/target/rsi-real"
rm -rf "$out"
"$EXE" --config "$CFG" --model deepseek-v4-flash --rounds 1 --task fix-add --out "$out" 2>&1 | tail -32
