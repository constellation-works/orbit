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
        msrv = yaml.safe_load((ROOT / ".github/workflows/msrv.yml").read_text())["jobs"]["msrv"]
        self.producers = {
            name: next(step for step in job["steps"]
                       if step.get("uses", "").startswith("Swatinem/rust-cache@"))
            for name, job in (("ci", self.jobs["ci"]), ("test-build", self.jobs["test-build"]),
                              ("msrv", msrv))
        }
        # Every dependency-cache step in the three workflows, producers and
        # seeders alike, with the job whose earlier steps derive its key.
        self.cache_steps = [
            (f"{workflow}:{name}:{index}", job, step)
            for workflow, jobs in (("ci", self.jobs), ("seed", self.seed["jobs"]),
                                   ("msrv", {"msrv": msrv}))
            for name, job in jobs.items()
            for index, step in enumerate(job.get("steps", []))
            if step.get("uses", "").startswith("Swatinem/rust-cache@")]
        self.producer_jobs = {"ci": self.jobs["ci"], "test-build": self.jobs["test-build"],
                              "msrv": msrv}

    def render_cache_input(self, value):
        # Execute hashFiles' documented SHA-256-of-file-digests contract for
        # the workflow inputs.
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

    def step_outputs(self, job, step):
        # Execute, through real Bash in the fixture checkout, every earlier
        # run step with an id this cache step reads (the shared key helper),
        # from whichever checkout the workflow invokes it.
        outputs = {}
        for earlier in job["steps"][:job["steps"].index(step)]:
            if "run" not in earlier or "id" not in earlier or \
                    f"steps.{earlier['id']}.outputs." not in json.dumps(step):
                continue
            for helper in re.findall(r"(?:[\w.-]+/)*scripts/ci-[\w.-]+\.py", earlier["run"]):
                target = self.root / helper
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy(ROOT / "scripts" / Path(helper).name, target)
            sink = self.root / "github-output"
            sink.write_text("")
            self_test_hang.run(["bash", "-e", "-o", "pipefail", "-c", earlier["run"]],
                               step=earlier.get("name", earlier["id"]), cwd=self.root, check=True,
                               env=dict(os.environ, GITHUB_OUTPUT=str(sink)),
                               text=True, capture_output=True)
            for line in sink.read_text().splitlines():
                key, _, value = line.partition("=")
                outputs[f"steps.{earlier['id']}.outputs.{key}"] = value
        return outputs

    def cache_identity(self, step, job=None):
        # Model rust-cache's public shared-key precedence: `key` is ignored
        # when `shared-key` is present. Automatic environment and member/lock
        # hashes stay constant in this root-only reproduction.
        outputs = self.step_outputs(job, step) if job is not None else {}

        def render(value):
            value = re.sub(r"\$\{\{\s*([\w.-]+)\s*\}\}",
                           lambda match: outputs.get(match[1], match[0]), str(value))
            value = self.render_cache_input(value)
            self.assertNotIn("${{", value, "cache identity must be derived before the cache step")
            return value

        inputs = step.get("with", {})
        return (render(inputs.get("prefix-key", "v0-rust")),
                render(inputs.get("shared-key") or inputs.get("key", "")))

    def write_virtual_root(self, root_text):
        member = self.root / "crates/member/Cargo.toml"
        member.parent.mkdir(parents=True, exist_ok=True)
        member.write_text('[package]\nname="member"\nversion="0.1.0"\n')
        lock = self.root / "Cargo.lock"
        lock.write_text("version = 4\n")
        root = self.root / "Cargo.toml"
        root.write_text(root_text)
        return root, member, lock

    def all_identities(self):
        return {label: self.cache_identity(step, job) for label, job, step in self.cache_steps}

    def test_workflow_edits_reuse_workspace_cache_but_build_inputs_change_identity(self):
        restore = next(step for step in self.jobs["test-build"]["steps"]
                       if step.get("id") == "workspace-test-cache")
        inputs = {
            "Cargo.toml": '[workspace]\nmembers=["crates/member"]\n',
            "Cargo.lock": "version = 4\n",
            "crates/member/Cargo.toml": '[package]\nname="member"\nversion="0.1.0"\n',
            ".cargo/config.toml": "[build]\njobs=4\n",
            "scripts/with-test-build-lint.sh": "exec cargo test\n",
            "scripts/ci-workspace-cache.py": "# source manifest implementation\n",
            ".github/workflows/ci.yml": "jobs: {}\n",
        }
        for name, content in inputs.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)

        def identity():
            return tuple(self.render_cache_input(restore["with"][field])
                         for field in ("key", "restore-keys"))

        before = identity()
        workflow = self.root / ".github/workflows/ci.yml"
        workflow.write_text(workflow.read_text() + "# unrelated job scheduling edit\n")
        self.assertEqual(identity(), before,
                         "CI-only edits must not force a cold workspace build: hosted run "
                         "44043933622 missed its workspace cache after agent-main commit "
                         "46c0b7754 changed only ci.yml; Cargo and the source manifest still "
                         "validate reused outputs")
        for name in sorted(inputs.keys() - {".github/workflows/ci.yml"}):
            with self.subTest(build_input=name):
                path = self.root / name
                original = path.read_text()
                path.write_text(original + "# changed build input\n")
                self.assertNotEqual(identity(), before,
                                    f"{name} must retain its separate workspace cache identity")
                path.write_text(original)

    def test_virtual_root_profile_alone_invalidates_every_ci_dependency_identity(self):
        root, member, lock = self.write_virtual_root(
            '[workspace]\nmembers=["crates/member"]\n[profile.dev.package."*"]\nopt-level=2\n')
        self.assertGreaterEqual(len(self.cache_steps), 8, "every producer and seeder is checked")
        original = self.all_identities()
        self.assertEqual(original, self.all_identities())
        member_before, lock_before = member.read_bytes(), lock.read_bytes()
        root.write_text(root.read_text().replace("opt-level=2", "opt-level=1"))
        changed = self.all_identities()
        for label in original:
            with self.subTest(cache_step=label):
                self.assertNotEqual(original[label], changed[label],
                                    "virtual workspace profiles must invalidate dependency archives")
        self.assertEqual((member.read_bytes(), lock.read_bytes()), (member_before, lock_before))

    def test_dependency_and_release_root_edits_keep_every_dependency_restore_namespace(self):
        # PR #9937 (runs 44236419113, 44234277166) added two workspace
        # dependencies; the whole-root digest moved the restore prefix, so
        # rust-cache restored nothing and Test build compiled for 524/518 s.
        # The exact key still changes through Cargo.lock; Cargo rebuilds only
        # the units whose inputs changed (docs/runbooks/compiler-cache.md).
        base = ('[workspace]\nmembers=["crates/member"]\nresolver="2"\n'
                '[profile.dev.package."*"]\nopt-level=1\n'
                '[workspace.package]\nversion="0.51.0"\nedition="2024"\n'
                '[workspace.lints.rust]\nmissing_docs="warn"\n'
                '[workspace.dependencies]\nchrono-tz="0.10"\nserde_json="1"\n')
        root, _, _ = self.write_virtual_root(base)
        original = self.all_identities()
        edits = {
            "add dependencies": base + ('tz = { package = "tz-rs", version = "0.7", '
                                        'default-features = false, features = ["alloc"] }\n'
                                        '# keep the same IANA release\ntzdb_data = "0.2.2"\n'),
            "remove a dependency": base.replace('chrono-tz="0.10"\n', ""),
            "release version bump": base.replace('version="0.51.0"', 'version="0.51.1"'),
            "add a member": base.replace('members=["crates/member"]',
                                         'members=["crates/member", "crates/other"]'),
        }
        for edit, text in edits.items():
            root.write_text(text)
            changed = self.all_identities()
            for label in original:
                with self.subTest(edit=edit, cache_step=label):
                    self.assertEqual(changed[label], original[label],
                                     "member-only and lock-recorded root edits must keep the "
                                     "dependency restore namespace")
        for edit, text in {"patch": base + '[patch.crates-io]\nserde_json={path="vendor/serde_json"}\n',
                           "resolver": base.replace('resolver="2"', 'resolver="3"')}.items():
            root.write_text(text)
            changed = self.all_identities()
            for label in original:
                with self.subTest(edit=edit, cache_step=label):
                    self.assertNotEqual(changed[label], original[label],
                                        "root settings outside the lock hash must start a new namespace")

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
            self.write_virtual_root('[workspace]\nmembers=["crates/member"]\n')
            producer = self.producers[source]
            producer_identity = self.cache_identity(producer, self.producer_jobs[source])
            for step in caches:
                self.assertEqual(step["uses"], producer["uses"])
                for key in ("shared-key", "env-vars", "workspaces",
                            "cache-targets", "cache-bin", "add-rust-environment-hash-key"):
                    self.assertEqual(step["with"].get(key), producer["with"].get(key), key)
                self.assertEqual(self.cache_identity(step, job), producer_identity)

            # rust-cache's exact key hashes every .cargo/config.toml and
            # rust-toolchain(.toml) below the workspace, hidden directories
            # included: hosted Check run 44234785691 lists those of its
            # .orbit/tmp/dependency-producer checkout. A seeder without the
            # same extra checkouts writes a key its consumer never hits.
            def key_file_checkouts(steps, cache):
                found = set()
                for step in steps[:steps.index(cache)]:
                    options = step.get("with", {})
                    if not step.get("uses", "").startswith("actions/checkout@") or \
                            not options.get("path"):
                        continue
                    sparse = options.get("sparse-checkout")
                    if sparse and not re.search(r"\.cargo|rust-toolchain|^/?\*|^/$", sparse, re.M):
                        continue
                    found.add((options["path"], options.get("ref", "${{ github.sha }}")))
                return found
            source_steps = self.producer_jobs[source]["steps"]
            for step in caches:
                self.assertEqual(key_file_checkouts(job["steps"], step),
                                 key_file_checkouts(source_steps, producer), role)
            from importlib import import_module
            evaluate = import_module("test-pr-state-workflows").evaluate
            # Key derivation precedes the lookup; everything after it is gated.
            after_lookup = job["steps"][job["steps"].index(lookup) + 1:]
            for step in (writer, *[s for s in after_lookup if "run" in s]):
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
                    if role == "seed-check":
                        self.assertEqual([c["argv"] for c in commands], [
                            ["check", "--locked", "--workspace", "--lib", "--bins"],
                            ["doc", "--locked", "--no-deps", "--workspace"],
                            ["check", "--locked", "--workspace", "--lib", "--bins", "--tests",
                             "--target", "aarch64-apple-darwin"]])
                        self.assertEqual([c["wrapper"] for c in commands],
                                         ["clippy-driver", "clippy-driver", None])
                        self.assertEqual(commands[1]["doc"], "-D warnings")
                        self.assertIsNone(commands[2]["lint"])
                        self.assertTrue(all("-A__CLIPPY_HACKERY__clippy::disallowed_methods" in c["lint"]
                                            for c in commands[:2]))
                    else:
                        self.assertTrue(all(c["wrapper"] == "clippy-driver" for c in commands))
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
