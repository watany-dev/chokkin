import sys

# `json` ships with every Python `>=3.9` allows: these handlers never run (#1027).
try:
    import json
except ImportError:
    import simplejson as json

try:
    import json as fast_json
except ImportError:
    import ujson as fast_json

# `tomllib` is 3.11+: the fallback runs on 3.9 and 3.10.
try:
    import tomllib
except ImportError:
    import tomli as tomllib

# Every Python `>=3.9` allows takes the first branch (#997).
if sys.version_info >= (3, 9):
    from zoneinfo import ZoneInfo
else:
    from backports.zoneinfo import ZoneInfo

# 3.9 takes the `else`.
if sys.version_info[:2] >= (3, 10):
    from importlib import metadata
else:
    import importlib_metadata as metadata

# `.micro` is not read, so the `else` may run.
if sys.version_info.micro >= 0:
    text = str
else:
    import six

    text = six.text_type


def main() -> None:
    print(json, fast_json, tomllib, ZoneInfo, metadata, text)
