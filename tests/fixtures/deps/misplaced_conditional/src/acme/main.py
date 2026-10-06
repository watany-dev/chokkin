import importlib

try:
    import polars
except ImportError:
    polars = None

try:
    rich = importlib.import_module("rich")
except ImportError:
    rich = None


def to_array():
    import xarray

    return xarray


import xarray as xr


def main() -> None:
    import sympy

    return polars, sympy, xr, to_array()


def solve():
    import sympy

    return sympy


def load_keras():
    return importlib.import_module("keras")
