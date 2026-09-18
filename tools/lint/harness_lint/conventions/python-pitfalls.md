# Python 常见陷阱

> 对应规则：`HX203`（可变默认参数）、`HX204`（`== None`）

## HX203 可变默认参数

```python
def add_item(item, bucket=[]):        # ← 规则报错
    bucket.append(item)
    return bucket

add_item(1)     # [1]
add_item(2)     # [1, 2]  ← 你以为会是 [2]
```

**根因**：默认值只在**函数定义时求值一次**，之后被所有调用共享。也就是说
`bucket` 不是"每次调用新建的列表"，而是函数对象上的一个属性。

正确写法 —— 用 `None` 哨兵：

```python
def add_item(item, bucket=None):
    if bucket is None:
        bucket = []
    bucket.append(item)
    return bucket
```

同样中招的：`{}`、`set()`、`bytearray()`、以及任何在默认值位置调用的构造函数。

## HX204 `== None` 与 `is None`

```python
if value == None:      # ← 规则报错（可自动修复）
if value is None:      # ✅
```

**为什么 `==` 是错的**：`==` 调用的是 `type(value).__eq__`，可以被重载成任何行为：

| 场景 | `x == None` 的结果 |
|---|---|
| numpy 数组 | 元素级布尔数组 → `if` 抛 `ValueError: truth value is ambiguous` |
| pandas Series | 同上 |
| SQLAlchemy / ORM 字段代理 | 生成 `IS NULL` 的 SQL 片段，而不是 `True`/`False` |
| 任意自定义 `__eq__` | 完全不可预测 |

`is` 判断的是**同一性**（内存地址），不可被重载，永远是你想要的语义。

**这条规则是本套里唯一达到 `MachineApplicable` 级别的**：`== None` → `is None`
是语义等价、无歧义的替换，可以放心自动应用。反过来 `!= None` → `is not None`。

## 相关

- 可变默认参数在 `dataclass` 里由 `field(default_factory=...)` 解决，规则不会误报
- 多行表达式上的 `== None` 只报不给建议：无法在不理解换行的情况下安全切片替换
