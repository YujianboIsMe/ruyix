# 配置

配置系统分为三级
- 全局配置
- 项目配置
- 环境配置
- 运行配置

配置有固定前缀darkhorse.code
在toml文件里可以省略darkhorse.code前缀
toml文件的文件名也是前缀的一部分
但是为了配置统一，读取toml文件加载到运行时配置结构体后要补上darkhorse.code前缀

## 全局配置
位于~/.darkhorse/code
格式为toml文件

## 项目配置
位于./.darkhorse/code
格式为toml文件
## 环境配置
启动darkhorse-code.exe时带的命令行参数
待定
## 运行配置
使用命令系统修改的配置。
待定



## 配置项
### 项目

为了避免每次都打开项目。
所以在~/.darkhorse/code/projects.toml里保存项目
项目配置项举例（标准运行时）
```
darkhorse.code.projects.current="C:\Users\yujia\PycharmProjects\tmf"
darkhorse.code.projects.list=["C:\Users\yujia\PycharmProjects\rag","C:\Users\yujia\PycharmProjects\tmf"]
```