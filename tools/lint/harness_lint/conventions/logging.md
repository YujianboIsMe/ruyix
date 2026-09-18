# 日志与输出

> 对应规则：`HX104`（调试 print 残留）

## 为什么不用 print

`print` 写的是 **stdout**，而 stdout 是程序的**产品输出通道**。两者混在一起会出两类问题：

1. **污染机器可读输出。** 一个模块被用在管道里（`tool --json | jq`）时，
   一行调试 print 就能让下游解析失败，而且报错位置在下游，排查要绕一大圈。
2. **无法分级、无法关闭。** 上线后想关掉调试输出，只能改代码重发版。

## 怎么改

```python
import logging

logger = logging.getLogger(__name__)      # 模块级，不要在函数里反复 getLogger

logger.debug("items=%s", items)           # 调试细节，默认级别下不输出
logger.info("已处理 %d 条", n)             # 业务事件
logger.warning("跳过非法条目: %s", e)      # 可恢复的问题
logger.error("写入失败: %s", e)            # 需要人介入
```

### 用 %s 惰性插值，不要 f-string

```python
✅ logger.debug("items=%s", items)
❌ logger.debug(f"items={items}")      # 消息被过滤掉时，f-string 也已经算完了
```

调试日志往往在热路径上；惰性插值让"日志没开"时不付出格式化成本。

## 允许 print 的地方

**入口函数的用户输出**是正当的，规则不报：

```python
def main() -> int:
    print(render_report())        # ✅ 这就是 CLI 的产品输出
    return 0

if __name__ == "__main__":        # ✅ main guard 内也不报
    print("starting...")
    raise SystemExit(main())
```

判定方式：print 是否位于名为 `main` / `cli` / `entrypoint` 的函数内，
或位于 `if __name__ == "__main__":` 块内。

## 关于自动修复

本规则**不宣称可自动应用**。替换建议（`print(...)` → `logger.debug(...)`）标注为
`MaybeIncorrect`：替换后仍是合法 Python，但日志级别、要不要保留 f-string 插值、
是否该输出到 stdout 都需要人判断。谎称"机器可应用"会让自动修复工具改出合法但语义错误的结果。
