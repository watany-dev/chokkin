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


def main() -> None:
    print(json, fast_json, tomllib)
