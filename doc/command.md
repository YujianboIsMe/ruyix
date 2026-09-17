# 命令系统

## 三级命令结构

```
  用户输入 “把窗口改成 900×600”
          │
          ▼
    ┌──────────────────────────┐
    │ 一级：标准命令解析         │  open/close/config/run 严格匹配
    │ 命中了 → 直接执行 ✅       │
    └─────────┬────────────────┘
              │ 未命中
              ▼
    ┌──────────────────────────┐
    │ 二级：Lua 脚本匹配         │  项目级 learn.lua，沙箱执行
    │ 命中了 → 返回标准命令 ✅   │  微秒级，零网络开销
    └─────────┬────────────────┘
              │ 未命中
              ▼
    ┌──────────────────────────┐
    │ 三级：LLM API 调用        │  DeepSeek/通义千问
    │ 翻译 + 更新 learn.lua     │  下次类似表达走二级
    └──────────────────────────┘
```

### 设计目标

- **二级（Lua）是第一道缓存**：LLM 每次翻译成功后，不仅返回标准命令，还生成一段 Lua 模式匹配代码追加到 `learn.lua`。下次相同或类似的自然语言输入被 Lua 捕获，不再调用 LLM。
- **逐项目学习**：`learn.lua` 存放在项目根目录的 `.ruyix/code/learn.lua`，每个项目的习惯独立积累。
- **渐进升级**：脚本从简单的字符串匹配开始，随着 LLM 不断更新，逐步覆盖更多模式。
- **Lua 选择理由**：
  - 解释器可嵌入 Rust 二进制（mlua），零外部依赖
  - 沙箱安全可控（`os`/`io` 已剥离）
  - LLM 训练数据中 Lua 代码量大，生成正确率高
  - 启动 < 1ms，执行微秒级

### 当前状态

一级（标准命令）和三级（LLM）已实现。
二级（Lua 脚本）待实现。

---

目前只支持标准命令和AI命令。
此外，让大模型进行意图识别，如果是闲聊。简单回复20字以内的提供情绪价值的话语。
如果回复预计会超过20字，则回复”你还是好好工作吧，房贷还清了吗？车贷还清了吗？”

## AI上下文
如果是打开项目的状态，把当前项目目录完整路径作为上下文发送给AI。
这样在用户提出简短的问题后，AI可以推测出完整的绝对路径。

## open 命令
目前有以下子命令：
1. project 打开/切换项目。顶部窗口显示项目的绝对路径或项目名。
   已打开其他项目时，open project 会自动关闭当前项目并切换（无需先 close project）；打开的是当前项目则直接提示。
2. file，禁止打开绝对路径文件，只能打开相对路径。
示例：
```
open project C:\Users\yujia\PycharmProjects\tmf
```
打开项目后顶部窗口显示项目的绝对路径，在窗口宽度缩小后显示项目名。
例如:
- 窗口宽度大时显示`C:\Users\yujia\PycharmProjects\tmf`
- 窗口宽度小时显示`stuty-assistent`

## close 命令
目前有以下子命令
- project，关闭项目
- all，关闭编辑区已打开的所有文件
- index, 按序号关闭文件
- other或others，关闭编辑区除当前文件外的所有文件
- left，关闭编辑区除当前文件左边的所有文件
- right，关闭编辑区除当前文件右边的所有文件
例如：
```
close project
```
关闭项目后，窗口标题恢复为`Darkhorse Code`

### close index
index已0开始，从左到右
如果index是负数，则以-1开始，从右到左
假如编辑器打开了五个文件,关闭第二个文件为`close 1`或`close -4` 

## config 命令

子命令
- add 添加配置，已存在则报错
- remove/delete 删除配置，不存在则报错
- update 修改配置，不存在则添加
- get 获取配置，不存在则报错

以上四个子命令都可以配合命令级别选项
- -g 全局配置，刷新运行时，并保存在全局配置
- -p 项目配置，刷新运行时，并保存在项目配置
- -r 运行配置，只刷新运行时，不保存

举例
``` 
config add -p ruyix.code.run.target0.cmd="C:\Users\yujia\PycharmProjects\rag\.venv\Scripts\python.exe C:\Users\yujia\PycharmProjects\rag\src\P05-es\pdf2es.py"
config add -p ruyix.code.run.target0.name="pdf转储es"
config add -r ruyix.code.run.target0.cmd="C:\Users\yujia\PycharmProjects\rag\.venv\Scripts\python.exe C:\Users\yujia\PycharmProjects\rag\src\P05-es\pdf2es.py"
config add -r ruyix.code.run.target0.name="pdf转储es"
```

## new 命令
用法: new <子命令> <相对路径>
相对路径基于项目根目录
禁止使用绝对路径:
- 发现 /开头，立即报错：“您没有权限！”
- 发现C:/ D:/开头，立即报错“您没有权限！”
### 子命令 
- file 创建文件
- folder/dir 创建目录
### 类型子命令
类型子命令可以让相对路径省略后缀。
- py 创建.py文件
- rs 创建.rs文件
- md 创建.md文件
- c 创建.c 文件
类型子命令必须限定范围，假如new png my_photo 这样会创建出一个无法查看的图片。
所以目前只支持上述类型子命令。

## del/delete/remove/rm 命令
用法: del/delete/remove/rm <相对路径>
需要用户弹窗确认
这四个命令都是删除文件或文件夹
禁止使用绝对路径:
- 发现 /开头，立即报错：“您没有权限！”
- 发现C:/ D:/开头，立即报错“您没有权限！”

## run 命令

语法分两种：
1. run <name\>\=<cmd>
2. run del/delete/remove/rm <name\>

### 添加run
这是一个快捷命令，先转化为两条标准命令
config add -p ruyix.code.run.target<index>.cmd=<cmd> 
config add -p ruyix.code.run.target<index>.name=<name\>
<index> 怎么计算呢？
很简单，max(已有索引) + 1。

### 删除run
这是删除运行目标的命令。
用法：
```
run del <名称>
run delete <名称>
run remove <名称>
run rm <名称>
```
删除时按名称或 key 匹配，找到后删除对应的 `.cmd` 和 `.name` 两条配置。
示例：
```
run del 构建
run rm 测试
```



## rename/mv 命令
功能：重命名
格式 rename <old-path> <new-path>
校验：
1. 禁止绝对路径文件，只能相对路径。
2. 不能含有非法字符
3. 不能与现有文件/目录重复

## project 命令
项目属性管理。

子命令：
- lang 设置项目语言
- edit 修改项目名称与图标（路径不可修改）
- delete 从项目列表删除项目
- migrate 迁移旧版项目配置

### project lang
```
project lang <语言> <项目路径>
```
语言可选值：unknown/mix/java/c/python/rust/web/golang/document/kotlin
（见[config.md](config.md) 的语言表）
路径取 lang 之后的剩余部分，可含空格。

### project edit
```
project edit "<项目路径>" "<名称>" <语言>
```
路径与名称须加引号（可含空格），语言是最后一个参数。
项目条目上的 ✍️ 按钮打开弹窗修改，确认后即执行此命令。

### project delete
```
project delete <项目路径>
```
从项目列表移除条目（不删除项目文件夹）。
路径取 delete 之后的剩余部分，可含空格，外层引号会自动去除。
项目条目上的 🗑️ 按钮经确认弹窗后执行此命令。

### project migrate
```
project migrate
```
把旧版项目配置（纯路径列表）迁移为新格式（name/path/lang），
旧项目语言全部默认 unknown。
等价于菜单【项目→迁移配置】。