#!/usr/bin/env python3
"""Finite model of the CHK008 decision (§10 unlisted binary dependency),
with the providers v0.7.11 added (#1241, #1240, #1239).

Ported from:

- ``src/rules/deps/binary.rs``  (``BinaryProviders::new`` / ``provides``,
  ``detect_unlisted_binaries``)
- ``src/plugins/config_scan.rs`` (``PreCommitHook::flush``: which hooks
  pre-commit installs into an env of its own, ``push_isolable`` /
  ``push_hook_command``)
- ``src/plugins/pytest.rs``      (a pytest usage is ``inferred`` when only
  pytest's default file names match: no pytest config, no ``conftest.py``)

What a usage is provided by:

- a distribution the root or a member declares, a requirements file outside
  the fixed names, tox ``deps`` / nox ``session.install`` or a command's
  ``pip install`` (``config_declared_distributions``, #1241), plus the lock
  edges and ``WRAPPER_PROVIDERS`` from those;
- the project's own distribution, root or member, compared after name
  normalization (black formatting itself, #1241);
- a hook pre-commit installs into its own env (a remote hook, or a local
  hook whose ``language`` is not one of the project-env languages and that
  has ``additional_dependencies``), for the config-section usages of its
  binary (#680, #1241);
- for the ``pre-commit`` usage ``.pre-commit-config.yaml`` itself implies,
  ``prek`` running anywhere (#1241).

The universe: one tool distribution ``tool`` with two binaries (``tool`` and
``tool-x``) × how it is provided (nothing, declared, ``pip install``ed by a
command, a lock edge from a declared distribution, a declared wrapper, the
project's own name spelled as written or in another spelling) × which usages exist (a command, a command of
the second binary, a tool config section, a pytest-style inferred usage) × a
pre-commit hook for ``tool`` (none, or remote / local × language unset /
``system`` / ``python`` × ``additional_dependencies``) × the ``prek`` /
``pre-commit`` usages (prek runs or not, the root config file, a
``pre-commit`` command elsewhere).  Only the root ``.pre-commit-config.yaml``
is read (``config_scan.rs``), so no other place is modelled.

Properties:

B1  A usage whose distribution is provided never yields CHK008.
B2  Inferred usages alone never yield CHK008.
B3  A hook in pre-commit's own env (remote, or local with an own-env
    language and ``additional_dependencies``) never yields CHK008 for its
    binary on its own; a local hook in the project's env, with nothing else
    providing ``tool``, yields CHK008 / certain.
B4  ``prek`` running suppresses only the ``pre-commit`` usage of the root
    ``.pre-commit-config.yaml``: a ``pre-commit`` command still yields
    CHK008, and so does the config file without ``prek``.
B5  At most one CHK008 per distribution. It is certain iff some unprovided,
    non-inferred usage of the distribution is a command (not a config
    section), and then the reported origin is a command's; otherwise likely
    (#1260).
B6  Monotonic in providers: declaring more (the tool, its wrapper, the
    project's own name) never adds a finding or raises a confidence.

Tie to the implementation: ``binary.rs`` ``decisions_follow_the_formal_model``
builds every combination of these inputs as real ``BinaryUsage`` hints and
manifests, runs ``detect_unlisted_binaries`` and compares binary, origin and
confidence with ``detect`` below transcribed to Rust.  Change both together.

Run:  python3 docs/dev/formal/binary_providers_model.py
Exit status 1 when a property is violated (counterexamples are printed).
"""

from __future__ import annotations

import itertools
import re
import sys
from dataclasses import dataclass

PRE_COMMIT_CONFIG = ".pre-commit-config.yaml"
# config_scan.rs PRE_COMMIT_PROJECT_ENV_LANGUAGES
PROJECT_ENV_LANGUAGES = ("system", "script", "unsupported", "unsupported_script")
# binary.rs WRAPPER_PROVIDERS, the entries the universe uses.
WRAPPERS = {"tool-wrapper": "tool", "prek": "pre-commit"}
# resolver binary_resolutions
RESOLVES = {
    "tool": "tool",
    "tool-x": "tool",
    "pre-commit": "pre-commit",
    "prek": "prek",
}


def normalize(name: str) -> str:
    """PEP 503 normalization (manifest ``normalize_distribution_name``)."""
    return re.sub(r"[-_.]+", "-", name).lower()


@dataclass(frozen=True)
class Usage:
    binary: str
    file: str
    config_section: bool
    inferred: bool = False


@dataclass(frozen=True)
class Project:
    declared: frozenset[str]
    lock_edges: tuple[tuple[str, str], ...]
    own_names: tuple[str, ...]
    hook_binaries: frozenset[str]
    usages: tuple[Usage, ...]


# --- port of config_scan.rs PreCommitHook::flush -----------------------------


def hook_usages(local: bool, language: str | None, additional: bool):
    """The usages and provided binaries of one hook for ``tool``: its id's
    binary, and for a local hook also its ``entry`` command."""
    isolated = not local or (
        additional and language is not None and language not in PROJECT_ENV_LANGUAGES
    )
    usages = [Usage("tool", PRE_COMMIT_CONFIG, config_section=isolated)]
    if local:
        usages.append(Usage("tool", PRE_COMMIT_CONFIG, config_section=isolated))
    provided = {"tool"} if isolated else set()
    return usages, provided, isolated


def pytest_usage(has_config: bool, has_conftest: bool) -> Usage:
    """pytest.rs: default ``test_*.py`` names alone infer the usage."""
    return Usage("tool", "tests", False, inferred=not has_config and not has_conftest)


# --- port of binary.rs -------------------------------------------------------


def provider_set(project: Project) -> set[str]:
    """``BinaryProviders::new``'s distributions, own names apart."""
    found = set(project.declared)
    transitive = {b for a, b in project.lock_edges if a in found}
    transitive |= {WRAPPERS[w] for w in WRAPPERS if w in found}
    return found | transitive


def provides(project: Project, providers: set[str], usage: Usage, dist: str) -> bool:
    prek_runs_hooks = any(u.binary == "prek" for u in project.usages)
    return (
        dist in providers
        or any(normalize(own) == dist for own in project.own_names)
        or (usage.config_section and usage.binary in project.hook_binaries)
        or (
            prek_runs_hooks
            and usage.binary == "pre-commit"
            and usage.file == PRE_COMMIT_CONFIG
        )
    )


def detect(project: Project) -> list[tuple[str, Usage, str]]:
    """``detect_unlisted_binaries``: (binary, reported usage, confidence)."""
    providers = provider_set(project)
    unprovided = [
        (u, RESOLVES[u.binary])
        for u in project.usages
        if not u.inferred
        and u.binary in RESOLVES
        and not provides(project, providers, u, RESOLVES[u.binary])
    ]
    run_by_command = {d for u, d in unprovided if not u.config_section}
    reported: set[str] = set()
    found = []
    for usage, dist in unprovided:
        run = dist in run_by_command
        if run and usage.config_section:
            continue
        if dist in reported:
            continue
        reported.add(dist)
        found.append((usage.binary, usage, "certain" if run else "likely"))
    return found


# --- the universe ------------------------------------------------------------

PROVIDERS = (
    "none",
    "declared",
    "pip_install",
    "lock_edge",
    "wrapper",
    "own_exact",
    "own_spelled",
)
LANGUAGES = (None, "system", "python")


def tool_provider(kind: str):
    declared, edges, own = set(), (), ()
    if kind in ("declared", "pip_install"):
        # A command's `pip install` reaches the same set as a declaration.
        declared = {"tool"}
    elif kind == "lock_edge":
        declared, edges = {"framework"}, (("framework", "tool"),)
    elif kind == "wrapper":
        declared = {"tool-wrapper"}
    elif kind == "own_exact":
        own = ("tool",)
    elif kind == "own_spelled":
        own = ("Tool",)
    return declared, edges, own


def hooks():
    yield None
    yield from itertools.product((False, True), LANGUAGES, (False, True))


@dataclass(frozen=True)
class Case:
    provider: str
    command: bool
    command_x: bool
    section: bool
    pytest: tuple[bool, bool] | None
    hook: tuple[bool, str | None, bool] | None
    prek: bool
    config_file: str | None
    pre_commit_command: bool


def cases():
    for (
        provider,
        command,
        command_x,
        section,
        pytest,
        hook,
        prek,
        config_file,
        pre_commit_command,
    ) in itertools.product(
        PROVIDERS,
        (False, True),
        (False, True),
        (False, True),
        (None, (False, False), (True, False), (False, True)),
        tuple(hooks()),
        (False, True),
        (None, PRE_COMMIT_CONFIG),
        (False, True),
    ):
        yield Case(
            provider,
            command,
            command_x,
            section,
            pytest,
            hook,
            prek,
            config_file,
            pre_commit_command,
        )


def build(case: Case, provider: str | None = None) -> tuple[Project, bool | None]:
    declared, edges, own = tool_provider(provider or case.provider)
    usages: list[Usage] = []
    hook_binaries: set[str] = set()
    isolated = None
    if case.command:
        usages.append(Usage("tool", "Makefile", False))
    if case.command_x:
        usages.append(Usage("tool-x", "noxfile.py", False))
    if case.section:
        usages.append(Usage("tool", "pyproject.toml", True))
    if case.pytest is not None:
        usages.append(pytest_usage(*case.pytest))
    if case.hook is not None:
        hook, provided, isolated = hook_usages(*case.hook)
        usages.extend(hook)
        hook_binaries |= provided
    if case.prek:
        usages.append(Usage("prek", "Makefile", False))
    if case.config_file is not None:
        usages.append(Usage("pre-commit", case.config_file, True))
    if case.pre_commit_command:
        usages.append(Usage("pre-commit", "tox.ini", False))
    project = Project(
        frozenset(declared),
        edges,
        own,
        frozenset(hook_binaries),
        tuple(usages),
    )
    return project, isolated


RANK = {"likely": 1, "certain": 2}


def check(case: Case) -> list[str]:
    project, isolated = build(case)
    found = detect(project)
    by_dist: dict[str, list[tuple[str, Usage, str]]] = {}
    for binary, usage, confidence in found:
        by_dist.setdefault(RESOLVES[binary], []).append((binary, usage, confidence))
    errors = []
    tool_provided = case.provider != "none"

    # B1
    if tool_provided and "tool" in by_dist:
        errors.append("B1: a provided tool yields CHK008")
    # B2
    only_inferred = (
        case.pytest is not None
        and not any(case.pytest)
        and not (case.command or case.command_x or case.section or case.hook)
    )
    if only_inferred and "tool" in by_dist:
        errors.append("B2: inferred usages alone yield CHK008")
    # B3: pre-commit builds a hook its own env for a remote repo, or for a
    # local hook naming a non-project language and real dependencies.
    if case.hook is not None:
        local, language, additional = case.hook
        own_env = not local or (
            language in ("python", "node") and additional
        )
        if isolated != own_env:
            errors.append(f"B3: hook isolated={isolated}, expected {own_env}")
    if (
        case.hook is not None
        and isolated
        and not (case.command or case.command_x or case.section)
        and (case.pytest is None or not any(case.pytest))
        and "tool" in by_dist
    ):
        errors.append("B3: a hook in pre-commit's own env yields CHK008")
    if (
        case.hook is not None
        and not isolated
        and not tool_provided
        and [c for _, _, c in by_dist.get("tool", [])] != ["certain"]
    ):
        errors.append("B3: a project-env local hook does not yield certain CHK008")
    # B4
    pre_commit = by_dist.get("pre-commit", [])
    expected_pre_commit = case.pre_commit_command or (
        case.config_file is not None
        and not (case.prek and case.config_file == PRE_COMMIT_CONFIG)
    )
    if bool(pre_commit) != expected_pre_commit:
        errors.append(f"B4: pre-commit reported={bool(pre_commit)}")
    # B5
    providers = provider_set(project)
    for dist, hits in by_dist.items():
        if len(hits) != 1:
            errors.append(f"B5: {len(hits)} findings for {dist}")
            continue
        _, usage, confidence = hits[0]
        commands = [
            u
            for u in project.usages
            if RESOLVES.get(u.binary) == dist
            and not u.inferred
            and not u.config_section
            and not provides(project, providers, u, dist)
        ]
        if (confidence == "certain") != bool(commands):
            errors.append(f"B5: {dist} is {confidence} with commands={commands}")
        if confidence == "certain" and usage.config_section:
            errors.append(f"B5: {dist} certain but reports a config section")
    # B6
    for more in ("declared", "wrapper", "own_spelled"):
        bigger, _ = build(case, more)
        bigger = Project(
            bigger.declared | project.declared,
            bigger.lock_edges + project.lock_edges,
            bigger.own_names + project.own_names,
            project.hook_binaries,
            project.usages,
        )
        after = {RESOLVES[b]: c for b, _, c in detect(bigger)}
        before = {RESOLVES[b]: c for b, _, c in found}
        for dist, confidence in after.items():
            if dist not in before or RANK[confidence] > RANK[before[dist]]:
                errors.append(f"B6: adding {more} raised {dist} to {confidence}")
    return errors


def main() -> int:
    failures = 0
    total = 0
    for case in cases():
        total += 1
        for error in check(case):
            failures += 1
            if failures <= 20:
                print(f"{error}\n    {case}")
    if failures:
        print(f"{failures} counterexample(s) in {total} cases")
        return 1
    print(f"B1-B6 hold over {total} cases")
    return 0


if __name__ == "__main__":
    sys.exit(main())
