#!/usr/bin/env python3
"""Finite model of where chokkin finds ``DJANGO_SETTINGS_MODULE`` (§9 Django)
and what it reports when the settings module is not there, with the
environment files v0.7.11 reads (#999, #938, #1239).

Ported from ``src/plugins/django/mod.rs``:

- ``read_settings_module``: the setters in read order (``manage.py``, the
  discovered setters, the root setters discovery left out), then
  pytest-django's ``--ds`` / ini option, and, only for a project Django is
  in, the environment files (``.env``, compose files, a Dockerfile ``ENV``,
  a dev container's JSON);
- ``extract``: a Django *project* is one with a ``manage.py`` or a setter or
  pytest config that sets the variable; anything else depending on Django is
  a reusable app;
- ``missing_settings``: the file a missing-settings gap points to.

The universe: ``manage.py`` (absent, present without the key, a literal, a
template placeholder, a computed value) × the pytest option (absent, a
literal, a placeholder) × an environment file (absent, a literal, a value
with a variable) × whether the manifest depends on Django.  No settings
module exists, so every project ends in the missing-settings gap.

Properties:

D1  An environment file never makes a project: a project is exactly one with
    ``manage.py`` or a value set by a setter or pytest.
D2  The environment files are read only when Django is in the project
    (``manage.py``, a setter value or a Django dependency).
D3  Module references are the dotted literals only, in read order: setters,
    then pytest, then the environment.  A placeholder or a variable never
    becomes one.
D4  A project's missing-settings gap points at the first literal; with none,
    at the first value set in a form not read, setters and pytest before the
    environment; with none, at ``manage.py``.  A reusable app gets no gap.
D5  Adding an environment file never removes a reference the setters or
    pytest gave, and moves a project's gap only onto a literal it sets when
    no setter or pytest literal exists (a module name beats a placeholder
    or a computed value, wherever it is set), or onto a value it sets in a
    form not read when nothing else sets the variable.

Tie to the implementation: ``tests/plugins_extract.rs``
``django_settings_sources_follow_the_formal_model`` writes every combination
as a project, runs the plugin extractor and compares the
``DJANGO_SETTINGS_MODULE`` references and the unread-settings gap with
``decide`` below transcribed to Rust.  Change both together.

Run:  python3 docs/dev/formal/django_settings_model.py
Exit status 1 when a property is violated (counterexamples are printed).
"""

from __future__ import annotations

import itertools
import re
import sys

MANAGE = ("absent", "no_key", "literal", "placeholder", "computed")
PYTEST = ("absent", "literal", "placeholder")
ENV = ("absent", "literal", "variable")

VALUES = {
    "manage.py": "site.settings",
    "pytest.ini": "site.test_settings",
    ".env": "site.env_settings",
}


def dotted(value: str) -> bool:
    return re.fullmatch(r"[A-Za-z_]\w*(\.[A-Za-z_]\w*)*", value) is not None


def setter_values(manage: str, pytest: str):
    """The values setters and pytest set, in read order: (file, value) with
    ``None`` for one set in a form not read (``computed``)."""
    found = []
    if manage == "literal":
        found.append(("manage.py", VALUES["manage.py"]))
    elif manage == "placeholder":
        found.append(("manage.py", "{{ project_name }}.settings"))
    elif manage == "computed":
        found.append(("manage.py", None))
    if pytest == "literal":
        found.append(("pytest.ini", VALUES["pytest.ini"]))
    elif pytest == "placeholder":
        found.append(("pytest.ini", "{{ project_name }}.settings"))
    return found


def env_values(env: str):
    if env == "literal":
        return [(".env", VALUES[".env"])]
    if env == "variable":
        return [(".env", "${SETTINGS}")]
    return []


# --- port of read_settings_module / extract / missing_settings ---------------


def decide(manage: str, pytest: str, env: str, django_dep: bool):
    """(references as (module, file), gap file or None)."""
    has_manage = manage != "absent"
    literals: list[tuple[str, str]] = []
    unread: str | None = None
    env_unread: str | None = None
    for file, value in setter_values(manage, pytest):
        if value is not None and dotted(value):
            literals.append((value, file))
        elif unread is None:
            unread = file
    in_setters = bool(literals) or unread is not None
    if has_manage or in_setters or django_dep:
        for file, value in env_values(env):
            if dotted(value):
                literals.append((value, file))
            elif env_unread is None:
                env_unread = file
    is_project = has_manage or in_setters
    if not is_project:
        return literals, None
    if literals:
        return literals, literals[0][1]
    if unread is not None:
        return literals, unread
    if env_unread is not None:
        return literals, env_unread
    return literals, "manage.py"


# --- properties ----------------------------------------------------------------


def main() -> int:
    errors = []
    cases = 0
    for manage, pytest, env, dep in itertools.product(MANAGE, PYTEST, ENV, (False, True)):
        cases += 1
        refs, gap = decide(manage, pytest, env, dep)
        label = f"manage={manage} pytest={pytest} env={env} django_dep={dep}"
        setters = setter_values(manage, pytest)
        project = manage != "absent" or bool(setters)
        # D1
        if (gap is not None) != project:
            errors.append(f"D1: project={gap is not None}, expected {project}: {label}")
        # D2
        env_read = manage != "absent" or bool(setters) or dep
        if not env_read and any(file == ".env" for _, file in refs):
            errors.append(f"D2: environment read without Django: {label}")
        # D3
        expected = [
            (value, file)
            for file, value in setters + (env_values(env) if env_read else [])
            if value is not None and dotted(value)
        ]
        if refs != expected:
            errors.append(f"D3: refs {refs} != {expected}: {label}")
        if any("{" in module or "$" in module for module, _ in refs):
            errors.append(f"D3: a placeholder became a reference: {label}")
        # D4
        if project:
            ordered = [file for file, value in setters if value is not None and dotted(value)]
            ordered += [file for file, value in env_values(env) if dotted(value)]
            ordered += [file for file, value in setters if value is None or not dotted(value)]
            ordered += [file for file, value in env_values(env) if not dotted(value)]
            want = ordered[0] if ordered else "manage.py"
            if gap != want:
                errors.append(f"D4: gap at {gap}, expected {want}: {label}")
        # D5
        if env != "absent":
            base_refs, base_gap = decide(manage, pytest, "absent", dep)
            if refs[: len(base_refs)] != base_refs:
                errors.append(f"D5: the environment changed setter refs: {label}")
            setter_literal = any(v is not None and dotted(v) for _, v in setters)
            if base_gap != gap and (
                gap != ".env" or env != "literal" or setter_literal
            ) and not (base_gap == "manage.py" and not setters):
                errors.append(f"D5: the environment moved the gap off {base_gap}: {label}")
    for error in errors[:20]:
        print(error)
    if errors:
        print(f"{len(errors)} counterexample(s) in {cases} cases")
        return 1
    print(f"D1-D5 hold over {cases} cases")
    return 0


if __name__ == "__main__":
    sys.exit(main())
