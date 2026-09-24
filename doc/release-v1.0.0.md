# ruyix 1.0.0 —— 绿色便携发行说明
# ruyix 1.0.0 — Green Portable Release Notes

> 这一版只有一件事：**把 ruyix 变成真正的绿色软件。**
> 一个可执行文件 + `global/` + `projects/` + `plugins/`，全都在 exe 旁边；卸载 = 删掉那个文件夹；
> 而且**不再往你打开的项目里写任何东西**。
>
> This release does one thing: **make ruyix a truly portable app.** One executable plus
> `global/`, `projects/`, `plugins/` — all next to the exe. Uninstall = delete that folder.
> And it **no longer writes anything into the projects you open.**

---

## 一、发行形态 / What you download

```
ruyix/                      ← 解压出来就是这一层（拷到哪都行，例如 D:\ruyix）
├── ruyix.exe               ← 唯一可执行文件
├── global/                 ← 全局配置（*.toml）+ 全局数据（runs/ logs/ cache/ webview/）
├── projects/               ← 每个打开过的项目一个状态桶 projects/<项目 key>/
├── plugins/                ← 全局插件（1.0.0 只建目录、定清单位置，尚未实现加载）
├── README.md
├── LICENSE
└── SHA256SUMS.txt          ← exe 与随包文档的 sha256
```

- **也可以只拷 `ruyix.exe`**：放进一个空文件夹双击，它会自己长出上面那几个目录（实测判据 A1）。
- **卸载** = 删掉整个文件夹。`%LOCALAPPDATA%`、`~/.ruyix`、你的项目目录里都不会留下东西。
- 想要固定位置就用环境变量：`set RUYIX_HOME=D:\ruyix-data`（脚本 / 多环境 / 只读介质场景）。

| 中文 | English |
|---|---|
| 解压即用，无需安装 | Unzip and run — no installer |
| 配置与项目状态都在程序目录里 | Config and project state live inside the program folder |
| 整体备份 = 拷走整个文件夹 | Backup = copy the whole folder |
| 卸载 = 删文件夹 | Uninstall = delete the folder |
| 两台机器各自独立（便携形态天生允许多份副本并存） | Multiple copies coexist (each with its own root) |

---

## 二、这一版变了什么 / What changed

| # | 变更 | 一句话 |
|---|---|---|
| 1 | **便携根**：`exe 同目录` 是**唯一**的家 | 全局配置、日志、agent 运行产物、浏览器缓存都在这里 |
| 2 | **用户仓库零写入**：项目侧的 7 类写入全部搬进 `projects/<项目 key>/` | 打开项目跑 agent，你的 `git status` 只剩你自己的改动 |
| 3 | **项目状态桶**：`projects/<项目 key>/{stage,backups,proc,env,sessions,verify}` | 暂存、写前备份、服务日志、安装留痕、会话存档、验证产物 |
| 4 | **WebView2 数据目录落根内**（`global/webview`） | 不再往 `%LOCALAPPDATA%\com.ruyix.code` 写 40MB 缓存 |
| 5 | **单实例互斥体按根区分** | 两个不同目录的副本可以同时开，互不干扰 |
| 6 | **边界行为明确**：不可写的目录 / 从压缩包里直接双击 | 原生弹框说清原因并拒绝启动，**绝不静默写到别处** |
| 7 | **一条命令出便携包**：`node scripts/package-portable.js` | 组包 + sha256 + zip + 解压自校验 |
| 8 | 元数据对齐：`ruyix` / `1.0.0` | exe 属性页、窗口标题、界面文案统一 |

**`projects/<项目 key>` 的 key 是可读的**：`D:\Projects\Rust\ruyix` → `D-Projects-Rust-ruyix`
（分隔符换 `-`、盘符冒号丢掉、中文保留、超长与 Windows 保留名加 hash 后缀）。
每个桶里有一份 `project.toml` 自证"这个桶属于哪个项目"。

---

## 三、升级 / Upgrading from 0.x

**没有迁移步骤**（有意为之）：0.x 的旧残留是一次性的，本项目唯一的用户已经手工清完，
所以 1.0.0 里**没有任何"读旧路径"的代码分支** —— 路径解析只有一种形态，少一类永远不会被测到的代码。

- 旧配置若还在 `<项目>/.ruyix/code/*.toml` 或 `~/.ruyix/code/*.toml`：**手工拷**进 `global/`（一行 cp）。
- 旧项目状态（暂存/备份/会话）不需要带走：它们是 IDE 的状态，不是你的源码。
- 想把某台机器的 ruyix 整体搬到另一台：**拷整个文件夹**即可（`cache/` 与 `webview/` 可先删，只影响首次启动速度）。

There is **no migration code** by design: the only user already removed the legacy leftovers, so
1.0.0 has no "old path" branch at all — one shape, nothing untested. Manually copy old `*.toml`
into `global/` if you need them; move machines by copying the whole folder.

---

## 四、边界行为（都实测过）/ Boundary behaviour (measured)

| 情形 | 行为 | 证据 |
|---|---|---|
| exe 放在不可写的目录（Program Files / 只读介质） | 原生弹框【绿色软件无法安装在系统级目录】→ 退出码 2 | `node scripts/portable-root-probe.mjs`：`icacls` 造真只读目录，退出码 2 + 文案命中 |
| 从压缩包里直接双击（Windows 解到 `%TEMP%`） | 原生弹框【必须解压后试用】→ 退出码 2 | 同上（窗口标题为证：`ruyix —— 必须解压后试用`） |
| 想在上面两种位置运行 | 设 `RUYIX_HOME=<可写目录>` 即可（显式指定不再判定为临时根） | 同上 |
| 两份副本并存 | 各自建自己的 `global/`，互不干扰（互斥体名带根指纹） | 同上：两个进程同时在 |

**为什么只读目录是"拒绝启动"而不是"降级跑"**：WebView2 必须有可写的用户数据目录，
而只读根下它的合法落点不存在 —— 实测直接 `os error 5`。剩下的两条路（写 `%TEMP%` 或写
`%LOCALAPPDATA%`）恰好都是这一版要消灭的残留。所以拒绝是唯一诚实的答案。

---

## 五、已知问题与边界 / Known issues

1. **验证会跑你自己的工具链**：全量验证（Apply 模式交付前）会在项目里跑 `cargo check` /
   `pytest` / `node --check`。我们做了两件事把痕迹降到最低：
   - `cargo check` 可能生成的 `Cargo.lock`：**验证前不存在的，验证后我们会删掉**（还原现场）；
     原本就有的（bin crate 提交了 lock）一个字不动。
   - Python 的 `__pycache__`：通过 `PYTHONPYCACHEPREFIX` 改道到项目状态桶，源码树里不落字节码缓存。
   - 其余工具链缓存（`.pytest_cache` 等）保持工具自己的标准行为 —— 你在终端里跑同样的命令也会有。
     要**一个字节都不动**就用沙箱模式（`harness.sandbox.mode = require`，验证跑在项目的副本上）。
2. **跨卷验证**（程序在 C:、项目在 D:）已实测代价极小（构建耗时中位数 0.61s → 0.63s）。
   但**非 NTFS** 目标卷（exFAT 移动盘 / 网络盘）未实测。
3. **插件加载未实现**：`plugins/` 只建目录与清单位置（见 `doc/v0.x/需求-高亮插件化-v0.14.md`）。
4. **便携版是单用户形态**：同一台机器上多个 Windows 用户共用一份 ruyix 时，写权限按 OS 规则走（失败就是第一条边界的场景）。
5. **项目改名/移动会留下孤儿桶**：桶名来自项目路径。窗口的「项目列表」下方有一节
   **IDE 状态桶**（名称 / 体积 / 是否孤儿 / 删除），删除**只在确认后**发生，绝不自动删。

---

## 六、验收 / Verification

发布门槛是判据表 A1–A12（`doc/需求-便携形态与零残留-v1.0.0.md` §八），逐条留证据。命令：

```bash
cargo test                                        # 引擎 353 + 宿主 144（含 A3/A4 场景测试）
cargo clippy --all-targets && cargo fmt --check   # 0 warning
node scripts/check-style.js && node scripts/ui-smoke.js   # 318/318
cargo run -q -p harness-engine --example agent_loop_smoke # 工具循环 e2e
node scripts/p0-probes.mjs                        # WebView2 data dir + mutex（P0 回归）
node scripts/repo-clean-scenario.mjs --selftest   # 仓库干净场景（真机版）
node scripts/portable-root-probe.mjs              # 便携根五条臂（17/17）
node scripts/package-portable.js                  # 一条命令出 zip + 解压自校验
```

---

## 七、卸载 / Uninstall

1. 关掉 ruyix；
2. 删掉它的文件夹（`global/` 里的会话与配置、`projects/` 里的项目状态一起没了）；
3. 完事。`%LOCALAPPDATA%`、注册表、你的项目目录里都没有它留下的东西。

> 若你还留着 0.x 时代的残留（`~/.ruyix` / 某些项目里的 `.ruyix`），那是**旧版本**写的，
> 1.0.0 不会再动它们 —— 想清就手工删；不想清也没关系，它们不再增长。
