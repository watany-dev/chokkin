from importlib.util import find_spec

numpy_installed = find_spec("numpy") and find_spec("rich")

if numpy_installed:
    import numpy
import rich
