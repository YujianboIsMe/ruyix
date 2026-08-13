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

层级举例
┌─────────────────────────────────┬──────────┬──────────────┐
│               key               │ section  │   key        │
├─────────────────────────────────┼──────────┼──────────────┤
│ darkhorse.code.projects.current │ projects │ current      │
├─────────────────────────────────┼──────────┼──────────────┤
│ darkhorse.code.projects.list    │ projects │ list         │
├─────────────────────────────────┼──────────┼──────────────┤
│ darkhorse.code.run.target0.cmd  │ run      │ target0.cmd  │
├─────────────────────────────────┼──────────┼──────────────┤
│ darkhorse.code.run.target0.name │ run      │ target0.name │
└─────────────────────────────────┴──────────┴──────────────┘

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


## 配置项
### 项目

为了避免每次都打开项目。
所以在~/.darkhorse/code/projects.toml里保存项目
项目配置项举例（标准运行时）
```
darkhorse.code.projects.current="C:\Users\yujia\PycharmProjects\tmf"
darkhorse.code.projects.list=["C:\Users\yujia\PycharmProjects\rag","C:\Users\yujia\PycharmProjects\tmf"]
```

### 运行
运行目标通过配置来持久化
运行目标不能保存为全局，如果遇到保存全局运行命令，则报错【运行目标不能保存为全局】！
运行目标需要以配置的形式持久化。
./.darkhorse/code/run.toml里保存运行目标
配置项举例（标准运行时）
```
darkhorse.code.run.target0.cmd="C:\Users\yujia\PycharmProjects\rag\.venv\Scripts\python.exe C:\Users\yujia\PycharmProjects\rag\src\P05-es\pdf2es.py"
darkhorse.code.run.target0.name="pdf转储es"
darkhorse.code.run.target0.bind=src\P05-es\pdf2es.py
```
target0 是用户随意取的名字，不是系统递增的（IDE不维护计数器）。
只有IDE自动创建运行目标时，才遍历target开头的code/key，然后取最大的数字+1。

### AI
AI配置同样遵循配置四级配置机制。

配置项
darkhorse.code.ai.api_key
darkhorse.code.ai.api_url
darkhorse.code.ai.model
darkhorse.code.ai.alias 如果没有指定，则取darkhorse.code.ai.model

### RAG（智搜）
RAG 配置不走四层配置系统，独立存放于两个 rag.toml：

全局配置 `~/.darkhorse/code/rag.toml`（扁平键，无 darkhorse.code 前缀）：
```
enabled = true
permanently_disabled = false
api_url = "https://api.deepseek.com/v1/embeddings"
api_key = "sk-xxxx"            # 可选，缺省复用 darkhorse.code.ai.api_key
model = "deepseek-embedding"   # 可选，嵌入模型名
dim = 1024                     # 可选，向量维度，默认 1024
```

项目配置 `./.darkhorse/code/rag.toml`（索引状态，由系统自动维护）：
```
enabled = true
last_indexed = "1786526560"
files_count = 312

[files]
"src/main/Calc.java" = "a1b2c3d4"
```

修改 api_url / dim 的入口：菜单栏【智搜】→ 确认弹窗 → 接受。
修改 dim 后旧向量库自动清空，需要全量重建索引。