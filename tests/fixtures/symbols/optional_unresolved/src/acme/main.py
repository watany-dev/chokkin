from contextlib import suppress

try:
    import optionalpkg
except ImportError:
    import fallbackpkg as optionalpkg

with suppress(ImportError):
    import suppressedpkg

import mixedpkg

try:
    import mixedpkg.sub
except ImportError:
    pass


def main() -> None:
    optionalpkg.run(suppressedpkg, mixedpkg)


try:
    import threading
except ImportError:
    import dummy_threading
