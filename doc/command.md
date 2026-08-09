# 命令系统

## 二级命令结构

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
