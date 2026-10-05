"""Regenerate `tests/fixtures/pep508/packaging_golden.json` with `packaging` as
the oracle (dev only; tests read the JSON and never run Python).

    uvx --with packaging==24.0 python scripts/generate-pep508-golden.py
"""

import json
from pathlib import Path

import packaging
from packaging.requirements import InvalidRequirement, Requirement
from packaging.utils import canonicalize_name

CASES = [
    # Issue #503
    "foo==1.99999999999999999999",
    "foo>=1.99999999999999999999",
    "foo~=1.99999999999999999999",
    "foo===1.*",
    "foo===",
    "foo @ HTTPS://example.com/foo.whl",
    "foo @ Git+HTTPS://example.com/foo.git",
    "foo @ ftp://example.com/foo.tar.gz",
    "foo @ https://example.com/a b.whl",
    "foo.whl[tests]",
    "foo.tar.gz",
    "foo[tests,]",
    "foo[,tests]",
    "foo[tests,,dev]",
    "A",
    "Foo_Bar.baz-qux",
    "foo-",
    "-foo",
    "foo[]",
    "foo [ Tests , Dev ]",
    "foo[tests dev]",
    "foo[te-]",
    "foo[",
    "foo>=1.0,<2",
    "foo (>=1.0, <2)",
    "foo (>=1.0",
    "foo==1.0.*",
    "foo!=1.0.*",
    "foo>=1.0.*",
    "foo==1.0a1.*",
    "foo~=1",
    "foo~=1.0.post1",
    "foo>=1.0+local",
    "foo==1.0+local",
    "foo===2013b-custom",
    "foo==v1.0",
    "foo==1!2.0rc1.post3.dev4",
    "foo>= 1.0 extra",
    "foo==",
    "foo<=>1",
    "foo @ https://h/x.whl ; python_version < '3.11'",
    "foo @ https://h/x.whl;python_version<'3.11'",
    "foo @ https://h/x.whl#sha256=abc",
    "foo @ file:///tmp/x",
    "foo @ https://h/x trailing",
    "foo@https://h/x",
    "foo @",
    "foo ; python_version >= '3.8'",
    "foo; extra == 'tests' and (os_name == 'nt' or sys_platform == 'linux')",
    "foo ; extra not in 'a'",
    "foo ;",
    "foo ; python_version",
    "foo ; unknown_var == '1'",
    "foo ; python_version << '3'",
    "foo ; (python_version == '3'",
    "foo ; python_version == '3",
]


def read(raw: str) -> dict:
    try:
        req = Requirement(raw)
    except InvalidRequirement:
        return {"input": raw, "valid": False}
    return {
        "input": raw,
        "valid": True,
        "name": canonicalize_name(req.name),
        "extras": sorted(canonicalize_name(extra) for extra in req.extras),
    }


golden = {"packaging": packaging.__version__, "cases": [read(raw) for raw in CASES]}
out = Path(__file__).resolve().parent.parent / "tests/fixtures/pep508/packaging_golden.json"
out.write_text(json.dumps(golden, indent=2, ensure_ascii=False) + "\n")
