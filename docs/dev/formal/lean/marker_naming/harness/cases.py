"""Fixed cases the generators do not cover; expectations come from the
Lean spec, the observation from chokkin."""

import json

from common import chokkin, issues, oracle, pyproject

SP = ".venv/lib/python3.12/site-packages/"


def venv_case(title, dists, imports):
    """dists: (name, metadata_extra, record_rows, files_on_disk)"""
    files = {".venv/pyvenv.cfg": "version = 3.12.1\n"}
    query = []
    for name, extra, rows, on_disk in dists:
        info = f"{SP}{name.replace('-', '_')}-1.0.dist-info/"
        meta = f"Metadata-Version: 2.4\nName: {name}\nVersion: 1.0\n{extra}"
        record = "".join(r + "\n" for r in rows)
        files[info + "METADATA"] = meta
        files[info + "RECORD"] = record
        for f in on_disk:
            files[SP + f] = ""
        kind = "editable" if any("__editable__" in r for r in rows) else "wheel"
        query.append(
            {"kind": kind, "metadata": meta, "files": record, "dir_name": name}
        )
    [answer] = oracle([{"kind": "venv", "dists": query, "imports": imports}])
    declared = [d[0] for d in dists]
    files["pyproject.toml"] = pyproject(declared)
    files["main.py"] = "".join(f"import {m}\n" for m in imports)
    report = chokkin(files)
    used = {u for a in answer for u in a["used"]}
    expect = {"CHK002": sorted(set(declared) - used), "CHK010": []}
    got = {
        "CHK002": sorted(issues(report, "CHK002")),
        "CHK010": sorted(issues(report, "CHK010")),
    }
    print(("ok  " if expect == got else "DIFF"), title, "expect", expect, "got", got)


venv_case(
    "RECORD quoted path (CSV)",
    [("zz-h", "", ['"zzh/__init__.py",sha256=x,1'], ["zzh/__init__.py"])],
    ["zzh"],
)
venv_case(
    "Import-Name lower-case field name",
    [("zz-f", "import-name: zzf\n", ["__editable__.zz_f-1.0.pth,,"], [])],
    ["zzf"],
)
venv_case(
    "dotted Import-Name",
    [("zz-m", "Import-Name: zzm.sub\n", ["__editable__.zz_m-1.0.pth,,"], [])],
    ["zzm.sub"],
)
venv_case(
    "Import-Name; private",
    [("zz-p", "Import-Name: zzp; private\n", ["__editable__.zz_p-1.0.pth,,"], [])],
    ["zzp"],
)
venv_case(
    "namespace portions",
    [
        ("zz-auth-lib", "", ["zzcorp/auth/__init__.py,,"], ["zzcorp/auth/__init__.py"]),
        (
            "zz-billing-client",
            "",
            ["zzcorp/billing/__init__.py,,"],
            ["zzcorp/billing/__init__.py"],
        ),
    ],
    ["zzcorp.billing"],
)
venv_case(
    "description line looks like a field",
    [
        ("zzfoo", "", ["zzfoo/__init__.py,,"], ["zzfoo/__init__.py"]),
        (
            "zzdoc",
            "\nImport-Name: zzfoo\n",
            ["zzdoc/__init__.py,,"],
            ["zzdoc/__init__.py"],
        ),
    ],
    ["zzfoo"],
)

# setuptools extras_require keys
for key, runtime_dup in [
    (":python_version>='3.0'", False),
    (":python_version<'3.8'", True),
    ("test:sys_platform=='win32'", True),
]:
    [k] = oracle([{"kind": "key", "key": key}])
    dup = ", 'importlib-metadata'" if runtime_dup else ""
    setup = (
        "from setuptools import setup\n"
        f"setup(name='zzproj', version='0', packages=['zzproj'], install_requires=['requests'{dup}],\n"
        f"      extras_require={{{json.dumps(key)}: ['importlib-metadata']}})\n"
    )
    files = {
        "setup.py": setup,
        "zzproj/__init__.py": "import requests\n",
        "pyproject.toml": '[tool.chokkin]\nmode = "app"\nentry = ["zzproj/__init__.py"]\n',
    }
    if runtime_dup:
        # Different markers (or a marker against none) are no duplicate (spec §10 CHK009).
        [m] = oracle(
            [{"kind": "marker", "requires_python": ">=3", "marker": k["marker"]}]
        )
        expect = {"CHK009": []}
        got = {"CHK009": sorted(issues(chokkin(files), "CHK009"))}
    else:
        [m] = oracle(
            [{"kind": "marker", "requires_python": ">=3", "marker": k["marker"]}]
        )
        # An install requirement whose marker always holds and that nothing imports is CHK002.
        expect = {
            "CHK002": ["importlib-metadata"]
            if k["extra"] is None and m["verdict"] == "always"
            else []
        }
        got = {"CHK002": sorted(issues(chokkin(files), "CHK002"))}
    print(
        ("ok  " if expect == got else "DIFF"),
        "extras_require key",
        key,
        k,
        "expect",
        expect,
        "got",
        got,
    )
