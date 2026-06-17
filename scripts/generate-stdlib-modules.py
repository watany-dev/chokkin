#!/usr/bin/env python3
"""Generate versioned stdlib module lists for chokkin resolver."""

from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT_DIR = ROOT / "src" / "resolver" / "stdlib"

# Curated import roots shared across Python 3.11 (see docs/dev/plans/step-07-import-resolution.md).
BASE_311 = """\
abc
aifc
argparse
array
ast
asyncio
atexit
base64
bdb
binascii
bisect
builtins
bz2
cProfile
calendar
cgi
cmath
cmd
code
codecs
collections
colorsys
compileall
concurrent
configparser
contextlib
contextvars
copy
copyreg
crypt
csv
ctypes
curses
dataclasses
datetime
dbm
decimal
difflib
dis
doctest
email
encodings
enum
errno
faulthandler
fcntl
filecmp
fileinput
fnmatch
fractions
ftplib
functools
gc
getopt
getpass
gettext
glob
graphlib
grp
gzip
hashlib
heapq
hmac
html
http
idlelib
imaplib
imghdr
importlib
inspect
io
ipaddress
itertools
json
keyword
lib2to3
linecache
locale
logging
lzma
mailbox
mailcap
marshal
math
mimetypes
mmap
modulefinder
multiprocessing
netrc
nntplib
numbers
operator
optparse
os
pathlib
pdb
pickle
pickletools
pipes
pkgutil
platform
plistlib
poplib
posix
posixpath
pprint
profile
pstats
pty
pwd
py_compile
pyclbr
pydoc
queue
quopri
random
re
readline
reprlib
resource
runpy
sched
secrets
select
selectors
shelve
shlex
shutil
signal
site
smtplib
socket
socketserver
sqlite3
ssl
stat
statistics
string
stringprep
struct
subprocess
symtable
sys
sysconfig
tabnanny
tarfile
telnetlib
tempfile
termios
test
textwrap
threading
time
timeit
tkinter
token
tokenize
tomllib
trace
traceback
tracemalloc
tty
turtle
types
typing
unittest
urllib
uuid
venv
warnings
wave
weakref
webbrowser
wsgiref
xml
xmlrpc
zipapp
zipfile
zipimport
zlib
zoneinfo
""".splitlines()

ALWAYS_INCLUDE = ["__future__"]

ADDED_IN_311 = ["tomllib"]

REMOVED_IN_313 = [
    "aifc",
    "audioop",
    "cgi",
    "cgitb",
    "chunk",
    "crypt",
    "imghdr",
    "lib2to3",
    "mailcap",
    "msilib",
    "nntplib",
    "pipes",
    "sndhdr",
    "spwd",
    "sunau",
    "telnetlib",
    "uu",
    "xdrlib",
]


def build_set(base: list[str], *, include: list[str], exclude: list[str]) -> list[str]:
    modules = set(base) | set(ALWAYS_INCLUDE) | set(include)
    modules -= set(exclude)
    return sorted(modules)


def write_version(name: str, modules: list[str]) -> None:
    path = OUT_DIR / name
    path.write_text("\n".join(modules) + "\n", encoding="utf-8")
    print(f"wrote {path} ({len(modules)} modules)")


def main() -> None:
    base = [line for line in BASE_311 if line]
    py310 = build_set(base, include=[], exclude=ADDED_IN_311)
    py311 = build_set(base, include=[], exclude=[])
    py312 = py311
    py313 = build_set(py311, include=[], exclude=REMOVED_IN_313)

    write_version("py310.txt", py310)
    write_version("py311.txt", py311)
    write_version("py312.txt", py312)
    write_version("py313.txt", py313)


if __name__ == "__main__":
    main()
