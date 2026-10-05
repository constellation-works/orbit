#!/usr/bin/env python3
"""Exercise local CodeQL's subprocess boundary without CodeQL or downloads."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


REPO_ROOT = Path(__file__).resolve().parent.parent
SCRATCH = Path(os.environ.get("ORBIT_SCRATCH_DIR", REPO_ROOT / ".orbit/tmp"))

# The codeql stub enumerates .rs files under --source-root minus the effective
# config's paths-ignore globs, as CodeQL's build-mode none source walk does.
STUB = r'''#!/usr/bin/env python3
import json, os, re, sys
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
if name == "rustup":
    if os.environ.get("STUB_INSTALL_FAIL"):
        print("error: could not create temp file: Read-only file system (os error 30)", file=sys.stderr)
        sys.exit(30)
    home = Path(os.environ["RUSTUP_HOME"])
    (home / "installed").write_text(args[2])
    vendored = home / "toolchains" / args[2] / "lib/rustlib/src/rust/library/vendor/wasip3"
    (vendored / "src").mkdir(parents=True)
    (vendored / "Cargo.toml").write_text("[package]\n")
    (vendored / "src/lib.rs").write_text("pub fn vendored() {}\n")
elif args[:2] == ["database", "create"]:
    database = Path(args[2])
    (database / "log").mkdir(parents=True)
    log = os.environ.get("STUB_EXTRACTION_LOG", "INFO semantic crate graph extracted")
    if os.environ.get("STUB_LOG_FILE_ONLY"):
        (database / "log/extractor.log").write_text(log)
    else:
        print(log)
    sys.exit(int(os.environ.get("STUB_CREATE_EXIT", "0")))
elif args[:2] == ["database", "analyze"]:
    if os.environ.get("STUB_ANALYZE_FAIL"):
        print("query evaluation: heap exhausted", file=sys.stderr)
        sys.exit(7)
    output = next(arg.split("=", 1)[1] for arg in args if arg.startswith("--output="))
    Path(output).write_text(json.dumps({"version": "2.1.0", "runs": [{"results": []}]}))
else:
    sys.exit(99)
'''

TRACKED = ["Cargo.toml", "crates/core/src/lib.rs", "crates/core/tests/it.rs"]
# Residue a checkout accumulates: generated build output, earlier run scratch
# with a prepared toolchain, and ad-hoc measurement files.
RESIDUE = ["target/debug/build/serde-1/out/generated.rs",
           ".orbit/tmp/measure.rs",
           ".orbit/tmp/codeql-rust-local.prior/rustup/toolchains/1.97.0/lib/rustlib/src/rust/library/core/src/lib.rs"]
# Tracked production source plus a new file a candidate has not yet committed;
# the shared configuration still excludes tests.
SELECTED = ["crates/core/src/added.rs", "crates/core/src/lib.rs"]


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
        self.calls_file = self.root / "calls.jsonl"
        self.global_dirs = [self.root / "user-rustup", self.root / "user-cargo"]
        for directory in self.global_dirs:
            directory.mkdir()
            (directory / "sentinel").write_text("preserve")
        self.scratch = self.root / "run scratch"
        self.env = dict(git_env, PATH=f"{self.bin}:{os.environ['PATH']}",
                        STUB_CALLS=str(self.calls_file),
                        ORBIT_SCRATCH_DIR=str(self.scratch),
                        RUSTUP_HOME=str(self.global_dirs[0]),
                        CARGO_HOME=str(self.global_dirs[1]))

    def run_script(self, *args, **env):
        return subprocess.run(["bash", str(self.script), *args], cwd=self.root,
                              env=dict(self.env, **env), text=True, capture_output=True,
                              timeout=20)

    def use_scratch(self, scratch):
        self.scratch = scratch
        if scratch is None:
            self.env.pop("ORBIT_SCRATCH_DIR", None)
            self.scratch = self.repo / ".orbit/tmp"
        else:
            self.env["ORBIT_SCRATCH_DIR"] = str(scratch)

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
            run_dir = Path(env["RUSTUP_HOME"]).parent
            self.assertTrue(run_dir.is_relative_to(self.scratch))
            for key in ("RUSTUP_HOME", "CARGO_HOME", "CARGO_TARGET_DIR", "TMPDIR", "XDG_CACHE_HOME"):
                self.assertTrue(Path(env[key]).is_relative_to(run_dir), (key, env[key]))
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
        ]
        for log, file_only in cases:
            with self.subTest(log=log, file_only=file_only):
                self.calls_file.write_text("")
                result = self.run_script("fixture.qls", STUB_EXTRACTION_LOG=log,
                                         STUB_LOG_FILE_ONLY="1" if file_only else "")
                self.assert_no_result(result, ["rustup", "codeql"])
                self.assertIn(log, result.stderr)
                self.assertIn(self.calls()[0]["args"][2], result.stderr)

    def test_install_refusal_names_version_and_leaves_user_homes_untouched(self):
        before = {path.relative_to(self.root) for path in self.root.rglob("*")}
        result = self.run_script("--toolchain", "1.98.1", "fixture.ql", STUB_INSTALL_FAIL="1")
        self.assert_no_result(result, ["rustup"])
        self.assertIn("1.98.1", result.stderr)
        self.assertIn("Read-only file system (os error 30)", result.stderr)
        added = {path.relative_to(self.root) for path in self.root.rglob("*")} - before
        self.assertTrue(all(path == Path("calls.jsonl") or path.parts[0] == "run scratch"
                            for path in added), added)

    def test_nonzero_extraction_without_warning_never_queries(self):
        result = self.run_script("fixture.ql", STUB_CREATE_EXIT="12")
        self.assert_no_result(result, ["rustup", "codeql"])
        self.assertIn("12", result.stderr)

    def test_failed_query_is_not_reported_as_completed(self):
        result = self.run_script("fixture.ql", STUB_ANALYZE_FAIL="1")
        self.assert_no_result(result, ["rustup", "codeql", "codeql"])
        self.assertIn("heap exhausted", result.stderr)


if __name__ == "__main__":
    unittest.main()
