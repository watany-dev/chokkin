import importlib
from typing import cast

from acme import m


def main(v: object, y: "m.AnnotParam" = None) -> "m.AnnotRet":
    cast("m.CastTarget", v)
    importlib.import_module("acme.m").via_import_module()
    mod = importlib.import_module("acme.m")
    mod.via_bound()
