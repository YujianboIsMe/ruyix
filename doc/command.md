# 命令系统

## 三级命令结构

```
  用户输入 "把窗口改成 900×600"
          │
          ▼
    ┌─────────────────────┐
    │ 一级：标准命令解析     │  open/close/config 严格匹配
    │ 命中了 → 直接执行 ✅   │
    └─────────┬───────────┘
              │ 未命中
              ▼
    ┌─────────────────────┐
    │ 二级：本地缓存匹配     │  遍历历史 LLM 返回的正则模板
    │ 命中了 → 提取参数执行 ✅│
    └─────────┬───────────┘
              │ 未命中
              ▼
    ┌─────────────────────┐
    │ 三级：LLM API 调用    │  DeepSeek/通义千问 → 返回 {action, params, pattern}
    │ 执行 + 写入本地缓存    │  下次类似表达走二级
    └─────────────────────┘
```
目前只支持标准命令和AI命令。
第二级缓存暂不实现。
此外，让大模型进行意图识别，如果是闲聊。简单回复20字以内的提供情绪价值的话语。
如果回复预计会超过20字，则回复“你还是好好工作吧，房贷还清了吗？车贷还清了吗？”
## open 命令
目前有以下子命令：
1. project 打开项目。顶部窗口显示项目的绝对路径或项目名。
2. file
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
config add -p darkhorse.code.run.target0.cmd="C:\Users\yujia\PycharmProjects\rag\.venv\Scripts\python.exe C:\Users\yujia\PycharmProjects\rag\src\P05-es\pdf2es.py"
config add -p darkhorse.code.run.target0.name="pdf转储es"
config add -r darkhorse.code.run.target0.cmd="C:\Users\yujia\PycharmProjects\rag\.venv\Scripts\python.exe C:\Users\yujia\PycharmProjects\rag\src\P05-es\pdf2es.py"
config add -r darkhorse.code.run.target0.name="pdf转储es"
```

## AI命令
AI命令以！或!(中英文感叹号)开头，可以使用自然语言，比如：
```
！把窗口大小调整到900*600
```
本阶段AI命令暂不实现。
但是在命令输入框里改下提示。
旧提示
> 输入命令，回车执行...
新提示
> 输入命令，！/!开头可以输入AI命令，回车执行...