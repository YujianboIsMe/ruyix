"""测试文件。第一个测试没有断言，第二个是正确的写法。"""

from util.parse import parse


def test_parse_empty():
    # HX103：没有任何断言
    parse([])


def test_parse_one():
    assert parse([1]) == [1]
