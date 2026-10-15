"""Regenerate `tests/fixtures/pep508/packaging_golden.json` with `packaging` as
the oracle (dev only; tests read the JSON and never run Python).

    uvx --with packaging==24.0 python scripts/generate-pep508-golden.py
"""

import json
from pathlib import Path

import packaging
from packaging.requirements import InvalidRequirement, Requirement
from packaging.specifiers import InvalidSpecifier
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
    "foo>=1.0,",
    "foo (>=1.0, <2 ,)",
    "foo>=1.0,,",
    "foo ,",
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
    # Issue #572: `packaging` ends a name at a word boundary, so `_` may end it
    "foo_",
    "foo-_",
    "foo._",
    "foo_.",
    "foo[te_]",
    "foo_[a]>=1",
    "foo_ @ https://h/x.whl",
    "foo- @ https://h/x.whl",
    # Issue #572: `===` reads up to whitespace, `;` or `)`, commas included,
    # and the set is split on commas afterwards
    "foo===1.0,>=1",
    "foo===1.0 , >=1",
    "foo===1.0, >=1",
    "foo (===1.0,>=1)",
    "foo (===1.0, >=1)",
    "foo===1.0,,",
    "foo===1.0,,>=1",
    "foo>=1,===2,,",
    "foo===,>=1",
    "foo=== 1.0,>=1",
    "foo===1.0,>= 2",
    "foo===1.0,foo",
    "foo===1.0)",
    "foo===1.0,>=1;python_version>'3'",
    "foo>=1,,<2",
    # Issue #879: only space and tab separate tokens; `packaging` reads
    # `\s*` (any Unicode whitespace) only between an operator and its version
    "foo\u00a0>=1",
    "bar>=1\u00a0,<2",
    "foo>=1,\u00a0<2",
    "baz\u00a0[a]",
    "foo[\u00a0a]",
    "foo[a\u00a0]",
    "foo\u00a0@ https://h/x.whl",
    "foo @\u00a0https://h/x.whl",
    "foo @\u00a0 https://h/x.whl",
    "foo @\u3000https://h/x.whl",
    "foo @ https://h/x.whl \u00a0;python_version>'3'",
    "foo;\u00a0python_version>'3'",
    "foo; python_version\u00a0>'3'",
    "foo; python_version>'\u00a03'",
    "foo (\u00a0>=1)",
    "foo (>=1\u00a0)",
    "foo>=\u00a01",
    "foo===\u00a01.0",
    "foo===1.0\u00a0,>=1",
    "foo\u3000>=1",
    "foo\x0b>=1",
    "foo>=1\n,<2",
    "foo\t>=1",
    "foo>=1 ;\tpython_version>'3'",
    # Issue #890: Python's `\s` also matches U+001C..U+001F (none at the
    # ends: callers strip the whole requirement like `str.strip`)
    "foo>=\x1f1",
    "foo>=\x1c1",
    "foo===\x1f1.0",
    "foo===1.0\x1f,>=1",
    "foo\x1f>=1",
    "foo>=1\x1f,<2",
    "foo;\x1fpython_version>'3'",
    "foo @\x1fhttps://h/x.whl",
]


def read(raw: str) -> dict:
    try:
        req = Requirement(raw)
    # Before 26.0, a bad item after `===`'s commas escapes as InvalidSpecifier.
    except (InvalidRequirement, InvalidSpecifier):
        return {"input": raw, "valid": False}
    return {
        "input": raw,
        "valid": True,
        "name": canonicalize_name(req.name),
        "extras": sorted(canonicalize_name(extra) for extra in req.extras),
        "specifiers": sorted(str(specifier) for specifier in req.specifier),
    }


golden = {"packaging": packaging.__version__, "cases": [read(raw) for raw in CASES]}
out = Path(__file__).resolve().parent.parent / "tests/fixtures/pep508/packaging_golden.json"
out.write_text(json.dumps(golden, indent=2) + "\n")
