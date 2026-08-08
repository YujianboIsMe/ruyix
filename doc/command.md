# 命令系统

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


