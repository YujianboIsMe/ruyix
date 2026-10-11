#!/usr/bin/env python3
"""生成 ruyix 命令翻译 LoRA 微调数据集（dataset.jsonl）。

数据格式与 src-tauri/src/ai.rs::translate() 的真实输入输出严格一致：
- system      = src-tauri/src/command.md（训练脚本运行时注入，本文件不重复存储）
- user        = ["当前项目路径: <path>\n\n"]? + 用户自然语言
- assistant   = "---COMMAND---\n<DSL>\n---LUA---\n<Lua>"，或纯文本（闲聊/不支持）

每行 JSON：{"project_path": str|null, "input": str, "output": str}

用法：python3 training/dataset/build_dataset.py
"""

import json
from pathlib import Path

# ============================================
# 用例登记
# ============================================

EX = []


def op(user, command, lua, path=None):
    """操作请求：assistant 必须同时产出 DSL 与 Lua（command.md 规则 2）。"""
    EX.append({
        "project_path": path,
        "input": user,
        "output": f"---COMMAND---\n{command}\n---LUA---\n{lua}",
    })


def plain(user, reply, path=None):
    """纯文本回复：闲聊（规则 4）或不支持的操作（规则 3），无标记无 Lua。"""
    EX.append({"project_path": path, "input": user, "output": reply})


# ---------- open project ----------

op(
    "打开项目 D:\\projects\\my-app",
    "open project D:\\projects\\my-app",
    'if input:match("打开项目%s+(.+)") then\n'
    '  return "open project " .. input:match("打开项目%s+(.+)")\n'
    "end",
)
op(
    "帮我打开项目 /Users/mac/PycharmProjects/newest-ai-code",
    "open project /Users/mac/PycharmProjects/newest-ai-code",
    'if input:match("打开项目%s+(.+)") then\n'
    '  return "open project " .. input:match("打开项目%s+(.+)")\n'
    "end",
)
op(
    "切换到项目 D:\\work\\ruyix",
    "open project D:\\work\\ruyix",
    'if input:match("切换到项目%s+(.+)") then\n'
    '  return "open project " .. input:match("切换到项目%s+(.+)")\n'
    "end",
)
op(
    "open project /home/dev/webapp",
    "open project /home/dev/webapp",
    'if input:match("[Oo][Pp][Ee][Nn]%s+project%s+(.+)") then\n'
    '  return "open project " .. input:match("[Oo][Pp][Ee][Nn]%s+project%s+(.+)")\n'
    "end",
)
op(
    "换个项目：D:\\games\\demo",
    "open project D:\\games\\demo",
    'if input:match("项目[：:]%s*(%S.*)") and input:match("换") then\n'
    '  return "open project " .. input:match("项目[：:]%s*(%S.*)")\n'
    "end",
)

# ---------- open file ----------

op(
    "打开文件 src/main.rs",
    "open file src/main.rs",
    'if input:match("打开文件%s+(.+)") then\n'
    '  return "open file " .. input:match("打开文件%s+(.+)")\n'
    "end",
)
op(
    "打开文件 ui/index.html",
    "open file ui/index.html",
    'if input:match("打开文件%s+(.+)") then\n'
    '  return "open file " .. input:match("打开文件%s+(.+)")\n'
    "end",
    path="D:\\projects\\ruyix",
)
op(
    "帮我看看 ui/command.js",
    "open file ui/command.js",
    'if input:match("看看%s+([%w%.%-%_/\\]+)") then\n'
    '  return "open file " .. input:match("看看%s+([%w%.%-%_/\\]+)")\n'
    "end",
)
op(
    "打开 README.md",
    "open file README.md",
    'if input:match("打开%s-([%w_%.%-%/\\]+%.[%w]+)%s*$") then\n'
    '  return "open file " .. input:match("打开%s-([%w_%.%-%/\\]+%.[%w]+)%s*$")\n'
    "end",
)
op(
    "查看 package.json",
    "open file package.json",
    'if input:match("查看%s+([%w%.%-%_/\\]+%.[%w]+)") then\n'
    '  return "open file " .. input:match("查看%s+([%w%.%-%_/\\]+%.[%w]+)")\n'
    "end",
)
op(
    "open file src/lib/utils.py",
    "open file src/lib/utils.py",
    'if input:match("[Oo][Pp][Ee][Nn]%s+file%s+(.+)") then\n'
    '  return "open file " .. input:match("[Oo][Pp][Ee][Nn]%s+file%s+(.+)")\n'
    "end",
)

# ---------- close project / tabs ----------

op(
    "关闭项目",
    "close project",
    'if input:match("关闭?项目") or input:match("关掉.*项目") then\n'
    '  return "close project"\n'
    "end",
)
op(
    "关掉当前项目",
    "close project",
    'if input:match("关掉.*项目") or input:match("关闭?项目") then\n'
    '  return "close project"\n'
    "end",
)
op(
    "关闭所有标签页",
    "close all",
    'if input:match("关闭.*所有") or input:match("标签.*全关") then\n'
    '  return "close all"\n'
    "end",
)
op(
    "把标签全关了",
    "close all",
    'if input:match("标签.*全关") or input:match("关闭.*所有") then\n'
    '  return "close all"\n'
    "end",
)
op(
    "关闭第0个标签",
    "close 0",
    'if input:match("关闭?第(%d+)个") then\n'
    '  return "close " .. input:match("关闭?第(%d+)个")\n'
    "end",
)
op(
    "关闭第2个标签页",
    "close 2",
    'if input:match("关闭?第(%d+)个") then\n'
    '  return "close " .. input:match("关闭?第(%d+)个")\n'
    "end",
)
op(
    "关掉最后一个标签",
    "close -1",
    'if input:match("最后") then\n'
    '  return "close -1"\n'
    "end",
)
op(
    "关闭倒数第二个标签",
    "close -2",
    'if input:match("倒数第二") then\n'
    '  return "close -2"\n'
    "end",
)
op(
    "关闭其他标签页",
    "close other",
    'if input:match("关闭?其他") then\n'
    '  return "close other"\n'
    "end",
)
op(
    "关闭左边的标签",
    "close left",
    'if input:match("关闭?左边") then\n'
    '  return "close left"\n'
    "end",
)
op(
    "把右边的标签都关了",
    "close right",
    'if input:match("右边.*关") then\n'
    '  return "close right"\n'
    "end",
)

# ---------- new（带类型的快捷创建） ----------

op(
    "帮我创建python文件叫hello",
    "new py hello",
    'if input:match("创建.*[Pp]ython.*文件") or input:match("新建.*[Pp]ython") then\n'
    '  local name = input:match("叫(%S+)") or input:match("名为(%S+)") or input:match("文件%s+(%S+)")\n'
    '  return "new py " .. (name or "untitled")\n'
    "end",
)
op(
    "新建一个rust文件，名字是server",
    "new rs server",
    'if input:match("新建.*rust") or input:match("创建.*[Rr]ust") then\n'
    '  local name = input:match("名字是(%S+)") or input:match("叫(%S+)") or input:match("名为(%S+)")\n'
    '  return "new rs " .. (name or "untitled")\n'
    "end",
)
op(
    "帮我建个markdown文件叫notes",
    "new md notes",
    'if input:match("建.*markdown") or input:match("新建.*md") then\n'
    '  local name = input:match("叫(%S+)") or input:match("名为(%S+)")\n'
    '  return "new md " .. (name or "untitled")\n'
    "end",
)
op(
    "创建c文件叫main",
    "new c main",
    'if input:match("创建.*c文件") or input:match("新建.*c文件") then\n'
    '  local name = input:match("叫(%S+)") or input:match("名为(%S+)")\n'
    '  return "new c " .. (name or "untitled")\n'
    "end",
)
op(
    "新建Python文件 utils",
    "new py utils",
    'if input:match("新建.*[Pp]ython") then\n'
    '  local name = input:match("[Pp]ython文件%s+(%S+)") or input:match("叫(%S+)")\n'
    '  return "new py " .. (name or "untitled")\n'
    "end",
)
op(
    "create a rust file called demo",
    "new rs demo",
    'if input:match("[Cc]reate.*rust.*file") or input:match("new rust file") then\n'
    '  local name = input:match("called%s+(%S+)") or input:match("named%s+(%S+)")\n'
    '  return "new rs " .. (name or "untitled")\n'
    "end",
)
op(
    "新文档 meeting",
    "new md meeting",
    'if input:match("新文档%s+(%S+)") then\n'
    '  return "new md " .. input:match("新文档%s+(%S+)")\n'
    "end",
)

# ---------- new file / folder ----------

op(
    "创建文件 src/lib/helper.py",
    "new file src/lib/helper.py",
    'if input:match("创建文件%s+(.+)") then\n'
    '  return "new file " .. input:match("创建文件%s+(.+)")\n'
    "end",
)
op(
    "新建文件 docs/api.md",
    "new file docs/api.md",
    'if input:match("新建文件%s+(.+)") then\n'
    '  return "new file " .. input:match("新建文件%s+(.+)")\n'
    "end",
)
op(
    "新建文件夹 docs/api",
    "new folder docs/api",
    'if input:match("新建?(文件夹|目录)%s+(.+)") then\n'
    '  return "new folder " .. input:match("新建?(文件夹|目录)%s+(.+)")\n'
    "end",
)
op(
    "创建目录 logs",
    "new dir logs",
    'if input:match("创建目录%s+(.+)") then\n'
    '  return "new dir " .. input:match("创建目录%s+(.+)")\n'
    "end",
)
op(
    "new folder test/fixtures",
    "new folder test/fixtures",
    'if input:match("new%s+folder%s+(.+)") then\n'
    '  return "new folder " .. input:match("new%s+folder%s+(.+)")\n'
    "end",
)

# ---------- rename ----------

op(
    "把a.txt重命名为b.txt",
    "rename a.txt b.txt",
    'if input:match("重命名") then\n'
    '  local old = input:match("把%s*(%S+)")\n'
    '  local new = input:match("为%s*(%S+)")\n'
    '  if old and new then return "rename " .. old .. " " .. new end\n'
    "end",
)
op(
    "重命名 utils.rs 为 tools.rs",
    "rename utils.rs tools.rs",
    'if input:match("重命名%s+(%S+)%s+为%s+(%S+)") then\n'
    '  local old, new = input:match("重命名%s+(%S+)%s+为%s+(%S+)")\n'
    '  return "rename " .. old .. " " .. new\n'
    "end",
)
op(
    "mv old.json new.json",
    "mv old.json new.json",
    'if input:match("^mv%s+(%S+)%s+(%S+)") then\n'
    '  local old, new = input:match("^mv%s+(%S+)%s+(%S+)")\n'
    '  return "mv " .. old .. " " .. new\n'
    "end",
)

# ---------- delete ----------

op(
    "删除测试目录",
    "del 测试",
    'if input:match("删除") then\n'
    '  local path = input:match("删除%s*(%S+)")\n'
    '  if path then return "del " .. path end\n'
    "end",
)
op(
    "删除 temp.log",
    "del temp.log",
    'if input:match("删除%s+(%S+)") then\n'
    '  return "del " .. input:match("删除%s+(%S+)")\n'
    "end",
)
op(
    "删掉build文件夹",
    "del build",
    'if input:match("删掉%s*([%w%-%_%.]+)") then\n'
    '  return "del " .. input:match("删掉%s*([%w%-%_%.]+)")\n'
    "end",
)
op(
    "移除 src/old.rs",
    "rm src/old.rs",
    'if input:match("移除%s+(%S+)") then\n'
    '  return "rm " .. input:match("移除%s+(%S+)")\n'
    "end",
)
op(
    "去掉node_modules目录",
    "del node_modules",
    'if input:match("去掉%s+([%w%-%_%.]+)") then\n'
    '  return "del " .. input:match("去掉%s+([%w%-%_%.]+)")\n'
    "end",
    path="/Users/mac/PycharmProjects/newest-ai-code",
)

# ---------- refresh ----------

op(
    "刷新文件树",
    "refresh",
    'if input:match("刷新") and not input:match("刷新%s+%S") then\n'
    '  return "refresh"\n'
    "end",
)
op(
    "更新文件列表",
    "refresh",
    'if input:match("更新文件列表") then\n'
    '  return "refresh"\n'
    "end",
)
op(
    "刷新一下docs目录",
    "refresh docs",
    'if input:match("刷新.-([%w%-%_%.]+)目录") then\n'
    '  return "refresh " .. input:match("刷新.-([%w%-%_%.]+)目录")\n'
    "end",
)
op(
    "外部工具改了文件，刷新",
    "refresh",
    'if input:match("刷新") then\n'
    '  return "refresh"\n'
    "end",
    path="D:\\projects\\ruyix",
)

# ---------- config ----------

op(
    "配置API密钥 sk-abc123",
    "config add -g ruyix.code.ai.api_key sk-abc123",
    'if input:match("[Aa][Pp][Ii].*密钥") then\n'
    '  local key = input:match("(sk-%S+)")\n'
    '  if key then return "config add -g ruyix.code.ai.api_key " .. key end\n'
    "end",
)
op(
    "config add -g ruyix.code.ai.model qwen2.5-1.5b-instruct",
    "config add -g ruyix.code.ai.model qwen2.5-1.5b-instruct",
    'if input:match("^config%s+") then\n'
    "  return input\n"
    "end",
)
op(
    "查看当前模型配置",
    "config get ruyix.code.ai.model",
    'if input:match("查看.*模型") or input:match("当前模型") then\n'
    '  return "config get ruyix.code.ai.model"\n'
    "end",
)
op(
    "读取全局API地址",
    "config get -g ruyix.code.ai.api_url",
    'if input:match("[Aa][Pp][Ii]地址") then\n'
    '  return "config get -g ruyix.code.ai.api_url"\n'
    "end",
)
op(
    "更新模型为 deepseek-chat",
    "config update -g ruyix.code.ai.model deepseek-chat",
    'if input:match("更新?模型为%s+(%S+)") then\n'
    '  return "config update -g ruyix.code.ai.model " .. input:match("更新?模型为%s+(%S+)")\n'
    "end",
)
op(
    "删除项目级AI配置 ruyix.code.ai.alias",
    "config remove -p ruyix.code.ai.alias",
    'if input:match("删除.*配置%s+(ruyix%.code%.%S+)") then\n'
    '  return "config remove -p " .. input:match("删除.*配置%s+(ruyix%.code%.%S+)")\n'
    "end",
)
op(
    "config get ruyix.code.run.target",
    "config get ruyix.code.run.target",
    'if input:match("^config%s+") then\n'
    "  return input\n"
    "end",
)

# ---------- run targets ----------

op(
    "添加运行目标 构建=cargo build",
    "run 构建=cargo build",
    'if input:match("运行目标%s*(.+)=(.+)") or input:match("添加运行目标%s+(.+)") then\n'
    '  local pair = input:match("运行目标%s*(%S+=%S+.*)")\n'
    '  if pair then return "run " .. pair end\n'
    "end",
)
op(
    "run dev=npm run dev",
    "run dev=npm run dev",
    'if input:match("^run%s+(%S+=.+)") then\n'
    "  return input\n"
    "end",
)
op(
    "设置启动命令 dev=pnpm run dev",
    "run dev=pnpm run dev",
    'if input:match("命令%s+(%S+=%S+.*)") then\n'
    '  return "run " .. input:match("命令%s+(%S+=%S+.*)")\n'
    "end",
)
op(
    "删除运行目标 构建",
    "run del 构建",
    'if input:match("删除运行目标%s+(%S+)") then\n'
    '  return "run del " .. input:match("删除运行目标%s+(%S+)")\n'
    "end",
)

# ---------- help ----------

op(
    "怎么用这个IDE",
    "help",
    'if input:match("怎么用") or input:match("如何使用") then\n'
    '  return "help"\n'
    "end",
)
op(
    "打开帮助页",
    "help",
    'if input:match("帮助") then\n'
    '  return "help"\n'
    "end",
)
op(
    "help",
    "help",
    'if input:match("^help$") then\n'
    '  return "help"\n'
    "end",
)

# ---------- git（规则 5：原样输出 git 命令） ----------

op(
    "git status",
    "git status",
    'if input:match("^git%s+") then\n'
    "  return input\n"
    "end",
)
op(
    "查看git状态",
    "git status",
    'if input:match("查看.*[Gg]it.*状态") or input:match("[Gg]it状态") then\n'
    '  return "git status"\n'
    "end",
)
op(
    "看看状态",
    "git status",
    'if input:match("看看?状态") then\n'
    '  return "git status"\n'
    "end",
    path="D:\\projects\\ruyix",
)
op(
    "git add -A",
    "git add -A",
    'if input:match("^git%s+") then\n'
    "  return input\n"
    "end",
)
op(
    "暂存所有改动",
    "git add -A",
    'if input:match("暂存.*所有") or input:match("暂存.*全部") then\n'
    '  return "git add -A"\n'
    "end",
)
op(
    "git add src/main.rs",
    "git add src/main.rs",
    'if input:match("^git%s+") then\n'
    "  return input\n"
    "end",
)
op(
    "暂存main.rs",
    "git add main.rs",
    'if input:match("暂存%s+([%w%-%_%.%/]+)") then\n'
    '  return "git add " .. input:match("暂存%s+([%w%-%_%.%/]+)")\n'
    "end",
)
op(
    "提交代码，信息是\"修复登录bug\"",
    'git commit -m "修复登录bug"',
    'if input:match("提交") then\n'
    '  local msg = input:match("[\\"”]([^\\"”]+)[\\"”]")\n'
    "  if msg then return 'git commit -m \"' .. msg .. '\"' end\n"
    "end",
)
op(
    "git commit -m \"docs: update readme\"",
    'git commit -m "docs: update readme"',
    'if input:match("^git%s+") then\n'
    "  return input\n"
    "end",
)
op(
    "推送",
    "git push",
    'if input:match("^推") then\n'
    '  return "git push"\n'
    "end",
)
op(
    "git push",
    "git push",
    'if input:match("^git%s+") then\n'
    "  return input\n"
    "end",
)
op(
    "push my code",
    "git push",
    'if input:match("[Pp]ush") then\n'
    '  return "git push"\n'
    "end",
)
op(
    "拉取最新代码",
    "git pull",
    'if input:match("拉取") then\n'
    '  return "git pull"\n'
    "end",
)
op(
    "看看提交历史",
    "git log",
    'if input:match("提交历史") or input:match("提交记录") then\n'
    '  return "git log"\n'
    "end",
)
op(
    "git diff",
    "git diff",
    'if input:match("^git%s+") then\n'
    "  return input\n"
    "end",
)
op(
    "有什么改动",
    "git diff",
    'if input:match("有什么改动") or input:match("哪些改动") then\n'
    '  return "git diff"\n'
    "end",
)
op(
    "show me the diff",
    "git diff",
    'if input:match("[Dd]iff") then\n'
    '  return "git diff"\n'
    "end",
)
op(
    "新建分支 feature/login",
    "git branch feature/login",
    'if input:match("新建分支%s+(%S+)") then\n'
    '  return "git branch " .. input:match("新建分支%s+(%S+)")\n'
    "end",
)
op(
    "切换分支 dev",
    "git checkout dev",
    'if input:match("切换分支%s+(%S+)") then\n'
    '  return "git checkout " .. input:match("切换分支%s+(%S+)")\n'
    "end",
)
op(
    "git stash",
    "git stash",
    'if input:match("^git%s+") then\n'
    "  return input\n"
    "end",
)
op(
    "暂存当前工作区",
    "git stash",
    'if input:match("暂存当前工作区") or input:match("先存起来") then\n'
    '  return "git stash"\n'
    "end",
)

# ---------- 多命令（每行一条） ----------

op(
    "创建rust文件demo然后打开它",
    "new rs demo\nopen file demo.rs",
    'if input:match("创建.*rust.*然后.*打开") then\n'
    '  local name = input:match("叫(%S+)") or input:match("文件(%S+)")\n'
    '  name = name or "demo"\n'
    '  return "new rs " .. name .. "\\nopen file " .. name .. ".rs"\n'
    "end",
)
op(
    "新建文件src/a.py并打开",
    "new file src/a.py\nopen file src/a.py",
    'if input:match("新建文件(%S+)并打开") then\n'
    '  local p = input:match("新建文件(%S+)并打开")\n'
    '  return "new file " .. p .. "\\nopen file " .. p\n'
    "end",
)
op(
    "关掉所有标签再打开README.md",
    "close all\nopen file README.md",
    'if input:match("关.*所有.*打开") then\n'
    '  local f = input:match("打开%s-([%w_%.%-%/\\]+%.[%w]+)")\n'
    '  if f then return "close all\\nopen file " .. f end\n'
    "end",
)

# ---------- 不支持的操作（规则 3：纯文本，无 Lua） ----------

plain("帮我写一个红黑树实现", "不支持的操作：代码生成不在命令翻译范围内")
plain("重启电脑", "不支持的操作：无法控制系统")
plain("订一杯奶茶", "不支持的操作：臣妾做不到")
plain("帮我发邮件给老板", "不支持的操作：无法访问邮箱")
plain("格式化整个磁盘", "不支持的操作：危险操作被拒绝")

# ---------- 闲聊（规则 4：≤20 字，超长回固定调侃） ----------

plain("你好", "你好")
plain("你是谁", "我是ruyix命令助手")
plain("你能干什么", "把自然语言翻译成IDE命令")
plain("下班了吗", "还没，你先忙")
plain("今晚有空吗", "你还是好好工作，房贷还清了吗？车贷还清了吗？")
plain("今天天气怎么样", "你还是好好工作，房贷还清了吗？车贷还清了吗？")
plain(
    "你好呀",
    "你好",
    path="D:\\projects\\ruyix",
)

# ============================================
# 结构校验（command.md 的 Lua 规范）
# ============================================


def check_lua(lua: str) -> list[str]:
    """启发式校验：无 else、无禁用函数、if/end 配平。"""
    problems = []
    if "else" in lua:
        problems.append("含 else 分支（不匹配应自然落空）")
    for banned in ("os.", "io.", "require", "loadfile", "dofile", "load("):
        if banned in lua:
            problems.append(f"使用禁用调用: {banned}")
    opens = sum(lua.count(k) for k in ("if ", "for ", "while ", "function"))
    if opens != lua.count("end"):
        problems.append(f"块配平失败: {opens} 个块开启 vs {lua.count('end')} 个 end")
    return problems


def main() -> None:
    errors = 0
    for i, row in enumerate(EX):
        if "---LUA---" not in row["output"]:
            continue
        lua = row["output"].split("---LUA---", 1)[1]
        for problem in check_lua(lua):
            errors += 1
            print(f"[用例 {i}] {row['input']!r} → {problem}")
    if errors:
        raise SystemExit(f"校验失败：{errors} 个问题")

    out = Path(__file__).parent / "dataset.jsonl"
    with out.open("w", encoding="utf-8") as f:
        for row in EX:
            f.write(json.dumps(row, ensure_ascii=False) + "\n")

    ops_count = sum(1 for r in EX if "---COMMAND---" in r["output"])
    ctx_count = sum(1 for r in EX if r["project_path"])
    print(f"OK: {len(EX)} 条用例（操作 {ops_count} / 纯文本 {len(EX) - ops_count}，"
          f"带项目路径上下文 {ctx_count}）→ {out}")


if __name__ == "__main__":
    main()
