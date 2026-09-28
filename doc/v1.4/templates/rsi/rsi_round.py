#!/usr/bin/env python3
"""RSI 一回合的驱动器（v1.4 P2 用户侧脚本模板，**零依赖**：只用标准库）。

放这里的理由（需求 §4.1）：自改闭环与它的产物**落在用户侧**，不进 ruyix 源码。
这一份是**模板**，实际跑的那份拷到 `<便携根>/projects/<rsi 键>/rsi/rsi_round.py` ——
拷过去之后两边内容应当一致（不一样就说明"审计的那份"和"跑的那份"已经分家了）。

它这一版只做 ②③ 两步（提议步由人/agent 在**副本**上做，见 `doc/v1.4/P2-自改闭环-设计待拍板.md` §2）：

  plan             列出这一回合的分工与路径；缺 allow.toml / 任务集 / 尺子 ⇒ **拒绝开跑**（不猜默认值）
  propose <n>      建 arms/round-<n>（= 基准臂的副本）并打印该改哪；给了 --propose-cmd 就顺手跑它
  score <n>        跑配对两臂 → 读 receipt → 差额 > MARGIN 才出 out/round-<n>.patch + receipts/round-<n>.json

**永不写便携根的 surfaces**（`plugins/` 等）：patch 交给人合并（需求 §5：v1.4 绝不自动进主干）。
考卷（tasks/）只读，且跑前跑后各算一次哈希（判据 2）。

用法：
  python rsi_round.py --root "D:/Tools/ruyix" --key rsi plan
  python rsi_round.py --root "D:/Tools/ruyix" --key rsi propose 1
  python rsi_round.py --root "D:/Tools/ruyix" --key rsi score 1
"""

import argparse
import difflib
import hashlib
import json
import pathlib
import shutil
import subprocess
import sys
import time
import tomllib

# ---------------------------------------------------------------- 小工具

def die(*msg):
    print("拒绝开跑：" + " ".join(str(m) for m in msg), file=sys.stderr)
    sys.exit(2)


def sha256_tree(root: pathlib.Path):
    """整棵树的指纹 + 逐文件指纹（与尺子同款：变了要能**指名**是哪个文件）。"""
    per, acc = {}, hashlib.sha256()
    for p in sorted(x for x in root.rglob("*") if x.is_file()):
        rel = p.relative_to(root).as_posix()
        h = hashlib.sha256(p.read_bytes()).hexdigest()
        per[rel] = h
        acc.update(rel.encode("utf-8") + b"|" + h.encode("ascii") + b"\n")
    return acc.hexdigest(), per


def changed(before, after):
    out = [f"改动 {k}" for k, v in after.items() if before.get(k) not in (None, v)]
    out += [f"新增 {k}" for k in after if k not in before]
    out += [f"删除 {k}" for k in before if k not in after]
    return out


def copy_tree(src: pathlib.Path, dst: pathlib.Path):
    if dst.exists():
        shutil.rmtree(dst)
    if src.exists():
        shutil.copytree(src, dst)


class Rsi:
    def __init__(self, args):
        self.extra = list(getattr(args, "bench_arg", []) or [])
        self.cli_bench = getattr(args, "bench", "") or ""
        self.root = pathlib.Path(args.root).resolve()
        self.home = self.root / "projects" / args.key / "rsi"
        self.allow_file = self.home / "allow.toml"
        self.cfg = None
        self.notes = []

    # ---- 配置：缺就拒绝（不猜默认值）

    def load(self, require_bench=True):
        if not self.allow_file.is_file():
            die(f"没有 {self.allow_file} —— 白名单是这一版的边界，缺了就不许开跑")
        with open(self.allow_file, "rb") as f:
            doc = tomllib.load(f)
        rsi = doc.get("rsi") or {}
        surfaces = rsi.get("surfaces") or []
        if not surfaces:
            die("[rsi].surfaces 为空 —— 没有允许自改的面，这一回合无事可做")
        for s in surfaces:
            if pathlib.Path(s).is_absolute() or ".." in pathlib.Path(s).parts:
                die(f"surfaces 里 {s!r} 不是便携根内的相对路径（白名单不许越出便携根）")
        self.cfg = {
            "surfaces": surfaces,
            "rounds": int(rsi.get("rounds", 3)),
            "margin_pp": float(rsi.get("margin_pp", 5.0)),
            "budget_secs": int(rsi.get("budget_secs", 3600)),
            "bench_exe": rsi.get("bench_exe", ""),
            "tasks": rsi.get("tasks", "tasks"),
        }
        for s in self.cfg["surfaces"]:
            if not (self.root / s).exists():
                die(f"surface {s!r} 在便携根里不存在：{self.root / s}")
        self.tasks = (self.home / self.cfg["tasks"]).resolve()
        if not self.tasks.is_dir():
            die(f"没有任务集目录 {self.tasks}（冻结考卷是适应度的来源，缺了就没有读数）")
        self.bench = self._find_bench(require_bench)
        return self

    def _find_bench(self, required=True):
        cands = [self.cli_bench, self.cfg["bench_exe"], str(self.home / "rsi_bench.exe")]
        for c in cands:
            if c and pathlib.Path(c).is_file():
                return pathlib.Path(c)
        import os  # noqa: PLC0415 —— 只有找不到时才用它兜底
        env = os.environ.get("RSI_BENCH_EXE", "")
        if env and pathlib.Path(env).is_file():
            return pathlib.Path(env)
        msg = ("找不到评测台 rsi_bench：在 allow.toml 里写 [rsi].bench_exe，"
               "或用 --bench / 环境变量 RSI_BENCH_EXE 指过去")
        if required:
            # 只有真正要跑尺子的时候才硬拒 —— `plan` 是"看看这一回合怎么分工"，可以还没就位
            die(msg)
        print(f"提醒：{msg}（`plan` 照常，`score` 之前必须就位）", file=sys.stderr)
        return None

    # ---- 臂

    def arm(self, name):
        return self.home / "arms" / name

    def make_arm(self, src_from_root, name, label):
        """把 surfaces 照原样拷成一条臂（臂目录 = 数据面的副本，评测台认的就是这个形状）。"""
        dst = self.arm(name)
        dst.mkdir(parents=True, exist_ok=True)
        for s in self.cfg["surfaces"]:
            src = (self.root / s) if src_from_root else (self.arm(label) / s)
            copy_tree(src, dst / s)
        return dst

    # ---- 命令

    def plan(self):
        c = self.cfg
        print(f"RSI 主页   : {self.home}")
        print(f"便携根     : {self.root}")
        print(f"允许自改的面: {', '.join(c['surfaces'])}")
        print(f"冻结考卷   : {self.tasks}（只读；跑前跑后各算一次哈希）")
        print(f"评测台     : {self.bench or '（未就位）'}")
        print(f"每回合     : {c['rounds']} 轮 · MARGIN {c['margin_pp']} pp · 预算硬顶 {c['budget_secs']}s")
        print("")
        print("① 提议（人/agent 在副本上做）：")
        print(f"   基准臂 {self.arm('baseline')} ← 现状数据面的副本")
        print(f"   候选臂 {self.arm('round-<n>')} ← `propose <n>` 生成，然后改它")
        print("② 配对测：`score <n>`（一臂一进程、同任务集、同温度、同轮数）")
        print("③ 判定：差额 > MARGIN 才出 out/round-<n>.patch + receipts/round-<n>.json；**人合并**")
        for s in c["surfaces"]:
            print(f"   （绝不允许写便携根的 {s}）")

    def propose(self, n, cmd):
        base = self.arm("baseline")
        if not base.is_dir():
            print("基准臂不存在，先建（现状数据面的副本）")
            self.make_arm(True, "baseline", "baseline")
        dst = self.make_arm(False, f"round-{n}", "baseline")
        print(f"候选臂已就位：{dst}")
        print("要改的就是这份**副本**（不是便携根里的原件）。评测台只认 `plugins/tools/*/tools.toml` 这类形状。")
        if cmd:
            print(f"跑提议命令：{cmd}")
            rc = subprocess.call(cmd, shell=True, cwd=str(dst))
            print(f"提议命令退出码 {rc}")
        else:
            print("（没给 --propose-cmd：由人/agent 编辑这份副本，改完跑 `score <n>`）")

    def score(self, n):
        c = self.cfg
        base, cand = self.arm("baseline"), self.arm(f"round-{n}")
        if not base.is_dir():
            die(f"没有基准臂 {base}（先 `propose {n}` 建它）")
        if not cand.is_dir():
            die(f"没有候选臂 {cand}（先 `propose {n}`）")
        h0, per0 = sha256_tree(self.tasks)
        out = self.home / "arms"
        started = time.time()
        argv = [
            str(self.bench), "--tasks", str(self.tasks),
            "--arm", str(base), "--arm", str(cand),
            "--rounds", str(c["rounds"]), "--margin", str(c["margin_pp"]),
            "--out", str(out), "--budget-secs", str(c["budget_secs"]),
        ] + list(self.extra)
        print("跑尺子：" + " ".join(argv))
        rc = subprocess.call(argv)
        h1, per1 = sha256_tree(self.tasks)
        drift = changed(per0, per1)
        receipt_in = out / "receipt.json"
        if not receipt_in.is_file():
            die(f"尺子没产出 {receipt_in}（退出码 {rc}）—— 这一回合没有读数")
        r = json.loads(receipt_in.read_text(encoding="utf-8"))
        decision = r.get("decision", "")
        better = "候选更优" in decision
        # ③ 产物：**只有**"更优"才出 patch；其余照样留 receipt（判据 7：无信号不采纳，但要留痕）
        patch, note = None, []
        if drift:
            note.append("考卷在本回合内变过 ⇒ 作废（判据 2）")
        if better:
            patch = self.write_patch(n, base, cand)
        (self.home / "receipts").mkdir(exist_ok=True)
        rec = {
            "kind": "rsi-round-receipt",
            "round": n,
            "when": int(started),
            "allow": self.cfg,
            "bench_argv": argv,
            "bench_rc": rc,
            "tasks_hash_before": h0,
            "tasks_hash_after": h1,
            "tasks_drifted": drift,
            "decision": decision,
            "arms": r.get("arms"),
            "median_pct": {a["arm"]: a.get("median_pct") for a in r.get("arms", [])},
            "records": r.get("records"),
            "patch": patch,
            "notes": note,
            "next": ("人审 receipt + patch 决定合不合；合了才把候选提升成新的 baseline（v1.4 绝不自动合并）"
                     if better else "差额没到 MARGIN 或无读数 ⇒ 这一回合到此为止（判据 7）"),
        }
        rp = self.home / "receipts" / f"round-{n}.json"
        rp.write_text(json.dumps(rec, ensure_ascii=False, indent=2), encoding="utf-8")
        print("")
        print(f"裁决：{decision}")
        print(f"receipt：{rp}")
        if patch:
            print(f"patch  ：{patch}（**由人合并**；驱动器不写便携根的 surfaces）")
        else:
            print("没出 patch（这是对的：无信号不采纳）")
        for s in self.cfg["surfaces"]:
            print(f"便携根的 {s} 一个字节都没动 ✓")

    def write_patch(self, n, base, cand):
        """候选 vs 基准的 unified diff（`difflib`，零依赖）。只覆盖数据面副本。"""
        chunks = []
        for s in sorted(self.cfg["surfaces"]):
            b, c2 = base / s, cand / s
            files = set()
            for r in (b, c2):
                if r.is_dir():
                    files |= {p.relative_to(r).as_posix() for p in r.rglob("*") if p.is_file()}
                elif r.is_file():
                    files.add(r.name)
            for rel in sorted(files):
                fb, fc = b / rel, c2 / rel
                tb = fb.read_text(encoding="utf-8", errors="replace").splitlines(keepends=True) if fb.is_file() else []
                tc = fc.read_text(encoding="utf-8", errors="replace").splitlines(keepends=True) if fc.is_file() else []
                if tb == tc:
                    continue
                chunks.extend(
                    difflib.unified_diff(
                        tb, tc,
                        fromfile=f"a/{s}/{rel}", tofile=f"b/{s}/{rel}", n=3,
                    )
                )
        if not chunks:
            die("判定是「候选更优」但两条臂的数据面**逐字节相同** —— 这不是候选更优，是读数出了问题")
        (self.home / "out").mkdir(exist_ok=True)
        p = self.home / "out" / f"round-{n}.patch"
        p.write_text("".join(chunks), encoding="utf-8")
        return str(p)


def main():
    ap = argparse.ArgumentParser(description="RSI 一回合的驱动器（v1.4 P2，用户侧，零依赖）")
    ap.add_argument("--root", required=True, help="便携根（ruyix.exe 所在目录）")
    ap.add_argument("--key", required=True, help="RSI 项目的项目键（projects/<键>/）")
    ap.add_argument("--bench", default="", help="评测台路径（覆盖 allow.toml 里的 bench_exe）")
    ap.add_argument("--propose-cmd", default="", help="propose 步顺带跑的命令（可选）")
    ap.add_argument("--bench-arg", action="append", default=[],
                    help="原样透给评测台的参数（可重复）：--fake / --model deepseek-v4-flash / --config …。"
                         "驱动器不替尺子决定参数 —— 要覆盖什么就显式写出来")
    ap.add_argument("cmd", choices=["plan", "propose", "score"])
    ap.add_argument("n", nargs="?", type=int, help="回合号（propose / score 要）")
    a = ap.parse_args()
    r = Rsi(a).load(require_bench=(a.cmd == "score"))
    if a.cmd == "plan":
        r.plan()
    elif a.cmd == "propose":
        r.propose(a.n or 1, a.propose_cmd)
    else:
        r.score(a.n or 1)


if __name__ == "__main__":
    main()
