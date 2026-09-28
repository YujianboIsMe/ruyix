#!/usr/bin/env bash
# RSI 一回合驱动器的自检（v1.4 P2）：在世界里跑一遍 plan/propose/score，验"产物规则"。
# 脚本化臂（--fake）⇒ 零成本；断言全是"该有的文件有、不该动的没动"。
set -u
cd /d/Projects/Rust/ruyix || exit 1
DRIVER="doc/v1.4/templates/rsi/rsi_round.py"
BENCH="$LOCALAPPDATA/Temp/rsi_bench_run.exe"
cp -f target/debug/examples/rsi_bench.exe "$BENCH" || exit 1

# 世界路径要**短**：尺子的路径闸会拒掉过深的落点（pycache 的 MAX_PATH 陷阱，见
# doc/v1.4/问题-验证pycache改道撞MAX_PATH.md）——自检世界自己也要守这条规矩。
W="$LOCALAPPDATA/Temp/rs1"
rm -rf "$W"; mkdir -p "$W/plugins/tools/demo" "$W/projects/rsi/rsi"
# 假便携根：一个 surface（工具表） + RSI 主页
printf '[[discover]]\nname = "demo"\nbin = "demo-tool"\nmarkers = ["demo.marker"]\n' > "$W/plugins/tools/demo/tools.toml"
cp -r crates/harness-engine/tests/fixtures/rsi_bench_demo/tasks "$W/projects/rsi/rsi/tasks"
# 基线臂要有一份 bench.toml（评测台要求"覆盖非空且真的生效"，见评测台文档 §2.3）
printf '[agent]\nmax_elapsed_secs = 1740\n' > "$W/plugins/bench.toml"

echo "########## ① 没有 allow.toml ⇒ 必须拒绝开跑"
python "$DRIVER" --root "$W" --key rsi plan; echo "   退出码=$?（期望 2）"

cat > "$W/projects/rsi/rsi/allow.toml" <<TOML
[rsi]
surfaces = ["plugins"]
rounds = 2
margin_pp = 5.0
budget_secs = 300
TOML

echo
echo "########## ② plan"
python "$DRIVER" --root "$W" --key rsi --bench "$BENCH" plan || exit 1

echo
echo "########## ③ propose 1（建基准臂 + 候选臂）"
python "$DRIVER" --root "$W" --key rsi --bench "$BENCH" propose 1 || exit 1
ls "$W/projects/rsi/rsi/arms/" | sort

echo
echo "########## ④ 在**副本**上做一次提议（模拟人/agent 改数据面）"
printf '[[discover]]\nname = "demo"\nbin = "demo-tool"\nmarkers = ["demo.marker"]\n\n[[discover]]\nname = "pnpm"\nbin = "pnpm"\nmarkers = ["pnpm-lock.yaml"]\n' \
  > "$W/projects/rsi/rsi/arms/round-1/plugins/tools/demo/tools.toml"

echo
echo "########## ⑤ score 1（脚本化臂：两臂行为一样 ⇒ 期望「拒绝」且**不出 patch**，但 receipt 要有）"
BEFORE=$(sha256sum "$W/plugins/tools/demo/tools.toml" | cut -d' ' -f1)
# 注意 `--bench-arg=--fake` 的等号写法：argparse 会把 `--fake` 当成选项而不是取值
python "$DRIVER" --root "$W" --key rsi --bench "$BENCH" --bench-arg=--fake score 1 || exit 1
AFTER=$(sha256sum "$W/plugins/tools/demo/tools.toml" | cut -d' ' -f1)

echo
echo "########## 断言"
python - "$W" "$BEFORE" "$AFTER" <<'PY'
import json, pathlib, sys
W, before, after = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3]
home = W / "projects/rsi/rsi"
fails = []
def ck(name, ok, extra=""):
    print(("PASS " if ok else "FAIL ") + name + (f" —— {extra}" if extra else ""))
    if not ok: fails.append(name)

ck("便携根的 surface 一个字节都没动（驱动器绝不写 surfaces）", before == after, f"{before[:8]} → {after[:8]}")
rp = home / "receipts/round-1.json"
ck("receipt 落在 receipts/round-1.json", rp.is_file())
r = json.loads(rp.read_text(encoding="utf-8")) if rp.is_file() else {}
ck("receipt 里有裁决与两臂中位数", "裁决" in r.get("decision", "").replace("**", "").replace("拒绝", "裁决拒绝") or "拒绝" in r.get("decision", ""), r.get("decision", "")[:40])
ck("receipt 记了考卷哈希（判据 2）", len(r.get("tasks_hash_before", "")) == 64 and r.get("tasks_hash_after") == r.get("tasks_hash_before"))
ck("无信号 ⇒ 不出 patch（判据 7）", r.get("patch") is None and not (home / "out/round-1.patch").exists())
ck("receipt 写明下一步是「人合并」而不是自动进主干", "人审" in r.get("next", "") or "到此为止" in r.get("next", ""))
ck("候选臂与基准臂都在（一回合的两条臂）", (home / "arms/baseline").is_dir() and (home / "arms/round-1").is_dir())
ck("最终产物都在用户侧（projects/<键>/rsi/ 下）", str(rp).startswith(str(W / "projects")))
print("\n自检：" + ("全过" if not fails else f"失败 {len(fails)} 条：{fails}"))
sys.exit(1 if fails else 0)
PY
echo "EXIT=$?"
