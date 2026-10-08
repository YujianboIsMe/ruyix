#!/usr/bin/env bash
# RSI 一回合驱动器的自检（v1.4 P2）：在世界里跑一遍 plan/propose/score，验"产物规则"。
# 脚本化臂（--fake）⇒ 零成本；断言全是"该有的文件有、不该动的没动"。
#
# 世界路径与任务集都刻意**短**：尺子的路径闸会拒掉过深的落点（pycache 的 MAX_PATH 陷阱，
# 见 doc/v1.4/问题-验证pycache改道撞MAX_PATH.md）。真实便携根（`D:\Tools\ruyix`）下
# `projects/rsi/arms` 约 32 字符够用；`%TEMP%` 里再叠 RSI 的层级就得靠短任务名让路。
set -u
cd /d/Projects/Rust/ruyix || exit 1
DRIVER="doc/v1.4/templates/rsi/rsi_round.py"
BENCH="$LOCALAPPDATA/Temp/rsi_bench_run.exe"
cp -f target/debug/examples/rsi_bench.exe "$BENCH" || exit 1

W="$LOCALAPPDATA/Temp/rs1"
H="$W/projects/rsi/rsi"
rm -rf "$W"; mkdir -p "$W/plugins/tools/demo" "$H/tasks/t1"

# 假便携根：一个 surface（工具表）
printf '[[discover]]\nname = "demo"\nbin = "demo-tool"\nmarkers = ["demo.marker"]\n' > "$W/plugins/tools/demo/tools.toml"
# 极简任务集（1 个正样本）：任务名短 ⇒ 尺子的路径闸过得去
cat > "$H/tasks/t1/task.toml" <<'TOML'
prompt = "把 a.py 的 n 设成 1。"
notes = "自检用的最小任务"
[judge]
cmd = '''python -c "import a; assert a.n == 1"'''
timeout_secs = 30
[fake]
final_text = "done"
[[fake.turn]]
write = [{ path = "a.py", content = """
n = 1
""" }]
TOML

echo "########## ① 没有 allow.toml ⇒ 必须拒绝开跑"
python "$DRIVER" --root "$W" --key rsi --bench "$BENCH" plan
echo "   退出码=$?（期望 2）"

cat > "$H/allow.toml" <<'TOML'
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
ls "$H/arms/"

echo
echo "########## ④ 在**副本**上做一次提议（模拟人/agent 改数据面）"
printf '[[discover]]\nname = "demo"\nbin = "demo-tool"\nmarkers = ["demo.marker"]\n\n[[discover]]\nname = "pnpm"\nbin = "pnpm"\nmarkers = ["pnpm-lock.yaml"]\n' \
  > "$H/arms/round-1/plugins/tools/demo/tools.toml"

echo
echo "########## ⑤ score 1（两臂行为一样 ⇒ 期望裁决「拒绝」且**不出 patch**，但 receipt 要有）"
BEFORE=$(sha256sum "$W/plugins/tools/demo/tools.toml" | cut -d' ' -f1)
# `--bench-arg=--fake` 用等号：argparse 会把裸 `--fake` 当成选项而不是取值
python "$DRIVER" --root "$W" --key rsi --bench "$BENCH" --bench-arg=--fake score 1 || exit 1
AFTER=$(sha256sum "$W/plugins/tools/demo/tools.toml" | cut -d' ' -f1)

echo
echo "########## 断言"
python - "$W" "$BEFORE" "$AFTER" <<'PY'
import importlib.util, json, pathlib, sys
W, before, after = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3]
home = W / "projects/rsi/rsi"
fails = []
def ck(name, ok, extra=""):
    print(("PASS " if ok else "FAIL ") + name + (f" —— {extra}" if extra else ""))
    if not ok:
        fails.append(name)

ck("便携根的 surface 一个字节都没动（驱动器绝不写 surfaces）", before == after, f"{before[:8]} → {after[:8]}")
rp = home / "receipts/round-1.json"
r = json.loads(rp.read_text(encoding="utf-8")) if rp.is_file() else {}
ck("receipt 落在 receipts/round-1.json 且是一份完整账", rp.is_file() and r.get("kind") == "rsi-round-receipt")
ck("receipt 里有裁决", "拒绝" in r.get("decision", ""), r.get("decision", "")[:44])
ck("receipt 记了考卷哈希且跑前跑后一致（判据 2）",
   len(r.get("tasks_hash_before", "")) == 64 and r.get("tasks_hash_after") == r.get("tasks_hash_before"))
ck("receipt 记了两臂中位数", set(r.get("median_pct", {})) == {"baseline", "round-1"}, str(r.get("median_pct")))
ck("无信号 ⇒ 不出 patch（判据 7）", r.get("patch") is None and not (home / "out/round-1.patch").exists())
ck("receipt 写明下一步（人审 / 到此为止），不是自动进主干",
   ("人审" in r.get("next", "")) or ("到此为止" in r.get("next", "")))
ck("一回合的两条臂都在（baseline + round-1）",
   (home / "arms/baseline").is_dir() and (home / "arms/round-1").is_dir())
ck("候选臂与基准臂的数据面**确实不同**（提议落进了副本）",
   (home / "arms/baseline/plugins/tools/demo/tools.toml").read_text(encoding="utf-8")
   != (home / "arms/round-1/plugins/tools/demo/tools.toml").read_text(encoding="utf-8"))
ck("产物全在用户侧（projects/<键>/rsi/ 下）", str(rp).startswith(str(W / "projects")))
# A-6（2026-10-08 拍板：不补 execute 闸、靠副本隔离）——判据是 receipt 能证明「这一轮在副本里跑」
sh = r.get("surfaces_hash", {}).get("plugins", {})
ck("receipt 逐面记了三方哈希（原件 / 基准臂 / 候选臂）",
   set(sh) == {"portable_root", "baseline", "candidate"}, str(sorted(sh)))
ck("便携根原件 == 基准臂（基准臂是原件的副本）", sh.get("portable_root") == sh.get("baseline"))
ck("候选臂 ≠ 基准臂（提议落在副本里，原件不受影响）", sh.get("candidate") != sh.get("baseline"))
spec = importlib.util.spec_from_file_location("rsi_round", "doc/v1.4/templates/rsi/rsi_round.py")
m = importlib.util.module_from_spec(spec)
sys.dont_write_bytecode = True
spec.loader.exec_module(m)
ck("receipt 里的原件哈希可复算（不是编的）",
   sh.get("portable_root") == m.sha256_tree(W / "plugins")[0])
print("\n自检：" + ("全过" if not fails else f"失败 {len(fails)} 条：{fails}"))
sys.exit(1 if fails else 0)
PY
echo
echo
echo "########## ⑥ patch 生成器（「候选更优」那一支出产物的路径，单独验）"
python - "$W" <<'PY'
import importlib.util, pathlib, sys

sys.dont_write_bytecode = True  # import 模板别在仓库里留下 __pycache__
spec = importlib.util.spec_from_file_location("rsi_round", "doc/v1.4/templates/rsi/rsi_round.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class Args:
    root = sys.argv[1]
    key = "rsi"
    bench = ""
    bench_arg = []
    propose_cmd = ""


r = m.Rsi(Args())
home = pathlib.Path(Args.root) / "projects/rsi/rsi"
r.cfg = {"surfaces": ["plugins"], "rounds": 1, "margin_pp": 5.0,
         "budget_secs": 60, "bench_exe": "", "tasks": "tasks"}
base, cand = home / "arms/baseline", home / "arms/round-1"
fails = []


def ck(name, ok, extra=""):
    print(("PASS " if ok else "FAIL ") + name + (f" —— {extra}" if extra else ""))
    if not ok:
        fails.append(name)


txt = pathlib.Path(r.write_patch(99, base, cand)).read_text(encoding="utf-8")
ck("patch 是 unified diff 且只覆盖数据面",
   "--- a/plugins/tools/demo/tools.toml" in txt and "+++ b/plugins/tools/demo/tools.toml" in txt
   and "+[[discover]]" in txt and "pnpm" in txt)
# 两条臂逐字节相同 ⇒ 必须**拒绝**（"候选更优"却零差异 = 读数出了问题，不许产出空 patch）
same = home / "arms/same"
m.copy_tree(base / "plugins", same / "plugins")
try:
    r.write_patch(98, base, same)
    ck("两臂逐字节相同 ⇒ 拒绝产 patch", False, "没拒绝")
except SystemExit as e:
    ck("两臂逐字节相同 ⇒ 拒绝产 patch（读数有问题不许装成候选更优）", e.code == 2, f"退出码 {e.code}")
print("\n自检(⑥)：" + ("全过" if not fails else f"失败 {len(fails)} 条"))
sys.exit(1 if fails else 0)
PY
RC=$?
rm -rf "$W"
echo "EXIT=$RC"
exit $RC
