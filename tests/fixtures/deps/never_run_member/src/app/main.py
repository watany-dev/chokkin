import sys

import acme_core

# The root's `>=3.10` rules the `else` out (#997).
if sys.version_info >= (3, 9):
    from zoneinfo import ZoneInfo
else:
    from backports.zoneinfo import ZoneInfo


def main() -> None:
    print(acme_core, ZoneInfo)
