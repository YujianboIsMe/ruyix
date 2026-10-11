# 密钥与敏感信息

> 对应规则：`HX202`

## 为什么硬编码密钥是"必须立刻改"而不是"有空再改"

密钥一旦写进源码，它的生命周期就不再受你控制：

1. **进版本库。** 即使下一版删掉，`git log -p` 里永远还在。删文件、改代码都不算清除，
   只能轮换。
2. **进日志、进镜像层、进构建缓存。** 只要有一步把源码或环境变量打了出来，就多一份副本。
3. **进别人的机器。** 依赖方、CI runner、同事的本地仓库，全都是泄漏面。

结论：**发现即视为已泄漏，处理方式是"换成环境变量读取 + 轮换密钥"，不是"从代码里删掉"。**

## 正确写法

```python
✅ import os
   API_KEY = os.environ["API_KEY"]                  # 缺失时直接 KeyError，早失败

✅ API_KEY = os.environ.get("API_KEY", "")          # 允许缺省（但要有显式处理）

✅ from myapp.config import settings
   API_KEY = settings.api_key                       # 从配置层统一读取

❌ API_KEY = "sk-f5f7...c63e230"                    # 规则报错
❌ DEFAULT_TOKEN = "ghp_AAAAAAAAAAAAAAAAAAAA"        # 规则报错
```

## 配套工程实践

- `.env.example` 里写**占位值**（`API_KEY=your-key-here`），真实 `.env` 进 `.gitignore`
- 生产用密钥管理服务（Vault / AWS Secrets Manager / K8s Secret），不要走 `.env` 文件
- CI 里用受保护的 secret 变量，不要 echo 到日志
- 提交前加一道扫描（本规则可以直接当 pre-commit / CI 步骤）

## 关于本规则的保守性

这条规则**刻意偏保守**，因为它误报的代价特别高：一旦在正常代码里刷出"假密钥"，
用户会很快学会忽略这条告警，于是真正的泄漏也会被一起忽略。

判定需要同时满足：

1. 变量名/关键字像密钥（`password` / `secret` / `token` / `api_key` / ...），
   **或**字面量匹配已知令牌前缀（`sk-` / `ghp_` / `AKIA` / ...）
2. 长度 ≥ 16
3. 香农熵 ≥ 3.2（或命中已知前缀）
4. 不含占位符标记（`xxx` / `example` / `dummy` / `your` / `test` / `<` / `${` ...）

代价是**会漏掉**一些弱密钥（例如包含 `test` 字样的真密钥）。漏报可以靠轮换流程兜底，
误报会毁掉整条反馈链路——这个取舍是明确的。

诊断里回显的密钥是**掩码后的**（`sk-f******e230（共 37 字符）`），避免二次泄漏。
