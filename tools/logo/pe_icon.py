#!/usr/bin/env python3
"""从 Windows PE 可执行文件里读图标资源（RT_GROUP_ICON / RT_ICON）。

为什么需要它：改了 `bundle.icon` 之后，"任务栏上到底会不会变"只有**编译产物的
资源段**说了算 —— tauri.conf.json 写对了、文件也在磁盘上，仍可能因为没重新链接
而没进 exe。这条检查不看配置、不看源码，只读 exe 里真实存在的字节。

不依赖任何第三方库（PE 是定长结构，直接按偏移读）。

用法：
    python tools/logo/pe_icon.py target/debug/ruyix.exe
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path

RT_ICON = 3
RT_GROUP_ICON = 14


def _sections(buf: bytes):
    lfanew = struct.unpack_from("<I", buf, 0x3C)[0]
    if buf[lfanew:lfanew + 4] != b"PE\0\0":
        raise ValueError("不是 PE 文件")
    n_sections = struct.unpack_from("<H", buf, lfanew + 6)[0]
    opt_size = struct.unpack_from("<H", buf, lfanew + 20)[0]
    base = lfanew + 24 + opt_size
    out = {}
    for i in range(n_sections):
        off = base + i * 40
        name = buf[off:off + 8].rstrip(b"\0").decode("ascii", "replace")
        vaddr, raw_size, raw_ptr = struct.unpack_from("<III", buf, off + 12)
        out[name] = (vaddr, raw_size, raw_ptr)
    return out


def _walk(buf: bytes, rsrc_raw: int, rsrc_va: int, offset: int, level: int, path=()):
    """递归走资源目录树，产出 (path, leaf_offset_in_file, size)。"""
    n_named, n_id = struct.unpack_from("<HH", buf, rsrc_raw + offset + 12)
    for i in range(n_named + n_id):
        ent = rsrc_raw + offset + 16 + i * 8
        name, sub = struct.unpack_from("<II", buf, ent)
        if level == 0 and i < n_named:
            continue                       # 具名类型只可能是自定义资源
        next_p = path + (name if level else name & 0xFFFF,)
        if sub & 0x80000000:               # 还有下一层
            yield from _walk(buf, rsrc_raw, rsrc_va, sub & 0x7FFFFFFF, level + 1, next_p)
        else:                              # 叶子：IMAGE_RESOURCE_DATA_ENTRY
            data_rva, size = struct.unpack_from("<II", buf, rsrc_raw + sub)
            yield next_p, rsrc_raw + (data_rva - rsrc_va), size


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    dump = None
    if "--dump-largest" in sys.argv:
        dump = Path(sys.argv[sys.argv.index("--dump-largest") + 1])
    path = Path(args[0] if args else "target/debug/ruyix.exe")
    buf = path.read_bytes()
    secs = _sections(buf)
    if ".rsrc" not in secs:
        print(f"{path.name}: 没有 .rsrc 段 —— 图标资源没进库")
        return 1
    rsrc_va, _raw_size, rsrc_raw = secs[".rsrc"]

    groups, icons = [], {}
    for p, off, size in _walk(buf, rsrc_raw, rsrc_va, 0, 0):
        if p[0] == RT_GROUP_ICON:
            groups.append((p, buf[off:off + size]))
        elif p[0] == RT_ICON:
            icons[p[1]] = buf[off:off + size]

    if not groups:
        print(f"{path.name}: .rsrc 里没有 RT_GROUP_ICON —— exe 不带图标")
        return 1

    print(f"{path.name}: {len(groups)} 个图标组 / {len(icons)} 个 RT_ICON")
    worst, best_id = 0, None
    for p, data in groups:
        _res, _type, count = struct.unpack_from("<HHH", data, 0)
        sizes = []
        for i in range(count):
            w, h, _c, _r, _pl, bpp, nbytes, iid = struct.unpack_from(
                "<BBBBHHIH", data, 6 + i * 14
            )
            sizes.append(f"{w or 256}x{h or 256}@{bpp}bpp/{nbytes}B")
            if (w or 256) > worst:
                worst, best_id = (w or 256), iid
        print(f"  组 {p[1:]}: {count} 项 -> {', '.join(sizes)}")
    print(f"最大边长 = {worst}px")

    if dump and best_id in icons:
        # 把 RT_ICON 的裸数据套回一个最小 ICO 容器，交给 Pillow 解码
        # （256 那档通常是 PNG 压的，小尺寸是 BMP —— 统一走容器最省事）
        raw = icons[best_id]
        # 注意 ICO 目录项是 16 字节（末尾是 dwImageOffset），14 字节那个是
        # RT_GROUP_ICON 的项（末尾是 id）—— 两者别串用，否则 Pillow 认不出。
        hdr = struct.pack("<HHH", 0, 1, 1) + struct.pack(
            "<BBBBHHII", 0, 0, 0, 0, 1, 32, len(raw), 6 + 16
        )
        dump.parent.mkdir(parents=True, exist_ok=True)
        dump.write_bytes(hdr + raw)
        print(f"已导出最大图标 -> {dump}（{len(raw)}B）")
    return 0 if worst >= 256 else 2


if __name__ == "__main__":
    raise SystemExit(main())
