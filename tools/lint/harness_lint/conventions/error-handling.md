# 异常处理

> 对应规则：`HX201`（裸 except / except 后吞掉）

## 裸 `except:` 的两个具体后果

```python
try:
    do_work()
except:            # ← 规则报错
    pass
```

1. **吞掉控制流异常。** `KeyboardInterrupt`（Ctrl+C）和 `SystemExit`（`sys.exit()`）
   都是继承自 `BaseException` 的异常。裸 `except:` 会把它们一起捕获，表现是
   "Ctrl+C 杀不掉"或"进程退出码永远是 0"——CI 里尤其难查，因为流水线永远显示成功。
2. **掩盖真实故障。** 配 `pass` 时故障被静默化：典型症状是**测试全绿但数据不对**。

## 正确的三种写法

```python
# ① 捕获具体类型 + 记日志（可恢复的问题）
try:
    row = parse(line)
except (ValueError, KeyError) as e:
    logger.warning("跳过非法条目: %s", e)
    continue

# ② 转成本层领域异常再抛（不丢原始 traceback）
try:
    data = self._repo.fetch(order_id)
except ConnectionError as e:
    raise OrderUnavailable(f"拉取订单 {order_id} 失败") from e

# ③ 明确说明为什么可以忽略（罕见，且必须写理由）
try:
    os.remove(temp_path)
except FileNotFoundError:
    pass  # 临时文件已被后续步骤清理，此处缺失是正常情况
```

③ 这种写法要加行内豁免，并且**必须带理由**：

```python
try:
    os.remove(temp_path)
except FileNotFoundError:  # noqa: HX201 reason=临时文件可能已被上游清理，此处缺失属正常
    pass
```

## `except Exception` 什么时候可以

捕获 `Exception`（不含 `BaseException`）本身是可以的，尤其是在**任务边界**上：

```python
async def run_task(task):
    try:
        await task.run()
    except Exception as e:                # ✅ 边界处兜底，防止一个任务拖垮整个 worker
        logger.exception("任务 %s 失败", task.id)
        return TaskResult.failed(e)
```

判断标准不是"捕获了多宽的异常"，而是**捕获后有没有做有意义的事**：
记日志 / 转换后重抛 / 返回明确失败态。直接 `pass` 才是问题。

## 验证顺序（顺带说明）

harness 的验证阶段是"先语法/编译，后测试"。编译不过时测试会标 `skipped` 并写明
"因为编译失败"——**不会假装跑过**。看到 skipped 就去修上面那条 error，不要忽略。
