#!/usr/bin/env python3
"""Remove-and-test oracle over the pinned OSS corpus (#114, #338, #339).

For every CHK001 (unused file) or CHK006 (unused export) finding in a
disposable copy of each cloned project, remove what chokkin flagged, run the
project's test command, and compare against a baseline run on the untouched
tree.

THIS RUNS UNTRUSTED THIRD-PARTY TEST CODE. It is opt-in and never part of the
chokkin CLI/library pipeline, release jobs, or default CI. Without --execute it
is a dry run: chokkin analysis, test-command detection and (for CHK006) symbol
span lookup only; nothing from the analyzed projects is executed. See
docs/dev/chk001-remove-and-test.md for the isolation this needs.

Usage:
  scripts/oss-remove-and-test.py [OPTIONS]

Options:
  -m, --manifest PATH   Clone list (default: scripts/oss-clones.manifest)
  -c, --clones DIR      Clone root (default: target/oss-clones)
  -o, --output DIR      Output directory (default: target/oss-oracle/<rule>)
  -b, --bin PATH        chokkin binary (default: target/release/chokkin)
  --rule CODE           CHK001 (delete the file, default) or CHK006 (remove the
                        symbol, see below)
  --python PATH         Interpreter for test runs when --envs has no venv for a
                        project (default: python3)
  --envs DIR            Per-project venvs from scripts/oss-provision-envs.py
                        (e.g. target/oss-envs). Projects listed in --env-manifest
                        use <DIR>/<slug>/bin/python, that manifest's test
                        command and PYTHONPATH.
  --env-manifest PATH   default: scripts/oss-test-env.manifest
  --offline             Run tests in a new user+network namespace with only
                        loopback up (unshare -rn + scripts/oss_netns_exec.py)
  --wrap CMD            Prefix every test run (applied outside --offline)
  --timeout SECS        Per test run timeout (default: 600)
  --sample N            Per project, test only N findings chosen by a stable
                        hash of their target; the rest are not-run (sampled-out)
  --max-findings N      Per project, test only the first N findings (sorted);
                        the rest are not-run (over-max-findings)
  --jobs N              Parallel working copies per project (default: 1)
  --projects a,b        Only these slugs
  --build               cargo build --release before running
  --execute             Actually run project tests (otherwise dry run)
  -h, --help            Show help

Test command (first match wins; otherwise `no-test-command`):
  the --env-manifest `test` column when --envs is given for that project, else
  pyproject.toml [tool.pytest.ini_options], pytest.ini, setup.cfg [tool:pytest],
  tox.ini [pytest]  ->  <python> -m pytest -q -x
tox/nox are not used: they install dependencies.

CHK006 removal (the source is parsed with Python's `ast`, never executed):
  delete     the name is not used anywhere else in its module: remove the
             top-level def/class/assignment span (decorators included); a
             block left empty gets `pass`
  privatize  the module still uses the name: rename the definition and its
             in-module references to `_chokkin_private_<name>`, so only
             references from outside the module break
  Both test the CHK006 claim "nothing outside the module uses this name".
  Findings whose span cannot be determined are not-run (span-*).

Per-finding status:
  pass           baseline passed and the suite still passes after removal
  break          baseline passed, suite fails after removal, and a baseline
                 re-run passes again (so the failure is attributed to removal)
  baseline-fail  the untouched tree already fails (or times out), or the
                 baseline re-run after a post-removal failure fails (flaky)
  not-run        no-test-command, dry-run, sampled-out, over --max-findings,
                 or no removable span (CHK006)

Outputs (under --output):
  results.tsv     one row per finding
  summary.json    per-project and per-rule counts, corpus revisions, command
  report.md       human-readable summary
  logs/<slug>/    test run output
"""

from __future__ import annotations

import argparse
import ast
import configparser
import hashlib
import io
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import threading
import tokenize
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import oss_corpus as oc

RULES = ("CHK001", "CHK006")
STATUSES = ("pass", "break", "baseline-fail", "not-run")


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(add_help=False)
    p.add_argument("-m", "--manifest", type=Path, default=oc.DEFAULT_MANIFEST)
    p.add_argument("-c", "--clones", type=Path, default=oc.DEFAULT_CLONES)
    p.add_argument("-o", "--output", type=Path, default=None)
    p.add_argument("-b", "--bin", type=Path, default=oc.DEFAULT_BIN)
    p.add_argument("--rule", default="CHK001", choices=RULES)
    p.add_argument("--python", default="python3")
    p.add_argument("--envs", type=Path, default=None)
    p.add_argument("--env-manifest", type=Path, default=oc.DEFAULT_ENV_MANIFEST)
    p.add_argument("--wrap", default="")
    p.add_argument("--offline", action="store_true")
    p.add_argument("--timeout", type=int, default=600)
    p.add_argument("--sample", type=int, default=0)
    p.add_argument("--max-findings", type=int, default=0)
    p.add_argument("--jobs", type=int, default=1)
    p.add_argument("--projects", default="")
    p.add_argument("--build", action="store_true")
    p.add_argument("--execute", action="store_true")
    p.add_argument("-h", "--help", action="store_true")
    args = p.parse_args()
    if args.help:
        print(__doc__)
        sys.exit(0)
    if args.output is None:
        args.output = oc.ROOT / "target/oss-oracle" / args.rule.lower()
    if args.offline:
        netns = shlex.quote(str(oc.ROOT / "scripts/oss_netns_exec.py"))
        args.wrap = f"{args.wrap} unshare -rn python3 {netns}".strip()
    return args


def has_pytest_config(proj: Path) -> bool:
    pyproject = proj / "pyproject.toml"
    if pyproject.is_file() and "[tool.pytest.ini_options]" in pyproject.read_text(
        encoding="utf-8", errors="replace"
    ):
        return True
    if (proj / "pytest.ini").is_file():
        return True
    for name, section in (("setup.cfg", "tool:pytest"), ("tox.ini", "pytest")):
        cfg_path = proj / name
        if not cfg_path.is_file():
            continue
        cfg = configparser.RawConfigParser(strict=False, interpolation=None)
        try:
            cfg.read(cfg_path, encoding="utf-8")
        except configparser.Error:
            continue
        if cfg.has_section(section):
            return True
    return False


# ─── Findings ────────────────────────────────────────────────────────────────


def findings(report: dict, rule: str) -> list[dict]:
    """One entry per finding: target (stable key), path, and for CHK006 the
    file / line / symbol name."""
    out = {}
    for i in report.get("issues", []):
        if i.get("code") != rule:
            continue
        if rule == "CHK001":
            out[i["path"]] = {"target": i["path"], "path": i["path"]}
        else:
            name = (i.get("symbol") or "").rpartition(":")[2]
            out[i["target"]] = {
                "target": i["target"],
                "path": i["file"],
                "line": i["line"],
                "name": name,
            }
    return [out[k] for k in sorted(out)]


def stable_rank(target: str) -> str:
    return hashlib.sha256(target.encode("utf-8")).hexdigest()


# ─── CHK006 symbol removal ───────────────────────────────────────────────────


def _defines(node: ast.stmt, name: str) -> bool:
    if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
        return node.name == name
    if isinstance(node, ast.Assign):
        return len(node.targets) == 1 and isinstance(node.targets[0], ast.Name) and (
            node.targets[0].id == name
        )
    if isinstance(node, ast.AnnAssign):
        return isinstance(node.target, ast.Name) and node.target.id == name
    if sys.version_info >= (3, 12) and isinstance(node, ast.TypeAlias):
        return isinstance(node.name, ast.Name) and node.name.id == name
    return False


def _find_def(body: list[ast.stmt], name: str, line: int):
    """(node, enclosing body) of the module-level definition of `name` at
    `line`, looking into if/try blocks (TYPE_CHECKING, version guards)."""
    for node in body:
        start = min([node.lineno, *(d.lineno for d in getattr(node, "decorator_list", []))])
        if not (start <= line <= (node.end_lineno or node.lineno)):
            continue
        if _defines(node, name):
            return node, body
        for field in ("body", "orelse", "finalbody"):
            found = _find_def(getattr(node, field, []) or [], name, line)
            if found:
                return found
        for handler in getattr(node, "handlers", []) or []:
            found = _find_def(handler.body, name, line)
            if found:
                return found
    return None


def _name_tokens(source: str, name: str) -> list[tuple[int, int]]:
    """(row, col) of NAME tokens equal to `name` that are not attribute
    accesses (`x.name`); rows are 1-based."""
    out = []
    prev = None
    for tok in tokenize.generate_tokens(io.StringIO(source).readline):
        if tok.type == tokenize.NAME and tok.string == name and not (
            prev is not None and prev.type == tokenize.OP and prev.string == "."
        ):
            out.append(tok.start)
        if tok.type not in (tokenize.NL, tokenize.NEWLINE, tokenize.COMMENT,
                            tokenize.INDENT, tokenize.DEDENT):
            prev = tok
    return out


def plan_symbol_removal(source: str, line: int, name: str) -> tuple[str, str]:
    """Return (strategy, new_source); strategy is delete | privatize, or a
    span-* reason with new_source == ""."""
    try:
        tree = ast.parse(source)
    except (SyntaxError, ValueError):
        return "span-unparsable", ""
    found = _find_def(tree.body, name, line)
    if not found:
        return "span-not-found", ""
    node, body = found
    start = min([node.lineno, *(d.lineno for d in getattr(node, "decorator_list", []))])
    end = node.end_lineno or node.lineno
    try:
        refs = _name_tokens(source, name)
    except (tokenize.TokenError, IndentationError, SyntaxError):
        return "span-untokenizable", ""
    lines = source.splitlines(keepends=True)
    # Before 3.12 an f-string is one STRING token, so its uses are only in the AST.
    # ast col_offset is in UTF-8 bytes; skip any position that does not hold the name.
    refs = sorted({*refs, *((n.lineno, n.col_offset) for n in ast.walk(tree)
                            if isinstance(n, ast.Name) and n.id == name)})
    refs = [(r, c) for r, c in refs if lines[r - 1][c:c + len(name)] == name]
    if any(not (start <= r <= end) for r, _ in refs):
        new = f"_chokkin_private_{name}"
        # `import name` / `from m import name` must keep importing `name`.
        imported = {(a.lineno, a.col_offset) for n in ast.walk(tree)
                    if isinstance(n, (ast.Import, ast.ImportFrom))
                    for a in n.names if a.name == name and a.asname is None}
        for r, c in sorted(refs, reverse=True):
            repl = f"{name} as {new}" if (r, c) in imported else new
            lines[r - 1] = lines[r - 1][:c] + repl + lines[r - 1][c + len(name):]
        return "privatize", "".join(lines)
    keep = lines[: start - 1]
    if len(body) == 1:
        # The block would be left empty: keep it syntactically valid.
        indent = re.match(r"[ \t]*", lines[start - 1]).group(0)
        keep.append(f"{indent}pass\n")
    return "delete", "".join(keep + lines[end:])


# ─── Measurement ─────────────────────────────────────────────────────────────


class Project:
    def __init__(self, slug: str, args: argparse.Namespace, envs: dict):
        self.slug = slug
        self.args = args
        self.logs = args.output / "logs" / slug
        self.work_root = args.output / "work"
        env = envs.get(slug) if args.envs else None
        venv_python = args.envs / slug / "bin/python" if env else None
        if env and venv_python.exists():
            self.python = str(venv_python)
            self.test_template = env["test"]
            self.pythonpath = [p for p in env["pythonpath"].split(":") if p]
            self.env_source = "envs"
        else:
            self.python = args.python
            self.test_template = oc.DEFAULT_TEST_CMD
            self.pythonpath = []
            self.env_source = "host"

    def work(self, k: int) -> Path:
        return self.work_root / f"{self.slug}.{k}"

    def command(self) -> list[str]:
        tmpl = shlex.split(self.test_template)
        return shlex.split(self.args.wrap) + [
            self.python if t == "{python}" else t for t in tmpl
        ]

    def run(self, work: Path, log_name: str) -> tuple[str, int | None, float]:
        extra = {}
        if self.pythonpath:
            extra["PYTHONPATH"] = os.pathsep.join(str(work / p) for p in self.pythonpath)
        return oc.run_isolated(
            self.command(), work, self.args.timeout, self.logs / log_name, extra
        )


def apply_removal(work: Path, f: dict, rule: str) -> str:
    """Mutate the working copy for finding `f`; return the strategy used."""
    target = work / f["path"]
    if rule == "CHK001":
        target.unlink(missing_ok=True)
        return "delete-file"
    strategy, new = plan_symbol_removal(
        target.read_text(encoding="utf-8"), f["line"], f["name"]
    )
    if new:
        target.write_text(new, encoding="utf-8")
    return strategy


def measure_project(slug: str, args, rows: list[dict], projects: dict, envs: dict) -> None:
    proj = Project(slug, args, envs)
    shutil.rmtree(proj.logs, ignore_errors=True)
    proj.logs.mkdir(parents=True)
    base = proj.work(0)
    oc.fresh_copy(args.clones / slug, base)

    info: dict = {"findings": 0, "test_command": None, "baseline": None,
                  "env": proj.env_source}
    projects[slug] = info
    try:
        found = findings(oc.chokkin_report(args.bin, base), args.rule)
    except (json.JSONDecodeError, KeyError) as err:
        info["error"] = f"chokkin output unreadable: {err}"
        print(f"    {info['error']}", file=sys.stderr)
        return
    info["findings"] = len(found)
    print(f"==> {slug}: {len(found)} {args.rule} finding(s)", flush=True)

    def record(f: dict, status: str, detail: str, strategy: str = "",
               post_exit: int | None = None, secs: float = 0.0) -> None:
        rows.append({"slug": slug, "rule": args.rule, "target": f["target"],
                     "status": status, "strategy": strategy, "detail": detail,
                     "post_exit": post_exit, "seconds": round(secs, 2)})

    if proj.env_source == "envs" or has_pytest_config(base):
        info["test_command"] = shlex.join(proj.command())
    if not found:
        return
    if info["test_command"] is None:
        for f in found:
            record(f, "not-run", "no-test-command")
        return

    selected = found
    if args.sample and len(found) > args.sample:
        keep = {f["target"] for f in sorted(found, key=lambda f: stable_rank(f["target"]))[: args.sample]}
        selected = [f for f in found if f["target"] in keep]
        for f in found:
            if f["target"] not in keep:
                record(f, "not-run", "sampled-out")
    if args.max_findings and len(selected) > args.max_findings:
        for f in selected[args.max_findings:]:
            record(f, "not-run", "over-max-findings")
        selected = selected[: args.max_findings]

    if args.rule == "CHK006":
        # Plan every removal up front (parse only) so unsupported spans are
        # reported even in a dry run.
        runnable = []
        for f in selected:
            strategy, new = plan_symbol_removal(
                (base / f["path"]).read_text(encoding="utf-8"), f["line"], f["name"]
            )
            if new:
                runnable.append(f)
            else:
                record(f, "not-run", strategy, strategy)
        selected = runnable

    if not args.execute:
        for f in selected:
            record(f, "not-run", "dry-run")
        return

    oc.reset_tree(base)
    outcome, code, secs = proj.run(base, "baseline.log")
    info["baseline"] = {"outcome": outcome, "exit": code, "seconds": round(secs, 2)}
    print(f"    baseline: {outcome} (exit {code}, {secs:.1f}s)", flush=True)
    if outcome != "ok":
        detail = "baseline-timeout" if outcome == "timeout" else f"baseline-exit-{code}"
        for f in selected:
            record(f, "baseline-fail", detail)
        return

    jobs = max(1, min(args.jobs, len(selected)))
    for k in range(1, jobs):
        oc.fresh_copy(args.clones / slug, proj.work(k))
    free = list(range(jobs))
    lock = threading.Lock()

    def one(n_f: tuple[int, dict]) -> None:
        n, f = n_f
        with lock:
            k = free.pop()
        work = proj.work(k)
        try:
            oc.reset_tree(work)
            strategy = apply_removal(work, f, args.rule)
            outcome, code, secs = proj.run(work, f"{n:04d}.log")
            if outcome == "ok":
                with lock:
                    record(f, "pass", "", strategy, code, secs)
                return
            # Confirm the failure is caused by the removal, not flakiness.
            oc.reset_tree(work)
            again, _, _ = proj.run(work, f"{n:04d}.rebaseline.log")
            detail = "post-timeout" if outcome == "timeout" else f"post-exit-{code}"
            status = "break" if again == "ok" else "baseline-fail"
            if status == "baseline-fail":
                detail = f"flaky-baseline;{detail}"
            with lock:
                record(f, status, detail, strategy, code, secs)
                print(f"    {status}: {f['target']} ({detail})", flush=True)
        finally:
            with lock:
                free.append(k)

    with ThreadPoolExecutor(max_workers=jobs) as pool:
        list(pool.map(one, enumerate(selected)))


def counts(rows: list[dict]) -> dict[str, int]:
    c = {s: 0 for s in STATUSES}
    for r in rows:
        c[r["status"]] += 1
    c["total"] = len(rows)
    tested = c["pass"] + c["break"]
    c["precision_pct"] = round(100 * c["pass"] / tested, 1) if tested else None
    return c


def write_outputs(args, rows: list[dict], projects: dict, meta: dict) -> None:
    out = args.output
    rows.sort(key=lambda r: (r["slug"], r["target"]))
    rule = args.rule
    with (out / "results.tsv").open("w", encoding="utf-8") as fh:
        fh.write("slug\trule\ttarget\tstatus\tstrategy\tdetail\tpost_exit\tseconds\n")
        for r in rows:
            post = "" if r["post_exit"] is None else r["post_exit"]
            fh.write(f"{r['slug']}\t{r['rule']}\t{r['target']}\t{r['status']}\t"
                     f"{r['strategy']}\t{r['detail']}\t{post}\t{r['seconds']}\n")

    for slug, info in projects.items():
        info["counts"] = counts([r for r in rows if r["slug"] == slug])
    summary = {**meta, "rules": {rule: counts(rows)}, "projects": projects}
    if rule == "CHK006":
        by_strategy: dict[str, dict[str, int]] = {}
        for r in rows:
            if r["strategy"] in ("delete", "privatize"):
                by_strategy.setdefault(r["strategy"], {s: 0 for s in STATUSES})[r["status"]] += 1
        summary["by_strategy"] = by_strategy
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")

    def pct(c: dict) -> str:
        return "n/a" if c["precision_pct"] is None else f"{c['precision_pct']}%"

    t = summary["rules"][rule]
    lines = [
        f"# {rule} remove-and-test oracle",
        "",
        f"- chokkin: `{meta['chokkin_version']}`",
        f"- generated: {meta['generated']}",
        f"- mode: {'execute' if meta['execute'] else 'dry-run'}",
        f"- command: `{meta['command']}`",
        "",
        "Precision = pass / (pass + break) over findings that reached a post-removal run.",
        "",
        *oc.md_table(
            ["Rule", "Total", "pass", "break", "baseline-fail", "not-run", "Precision"],
            [[rule, t["total"], t["pass"], t["break"], t["baseline-fail"], t["not-run"], pct(t)]],
            "lrrrrrr",
        ),
        "",
    ]
    proj_rows = []
    for slug, info in projects.items():
        c = info["counts"]
        b = info["baseline"]
        base_s = "-" if b is None else f"{b['outcome']} ({b['seconds']}s)"
        sha = meta["corpus"].get(slug, {}).get("sha", "?")[:12]
        proj_rows.append([slug, f"`{sha}`", info["env"], base_s, c["total"], c["pass"],
                          c["break"], c["baseline-fail"], c["not-run"], pct(c)])
    lines += oc.md_table(
        ["Project", "SHA", "Env", "Baseline", "Total", "pass", "break",
         "baseline-fail", "not-run", "Precision"],
        proj_rows, "llllrrrrrr",
    )
    if rule == "CHK006":
        lines += ["", "## By strategy", ""]
        lines += oc.md_table(
            ["Strategy", "pass", "break", "baseline-fail", "not-run"],
            [[k, v["pass"], v["break"], v["baseline-fail"], v["not-run"]]
             for k, v in sorted(summary["by_strategy"].items())],
            "lrrrr",
        )
    not_run: dict[str, int] = {}
    for r in rows:
        if r["status"] == "not-run":
            not_run[r["detail"]] = not_run.get(r["detail"], 0) + 1
    lines += ["", "## not-run reasons", ""]
    lines += oc.md_table(["Reason", "Count"], sorted(not_run.items()), "lr") if not_run else ["_None._"]
    breaks = [r for r in rows if r["status"] == "break"]
    lines += ["", "## Breaks", ""]
    if breaks:
        lines += oc.md_table(["Project", "Target", "Strategy", "Detail"],
                             [[r["slug"], f"`{r['target']}`", r["strategy"], r["detail"]]
                              for r in breaks])
    else:
        lines.append("_None._")
    (out / "report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> int:
    args = parse_args()
    if args.build:
        oc.build(args.bin)
    oc.require_bin(args.bin)
    if not args.manifest.is_file():
        print(f"manifest not found: {args.manifest}", file=sys.stderr)
        return 2
    args.output = args.output.resolve()
    if args.envs is not None:
        args.envs = args.envs.resolve()
    if os.sep in args.python:
        # Tests run with cwd inside the working copy.
        args.python = str(Path(args.python).resolve())
    args.output.mkdir(parents=True, exist_ok=True)
    envs = oc.read_env_manifest(args.env_manifest) if args.envs else {}

    only = {s for s in args.projects.split(",") if s}
    rows: list[dict] = []
    projects: dict = {}
    skipped: list[str] = []
    for row in oc.read_manifest(args.manifest, core_only=True):
        slug = row["slug"]
        if only and slug not in only:
            continue
        if not (args.clones / slug).is_dir():
            print(f"skip (not cloned): {slug}", file=sys.stderr)
            skipped.append(slug)
            continue
        try:
            measure_project(slug, args, rows, projects, envs)
        finally:
            shutil.rmtree(args.output / "work", ignore_errors=True)

    if not projects:
        print("no projects measured — run clone-oss-fixtures.sh first", file=sys.stderr)
        return 2

    lock = oc.read_lock(args.clones)
    meta = {
        "rule": args.rule,
        "chokkin_version": oc.chokkin_version(args.bin),
        "generated": oc.utc_now(),
        "execute": args.execute,
        "command": shlex.join([Path(sys.argv[0]).name, *sys.argv[1:]]),
        "timeout_seconds": args.timeout,
        "sample": args.sample,
        "corpus": {s: lock[s] for s in projects if s in lock},
        "skipped_not_cloned": skipped,
    }
    write_outputs(args, rows, projects, meta)
    print((args.output / "report.md").read_text(encoding="utf-8"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
