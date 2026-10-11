"""诊断的数据模型。

**字段语义直接对齐 rustc 的 JSON 诊断**（doc.rust-lang.org/rustc/json.html）：
`level` / `code` / `spans[].label` / `suggested_replacement` / `suggestion_applicability` /
`children` / `rendered`。抄这套形状的理由：① 语义已被大规模验证 ② "能不能自动改"这件事
rustc 已经用四级 applicability 划清了 ③ 未来接 LSP / CI 注解能直接映射。

除了 rustc 那套字段，我们多两个顶层字段（`rule` / `fix_blocked_reason`）：
前者是稳定的规则 ID（跟 `code.code` 冗余，但读取方便），后者回答"为什么这条不给自动修复"——
**不给自动修复必须给理由**，否则等于承认"我也不知道怎么改"。
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Iterable, List, Optional

# 严重级：与 rustc 一致
Level = str  # "error" | "warning" | "note" | "help"
LEVEL_ORDER = {"error": 0, "warning": 1, "note": 2, "help": 3}

# 自动修复的可用性：语义照抄 rustc
#   MachineApplicable  肯定是作者想要的，应当自动应用
#   MaybeIncorrect     可能是想要的但不一定；**应用后仍是合法代码**
#   HasPlaceholders    含 (...) 占位符，不能自动应用（应用后不是合法代码）
#   Unspecified        未知 / 本条不给建议
Applicability = str
APPLICABILITIES = (
    "MachineApplicable",
    "MaybeIncorrect",
    "HasPlaceholders",
    "Unspecified",
)
# 只有这两个级别允许被自动应用（本版还没开 --fix，但先把它写进契约）
AUTO_APPLICABLE = ("MachineApplicable",)


@dataclass
class Span:
    """一处位置。列号 1-based，`column_end` 与 rustc 一致按"排他"处理。"""

    file: str
    line_start: int
    column_start: int = 1
    line_end: Optional[int] = None
    column_end: Optional[int] = None
    label: str = ""
    text: List[str] = field(default_factory=list)  # 涉及的源码行（渲染用，不含行号）
    # text[0] 对应的真实行号。上下文被开头 clamp 时它不等于 line_start-1，
    # 所以必须显式记下来——踩过一次：line_start==1 时渲染出重复行号。
    text_start: int = 1
    suggested_replacement: Optional[str] = None
    suggestion_applicability: Applicability = "Unspecified"
    is_primary: bool = True

    def __post_init__(self) -> None:
        if self.line_end is None:
            self.line_end = self.line_start
        if self.column_end is None:
            # 默认高亮到行尾
            if self.text:
                self.column_end = len(self.text[0]) + 1
            else:
                self.column_end = self.column_start + 1

    def to_json(self) -> dict:
        # rustc 的 text[] 用 highlight_start/end 标出要画 ^ 的范围
        out_text = []
        for i, line in enumerate(self.text):
            hl_s, hl_e = 1, 1
            if self.text_start + i == self.line_start:  # 只有主行才画高亮
                hl_s = self.column_start
                hl_e = self.column_end if self.line_end == self.line_start else len(line) + 1
            out_text.append({"text": line, "highlight_start": hl_s, "highlight_end": hl_e})
        return {
            "file_name": self.file,
            "line_start": self.line_start,
            "line_end": self.line_end,
            "column_start": self.column_start,
            "column_end": self.column_end,
            "is_primary": self.is_primary,
            "label": self.label,
            "text": out_text,
            "text_start": self.text_start,
            "suggested_replacement": self.suggested_replacement,
            "suggestion_applicability": self.suggestion_applicability,
        }


@dataclass
class Child:
    """附加信息。rustc 里 children 挂的就是 help / note。"""

    level: Level
    message: str

    def to_json(self) -> dict:
        return {"level": self.level, "message": self.message}


@dataclass
class Diagnostic:
    """一条诊断。`message` 只说"违反了什么"，why/how/参考 放 children。"""

    rule: str
    level: Level
    message: str
    span: Optional[Span] = None
    children: List[Child] = field(default_factory=list)
    doc: Optional[str] = None
    fixable: bool = False
    fix_blocked_reason: Optional[str] = None

    # ---- 构造期的小工具，让规则写法短一些 ----
    def with_why(self, why: str) -> "Diagnostic":
        self.children.append(Child("note", why))
        return self

    def with_fix(self, how: str, doc: Optional[str] = None) -> "Diagnostic":
        self.children.append(Child("help", how))
        if doc:
            self.doc = doc
        return self

    def with_child(self, level: Level, message: str) -> "Diagnostic":
        self.children.append(Child(level, message))
        return self

    @property
    def line(self) -> int:
        return self.span.line_start if self.span else 0

    @property
    def file(self) -> str:
        return self.span.file if self.span else ""

    def to_json(self) -> dict:
        return {
            # --- rustc 兼容部分 ---
            "message": self.message,
            "code": {"code": self.rule, "explanation": self.doc},
            "level": self.level,
            "spans": [self.span.to_json()] if self.span else [],
            "children": [c.to_json() for c in self.children],
            # --- 扩展部分 ---
            "rule": self.rule,
            "doc": self.doc,
            "fixable": self.fixable,
            "fix_blocked_reason": self.fix_blocked_reason,
        }


def diagnostics_to_json(diags: Iterable[Diagnostic]) -> List[dict]:
    return [d.to_json() for d in diags]
