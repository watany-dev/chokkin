from typing import TYPE_CHECKING

import _typeshed
import runtimeonlypkg

if TYPE_CHECKING:
    from _typeshed.wsgi import WSGIEnvironment

    import typeonlypkg


def main(environ: "WSGIEnvironment") -> "typeonlypkg.Thing":
    return runtimeonlypkg.run(environ, _typeshed)
