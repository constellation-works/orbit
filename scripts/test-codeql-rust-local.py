#!/usr/bin/env python3
"""Exercise local CodeQL's subprocess boundary without CodeQL or downloads."""

import fcntl
import functools
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import re
import select
import signal
import sys
import tempfile
import time
import unittest


REPO_ROOT = Path(__file__).resolve().parent.parent
SCRATCH = Path(os.environ.get("ORBIT_SCRATCH_DIR", REPO_ROOT / ".orbit/tmp"))

# The script runs only on Linux, where it relies on util-linux flock(1), GNU
# mv -T and GNU nproc. The cases stub uname to Linux, so another host must also
# supply those commands. There, and only for a command the host lacks, the
# cases use these shims: flock takes a real flock(2) lock on the inherited
# descriptor, as util-linux does, mv -T is one rename(2), and nproc counts the
# processors this process may use. Linux always runs its own tools. A shim
# refuses any other usage, so a script change cannot pass untested.
FLOCK_SHIM = r'''#!/usr/bin/env python3
"""util-linux flock(1) as the script uses it: flock [-s | -x] [-n] FD."""
import fcntl, sys

operation, flags, args = fcntl.LOCK_EX, 0, sys.argv[1:]
while len(args) > 1 and args[0] in ("-s", "-x", "-n"):
    option = args.pop(0)
    if option == "-n":
        flags = fcntl.LOCK_NB
    else:
        operation = fcntl.LOCK_SH if option == "-s" else fcntl.LOCK_EX
if len(args) != 1 or not args[0].isdigit():
    print(f"flock shim: unsupported arguments {sys.argv[1:]}", file=sys.stderr)
    sys.exit(64)
try:
    fcntl.flock(int(args[0]), operation | flags)
except BlockingIOError:
    sys.exit(1)
except OSError as error:
    print(f"flock: {args[0]}: {error.strerror}", file=sys.stderr)
    sys.exit(1)
'''

MV_SHIM = r'''#!/usr/bin/env python3
"""GNU mv -T as the script uses it: mv -T -- SOURCE DEST is one rename(2)."""
import os, sys

args = sys.argv[1:]
if args[:1] != ["-T"]:
    os.execv(REAL_MV, [REAL_MV, *args])
if len(args) != 4 or args[1] != "--":
    print(f"mv shim: unsupported arguments {args}", file=sys.stderr)
    sys.exit(64)
try:
    os.rename(args[2], args[3])
except OSError as error:
    print(f"mv: cannot move '{args[2]}' to '{args[3]}': {error.strerror}", file=sys.stderr)
    sys.exit(1)
'''


NPROC_SHIM = r'''#!/usr/bin/env python3
"""GNU nproc as the script uses it, with no option or OpenMP limit."""
import os, sys

if sys.argv[1:]:
    print(f"nproc shim: unsupported arguments {sys.argv[1:]}", file=sys.stderr)
    sys.exit(64)
print(len(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else os.cpu_count())
'''


def usable_processors():
    """The processors this process may use, as nproc counts them."""
    if hasattr(os, "sched_getaffinity"):
        return len(os.sched_getaffinity(0))
    return os.cpu_count()


def mv_has_no_target_option():
    with tempfile.TemporaryDirectory(prefix="codeql-local-mv-probe.") as directory:
        source = Path(directory) / "source"
        source.write_text("")
        probe = subprocess.run(["mv", "-T", "--", source, Path(directory) / "target"],
                               capture_output=True)
        return probe.returncode != 0


@functools.cache
def linux_tool_shims():
    """The shim each Linux command the script uses needs on this host."""
    if platform.system() == "Linux":
        return {}
    shims = {}
    if shutil.which("flock") is None:
        shims["flock"] = FLOCK_SHIM
    if mv_has_no_target_option():
        real_mv = shutil.which("mv")
        if real_mv is None:
            raise RuntimeError("no mv on PATH")
        shims["mv"] = MV_SHIM.replace("REAL_MV", repr(real_mv))
    if shutil.which("nproc") is None:
        shims["nproc"] = NPROC_SHIM
    return shims

# The codeql stub enumerates .rs files under --source-root minus the effective
# config's paths-ignore globs, as CodeQL's build-mode none source walk does.
STUB = r'''#!/usr/bin/env python3
import json, os, re, sys, time
from pathlib import Path


def ignored_by(config):
    globs, block = [], False
    for line in config.splitlines():
        if line.startswith("paths-ignore:"):
            if globs or line.split(":", 1)[1].strip():
                sys.exit(98)
            block = True
        elif block and re.match(r"\s*-\s", line):
            parts = re.split(r"(\*\*/|\*\*|\*)", line.split("-", 1)[1].strip().strip('"'))
            globs.append(re.compile("".join(
                {"**/": "(?:.*/)?", "**": ".*", "*": "[^/]*"}.get(part, re.escape(part))
                for part in parts) + r"\Z"))
        elif line.strip() and not line.lstrip().startswith("#"):
            block = False
    return globs


name = Path(sys.argv[0]).name
args = sys.argv[1:]
if name == "codeql" and args == ["version", "--format=terse"]:
    print(os.environ.get("STUB_CODEQL_VERSION", "2.27.1"))
    sys.exit(0)
keys = ("RUSTUP_HOME", "CARGO_HOME", "CARGO_TARGET_DIR", "TMPDIR",
        "XDG_CACHE_HOME", "RUSTUP_TOOLCHAIN", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER")
call = dict(name=name, args=args, env={k: os.environ[k] for k in keys})
if args[:2] == ["database", "create"]:
    option = lambda prefix: next(arg[len(prefix):] for arg in args if arg.startswith(prefix))
    root = Path(option("--source-root="))
    globs = ignored_by(Path(option("--codescanning-config=")).read_text())
    call["sources"] = sorted(
        rel for rel in (path.relative_to(root).as_posix() for path in root.rglob("*.rs"))
        if not any(glob.match(rel) for glob in globs))
with open(os.environ["STUB_CALLS"], "a") as log:
    log.write(json.dumps(call) + "\n")
if args[:2] == ["database", "create"] and os.environ.get("STUB_ADMISSION_EVENT"):
    with open(os.environ["STUB_ADMISSION_EVENT"], "w") as pipe:
        pipe.write("create\n")


def event(kind):
    if os.environ.get("STUB_EVENTS"):
        with open(os.environ["STUB_EVENTS"], "a") as log:
            log.write(json.dumps(dict(kind=kind, call=args[:2], at=time.monotonic())) + "\n")


if name == "rustup":
    if os.environ.get("STUB_INSTALL_FAIL"):
        print("error: could not create temp file: Read-only file system (os error 30)", file=sys.stderr)
        sys.exit(30)
    home = Path(os.environ["RUSTUP_HOME"])
    (home / "installed").write_text(args[2])
    toolchain = home / "toolchains" / f"{args[2]}-x86_64-unknown-linux-gnu"
    library = toolchain / "lib/rustlib/src/rust/library"
    (library / "core/src").mkdir(parents=True)
    (library / "core/src/lib.rs").write_text("pub fn core() {}\n")
    vendored = library / "vendor/wasip3"
    (vendored / "src").mkdir(parents=True)
    (vendored / "Cargo.toml").write_text("[package]\n")
    (vendored / "src/lib.rs").write_text("pub fn vendored() {}\n")
    (toolchain / "bin").mkdir()
    (toolchain / "bin/rustc").write_text(f"#!/bin/sh\necho 'rustc {args[2]} (stub 2026-07-07)'\n")
    (toolchain / "bin/rustc").chmod(0o755)
elif args[:2] == ["database", "create"]:
    event("start")
    if os.environ.get("STUB_GATE"):
        deadline = time.monotonic() + 150
        while not Path(os.environ["STUB_GATE"]).exists() and time.monotonic() < deadline:
            time.sleep(0.05)
    database = Path(args[2])
    (database / "log").mkdir(parents=True)
    (database / "db-rust/default").mkdir(parents=True)
    (database / "db-rust/default/pool").write_text("dataset")
    (database / "src.zip").write_text("archive")
    registry = Path(os.environ["CARGO_HOME"]) / "registry/index"
    registry.mkdir(parents=True)
    (registry / "config.json").write_text("{}")
    (Path(os.environ["CARGO_TARGET_DIR"]) / "debug").mkdir(parents=True)
    (Path(os.environ["CARGO_TARGET_DIR"]) / "debug/out").write_text("build")
    if os.environ.get("STUB_EDIT_DURING_CREATE"):
        with open(os.environ["STUB_EDIT_DURING_CREATE"], "a") as edited:
            edited.write("pub fn edited() {}\n")
    log = os.environ.get("STUB_EXTRACTION_LOG", "INFO semantic crate graph extracted")
    if os.environ.get("STUB_LOG_FILE_ONLY"):
        (database / "log/extractor.log").write_text(log)
    else:
        print(log)
    event("end")
    sys.exit(int(os.environ.get("STUB_CREATE_EXIT", "0")))
elif args[:2] == ["database", "analyze"]:
    event("start")
    if os.environ.get("STUB_ANALYZE_FAIL"):
        print("query evaluation: heap exhausted", file=sys.stderr)
        sys.exit(7)
    output = next(arg.split("=", 1)[1] for arg in args if arg.startswith("--output="))
    Path(output).write_text(json.dumps({"version": "2.1.0", "runs": [{"results": []}]}))
    event("end")
else:
    sys.exit(99)
'''

TRACKED = ["Cargo.toml", "crates/core/src/lib.rs", "crates/core/src/test_support.rs",
           "crates/core/tests/it.rs"]
# Residue a checkout accumulates: generated build output, earlier run scratch
# with a prepared toolchain, and ad-hoc measurement files.
RESIDUE = ["target/debug/build/serde-1/out/generated.rs",
           ".orbit/tmp/measure.rs",
           ".orbit/tmp/codeql-rust-local.prior/rustup/toolchains/1.97.0/lib/rustlib/src/rust/library/core/src/lib.rs"]
# Tracked production source plus a new file a candidate has not yet committed;
# the shared configuration still excludes tests.
SELECTED = ["crates/core/src/added.rs", "crates/core/src/lib.rs"]
# What a passing run keeps as evidence; a failed run also keeps the dataset
# and build scratch.
EVIDENCE = ["analysis.log", "codeql-config.yml", "codeql-logs", "database",
            "extraction-problems.log", "extraction.log", "inputs.sha256", "results.sarif",
            "toolchain.log"]
BULK = ["cache", "cargo", "database/db-rust", "target", "tmp"]


class LocalCodeqlTests(unittest.TestCase):
    def setUp(self):
        SCRATCH.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix="codeql-local-test.", dir=SCRATCH)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.repo = self.root / "checkout with spaces"
        scripts = self.repo / "scripts"
        scripts.mkdir(parents=True)
        self.script = scripts / "codeql-rust-local.sh"
        shutil.copy2(REPO_ROOT / "scripts/codeql-rust-local.sh", self.script)
        self.config = self.repo / ".github/codeql/codeql-config.yml"
        self.config.parent.mkdir(parents=True)
        shutil.copy2(REPO_ROOT / ".github/codeql/codeql-config.yml", self.config)
        for relative in [*TRACKED, *RESIDUE, "crates/core/src/added.rs"]:
            (self.repo / relative).parent.mkdir(parents=True, exist_ok=True)
            (self.repo / relative).write_text("pub fn f() {}\n")
        git_env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        for command in (["init", "-q"], ["add", *TRACKED, "scripts", ".github"]):
            subprocess.run(["git", *command], cwd=self.repo, env=git_env, check=True)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name in ("codeql", "rustup"):
            path = self.bin / name
            path.write_text(STUB)
            path.chmod(0o755)
        # The host platform is stubbed too, so every case runs as on Linux
        # unless it names another host.
        uname = self.bin / "uname"
        uname.write_text('#!/bin/sh\n[ "$1" = -s ] || exit 99\necho "${STUB_UNAME:-Linux}"\n')
        uname.chmod(0o755)
        # The commands the script runs, after the stubs above.
        self.tool_path = os.environ["PATH"]
        if linux_tool_shims():
            userland = self.root / "linux userland"
            userland.mkdir()
            for name, source in linux_tool_shims().items():
                (userland / name).write_text(source)
                (userland / name).chmod(0o755)
            self.tool_path = f"{userland}:{self.tool_path}"
        self.calls_file = self.root / "calls.jsonl"
        self.global_dirs = [self.root / "user-rustup", self.root / "user-cargo"]
        for directory in self.global_dirs:
            directory.mkdir()
            (directory / "sentinel").write_text("preserve")
        self.scratch = self.root / "run scratch"
        # The host toolchain cache and slot locks; never the real ~/.orbit/cache.
        self.cache = self.root / "host cache"
        self.env = dict(git_env, PATH=f"{self.bin}:{self.tool_path}",
                        STUB_CALLS=str(self.calls_file),
                        ORBIT_SCRATCH_DIR=str(self.scratch),
                        ORBIT_CODEQL_CACHE_DIR=str(self.cache),
                        RUSTUP_HOME=str(self.global_dirs[0]),
                        CARGO_HOME=str(self.global_dirs[1]))

    # The timeout only guards against a hung script. A stubbed run takes about
    # 2 s, but a busy executor stretched runs past 20 s on a managed macOS leaf
    # (four concurrent cargo builds), which failed make ci-fast on an
    # unchanged base.
    def run_script(self, *args, **env):
        return subprocess.run(["bash", str(self.script), *args], cwd=self.root,
                              env=dict(self.env, **env), text=True, capture_output=True,
                              timeout=180)

    def use_scratch(self, scratch):
        self.scratch = scratch
        if scratch is None:
            self.env.pop("ORBIT_SCRATCH_DIR", None)
            self.scratch = self.repo / ".orbit/tmp"
        else:
            self.env["ORBIT_SCRATCH_DIR"] = str(scratch)

    def use_cache(self, cache):
        self.cache = cache
        self.env["ORBIT_CODEQL_CACHE_DIR"] = str(cache)

    def run_dir(self, result):
        marker = "codeql-rust-local: run directory: "
        return Path(next(line[len(marker):] for line in result.stderr.splitlines()
                         if line.startswith(marker)))

    def calls(self):
        if not self.calls_file.exists():
            return []
        return [json.loads(line) for line in self.calls_file.read_text().splitlines()]

    def assert_no_result(self, result, expected_calls):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual([call["name"] for call in self.calls()], expected_calls)
        self.assertEqual(list(self.root.rglob("results.sarif")), [])
        self.assertEqual(result.stdout, "")
        self.assert_isolated()

    def assert_isolated(self):
        for directory in self.global_dirs:
            self.assertEqual(list(directory.iterdir()), [directory / "sentinel"])
            self.assertEqual((directory / "sentinel").read_text(), "preserve")
        for call in self.calls():
            env = call["env"]
            run_dir = Path(env["CARGO_HOME"]).parent
            self.assertTrue(run_dir.is_relative_to(self.scratch))
            for key in ("CARGO_HOME", "CARGO_TARGET_DIR", "TMPDIR", "XDG_CACHE_HOME"):
                self.assertTrue(Path(env[key]).is_relative_to(run_dir), (key, env[key]))
            # The toolchain is the host cache's, or this run's own when the
            # cache is unusable; never the user's rustup.
            rustup_home = Path(env["RUSTUP_HOME"])
            self.assertTrue(rustup_home.is_relative_to(self.cache)
                            or rustup_home == run_dir / "rustup", rustup_home)
            self.assertEqual(env["RUSTC_WRAPPER"], "")
            self.assertEqual(env["RUSTC_WORKSPACE_WRAPPER"], "")
            if call["name"] == "codeql":
                for prefix in ("--common-caches=", "--logdir=", "--output=", "--compilation-cache=",
                               "--codescanning-config="):
                    for arg in call["args"]:
                        if arg.startswith(prefix):
                            self.assertTrue(Path(arg[len(prefix):]).is_relative_to(run_dir))

    def assert_selects_only_production_source(self):
        create = next(call for call in self.calls() if call["args"][:2] == ["database", "create"])
        self.assertEqual(create["sources"], SELECTED)

    def test_prepares_toolchain_then_extracts_and_analyzes_named_query(self):
        query = "queries with spaces/check.ql"
        result = self.run_script("--ram", "8192", "--toolchain", "1.98.1", query)
        self.assertEqual(result.returncode, 0, result.stderr)
        install, create, analyze = self.calls()
        self.assertEqual(install["args"], ["toolchain", "install", "1.98.1", "--profile",
                                           "minimal", "--component", "rust-src", "--no-self-update"])
        self.assertEqual(install["env"]["RUSTUP_TOOLCHAIN"], "1.98.1")
        self.assertTrue(Path(install["env"]["RUSTUP_HOME"]).is_relative_to(self.cache / "rust-1.98.1"))
        self.assertEqual(create["env"]["RUSTUP_HOME"], str(self.cache / "rust-1.98.1/rustup"))
        self.assertEqual(create["args"][:2], ["database", "create"])
        self.assertIn(f"--source-root={self.repo}", create["args"])
        self.assertIn("--build-mode=none", create["args"])
        self.assertEqual(analyze["args"][:4], ["database", "analyze", create["args"][2], query])
        self.assertIn("--ram=8192", create["args"])
        self.assertIn("--ram=8192", analyze["args"])
        self.assertIn("--no-default-compilation-cache", analyze["args"])
        self.assertEqual(len(list(self.root.rglob("results.sarif"))), 1)
        self.assert_selects_only_production_source()
        self.assert_isolated()

    def test_scratch_inside_checkout_is_never_extracted(self):
        nested = self.repo / "work/nested scratch"
        for scratch in (None, nested):
            with self.subTest(scratch=scratch):
                self.calls_file.write_text("")
                self.use_scratch(scratch)
                if scratch:
                    (nested / "codeql-rust-local.prior/target/debug").mkdir(parents=True)
                    (nested / "codeql-rust-local.prior/target/debug/out.rs").write_text("fn f() {}\n")
                    (nested / "baseline.rs").write_text("fn baseline() {}\n")
                result = self.run_script("fixture.qls")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assert_selects_only_production_source()
                self.assert_isolated()

    def test_earlier_run_under_another_scratch_is_never_extracted(self):
        earlier = self.repo / "work/codeql scratch"
        self.use_scratch(earlier)
        self.assertEqual(self.run_script("fixture.qls").returncode, 0)
        prior = next(earlier.glob("codeql-rust-local.??????"))
        registry = prior / "cargo/registry/src/index-1/serde-1.0.0/src/lib.rs"
        registry.parent.mkdir(parents=True)
        registry.write_text("pub fn serde() {}\n")
        # Only the run directory layout is excluded, never a lookalike source.
        lookalike = "crates/core/src/codeql-rust-local.abcdef/mod.rs"
        (self.repo / lookalike).parent.mkdir()
        (self.repo / lookalike).write_text("pub fn f() {}\n")
        residue = sorted(earlier.rglob("*"))
        for scratch in (None, self.repo / "work/second scratch", self.root / "outside scratch"):
            with self.subTest(scratch=scratch):
                self.calls_file.write_text("")
                self.use_scratch(scratch)
                result = self.run_script("fixture.qls")
                self.assertEqual(result.returncode, 0, result.stderr)
                create = next(call for call in self.calls() if call["args"][:2] == ["database", "create"])
                self.assertEqual(create["sources"], sorted([*SELECTED, lookalike]))
                self.assert_isolated()
                self.assertEqual(sorted(earlier.rglob("*")), residue)

    def test_earlier_run_under_unmarked_matching_ancestor_is_never_extracted(self):
        # A scratch ancestor can itself look like a run directory without
        # being one. Discovery must keep walking until it finds the marked run.
        earlier = self.repo / "work/codeql-rust-local.abcdef"
        self.use_scratch(earlier)
        self.assertEqual(self.run_script("fixture.qls").returncode, 0)
        prior = next(earlier.glob("codeql-rust-local.??????"))
        registry = prior / "cargo/registry/src/index-1/serde-1.0.0/src/lib.rs"
        registry.parent.mkdir(parents=True)
        registry.write_text("pub fn serde() {}\n")
        residue = sorted(earlier.rglob("*"))

        for scratch in (None, self.repo / "work/second scratch"):
            with self.subTest(scratch=scratch):
                self.calls_file.write_text("")
                self.use_scratch(scratch)
                result = self.run_script("fixture.qls")
                self.assertEqual(result.returncode, 0, result.stderr)
                create = next(call for call in self.calls() if call["args"][:2] == ["database", "create"])
                self.assertEqual(create["sources"], SELECTED)
                self.assert_isolated()
                self.assertEqual(sorted(earlier.rglob("*")), residue)

    def test_unexcludable_earlier_run_directory_refuses_before_preparation(self):
        for label, parent, track in (("tracked", "crates/core/src", True), ("glob", "work/scratch[1]", False)):
            with self.subTest(label):
                prior = self.repo / parent / "codeql-rust-local.abcdef"
                prior.mkdir(parents=True)
                for name in ("codeql-config.yml", "lib.rs"):
                    (prior / name).write_text("\n")
                if track:
                    subprocess.run(["git", "add", str(prior)], cwd=self.repo, env=self.env, check=True)
                result = self.run_script("fixture.qls")
                self.assert_no_result(result, [])
                self.assertIn(str(prior), result.stderr)
                self.assertEqual(list(self.scratch.glob("codeql-rust-local.??????")), [])
                subprocess.run(["git", "rm", "-rqf", "--cached", "--ignore-unmatch", str(prior)],
                               cwd=self.repo, env=self.env, check=True)
                shutil.rmtree(prior)

    def test_ambiguous_source_selection_refuses_before_preparation(self):
        cases = [
            ("checkout", self.repo, None),
            ("tracked", self.repo / "crates", None),
            ("glob", self.repo / "work/scratch[1]", None),
            ("flow", None, 'name: fixture\npaths-ignore: ["**/tests/**"]\n'),
        ]
        for label, scratch, config in cases:
            with self.subTest(label):
                self.use_scratch(scratch)
                if config:
                    self.config.write_text(config)
                result = self.run_script("fixture.qls")
                self.assert_no_result(result, [])
                self.assertEqual(list(self.repo.rglob("codeql-rust-local.??????")), [])

    def test_incomplete_extraction_never_queries_even_when_create_exits_zero(self):
        cases = [
            ("WARN semantic analyzer unavailable (unable to load manifest): macro expansion will be skipped; "
             "Rust 1.99.0: Read-only file system (os error 30)", False),
            ("macro expansion will be skipped", True),
            ("semantic analysis disabled", False),
            ("[WARN] unable to load crate graph", True),
            ("ERROR: extractor failed to resolve dependencies", False),
            # On Linux every production module is active, the Linux-only ones
            # included, so a skip of one is never a platform exclusion.
            ("WARN skipping semantic analysis of crates/orbit-core/src/runtime/linux.rs", True),
        ]
        for log, file_only in cases:
            with self.subTest(log=log, file_only=file_only):
                self.calls_file.write_text("")
                self.use_cache(self.root / f"host cache {len(log)}")
                result = self.run_script("fixture.qls", STUB_EXTRACTION_LOG=log,
                                         STUB_LOG_FILE_ONLY="1" if file_only else "")
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assert_no_result(result, ["rustup", "codeql"])
                self.assertIn(log, result.stderr)
                self.assertIn(self.calls()[0]["args"][2], result.stderr)

    def test_non_linux_host_refuses_before_preparation_with_platform_status(self):
        # Its extractor always skips the Linux-only modules, so the run is
        # owed by Linux evidence rather than attempted and found incomplete.
        for host in ("Darwin", "FreeBSD"):
            with self.subTest(host=host):
                result = self.run_script("fixture.qls", STUB_UNAME=host)
                self.assertEqual(result.returncode, 3, result.stderr)
                self.assert_no_result(result, [])
                self.assertIn(host, result.stderr)
                self.assertFalse(self.scratch.exists())

    def test_install_refusal_names_version_and_leaves_user_homes_untouched(self):
        before = {path.relative_to(self.root) for path in self.root.rglob("*")}
        result = self.run_script("--toolchain", "1.98.1", "fixture.ql", STUB_INSTALL_FAIL="1")
        self.assert_no_result(result, ["rustup"])
        self.assertIn("1.98.1", result.stderr)
        self.assertIn("Read-only file system (os error 30)", result.stderr)
        added = {path.relative_to(self.root) for path in self.root.rglob("*")} - before
        self.assertTrue(all(path == Path("calls.jsonl") or path.parts[0] in ("run scratch", "host cache")
                            for path in added), added)
        # A failed install leaves nothing a later run could mistake for a toolchain.
        self.assertEqual(sorted(p.name for p in (self.cache / "rust-1.98.1").iterdir()), ["lock"])

    def test_nonzero_extraction_without_warning_never_queries(self):
        result = self.run_script("fixture.ql", STUB_CREATE_EXIT="12")
        self.assert_no_result(result, ["rustup", "codeql"])
        self.assertIn("12", result.stderr)

    def test_failed_query_is_not_reported_as_completed(self):
        result = self.run_script("fixture.ql", STUB_ANALYZE_FAIL="1")
        self.assert_no_result(result, ["rustup", "codeql", "codeql"])
        self.assertIn("heap exhausted", result.stderr)

    def test_second_run_reuses_the_cached_toolchain_without_rustup(self):
        first = self.run_script("fixture.ql")
        self.assertEqual(first.returncode, 0, first.stderr)
        self.calls_file.write_text("")
        second = self.run_script("--fresh", "fixture.ql")
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual([call["name"] for call in self.calls()], ["codeql", "codeql"])
        self.assertIn("no rustup install ran", (self.run_dir(second) / "toolchain.log").read_text())
        home = self.cache / "rust-1.97.0/rustup"
        self.assertTrue(all(call["env"]["RUSTUP_HOME"] == str(home) for call in self.calls()))
        self.assertEqual((home / "manifest.sha256").stat().st_mode & 0o222, 0)
        self.assert_selects_only_production_source()
        self.assert_isolated()

    def test_corrupt_or_wrong_version_cache_is_reinstalled_whole(self):
        self.assertEqual(self.run_script("fixture.ql").returncode, 0)
        home = self.cache / "rust-1.97.0/rustup"
        library = home / "toolchains/1.97.0-x86_64-unknown-linux-gnu/lib/rustlib/src/rust/library"
        # A copy whose manifest is intact but whose rustc is another version.
        shutil.copytree(self.cache / "rust-1.97.0", self.cache / "rust-1.98.1", symlinks=True)
        library.chmod(0o755)
        (library / "core/src").chmod(0o755)
        (library / "core/src/lib.rs").unlink()
        for toolchain, damaged in (("1.97.0", "missing rust-src file"), ("1.98.1", "wrong version")):
            with self.subTest(damaged):
                self.calls_file.write_text("")
                result = self.run_script("--fresh", "--toolchain", toolchain, "fixture.ql")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual([call["name"] for call in self.calls()], ["rustup", "codeql", "codeql"])
                version_dir = self.cache / f"rust-{toolchain}"
                self.assertEqual(sorted(p.name for p in version_dir.iterdir()), ["lock", "rustup"])
                rustc = version_dir / f"rustup/toolchains/{toolchain}-x86_64-unknown-linux-gnu/bin/rustc"
                self.assertIn(toolchain, subprocess.run([rustc], capture_output=True, text=True).stdout)
                self.assert_isolated()

    def test_unusable_cache_falls_back_to_a_run_local_install_without_a_slot(self):
        blocker = self.root / "not a directory"
        blocker.write_text("")
        self.use_cache(blocker / "cache")
        result = self.run_script("fixture.ql")
        self.assertEqual(result.returncode, 0, result.stderr)
        install = self.calls()[0]
        run_dir = self.run_dir(result)
        self.assertEqual(install["env"]["RUSTUP_HOME"], str(run_dir / "rustup"))
        self.assertIn("for this run only", result.stderr)
        self.assertIn("running without a host slot", result.stderr)
        self.assertFalse((run_dir / "rustup").exists())
        self.assert_isolated()

    def test_warm_read_only_cache_is_used_without_writing(self):
        self.assertEqual(self.run_script("fixture.ql").returncode, 0)
        for directory in (self.cache, self.cache / "rust-1.97.0", self.cache / "slots"):
            directory.chmod(0o555)
        self.addCleanup(lambda: [d.chmod(0o755) for d in (self.cache, self.cache / "rust-1.97.0",
                                                          self.cache / "slots")])
        before = sorted(self.cache.rglob("*"))
        self.calls_file.write_text("")
        result = self.run_script("--fresh", "fixture.ql")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([call["name"] for call in self.calls()], ["codeql", "codeql"])
        self.assertIn("acquired CodeQL slot", result.stderr)
        self.assertEqual(sorted(self.cache.rglob("*")), before)

    def test_persistent_count_waits_for_existing_lock_on_cold_and_warm_caches(self):
        # Pause the admission retry at a FIFO, rather than infer ordering from
        # elapsed time. Premature extraction signals the same FIFO and fails.
        sleep = self.bin / "sleep"
        sleep.write_text('''#!/usr/bin/env python3
import os
with open(os.environ["STUB_ADMISSION_EVENT"], "w") as pipe:
    pipe.write("waiting\\n")
with open(os.environ["STUB_ADMISSION_RELEASE"]) as pipe:
    pipe.read(1)
''')
        sleep.chmod(0o755)
        for warm in (False, True):
            with self.subTest(warm=warm):
                self.use_cache(self.root / f"count cache {warm}")
                if warm:
                    primed = self.run_script("--fresh", "fixture.ql")
                    self.assertEqual(primed.returncode, 0, primed.stderr)
                slot_dir = self.cache / "slots"
                slot_dir.mkdir(parents=True, exist_ok=True)
                count = self.cache / "slot-count"
                count.write_text(" 1\n")
                events, release = self.root / f"events-{warm}", self.root / f"release-{warm}"
                os.mkfifo(events)
                os.mkfifo(release)
                self.calls_file.write_text("")
                lock = slot_dir / "slot-001.lock"
                with lock.open("a+") as held, \
                        os.fdopen(os.open(events, os.O_RDWR | os.O_NONBLOCK), "r+b", buffering=0) as event_pipe, \
                        os.fdopen(os.open(release, os.O_RDWR | os.O_NONBLOCK), "r+b", buffering=0) as release_pipe:
                    fcntl.flock(held, fcntl.LOCK_EX)
                    inode = lock.stat().st_ino
                    env = dict(self.env, ORBIT_CODEQL_SLOTS="", STUB_ADMISSION_EVENT=str(events),
                               STUB_ADMISSION_RELEASE=str(release))
                    process = subprocess.Popen(["bash", str(self.script), "--fresh", "fixture.ql"],
                                               cwd=self.root, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                               process_group=0)
                    try:
                        ready, _, _ = select.select([event_pipe], [], [], 180)
                        self.assertTrue(ready, "child never reached admission or extraction")
                        self.assertEqual(event_pipe.read(64), b"waiting\n", "extraction started before admission")
                        self.assertFalse(any(call["name"] == "codeql" for call in self.calls()))
                        self.assertEqual(lock.stat().st_ino, inode, "a live slot lock must not be replaced")
                        fcntl.flock(held, fcntl.LOCK_UN)
                        release_pipe.write(b"1")
                        stdout, stderr = process.communicate(timeout=180)
                        self.assertEqual(process.returncode, 0, stdout + stderr)
                        self.assertEqual([call["args"][:2] for call in self.calls() if call["name"] == "codeql"],
                                         [["database", "create"], ["database", "analyze"]])
                        self.assertIn("acquired CodeQL slot 1 of 1", stderr)
                        self.assertNotIn("running without a host slot", stderr)
                        self.assertEqual(count.read_text(), " 1\n")
                        self.assertEqual(lock.stat().st_ino, inode)
                        # The completed run releases the very same lock inode.
                        fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    finally:
                        try:
                            os.killpg(process.pid, signal.SIGKILL)
                        except ProcessLookupError:
                            pass
                        process.communicate(timeout=180)
                self.assert_isolated()

    def test_environment_count_overrides_persistent_count_and_keeps_existing_locks(self):
        slot_dir = self.cache / "slots"
        slot_dir.mkdir(parents=True)
        # An invalid persistent value is ignored when an override is present.
        (self.cache / "slot-count").write_text("not a count\n")
        lock = slot_dir / "slot-001.lock"
        with lock.open("a+") as held:
            fcntl.flock(held, fcntl.LOCK_EX)
            inode = lock.stat().st_ino
            result = self.run_script("fixture.ql", ORBIT_CODEQL_SLOTS="2")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("acquired CodeQL slot 2 of 2", result.stderr)
            self.assertNotIn("running without a host slot", result.stderr)
            self.assertEqual(lock.stat().st_ino, inode)
        self.assertEqual([call["name"] for call in self.calls()], ["rustup", "codeql", "codeql"])
        self.assert_isolated()

    def test_legacy_count_is_migrated_without_losing_its_value(self):
        self.cache.mkdir()
        legacy = self.cache / "slots"
        legacy.write_text(" 1\n")
        inode = legacy.stat().st_ino
        result = self.run_script("fixture.ql", ORBIT_CODEQL_SLOTS="")
        self.assertEqual(result.returncode, 0, result.stderr)
        count = self.cache / "slot-count"
        self.assertEqual(count.read_text(), " 1\n")
        self.assertEqual(count.stat().st_ino, inode)
        self.assertEqual([path.name for path in legacy.iterdir()], ["slot-001.lock"])
        self.assertIn("acquired CodeQL slot 1 of 1", result.stderr)
        self.assertNotIn("running without a host slot", result.stderr)
        self.assertEqual([call["name"] for call in self.calls()], ["rustup", "codeql", "codeql"])
        self.assert_isolated()

    def test_legacy_migration_denial_stops_before_extraction(self):
        self.cache.mkdir()
        legacy = self.cache / "slots"
        legacy.write_text("1\n")
        # Inject a rename denial without relying on UID-dependent permissions.
        real_mv = shutil.which("mv", path=self.tool_path)
        self.assertIsNotNone(real_mv)
        mv = self.bin / "mv"
        mv.write_text(f'''#!/usr/bin/env python3
import os, sys
cache = os.environ["ORBIT_CODEQL_CACHE_DIR"]
if sys.argv[1:] == ["-T", "--", cache + "/slots", cache + "/slot-count"]:
    print("mv: injected migration permission denied", file=sys.stderr)
    sys.exit(1)
os.execv({real_mv!r}, sys.argv)
''')
        mv.chmod(0o755)
        result = self.run_script("fixture.ql", ORBIT_CODEQL_SLOTS="2")
        self.assert_no_result(result, ["rustup"])
        self.assertIn("cannot migrate", result.stderr)
        self.assertNotIn("running without a host slot", result.stderr)
        self.assertEqual(legacy.read_text(), "1\n")
        self.assertFalse((self.cache / "slot-count").exists())

    def test_conflicting_legacy_and_current_count_files_are_preserved(self):
        self.cache.mkdir()
        legacy, current = self.cache / "slots", self.cache / "slot-count"
        legacy.write_text("1\n")
        current.write_text("2\n")
        result = self.run_script("fixture.ql")
        self.assert_no_result(result, ["rustup"])
        self.assertIn("reconcile the CodeQL slot counts", result.stderr)
        self.assertEqual(legacy.read_text(), "1\n")
        self.assertEqual(current.read_text(), "2\n")

    def test_third_concurrent_run_queues_for_one_of_two_slots(self):
        # Warm the cache so only the slots, not the install lock, order the runs.
        self.assertEqual(self.run_script("fixture.ql").returncode, 0)
        gate, events = self.root / "gate", self.root / "events.jsonl"
        env = dict(self.env, ORBIT_CODEQL_SLOTS="2", STUB_GATE=str(gate), STUB_EVENTS=str(events),
                   _ORBIT_CODEQL_WAIT_REPORT_SECONDS="1")
        runs = []
        for index in range(3):
            stderr = (self.root / f"run-{index}.stderr").open("w")
            self.addCleanup(stderr.close)
            runs.append((subprocess.Popen(["bash", str(self.script), "--fresh", "fixture.ql"], cwd=self.root,
                                          env=env, stdout=subprocess.DEVNULL, stderr=stderr),
                         self.root / f"run-{index}.stderr"))

        def starts():
            lines = events.read_text().splitlines() if events.exists() else []
            return [json.loads(line) for line in lines if json.loads(line)["kind"] == "start"]

        def waiting():
            return [path for _, path in runs if "still waiting for a CodeQL slot" in path.read_text()]

        deadline = time.monotonic() + 120
        while (len(starts()) < 2 or not waiting()) and time.monotonic() < deadline:
            time.sleep(0.1)
        self.assertEqual(len(starts()), 2, [path.read_text() for _, path in runs])
        self.assertEqual(len(waiting()), 1)
        gate.touch()
        for process, path in runs:
            self.assertEqual(process.wait(timeout=180), 0, path.read_text())
        queued = waiting()[0].read_text()
        self.assertRegex(queued, r"waiting for a CodeQL slot \(slots=2, ")
        seconds = float(re.search(r"acquired CodeQL slot [12] of 2 after queueing ([0-9.]+)s", queued)[1])
        self.assertGreaterEqual(seconds, 1.0)
        # Each run's create and analyze hold its slot, so at most two overlap.
        marks = sorted((json.loads(line)["at"], json.loads(line)["kind"] == "start")
                       for line in events.read_text().splitlines())
        active = peak = 0
        for _, start in marks:
            active += 1 if start else -1
            peak = max(peak, active)
        self.assertEqual(peak, 2)

    def test_threads_default_to_each_slots_share_of_the_processors(self):
        # nproc counts the processors this process may use, less any OpenMP limit.
        for name in ("OMP_NUM_THREADS", "OMP_THREAD_LIMIT"):
            self.env.pop(name, None)
        share = max(1, usable_processors() // 2)
        for label, env, expected in (("share", dict(ORBIT_CODEQL_SLOTS="2"), share),
                                     ("override", dict(ORBIT_CODEQL_THREADS="3"), 3)):
            with self.subTest(label):
                self.calls_file.write_text("")
                result = self.run_script("--fresh", "fixture.ql", **env)
                self.assertEqual(result.returncode, 0, result.stderr)
                create, analyze = (call for call in self.calls() if call["name"] == "codeql")
                self.assertIn(f"--threads={expected}", create["args"])
                self.assertIn(f"--threads={expected}", analyze["args"])
                self.assertIn("query analysis took", result.stderr)
        for invalid in ("0", "many"):
            with self.subTest(invalid=invalid):
                self.calls_file.write_text("")
                result = self.run_script("--fresh", "fixture.ql", ORBIT_CODEQL_THREADS=invalid)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertEqual(self.calls(), [])
                self.assertFalse((self.run_dir(result) / "results.sarif").exists())
                self.assertIn("ORBIT_CODEQL_THREADS", result.stderr)

    def test_identical_inputs_reuse_the_passing_run_without_extracting(self):
        first = self.run_script("fixture.qls")
        self.assertEqual(first.returncode, 0, first.stderr)
        passed = self.run_dir(first)
        # Orbit state and Cargo output are never extraction inputs.
        for ignored in (".orbit/state.json", "target/debug/build/serde-1/out/other.rs"):
            (self.repo / ignored).write_text("changed\n")
        self.calls_file.write_text("")
        again = self.run_script("fixture.qls")
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertEqual(self.calls(), [])
        self.assertEqual(self.run_dir(again), passed)
        self.assertEqual(again.stdout.strip(), "codeql-rust-local: analysis completed; inspect rule and "
                         f"affected locations in {passed / 'results.sarif'}")
        self.assertEqual(list(self.scratch.glob("codeql-rust-local.??????")), [passed])

        source = self.repo / "crates/core/src/lib.rs"
        changes = [
            ("--fresh", lambda: None, {}, "fixture.qls"),
            ("query", lambda: None, {}, "other.qls"),
            ("codeql version", lambda: None, dict(STUB_CODEQL_VERSION="2.28.0"), "fixture.qls"),
            ("extractor option", lambda: None, dict(CODEQL_EXTRACTOR_RUST_OPTION_CARGO_FEATURES="x"), "fixture.qls"),
            ("tracked source", lambda: source.write_text("pub fn g() {}\n"), {}, "fixture.qls"),
            ("untracked source", lambda: (self.repo / "crates/core/src/new.rs").write_text("fn n() {}\n"), {},
             "fixture.qls"),
            ("configuration", lambda: self.config.write_text(self.config.read_text() + "\n# edited\n"), {},
             "fixture.qls"),
        ]
        for label, change, env, query in changes:
            with self.subTest(label):
                change()
                args = ["--fresh", query] if label == "--fresh" else [query]
                self.calls_file.write_text("")
                result = self.run_script(*args, **env)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual([call["args"][:2] for call in self.calls()],
                                 [["database", "create"], ["database", "analyze"]])
                self.assertNotEqual(self.run_dir(result), passed)
                self.assert_isolated()

    def test_external_suite_query_edits_run_analysis_again(self):
        external = self.root / "external queries"
        external.mkdir()
        suite = external / "review.qls"
        query = external / "check.ql"
        suite.write_text("- query: check.ql\n")
        query.write_text('select "before"\n')

        first = self.run_script(str(suite))
        self.assertEqual(first.returncode, 0, first.stderr)
        first_dir = self.run_dir(first)
        self.assertEqual([call["args"][:2] for call in self.calls() if call["name"] == "codeql"],
                         [["database", "create"], ["database", "analyze"]])
        self.assertFalse((first_dir / "inputs.sha256").exists())

        query.write_text('select "after"\n')
        self.calls_file.write_text("")
        second = self.run_script(str(suite))
        self.assertEqual(second.returncode, 0, second.stderr)
        second_dir = self.run_dir(second)
        self.assertNotEqual(second_dir, first_dir)
        self.assertEqual([call["args"][:2] for call in self.calls() if call["name"] == "codeql"],
                         [["database", "create"], ["database", "analyze"]])
        self.assertFalse((second_dir / "inputs.sha256").exists())
        self.assert_isolated()

    def test_external_imported_library_edits_run_analysis_again(self):
        external = self.root / "external query pack"
        external.mkdir()
        query = external / "check.ql"
        library = external / "Helpers.qll"
        query.write_text('import Helpers\nselect "before"\n')
        library.write_text("module Helpers { predicate included() { any() } }\n")

        first = self.run_script(str(query))
        self.assertEqual(first.returncode, 0, first.stderr)
        first_dir = self.run_dir(first)
        self.assertEqual([call["args"][:2] for call in self.calls() if call["name"] == "codeql"],
                         [["database", "create"], ["database", "analyze"]])
        self.assertFalse((first_dir / "inputs.sha256").exists())

        library.write_text("module Helpers { predicate included() { none() } }\n")
        self.calls_file.write_text("")
        second = self.run_script(str(query))
        self.assertEqual(second.returncode, 0, second.stderr)
        second_dir = self.run_dir(second)
        self.assertNotEqual(second_dir, first_dir)
        self.assertEqual([call["args"][:2] for call in self.calls() if call["name"] == "codeql"],
                         [["database", "create"], ["database", "analyze"]])
        self.assertFalse((second_dir / "inputs.sha256").exists())
        self.assert_isolated()

    def test_failed_or_edited_run_is_never_reused(self):
        failed = self.run_script("fixture.ql", STUB_ANALYZE_FAIL="1")
        self.assertNotEqual(failed.returncode, 0)
        edited = self.run_script("fixture.ql", STUB_EDIT_DURING_CREATE=str(self.repo / "crates/core/src/lib.rs"))
        self.assertEqual(edited.returncode, 0, edited.stderr)
        self.assertIn("the checkout changed during this run", edited.stderr)
        self.assertFalse((self.run_dir(edited) / "inputs.sha256").exists())
        self.calls_file.write_text("")
        result = self.run_script("fixture.ql")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([call["args"][:2] for call in self.calls()],
                         [["database", "create"], ["database", "analyze"]])
        self.assertTrue((self.run_dir(result) / "inputs.sha256").is_file())

    def test_passing_run_keeps_evidence_and_failing_run_keeps_everything(self):
        passed = self.run_dir(self.run_script("fixture.ql"))
        self.assertEqual(sorted(p.name for p in passed.iterdir()), EVIDENCE)
        self.assertEqual(sorted(p.name for p in (passed / "database").iterdir()), ["log", "src.zip"])
        for case, env in (("extraction", dict(STUB_CREATE_EXIT="12")), ("analysis", dict(STUB_ANALYZE_FAIL="1"))):
            with self.subTest(case):
                result = self.run_script("--fresh", "fixture.ql", **env)
                self.assertNotEqual(result.returncode, 0)
                failed = self.run_dir(result)
                for bulk in BULK:
                    self.assertTrue((failed / bulk).is_dir(), bulk)
                self.assertTrue((failed / "database/db-rust/default/pool").is_file())


if __name__ == "__main__":
    if linux_tool_shims():
        print(f"test-codeql-rust-local: this {platform.system()} host lacks the Linux "
              f"{', '.join(sorted(linux_tool_shims()))} the script uses, so the cases run it "
              "with the test's shims for them", file=sys.stderr)
    unittest.main()
