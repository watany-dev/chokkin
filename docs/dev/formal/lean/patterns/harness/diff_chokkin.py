"""Differential run: the Lean oracle against target/release/chokkin.

Each family builds small projects, asks the Lean spec what the tool that
reads the setting would do, runs chokkin as a black box (`--reporter json`)
and compares one observable:

* gitignore  - which `.py` files are scanned (CHK001 in app mode)
* pyfiles    - which files pytest collects through `python_files`
* testpaths  - which files pytest collects through `testpaths` + `python_files`
* find       - which files a setuptools `packages.find` wheel ships (CHK001 in library mode)
* tox        - whether a factor-conditional `deps` line installs its requirement (CHK008)

    python3 harness/diff_chokkin.py FAMILY N [SEED]
"""
import json
import os
import random
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import chk
from oracle import Oracle

o = Oracle()
APP = '[project]\nname="t"\nversion="0"\n[tool.chokkin]\nmode="app"\nentry=["main.py"]\n'


def toml_list(items):
    return "[" + ", ".join(json.dumps(i) for i in items) + "]"


def tree(rng, dir_names, file_names, depth=2, p_dir=0.6):
    dirs, files = [], []

    def go(base, d):
        for name in rng.sample(dir_names, rng.randint(0, min(3, len(dir_names)))):
            p = f"{base}/{name}" if base else name
            if d < depth and rng.random() < p_dir:
                dirs.append(p)
                go(p, d + 1)
        files.extend(f"{base}/{name}" if base else name
                     for name in rng.sample(file_names, rng.randint(0, min(2, len(file_names)))))
    go("", 0)
    return dirs, files


# ---------------------------------------------------------------- gitignore
GI_PAT = ["a", "b", "ab", "*", "?", "a*", "*b", "**", "/", "!", "\\ ", " ", "[ab]", "[!a]", "[^a]",
          "{a,b}", "\\#", "\\!", "**/", "/**", ".py", "\\*", "c d", "x "]


def case_gitignore(rng):
    dirs, files = tree(rng, ["a", "b", "ab", "c d", "x "], ["a.py", "b.py", "ab.py"])
    if not files:
        return None
    ignores = {}
    for d in [""] + rng.sample(dirs, min(len(dirs), rng.randint(0, 2))):
        lines = []
        for _ in range(rng.randint(1, 4)):
            line = "".join(rng.choice(GI_PAT) for _ in range(rng.randint(1, 3)))
            if line.endswith("\\") and not line.endswith("\\\\"):
                continue
            lines.append(line)
        ignores[d] = "\n".join(lines) + "\n"
    proj = {"pyproject.toml": APP, "main.py": ""}
    for d in dirs:
        proj[d] = None
    for f in files:
        proj[f] = ""
    for d, text in ignores.items():
        proj[os.path.join(d, ".gitignore")] = text
    want = o.gitignore(ignores, files)
    got = reported_files(proj, files)
    return {"ignores": ignores, "files": files}, sorted(set(files) - set(want)), sorted(set(files) - set(got))


def reported_files(proj, files):
    rep = chk.run(proj)
    reported = set(chk.codes(rep, "CHK001"))
    return sorted(f for f in files if f in reported)


# ------------------------------------------------------------ python_files
PYF = ["chk", "_", "*", "?", "{chk,x}", "[!c]", "[^c]", "[c]", "x"]


def case_pyfiles(rng):
    pats = []
    for _ in range(rng.randint(1, 2)):
        p = "".join(rng.choice(PYF) for _ in range(rng.randint(1, 3))) + ".py"
        pats.append(p)
    dirs, files = tree(rng, ["chk_d", "x", "{chk,x}", "^d"], ["chk.py", "x.py", "chk_a.py", "{chk,x}.py",
                                                               "^a.py", "xchk.py"])
    if not files:
        return None
    proj = {"pyproject.toml": APP + "[tool.pytest.ini_options]\npython_files = " + toml_list(pats) + "\n",
            "main.py": ""}
    for d in dirs:
        proj[d] = None
    for f in files:
        proj[f] = ""
    want = sorted(f for f in files if o.pytest_file(pats, f))
    got = sorted(set(files) - set(reported_files(proj, files)))
    return {"python_files": pats, "files": files}, want, got


# ------------------------------------------------------------ testpaths
TPC = ["*", "a", "b", "?", "**", "t{1,2}", "t[12]", "[!a]", "[^a]", "nope", "t1", "a*"]


def case_testpaths(rng):
    dirs, _ = tree(rng, ["a", "b", "t1", "t{1,2}", "ab"], [], depth=2, p_dir=0.7)
    if not dirs:
        return None
    files = [d + "/chk.py" for d in dirs]
    tps = ["/".join(rng.choice(TPC) for _ in range(rng.randint(1, 2))) for _ in range(rng.randint(1, 2))]
    proj = {"pyproject.toml": APP + "[tool.pytest.ini_options]\npython_files = [\"chk.py\"]\ntestpaths = "
            + toml_list(tps) + "\n", "main.py": ""}
    for f in files:
        proj[f] = ""
    expanded = []
    for tp in tps:
        expanded += o.pyglob(tp, files + ["main.py", "pyproject.toml"], dirs)
    expanded = [e.rstrip("/") for e in expanded]
    if not expanded:
        expanded = [""]  # pytest falls back to the rootdir
    want = sorted(f for f in files if any(e == "" or f == e or f.startswith(e + "/") for e in expanded))
    got = sorted(set(files) - set(reported_files(proj, files)))
    return {"testpaths": tps, "dirs": dirs}, want, got


# ------------------------------------------------------------ setuptools find
FIND = ["acme*", "acme", "acme.*", "acme.{x,y}*", "acme.[^x]*", "acme.[!x]*", "*", "*.x", "x*", "a?me*",
        "acme.x", "acme\\.x"]
LIB = ('[build-system]\nrequires=["setuptools"]\nbuild-backend="setuptools.build_meta"\n'
       '[project]\nname="acme"\nversion="0"\n[tool.chokkin]\nmode="library"\n'
       '[tool.setuptools.packages.find]\ninclude={inc}\nexclude={exc}\nnamespaces={ns}\n')


def case_find(rng):
    dirs = ["acme"]

    def go(base, d):
        for name in rng.sample(os.environ.get("FIND_NAMES", "x,y,a.b").split(","), rng.randint(0, 2)):
            p = f"{base}/{name}"
            dirs.append(p)
            if d < 2:
                go(p, d + 1)
    go("acme", 0)
    inits = [d for d in dirs if d == "acme" or rng.random() < 0.7]
    inc = rng.sample(FIND, rng.randint(1, 2))
    exc = rng.sample(FIND, rng.randint(0, 2))
    ns = rng.random() < 0.5
    files = [d + "/m.py" for d in dirs]
    proj = {"pyproject.toml": LIB.format(inc=toml_list(inc), exc=toml_list(exc), ns=str(ns).lower())}
    for d in inits:
        proj[d + "/__init__.py"] = ""
    for f in files:
        proj[f] = ""
    pkgs = o.find(dirs, inits, "", inc, exc, ns)
    shipped = sorted(f for f in files if f.rsplit("/", 1)[0].replace("/", ".") in pkgs)
    # chokkin documents that targets matching no file leave every file public
    want = shipped if shipped else files
    got = sorted(set(files) - set(reported_files(proj, files)))
    return {"include": inc, "exclude": exc, "namespaces": ns, "dirs": dirs, "inits": inits}, sorted(want), got


# ------------------------------------------------------------ tox
TOX = ["py", "38", "a", "!", ",", "-", "{", "}", " ", ".", "_", "x", "é", "*", "[", "]", "py39", "lint"]


def case_tox(rng):
    cond = "".join(rng.choice(TOX) for _ in range(rng.randint(1, 6)))
    if any(f"{d}-" in cond or f"-{d}" in cond for d in "0123456789") or ":" in cond:
        return None
    sep = rng.choice([": ", ":", ":\t"])
    line = f"{cond}{sep}black"
    ans = o.tox(line)
    installs = ans["factors"] not in (None, []) and ans["content"] == "black"
    proj = {"pyproject.toml": APP, "main.py": "",
            "tox.ini": f"[tox]\nenv_list = py38\n[testenv]\ndeps =\n    {line}\ncommands =\n    black .\n"}
    rep = chk.run(proj)
    got_installs = "black" not in chk.codes(rep, "CHK008")
    return {"line": line, "tox": ans}, installs, got_installs


FAMILIES = {"gitignore": case_gitignore, "pyfiles": case_pyfiles, "testpaths": case_testpaths,
            "find": case_find, "tox": case_tox}

if __name__ == "__main__":
    fam, n = sys.argv[1], int(sys.argv[2])
    rng = random.Random(int(sys.argv[3]) if len(sys.argv) > 3 else 1)
    bad = 0
    for _ in range(n):
        r = FAMILIES[fam](rng)
        if r is None:
            continue
        case, want, got = r
        if want != got:
            bad += 1
            print(json.dumps({"case": case, "spec": want, "chokkin": got}, ensure_ascii=False))
    print(f"# {fam}: {bad} mismatches in {n} cases", file=sys.stderr)
