# 分层与依赖方向

> 对应规则：`HX301`（分层依赖方向）、`HX302`（循环依赖）

## 允许的依赖方向

```
types  →  config  →  repo  →  service  →  runtime  →  ui
(最底层，谁也不依赖)                              (最上层)
```

**依赖方向必须向下**：高层可以依赖低层，低层不得依赖高层。写法上就是从右往左 import。

| 从 | 到 | 判定 |
|---|---|---|
| `ui/cli.py` | `service/order_service.py` | ✅ 向上依赖？不，service 在 ui 左边（更低层）→ ✅ 允许 |
| `service/order_service.py` | `repo/order_repo.py` | ✅ 允许（相邻向下） |
| `repo/order_repo.py` | `service/order_service.py` | ❌ **反向依赖**（HX301 报错） |
| `ui/cli.py` | `repo/order_repo.py` | ⚠️ 默认允许；开启 `strict = true` 后算**越层**（HX301 报错） |

各层的职责：

| 层 | 职责 | 不该做的事 |
|---|---|---|
| `types` | 数据结构、枚举、协议、常量 | 任何 IO、任何 import 其它层 |
| `config` | 配置读取与校验 | 业务逻辑 |
| `repo` | 数据存取（DB / HTTP / 文件） | 业务规则判断 |
| `service` | 业务逻辑编排 | 直接拼 SQL / 直接发 HTTP |
| `runtime` | 运行时装配、路由、中间件、并发调度 | 业务规则 |
| `ui` | 命令行 / 界面 / 模板 | 直接访问存储 |

## 为什么反向依赖是硬伤

反向依赖会让**改动成本沿着箭头反向传导**：

```
repo/order_repo.py  import  service/order_service.py
```

于是"换一个存储实现"这种纯粹的底层改动，被迫去改上层代码；
`repo` 也不再能被单独测试（导入它就会拉起整个 service 层）。

## 怎么修（反向依赖）

```
① 在中间层暴露方法：service 层本来就应该有 list_orders()
② 把 import 改成依赖中间层
③ 如果中间层确实缺这个能力 —— 先加中间层的方法，再改调用方
   （不要用"先在 repo 里 import 一下顶住"来换一时方便）
```

## HX302 循环依赖

```
a.py  import  b.py
b.py  import  a.py        ← 环
```

后果：模块无法被单独导入/单独测试；导入顺序变化会造出**部分初始化的模块**
（`ImportError` 或属性为 `None`），是典型的偶发故障来源。

三种修法：

1. **抽公共部分到 `types` 层**（首选）：双方都需要的结构定义上移，两边都依赖它
2. **依赖抽象而不是具体**：把函数/对象作为参数传进来，而不是 import
3. **延迟导入**（`def f(): from .other import X`）：能解导入期问题，
   **但不改变依赖方向**，属于治标——只在环确实无法拆时用，并写明原因

## 配置

```toml
[architecture]
layers = ["types", "config", "repo", "service", "runtime", "ui"]
strict = false           # true = 只允许依赖紧邻下层（越层也算违规）

# 层名 → 目录名（可加同义词，适配你自己的目录结构）
[architecture.layer_dirs]
types   = ["types", "models", "schemas", "domain"]
config  = ["config", "settings"]
repo    = ["repo", "repositories", "dao", "persistence"]
service = ["service", "services", "usecases", "core"]
runtime = ["runtime", "infra", "handlers", "api"]
ui      = ["ui", "cli", "web", "views"]
```

**判不出层就不报**：目录名匹配不上任何一层（`misc/`、`examples/`、`scripts/`）时规则跳过。
这是刻意的——架构规则误报的代价是用户对整个 linter 失去信任。
