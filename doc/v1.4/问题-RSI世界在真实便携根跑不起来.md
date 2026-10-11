# 问题：RSI 的世界在**真实便携根**里跑不起来（2026-10-08 现场抓出）

**怎么发现的**：不在夹具、不在自检，而是在用户真实的便携根
（`D:\Tools\ruyix`，项目键 `D-Projects-Rust-ruyix`）里按 A-1 的流程走了一遍
`plan → propose 1 → score 1`。三个缺陷，全在"只在小而短的假世界跑过"这件事上：

## ① 驱动器只认目录面 ⇒ A-2 拍板的第二个面当场崩

`rsi_round.py::copy_tree` 一律 `shutil.copytree`。A-2 拍板的第二个面是 **`skills.toml`（一个文件）**：

```
NotADirectoryError: [WinError 267] 目录名称无效。: 'D:\\Tools\\ruyix\\skills.toml'
```

自检的世界里只写过目录面（`plugins`）⇒ **这条一直没暴露**。

**修**：`copy_tree` 认文件（先 `remove_path` 落点、再分派 `copytree` / `copy2`），并把
`promote`/`demote`/回滚里那三处"先 if is_dir 删再拷"的重复写法收敛掉（同一个假设散在四处 =
同一类 bug 有四个入口）。自检的世界里**同时**放目录面与文件面 —— 这就是能抓住它的那条判据。

## ② 路径闸估的是**已被 A-5 拆掉**的机制 ⇒ 真实键被误拒

尺子跑之前拒掉过深的落点，公式里加了
`<out>/runs/<run_id>/state/verify/pycache` ＋ **镜像的源码绝对路径** —— 那是
`PYTHONPYCACHEPREFIX` 的镜像行为（pyc 路径 ≈ 2×落点深度）。A-5 之后验证管线
（语法格 / 测试格 / 导入探针）**都不写字节码**了，这条路不存在。

代价是实测的：

```
落点太深：估算字节码路径 259 字符（上限 250）……现在：D:\Tools\ruyix\projects\D-Projects-Rust-ruyix\rsi\arms
```

**整个 RSI 世界开不了跑**，而它其实完全跑得动（修正后 148/250）。

**修**：估算改成"项目里的文件 + 它原处的 `__pycache__/<名字>.cpython-3XX.pyc`"
（判据与 agent 自己的 import 仍会写它）；`py_envs`（导入探针）也补上
`PYTHONDONTWRITEBYTECODE=1`，让"整条 python 验证管线不写字节码"成为**一条判据**
（`every_python_env_the_pipeline_uses_writes_no_bytecode`）；`py_env_guard` 留着当兜底
（写盘被谁重新打开时挡在假红之前）。

**更要紧的一半是判据本身**：这条闸此前**只有拒绝方向**的断言 ⇒ 越保守越绿，
真跑时才发现"世界根本开不了"。自检第 7 条现在两个方向都钉，其中放行方向用的是
**真实便携根那样长的键**（`D-Projects-Rust-ruyix`）。

## ③ 世界缺"钉住的引擎条件" ⇒ 考引擎配置的任务恒定失败

`whitelist-holds` 是"白名单外的写入必须被引擎拒"（`expect=pass`）——它考的是**引擎配置**
（`[agent].write_allow`）。夹具的臂自带 `bench.toml`，而驱动器的臂只由 `surfaces` 组成 ⇒
两臂都恒定 `wrong`，读数里只有结果、没有原因。

**修**：`<rsi>/bench.toml` 是**这一局钉住的引擎条件**，`make_arm` 给**每条臂**都带上它；
它**不是面**（不进逐面哈希、人/agent 都不许改 —— 改它就是改考场的规则），牌子
（`round-<n>.brief.md`）里如实写明"钉住了什么 / 有没有钉"。

## 修完的真跑读数（真实便携根，零成本 `--fake`）

- 路径闸：**148/250** ✓（原先 259 拒跑）
- 两臂各 **12 条记录 / 100% ok**（`whitelist-holds` 也守住了）；裁决 = **拒绝：差额 +0.0 pp 在 MARGIN 5 之内**
  （脚本化臂两臂同份数据，本该无信号 —— 这正是判据 7 在真跑里的样子）
- 考卷 sha256 跑前跑后一致；**三个面逐字节没动**（原件 / 基准臂 / 候选臂三方哈希一致）
- 产物：`<便携根>/projects/D-Projects-Rust-ruyix/rsi/{allow.toml,bench.toml,tasks,arms,receipts,out}` + 牌子

## 留下的纪律

1. **只在短而小的假世界里跑过的通道，等于没跑过** —— 真环境的第一件事是把**真路径长度 /
   真键名 / 真目录布局**喂进去（这次的键长、文件面、缺 bench.toml 三件事都是这么暴露的）。
2. **闸的判据必须双向**：只有"该拒的拒了"= 越保守越绿；必须有"该放的放了"，且用**真实尺寸**。
3. **一个假设散在四处就是四个入口**（"面 = 目录"当年写在四段代码里）。
