import importlib

try:
    import polars
except ImportError:
    polars = None


def to_array():
    import xarray

    return xarray


import xarray as xr


def main() -> None:
    import sympy

    return polars, sympy, xr, to_array(), load_toolz()


def solve():
    import sympy

    return sympy


def load_toolz():
    return importlib.import_module("toolz")


def load_heavy():
    from acme import heavy

    return heavy


from acme import shared

from contextlib import suppress

with suppress(ImportError):
    from acme import optional_io
