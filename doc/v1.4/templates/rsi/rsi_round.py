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


def sha256_path(p: pathlib.Path):
    """一个文件或一棵树的指纹（不存在就如实返回 None，不编一个空串冒充）。"""
    if p.is_file():
        return hashlib.sha256(p.read_bytes()).hexdigest()
    if p.is_dir():
        return sha256_tree(p)[0]
    return None


def changed(before, after):
    out = [f"改动 {k}" for k, v in after.items() if before.get(k) not in (None, v)]
    out += [f"新增 {k}" for k in after if k not in before]
    out += [f"删除 {k}" for k in before if k not in after]
    return out


def remove_path(p: pathlib.Path):
    """删一个**文件**或一棵**树** —— 路径的种类不该让每个调用点各写一遍 if/elif。

    `skills.toml` 这样的"面"就是一个文件（A-2 拍板的第二个面就是它），所以"面"的种类是**数据**，
    不是这段代码的假设。
    """
    if p.is_dir():
        shutil.rmtree(p)
    elif p.exists() or p.is_symlink():
        p.unlink()


def copy_tree(src: pathlib.Path, dst: pathlib.Path):
    """照原样拷一份（**文件也行**），先清掉落点上原来那个。

    这里原先一律走 `copytree`，于是 `surfaces` 里只要写一个**文件**（`skills.toml`）就当场
    `NotADirectoryError` —— 而它正是 A-2 拍板的第二个面。自检里当时只写过目录面（`plugins`），
    所以这条一直没暴露：**面的形状是数据**，自检的世界里必须同时有目录面和文件面。
    """
    if not src.exists():
        return
    remove_path(dst)
    dst.parent.mkdir(parents=True, exist_ok=True)
    if src.is_dir():
        shutil.copytree(src, dst)
    else:
        shutil.copy2(src, dst)


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
        # 默认值 = v1.4 拍板单 A-2 / A-3 / A-4（2026-10-08 拍板「照建议来」）：
        # A-2 第一版允许自改的面就是这两个加载点；A-3 每轮 3 轮 / 1 候选 / 1h 硬顶；
        # A-4 既有门禁**声明式** —— 驱动器只跑 `gates` 里列出来的命令，不替人决定跑什么。
        cands = int(rsi.get("candidates", 1))
        if cands < 1:
            die("[rsi].candidates 必须 >= 1")
        gates = list(rsi.get("gates") or [])
        for g in gates:
            if not isinstance(g, str) or not g.strip():
                die("[rsi].gates 里每一项都是要跑的命令字符串")
        self.cfg = {
            "surfaces": surfaces,
            "rounds": int(rsi.get("rounds", 3)),
            "candidates": cands,
            "margin_pp": float(rsi.get("margin_pp", 5.0)),
            "budget_secs": int(rsi.get("budget_secs", 3600)),
            "gates": gates,
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
        # **钉住的引擎条件**（`<rsi>/bench.toml`，如果有）：每条臂都要带上它，且它**不是面** ——
        # 人/agent 都不许改它（改了就是改考场的规则，不是改候选）。没有它，那些"考引擎配置"的
        # 任务（例如白名单边界）在臂里根本没有条件，会稳定地失败在"闸没开"上 —— 而读数不解释原因，
        # 只留下一个恒定的 wrong（真踩过：`whitelist-holds` 两臂都 wrong）。
        pinned = self.home / "bench.toml"
        if pinned.is_file():
            copy_tree(pinned, dst / "bench.toml")
        for s in self.cfg["surfaces"]:
            src = (self.root / s) if src_from_root else (self.arm(label) / s)
            copy_tree(src, dst / s)
            # 每一条臂都必须**带全**声明的面：少一个面，读数就是在另一个世界上读的，却没人
            # 看得出来。真实事故：一次崩溃把基准臂留在半途（文件面没拷上），此后每个候选都少这一面，
            # 而 `promote` 的逐面三方哈希也照常"通过"（它只比自己那几面）—— 判据不会替你发现"面少了"。
            if sha256_path(dst / s) is None:
                die(f"臂 {name} 缺 surface {s!r}（源 {src} 不存在）—— 读数会悄悄少一个面。"
                    f"修法：删掉 arms/{label} 再 `propose`（让它从便携根重播），或把那一面补回基准臂")
        return dst

    # ---- 命令

    def plan(self):
        c = self.cfg
        print(f"RSI 主页   : {self.home}")
        print(f"便携根     : {self.root}")
        print(f"允许自改的面: {', '.join(c['surfaces'])}")
        print(f"冻结考卷   : {self.tasks}（只读；跑前跑后各算一次哈希）")
        print(f"评测台     : {self.bench or '（未就位）'}")
        print(f"每回合     : {c['rounds']} 轮 · {c['candidates']} 个候选（A-3）· MARGIN {c['margin_pp']} pp "
              f"· 预算硬顶 {c['budget_secs']}s")
        print(f"门禁（A-4）: {'；'.join(c['gates']) if c['gates'] else '**没声明**（这一回合没有门禁证据）'}")
        print("")
        print("① 提议（人/agent 在副本上做）：")
        print(f"   基准臂 {self.arm('baseline')} ← 现状数据面的副本")
        print(f"   候选臂 {self.arm('round-<n>')} ← `propose <n>` 生成，然后改它")
        print("② 配对测：`score <n>`（一臂一进程、同任务集、同温度、同轮数）")
        print("③ 判定：差额 > MARGIN 才出 out/round-<n>.patch + receipts/round-<n>.json")
        print("④ **人合并**：`promote <n>`（复核证据 → 备份回滚点 → 提升 → 立刻跑回归；")
        print("   有回归就自动回滚）；反悔用 `demote`（退回最近一次提升前的 baseline）")
        for s in c["surfaces"]:
            print(f"   （绝不允许写便携根的 {s}）")

    def gates(self, in_dir):
        """**声明式门禁**（A-4）：只跑 `allow.toml` 里列出来的命令，不替人决定跑什么。

        跑在**候选臂**那份副本里（被审的就是它）。任何一条非 0 ⇒ 这一回合**不产 patch** ——
        判据 4 的「门禁全绿」从此有证据，不再靠口头核对。没声明门禁时如实记空表并提示：
        这一回合的门禁面是空的，那也是"没有证据"，不是"通过"。
        """
        out = []
        for cmd in self.cfg.get("gates") or []:
            print(f"门禁：{cmd}")
            rc = subprocess.call(cmd, shell=True, cwd=str(in_dir))
            out.append({"cmd": cmd, "rc": rc, "ok": rc == 0})
        return out

    def _brief(self, n):
        """给**人**开这一局用的牌子（A-1「会话里由人驱动」的材料）。

        为什么要有它：A-1 的全部动作就是"人打开这份副本、对 agent 说一句'按读数改'"，那这一句
        得**有据可依** —— 上一轮哪个任务掉了分、允许改什么、预算多少、改完跑哪条命令，
        全部写在牌子上，人不必去翻 receipt。
        """
        c = self.cfg
        lines = [
            f"# RSI 提议步的牌子（第 {n} 回合）",
            "",
            "这一局只改**副本**（就是本目录）。便携根里的原件不在这一链上，改它等于把"
            "「原件没被写」那条判据作废。",
            "",
            f"- 允许自改的面：{', '.join(c['surfaces'])}",
            f"- 预算（A-3）：{c['rounds']} 轮 · {c['candidates']} 个候选 · 硬顶 {c['budget_secs']}s",
            f"- 候选数：本次要出 **{c['candidates']}** 个候选（先出第 1 个，不够再谈）",
        ]
        g = c.get("gates") or []
        lines.append(f"- 门禁（A-4，声明式）：{'；'.join(g) if g else '**没声明**（这一回合没有门禁证据）'}")
        pinned = self.home / "bench.toml"
        lines.append(
            "- 钉住的引擎条件：`<rsi>/bench.toml`" + ("（每条臂都会带上它，**不是**面、不许改）"
                                                   if pinned.is_file() else "（**没有**：臂跑的是引擎出厂条件）"))
        lines += [
            "- 不许动的：冻结考卷（`tasks/`）、判据、`receipts/`、`out/` —— 它们是适应度来源，"
            "改了就等于自己给自己发分",
            "",
        ]
        prev = self.home / "receipts" / f"round-{n - 1}.json"
        if prev.is_file():
            try:
                r = json.loads(prev.read_text(encoding="utf-8"))
            except Exception:
                r = {}
            lines += [f"## 上一回合（round-{n - 1}）的读数", "", f"- 裁决：{r.get('decision', '（没记）')}"]
            per = {}
            for rec in r.get("records") or []:
                per.setdefault(rec.get("task"), {}).setdefault(rec.get("arm"), []).append(rec.get("kind"))
            for tk in sorted(per):
                arms = per[tk]
                names = list(arms)
                detail = "，".join(f"{a}: {'/'.join(arms[a])}" for a in names)
                lines.append(f"- 任务 `{tk}`：{detail}")
            if r.get("patch"):
                lines.append(f"- 上一轮的 patch：`{r['patch']}`（人合并过没有？看 `receipts/promote-*.json`）")
            if r.get("gates"):
                gsum = "；".join("{}→rc={}".format(x.get("cmd"), x.get("rc")) for x in r["gates"])
                lines.append("- 上一轮门禁：" + gsum)
            lines.append("")
        else:
            lines += ["## 上一回合的读数", "", "（还没有读数：这一回合先按常识改第一个面。）", ""]
        lines += [
            "## 怎么走",
            "",
            "1. 在 ruyix 里把**本目录**作为项目打开（会话 → 对 agent 说「照牌子改」）；",
            f"2. 改完：`python rsi_round.py --root <便携根> --key <键> score {n}`",
            f"3. 出 patch 后：`… promote {n}`（复核证据 → 备份回滚点 → 提升 → 立刻回归；有回归自动退回）",
        ]
        return "\n".join(lines) + "\n"

    def propose(self, n, cmd):
        base = self.arm("baseline")
        if not base.is_dir():
            print("基准臂不存在，先建（现状数据面的副本）")
            self.make_arm(True, "baseline", "baseline")
        dst = self.make_arm(False, f"round-{n}", "baseline")
        brief = dst / f"round-{n}.brief.md"
        brief.write_text(self._brief(n), encoding="utf-8")
        print(f"候选臂已就位：{dst}")
        print(f"牌子（A-1：人一开局要看的东西）：{brief}")
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
        # A-4：门禁**声明式** —— 只有裁决是「候选更优」时才值得跑（其余没资格产 patch），
        # 但跑了就必须全绿，否则这一回合不产 patch（判据 4 的「门禁全绿」要留证据）
        gres = self.gates(cand) if (better and not drift) else []
        if not (self.cfg.get("gates") or []):
            note.append("没声明门禁（A-4）—— 这一回合的门禁面是空的，那是「没有证据」，不是「通过」")
        bad_gates = [g for g in gres if not g.get("ok")]
        if bad_gates:
            note.append("门禁没过：" + "；".join(f"{g['cmd']}（rc={g['rc']}）" for g in bad_gates))
        if better and not drift and not bad_gates:
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
            # A-4：门禁结论进 receipt —— 判据 4 从此有证据（空表 = 没声明，不是通过）
            "gates": gres,
            # A-6 的判据：逐面三方哈希（原件 / 基准臂 / 候选臂）
            "surfaces_hash": self.surface_hashes(base, cand),
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
        sh = rec["surfaces_hash"]
        for s in self.cfg["surfaces"]:
            h = sh[s]
            in_copy = h["portable_root"] == h["baseline"]
            print(f"便携根的 {s} 一个字节都没动 ✓（逐面哈希：原件 {str(h['portable_root'])[:8]} / "
                  f"基准臂 {str(h['baseline'])[:8]} / 候选臂 {str(h['candidate'])[:8]}）")
            if not in_copy:
                print(f"  ⚠ 基准臂与便携根原件**不一致** —— 原件在别人手里变过了？先 `propose` 重建基准臂再谈读数")

    def surface_hashes(self, base, cand):
        """**逐面三方哈希对照**（v1.4 拍板单 A-6 的判据，2026-10-08 拍板「不补 execute 闸、
        靠副本隔离」）：receipt 必须能让人看出「这一轮确实是在副本里跑的」——

        - `portable_root` = 便携根原件（跑完必须与跑前一致：**一个字节都没被写**）；
        - `baseline`      = 基准臂（应当**等于**原件 —— 它是原件那一刻的副本）；
        - `candidate`     = 候选臂（应当**不同于**基准臂，且不同之处正是本次提议）。

        `execute` 这条原语的越界（例如 `python -c "open('x','w')"`）在 v1.4 范围外，
        防线就落在这三行对照上：原件没动 ⇒ 就算模型在副本里越了界，也污染不到便携根。
        """
        out = {}
        for s in self.cfg["surfaces"]:
            out[s] = {
                "portable_root": sha256_path(self.root / s),
                "baseline": sha256_path(base / s),
                "candidate": sha256_path(cand / s),
            }
        return out

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


    # ---------------------------------------------------------------- P3：人合并与回滚

    def _surfaces_now(self, arm_dir):
        return {s: sha256_path(arm_dir / s) for s in self.cfg["surfaces"]}

    def _regressions(self, receipt):
        """从尺子的读数里挑出**回归**：某个任务在旧臂上是 ok、在新臂上不是 ok。

        负样本（`expect=fail`）现在**通过**了，在尺子里就是 `wrong` ⇒ 同一套比较天然覆盖
        「奖励劫持」那条（判据 5 的回归面）。
        """
        per = {}
        for r in receipt.get("records") or []:
            per.setdefault((r.get("task"), r.get("arm")), []).append(r.get("kind"))
        tasks = sorted({k[0] for k in per})
        # **新旧按尺子收参数的顺序**（`--arm 旧 --arm 新`），不按字母序 —— 目录名是
        # `baseline` 与 `baseline.bak-<ts>`，字母序会把两者**颠倒**，于是把"变好"报成"回归"。
        arms = [a.get("arm") for a in (receipt.get("arms") or []) if a.get("arm")]
        if len(arms) != 2:
            return [], f"读数里不是两条臂（{arms}）—— 没法比回归"
        old, new = arms[0], arms[1]
        bad = []
        for tk in tasks:
            o = per.get((tk, old), [])
            n = per.get((tk, new), [])
            if o and n and all(k == "ok" for k in o) and any(k != "ok" for k in n):
                bad.append({"task": tk, "old": o, "new": n})
        return bad, ""

    def promote(self, n, verify=True):
        """**人**把第 n 回合的候选提升成新的基准臂（v1.4 P3）。**驱动器绝不自动合并** ——
        这条命令由人显式跑，它只负责三件机械的事：复核证据、留回滚点、提升后立刻回归。

        为什么提升完不能就算完：候选是在副本里改出来的，它一进 baseline 就成了下一回合的**起点**
        与原件对照物；万一它其实更差（读数被噪声/环境骗了），后面每一回合都踩在坏地基上。
        所以任何一步不对就**自己退回去**，并把原因写清楚。
        """
        base, cand = self.arm("baseline"), self.arm(f"round-{n}")
        rec_path = self.home / "receipts" / f"round-{n}.json"
        if not rec_path.is_file():
            die(f"没有 {rec_path} —— 没跑过 `score {n}` 就没有可复核的证据，不许合并")
        rec = json.loads(rec_path.read_text(encoding="utf-8"))
        if "候选更优" not in (rec.get("decision") or ""):
            die(f"第 {n} 回合的裁决是「{rec.get('decision')}」—— 没有「候选更优」就不许提升（判据 7）")
        if not rec.get("patch"):
            die("这一回合没出 patch —— 没有可审的改动就不许提升")
        bad = [g for g in (rec.get("gates") or []) if not g.get("ok")]
        if bad:
            die("这一回合的门禁没过（" + "；".join(f"{g['cmd']} rc={g['rc']}" for g in bad)
                + "）—— 门禁没绿不许进主干（判据 4）")
        if not cand.is_dir():
            die(f"候选臂不在 {cand}（提升谁？）")
        if not base.is_dir():
            die(f"基准臂不在 {base}（先 `propose {n}` 建它）")

        # ① 复核证据：三条哈希都要与 receipt 记的一致 —— 原件没被写、候选没被后来动过
        want = rec.get("surfaces_hash") or {}
        now_root = {s: sha256_path(self.root / s) for s in self.cfg["surfaces"]}
        now_cand = self._surfaces_now(cand)
        for s in self.cfg["surfaces"]:
            w = want.get(s) or {}
            if now_root[s] != w.get("portable_root"):
                die(f"便携根的 {s} 与 receipt 记的不一致（原件被谁动过？）—— 先重建基准臂再谈合并")
            if now_cand[s] != w.get("candidate"):
                die(f"候选臂的 {s} 与 receipt 记的不一致 —— 候选在评分之后被改过了，这一回合的证据作废")

        # ② 回滚点：先备份当前 baseline，备份失败就**不许**继续（没有回滚点的合并不做）
        ts = time.strftime("%Y%m%d-%H%M%S")
        backup = self.home / "arms" / f"baseline.bak-{ts}"
        before = self._surfaces_now(base)
        backup.mkdir(parents=True, exist_ok=True)
        for s in self.cfg["surfaces"]:
            copy_tree(base / s, backup / s)
        if self._surfaces_now(backup) != before:
            die(f"备份校验不过（{backup}）—— 没有可信回滚点，拒绝合并")

        # ③ 提升：用候选的数据面覆盖基准臂（只管 arms/，**不碰便携根**）
        for s in self.cfg["surfaces"]:
            copy_tree(cand / s, base / s)   # 落点原来那份（文件或树）由 copy_tree 自己清
        print(f"已提升：round-{n} → {base}")
        print(f"回滚点：{backup}（`demote` 可退回）")

        # ④ 回归：同一套参数再跑一次（新 baseline vs 旧 baseline）。
        #    判据 5 要的是"人能凭 receipt 决定合不合，且合完可回滚" —— 这一步就是那句话的执行体。
        reg, err = [], ""
        verify_rec = None
        if verify:
            out = self.home / "arms"
            argv = [
                str(self.bench), "--tasks", str(self.tasks),
                "--arm", str(backup), "--arm", str(base),
                "--rounds", str(self.cfg["rounds"]), "--margin", str(self.cfg["margin_pp"]),
                "--out", str(out / f"verify-{n}"), "--budget-secs", str(self.cfg["budget_secs"]),
            ] + list(self.extra)
            print("回归跑尺子：" + " ".join(argv))
            rc = subprocess.call(argv)
            rp = out / f"verify-{n}" / "receipt.json"
            if not rp.is_file():
                err = f"回归没产出读数（退出码 {rc}）"
            else:
                verify_rec = json.loads(rp.read_text(encoding="utf-8"))
                reg, err = self._regressions(verify_rec)
            rolled = bool(reg) or bool(err)
            if rolled:
                # ⑤ 自动回滚：把备份放回去，**并再核一次**哈希
                for s in self.cfg["surfaces"]:
                    copy_tree(backup / s, base / s)
                ok = self._surfaces_now(base) == before
                print(f"✗ 回归不过 ⇒ 已自动回滚：{'哈希核对通过 ✓' if ok else '哈希核对**失败**，请手工检查'}")
        else:
            print("⚠ 按 `--skip-verify` 跳过了回归 —— 这次提升**没有**回归证据（receipt 里会如实记）")

        rec2 = {
            "kind": "rsi-promote-receipt",
            "round": n,
            "when": int(time.time()),
            "backup": str(backup),
            "baseline_before": before,
            "baseline_after": self._surfaces_now(base),
            "portable_root_after": now_root,
            "regression": reg,
            "regression_error": err,
            "verify_receipt": str((self.home / "arms" / f"verify-{n}" / "receipt.json")) if verify_rec else None,
            "verify_rc_decision": (verify_rec or {}).get("decision"),
            "rolled_back": bool(reg) or bool(err),
            "verify_skipped": (not verify),
        }
        (self.home / "receipts").mkdir(exist_ok=True)
        rp2 = self.home / "receipts" / f"promote-{n}.json"
        rp2.write_text(json.dumps(rec2, ensure_ascii=False, indent=2), encoding="utf-8")
        print(f"提升 receipt：{rp2}")
        if rec2["rolled_back"]:
            die(f"提升**已回滚**（回归 {len(reg)} 项{('；' + err) if err else ''}）—— 这一版不许合")
        for s in self.cfg["surfaces"]:
            same = now_root[s] == sha256_path(self.root / s)
            print(f"便携根的 {s} 仍未被动过：{'✓' if same else '✗'}")

    def demote(self, tag=""):
        """回滚：把 `arms/baseline` 退回某个备份（缺省取最新的 `baseline.bak-*`）。

        与 `promote` 一样只动 `arms/`：**便携根的原件永远不在这一链上**（要不"原件没被写"
        那条判据就白立了）。
        """
        base = self.arm("baseline")
        cands = sorted((self.home / "arms").glob("baseline.bak-*"))
        if tag:
            cands = [c for c in cands if tag in c.name]
        if not cands:
            die("没有可回滚的备份（`arms/baseline.bak-*`）—— 没提升过就不需要回滚")
        src = cands[-1]
        for s in self.cfg["surfaces"]:
            copy_tree(src / s, base / s)
        after = self._surfaces_now(base)
        (self.home / "receipts").mkdir(exist_ok=True)
        rp = self.home / "receipts" / f"demote-{time.strftime('%Y%m%d-%H%M%S')}.json"
        rp.write_text(json.dumps({
            "kind": "rsi-demote-receipt",
            "when": int(time.time()),
            "restored_from": str(src),
            "baseline_after": after,
        }, ensure_ascii=False, indent=2), encoding="utf-8")
        print(f"已回滚：{src} → {base}")
        print(f"回滚 receipt：{rp}")


def main():
    ap = argparse.ArgumentParser(description="RSI 一回合的驱动器（v1.4 P2，用户侧，零依赖）")
    ap.add_argument("--root", required=True, help="便携根（ruyix.exe 所在目录）")
    ap.add_argument("--key", required=True, help="RSI 项目的项目键（projects/<键>/）")
    ap.add_argument("--bench", default="", help="评测台路径（覆盖 allow.toml 里的 bench_exe）")
    ap.add_argument("--propose-cmd", default="", help="propose 步顺带跑的命令（可选）")
    ap.add_argument("--bench-arg", action="append", default=[],
                    help="原样透给评测台的参数（可重复）：--fake / --model deepseek-v4-flash / --config …。"
                         "驱动器不替尺子决定参数 —— 要覆盖什么就显式写出来")
    ap.add_argument("--skip-verify", action="store_true",
                    help="promote 时跳过提升后的回归跑（**不推荐**：这次提升就没有回归证据，"
                         "receipt 里会如实记 verify_skipped）")
    ap.add_argument("cmd", choices=["plan", "propose", "score", "promote", "demote"])
    ap.add_argument("n", nargs="?", type=int, help="回合号（propose / score / promote 要）")
    ap.add_argument("--tag", default="", help="demote 时指定备份名里的片段（缺省取最新那个）")
    a = ap.parse_args()
    r = Rsi(a).load(require_bench=(a.cmd == "score"))
    if a.cmd == "plan":
        r.plan()
    elif a.cmd == "propose":
        r.propose(a.n or 1, a.propose_cmd)
    elif a.cmd == "promote":
        r.promote(a.n or 1, verify=not a.skip_verify)
    elif a.cmd == "demote":
        r.demote(a.tag)
    else:
        r.score(a.n or 1)


if __name__ == "__main__":
    main()
