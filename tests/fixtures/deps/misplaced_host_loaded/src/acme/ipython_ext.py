import rich
from IPython.core.magic import Magics


def load_ipython_extension(ipython):
    ipython.register_magics(Magics)
    rich.print("acme loaded")
