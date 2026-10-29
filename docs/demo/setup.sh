#!/usr/bin/env sh
# Build the small FastAPI project the README demo GIF runs chokkin on.
# Usage: docs/demo/setup.sh <dir>   (the directory must not exist yet)
#
# The project is generated rather than checked in so chokkin's own baseline
# run over this repository never sees its deliberately broken manifest.
set -eu

dir=${1:?usage: setup.sh <dir>}
mkdir "$dir"
cd "$dir"
mkdir todo tests

cat > pyproject.toml <<'TOML'
[project]
name = "todo-api"
version = "0.1.0"
requires-python = ">=3.12"
dependencies = [
  "fastapi",
  "requests",
  "pandas",
]

[dependency-groups]
dev = ["pytest"]
TOML

cat > app.py <<'PY'
from fastapi import FastAPI

from todo.store import add_todo, list_todos

app = FastAPI()


@app.get("/todos")
def get_todos():
    return list_todos()


@app.post("/todos")
def post_todo(title: str):
    return add_todo(title)
PY

: > todo/__init__.py

cat > todo/store.py <<'PY'
import pytest
import yaml

_TODOS: list[dict] = []


def add_todo(title: str) -> dict:
    todo = {"id": len(_TODOS) + 1, "title": title}
    _TODOS.append(todo)
    return todo


def list_todos() -> list[dict]:
    return _TODOS


def export_yaml() -> str:
    return yaml.safe_dump(_TODOS)
PY

cat > todo/legacy_csv.py <<'PY'
import csv


def write_csv(path: str, rows: list[dict]) -> None:
    with open(path, "w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=["id", "title"])
        writer.writeheader()
        writer.writerows(rows)
PY

cat > tests/test_store.py <<'PY'
from todo.store import add_todo, list_todos


def test_add_todo():
    add_todo("write docs")
    assert list_todos()[-1]["title"] == "write docs"
PY
