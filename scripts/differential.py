#!/usr/bin/env python3
"""Differential test: quiver vs pyflakes and pycodestyle on a corpus.

Compares the sets of (file, line, code) reported by each tool for the rules
they share and prints agreement statistics per rule.

usage: differential.py QUIVER_BIN DIR [--max-line-length N]
"""
import json
import os
import re
import subprocess
import sys
from collections import defaultdict

PYFLAKES = [
    (r"import \*' used", "F403"),
    (r"imported but unused", "F401"),
    (r"redefinition of unused", "F811"),
    (r"undefined name .* in __all__", "F822"),
    (r"undefined name", "F821"),
    (r"local variable .* is assigned to but never used", "F841"),
    (r"f-string is missing placeholders", "F541"),
    (r"use ==/!= to compare constant literals", "F632"),
    (r"dictionary key .* repeated with different values", "F601"),
    (r"assertion is always true", "F631"),
    (r"'break' outside loop", "F701"),
    (r"'continue' not properly in loop", "F702"),
    (r"'return' outside function", "F706"),
    (r"'yield' outside function", "F704"),
    (r"NotImplemented", "F901"),
]
PYCODESTYLE = "E402,E501,E701,E702,E703,E711,E712,E713,E714,E721,E722,E731,E741,W191,W291,W292,W293,W605"


def files_in(d):
    out = []
    for root, dirs, names in os.walk(d):
        dirs[:] = [x for x in dirs if "-packages" not in x and not x.startswith(".")]
        out += [os.path.join(root, n) for n in names if n.endswith(".py")]
    return sorted(out)


def quiver(bin_, files, codes, ll):
    res = subprocess.run([bin_, "check", "--no-config", "--line-length", str(ll), "--select", codes, "--output-format", "json", *files], capture_output=True, text=True)
    return {(os.path.realpath(d["filename"]), d["location"]["row"], d["code"]) for d in json.loads(res.stdout)}


def pyflakes(files):
    out = set()
    res = subprocess.run([sys.executable, "-m", "pyflakes", *files], capture_output=True, text=True)
    for line in res.stdout.splitlines():
        m = re.match(r"(.*?):(\d+):(?:\d+:)? (.*)", line)
        if not m:
            continue
        for pat, code in PYFLAKES:
            if re.search(pat, m.group(3)):
                out.add((os.path.realpath(m.group(1)), int(m.group(2)), code))
                break
    return out


def pycodestyle(files, ll):
    out = set()
    res = subprocess.run([sys.executable, "-m", "pycodestyle", f"--select={PYCODESTYLE}", f"--max-line-length={ll}", *files], capture_output=True, text=True)
    for line in res.stdout.splitlines():
        m = re.match(r"(.*?):(\d+):\d+: (\w\d+) ", line)
        if m:
            out.add((os.path.realpath(m.group(1)), int(m.group(2)), m.group(3)))
    return out


def report(name, ours, theirs, codes):
    print(f"\n== quiver vs {name}")
    print(f"{'rule':<6} {'both':>7} {'quiver-only':>12} {'%s-only' % name:>16} {'agreement':>10}")
    tot_b = tot_o = tot_t = 0
    for code in codes:
        o = {x for x in ours if x[2] == code}
        t = {x for x in theirs if x[2] == code}
        b = len(o & t)
        tot_b, tot_o, tot_t = tot_b + b, tot_o + len(o - t), tot_t + len(t - o)
        denom = len(o | t)
        agree = f"{100 * b / denom:.1f}%" if denom else "n/a"
        print(f"{code:<6} {b:>7} {len(o - t):>12} {len(t - o):>16} {agree:>10}")
    denom = tot_b + tot_o + tot_t
    print(f"{'TOTAL':<6} {tot_b:>7} {tot_o:>12} {tot_t:>16} {100 * tot_b / max(denom, 1):>9.1f}%")
    return ours - theirs, theirs - ours


def main():
    bin_, d = sys.argv[1], sys.argv[2]
    ll = 79
    if "--max-line-length" in sys.argv:
        ll = int(sys.argv[sys.argv.index("--max-line-length") + 1])
    files = files_in(d)
    print(f"corpus: {len(files)} files under {d}")
    flake_codes = sorted({c for _, c in PYFLAKES})
    ours = quiver(bin_, files, ",".join(flake_codes), ll)
    theirs = pyflakes(files)
    qo, to = report("pyflakes", ours, theirs, flake_codes)
    for label, s in (("quiver-only", qo), ("pyflakes-only", to)):
        for x in sorted(s)[:8]:
            print(f"  {label}: {x[0]}:{x[1]} {x[2]}")
    style_codes = PYCODESTYLE.split(",")
    ours = quiver(bin_, files, PYCODESTYLE, ll)
    theirs = pycodestyle(files, ll)
    qo, to = report("pycodestyle", ours, theirs, style_codes)
    for label, s in (("quiver-only", qo), ("pycodestyle-only", to)):
        for x in sorted(s)[:8]:
            print(f"  {label}: {x[0]}:{x[1]} {x[2]}")


if __name__ == "__main__":
    main()
