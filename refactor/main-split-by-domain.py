#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""main.rs 1 拆 N —— 第七刀（收尾）：把 "Tauri 命令" 段**按域**拆成三个模块。

`main.rs` 的"Tauri 命令"段是最大的一坨（527 行、横跨文件/项目/执行面板三个域）。按域拆：

- `fs_cmds.rs`      文件/目录：DirEntry / FileContent / FileBase64 + list_dir / read_file /
                    read_file_base64 / write_file / create_file / create_dir / delete_path /
                    path_exists / rename_path（+ mime_from_ext 私有助手）
- `project_cmds.rs` 项目：ProjectInfo + open_project / project_buckets / project_bucket_delete /
                    is_another_instance / get_last_project / get_projects / set_project_lang /
                    update_project / delete_project / get_run_targets
- `plugin_cmds.rs`  执行面板/插件：ExecuteStatus + get_execute_status / set_execute_entry /
                    ai_execute_check / lua_translate / reload_plugins

每段都是**多段不连续**的（原始布局里三个域是交错排列的），所以按"锚点 → 段尾花括号"逐段定位、
逐段点名核对，再按升序拼进模块。留在 main.rs：`mod` 声明 + invoke_handler 的注册（24 条）。

自检：每段的项逐个点名 / 顶层项守恒（main.rs + tests.rs + 三个新模块）/ 注册条数。
"""
import io
import os
import re

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
MAIN = os.path.join(ROOT, "src-tauri", "src", "main.rs")
TESTS = os.path.join(ROOT, "src-tauri", "src", "tests.rs")

FS = os.path.join(ROOT, "src-tauri", "src", "fs_cmds.rs")
PROJ = os.path.join(ROOT, "src-tauri", "src", "project_cmds.rs")
PLG = os.path.join(ROOT, "src-tauri", "src", "plugin_cmds.rs")

FS_HEADER = """\
//! 文件与目录命令 —— 2026-09-28 从 `main.rs` 拆出（1 拆 N 的第七刀）。
//!
//! 前端所有读写都经命令系统（`handleCommand` → `invoke`），这里就是那条链的后端一段。
//! 路径由前端 `resolveProjectPath()` 归一（拒绝绝对路径），后端只做**再一层**校验与 IO；
//! `read_file_base64` 走图片预览（`mime_from_ext` 定 mime），`list_dir` 给文件树。
//!
//! `clean_path` 不在这里：它是 crate 根的共享工具（`paths.rs` 也在用），留在 `main.rs`。

"""

PROJ_HEADER = """\
//! 项目与项目状态桶命令 —— 2026-09-28 从 `main.rs` 拆出（1 拆 N 的第七刀）。
//!
//! 都围着"当前项目"转：打开/切换（`open_project` 写全局 current）、清单（`get_projects`）、
//! 改名与语言（`update_project` / `set_project_lang`）、从清单里移除（`delete_project` ——
//! **只删条目，绝不删用户的文件夹**）、以及 v1.0.0 的**状态桶**（`project_buckets` /
//! `project_bucket_delete`：便携根里那些暂存/备份/会话存档，用户要能看见、能删）。
//! 另有单实例判定（`is_another_instance`）与运行目标清单（`get_run_targets`）。

"""

PLG_HEADER = """\
//! 插件与"这个文件能不能跑"的命令 —— 2026-09-28 从 `main.rs` 拆出（1 拆 N 的第七刀）。
//!
//! · **执行状态**（[`get_execute_status`] / [`set_execute_entry`]）：编辑器右上角那个 ▶ 的判据 ——
//!   能不能跑、有没有绑定运行目标、有没有建议命令（预置清单来的）。判据落在**清单内容**上
//!   （`runner::manifest_run_specs`），不是按扩展名猜。
//! · **插件**（[`reload_plugins`] / [`lua_translate`]）：插件注册表加载与 Lua 翻译入口。
//! · [`ai_execute_check`]：模型对"这条命令能不能跑"的判断入口。

"""

RANGES = [
    # (模块, 起锚点名(函数/结构), 段尾项名, 该段应有的项)
    ("fs_cmds", "struct", "DirEntry", "struct", "FileBase64",
     ["DirEntry", "FileContent", "FileBase64"]),
    ("fs_cmds", "fn", "list_dir", "fn", "delete_path",
     ["list_dir", "read_file", "read_file_base64", "write_file", "create_file", "create_dir",
      "delete_path"]),
    ("fs_cmds", "fn", "path_exists", "fn", "rename_path", ["path_exists", "rename_path"]),
    ("project_cmds", "struct", "ProjectInfo", "struct", "ProjectInfo", ["ProjectInfo"]),
    ("project_cmds", "fn", "open_project", "fn", "project_bucket_delete",
     ["open_project", "project_buckets", "project_bucket_delete"]),
    ("project_cmds", "fn", "is_another_instance", "fn", "get_run_targets",
     ["is_another_instance", "get_last_project", "get_projects", "set_project_lang",
      "update_project", "delete_project", "get_run_targets"]),
    ("plugin_cmds", "struct", "ExecuteStatus", "fn", "reload_plugins",
     ["ExecuteStatus", "get_execute_status", "set_execute_entry", "ai_execute_check",
      "lua_translate", "reload_plugins"]),
]

MODS = {
    "fs_cmds": (FS, FS_HEADER, "use std::path::Path;\n", "mod config;\n"),
    "project_cmds": (PROJ, PROJ_HEADER,
                     "use std::path::Path;\nuse std::sync::Mutex;\nuse crate::{config, instance, paths};\n",
                     "mod proc_cmds;\n"),
    "plugin_cmds": (PLG, PLG_HEADER,
                    "use std::path::Path;\nuse std::sync::Mutex;\n"
                    "use crate::{config, paths, plugin, preinstalled, runner};\n",
                    "mod plugin;\n"),
}

REGS = {
    "fs_cmds": ["list_dir", "read_file", "read_file_base64", "write_file", "create_file",
                "create_dir", "delete_path", "path_exists", "rename_path"],
    "project_cmds": ["open_project", "project_buckets", "project_bucket_delete",
                     "is_another_instance", "get_last_project", "get_projects",
                     "set_project_lang", "update_project", "delete_project", "get_run_targets"],
    "plugin_cmds": ["get_execute_status", "set_execute_entry", "ai_execute_check",
                    "lua_translate", "reload_plugins"],
}


def rd(p):
    with io.open(p, encoding="utf-8", newline="") as f:
        return f.read()


def wr(p, t):
    with io.open(p, "w", encoding="utf-8", newline="") as f:
        f.write(t)


def items(src):
    out = set()
    for m in re.finditer(r"(?m)^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?"
                         r"(fn|enum|struct|const|static|type|trait|union|mod)\s+([A-Za-z_]\w*)", src):
        if m.group(1) != "mod":
            out.add(m.group(1) + " " + m.group(2))
    return out


def main():
    main_t = rd(MAIN)
    nl = "\r\n" if "\r\n" in main_t else "\n"
    if "mod fs_cmds;" in main_t:
        print("这一刀已经切过了 —— 这活已经做完")
        return 0
    for p in (FS, PROJ, PLG):
        if os.path.exists(p):
            raise SystemExit("!! %s 已存在但 main.rs 没接线，先弄清状态" % p)

    before = items(main_t) | items(rd(TESTS))
    lines = main_t.split(nl)

    def one(pred, tag):
        hits = [i for i, l in enumerate(lines) if pred(l)]
        if len(hits) != 1:
            raise SystemExit("!! %s 命中 %d 次（应为 1）" % (tag, len(hits)))
        return hits[0]

    def block(kind, name, end_kind, end_name, want):
        pat_s = re.compile(r"^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?%s %s\b" %
                           (kind, re.escape(name)))
        s = one(lambda l: bool(pat_s.match(l)), "%s %s" % (kind, name))
        while s > 0 and (lines[s - 1].startswith("///") or lines[s - 1].startswith("#[")
                         or lines[s - 1].startswith("//")):
            s -= 1
        pat_e = re.compile(r"^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?%s %s\b" %
                           (end_kind, re.escape(end_name)))
        e0 = one(lambda l: bool(pat_e.match(l)), "%s %s" % (end_kind, end_name))
        e = next((i for i in range(e0 + 1, len(lines)) if lines[i] == "}"), None)
        if e is None:
            raise SystemExit("!! %s %s 找不到收尾 `}`" % (end_kind, end_name))
        got = [m.group(1) for m in
               (re.match(r"^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:fn|struct|enum)\s+(\w+)", l)
                for l in lines[s:e + 1]) if m]
        for w in want:
            if w not in got:
                raise SystemExit("!! 段 %s 里没有 %s（实际：%s）" % (name, w, got))
        return (s, e)

    per_mod = {k: [] for k in MODS}
    ranges = []
    for mod, k0, n0, k1, n1, want in RANGES:
        r = block(k0, n0, k1, n1, want)
        per_mod[mod].append(r)
        ranges.append(r)

    for mod, rs in per_mod.items():
        rs.sort()
        for a, b in zip(rs, rs[1:]):
            if a[1] >= b[0]:
                raise SystemExit("!! %s 的段重叠：%s / %s" % (mod, a, b))

    # 1) 三个新模块
    for mod, rs in per_mod.items():
        path, header, imports, _ = MODS[mod]
        body = []
        for i, (s, e) in enumerate(rs):
            if i:
                body.append("")
            body.extend(lines[s:e + 1])
        # 搬过去的命令/结构体统一加 pub（跨模块访问 + tauri 路径注册都要）
        vis = []
        for l in body:
            m = re.match(r"^(async )?fn (\w+)\(", l)
            if m and m.group(2) in REGS[mod] + ["reload_plugins"]:
                vis.append("pub " + l)
            elif re.match(r"^struct (\w+) \{", l):
                vis.append("pub " + l)
            else:
                vis.append(l)
        wr(path, header.replace("\n", nl) + imports.replace("\n", nl) + nl + nl.join(vis) + nl)

    # 2) main.rs：删段 + 注册加前缀 + mod 声明
    rest = list(lines)
    for s, e in sorted(ranges, reverse=True):
        del rest[s:e + 1]
    out = nl.join(rest)
    for mod, cmds in REGS.items():
        for c in cmds:
            if out.count("            %s,\n" % c) != 1:
                raise SystemExit("!! 注册 %s 命中 != 1" % c)
            out = out.replace("            %s,\n" % c, "            %s::%s,\n" % (mod, c), 1)
    for mod, (_, _, _, anchor) in MODS.items():
        if out.count(anchor) != 1:
            raise SystemExit("!! mod 声明锚点 %r 命中 != 1" % anchor)
        out = out.replace(anchor, anchor + "mod %s;\n" % mod, 1)
    wr(MAIN, out)

    after = items(rd(MAIN)) | items(rd(TESTS)) | items(rd(FS)) | items(rd(PROJ)) | items(rd(PLG))
    if after != before:
        raise SystemExit("!! 顶层项集合变了：丢 %s / 多 %s"
                         % (sorted(before - after), sorted(after - before)))
    print("main.rs %d → %d 行；fs_cmds %d / project_cmds %d / plugin_cmds %d 行；注册 %d 条（守恒 ✓）"
          % (len(lines), len(rd(MAIN).split(nl)), len(rd(FS).split(nl)),
             len(rd(PROJ).split(nl)), len(rd(PLG).split(nl)), sum(len(v) for v in REGS.values())))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
