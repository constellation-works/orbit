"""Public workflow regressions for virtual-root cache identity and reseeding."""

import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import tempfile
import unittest

import self_test_hang

try:
    import yaml
except ImportError:
    yaml = None


ROOT = Path(__file__).resolve().parent.parent


@unittest.skipIf(yaml is None, "PyYAML is required to execute workflow controls")
class CiDependencyCacheWorkflowTests(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(dir=scratch, prefix="dependency-cache-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.document = yaml.safe_load((ROOT / ".github/workflows/ci.yml").read_text())
        self.jobs = self.document["jobs"]
        self.seed = yaml.safe_load((ROOT / ".github/workflows/ci-cache-seed.yml").read_text())
        self.producers = {
            name: next(step for step in self.jobs[name]["steps"]
                       if step.get("uses", "").startswith("Swatinem/rust-cache@"))
            for name in ("ci", "test-build", "msrv")
        }

    def cache_identity(self, step):
        # Execute hashFiles' documented SHA-256-of-file-digests contract for
        # the workflow inputs. Model rust-cache's public shared-key precedence:
        # `key` is ignored when `shared-key` is present. Automatic environment
        # and member/lock hashes stay constant in this root-only reproduction.
        def render(value):
            def hash_files(match):
                patterns = re.findall(r"'([^']+)'", match[1])
                files = sorted({path for pattern in patterns for path in self.root.glob(pattern)
                                if path.is_file()})
                if not files:
                    return ""
                digest = hashlib.sha256()
                for path in files:
                    digest.update(hashlib.sha256(path.read_bytes()).digest())
                return digest.hexdigest()

            return re.sub(r"\$\{\{\s*hashFiles\((.*?)\)\s*\}\}", hash_files, str(value))

        inputs = step.get("with", {})
        return (render(inputs.get("prefix-key", "v0-rust")),
                render(inputs.get("shared-key") or inputs.get("key", "")))

    def test_virtual_root_profile_alone_invalidates_every_ci_dependency_identity(self):
        member = self.root / "crates/member/Cargo.toml"
        member.parent.mkdir(parents=True)
        member.write_text('[package]\nname="member"\nversion="0.1.0"\n')
        lock = self.root / "Cargo.lock"
        lock.write_text("version = 4\n")
        root = self.root / "Cargo.toml"
        root.write_text('[workspace]\nmembers=["crates/member"]\n'
                        '[profile.dev.package."*"]\nopt-level=2\n')
        original = {name: self.cache_identity(step) for name, step in self.producers.items()}
        unchanged = {name: self.cache_identity(step) for name, step in self.producers.items()}
        self.assertEqual(original, unchanged)
        member_before, lock_before = member.read_bytes(), lock.read_bytes()
        root.write_text(root.read_text().replace("opt-level=2", "opt-level=1"))
        for name, step in self.producers.items():
            with self.subTest(producer=name):
                self.assertNotEqual(original[name], self.cache_identity(step),
                                    "virtual workspace profiles must invalidate dependency archives")
        self.assertEqual((member.read_bytes(), lock.read_bytes()), (member_before, lock_before))

    def test_seeders_use_producer_inputs_and_read_only_download_free_lookups(self):
        for role, source in (("seed-check", "ci"), ("seed-build", "test-build")):
            job = self.seed["jobs"][role]
            caches = [step for step in job["steps"]
                      if step.get("uses", "").startswith("Swatinem/rust-cache@")]
            self.assertEqual(len(caches), 2)
            lookup, writer = caches
            self.assertIs(lookup["with"]["lookup-only"], True)
            self.assertIs(lookup["with"]["save-if"], False)
            self.assertIs(lookup["continue-on-error"], True,
                          "unavailable cache presence must still permit bounded production")
            for step in caches:
                producer = self.producers[source]
                self.assertEqual(step["uses"], producer["uses"])
                for key in ("prefix-key", "shared-key", "env-vars", "workspaces",
                            "cache-targets", "cache-bin", "add-rust-environment-hash-key"):
                    self.assertEqual(step["with"].get(key), producer["with"].get(key), key)
            from importlib import import_module
            evaluate = import_module("test-pr-state-workflows").evaluate
            for step in (writer, *[s for s in job["steps"] if "run" in s]):
                self.assertIn("if", step, "an exact hit must skip restore, cleanup and compilation")
                for hit in ("true", "false", "", "unknown"):
                    self.assertEqual(evaluate(step["if"], "push",
                                              bindings={"steps.lookup.outputs.cache-hit": hit}),
                                     hit != "true", "only exact presence avoids the bounded producer")
            self.assertFalse(writer["with"].get("cache-on-failure", False),
                             "failed seeding must remain recoverable on the next push")
            # rust-cache fingerprints these variables before compiling. Step-
            # local lint wrappers match the corresponding CI compile below.
            cache_env = lambda workflow, job: {
                key: value for key, value in (workflow.get("env", {}) | job.get("env", {})).items()
                if key.startswith(("CARGO", "CC", "CFLAGS", "CXX", "CMAKE", "RUST"))}
            self.assertEqual(cache_env(self.seed, job), cache_env(self.document, self.jobs[source]))
            toolchain = lambda job: next(s for s in job["steps"]
                                        if s.get("uses", "").startswith("dtolnay/rust-toolchain@"))
            self.assertEqual(toolchain(job), toolchain(self.jobs[source]))
            self.assertLessEqual(job["timeout-minutes"], self.jobs[source]["timeout-minutes"])

    def test_only_validated_protected_pushes_seed_and_pr_writes_remain_impossible(self):
        from importlib import import_module
        evaluate = import_module("test-pr-state-workflows").evaluate
        events = self.seed.get("on", self.seed.get(True, {}))
        self.assertEqual(set(events), {"push"},
                         "push-only seeding must never enter the PR emission contract")
        self.assertEqual(events["push"], self.document.get("on", self.document.get(True, {}))["push"])
        self.assertEqual(self.seed["permissions"], {"contents": "read"})
        self.assertIs(self.seed["concurrency"]["cancel-in-progress"], False,
                      "active seeders must finish when a pending push is replaced")
        for role in ("seed-check", "seed-build"):
            job = self.seed["jobs"][role]
            self.assertNotIn("permissions", job, "a cache writer needs no elevated token")
            self.assertEqual(job["needs"], "scope")
            writer = next(s for s in job["steps"] if s.get("with", {}).get("save-if"))
            for event in ("push", "pull_request", "pull_request_target", "workflow_dispatch"):
                self.assertEqual(evaluate(writer["with"]["save-if"], event), event == "push")
                for mode in ("skip", "full", "docs", "affected:orbit-cli", "", "unknown"):
                    with self.subTest(role=role, event=event, mode=mode):
                        self.assertEqual(evaluate(job["if"], event, bindings={"needs.scope.outputs.mode": mode}),
                                         event == "push" and mode == "skip")
            checkout = next(s for s in job["steps"]
                            if s.get("uses", "").startswith("actions/checkout@"))
            self.assertEqual(checkout["with"]["ref"], "${{ github.sha }}",
                             "a delayed push must compile its own cache inputs")
            self.assertIs(checkout["with"]["persist-credentials"], False)

    def run_seed_compile(self, role, hit, failure=0):
        from importlib import import_module
        evaluate = import_module("test-pr-state-workflows").evaluate
        job = self.seed["jobs"][role]
        step = next(s for s in job["steps"] if s.get("name", "").startswith("Compile "))
        compile_admitted = evaluate(step["if"], "push", bindings={"steps.lookup.outputs.cache-hit": hit})
        # The fake Cargo records argv and lint environment through real Bash
        # and the public folded-build wrapper; no compile or synthetic load.
        commands = self.root / "commands.jsonl"
        commands.write_text("")
        cargo = self.root / "bin/cargo"
        cargo.parent.mkdir(exist_ok=True)
        cargo.write_text("#!/usr/bin/env python3\nimport os,sys,json\n"
                         "with open(os.environ['FIXTURE_COMMANDS'],'a') as f:\n"
                         " f.write(json.dumps(dict(argv=sys.argv[1:],wrapper=os.environ.get('RUSTC_WORKSPACE_WRAPPER'),"
                         "lint=os.environ.get('CLIPPY_ARGS'),doc=os.environ.get('RUSTDOCFLAGS')))+'\\n')\n"
                         "sys.exit(int(os.environ['FIXTURE_EXIT']))\n")
        cargo.chmod(0o755)
        scripts = self.root / "scripts"
        scripts.mkdir(exist_ok=True)
        shutil.copy(ROOT / "scripts/with-test-build-lint.sh", scripts)
        env = dict(os.environ, **self.seed["env"], **job.get("env", {}), **step.get("env", {}))
        env.update(PATH=f"{cargo.parent}:{os.environ['PATH']}", FIXTURE_COMMANDS=str(commands),
                   FIXTURE_EXIT=str(failure))
        result = None
        if compile_admitted:
            result = self_test_hang.run(["bash", "-e", "-o", "pipefail", "-c", step["run"]],
                                        step=step["name"], cwd=self.root, env=env,
                                        text=True, capture_output=True)
            self.assertEqual(result.returncode, failure, result.stdout + result.stderr)
        return [json.loads(line) for line in commands.read_text().splitlines()], result

    def test_exact_presence_skips_compilation_and_prefix_missing_or_error_produces_each_role(self):
        for role in ("seed-check", "seed-build"):
            for hit in ("true", "false", "", "unknown"):
                with self.subTest(role=role, cache_hit=hit):
                    commands, _ = self.run_seed_compile(role, hit)
                    if hit == "true":
                        self.assertEqual(commands, [])
                        continue
                    self.assertTrue(commands)
                    self.assertTrue(all(c["wrapper"] == "clippy-driver" for c in commands))
                    if role == "seed-check":
                        self.assertEqual([c["argv"] for c in commands], [
                            ["check", "--locked", "--workspace", "--lib", "--bins"],
                            ["doc", "--locked", "--no-deps", "--workspace"]])
                        self.assertEqual(commands[1]["doc"], "-D warnings")
                        self.assertTrue(all("-D__CLIPPY_HACKERY__clippy::disallowed_methods" in c["lint"]
                                            for c in commands))
                    else:
                        self.assertEqual([c["argv"] for c in commands], [
                            ["test", "--no-run", "--locked", "--workspace", "--lib", "--bins", "--tests", "--examples"]])
                        self.assertIn("-A__CLIPPY_HACKERY__clippy::disallowed_methods", commands[0]["lint"])
            # A producer failure is visible and does not run later commands;
            # a subsequent ordinary push can try again, with no hidden retry.
            commands, result = self.run_seed_compile(role, "false", failure=7)
            self.assertEqual(result.returncode, 7)
            self.assertEqual(len(commands), 1)
            commands, result = self.run_seed_compile(role, "false")
            self.assertEqual(result.returncode, 0)
