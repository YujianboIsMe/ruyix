# Git
作为一款开发工具，GIT是必须的功能
右边的大纲区做成多标签
第一个标签：大纲/Outline
第二个标签：Git

## 界面

界面上下布局：
1. 按钮区：只有三个按钮：Commit✅ Pull📥 Push📥
2. 输入区：输入commit message
3. staged:文件树组件
4. unstaged：文件树组件

## 命令
命令系统就不要设计了，用户自己会用终端执行git命令的
再设计命令就属于重复设计了。

但是，AI命令那里需要加个判断：
1. LLM返回的如果是git开始的命令(llm_response[0:4] == 'git ')，则需要执行系统命令。
2. LLM返回的其他走旧逻辑



