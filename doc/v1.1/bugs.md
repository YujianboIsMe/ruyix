# ruyix v1.1 问题与方案（ISSUE）

> 跨版本的约定留在 `doc/` 根，**版本专指**的 bug 记在本文件所属版本下（`doc/v1.1/`）。
> 每条格式：**症状（可复现）→ 根因 → 证据 → 改法 → 门禁**。修完把根因与门禁写回同一文件。

---

## ISSUE-1：进程表的"测试隔离锁"锁的不是进程表 —— 两条 agent 用例并行跑会间歇性红

**状态**：**未修**（根因已定，改法已写，留给下一轮；登记即为此）

### 症状（可复现）

单独跑必绿，**并行跑全量**间歇性红，红的两条都属于同一族（都断言"进程必须活着"）：

```bash
cargo test -p harness-engine --lib a_missed_criterion_carries_the_port_comparison -- --test-threads=1
# → ok. 1 passed; 379 filtered out; finished in 5.95s

cargo test --workspace          # 第 1 遍 exit=101；第 2 遍红的是另一条同类：
#   test agent::tests::a_service_reported_as_running_must_survive_the_run ... FAILED
#   test result: FAILED. 371 passed; 1 failed
```

失败信息（`agent/tests.rs:2174` 的 `expect`）：

```text
判据没命中不等于启动失败: "handle=p1 已从进程表移除"
```

### 根因（两条叠在一起）

1. **隔离锁与表锁不是同一把**：
   - `proc.rs:171` `fn lock()` → 锁的是 `table()`（`Mutex<HashMap<u32, Managed>>`），所有真实读写都走它；
   - `proc.rs:1156` `pub(crate) fn table_lock()` → 锁的是一个**独立的** `static G: Mutex<()>`，与进程表毫无关系。
   于是"测试隔离"的语义被误解成"我拿锁了就不会被打扰"，但**没拿这把锁的测试照样能改表** ——
   锁只排除**同样拿它的人**。
2. **handle 是全局唯一名字、查表是全局线性扫描**：
   `proc.rs:180` `by_handle(&t, handle) = t.values().find(|m| m.handle == handle)`，
   而 handle 形如 `p1`/`p2`（按项目内的序号起名）。两个测试并行时各自都有 `p1`，
   查到的是"第一个名字叫 p1 的进程" —— 可能是**别人的**那条，也可能已被清掉 ⇒
   `info_of()` 只能回 `handle=p1 已从进程表移除`（`proc.rs:774`）。
   **这不只是测试问题**：生产里一次只跑一个 run 所以撞不到，但按 handle 查/读日志/停的接口
   **跨项目语义是模糊的**（项目 A 的模型理论上能读到项目 B 的 handle 同名进程）。

### 证据

- `proc.rs:171-173`（`lock` 锁 `table()`）与 `proc.rs:1156-1161`（`table_lock` 锁独立 `G`）并列存在；
- `proc.rs:180-182` `by_handle` 全局扫描 + `proc.rs:770-775` `info_of` 的错误文案；
- 上面两条命令的实测输出（一次串行绿、两次并行红，红的是**不同**的两条同类用例）。

### 改法（下一轮）

1. **查表按项目限定**：`info_of` / `log_tail` / `stop_handle` 三个入口接受 `proj`，
   `by_handle` 改成 `by_handle_in(&t, proj, handle)` —— 这一条同时修掉产品侧的跨项目歧义。
2. **测试隔离改成同一把锁**：去掉 `G`，`table_lock()` 返回**表锁的守卫**会与测试调用的
   引擎 API 自锁造成 std `Mutex` 不可重入的死锁 ⇒ 正确做法是给测试一个
   **全局测试锁**并让**所有碰表的用例**都拿它（现在只有一部分拿），
   或在测试里改用 `clear_table()` + 独立项目根，断言只看自己那条 pid。
3. 顺手：`refresh_all` 的 `is_dead` 判定加注释说明它**只标记不删除**（`t.remove` 只出现在容量腾位与显式停止）。

### 门禁（修完要满足）

- `cargo test --workspace` **连跑 3 遍全绿**（这是本次唯一无法用单条命令判定的地方 ——
  门禁就得是"重复跑"这种形状，因为缺陷本身是时序的）；
- 新增一条判据：**按 handle 查进程必须带项目作用域**（`by_handle_in` 存在且三个入口都用它）；
- `ui-smoke` 无需改（进程表是引擎侧）。
