"""Check each Lean specification against the tool it describes.

The spec is written from documentation; this run makes sure it agrees with
the reference implementation (CPython fnmatch / glob, git, tox, setuptools,
pytest) before the spec is used to judge chokkin. Run with the venv that
has tox, setuptools and pytest installed:

    VENV/bin/python -I harness/validate_spec.py [N]
"""
import fnmatch
import glob
import os
import random
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from oracle import Oracle

N = int(sys.argv[1]) if len(sys.argv) > 1 else 2000
rng = random.Random(4)
o = Oracle()
fails = {}


def fail(kind, case):
    fails.setdefault(kind, []).append(case)


def rand_pat(alpha, n):
    return "".join(rng.choice(alpha) for _ in range(rng.randint(0, n)))


# fnmatch -----------------------------------------------------------------
PAT_ALPHA = ["a", "b", ".", "/", "*", "?", "[", "]", "!", "^", "{", "}", ",", "\\", "a-b", "-"]
for _ in range(N * 5):
    pat = rand_pat(PAT_ALPHA, 6)
    name = rand_pat(["a", "b", ".", "/", "{", "}", ",", "\\", "^", "!", "-", "["], 5)
    if o.fnmatch(pat, name) != fnmatch.fnmatchcase(name, pat):
        fail("fnmatch", (pat, name))

# pytest python_files -------------------------------------------------------
from _pytest.pathlib import fnmatch_ex

for _ in range(N):
    pat = rand_pat(["c", "_", "*", "?", "/", ".py", "{c,d}", "[!c]", "[^c]"], 4)
    path = "/".join(rand_pat(["c", "d", "_", "x", "{c,d}"], 4) or "c" for _ in range(rng.randint(1, 3))) + ".py"
    want = fnmatch_ex(pat, "/rootdir/" + path)
    if o.pytest_file([pat], path) != want:
        fail("pytest", (pat, path))


# Python glob ---------------------------------------------------------------
NAMES = ["a", "b", ".h", "t{1,2}", "t1", "a b", "[x]", "x"]


def rand_tree(depth=3):
    dirs, files = [], []

    def go(base, d):
        for name in rng.sample(NAMES, rng.randint(0, 3)):
            p = f"{base}/{name}" if base else name
            if d < depth and rng.random() < 0.6:
                dirs.append(p)
                go(p, d + 1)
            else:
                files.append(p)
    go("", 0)
    return dirs, files


def materialize(root, dirs, files):
    for d in dirs:
        os.makedirs(os.path.join(root, d), exist_ok=True)
    for f in files:
        os.makedirs(os.path.dirname(os.path.join(root, f)) or root, exist_ok=True)
        open(os.path.join(root, f), "w").close()


GLOB_COMP = ["*", "a", "b", "?", "**", ".h", ".*", "t{1,2}", "t[12]", "[!a]", "[^a]", "a*", "x**", ""]
for _ in range(N // 4):
    dirs, files = rand_tree()
    pat = "/".join(rng.choice(GLOB_COMP) for _ in range(rng.randint(1, 3)))
    if not pat.strip("/"):
        continue
    root = tempfile.mkdtemp()
    try:
        materialize(root, dirs, files)
        cwd = os.getcwd()
        os.chdir(root)
        try:
            want = sorted(set(glob.iglob(pat, recursive=True)))
        finally:
            os.chdir(cwd)
    finally:
        shutil.rmtree(root)
    got = o.pyglob(pat, files, dirs)
    if got != want:
        fail("pyglob", (pat, dirs, files, got, want))

# tox -------------------------------------------------------------------------
from tox.config.loader.ini.factor import expand_factors

TOX_ALPHA = ["py", "38", "a", "!", ",", "-", "{", "}", " ", ":", ".", "_", "x", "é", "*", "[", "]", "/"]
for _ in range(N * 3):
    line = rand_pat(TOX_ALPHA, 8)
    # numeric ranges are outside the model
    if any(f"{d}-" in line or f"-{d}" in line for d in "0123456789"):
        continue
    (factors, content), = list(expand_factors(line))
    got = o.tox(line)
    want_f = None if factors is None else [[list(f) for f in g] for g in factors]
    if got["factors"] != want_f or got["content"] != content:
        fail("tox", (line, got, want_f, content))

# setuptools find --------------------------------------------------------------
from setuptools.discovery import PackageFinder, PEP420PackageFinder


def find_tree(names, dirs, base, d):
    for name in rng.sample(names, rng.randint(0, 3)):
        p = f"{base}/{name}" if base else name
        dirs.append(p)
        if d < 3:
            find_tree(names, dirs, p, d + 1)


FIND_PAT = ["acme*", "acme", "acme.*", "acme.{x,y}*", "acme.[^x]*", "acme.[!x]*", "*", "*.x", "x", "a?me*"]
for _ in range(N // 4):
    names = ["acme", "x", "y", "a.b", "__pycache__", "ez_setup"]
    dirs = []
    find_tree(names, dirs, "", 0)
    inits = [d for d in dirs if rng.random() < 0.7]
    inc = rng.sample(FIND_PAT, rng.randint(1, 2))
    exc = rng.sample(FIND_PAT, rng.randint(0, 2))
    ns = rng.random() < 0.5
    root = tempfile.mkdtemp()
    try:
        materialize(root, dirs, [d + "/__init__.py" for d in inits])
        finder = PEP420PackageFinder if ns else PackageFinder
        want = sorted(finder.find(root, exclude=exc, include=inc))
    finally:
        shutil.rmtree(root)
    got = o.find(dirs, inits, "", inc, exc, ns)
    if got != want:
        fail("find", (dirs, inits, inc, exc, ns, got, want))

# gitignore vs git --------------------------------------------------------------
GI_NAMES = ["a", "b", "ab", "c d", "x "]
def gi_tree(dirs, files, base, d):
    for name in rng.sample(GI_NAMES, rng.randint(0, 3)):
        p = f"{base}/{name}" if base else name
        if d < 2 and rng.random() < 0.6:
            dirs.append(p)
            gi_tree(dirs, files, p, d + 1)
        else:
            files.append(p + ".py")


GI_PAT = ["a", "b", "*", "?", "a*", "*b", "**", "/", "!", "\\ ", " ", "[ab]", "[!a]", "[^a]", "{a,b}",
          "#", "\\#", "\\!", "**/", "/**", ".py", "[", "]", "\\*"]
for _ in range(N // 4):
    dirs, files = [], []
    gi_tree(dirs, files, "", 0)
    ignores = {}
    for d in [""] + rng.sample(dirs, min(len(dirs), rng.randint(0, 2))):
        lines = []
        for _ in range(rng.randint(1, 4)):
            line = "".join(rng.choice(GI_PAT) for _ in range(rng.randint(1, 4)))
            if line.endswith("\\") and not line.endswith("\\\\"):
                continue
            if "[:" in line:
                continue
            lines.append(line)
        ignores[d] = "\n".join(lines) + "\n"
    root = tempfile.mkdtemp()
    try:
        materialize(root, dirs, files)
        for d, text in ignores.items():
            with open(os.path.join(root, d, ".gitignore"), "w") as f:
                f.write(text)
        subprocess.run(["git", "init", "-q", root], check=True)
        out = subprocess.run(["git", "-C", root, "ls-files", "--others", "--exclude-standard", "-z"],
                             capture_output=True, text=True, check=True).stdout
        want = sorted(p for p in out.split("\0") if p.endswith(".py"))
    finally:
        shutil.rmtree(root)
    got = sorted(o.gitignore(ignores, files))
    if got != want:
        fail("gitignore", (ignores, files, got, want))

for kind, cases in fails.items():
    print(f"{kind}: {len(cases)} disagreements")
    for c in cases[:5]:
        print("   ", c)
if not fails:
    print("all specs agree with their reference implementations")
