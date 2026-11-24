"""Generate small projects that reach ``pkg.m`` through one binding path each.

A case is ``source`` (code that yields an expression for the module object
``pkg.m``) x ``carrier`` (code that passes that value through some binding
construct) x ``sink`` (code that reads ``.target`` / ``.Target`` /
``.TARGET`` off it). ``pkg/m.py`` also defines ``other`` / ``Other`` /
``OTHER``, which no case reads: the control that both sides must leave
unreferenced.

Negative cases (``NEG``) are written so that, by the Language Reference, the
read does *not* reach ``pkg.m``; the spec must then claim no reference.
"""

import itertools

M_PY = '''\
def target():
    return 1


def other():
    return 2


class Target:
    pass


class Other:
    pass


TARGET = 1
OTHER = 2
'''

# (name, setup lines, expression, extra files)
SOURCES = [
    ("import", ["import pkg.m"], "pkg.m", {}),
    ("import_as", ["import pkg.m as mm"], "mm", {}),
    ("from_pkg", ["from pkg import m"], "m", {}),
    ("from_pkg_as", ["from pkg import m as mm"], "mm", {}),
    ("import_module", ["import importlib"], 'importlib.import_module("pkg.m")', {}),
    ("import_module_from", ["from importlib import import_module"], 'import_module("pkg.m")', {}),
    ("import_module_rel", ["import importlib"], 'importlib.import_module(".m", "pkg")', {}),
    ("sys_modules", ["import sys", "import pkg.m"], 'sys.modules["pkg.m"]', {}),
    ("dunder_import_fromlist", [], '__import__("pkg.m", fromlist=["target"])', {}),
    ("dunder_import_top", [], '__import__("pkg.m").m', {}),
    ("pkg_attr_reexport", ["import pkg"], "pkg.m", {"pkg/__init__.py": "from . import m\n"}),
    ("pkg_attr_alias", ["import pkg"], "pkg.mod", {"pkg/__init__.py": "from . import m as mod\n"}),
    ("getattr_pkg", ["import pkg.m"], 'getattr(pkg, "m")', {}),
    ("import_two", ["import pkg.m, pkg"], "pkg.m", {}),
    ("import_parent_then_sub", ["import pkg", "import pkg.m"], "pkg.m", {}),
]

# (name, template lines using {E}, resulting expression, needs dotted E)
CARRIERS = [
    ("direct", [], "{E}"),
    ("alias", ["f = {E}"], "f"),
    ("chain_alias", ["f = {E}", "g = f"], "g"),
    ("tuple_unpack", ["a, f = 0, {E}"], "f"),
    ("list_unpack", ["[a, f] = [0, {E}]"], "f"),
    ("star_unpack", ["a, *rest = 0, {E}"], "rest[0]"),
    ("list_index", ["L = [{E}]"], "L[0]"),
    ("tuple_index", ["T = (0, {E})"], "T[1]"),
    ("walrus", ["if (f := {E}):", "    pass"], "f"),
    ("func_return", ["def get():", "    return {E}"], "get()"),
    ("identity_call", ["def ident(x):", "    return x"], "ident({E})"),
    ("kw_call", ["def ident(*, x):", "    return x"], "ident(x={E})"),
    ("default_arg", ["def get(x={E}):", "    return x"], "get()"),
    ("lambda", ["g = lambda: {E}"], "g()"),
    ("class_attr", ["class Holder:", "    m = {E}"], "Holder.m"),
    ("instance_attr", ["class Holder:", "    def __init__(self):", "        self.m = {E}"], "Holder().m"),
    ("method_return", ["class Holder:", "    def get(self):", "        return {E}"], "Holder().get()"),
    ("staticish", ["class Holder:", "    @staticmethod", "    def get():", "        return {E}"], "Holder.get()"),
    ("global_in_func", ["def init():", "    global f", "    f = {E}", "", "", "init()"], "f"),
    ("nonlocal", ["def outer():", "    f = None", "", "    def inner():", "        nonlocal f",
                  "        f = {E}", "", "    inner()", "    return f"], "outer()"),
    ("for_loop", ["for f in [{E}]:", "    pass"], "f"),
    ("for_tuple", ["for a, f in [(0, {E})]:", "    pass"], "f"),
    ("listcomp", ["L = [x for x in [{E}]]"], "L[0]"),
    ("walrus_in_comp", ["L = [(f := x) for x in [{E}]]"], "f"),
    ("match_capture", ["match {E}:", "    case f:", "        pass"], "f"),
    ("match_as", ["match {E}:", "    case object() as f:", "        pass"], "f"),
    ("match_seq", ["match (0, {E}):", "    case (a, f):", "        pass"], "f"),
    ("ifexp", ["f = {E} if len('x') else None"], "f"),
    ("or_expr", ["f = None or {E}"], "f"),
    ("try_assign", ["try:", "    f = {E}", "except Exception:", "    f = None"], "f"),
    ("with_body", ["import contextlib", "with contextlib.suppress(Exception):", "    f = {E}"], "f"),
    ("ann_assign", ["f: object = {E}"], "f"),
    ("decorated_return", ["def deco(fn):", "    return fn", "", "", "@deco", "def get():",
                          "    return {E}"], "get()"),
    ("module_store", ["import pkg.reg", "pkg.reg.h = {E}"], "pkg.reg.h"),
    ("module_store_from", ["import pkg.reg", "from pkg import reg", "reg.h = {E}"], "pkg.reg.h"),
]

# (name, template using {X} the full attribute expression, symbol read)
SINKS = [
    ("call", ["{X}()"], "target", "target"),
    ("load", ["x = {X}"], "target", "target"),
    ("print", ["print({X})"], "target", "target"),
    ("in_func", ["def run():", "    return {X}"], "target", "target"),
    ("in_class", ["class User:", "    x = {X}"], "target", "target"),
    ("getattr_lit", ["{G}"], "target", "target"),
    ("decorator", ["@{X}", "def run():", "    pass"], "target", "target"),
    ("base_class", ["class Sub({X}):", "    pass"], "Target", "Target"),
    ("annotation", ["x: {X} = None"], "Target", "Target"),
    ("param_annotation", ["def run(v: {X}):", "    return v"], "Target", "Target"),
    ("constant", ["print({X})"], "TARGET", "TARGET"),
    ("isinstance", ["isinstance(1, {X})"], "Target", "Target"),
]

NEG = [
    # (name, main body lines, expected: the read does not reach pkg.m.target)
    ("class_scope_hidden", ["import pkg.m", "m = None", "", "", "class C:", "    m = pkg.m", "",
                            "    def g(self):", "        return m.target()"]),
    ("local_shadow", ["import pkg.m", "", "", "def f():", "    pkg = None",
                      "    return pkg.m.target()"]),
    ("param_shadow", ["import pkg.m", "", "", "def f(pkg):", "    return pkg.m.target()"]),
    ("comp_shadow", ["import pkg.m", "x = [pkg.m.target for pkg in [None]]"]),
    ("lambda_shadow", ["import pkg.m", "g = lambda pkg: pkg.m.target"]),
    ("typeparam_shadow", ["import pkg.m", "", "", "def f[pkg]() -> None:", "    return pkg.m.target"]),
    ("other_object", ["import pkg.m", "", "", "class O:", "    target = 1", "", "",
                      "O().target", "O.target"]),
    ("except_shadow", ["import pkg.m", "", "", "def f():", "    try:", "        pass",
                       "    except Exception as pkg:", "        pkg.m.target"]),
    ("match_shadow", ["import pkg.m", "", "", "def f():", "    match 1:", "        case pkg:",
                      "            pkg.m.target"]),
    ("for_shadow", ["import pkg.m", "", "", "def f():", "    for pkg in [None]:",
                    "        pkg.m.target"]),
    ("with_shadow", ["import pkg.m", "import contextlib", "", "", "def f():",
                     "    with contextlib.nullcontext() as pkg:", "        pkg.m.target"]),
    ("del_attr", ["import pkg.m", "del pkg.m.target"]),
    ("store_attr", ["import pkg.m", "pkg.m.target = None"]),
    ("other_module_same_name", ["import pkg.m", "import pkg.n", "pkg.n.target()"]),
    ("from_other_module", ["import pkg.m", "from pkg.n import target", "target()"]),
    ("global_name_only", ["import pkg.m", "target = 1", "print(target)"]),
    ("nested_func_local", ["import pkg.m", "", "", "def f():", "    pkg = None", "",
                           "    def g():", "        return pkg.m.target", "    return g"]),
    ("walrus_shadow", ["import pkg.m", "", "", "def f():", "    if (pkg := None):", "        pass",
                       "    return pkg.m.target"]),
    ("import_shadow_in_func", ["import pkg.m", "", "", "def f():", "    import pkg.n as pkg",
                               "    return pkg.m.target"]),
]

PYPROJECT = '''\
[project]
name = "demo"
version = "0"
dependencies = []

[tool.chokkin]
mode = "app"
entry = ["main.py"]
'''


def _fmt(lines, **kw):
    return [line.format(**kw) for line in lines]


def project(main_lines, extra=None):
    files = {
        "pyproject.toml": PYPROJECT,
        "pkg/__init__.py": "",
        "pkg/m.py": M_PY,
        "pkg/n.py": "def target():\n    return 3\n",
        "pkg/reg.py": "",
        "main.py": "\n".join(main_lines) + "\n",
    }
    # main must import pkg.n and pkg.reg so that they are not CHK001 noise.
    files["main.py"] += "import pkg.n\nimport pkg.reg\n"
    files.update(extra or {})
    return files


def positive_cases():
    for (sn, setup, e, extra), (cn, ctpl, cexpr), (kn, ktpl, sym, _) in itertools.product(
        SOURCES, CARRIERS, SINKS
    ):
        lines = list(setup)
        lines += _fmt(ctpl, E=e)
        v = cexpr.format(E=e)
        lines += _fmt(ktpl, X=f"{v}.{sym}", G=f'getattr({v}, "{sym}")')
        yield f"{sn}+{cn}+{kn}", project(lines, extra), [("pkg.m", sym)]


def star_cases():
    cases = [
        ("star_main", {"main.py": None}, ["from pkg.m import *", "target()"]),
        ("star_all", {"pkg/m.py": M_PY + '\n__all__ = ["target", "Target", "TARGET", "other"]\n'},
         ["from pkg.m import *", "target()"]),
        ("star_pkg_attr", {"pkg/__init__.py": "from .m import *\n"}, ["import pkg", "pkg.target()"]),
        ("star_pkg_from", {"pkg/__init__.py": "from .m import *\n"}, ["from pkg import target", "target()"]),
        ("star_pkg_abs", {"pkg/__init__.py": "from pkg.m import *\n"}, ["import pkg", "pkg.target()"]),
        ("star_chain", {"pkg/mid.py": "from pkg.m import *\n"},
         ["from pkg.mid import *", "target()", "import pkg.mid"]),
        ("star_chain_attr", {"pkg/mid.py": "from .m import *\n"}, ["import pkg.mid", "pkg.mid.target()"]),
        ("star_in_func_user", {}, ["from pkg.m import *", "", "", "def run():", "    return target()"]),
        ("reexport_attr", {"pkg/__init__.py": "from .m import target\n"}, ["import pkg", "pkg.target()"]),
        ("reexport_as", {"pkg/__init__.py": "from .m import target as t\n"}, ["from pkg import t", "t()"]),
        ("reexport_module", {"pkg/__init__.py": "from . import m\n"}, ["from pkg import m", "m.target()"]),
        ("reexport_chain", {"pkg/__init__.py": "from .mid import target\n", "pkg/mid.py": "from .m import target\n"},
         ["import pkg", "pkg.target()", "import pkg.mid"]),
        ("private_all", {"pkg/m.py": M_PY + '\ndef _target():\n    return 1\n\n\n__all__ = ["_target"]\n'},
         ["from pkg.m import *", "_target()"]),
        ("relative_user", {"pkg/user.py": "from . import m\n\nm.target()\n"}, ["import pkg.user"]),
        ("relative_user_dot_m", {"pkg/user.py": "from .m import target\n\ntarget()\n"}, ["import pkg.user"]),
        ("relative_parent", {"pkg/sub/__init__.py": "", "pkg/sub/user.py": "from .. import m\n\nm.target()\n"},
         ["import pkg.sub.user"]),
        ("relative_parent_from_m", {"pkg/sub/__init__.py": "", "pkg/sub/user.py": "from ..m import target\n\ntarget()\n"},
         ["import pkg.sub.user"]),
        ("import_module_rel_user", {"pkg/user.py": "import importlib\n\nimportlib.import_module('.m', __package__).target()\n"},
         ["import pkg.user"]),
        ("import_module_rel_pkgname", {"pkg/user.py": "import importlib\n\nimportlib.import_module('.m', 'pkg').target()\n"},
         ["import pkg.user"]),
        ("cross_module_store", {"pkg/reg.py": "h = None\n", "pkg/user.py": "import pkg.reg\n\npkg.reg.h.target()\n"},
         ["import pkg.m", "import pkg.reg", "pkg.reg.h = pkg.m", "import pkg.user"]),
        ("cross_module_return", {"pkg/user.py": "import pkg.m\n\n\ndef get():\n    return pkg.m\n"},
         ["import pkg.user", "pkg.user.get().target()"]),
        ("cross_module_alias", {"pkg/user.py": "import pkg.m\n\nmod = pkg.m\n"},
         ["import pkg.user", "pkg.user.mod.target()"]),
        ("cross_module_alias_from", {"pkg/user.py": "import pkg.m as mod\n"},
         ["from pkg.user import mod", "mod.target()"]),
        ("cross_module_param", {"pkg/user.py": "def use(x):\n    return x.target()\n"},
         ["import pkg.m", "import pkg.user", "pkg.user.use(pkg.m)"]),
        ("cross_module_global", {"pkg/user.py": "f = None\n\n\ndef init():\n    global f\n    import pkg.m as f\n"},
         ["import pkg.user", "pkg.user.init()", "pkg.user.f.target()"]),
    ]
    for name, extra, main in cases:
        extra = {k: v for k, v in extra.items() if v is not None}
        yield name, project(main, extra), [("pkg.m", "target")] if name != "private_all" else [("pkg.m", "_target")]


def negative_cases():
    for name, main in NEG:
        yield "neg:" + name, project(main), []


def all_cases():
    yield from star_cases()
    yield from negative_cases()
    yield from positive_cases()


# Where the import statement and the read sit. {I} is the import, {U} the read.
PLACEMENTS = [
    ("toplevel", ["{I}", "{U}"]),
    ("func_import_and_use", ["def run():", "    {I}", "    {U}"]),
    ("func_use_only", ["{I}", "", "", "def run():", "    {U}"]),
    ("async_func", ["{I}", "", "", "async def run():", "    {U}"]),
    ("nested_func", ["{I}", "", "", "def outer():", "    def inner():", "        {U}", "    return inner"]),
    ("nested_import", ["def outer():", "    {I}", "", "    def inner():", "        {U}", "    return inner"]),
    ("class_body", ["{I}", "", "", "class C:", "    x = {X}"]),
    ("class_import", ["class C:", "    {I}", "    x = {X}"]),
    ("method", ["{I}", "", "", "class C:", "    def run(self):", "        {U}"]),
    ("method_import", ["class C:", "    def run(self):", "        {I}", "        {U}"]),
    ("if_block", ["if len('x'):", "    {I}", "    {U}"]),
    ("if_import_use_after", ["if len('x'):", "    {I}", "{U}"]),
    ("else_block", ["if len(''):", "    pass", "else:", "    {I}", "{U}"]),
    ("try_import", ["try:", "    {I}", "except ImportError:", "    pass", "{U}"]),
    ("try_except_fallback", ["try:", "    {I}", "except ImportError:", "    {I}", "{U}"]),
    ("finally_block", ["try:", "    pass", "finally:", "    {I}", "{U}"]),
    ("with_block", ["import contextlib", "with contextlib.suppress(Exception):", "    {I}", "{U}"]),
    ("for_body", ["for _ in range(1):", "    {I}", "    {U}"]),
    ("while_body", ["while len(''):", "    {I}", "    {U}"]),
    ("match_body", ["match 1:", "    case 1:", "        {I}", "        {U}"]),
    ("lambda", ["{I}", "g = lambda: {X}"]),
    ("listcomp", ["{I}", "L = [{X} for _ in range(1)]"]),
    ("genexp_in_func", ["{I}", "", "", "def run():", "    return list({X} for _ in range(1))"]),
    ("dictcomp", ["{I}", "D = {{k: {X} for k in range(1)}}"]),
    ("default_arg", ["{I}", "", "", "def run(x={X}):", "    return x"]),
    ("decorator_arg", ["{I}", "", "", "def deco(x):", "    return lambda f: f", "", "",
                       "@deco({X})", "def run():", "    pass"]),
    ("return_annotation", ["{I}", "", "", "def run() -> {X}:", "    pass"]),
    ("kw_default_lambda", ["{I}", "g = lambda x={X}: x"]),
    ("global_import", ["def init():", "    global pkg", "    {I}", "", "", "init()", "{U}"]),
    ("fstring", ["{I}", "s = f\"{{{X}}}\""]),
    ("assert_stmt", ["{I}", "assert {X}"]),
    ("raise_stmt", ["{I}", "", "", "def run():", "    raise {X}"]),
    ("del_name_then_use", ["{I}", "x = {X}", "del x"]),
    ("type_alias", ["{I}", "type A = {X}"]),
    ("generic_func", ["{I}", "", "", "def run[T](x: T) -> T:", "    {U}", "    return x"]),
    ("generic_class", ["{I}", "", "", "class C[T]:", "    x = {X}"]),
    ("subscript_index", ["{I}", "D = {{}}", "D[{X}] = 1"]),
    ("star_arg", ["{I}", "print(*[{X}])"]),
    ("kwarg", ["{I}", "print(sep={X})"]),
    ("walrus_value", ["{I}", "if (v := {X}):", "    pass"]),
    ("chained_compare", ["{I}", "b = 0 < {X}"]),
    ("slice", ["{I}", "L = [1][{X}:]"]),
    ("await_in_async", ["{I}", "", "", "async def run():", "    await {X}()"]),
    ("yield_in_gen", ["{I}", "", "", "def run():", "    yield {X}"]),
    ("starred_assign_target", ["{I}", "a, *b = [{X}]"]),
    ("augassign_value", ["{I}", "x = 0", "x += {X}"]),
    ("annassign_noval_ann", ["{I}", "", "", "def run():", "    v: {X}"]),
    ("global_stmt_use", ["{I}", "", "", "def run():", "    global pkg", "    {U}"]),
    ("conditional_expr", ["{I}", "v = {X} if len('') else None"]),
]

IMPORT_FORMS = [
    ("import", "import pkg.m", "pkg.m"),
    ("import_as", "import pkg.m as mm", "mm"),
    ("from_pkg", "from pkg import m", "m"),
    ("from_m", "from pkg.m import target", None),
    ("from_m_as", "from pkg.m import target as tt", None),
]


def placement_cases():
    for (pn, tpl), (iname, imp, e) in itertools.product(PLACEMENTS, IMPORT_FORMS):
        if e is None:
            x = "tt" if iname.endswith("_as") else "target"
        else:
            x = f"{e}.target"
        if pn == "global_import" and iname != "import":
            continue
        lines = [line.format(I=imp, U=f"{x}()", X=x) for line in tpl]
        yield f"place:{pn}:{iname}", project(lines), [("pkg.m", "target")]


_all_cases_v1 = all_cases


def all_cases():
    yield from _all_cases_v1()
    yield from placement_cases()


def package_cases():
    """Packages, `__init__` re-exports (CHK007) and submodule attributes."""
    M = "def target():\n    return 1\n\n\ndef other():\n    return 2\n"
    cases = [
        ("star_pkg_all_submodule", {"pkg/__init__.py": '__all__ = ["m"]\n'},
         ["from pkg import *", "m.target()", "import pkg.m as _unused"]),
        ("subpkg_attr", {"pkg/m.py": None, "pkg/m/__init__.py": M, "pkg/m/sub.py": ""},
         ["import pkg.m.sub", "pkg.m.target()"]),
        ("subpkg_attr_as", {"pkg/m.py": None, "pkg/m/__init__.py": M, "pkg/m/sub.py": ""},
         ["import pkg.m.sub as s", "import pkg", "pkg.m.target()"]),
        ("imported_elsewhere", {"pkg/loader.py": "import pkg.m\n"},
         ["import pkg.loader", "import pkg", "pkg.m.target()"]),
        ("reexport_two_levels", {"pkg/sub/__init__.py": "from .m import target\n", "pkg/sub/m.py": M, "pkg/m.py": M,
                                 "pkg/__init__.py": "from .sub import target\n"},
         ["from pkg import target", "target()", "import pkg.m"]),
        ("reexport_star_two_levels", {"pkg/sub/__init__.py": "from .m import *\n", "pkg/sub/m.py": M, "pkg/m.py": M,
                                      "pkg/__init__.py": "from .sub import *\n"},
         ["import pkg", "pkg.target()", "import pkg.m"]),
        ("reexport_used_in_func", {"pkg/__init__.py": "from .m import target\n"},
         ["import pkg", "", "", "def run():", "    return pkg.target()"]),
        ("reexport_alias_module", {"pkg/__init__.py": "from .m import target\n"},
         ["import pkg as p", "p.target()"]),
        ("reexport_star_consumer", {"pkg/__init__.py": "from .m import target\n"},
         ["from pkg import *", "target()"]),
        ("reexport_star_consumer_all", {"pkg/__init__.py": 'from .m import target\n\n__all__ = ["target"]\n'},
         ["from pkg import *", "target()"]),
        ("reexport_getattr", {"pkg/__init__.py": "from .m import target\n"},
         ["import pkg", 'getattr(pkg, "target")()']),
        ("reexport_from_user_relative", {"pkg/__init__.py": "from .m import target\n", "pkg/user.py": "from . import target\n\ntarget()\n"},
         ["import pkg.user"]),
        ("reexport_import_module", {"pkg/__init__.py": "from .m import target\n"},
         ["import importlib", 'importlib.import_module("pkg").target()']),
        ("reexport_as_same", {"pkg/__init__.py": "from .m import target as target\n"},
         ["import pkg", "pkg.target()"]),
        ("reexport_module_attr_chain", {"pkg/__init__.py": "from . import m\n"},
         ["import pkg", "pkg.m.target()"]),
        ("reexport_from_abs_self", {"pkg/__init__.py": "from pkg.m import target\n"},
         ["import pkg", "pkg.target()"]),
        ("reexport_in_try", {"pkg/__init__.py": "try:\n    from .m import target\nexcept ImportError:\n    target = None\n"},
         ["import pkg", "pkg.target()"]),
        ("reexport_in_if", {"pkg/__init__.py": "import sys\n\nif sys.version_info >= (3,):\n    from .m import target\n"},
         ["from pkg import target", "target()"]),
        ("reexport_assign_alias", {"pkg/__init__.py": "from .m import target\n\nrun = target\n"},
         ["from pkg import run", "run()"]),
        ("module_assign_alias", {"pkg/user.py": "from pkg.m import target\n\nrun = target\n"},
         ["from pkg.user import run", "run()"]),
        ("user_imports_main_pkg_attr", {"pkg/__init__.py": "from .m import target\n", "pkg/user.py": "import pkg\n\npkg.target()\n"},
         ["import pkg.user"]),
        ("namespace_pkg", {"pkg/__init__.py": None, "ns/m.py": M, "pkg/m.py": M},
         ["import ns.m", "ns.m.target()", "import pkg.m"]),
        ("namespace_from", {"ns/m.py": M}, ["from ns import m", "m.target()", "import pkg.m"]),
        ("dunder_main_pkg", {"pkg/__main__.py": "from .m import target\n\ntarget()\n"},
         ["import pkg.__main__"]),
    ]
    for name, extra, main in cases:
        files = project(main, {k: v for k, v in extra.items() if v is not None})
        for k, v in extra.items():
            if v is None:
                files.pop(k, None)
        yield "pkg:" + name, files, [("pkg.m", "target")] if not name.startswith("namespace") else [("ns.m", "target")]


_all_cases_v2 = all_cases


def all_cases():
    yield from _all_cases_v2()
    yield from package_cases()
