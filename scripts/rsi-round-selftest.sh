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
echo
echo "########## ⑦ promote / demote（P3：人合并 + 备份回滚点 + 提升即回归 + 自动回滚）"
python - "$W" <<'PY'
import importlib.util, json, pathlib, sys

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("rsi_round", "doc/v1.4/templates/rsi/rsi_round.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

class Args:
    root = sys.argv[1]; key = "rsi"; bench = ""; bench_arg = []; propose_cmd = ""
    skip_verify = False; tag = ""

r = m.Rsi(Args())
home = pathlib.Path(Args.root) / "projects/rsi/rsi"
r.cfg = {"surfaces": ["plugins"], "rounds": 1, "margin_pp": 5.0,
         "budget_secs": 60, "bench_exe": "", "tasks": "tasks"}
base, cand = home / "arms/baseline", home / "arms/round-1"
fails = []
def ck(name, ok, extra=""):
    print(("PASS " if ok else "FAIL ") + name + (f" —— {extra}" if extra else ""))
    if not ok: fails.append(name)

def rc_of(fn):
    try:
        fn(); return 0
    except SystemExit as e:
        return e.code

# ① 没有 receipt ⇒ 不许合并（没有证据就没有合并）
ck("没跑过 score ⇒ promote 拒绝（没有可复核的证据）", rc_of(lambda: r.promote(1, verify=False)) == 2)

# ② 裁决不是「候选更优」⇒ 拒绝
rec = home / "receipts/round-1.json"
payload = json.loads(rec.read_text(encoding="utf-8")) if rec.is_file() else {}
payload.update({"decision": "拒绝：差额 +0.0 pp 在 MARGIN 5 之内", "patch": None})
rec.parent.mkdir(parents=True, exist_ok=True)
rec.write_text(json.dumps(payload, ensure_ascii=False), encoding="utf-8")
ck("裁决是「拒绝」⇒ promote 拒绝（判据 7：没有更优就不许合）", rc_of(lambda: r.promote(1, verify=False)) == 2)

# ③ 补一份**真的**「候选更优」receipt（三方哈希按现状算，patch 指向真实文件）⇒ 提升成功
before = {s: m.sha256_path(base / s) for s in r.cfg["surfaces"]}
cand_hash = {s: m.sha256_path(cand / s) for s in r.cfg["surfaces"]}
root_hash = {s: m.sha256_path(pathlib.Path(Args.root) / s) for s in r.cfg["surfaces"]}
patch = home / "out/round-1.patch"; patch.parent.mkdir(parents=True, exist_ok=True); patch.write_text("--- a\n+++ b\n", encoding="utf-8")
payload.update({
    "decision": "候选更优：+9.0 pp",
    "patch": str(patch),
    "surfaces_hash": {s: {"portable_root": root_hash[s], "baseline": before[s], "candidate": cand_hash[s]}
                      for s in r.cfg["surfaces"]},
})
rec.write_text(json.dumps(payload, ensure_ascii=False), encoding="utf-8")
ck("证据齐 ⇒ 提升成功", rc_of(lambda: r.promote(1, verify=False)) == 0)
after = {s: m.sha256_path(base / s) for s in r.cfg["surfaces"]}
ck("baseline 已变成候选的数据面", after == cand_hash, f"{after}")
baks = sorted((home / "arms").glob("baseline.bak-*"))
ck("提升前留了回滚点（baseline.bak-*）", len(baks) == 1, str([b.name for b in baks]))
prec = home / "receipts/promote-1.json"
pr = json.loads(prec.read_text(encoding="utf-8")) if prec.is_file() else {}
ck("提升 receipt 如实记了 skip-verify", pr.get("kind") == "rsi-promote-receipt" and pr.get("verify_skipped") is True)
ck("提升 receipt 记了提升前后哈希与备份路径",
   pr.get("baseline_after") == cand_hash and pr.get("baseline_before") == before and pr.get("backup"))
ck("提升 receipt 里原件哈希**仍与现状一致**（没写便携根）",
   pr.get("portable_root_after") == root_hash)

# ④ 回归比较：**按尺子收参数的顺序**认新旧（不是字母序 —— 那条会把 baseline 与 baseline.bak 颠倒，
#    于是把"变好"报成"回归"）
def fake(arms, kinds):
    return {"arms": [{"arm": a} for a in arms],
            "records": [{"task": "t1", "arm": a, "kind": k} for a, k in zip(arms, kinds)]}
reg, err = r._regressions(fake(["baseline.bak-1", "baseline"], ["ok", "wrong"]))
ck("回归判据：旧 ok / 新 wrong ⇒ 报回归", len(reg) == 1 and not err, f"{reg} {err}")
reg2, _ = r._regressions(fake(["baseline.bak-1", "baseline"], ["wrong", "ok"]))
ck("反过来（旧 wrong / 新 ok）⇒ 不是回归（顺序认错了就会把它当回归）", not reg2)
ng, _ = r._regressions(fake(["baseline.bak-1", "baseline"], ["ok", "ok"]))
ck("两臂都 ok ⇒ 不是回归（别把「本来就好」当回归）", not ng)
# 负样本（expect=fail）现在**通过**了，在尺子里就是 `wrong` —— 与上面那条同一个形状，
# 所以「奖励劫持」不需要另一套判据，它天然落在这条比较里。
ck("负样本通过 == new 是 wrong ⇒ 同一套比较抓住它（判据只需 kind）",
   reg and reg[0]["new"] == ["wrong"])

# ⑤ 提升后的自动回滚：桩掉子进程，让"回归那一跑"写出一个**有回归**的读数
real_call = m.subprocess.call
def stub_call(argv, *a, **k):
    out = pathlib.Path(Args.root) / "projects/rsi/rsi/arms/verify-1"
    out.mkdir(parents=True, exist_ok=True)
    names = [pathlib.Path(argv[i + 1]).name for i, x in enumerate(argv) if x == "--arm"]
    (out / "receipt.json").write_text(json.dumps({
        "decision": "候选更优：+9.0 pp",
        "arms": [{"arm": n} for n in names],
        "records": [{"task": "t1", "arm": names[0], "kind": "ok"},
                    {"task": "t1", "arm": names[1], "kind": "wrong"}],
    }, ensure_ascii=False), encoding="utf-8")
    return 0
# 先把 baseline 退回提升前，再走"带回归"的提升
m.copy_tree(baks[0] / "plugins", base / "plugins_tmp")
m.shutil.rmtree(base / "plugins"); m.copy_tree(baks[0] / "plugins", base / "plugins")
m.shutil.rmtree(base / "plugins_tmp")
# `load()` 之外手工构造：它会给的两个路径属性这里自己补上（真身都被桩掉了，只做占位）
r.bench = pathlib.Path("stub-bench")
r.tasks = pathlib.Path("tasks")
m.subprocess.call = stub_call
rc = rc_of(lambda: r.promote(1, verify=True))
m.subprocess.call = real_call
restored = {s: m.sha256_path(base / s) for s in r.cfg["surfaces"]}
ck("有回归 ⇒ 提升被拒（退出码 2）", rc == 2, f"退出码 {rc}")
ck("**已自动回滚**：baseline 回到提升前那份", restored == before, f"{restored}")
pr2 = json.loads(prec.read_text(encoding="utf-8"))
ck("回滚写进了 receipt（rolled_back + 回归清单）",
   pr2.get("rolled_back") is True and pr2.get("regression"))

# ⑥ demote：退回最近一次备份
ck("demote 成功", rc_of(lambda: r.demote("")) == 0)
ck("demote 之后 baseline == 备份", {s: m.sha256_path(base / s) for s in r.cfg["surfaces"]} == before)
ck("demote 也留 receipt", any(p.name.startswith("demote-") for p in (home / "receipts").iterdir()))

print("\n自检(⑦)：" + ("全过" if not fails else f"失败 {len(fails)} 条：{fails}"))
sys.exit(1 if fails else 0)
PY
RC2=$?
echo
echo "########## ⑧ A-1 的牌子 + A-2/A-3/A-4（声明式门禁）"
python - "$W" <<'PY'
import importlib.util, json, pathlib, sys

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("rsi_round", "doc/v1.4/templates/rsi/rsi_round.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

class Args:
    root = sys.argv[1]; key = "rsi"; bench = ""; bench_arg = []; propose_cmd = ""
    skip_verify = False; tag = ""

r = m.Rsi(Args())
home = pathlib.Path(Args.root) / "projects/rsi/rsi"
r.cfg = {"surfaces": ["plugins"], "rounds": 3, "candidates": 1, "margin_pp": 5.0,
         "budget_secs": 3600, "gates": [], "bench_exe": "", "tasks": "tasks"}
r.bench = pathlib.Path("stub-bench")
r.tasks = home / "tasks"
fails = []
def ck(name, ok, extra=""):
    print(("PASS " if ok else "FAIL ") + name + (f" —— {extra}" if extra else ""))
    if not ok: fails.append(name)

# ① A-1：propose 必须写牌子（人一开局要看的东西）
r.propose(2, "")
brief = home / "arms/round-2/round-2.brief.md"
txt = brief.read_text(encoding="utf-8") if brief.is_file() else ""
ck("propose 写了牌子（A-1：会话里由人驱动的材料）", brief.is_file())
for need in ["允许自改的面", "预算（A-3）", "门禁（A-4", "score 2", "不许动的"]:
    ck(f"牌子上有「{need}」", need in txt)
ck("牌子上写了上一回合的读数（没有就明说没有）",
   ("上一回合（round-1）的读数" in txt) or ("还没有读数" in txt))

# ② A-4：门禁**声明式** —— 只跑清单里的命令，没过就不产 patch
base, cand = home / "arms/baseline", home / "arms/round-2"
cand.mkdir(parents=True, exist_ok=True)
m.copy_tree(base / "plugins", cand / "plugins_tmp"); m.shutil.rmtree(cand / "plugins")
m.copy_tree(base / "plugins", cand / "plugins")
# 在副本上做一处真改动（否则 write_patch 会以「零差异」拒绝 —— 那条判据也要在）
(cand / "plugins/tools/demo/tools.toml").write_text(
    (cand / "plugins/tools/demo/tools.toml").read_text(encoding="utf-8") + '\n[[discover]]\nname = "x"\nbin = "x"\nmarkers = ["x"]\n',
    encoding="utf-8")
names = ["baseline", "round-2"]
real_call = m.subprocess.call
def stub_call(argv, *a, **k):
    if isinstance(argv, str):                      # 门禁那条路（shell=True 传字符串）
        return 0 if argv == "gate-ok" else 1
    if "--arm" in argv:                            # 尺子那条路
        out = pathlib.Path([argv[i + 1] for i, x in enumerate(argv) if x == "--out"][0])
        out.mkdir(parents=True, exist_ok=True)
        (out / "receipt.json").write_text(json.dumps({
            "decision": "候选更优：+9.0 pp",
            "arms": [{"arm": n} for n in names],
            "records": [{"task": "t1", "arm": n, "kind": "ok"} for n in names],
        }, ensure_ascii=False), encoding="utf-8")
        return 0
    return 0
m.subprocess.call = stub_call
try:
    r.cfg["gates"] = ["gate-bad"]
    r.score(2)
    rec = json.loads((home / "receipts/round-2.json").read_text(encoding="utf-8"))
    ck("门禁没过 ⇒ **不产 patch**（判据 4 有证据）",
       rec.get("patch") is None and not (home / "out/round-2.patch").exists())
    ck("receipt 记了门禁逐条退出码",
       [g.get("rc") for g in rec.get("gates", [])] == [1], str(rec.get("gates")))
    ck("receipt 的 notes 说清是门禁没过", any("门禁没过" in n for n in rec.get("notes", [])))

    r.cfg["gates"] = ["gate-ok"]
    r.score(2)
    rec2 = json.loads((home / "receipts/round-2.json").read_text(encoding="utf-8"))
    ck("门禁全绿 ⇒ 出 patch", bool(rec2.get("patch")) and (home / "out/round-2.patch").is_file())
    ck("receipt 记了门禁全绿", [g.get("ok") for g in rec2.get("gates", [])] == [True])

    # ③ 门禁没过的那份 receipt 不许被提升
    payload = dict(rec2)
    payload["gates"] = [{"cmd": "gate-bad", "rc": 1, "ok": False}]
    (home / "receipts/round-2.json").write_text(json.dumps(payload, ensure_ascii=False), encoding="utf-8")
    rc = 0
    try:
        r.promote(2, verify=False)
    except SystemExit as e:
        rc = e.code
    ck("门禁没绿 ⇒ promote 拒绝（不许进主干）", rc == 2, f"退出码 {rc}")
finally:
    m.subprocess.call = real_call

print("\n自检(⑧)：" + ("全过" if not fails else f"失败 {len(fails)} 条：{fails}"))
sys.exit(1 if fails else 0)
PY
RC3=$?
rm -rf "$W"
echo "EXIT=$RC"
[ "$RC" = "0" ] || exit $RC
echo "EXIT2=$RC2"
echo "EXIT3=$RC3"
[ "$RC3" = "0" ] || exit $RC3
exit $RC2
