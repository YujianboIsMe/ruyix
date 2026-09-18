"""命令行入口。

正常部分：ui 依赖 service（向下），main() 里的 print 是正当输出、不该被报。
"""

from service.order_service import list_orders


def main() -> int:
    print("orders:", list_orders())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
