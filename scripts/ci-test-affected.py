#!/usr/bin/env python3
"""Run complete test targets for changed crates and their workspace dependents.

Orbit's validation runner sets two environment variables [ORB-15131]. When
ORBIT_VALIDATION_SUMMARY names a file, the run writes a JSON summary there:
the selection it ran and how many tests nextest executed. When
ORBIT_VALIDATION_SELECTION holds such a selection (from a candidate run), the
run executes exactly those packages instead of selecting from a diff, so a
rerun on the base commit tests what the candidate tested. A selection that
also names `failing_tests` (each a nextest `<binary-id> <test>` or a bare test
name, as a failed candidate run printed them) runs only those tests, building
only the packages their binary ids name and skipping doctests [ORB-15328]; the
summary still reports the selection it was handed.

The summary also records what an implementer's deferred-sandbox handoff
needs, so Orbit derives that record instead of the implementer writing it
[ORB-15516]: the gate's own command (ORBIT_VALIDATION_GATE, which the make
target sets), the base it compared against, its exit status once it finishes,
and every skip or deferral notice its tests printed under nextest.

It records where that time went. compile_ms and test_ms are millisecond
totals, and package_timings lists each package's share. A total equals the
sum of the per-package values for that phase, except under the cargo test
fallback. Compile time is Cargo's unit durations (they overlap when the build
is parallel). Test time is nextest testcase time plus doctest time (testcase
time overlaps when tests are parallel). A phase that was not measured is null.
The cargo test fallback has no per-package test split: its libtest time has no
package, so those package test_ms values stay null while test_ms is that
libtest time plus the doctest time.
"""

import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import signal
import subprocess
import sys
import tempfile
import threading
import xml.etree.ElementTree


ROOT = Path(__file__).resolve().parent.parent
SUMMARY_ENV = "ORBIT_VALIDATION_SUMMARY"
SELECTION_ENV = "ORBIT_VALIDATION_SELECTION"
GATE_ENV = "ORBIT_VALIDATION_GATE"
FAILING_TESTS = "failing_tests"
# An optional nextest binary id (`crate`, `crate::target`, `crate::bin/name`),
# then a Rust test path: nothing a filterset or libtest filter must escape.
TEST_ID = re.compile(r"(?:([A-Za-z0-9_-]+)(?:::[A-Za-z0-9_./-]+)? )?[A-Za-z0-9_]+(?:::[A-Za-z0-9_]+)*")


def git(*arguments):
    return subprocess.check_output(
        ["git", "-C", str(ROOT), *arguments],
        env=dict(os.environ, GIT_OPTIONAL_LOCKS="0"),
    )


def comparison_base(explicit):
    if explicit:
        return git("rev-parse", "--verify", f"{explicit}^{{commit}}").decode().strip()
    for branch in ("refs/remotes/origin/agent-main", "refs/heads/agent-main"):
        exists = subprocess.run(
            ["git", "-C", str(ROOT), "rev-parse", "--verify", "--quiet", branch],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        if exists.returncode == 0:
            return git("merge-base", "HEAD", branch).decode().strip()
    raise ValueError("cannot resolve agent-main; set CI_TEST_BASE to the delivery base commit")


def changed_paths(base):
    # A commit-to-worktree diff includes committed, staged and unstaged changes.
    # Disabling rename detection includes both the old and new crate of a move.
    tracked = git("diff", "--name-only", "--no-renames", "-z", base, "--")
    untracked = git("ls-files", "--others", "--exclude-standard", "-z")
    return {os.fsdecode(path) for path in (tracked + untracked).split(b"\0") if path}


# Files that a crate's tests read at run time from outside its own directory:
# another crate's assets, or a repository-root file. Cargo metadata cannot see
# these edges, so each is declared here as (path prefix, reading crates). A
# changed path under a prefix selects the readers and their reverse dependents.
FILE_READERS = (
    (Path("scripts/build-budget.py"), ("orbit-engine",)),
    (Path("crates/orbit-core/assets/jobs"), ("orbit-engine", "orbit-cli")),
    (Path("crates/orbit-core/assets/activities"), ("orbit-engine",)),
    (Path("crates/orbit-core/assets/auto_tasks"), ("orbit-cli",)),
    (Path("plugin/hooks"), ("orbit-cli",)),
    (Path("server.json"), ("orbit-cli",)),
)

# ci-macos.yml names test paths and filters that a moved or deleted test leaves
# stale. Only check-ci-macos.sh reads them, so the gate runs it whenever a
# crate test file changes or the workflow itself does.
MACOS_WORKFLOW = ".github/workflows/ci-macos.yml"
MACOS_CHECK = ROOT / "scripts/check-ci-macos.sh"


def touches_macos_workflow(paths):
    """Whether a changed path is the macOS workflow or a file under a tests directory of a crate."""
    for raw_path in paths:
        parts = Path(raw_path).parts
        if raw_path == MACOS_WORKFLOW:
            return True
        if len(parts) >= 3 and parts[0] == "crates" and "tests" in parts[1:-1]:
            return True
    return False


def affected_packages(metadata, paths):
    members = set(metadata["workspace_members"])
    packages = {package["name"]: package for package in metadata["packages"]
                if package["id"] in members}
    if not packages:
        raise ValueError("cargo metadata reported no workspace packages")
    roots = {name: Path(package["manifest_path"]).resolve().parent.relative_to(ROOT)
             for name, package in packages.items()}
    by_root = {ROOT / path: name for name, path in roots.items()}
    selected = set()
    shared_files = {"Cargo.toml", "Cargo.lock", "rust-toolchain", "rust-toolchain.toml"}
    for raw_path in paths:
        path = Path(raw_path)
        if raw_path in shared_files or path.parts[0] in {".cargo", ".config"}:
            return sorted(packages)
        owners = {name for name, root in roots.items() if path.is_relative_to(root)}
        if not owners and path.parts[0] == "crates":
            # A removed member no longer appears in current Cargo metadata.
            return sorted(packages)
        selected.update(owners)
        for prefix, readers in FILE_READERS:
            if path.is_relative_to(prefix):
                missing = [name for name in readers if name not in packages]
                if missing:
                    raise ValueError(f"FILE_READERS names unknown workspace crates: {', '.join(missing)}")
                selected.update(readers)

    reverse = {name: set() for name in packages}
    for name, package in packages.items():
        for dependency in package["dependencies"]:
            # --no-deps metadata retains dev/build/optional/target-specific edges,
            # including renamed dependencies. Match workspace paths, not registry
            # names, so a registry package with the same name is not an edge.
            dependency_path = dependency.get("path")
            owner = by_root.get(Path(dependency_path).resolve()) if dependency_path else None
            if owner is not None:
                reverse[owner].add(name)
    pending = list(selected)
    while pending:
        for dependent in reverse[pending.pop()] - selected:
            selected.add(dependent)
            pending.append(dependent)
    return sorted(selected)


def given_selection(metadata, text):
    """The packages a candidate's summary selection names, all of them current
    members, and the failing tests it names, if any."""
    selection = json.loads(text)
    packages = selection.get("packages") if isinstance(selection, dict) else None
    if not isinstance(packages, list) or not all(isinstance(name, str) for name in packages):
        raise ValueError(f"{SELECTION_ENV} must be a JSON object with a list of package names")
    members = set(metadata["workspace_members"])
    known = {package["name"] for package in metadata["packages"] if package["id"] in members}
    missing = sorted(set(packages) - known)
    if missing:
        raise ValueError(f"{SELECTION_ENV} names packages this checkout lacks: {', '.join(missing)}")
    tests = selection.get(FAILING_TESTS)
    if tests is not None:
        if (not isinstance(tests, list) or not tests
                or not all(isinstance(test, str) and TEST_ID.fullmatch(test) for test in tests)):
            raise ValueError(f"{SELECTION_ENV} {FAILING_TESTS} must be a non-empty list of test names")
        outside = sorted({match.group(1) for match in map(TEST_ID.fullmatch, tests)
                          if match.group(1)} - set(packages))
        if outside:
            raise ValueError(f"{SELECTION_ENV} {FAILING_TESTS} name packages outside the selection: "
                             f"{', '.join(outside)}")
    return sorted(set(packages)), tests


def failing_test_runs(packages, tests):
    """The packages to build and the filters to run only `tests`: a nextest
    filterset, and libtest names for the cargo test fallback."""
    matches = [TEST_ID.fullmatch(test) for test in tests]
    # A bare name could be in any selected package; only binary ids narrow the build.
    owners = {match.group(1) for match in matches}
    run_packages = packages if None in owners else sorted(owners)
    filterset = " | ".join(
        f"(binary_id(={test.split(' ', 1)[0]}) & test(={test.split(' ', 1)[1]}))" if " " in test
        else f"test(={test})" for test in tests)
    names = sorted({test.split(" ", 1)[-1] for test in tests})
    return run_packages, filterset, names


def executed_tests(junit):
    """How many tests nextest's JUnit report says ran, or None without a report."""
    try:
        tests = xml.etree.ElementTree.parse(junit).getroot().get("tests")
    except (OSError, xml.etree.ElementTree.ParseError):
        return None
    return int(tests) if tests is not None and tests.isdigit() else None


def executed_test_identities(junit):
    """Normalized binary/test identities that actually ran, never skipped cases.

    A missing or malformed report provides no proof. Do not derive identities
    from the requested filter: a filter can match only part of that request.
    """
    try:
        root = xml.etree.ElementTree.parse(junit).getroot()
    except (OSError, xml.etree.ElementTree.ParseError):
        return None
    identities = set()
    for suite in root.iter("testsuite"):
        binary = suite.get("name") or ""
        for case in suite.findall("testcase"):
            if case.find("skipped") is not None:
                continue
            identity = f"{binary} {case.get('name') or ''}"
            if not TEST_ID.fullmatch(identity):
                return None
            identities.add(identity)
    return sorted(identities)


def milliseconds(seconds):
    """Round a non-negative duration to milliseconds, or None."""
    try:
        value = float(seconds)
    except (TypeError, ValueError):
        return None
    if value < 0:
        return None
    return int(round(value * 1000))


def text_lines(text):
    return ANSI_ESCAPE.sub("", text).replace("\r", "\n").splitlines()


def timing_report_paths(text):
    """HTML reports this cargo invocation said it wrote.

    The file name and parent directory are Cargo's, so a test's captured
    output cannot point the parser at an arbitrary path.
    """
    paths = []
    for line in text_lines(text):
        marker = "Timing report saved to "
        if marker not in line:
            continue
        candidate = Path(line.split(marker, 1)[1].strip())
        if (candidate.name.startswith("cargo-timing") and candidate.suffix == ".html"
                and candidate.parent.name == "cargo-timings" and candidate.is_file()):
            paths.append(candidate)
    return paths


def unit_milliseconds(path):
    """Package name to compile milliseconds from one Cargo timings report.

    None when the report is not readable. An empty dict is a report that
    named no units. Durations of the same package are summed.
    """
    try:
        if path.stat().st_size > 32 * 1024 * 1024:
            return None
        text = path.read_text(errors="replace")
    except OSError:
        return None
    marker = "const UNIT_DATA = "
    start = text.find(marker)
    if start < 0:
        return None
    try:
        units, _ = json.JSONDecoder().raw_decode(text[start + len(marker):])
    except json.JSONDecodeError:
        return None
    if not isinstance(units, list):
        return None
    totals = {}
    for unit in units:
        if not isinstance(unit, dict):
            continue
        name = unit.get("name")
        duration = unit.get("duration")
        if not isinstance(name, str) or not name or isinstance(duration, bool):
            continue
        ms = milliseconds(duration)
        if ms is None:
            continue
        totals[name] = totals.get(name, 0) + ms
    return totals


FINISHED_SECONDS = re.compile(r"^\s*Finished\b.*\bin\s+([0-9]*\.?[0-9]+)s\s*$")
COMPILING_PACKAGE = re.compile(r"^\s*Compiling (\S+) v\d")
DOCTEST_TARGET = re.compile(r"^\s*Doc-tests (\S+)\s*$")
TEST_RESULT_SECONDS = re.compile(r"test result:.*\bfinished in ([0-9]*\.?[0-9]+)s\s*$")


def finished_milliseconds(text):
    total = 0
    saw = False
    for line in text_lines(text):
        match = FINISHED_SECONDS.match(line)
        if match:
            ms = milliseconds(match.group(1))
            if ms is not None:
                total += ms
                saw = True
    return total if saw else None


def mentioned_packages(text, doctest_packages):
    names = []
    for line in text_lines(text):
        match = COMPILING_PACKAGE.match(line)
        name = match.group(1) if match else None
        match = DOCTEST_TARGET.match(line)
        if match:
            name = doctest_packages.get(match.group(1))
        if name and name not in names:
            names.append(name)
    return names


def suite_milliseconds(suite):
    cases = list(suite.findall("testcase"))
    timed = [milliseconds(case.get("time")) for case in cases]
    if cases and any(ms is not None for ms in timed):
        return sum(ms for ms in timed if ms is not None)
    return milliseconds(suite.get("time"))


def package_test_milliseconds(junit):
    """Per-package nextest time from a JUnit report, or None when it has none."""
    try:
        root = xml.etree.ElementTree.parse(junit).getroot()
    except (OSError, xml.etree.ElementTree.ParseError):
        return None
    totals = {}
    saw = False
    for suite in root.iter("testsuite"):
        name = (suite.get("name") or "").split("::", 1)[0]
        ms = suite_milliseconds(suite)
        if not name or ms is None:
            continue
        totals[name] = totals.get(name, 0) + ms
        saw = True
    return totals if saw else None


def doctest_milliseconds(text, doctest_packages):
    """Per-package doctest time, resolving Cargo's library target labels."""
    totals = {}
    current = None
    for line in text_lines(text):
        match = DOCTEST_TARGET.match(line)
        if match:
            current = doctest_packages.get(match.group(1))
            continue
        if current is None:
            continue
        match = TEST_RESULT_SECONDS.search(line)
        if match:
            ms = milliseconds(match.group(1))
            if ms is not None:
                totals[current] = totals.get(current, 0) + ms
            current = None
    return totals or None


def libtest_milliseconds(text):
    """Sum of libtest finished-in times, excluding doctest blocks."""
    total = 0
    saw = False
    in_doctest = False
    for line in text_lines(text):
        if DOCTEST_TARGET.match(line):
            in_doctest = True
            continue
        if in_doctest:
            if TEST_RESULT_SECONDS.search(line):
                in_doctest = False
            continue
        match = TEST_RESULT_SECONDS.search(line)
        if match:
            ms = milliseconds(match.group(1))
            if ms is not None:
                total += ms
                saw = True
    return total if saw else None


class PhaseTimings:
    """Compile and test durations that sum to their per-package parts.

    Cargo's `--timings` report lists every unit. Those durations overlap
    under a parallel build, so compile_ms is their sum, not the build's wall
    time. Test durations overlap the same way. The two totals are defined as
    those sums, which is what a later reader can check, except for libtest
    time under the cargo test fallback, which has no package to credit.
    """

    def __init__(self, packages, metadata):
        self.packages = list(packages)
        # Cargo prints the library target name, which may be normalized or
        # explicitly named. Only unique targets among selected packages can
        # be attributed from that label alone.
        owners = {}
        for package in metadata["packages"]:
            if package["name"] in self.packages:
                for target in package["targets"]:
                    if target["doctest"]:
                        owners.setdefault(target["name"], set()).add(package["name"])
        self.doctest_packages = {target: next(iter(names)) for target, names in owners.items()
                                 if len(names) == 1}
        self.compile = None
        self.test = None
        self.unsplit_compile = 0
        self.unsplit_compile_seen = False
        self.unsplit_test = 0
        self.unsplit_test_seen = False

    def add_compile_units(self, totals):
        if self.compile is None:
            self.compile = {name: 0 for name in self.packages}
        for name, value in totals.items():
            self.compile[name] = self.compile.get(name, 0) + value

    def add_tests(self, totals):
        if not totals:
            return
        if self.test is None:
            self.test = {name: 0 for name in self.packages}
        for name, value in totals.items():
            self.test[name] = self.test.get(name, 0) + value

    def add_unsplit_compile(self, ms, names):
        if ms is None:
            return
        targets = list(names)
        if not targets and len(self.packages) == 1:
            targets = list(self.packages)
        if self.compile is not None:
            if not targets:
                targets = sorted(self.packages)[:1] or sorted(self.compile)[:1]
            if not targets:
                return
            base, extra = divmod(ms, len(targets))
            for index, name in enumerate(sorted(targets)):
                self.compile[name] = self.compile.get(name, 0) + base + (1 if index < extra else 0)
            return
        self.unsplit_compile += ms
        self.unsplit_compile_seen = True

    def observe(self, text, junit=None):
        units = None
        for path in timing_report_paths(text):
            parsed = unit_milliseconds(path)
            if parsed is None:
                continue
            if units is None:
                units = {}
            for name, value in parsed.items():
                units[name] = units.get(name, 0) + value
        if units is not None:
            self.add_compile_units(units)
        else:
            self.add_unsplit_compile(finished_milliseconds(text), mentioned_packages(text, self.doctest_packages))
        if junit is not None:
            self.add_tests(package_test_milliseconds(junit))
        self.add_tests(doctest_milliseconds(text, self.doctest_packages))
        if self.test is None:
            ms = libtest_milliseconds(text)
            if ms is not None:
                self.unsplit_test += ms
                self.unsplit_test_seen = True

    def record(self):
        """`(compile_ms, test_ms, package_timings)`, or None when nothing was measured."""
        if (self.compile is None and self.test is None
                and not self.unsplit_compile_seen and not self.unsplit_test_seen):
            return None
        names = set(self.packages)
        if self.compile is not None:
            names.update(self.compile)
        if self.test is not None:
            names.update(self.test)
        ordered = sorted(names)
        compile_split = self.compile is not None
        if not compile_split and self.unsplit_compile_seen and len(ordered) == 1:
            self.compile = {ordered[0]: self.unsplit_compile}
            compile_split = True
        # libtest time from the cargo test fallback has no package to credit, so
        # the per-package test split is withheld rather than shown as zero.
        test_split = self.test is not None and not self.unsplit_test_seen
        entries = [dict(
            package=name,
            compile_ms=self.compile.get(name, 0) if compile_split else None,
            test_ms=self.test.get(name, 0) if test_split else None,
        ) for name in ordered]
        compile_ms = (sum(entry["compile_ms"] for entry in entries) if compile_split
                      else (self.unsplit_compile if self.unsplit_compile_seen else None))
        if test_split:
            test_ms = sum(entry["test_ms"] for entry in entries)
        elif self.unsplit_test_seen:
            # Doctests arrive per package even under the fallback, so they add to the unsplit libtest time.
            doctest_ms = sum(self.test.values()) if self.test is not None else 0
            test_ms = self.unsplit_test + doctest_ms
        else:
            test_ms = None
        return compile_ms, test_ms, entries


def write_summary(path, selection, tests_run, base=None, exit_code=None, notices=(), timings=None,
                  executed=None):
    if timings is None:
        compile_ms = test_ms = package_timings = None
    else:
        compile_ms, test_ms, package_timings = timings
    if path:
        Path(path).write_text(json.dumps(dict(
            schema_version=1, selection=selection, tests_run=tests_run,
            executed_tests=executed,
            command=os.environ.get(GATE_ENV), base=base, exit_code=exit_code,
            notices=list(notices),
            compile_ms=compile_ms, test_ms=test_ms, package_timings=package_timings)))


# Keep every notice the required-command host judge refuses, including ones
# from a passing test. Ordinary passing-test output stays in the full log.
HOST_NOTICE_PREFIXES = ("SKIP:", "DEFERRED:", "skipping:", "skipping ")
HOST_NOTICE_MARKERS = ("sandbox_apply: Operation not permitted",
                       "sandbox-exec cannot apply a profile",
                       "No permissions to create a new namespace",
                       "No permissions to create new namespace")
ANSI_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
NEXTEST_STATUS = re.compile(r"^( *)([A-Z][A-Z0-9 +/]*?)\s+\[")
# The existing task artifact inline ceiling, also enforced independently by
# the generic executor. An oversized source is reported as a partial prefix.
COMMAND_EVIDENCE_BYTES = 1024 * 1024


def report_command_evidence(log_path):
    """Report failed raw runner bytes through the host's generic evidence channel.

    Standalone gates retain their scratch log as before. The executor supplies
    a fresh directory and invocation; no runner pathname is interpreted by core.
    Copy at most the inline artifact ceiling, marking an oversized source's
    prefix incomplete. The host independently enforces read/retention ceilings
    and redaction; a producer report never claims durable retention.
    """
    directory = os.environ.get("ORBIT_COMMAND_EVIDENCE_DIR")
    invocation = os.environ.get("ORBIT_COMMAND_EVIDENCE_INVOCATION")
    if not directory or not invocation:
        return
    try:
        target = Path(directory) / log_path.name
        digest = hashlib.sha256()
        size = 0
        with log_path.open("rb") as source, target.open("xb") as output:
            while size < COMMAND_EVIDENCE_BYTES:
                block = source.read(min(64 * 1024, COMMAND_EVIDENCE_BYTES - size))
                if not block:
                    break
                digest.update(block)
                size += len(block)
                output.write(block)
            complete = not source.read(1)
        manifest = dict(schema_version=1, invocation=invocation, files=[dict(
            name=target.name, source=log_path.relative_to(ROOT).as_posix(),
            bytes=size, sha256=digest.hexdigest(), complete=complete)])
        (Path(directory) / "manifest.json").write_text(json.dumps(manifest))
    except (OSError, ValueError) as error:
        # Evidence failure never masks or repairs the runner's nonzero status.
        print(f"ci-test-affected: command evidence report unavailable: {error}",
              file=sys.stderr, flush=True)


def run_nextest(command, environment):
    """Retain the complete merged stream, relaying diagnostics and failed blocks.

    Nextest prints a status header before each captured test-output block.
    Passing blocks are suppressed except for host-evidence notices; progress
    headers and the summary delimit them and are always relayed. Returns the
    exit status and every line that leads with a host-notice prefix.
    """
    notices = []
    captured = []
    scratch = ROOT / ".orbit/tmp"
    scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(prefix="ci-test-affected-nextest-", suffix=".log",
                                     dir=scratch, delete=False) as log:
        print(f"ci-test-affected: full nextest output: {log.name}", file=sys.stderr, flush=True)
        passing_block = False
        with subprocess.Popen(command, cwd=ROOT, env=environment, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT) as process:
            try:
                for raw in process.stdout:
                    log.write(raw)
                    line = raw.decode("utf-8", errors="replace")
                    plain = ANSI_ESCAPE.sub("", line)
                    captured.append(plain)
                    status = NEXTEST_STATUS.match(plain)
                    # Nextest right-aligns the status in a 12-column field.
                    # Captured payload has its own indentation: a test printing
                    # nested runner output must not open a failing block here.
                    if status and status.end(2) != 12:
                        status = None
                    if status:
                        passing_block = status.group(2).endswith("PASS")
                    elif plain.startswith("────────") or plain.startswith("     Summary "):
                        passing_block = False
                    if plain.lstrip().startswith(HOST_NOTICE_PREFIXES):
                        notices.append(plain.strip())
                    notice = (plain.lstrip().startswith(HOST_NOTICE_PREFIXES)
                              or any(marker in plain for marker in HOST_NOTICE_MARKERS))
                    if status or not passing_block or notice:
                        print(plain if notice else line, end="", file=sys.stderr, flush=True)
                exit_code = process.wait()
                log.flush()
                if exit_code:
                    report_command_evidence(Path(log.name))
                return exit_code, notices, "".join(captured)
            except BaseException:
                process.terminate()
                process.wait()
                raise


def run_observed(command, environment):
    """Run a command, relay both streams, and return its exit code and merged output.

    Doctest compile lines and the test harness result are on different streams.
    Both are needed to attribute doctest time to a package. Each pipe has its
    own reader. A blocking line read waits only for that pipe, so a partial
    line cannot leave the other pipe unread while the child fills it. Lines
    are appended whole, so one stream's bytes never split a timing line.

    A relay failure wakes the supervisor before it joins either reader. The
    command owns a session so cleanup can stop descendants holding the pipes,
    even after the command exits. SIGTERM also enters this cleanup path when
    the surrounding executor stops the gate gracefully.
    """
    process = subprocess.Popen(command, cwd=ROOT, env=environment, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, start_new_session=True)
    previous_sigterm = None
    started = []
    try:
        if threading.current_thread() is threading.main_thread():
            def interrupted(signum, frame):
                raise SystemExit(128 + signum)

            previous_sigterm = signal.signal(signal.SIGTERM, interrupted)
        lines = []
        errors = []
        failed = threading.Event()
        lock = threading.Lock()

        def drain(pipe, dest):
            try:
                while True:
                    raw = pipe.readline()
                    if raw == b"":
                        return
                    # Publish the line before relaying it. The relay write can
                    # block on the consumer; holding the lock across it would
                    # stop the other pipe from being drained.
                    with lock:
                        lines.append(raw)
                    dest.write(raw)
                    dest.flush()
            except BaseException as caught:
                with lock:
                    errors.append(caught)
                failed.set()

        threads = (
            threading.Thread(target=drain, args=(process.stdout, sys.stdout.buffer)),
            threading.Thread(target=drain, args=(process.stderr, sys.stderr.buffer)),
        )
        for thread in threads:
            thread.start()
            started.append(thread)
        while any(thread.is_alive() for thread in started):
            if failed.wait(0.05):
                break
        if errors:
            raise errors[0]
        return process.wait(), b"".join(lines).decode("utf-8", errors="replace")
    except BaseException:
        # Cleanup must not replace the original relay error. Always kill
        # the group after the grace, including when its leader has exited:
        # a descendant may ignore SIGTERM and still hold a reader's pipe.
        if previous_sigterm is not None:
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
        with contextlib.suppress(OSError):
            os.killpg(process.pid, signal.SIGTERM)
        with contextlib.suppress(OSError, subprocess.TimeoutExpired):
            process.wait(timeout=0.1)
        with contextlib.suppress(OSError):
            os.killpg(process.pid, signal.SIGKILL)
        with contextlib.suppress(OSError):
            process.kill()
        with contextlib.suppress(OSError):
            process.wait()
        for thread in started:
            thread.join()
        raise
    finally:
        if previous_sigterm is not None:
            signal.signal(signal.SIGTERM, previous_sigterm)
        for pipe in (process.stdout, process.stderr):
            with contextlib.suppress(OSError):
                pipe.close()


def build_budget_command():
    return shlex.split(os.environ.get("BUILD_BUDGET", str(ROOT / "scripts/build-budget.py")))


def test_process_environment():
    environment = dict(os.environ)
    # A test that runs this script must not report into the outer run's summary.
    environment.pop(SUMMARY_ENV, None)
    environment.pop(SELECTION_ENV, None)
    # A claimed executor exports the worker-binding marker so the Orbit commands
    # its agent runs refuse to start without the binding recorded for them. A
    # test process has no binding: inherited, the marker makes every in-process
    # fixture that opens a runtime fail closed. Tests of the refusal set it.
    environment.pop("ORBIT_WORKER_CONTEXT_REQUIRED", None)
    temporary_root = Path(tempfile.gettempdir()).resolve()
    if temporary_root.is_relative_to(ROOT) and temporary_root != ROOT:
        # Non-Git fixtures must not discover the enclosing managed checkout.
        # Keep any caller-supplied boundaries and the sandbox permissions.
        environment["GIT_CEILING_DIRECTORIES"] = os.pathsep.join(filter(None, (
            environment.get("GIT_CEILING_DIRECTORIES"), str(temporary_root))))
    return environment


def run_macos_workflow_check():
    """Run check-ci-macos.sh under build admission; it lists the tests ci-macos.yml names and runs none."""
    print(f"ci-test-affected: a test file or {MACOS_WORKFLOW} changed; running scripts/check-ci-macos.sh",
          file=sys.stderr, flush=True)
    status = subprocess.run([*build_budget_command(), "--", str(MACOS_CHECK)], cwd=ROOT,
                            env=test_process_environment()).returncode
    if status:
        print(f"ci-test-affected: fix the stale path or filter in {MACOS_WORKFLOW} named above",
              file=sys.stderr, flush=True)
    return status


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--base", default=os.environ.get("CI_TEST_BASE"),
                        help="exact delivery base revision (default: merge base with origin/agent-main or agent-main)")
    parser.add_argument("--list", action="store_true",
                        help="list selected package names without compiling or running tests")
    arguments = parser.parse_args()
    cargo = shlex.split(os.environ.get("CARGO", "cargo"))
    given = os.environ.get(SELECTION_ENV)
    summary = None if arguments.list else os.environ.get(SUMMARY_ENV)
    base = None if given else comparison_base(arguments.base)
    metadata = json.loads(subprocess.check_output(
        [*cargo, "metadata", "--format-version", "1", "--no-deps", "--manifest-path", str(ROOT / "Cargo.toml")],
        cwd=ROOT,
    ))
    failing_tests = None
    changed = None
    if given:
        packages, failing_tests = given_selection(metadata, given)
        print(f"ci-test-affected: running the selection {SELECTION_ENV} names", file=sys.stderr, flush=True)
    else:
        changed = changed_paths(base)
        packages = affected_packages(metadata, changed)
        print(f"ci-test-affected: base={base}", file=sys.stderr, flush=True)
    # Cargo rejects --lib for a bin-only selection, even with --bins/--tests.
    library_kinds = {"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"}

    def target_flags_for(names):
        has_library = any(library_kinds.intersection(target["kind"])
                          for package in metadata["packages"] if package["name"] in names
                          for target in package["targets"])
        return (["--lib"] if has_library else []) + ["--bins", "--tests"] if names else []

    target_flags = target_flags_for(packages)
    # nextest does not execute doctests; they run through Cargo for the same set.
    # A bin-only selection has no doctest targets and Cargo rejects --doc for it.
    doc_packages = sorted(package["name"] for package in metadata["packages"]
                          if package["name"] in packages
                          and any(target["doctest"] for target in package["targets"]))
    selection = dict(packages=packages, target_flags=target_flags, doctest_packages=doc_packages)
    filters, libtest_filters = [], []
    run_packages, run_target_flags = packages, target_flags
    if failing_tests is not None:
        # Reported as handed, so the base run is shown to have replayed them.
        selection[FAILING_TESTS] = failing_tests
        run_packages, filterset, names = failing_test_runs(packages, failing_tests)
        run_target_flags = target_flags_for(run_packages)
        filters, libtest_filters = ["-E", filterset, "--test-threads", "1", "--retries", "0"], ["--", "--exact", *names, "--test-threads", "1"]
        doc_packages = []
    # Checked before the empty-selection return, so a workflow-only change still runs it.
    # A replayed selection carries no diff, so the candidate run that named it decided.
    if changed is not None and not arguments.list and touches_macos_workflow(changed):
        macos_status = run_macos_workflow_check()
        if macos_status:
            return macos_status
    if not packages:
        print("ci-test-affected: no affected workspace crates; no Rust tests to run", file=sys.stderr)
        write_summary(summary, selection, 0, base, 0, timings=(0, 0, []))
        return 0
    if arguments.list:
        print("\n".join(packages))
        return 0

    if failing_tests is None:
        print(f"ci-test-affected: running full test targets for {', '.join(packages)}", file=sys.stderr, flush=True)
    else:
        print(f"ci-test-affected: replaying {len(failing_tests)} failing test(s) in {', '.join(run_packages)}",
              file=sys.stderr, flush=True)
    package_flags = [argument for name in run_packages for argument in ("-p", name)]
    test_environment = test_process_environment()
    budget = build_budget_command()
    nextest = subprocess.run([*cargo, "nextest", "--version"], cwd=ROOT,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    # One timings object across the test command and the doctest command.
    # `--timings` is a report of the build those commands already do.
    timings = PhaseTimings(run_packages, metadata)
    with tempfile.TemporaryDirectory(prefix="ci-test-affected-") as report:
        junit = Path(report) / "junit.xml"
        if nextest.returncode == 0:
            report_flags = []
            if summary:
                # The executed-test count comes from nextest's JUnit report. A
                # tool config file adds it under the repository's own profile.
                tool_config = Path(report) / "nextest.toml"
                tool_config.write_text(f"[profile.default.junit]\npath = {json.dumps(str(junit))}\n")
                report_flags = ["--tool-config-file", f"ci-test-affected:{tool_config}"]
            # nextest hides a passing test's stdout and stderr, and with it the
            # `DEFERRED:` notice of a test that returned without running its
            # sandboxed path [ORB-15161]. The host judge classifies the gate's
            # notice output, so those lines from passing tests must reach it.
            command = [*cargo, "nextest", "run", "--timings", "--no-fail-fast", "--success-output", "immediate",
                       *report_flags, *package_flags, *run_target_flags, *filters]
        else:
            # cargo test reports no executed-test count, so the summary records none.
            print("ci-test-affected: cargo-nextest unavailable; falling back to cargo test", file=sys.stderr)
            command = [*cargo, "test", "--timings", "--no-fail-fast", *package_flags, *run_target_flags, *libtest_filters]
        # A run stopped before nextest finishes still reports what it selected,
        # so its failures so far can be replayed alone [ORB-15328].
        write_summary(summary, selection, None, base)
        notices = []
        if nextest.returncode == 0:
            exit_code, notices, output = run_nextest([*budget, "--", *command], test_environment)
            timings.observe(output, junit if summary else None)
        else:
            exit_code, output = run_observed([*budget, "--", *command], test_environment)
            timings.observe(output)
        tests_run = executed_tests(junit)
        executed = executed_test_identities(junit)

    def finished(code):
        write_summary(summary, selection, tests_run, base, code, notices, timings.record(), executed)
        return code

    if exit_code or not doc_packages:
        return finished(exit_code)
    # Doctests still run, so the summary does not yet report an exit status.
    write_summary(summary, selection, tests_run, base, None, notices, timings.record(), executed)
    doc_flags = [argument for name in doc_packages for argument in ("-p", name)]
    doc_code, doc_output = run_observed(
        [*budget, "--", *cargo, "test", "--timings", "--no-fail-fast", *doc_flags, "--doc"],
        test_environment)
    timings.observe(doc_output)
    return finished(doc_code)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"ci-test-affected: {error}", file=sys.stderr)
        sys.exit(1)
