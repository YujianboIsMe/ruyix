#!/usr/bin/env python3
"""ruyix / Darkhorse Code 应用图标生成器。

为什么要脚本而不是丢一个 SVG 进仓库：Tauri 需要一整套 PNG/ICO，
手画和矢量源之间**必然漂移**（改一次 SVG 就要重新导出十几个尺寸 =
迟早有人只改了其中一个）。这里把几何定义写成唯一真相，
同时输出 SVG（可编辑的矢量源）与 PNG（图标资产），二者同源。

几何：1024×1024 画布，三个字母在同一"字带"上等宽排布，
等线宽描边（monoline），圆头圆角。字母为几何化简笔字形，
R 的碗是外圆角矩形半圆、X 的两条斜线与 Y 的两臂同角度（31°），
让三个字在节奏上是一套东西，而不是三个字硬凑。

用法：
    python tools/logo/build_logo.py            # 生成全部资产
    python tools/logo/build_logo.py --check    # 只校验产物是否与几何一致

依赖：Pillow（仅用现成环境，不安装任何东西）。
"""

from __future__ import annotations

import argparse
import io
import math
import sys
from pathlib import Path

try:
    from PIL import Image, ImageDraw
except ImportError:  # pragma: no cover - 环境缺失时给出可执行的提示
    print("需要 Pillow：python -m pip install pillow", file=sys.stderr)
    raise SystemExit(2)

ROOT = Path(__file__).resolve().parents[2]

# ---------------------------------------------------------------- 几何定义
CANVAS = 1024.0

STROKE = 92.0                    # 描边宽度（占画布 9%，32px 下约 2.9px）
HALF = STROKE / 2.0
TOP = 322.0                      # 描边中心线的上端
BOT = 702.0                      # 描边中心线的下端（字高 380，外框 472）

# 字带：字母从左到右排布。**字宽逐字设定**（等宽会逼死 R —— 它的字腔要有
# 横长比例，等宽 + 92 描边下字腔只剩 80×88，看着像圆球挖了个方孔）。
# R:292 / Y:236 / X:264，字距 46，整条字带 884 宽，两侧留 70。
LAYOUT = [("R", 62.0, 300.0), ("Y", 408.0, 240.0), ("X", 694.0, 268.0)]
MARK_X0 = LAYOUT[0][1]                          # 字带左右边界（渐变端点）
MARK_X1 = LAYOUT[-1][1] + LAYOUT[-1][2]

BOWL_H = 190.0                   # R 碗高（占字高 50%）
BOWL_R = 55.0                    # R 碗外圆角
LEG_START = 0.55                 # R 斜腿起笔点（0=竖笔，1=碗外沿）
LEG_SPLAY = 24.0                 # 斜腿末端相对碗外沿的外扩量（略微出格，撑住右下角）
Y_CROTCH = 490.0                 # Y 两臂交汇点

TILE_RADIUS = 232.0              # squircle 圆角（22.7%，Windows/macOS 通用比例）

# 配色：与 IDE 的 --accent #007acc 同族，再加一档靛蓝收尾
TILE_DARK = (0.0, "#151D2A")
TILE_DARK2 = (1.0, "#090D14")
TILE_LIGHT = (0.0, "#FAFCFF")
TILE_LIGHT2 = (1.0, "#E2E9F4")
MARK_STOPS = [(0.0, "#8AECFF"), (0.55, "#3B8BFF"), (1.0, "#7A6BFF")]
MARK_STOPS_LIGHT = [(0.0, "#0E8FE0"), (0.55, "#1565D8"), (1.0, "#4B4FD8")]
MARK_INVERT = [(0.0, "#0B1120"), (1.0, "#162031")]   # 反色候选的深色字
BORDER_DARK = (255, 255, 255, 16)
BORDER_LIGHT = (15, 33, 58, 22)


def hex_rgb(h: str) -> tuple[int, int, int]:
    h = h.lstrip("#")
    return int(h[0:2], 16), int(h[2:4], 16), int(h[4:6], 16)


def stops_rgb(stops):
    return [(p, hex_rgb(c)) for p, c in stops]


def sample(stops, t: float):
    """在渐变 stops 上按 t∈[0,1] 取色。"""
    if t <= stops[0][0]:
        return stops[0][1]
    if t >= stops[-1][0]:
        return stops[-1][1]
    for i in range(len(stops) - 1):
        p0, c0 = stops[i]
        p1, c1 = stops[i + 1]
        if p0 <= t <= p1:
            k = 0.0 if p1 == p0 else (t - p0) / (p1 - p0)
            return tuple(round(c0[j] + (c1[j] - c0[j]) * k) for j in range(3))
    return stops[-1][1]


# ---------------------------------------------------------------- 路径模型
# 每条笔画 = 一串子路径；每个子路径 = 命令列表。
# 命令：["M"/"L", x, y] 或 ["A", cx, cy, r, a0, a1]（圆角弧，角度为度）

def _arc_pts(cx, cy, r, a0, a1, steps=48):
    return [
        (cx + r * math.cos(math.radians(a)),
         cy + r * math.sin(math.radians(a)))
        for a in [a0 + (a1 - a0) * i / steps for i in range(steps + 1)]
    ]


def flatten(sub):
    """把一条子路径压成点列（给光栅渲染用）。"""
    pts = []
    for cmd in sub:
        if cmd[0] in ("M", "L"):
            pts.append((cmd[1], cmd[2]))
        elif cmd[0] == "A":
            _, cx, cy, r, a0, a1 = cmd
            seg = _arc_pts(cx, cy, r, a0, a1)
            if pts and math.dist(pts[-1], seg[0]) > 1.0:
                seg = seg[::-1]          # 保证与上一个点接得上
            pts.extend(seg)
    return pts


def to_svg_path(subs) -> str:
    out = []
    for sub in subs:
        for cmd in sub:
            if cmd[0] in ("M", "L"):
                out.append(f"{cmd[0]}{_n(cmd[1])} {_n(cmd[2])}")
            elif cmd[0] == "A":
                _, cx, cy, r, a0, a1 = cmd
                sx, sy = _arc_pts(cx, cy, r, a0, a1, 1)[0]
                ex, ey = _arc_pts(cx, cy, r, a0, a1, 1)[1]
                large = 1 if abs(a1 - a0) > 180 else 0
                sweep = 1 if a1 > a0 else 0
                out.append(
                    f"A{_n(r)} {_n(r)} 0 {large} {sweep} {_n(ex)} {_n(ey)}"
                )
                _ = (sx, sy)
    return " ".join(out)


def _n(v: float) -> str:
    """去掉多余小数位，让 SVG 可读。"""
    return f"{v:.1f}".rstrip("0").rstrip(".")


# ---------------------------------------------------------------- 三个字母
def letters():
    """返回 [R, Y, X]，每个是子路径列表（坐标已落在 1024 画布上）。"""
    (_, rx, rw), (_, yx, yw), (_, xx, xw) = LAYOUT

    # R：左竖 + 上横 + D 形碗（右上圆角→右侧竖→右下圆角）+ 碗底横 + 斜腿
    l = rx + HALF                                  # 竖笔中心线
    r = rx + rw - HALF                             # 碗的外沿
    bb = TOP + BOWL_H                              # 碗底
    b = BOWL_R
    R = [
        [["M", l, BOT], ["L", l, TOP], ["L", r - b, TOP],
         ["A", r - b, TOP + b, b, -90, 0],           # 右上圆角
         ["L", r, bb - b],
         ["A", r - b, bb - b, b, 0, 90],             # 右下圆角
         ["L", l, bb]],                              # 碗底横：回到竖笔，字腔封闭
        # 斜腿：**起笔必须正好是碗的右下圆角端点**。起笔若偏左，碗底横就会
        # 从腿的右侧多伸出一截，形成一个尖角楔子，整个字读着像 "ᛒ"。
        [["M", r - b, bb], ["L", r + LEG_SPLAY, BOT]],
    ]

    # Y：两臂（V）+ 竖
    yc = yx + yw / 2
    Y = [
        [["M", yx + HALF, TOP], ["L", yc, Y_CROTCH], ["L", yx + yw - HALF, TOP]],
        [["M", yc, Y_CROTCH], ["L", yc, BOT]],
    ]

    # X：两条斜线，倾角与 Y 的两臂一致
    X = [
        [["M", xx + HALF, TOP], ["L", xx + xw - HALF, BOT]],
        [["M", xx + xw - HALF, TOP], ["L", xx + HALF, BOT]],
    ]
    return [R, Y, X]


# ---------------------------------------------------------------- 光栅渲染
def make_gradient(size, p0, p1, stops, space=CANVAS):
    """按 SVG 的 userSpaceOnUse 语义生成线性渐变（低分辨率后再放大）。"""
    img = Image.new("RGB", (size, size))
    px = img.load()
    (x1, y1), (x2, y2) = p0, p1
    dx, dy = x2 - x1, y2 - y1
    den = dx * dx + dy * dy
    scale = space / size
    for j in range(size):
        y = (j + 0.5) * scale
        for i in range(size):
            x = (i + 0.5) * scale
            t = ((x - x1) * dx + (y - y1) * dy) / den
            t = 0.0 if t < 0 else (1.0 if t > 1 else t)
            px[i, j] = sample(stops, t)
    return img


def stroke_mask(subs_list, size, scale, width):
    """把笔画画成遮罩（圆头圆角）。scale = 光栅尺寸 / 1024 设计尺寸。"""
    m = Image.new("L", (size, size), 0)
    d = ImageDraw.Draw(m)
    w = max(1, int(round(width * scale)))
    rad = w / 2.0
    for subs in subs_list:
        for sub in subs:
            pts = [(x * scale, y * scale) for x, y in flatten(sub)]
            # 不用 joint="curve"：Pillow 在弧线拐点会甩出扇形毛刺。
            # 每个顶点补一个圆 == round join + round cap，且无副作用。
            d.line(pts, fill=255, width=w)
            for x, y in pts:
                d.ellipse([x - rad, y - rad, x + rad, y + rad], fill=255)
    return m


def rounded_mask(size, radius_px):
    m = Image.new("L", (size, size), 0)
    ImageDraw.Draw(m).rounded_rectangle(
        [0, 0, size - 1, size - 1], radius=radius_px, fill=255
    )
    return m


def render_icon(size, dark=True, ss=4, mark_only=False, invert=False):
    """渲染一枚图标（RGBA）。ss 为超采样倍数。

    invert=True 出的是反色候选：亮渐变底 + 深色字（任务栏上更跳）。
    """
    s = size * ss
    scale = s / CANVAS                     # 设计坐标 → 光栅像素
    if mark_only:
        img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    else:
        if invert:
            tile_stops = stops_rgb(MARK_STOPS)
        else:
            tile_stops = stops_rgb([TILE_DARK, TILE_DARK2] if dark else [TILE_LIGHT, TILE_LIGHT2])
        tile = make_gradient(256, (0, 0), (CANVAS, CANVAS), tile_stops).resize(
            (s, s), Image.Resampling.BICUBIC
        )
        img = Image.composite(tile.convert("RGBA"),
                              Image.new("RGBA", (s, s), (0, 0, 0, 0)),
                              rounded_mask(s, TILE_RADIUS * scale))

    m_stops = stops_rgb(MARK_INVERT) if invert else \
        stops_rgb(MARK_STOPS if dark else MARK_STOPS_LIGHT)
    mark_grad = make_gradient(256, (MARK_X0, TOP), (MARK_X1, BOT),
                              m_stops).resize((s, s), Image.Resampling.BICUBIC)
    mask = stroke_mask(letters(), s, scale, STROKE)
    img = Image.composite(mark_grad.convert("RGBA"), img, mask)

    if not mark_only:                              # 内描边：让图标有厚度
        border = Image.new("RGBA", (s, s), (0, 0, 0, 0))
        ImageDraw.Draw(border).rounded_rectangle(
            [1.5 * scale, 1.5 * scale, s - 1.5 * scale, s - 1.5 * scale],
            radius=(TILE_RADIUS - 1.5) * scale,
            outline=(11, 17, 32, 40) if invert else
            (BORDER_DARK if dark else BORDER_LIGHT),
            width=max(1, int(round(1.5 * scale))),
        )
        img = Image.alpha_composite(img, border)

    return img.resize((size, size), Image.Resampling.LANCZOS)


# ---------------------------------------------------------------- SVG 输出
def svg_icon(dark=True) -> str:
    tile = [TILE_DARK, TILE_DARK2] if dark else [TILE_LIGHT, TILE_LIGHT2]
    mk = MARK_STOPS if dark else MARK_STOPS_LIGHT
    border = "rgba(255,255,255,.062)" if dark else "rgba(15,33,58,.086)"
    paths = "\n".join(
        f'    <path d="{to_svg_path(subs)}"/>' for subs in letters()
    )
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="{CANVAS:.0f}" height="{CANVAS:.0f}" viewBox="0 0 {CANVAS:.0f} {CANVAS:.0f}" role="img" aria-label="RYX">
  <defs>
    <linearGradient id="tile" x1="0" y1="0" x2="{CANVAS:.0f}" y2="{CANVAS:.0f}" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="{tile[0][1]}"/>
      <stop offset="1" stop-color="{tile[1][1]}"/>
    </linearGradient>
    <linearGradient id="mark" x1="{_n(MARK_X0)}" y1="{_n(TOP)}" x2="{_n(MARK_X1)}" y2="{_n(BOT)}" gradientUnits="userSpaceOnUse">
{chr(10).join(f'      <stop offset="{p}" stop-color="{c}"/>' for p, c in mk)}
    </linearGradient>
  </defs>
  <rect width="{CANVAS:.0f}" height="{CANVAS:.0f}" rx="{_n(TILE_RADIUS)}" fill="url(#tile)"/>
  <g fill="none" stroke="url(#mark)" stroke-width="{_n(STROKE)}" stroke-linecap="round" stroke-linejoin="round">
{paths}
  </g>
  <rect x="1.5" y="1.5" width="{CANVAS - 3:.0f}" height="{CANVAS - 3:.0f}" rx="{_n(TILE_RADIUS - 1.5)}" fill="none" stroke="{border}" stroke-width="1.5"/>
</svg>
"""


def svg_mark(mono=False, dark=True) -> str:
    stroke = "currentColor" if mono else "url(#mark)"
    defs = ""
    if not mono:
        mk = MARK_STOPS if dark else MARK_STOPS_LIGHT
        defs = f"""  <defs>
    <linearGradient id="mark" x1="{_n(MARK_X0)}" y1="{_n(TOP)}" x2="{_n(MARK_X1)}" y2="{_n(BOT)}" gradientUnits="userSpaceOnUse">
{chr(10).join(f'      <stop offset="{p}" stop-color="{c}"/>' for p, c in mk)}
    </linearGradient>
  </defs>
"""
    paths = "\n".join(f'  <path d="{to_svg_path(subs)}"/>' for subs in letters())
    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {CANVAS:.0f} {CANVAS:.0f}" width="{CANVAS:.0f}" height="{CANVAS:.0f}" role="img" aria-label="RYX">
{defs}  <g fill="none" stroke="{stroke}" stroke-width="{_n(STROKE)}" stroke-linecap="round" stroke-linejoin="round">
{paths}
  </g>
</svg>
"""


# ---------------------------------------------------------------- 产出
def contact_sheet(out: Path, dark=True, invert=False):
    """多尺寸对照图：证明 32px 下还认得出，而不是只在 1024 下好看。"""
    sizes = [256, 128, 64, 48, 32, 16]
    pad, gap = 40, 32
    icons = [render_icon(s, dark=dark, invert=invert) for s in sizes]
    w = pad * 2 + sum(i.width for i in icons) + gap * (len(icons) - 1)
    h = pad * 2 + max(i.height for i in icons)
    bg = (24, 26, 30) if dark else (246, 247, 249)
    sheet = Image.new("RGB", (w, h), bg)
    x = pad
    for i in icons:
        sheet.paste(i, (x, pad + (max(j.height for j in icons) - i.height) // 2), i)
        x += i.width + gap
    sheet.resize((w * 2, h * 2), Image.Resampling.NEAREST).save(out)  # 2x 便于看像素边缘


def two_up(out: Path, imgs, bg=(30, 32, 36)):
    """并排拼图（候选对照用）。"""
    pad, gap = 20, 20
    w = pad * 2 + sum(i.width for i in imgs) + gap * (len(imgs) - 1)
    h = pad * 2 + max(i.height for i in imgs)
    sheet = Image.new("RGB", (w, h), bg)
    x = pad
    for i in imgs:
        sheet.paste(i, (x, pad), i)
        x += i.width + gap
    sheet.save(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true", help="只校验，不写文件")
    ap.add_argument("--preview-dir", default=str(ROOT / "target" / "logo-preview"))
    args = ap.parse_args()

    ui = ROOT / "ui"
    outs = {
        ui / "logo.svg": svg_icon(dark=True),
        ui / "logo-light.svg": svg_icon(dark=False),
        ui / "logo-mark.svg": svg_mark(dark=True),
        ui / "logo-mono.svg": svg_mark(mono=True),
    }
    changed = [str(p.relative_to(ROOT)) for p, t in outs.items()
               if not p.exists() or p.read_text(encoding="utf-8") != t]
    if args.check:
        print("OUT-OF-DATE" if changed else "IN-SYNC", changed or "")
        return 0 if not changed else 1

    for p, t in outs.items():
        p.write_text(t, encoding="utf-8", newline="\n")
    print("svg:", ", ".join(sorted(p.name for p in outs)))

    pv = Path(args.preview_dir)
    pv.mkdir(parents=True, exist_ok=True)
    render_icon(1024).save(pv / "icon-1024.png")
    render_icon(256).save(pv / "icon-256.png")
    render_icon(1024, invert=True).save(pv / "candidate-invert-1024.png")
    render_icon(512, mark_only=True, ss=4).save(pv / "mark-512.png")
    contact_sheet(pv / "sheet-dark.png", dark=True)
    contact_sheet(pv / "sheet-light.png", dark=False)
    # 文档里要用的图：doc/preview/ 是仓库既有约定（截图都放这）
    doc_pv = ROOT / "doc" / "preview"
    doc_pv.mkdir(parents=True, exist_ok=True)
    render_icon(256).save(doc_pv / "logo.png")
    two_up(doc_pv / "logo-candidates.png", [render_icon(256), render_icon(256, invert=True)])
    print("preview:", pv, "+", doc_pv)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
