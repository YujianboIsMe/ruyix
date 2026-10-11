"""工具函数。这里的毛病属于卫生类，并演示三种豁免写法。"""


def parse(items):
    print("parse", items)  # noqa: HX104
    return items


def normalize(text):
    # TODO: 支持 Unicode 归一化
    return text.strip()


def truncate(text, limit):
    # noqa: HX201 reason=切片不会抛异常，这里只需要防御性兜底，已确认可忽略
    try:
        return text[:limit]
    except:
        return ""


def check(value):
    result = value == None  # noqa: HX999 reason=规则 ID 故意写错，用于验证 HX903
    return result
