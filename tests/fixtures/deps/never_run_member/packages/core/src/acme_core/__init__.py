import sys

# The member allows 3.8, where the `else` runs: the root's range does not rule
# it out.
if sys.version_info >= (3, 9):
    from zoneinfo import ZoneInfo
else:
    from backports.zoneinfo import ZoneInfo
