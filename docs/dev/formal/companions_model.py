#!/usr/bin/env python3
"""Finite model of runtime peers (§10 CHK002): the declared distributions a
used library, or a used module, loads without the project importing them,
and which of them an unread Django backend setting lowers (#1240, #1002).

Ported from:

- ``src/rules/deps/companions.rs`` (``used_companions``,
  ``companion_is_used``, ``uses_module``)
- ``src/rules/gaps.rs``            (``setting_may_load``: the peers a gap of
  kind ``UnreadBackendSetting`` lowers)
- ``scripts/generate-package-map.py`` (``load_runtime_peers``: seed entries
  merged by normalized peer name)

The table is the real one: ``data/package-map.seed.json`` ``runtime_peers``,
read the way the generator reads it, so a seed edit that breaks a property
fails here before it reaches ``src/resolver/bundled/runtime_peers.rs``.

The universe: the table's peers, the stub names of its providers and two
unrelated names as the declarations × every set of at most three used
providers × every set of at most two used modules, drawn from table modules of each
backend kind, a ``module.Attr`` below one, a parent package and a sibling
whose name only starts with a table module; C4 checks every table module and
those shapes of it alone.

Properties:

C1  ``used_companions`` is the least set closed under the peer rule: a
    declared peer is used when one of its providers is used or found, or
    when a used module is one of its modules or below one; a stub
    (``types-x``, ``x-stubs``) when ``x`` is.  Nothing outside it is found.
C2  The result does not depend on the order of the declarations.
C3  Monotonic: more used providers or modules never drop a companion.
C4  A table module counts only for itself and the names below it at a dot:
    its parent package and a sibling sharing its prefix use no peer.
C5  An unread backend setting lowers exactly the peers a value of that
    setting could make used: ``setting_may_load(s, p)`` iff ``p`` is in the
    closure (C1, with every peer declared) of all modules the entries that
    list ``s`` name.  A deeper provider chain or a module shared with an
    entry that does not list ``s`` would break it.

Tie to the implementation: ``companions.rs``
``companions_are_the_least_closed_set`` (proptest) compares
``used_companions`` with the closure over the bundled table, and ``gaps.rs``
``backend_settings_lower_the_closure_of_their_modules`` checks C5 on it.

Run:  python3 docs/dev/formal/companions_model.py
Exit status 1 when a property is violated (counterexamples are printed).
"""

from __future__ import annotations

import itertools
import json
import re
import sys
from pathlib import Path

SEED = Path(__file__).resolve().parents[3] / "data" / "package-map.seed.json"


def normalize(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()


def load_table() -> dict[str, dict[str, frozenset[str]]]:
    """``load_runtime_peers``: entries merged by normalized peer name."""
    peers: dict[str, dict[str, set[str]]] = {}
    for entry in json.loads(SEED.read_text(encoding="utf-8"))["runtime_peers"]:
        merged = peers.setdefault(
            normalize(entry["peer"]),
            {"providers": set(), "modules": set(), "settings": set()},
        )
        merged["providers"].update(normalize(p) for p in entry.get("providers", []))
        merged["modules"].update(entry.get("modules", []))
        merged["settings"].update(entry.get("settings", []))
    return {
        peer: {key: frozenset(values) for key, values in fields.items()}
        for peer, fields in peers.items()
    }


TABLE = load_table()


# --- port of companions.rs / gaps.rs -----------------------------------------


def uses_module(modules: frozenset[str], module: str) -> bool:
    return any(u == module or u.startswith(module + ".") for u in modules)


def companion_is_used(name: str, used, modules: frozenset[str]) -> bool:
    runtime = None
    if name.startswith("types-"):
        runtime = name[len("types-") :]
    elif name.endswith("-stubs"):
        runtime = name[: -len("-stubs")]
    if runtime is not None and used(normalize(runtime)):
        return True
    entry = TABLE.get(name)
    return entry is not None and (
        any(used(p) for p in entry["providers"])
        or any(uses_module(modules, m) for m in entry["modules"])
    )


def used_companions(declared, used, modules: frozenset[str]) -> list[str]:
    found: list[str] = []
    while True:
        more = [
            name
            for name in declared
            if name not in found
            and companion_is_used(name, lambda p: used(p) or p in found, modules)
        ]
        if not more:
            return found
        found.extend(more)


def setting_may_load(setting: str, peer: str) -> bool:
    def selects(name: str) -> bool:
        entry = TABLE.get(name)
        return entry is not None and setting in entry["settings"]

    entry = TABLE.get(peer)
    return selects(peer) or (
        entry is not None and any(selects(p) for p in entry["providers"])
    )


# --- the reference: a least fixed point written from the rule ----------------


def peer_rule(name: str, live: frozenset[str], modules) -> bool:
    """One step of the rule: a stub of a live distribution, or a peer of a
    live provider or of a used module (itself or a name below it)."""
    for prefix, suffix in (("types-", ""), ("", "-stubs")):
        if name.startswith(prefix) and name.endswith(suffix) and (prefix or suffix):
            base = name[len(prefix) : len(name) - len(suffix)]
            if normalize(base) in live:
                return True
    entry = TABLE.get(name)
    if entry is None:
        return False
    if entry["providers"] & live:
        return True
    return any(
        u == m or (u.startswith(m) and u[len(m)] == ".")
        for m in entry["modules"]
        for u in modules
    )


def closure(declared: frozenset[str], used: frozenset[str], modules) -> frozenset[str]:
    """Kleene iteration of the peer rule from the empty set."""
    found: frozenset[str] = frozenset()
    while True:
        live = used | found
        step = frozenset(name for name in declared if peer_rule(name, live, modules))
        if step == found:
            return found
        found = step


# --- the universe ------------------------------------------------------------

PROVIDERS = sorted({p for e in TABLE.values() for p in e["providers"]})
TABLE_MODULES = sorted({m for e in TABLE.values() for m in e["modules"]})


def module_pool() -> list[str]:
    pool = set(TABLE_MODULES)
    for module in TABLE_MODULES:
        pool.add(module + ".Attr")
        pool.add(module.rsplit(".", 1)[0])
        pool.add(module + "x")
    return sorted(pool)


DECLARED = tuple(
    sorted(set(TABLE) | {f"types-{p}" for p in PROVIDERS} | {f"{p}-stubs" for p in PROVIDERS})
) + ("rich", "types-rich")


def subsets(items, most):
    for size in range(most + 1):
        yield from itertools.combinations(items, size)


def main() -> int:
    errors: list[str] = []
    # Every table module is checked alone by C4; the combined cases take one
    # backend of each kind and the boundary shapes C4 names.
    picks = [
        "django.db.backends.postgresql",
        "django.db.backends.mysql",
        "django.core.cache.backends.redis",
        "lxml.cssselect",
        "storages.backends.s3boto3.S3Boto3Storage",
        "django.contrib.auth.hashers.Argon2PasswordHasher",
        "django.db.backends.postgresqlx",
        "django.db.backends",
        "lxml",
        "storages.backends.s3boto3x",
    ]
    assert all(m in module_pool() for m in picks if m != "storages.backends.s3boto3.S3Boto3Storage")
    cases = 0
    declared = frozenset(DECLARED)
    for used_set in subsets(PROVIDERS, 3):
        used = frozenset(used_set)
        for module_set in subsets(picks, 2):
            modules = frozenset(module_set)
            cases += 1
            got = used_companions(DECLARED, used.__contains__, modules)
            want = closure(declared, used, modules)
            # C1
            if frozenset(got) != want or len(got) != len(set(got)):
                errors.append(f"C1: used={sorted(used)} modules={sorted(modules)}: {got} != {sorted(want)}")
                continue
            # C2
            backwards = used_companions(DECLARED[::-1], used.__contains__, modules)
            if frozenset(backwards) != want:
                errors.append(f"C2: order changes the result for used={sorted(used)}")
            # C3
            for extra in ("redis", "transformers"):
                bigger = used_companions(DECLARED, (used | {extra}).__contains__, modules)
                if not want <= frozenset(bigger):
                    errors.append(f"C3: adding {extra} drops {sorted(want - set(bigger))}")
    # C4
    for module in TABLE_MODULES:
        peers = {p for p, e in TABLE.items() if module in e["modules"]}
        for probe, uses in (
            (module, True),
            (module + ".Attr", True),
            (module + "x", False),
            (module.rsplit(".", 1)[0], False),
        ):
            found = set(used_companions(DECLARED, lambda _: False, frozenset({probe})))
            expected = set()
            for peer, entry in TABLE.items():
                if any(probe == m or probe.startswith(m + ".") for m in entry["modules"]):
                    expected.add(peer)
            if uses and not peers <= found:
                errors.append(f"C4: {probe} does not use {sorted(peers - found)}")
            if not uses and (found & peers) - expected:
                errors.append(f"C4: {probe} uses {sorted((found & peers) - expected)}")
    # C5
    settings = sorted({s for e in TABLE.values() for s in e["settings"]})
    for setting in settings:
        selected = frozenset(
            m for e in TABLE.values() if setting in e["settings"] for m in e["modules"]
        )
        reach = closure(frozenset(TABLE), frozenset(), selected)
        errors.extend(
            f"C5: {setting} lowers {peer}={setting_may_load(setting, peer)}, "
            f"closure has it={peer in reach}"
            for peer in TABLE
            if setting_may_load(setting, peer) != (peer in reach)
        )
    for error in errors[:20]:
        print(error)
    if errors:
        print(f"{len(errors)} counterexample(s) in {cases} cases")
        return 1
    print(f"C1-C5 hold over {cases} cases and {len(settings)} backend settings")
    return 0


if __name__ == "__main__":
    sys.exit(main())
