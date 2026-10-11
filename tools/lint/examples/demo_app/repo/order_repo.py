"""数据访问层。这里做了两件坏事：反向依赖 + 循环依赖。"""

from service.order_service import reconcile

ORDER_TABLE = "orders"


class OrderRepo:
    def all(self):
        return [{"id": 1, "amount": 10}]

    def reconcile_all(self):
        # HX301：repo 层反向依赖 service 层
        return reconcile(self.all(), [])
