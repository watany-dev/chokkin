try:
    import threading
except ImportError:
    import dummy_threading as threading

try:
    from http.server import SimpleHTTPRequestHandler
except (ImportError, AttributeError):
    from SimpleHTTPServer import SimpleHTTPRequestHandler

try:
    import tomllib
except ModuleNotFoundError:
    import tomlfallback as tomllib

try:
    import zoneinfo
except ImportError:
    import zonefallback as zoneinfo

try:
    import fcntl
except ImportError:
    import lockfallback as fcntl

try:
    import json
    import optionalpkg
except ImportError:
    import mixedfallback as optionalpkg

try:
    import json as fast_json
    fast_json.loads("{}")
except ImportError:
    import callfallback as fast_json

try:
    import queue
except ValueError:
    import valuefallback as queue


def main() -> None:
    print(threading, SimpleHTTPRequestHandler, tomllib, zoneinfo, fcntl)
    print(json, optionalpkg, fast_json, queue)
