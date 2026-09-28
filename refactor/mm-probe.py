#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""多模态能力探针（一次性，可重放）：模型到底能不能读图、两条路由各自要什么形状、多大会慢。

**为什么要有它**：v1.3「截图发给 Agent」这条需求的前提是"厂商支持"。能力表里的
`ModelCaps.multimodal` 只是我们自己的声明（`crates/harness-engine/src/llm.rs` 的硬编码表），
不等于接口真的收图 —— 所以先用真密钥打真接口，把事实钉下来再动手（结论见
`doc/v1.3/需求-多模态输入-v1.3.md` §3）。

用法：
    python refactor/mm-probe.py              # 小图（8x8 纯色）+ 真截图（若 TEMP 里有）+ 大图上限
    python refactor/mm-probe.py --no-big     # 跳过 7MB 大图那一步（它要 ~90s）

它读 `<repo>/target/debug/global/ai.toml`（开发版便携根的全局配置），**只打印状态与响应片段，
不打印密钥**。
"""
import base64
import io
import json
import os
import re
import struct
import sys
import urllib.error
import urllib.request
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CFG = os.path.join(ROOT, "target", "debug", "global", "ai.toml")


def png_solid(w, h, rgb):
    def chunk(t, d):
        c = t + d
        return struct.pack(">I", len(d)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)
    raw = b"".join(b"\x00" + bytes(rgb) * w for _ in range(h))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
            + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))


def png_noise(w, h, seed=3):
    """真随机（每行都不同）⇒ 压不动，用来逼近体积上限。"""
    import random
    random.seed(seed)

    def chunk(t, d):
        c = t + d
        return struct.pack(">I", len(d)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)
    raw = bytes(random.getrandbits(8) for _ in range(w * h * 3))
    raw = b"".join(b"\x00" + raw[i * w * 3:(i + 1) * w * 3] for i in range(h))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
            + chunk(b"IDAT", zlib.compress(raw, 1)) + chunk(b"IEND", b""))


def cfg():
    t = io.open(CFG, encoding="utf-8").read()
    key = re.search(r'api_key\s*=\s*"([^"]+)"', t).group(1)
    url = re.search(r'api_url\s*=\s*"([^"]+)"', t).group(1).rstrip("/")
    model = re.search(r'model\s*=\s*"([^"]+)"', t).group(1)
    return key, url, model


def post(url, body, headers, timeout=300):
    req = urllib.request.Request(url, data=json.dumps(body).encode(), headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode("utf-8", "replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode("utf-8", "replace")
    except Exception as e:  # noqa: BLE001 —— 探针：任何异常都要如实打出来
        return "ERR", repr(e)


def main():
    key, url, model = cfg()
    root = url[:-len("/anthropic")] if url.endswith("/anthropic") else url
    print("路由 %s ｜ 模型 %s" % (url, model))
    b64 = base64.b64encode(png_solid(8, 8, (20, 120, 220))).decode()

    # ① anthropic 路：content 块数组
    st, resp = post(url + "/v1/messages",
                    {"model": model, "max_tokens": 32, "messages": [{"role": "user", "content": [
                        {"type": "text", "text": "这张图什么颜色？只回答颜色名。"},
                        {"type": "image", "source": {"type": "base64", "media_type": "image/png",
                                                    "data": b64}}]}]},
                    {"x-api-key": key, "anthropic-version": "2023-06-01",
                     "content-type": "application/json"})
    print("[anthropic 路] status=%s %s" % (st, resp[:220].replace("\n", " ")))

    # ② OpenAI 兼容路：image_url + data:
    st, resp = post(root + "/chat/completions",
                    {"model": model, "max_tokens": 32, "messages": [{"role": "user", "content": [
                        {"type": "text", "text": "这张图什么颜色？只回答颜色名。"},
                        {"type": "image_url", "image_url": {"url": "data:image/png;base64," + b64}}]}]},
                    {"Authorization": "Bearer " + key, "content-type": "application/json"})
    print("[openai  路] status=%s %s" % (st, resp[:220].replace("\n", " ")))

    # ③ 真截图（如果 TEMP 里有抓过；抓法见需求文档 §3：PowerShell Graphics.CopyFromScreen）
    shot = os.path.join(os.environ.get("TEMP", "/tmp"), "ruyix_shot_probe.png")
    if os.path.exists(shot):
        data = open(shot, "rb").read()
        st, resp = post(url + "/v1/messages",
                        {"model": model, "max_tokens": 24, "messages": [{"role": "user", "content": [
                            {"type": "text", "text": "图里是什么？一句话。"},
                            {"type": "image", "source": {"type": "base64", "media_type": "image/png",
                                                        "data": base64.b64encode(data).decode()}}]}]},
                        {"x-api-key": key, "anthropic-version": "2023-06-01",
                         "content-type": "application/json"})
        print("[真截图 %.0f KB] status=%s %s" % (len(data) / 1024, st, resp[:200].replace("\n", " ")))
    else:
        print("[真截图] 跳过：%s 不存在" % shot)

    # ④ 体积上限（默认跑；--no-big 跳过）
    if "--no-big" not in sys.argv:
        import time
        big = png_noise(2000, 1200)
        t0 = time.time()
        st, resp = post(url + "/v1/messages",
                        {"model": model, "max_tokens": 24, "messages": [{"role": "user", "content": [
                            {"type": "text", "text": "图里是什么？一句话。"},
                            {"type": "image", "source": {"type": "base64", "media_type": "image/png",
                                                        "data": base64.b64encode(big).decode()}}]}]},
                        {"x-api-key": key, "anthropic-version": "2023-06-01",
                         "content-type": "application/json"})
        print("[大图 %.1f MB] status=%s **耗时 %.0f 秒** %s"
              % (len(big) / 1048576, st, time.time() - t0, resp[:160].replace("\n", " ")))


if __name__ == "__main__":
    main()
