# 未完成占位

> 对应规则：`HX105`

## 两类占位

### 1. 注释标记：TODO / FIXME / XXX / HACK

```python
# TODO: 支持分页          ← 规则会报
# FIXME: 这里在并发下会出错  ← 规则会报
```

为什么报：生成产物如果是交付物，这些标记会一路带到生产；它们既不会被 CI 拦住，
也不会有人主动回来看。

处理方式二选一：

- **现在做完**，删掉标记（首选）
- **变成可追踪的待办**：写进任务清单/issue，而不是留在注释里

确实需要保留（例如上游问题已记录在外面）时：

```python
# harnesslint: disable-file HX105 reason=上游 API v3 分页未上线，见 ISSUE-123
```

### 2. 空桩函数：函数体只有 `...` 或 `pass`

```python
def send_notification(user, msg):
    ...        # ← 规则会报
```

为什么报：空桩**静默返回 None**。调用方拿到 `None` 继续往下跑，故障会在很远的地方
才浮现，而且看起来和这个函数毫无关系,排查成本极高。

推荐写法 —— 让失败尽早发生在调用点：

```python
def send_notification(user, msg):
    raise NotImplementedError("通知渠道尚未接入，见 ISSUE-123")
```

## 例外

- 抽象基类里的方法：加 `@abstractmethod` 装饰器后不报（这是契约声明，不是未完成）
- 协议/接口定义（`typing.Protocol` 的 stub）：同理，考虑加 `@abstractmethod` 或
  文件级豁免并写明理由

## 与 `raise NotImplementedError` 的关系

本规则**不报** `raise NotImplementedError(...)`：那是显式失败，符合"尽早暴露"的原则；
而 `pass` / `...` 是静默通过。两者的区别正是这条规则要强调的。
