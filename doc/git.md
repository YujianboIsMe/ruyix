# Git
作为一款开发工具，GIT是必须的功能
右边的大纲区做成多标签
第一个标签：大纲/Outline
第二个标签：Git

## 界面

界面上下布局：
1. 按钮区：只有三个按钮：Commit✅ Pull📥 Push📤
2. 输入区：输入commit message
3. staged:文件树组件
4. unstaged：文件树组件

### unstaged
这个小区域，标题右边一个
标题右边区域，有两个按钮：
- 🔄️，点击刷新STAGED
- ➕，点击就全部变成STAGED

然后每个文件树下每个条目后面都用一个➕，点击就stage这个文件夹或文件


## 非仓库状态

打开的项目不是 git 仓库时，面板不显示英文报错（原先会原样打印
`fatal: not a git repository ...`），而是展示**仓库初始化引导**：

1. 提示「当前项目还不是 Git 仓库」
2. 【初始化仓库】→ `git init`（走命令系统 `git <子命令>` 透传，无新增命令动词）
3. 【添加远程仓库】→ 弹窗收集地址后 `git remote add origin <url>`；
   已存在 `origin` 时改用 `git remote set-url` 覆盖

初始化前【添加远程仓库】为禁用态（`git remote` 必须在仓库内执行）。

判定依据：后端 `git_status` 返回的 `is_repo`（`git rev-parse --is-inside-work-tree`）。
用 rev-parse 而非检测 `.git` 目录：项目根位于父目录仓库内时，git 语义上也算在仓库中。

仓库已初始化但未配置 `origin` 时：仓库视图顶部显示提示条「尚未配置远程仓库」+【添加远程仓库】按钮，
分支行显示 `🌿 <branch> · <remote>`。

## 命令
命令系统就不要设计了，用户自己会用终端执行git命令的
再设计命令就属于重复设计了。

但是，AI命令那里需要加个判断：
1. LLM返回的如果是git开始的命令(llm_response[0:4] == 'git ')，则需要执行系统命令。
2. LLM返回的其他走旧逻辑