"""Writes random notebooks the way Jupyter (nbformat) writes them.

Usage: random_notebooks.py OUT_DIR COUNT SEED

Each OUT_DIR/nb<i>.ipynb is ``json.dumps(nb, sort_keys=True, indent=1, separators=(",", ": "),
ensure_ascii=False) + "\\n"`` of a notebook in nbformat's on-disk form (multi-line strings split
with ``str.splitlines(True)``), filled with random text (control characters, line separators,
non-BMP characters), numbers (arbitrary doubles, 64-bit integers) and unknown fields at every
level. The Rust model must reproduce every file byte for byte.
"""

import json
import os
import random
import struct
import sys

out_dir, count, seed = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
rng = random.Random(seed)

ALPHABET = (
    [chr(c) for c in range(0x20, 0x7F)] * 6
    + [chr(c) for c in range(0x00, 0x20)]
    + ["\x7f", "\x85", "\xa0", " ", " ", "﻿", "​", "é", "ß", "中", "文", "\U0001f600", "\r\n", "\\", '"']
)
SPECIAL_FLOATS = [0.0, -0.0, 1e-05, 1e-07, 0.0001, 1e15, 1e16, 0.1, 0.30000000000000004, 5e-324, 2.2250738585072014e-308, 1.7976931348623157e308, 100.0, 123456.789]


def text(n=None):
    n = rng.randint(0, 30) if n is None else n
    return "".join(rng.choice(ALPHABET) for _ in range(n))


def lines(s):
    return s.splitlines(True)


def number():
    kind = rng.randrange(6)
    if kind == 0:
        while True:
            x = struct.unpack("<d", struct.pack("<Q", rng.getrandbits(64)))[0]
            if x == x and x not in (float("inf"), float("-inf")):
                return x
    if kind == 1:
        return rng.uniform(-1000.0, 1000.0)
    if kind == 2:
        return rng.randint(-(2**63), 2**64 - 1)
    if kind == 3:
        return round(rng.random(), rng.randint(0, 6)) * 10.0 ** rng.randint(-12, 20)
    if kind == 4:
        return rng.randint(-1000, 1000)
    return rng.choice(SPECIAL_FLOATS)


def value(depth=0):
    kind = rng.randrange(7 if depth < 3 else 4)
    if kind == 0:
        return text()
    if kind in (1, 3):
        return number()
    if kind == 2:
        return rng.choice([True, False, None])
    if kind in (4, 5):
        return obj(depth + 1)
    return [value(depth + 1) for _ in range(rng.randint(0, 4))]


def obj(depth=0):
    return {text(rng.randint(0, 6)): value(depth) for _ in range(rng.randint(0, 4))}


def extra(target):
    """Unknown keys (prefixed so they never collide with nbformat's)."""
    for _ in range(rng.randint(0, 2)):
        target["x_" + text(rng.randint(0, 5))] = value(1)
    return target


def bundle():
    data = {}
    for mime in rng.sample(["text/plain", "text/html", "text/markdown", "image/svg+xml", "application/javascript"], rng.randint(0, 3)):
        data[mime] = lines(text())
    if rng.random() < 0.5:
        data["image/png"] = text(rng.randint(0, 20))
    if rng.random() < 0.5:
        data["application/json"] = value(1)
    if rng.random() < 0.3:
        data["application/vnd.custom.v1+json"] = value(1)
    return data


def output():
    kind = rng.randrange(6)
    if kind == 0:
        out = {"output_type": "stream", "name": rng.choice(["stdout", "stderr"]), "text": lines(text())}
    elif kind == 1:
        out = {"output_type": "display_data", "data": bundle(), "metadata": obj(1)}
    elif kind == 2:
        out = {"output_type": "execute_result", "execution_count": rng.choice([None, rng.randint(0, 2**32 - 1)]), "data": bundle(), "metadata": obj(1)}
    elif kind == 3:
        out = {"output_type": "error", "ename": text(8), "evalue": text(), "traceback": [text() for _ in range(rng.randint(0, 4))]}
    elif kind == 4:
        out = {"output_type": "x_unknown_" + text(4), "payload": value(1)}
    else:
        out = {"output_type": "stream", "name": "stdout", "text": lines(text(200))}
    return extra(out)


def cell():
    kind = rng.choice(["code", "code", "markdown", "raw"])
    c = {"cell_type": kind, "id": text(rng.randint(1, 12)), "metadata": obj(1), "source": lines(text(rng.randint(0, 120)))}
    if kind == "code":
        c["execution_count"] = rng.choice([None, rng.randint(0, 1000)])
        c["outputs"] = [output() for _ in range(rng.randint(0, 4))]
    elif rng.random() < 0.3:
        c["attachments"] = {text(5): {"image/png": text(10), "text/plain": lines(text())}}
    return extra(c)


for i in range(count):
    nb = {"cells": [cell() for _ in range(rng.randint(0, 8))], "metadata": obj(), "nbformat": 4, "nbformat_minor": rng.randint(0, 5)}
    extra(nb)
    text_out = json.dumps(nb, sort_keys=True, indent=1, separators=(",", ": "), ensure_ascii=False) + "\n"
    with open(os.path.join(out_dir, "nb%d.ipynb" % i), "w", encoding="utf-8", newline="\n") as f:
        f.write(text_out)
