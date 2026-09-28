"""任务 fix-mul-and-test 的判据（对 agent 只读；本轮内锁定 sha256）。

两条：① **独立的**行为断言（不信 agent 自己写的测试）；② agent 的测试**真的跑起来了**
（≥3 个用例）—— 只看 ① 会放过"没写测试"，只看 ② 会放过"写了三个空测试"。
"""
import sys
import unittest

sys.path.insert(0, ".")

try:
    from calc import add, mul
except Exception as e:  # noqa: BLE001 - 判据要把"导不进来"如实报出来
    print(f"FAIL 导不进 calc：{e!r}")
    raise SystemExit(1)

bad = []
if add(2, 3) != 5:
    bad.append(f"add(2, 3) == {add(2, 3)!r}，应为 5")
if add(-1, 1) != 0:
    bad.append(f"add(-1, 1) == {add(-1, 1)!r}，应为 0")
if mul(3, 4) != 12:
    bad.append(f"mul(3, 4) == {mul(3, 4)!r}，应为 12")
if bad:
    print("FAIL 行为断言：" + "；".join(bad))
    raise SystemExit(1)

try:
    import test_calc
except Exception as e:  # noqa: BLE001
    print(f"FAIL 导不进 test_calc.py：{e!r}（这个任务要求补一个 unittest 文件）")
    raise SystemExit(1)

res = unittest.TextTestRunner(verbosity=1).run(
    unittest.defaultTestLoader.loadTestsFromModule(test_calc)
)
if res.testsRun < 3 or not res.wasSuccessful():
    print(f"FAIL agent 的测试：跑了 {res.testsRun} 个用例，成功={res.wasSuccessful()}（要求 ≥3 且全过）")
    raise SystemExit(1)

print(f"OK 行为断言通过，且 agent 的测试跑了 {res.testsRun} 个用例全过")
