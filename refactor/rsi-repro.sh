#!/usr/bin/env bash
# v1.4 P0 的可复现性判据：同一任务集连跑两遍，逐轮得分/中位数/裁决必须一致。
set -u
cd /d/Projects/Rust/ruyix || exit 1
EXE="$LOCALAPPDATA/Temp/rsi_bench_run.exe"
cp -f target/debug/examples/rsi_bench.exe "$EXE" || exit 1
for tag in a b; do
  out="target/rsi-bench-rep-$tag"
  rm -rf "$out"
  echo "############ 第 $tag 次（$out）"
  "$EXE" --fake --rounds 3 --out "$out" 2>&1 | sed -n '/## 逐臂读数/,/^$/p;/## 裁决/,/^$/p'
  python - "$out" <<'PY'
import json, sys, pathlib
p = pathlib.Path(sys.argv[1]) / "receipt.json"
d = json.loads(p.read_text(encoding="utf-8"))
for a in d["arms"]:
    print("  ", a["arm"], "median", a["median_pct"], "rounds", a["round_scores"], "valid", a["valid"], "invalid", a["invalid"], "cfg", a["config_hash"][:8])
print("   decision:", d["decision"].splitlines()[0][:80])
print("   tasks_hash:", d["tasks_hash_before"][:16], "->", d["tasks_hash_after"][:16])
PY
done
echo "############ 两次的中位数是否一致（同一任务集、同一臂、同一模型=脚本化）"
python - <<'PY'
import json, pathlib
r = {}
for tag in "ab":
    d = json.loads((pathlib.Path("target") / f"rsi-bench-rep-{tag}" / "receipt.json").read_text(encoding="utf-8"))
    r[tag] = {a["arm"]: (a["median_pct"], tuple(a["round_scores"]), a["config_hash"]) for a in d["arms"]}
same = r["a"] == r["b"]
print("PASS 两次读数逐项一致" if same else f"FAIL 不一致：{r}")
PY
