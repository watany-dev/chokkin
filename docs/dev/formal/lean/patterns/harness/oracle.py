"""Client for the Lean `oracle` executable (one JSON query per line)."""
import json
import pathlib
import subprocess

HERE = pathlib.Path(__file__).resolve().parent
EXE = HERE.parent / ".lake/build/bin/oracle"


class Oracle:
    def __init__(self):
        self.p = subprocess.Popen([str(EXE)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)

    def ask(self, **q):
        self.p.stdin.write(json.dumps(q) + "\n")
        self.p.stdin.flush()
        return json.loads(self.p.stdout.readline())

    def fnmatch(self, pat, name):
        return self.ask(op="fnmatch", pat=pat, name=name)

    def gitignore(self, ignores, paths):
        return self.ask(op="gitignore", ignores=ignores, paths=paths)

    def pyglob(self, pat, files, dirs):
        return self.ask(op="pyglob", pat=pat, files=files, dirs=dirs)

    def tox(self, line):
        return self.ask(op="tox", line=line)

    def find(self, dirs, inits, where, include, exclude, namespaces):
        return self.ask(op="find", dirs=dirs, inits=inits, where=where, include=include,
                        exclude=exclude, namespaces=namespaces)

    def pytest_file(self, patterns, path):
        return self.ask(op="pytest_file", patterns=patterns, path=path)
