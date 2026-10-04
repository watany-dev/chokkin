"""Bring up loopback inside a fresh network namespace, then exec the command.

Used as `unshare -rn python3 scripts/oss_netns_exec.py CMD...` so test suites
that start local servers (httpx, uvicorn, requests' httpbin) still work while
the analyzed project has no outside network. `ip` is not assumed to exist.
"""

import fcntl
import os
import socket
import struct
import sys

SIOCGIFFLAGS = 0x8913
SIOCSIFFLAGS = 0x8914
IFF_UP = 0x1


def main() -> None:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        req = struct.pack("16sH14x", b"lo", 0)
        flags = struct.unpack("16sH14x", fcntl.ioctl(s, SIOCGIFFLAGS, req))[1]
        fcntl.ioctl(s, SIOCSIFFLAGS, struct.pack("16sH14x", b"lo", flags | IFF_UP))
    os.execvp(sys.argv[1], sys.argv[1:])


if __name__ == "__main__":
    main()
