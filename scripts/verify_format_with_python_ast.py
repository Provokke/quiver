#!/usr/bin/env python3
"""Cross-check the formatter against CPython's own `ast` module.

Copies a sample of .py files into a temp dir, runs `quiver format` on it, and
checks that `ast.dump(ast.parse(original)) == ast.dump(ast.parse(formatted))`.

usage: verify_format_with_python_ast.py QUIVER_BIN SRC_DIR [MAX_FILES]
"""
import ast
import os
import random
import shutil
import subprocess
import sys
import tempfile
import warnings

warnings.filterwarnings("ignore")


def main():
    quiver, src_dir = sys.argv[1], sys.argv[2]
    limit = int(sys.argv[3]) if len(sys.argv) > 3 else 1000
    files = []
    for root, _, names in os.walk(src_dir):
        for n in names:
            if n.endswith(".py"):
                files.append(os.path.join(root, n))
    files.sort()
    random.Random(42).shuffle(files)
    sample = []
    for f in files:
        try:
            with open(f, "rb") as fh:
                data = fh.read()
            data.decode("utf-8")
            ast.parse(data)
        except Exception:
            continue
        sample.append(f)
        if len(sample) >= limit:
            break
    tmp = tempfile.mkdtemp(prefix="quiver-ast-")
    mapping = {}
    for i, f in enumerate(sample):
        dst = os.path.join(tmp, f"f{i}.py")
        shutil.copyfile(f, dst)
        mapping[dst] = f
    subprocess.run([quiver, "format", "--no-config", tmp], check=False, stdout=subprocess.DEVNULL)
    ok = bad = 0
    for dst, orig in mapping.items():
        with open(orig, "rb") as fh:
            a = ast.dump(ast.parse(fh.read()))
        try:
            with open(dst, "rb") as fh:
                b = ast.dump(ast.parse(fh.read()))
        except SyntaxError as e:
            print(f"FAIL (syntax) {orig}: {e}")
            bad += 1
            continue
        if a == b:
            ok += 1
        else:
            bad += 1
            print(f"FAIL (ast differs) {orig}")
    shutil.rmtree(tmp)
    print(f"python-ast check: {ok}/{ok + bad} formatted files have identical CPython ASTs")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
