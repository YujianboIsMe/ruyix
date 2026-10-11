"""业务层。这里塞了一批典型毛病，用来验证规则能报出来。"""

import logging

from repo.order_repo import OrderRepo

logger = logging.getLogger(__name__)

# HX202：硬编码密钥
API_KEY = "sk-f5f78989471f49af8855cc019c63e230"


def total_orders(items):
    # HX201：裸 except + 静默吞掉
    try:
        return sum(item.amount for item in items)
    except:
        pass


def find_user(name):
    # HX204：== None（本规则唯一 MachineApplicable 的例子）
    if name == None:
        return None
    return name.strip()


def add_note(note, notes=[]):
    # HX203：可变默认参数
    notes.append(note)
    return notes


def send_mail(to):
    # HX105：空桩实现，会静默返回 None
    ...


def reconcile(orders, payments):
    # HX102：函数过长（阈值被样例配置调成 15 行）
    logger.info("开始对账")
    matched = []
    unmatched = []
    for order in orders:
        found = False
        for pay in payments:
            if pay.order_id == order.id:
                matched.append(order.id)
                found = True
                break
        if not found:
            unmatched.append(order.id)
            logger.warning("订单 %s 没有对应支付", order.id)
    logger.info("对账完成，未匹配 %d 条", len(unmatched))
    return {"matched": matched, "unmatched": unmatched}


def list_orders():
    # HX104：没有豁免的调试 print，用来验证规则确实会报
    print("listing orders")
    return OrderRepo().all()
