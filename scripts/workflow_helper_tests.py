"""Execute workflow/helper revision isolation and its ownership guard offline."""

import copy
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

import self_test_hang

try:
    import yaml
except ImportError:
    yaml = None


ROOT = Path(__file__).resolve().parent.parent
HELPER_CALL = re.compile(r"(?:[\w.-]+/)*scripts/ci-[\w.-]+\.(?:py|sh)")
WORKFLOW_REF = "${{ github.sha }}"
PRODUCER_REF = "${{ github.event.pull_request.base.sha || github.sha }}"


def helper_revision_errors(document):
    """Resolve each orchestration call to its preceding checkout's revision.

    Candidate validators belong to the tested code. Orchestration follows the
    workflow revision; isolated dependency evidence follows the trusted event base.
    """
    errors = []
    for name, job in document["jobs"].items():
        steps = job.get("steps", [])
        if not any("pull_request.head.sha" in step.get("with", {}).get("ref", "")
                   for step in steps):
            continue
        checkouts = {}
        for step in steps:
            if step.get("uses", "").startswith("actions/checkout@"):
                options = step.get("with", {})
                checkouts[options.get("path", "").removeprefix("./").rstrip("/")] = options.get("ref", WORKFLOW_REF)
            for call in HELPER_CALL.findall(step.get("run", "")):
                call = str(Path(step.get("working-directory", "")) / call)
                if Path(call).name == "ci-guardrails.sh":
                    continue
                call = call.removeprefix("./")
                owners = [path for path in checkouts if not path or call.startswith(path + "/")]
                owner = max(owners, key=len) if owners else None
                expected_ref = (PRODUCER_REF if step.get("env", {}).get("ORBIT_DEPENDENCY_EXECUTION")
                                == "isolated-source-v1" else WORKFLOW_REF)
                if owner is None or checkouts[owner] != expected_ref:
                    errors.append(f"{name}: {step.get('name', 'run')}: {call} must use a preceding "
                                  "checkout of the trusted source revision (protects workflow/helper revision agreement)")
    return errors


@unittest.skipIf(yaml is None, "PyYAML is required to execute workflow controls")
class WorkflowHelperRevisionTests(unittest.TestCase):
    def setUp(self):
        self.document = yaml.safe_load((ROOT / ".github/workflows/ci.yml").read_text())

    def test_dependency_producer_ignores_ambient_import_and_executable_interposition(self):
        # Execute the real workflow command/shell. The trusted wrapper only
        # bounds the real producer's API wait; it does not supply isolation.
        import shlex
        import shutil
        import subprocess
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        step = next(s for s in self.document["jobs"]["ci"]["steps"]
                    if s.get("name") == "Await the test build and nextest shards")
        with tempfile.TemporaryDirectory(dir=scratch, prefix="producer-interposition-") as directory:
            root = Path(directory)
            trusted = root / step.get("working-directory", ".")
            (trusted / "scripts").mkdir(parents=True)
            reached = root / "trusted-invocation"
            stub = (
                f"from pathlib import Path\nPath({str(reached)!r}).write_text('reached')\n"
                "import runpy, sys\nsys.argv += ['--timeout-seconds', '1']\n"
                f"runpy.run_path({str(ROOT / 'scripts/ci-await-jobs.py')!r}, run_name='__main__')\n")
            (trusted / "scripts/ci-await-jobs.py").write_text(stub)
            git_env = dict(os.environ, HOME=str(root), GIT_CONFIG_GLOBAL="/dev/null",
                           GIT_CONFIG_NOSYSTEM="1")

            def git(*args):
                return subprocess.run(
                    ["git", *args], cwd=trusted, env=git_env,
                    capture_output=True, text=True, timeout=10, check=True).stdout.strip()

            git("init")
            git("config", "user.email", "fixture@example.invalid")
            git("config", "user.name", "Fixture")
            git("add", "scripts/ci-await-jobs.py")
            git("commit", "-m", "trusted producer")
            base = git("rev-parse", "HEAD")
            trusted_bytes = root / "trusted-source.py"
            trusted_bytes.write_text(stub)
            command = root / "step"
            command.write_text(step["run"])
            marker = root / "interposed"
            poison = root / "poison"
            poison.mkdir()
            attack = (f"from pathlib import Path\nPath({str(marker)!r}).write_text('interposed')\n"
                      "print('orbit-dependency-evidence:{\"kind\":\"forged\"}')\nraise SystemExit(1)\n")
            for module in ("argparse", "subprocess", "sitecustomize"):
                (poison / (module + ".py")).write_text(attack)
            for executable in ("python3", "gh"):
                file = poison / executable
                file.write_text(f"#!{sys.executable}\n" + attack)
                file.chmod(0o755)
            for case in ("pythonpath", "cwd", "python-executable", "gh-executable",
                         "helper_overwrite", "clean_filter", "replace_object"):
                with self.subTest(case=case):
                    git("update-ref", "-d", "refs/replace/" + base)
                    (trusted / ".gitattributes").unlink(missing_ok=True)
                    (trusted / "scripts/ci-await-jobs.py").write_text(stub)
                    git("add", "scripts/ci-await-jobs.py")
                    for module in ("argparse", "subprocess", "sitecustomize"):
                        (trusted / "scripts" / (module + ".py")).unlink(missing_ok=True)
                    reached.unlink(missing_ok=True)
                    if marker.exists():
                        marker.unlink()
                    if case in ("helper_overwrite", "clean_filter", "replace_object"):
                        # An earlier candidate step overwrites the trusted
                        # checkout with ordinary workspace writes before launch.
                        (trusted / "scripts/ci-await-jobs.py").write_text(attack)
                    if case == "clean_filter":
                        # A clean filter can hash the trusted bytes while
                        # runpy still executes the overwritten file.
                        (trusted / ".gitattributes").write_text(
                            "scripts/ci-await-jobs.py filter=trusted\n")
                        git("config", "filter.trusted.clean", "cat " + shlex.quote(str(trusted_bytes)))
                    if case == "replace_object":
                        # HEAD's spelling stays the trusted base, but replace
                        # refs redirect its tree to the overwritten producer.
                        git("add", "scripts/ci-await-jobs.py")
                        tree = git("write-tree")
                        replacement = git("commit-tree", tree, "-p", base, "-m", "replacement")
                        git("replace", base, replacement)
                    path = os.environ["PATH"]
                    if case == "python-executable":
                        path = str(poison) + ":" + path
                    if case == "gh-executable":
                        gh_only = root / "gh-only"
                        gh_only.mkdir(exist_ok=True)
                        shutil.copy2(poison / "gh", gh_only / "gh")
                        path = str(gh_only) + ":" + path
                    if case == "cwd":
                        for module in ("argparse", "subprocess", "sitecustomize"):
                            shutil.copy2(poison / (module + ".py"), trusted / "scripts" / (module + ".py"))
                    env = dict(PATH=path, HOME=str(root), GH_CONFIG_DIR=str(root / "config"),
                               GITHUB_REPOSITORY="fixture/orbit", GITHUB_RUN_ID="7",
                               GITHUB_RUN_ATTEMPT="1", GITHUB_SHA="a" * 40,
                               GITHUB_JOB="ci", GITHUB_WORKFLOW_REF="fixture/orbit/.github/workflows/ci.yml@refs/pull/1/merge",
                               NEXTEST_SHARDS="3")
                    env.update(step.get("env", {}))
                    env["ORBIT_DEPENDENCY_PRODUCER_SHA"] = base
                    # No credentials: authentic gh refuses locally, never querying GitHub.
                    env.pop("GH_TOKEN", None)
                    if case == "pythonpath":
                        env["PYTHONPATH"] = str(poison)
                    shell = step.get("shell", "bash --noprofile --norc -e -o pipefail {0}")
                    result = self_test_hang.run(
                        [word.replace("{0}", str(command)) for word in shlex.split(shell)],
                        step="dependency producer ambient " + case, cwd=trusted, env=env,
                        text=True, capture_output=True)
                    if case in ("helper_overwrite", "clean_filter", "replace_object"):
                        self.assertFalse(reached.exists(), result.stdout + result.stderr)
                    else:
                        self.assertTrue(reached.exists(), result.stdout + result.stderr)
                    self.assertFalse(marker.exists(), result.stdout + result.stderr)
                    self.assertNotIn("orbit-dependency-evidence:", result.stdout)
                    self.assertNotEqual(result.returncode, 0)
                    for module in ("argparse", "subprocess", "sitecustomize"):
                        (trusted / "scripts" / (module + ".py")).unlink(missing_ok=True)

    def test_doctest_step_accepts_a_candidate_with_the_previous_guard_cli(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch, prefix="workflow-doctests-") as directory:
            root = Path(directory)
            scripts = root / "scripts"
            scripts.mkdir()
            # The workflow may advance while a PR head still has the previous
            # guard interface. Execute the actual step against that boundary.
            guard = scripts / "ci-guardrails.sh"
            guard.write_text("#!/bin/sh\necho 'old candidate guard: unsupported mode' >&2\nexit 2\n")
            guard.chmod(0o755)
            lint = scripts / "with-test-build-lint.sh"
            lint.write_bytes((ROOT / "scripts/with-test-build-lint.sh").read_bytes())
            lint.chmod(0o755)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            cargo = bin_dir / "cargo"
            cargo.write_text(f"#!{sys.executable}\nimport json, os, sys\n"
                             "print(json.dumps(sys.argv[1:]))\n"
                             "raise SystemExit(int(os.environ['DOC_EXIT']))\n")
            cargo.chmod(0o755)
            step = next(step for step in self.document["jobs"]["test-build"]["steps"]
                        if step.get("name") == "Run workspace doctests")
            for status in (0, 9):
                with self.subTest(status=status):
                    env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}", DOC_EXIT=str(status))
                    result = self_test_hang.run(
                        ["bash", "-euo", "pipefail", "-c", step["run"]],
                        step="old-candidate doctest workflow", cwd=root, env=env,
                        text=True, capture_output=True)
                    self.assertEqual(result.returncode, status, result.stdout + result.stderr)
                    self.assertEqual(json.loads(result.stdout),
                                     ["test", "--no-fail-fast", "--workspace", "--doc"])

    def test_every_pr_head_job_routes_orchestration_to_the_workflow_revision(self):
        for path in sorted((ROOT / ".github/workflows").glob("*.y*ml")):
            with self.subTest(workflow=path.name):
                self.assertEqual(helper_revision_errors(yaml.safe_load(path.read_text())), [])

    def test_guard_rejects_candidate_calls_wrong_revision_and_late_checkout(self):
        # Mutate parsed workflows, then execute the same ownership guard used
        # above. Each current call must be protected, not just the wait helper.
        checked = 0
        for job_name, job in self.document["jobs"].items():
            if not any("pull_request.head.sha" in step.get("with", {}).get("ref", "")
                       for step in job.get("steps", [])):
                continue
            for index, step in enumerate(job["steps"]):
                for call in set(HELPER_CALL.findall(step.get("run", ""))):
                    if Path(call).name == "ci-guardrails.sh":
                        continue
                    candidate = copy.deepcopy(self.document)
                    candidate["jobs"][job_name]["steps"][index]["run"] = step["run"].replace(
                        call, "scripts/" + Path(call).name)
                    if step.get("working-directory"):
                        candidate["jobs"][job_name]["steps"][index]["working-directory"] = "."
                    with self.subTest(job=job_name, call=call):
                        self.assertTrue(helper_revision_errors(candidate), "PR-head helper call must fail")
                    checked += 1
            helper_index = next((i for i, step in enumerate(job["steps"])
                                 if step.get("with", {}).get("path") == ".orbit/tmp/workflow-ci"), None)
            if helper_index is not None:
                for mode in ("wrong-ref", "late", "missing"):
                    candidate = copy.deepcopy(self.document)
                    steps = candidate["jobs"][job_name]["steps"]
                    helper = steps[helper_index]
                    if mode == "wrong-ref":
                        helper["with"]["ref"] = "${{ github.event.pull_request.head.sha }}"
                    else:
                        steps.pop(helper_index)
                        if mode == "late":
                            steps.append(helper)
                    with self.subTest(job=job_name, mutation=mode):
                        self.assertTrue(helper_revision_errors(candidate), "helper must precede its calls")
        self.assertGreater(checked, 0, "the ownership guard must exercise orchestration calls")

    def test_old_pr_head_executes_new_wait_cli_without_changing_candidate_code(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch, prefix="workflow-revisions-") as directory:
            root = Path(directory)
            source = root / "source"
            source.mkdir()

            def run(args, cwd=source, *, check=True, env=None):
                return self_test_hang.run(args, step="two-revision workflow fixture", cwd=cwd,
                                          check=check, env=env, text=True, capture_output=True)

            run(["git", "init", "-q"])
            run(["git", "config", "user.name", "CI fixture"])
            run(["git", "config", "user.email", "ci@example.invalid"])
            scripts = source / "scripts"
            scripts.mkdir()
            # An old public CLI rejects --step. Without it the in-progress
            # build would wait for the whole job; fail immediately in this fixture.
            (scripts / "ci-await-jobs.py").write_text(
                'import argparse\np = argparse.ArgumentParser()\n'
                'p.add_argument("--job", action="append", required=True)\np.parse_args()\n'
                'raise SystemExit("old helper waits for the still-running build")\n')
            (source / "candidate.py").write_text('print("old PR code")\n')
            run(["git", "add", "."])
            run(["git", "commit", "-qm", "old PR head"])
            old = run(["git", "rev-parse", "HEAD"]).stdout.strip()
            (scripts / "ci-await-jobs.py").write_bytes((ROOT / "scripts/ci-await-jobs.py").read_bytes())
            (scripts / "ci-download-verified.sh").write_bytes((ROOT / "scripts/ci-download-verified.sh").read_bytes())
            (source / "candidate.py").write_text('raise SystemExit("workflow code must not run")\n')
            run(["git", "add", "."])
            run(["git", "commit", "-qm", "workflow and new helper interface"])
            workflow = run(["git", "rev-parse", "HEAD"]).stdout.strip()
            bin_dir = root / "bin"
            bin_dir.mkdir()
            gh = bin_dir / "gh"
            # Transport fixture: the last upload succeeded but the build is
            # still running, so success requires the real helper's --step behavior.
            upload = [step["name"] for step in self.document["jobs"]["test-build"]["steps"]
                      if step.get("name", "").startswith("Upload nextest archive")][-1]
            job = {"name": "Test build", "status": "in_progress", "conclusion": None,
                   "steps": [{"name": upload, "status": "completed", "conclusion": "success"}]}
            gh.write_text(f"#!{sys.executable}\nimport sys\n"
                          f"print('HTTP/2.0 200 OK\\n\\n' + {json.dumps({'total_count': 1, 'jobs': [job]})!r} "
                          f"if '--include' in sys.argv else {json.dumps(job)!r})\n")
            gh.chmod(0o755)
            env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}",
                       GITHUB_REPOSITORY="fixture/repository", GITHUB_RUN_ID="1")
            bindings = {WORKFLOW_REF: workflow, PRODUCER_REF: workflow,
                        "${{ github.event_name == 'pull_request' && github.event.pull_request.head.sha || github.sha }}": old}
            for job_name in ("docs", "ci", "test-build", "test-1", "test-2", "test-3"):
                with self.subTest(job=job_name):
                    checkout = root / job_name
                    run(["git", "clone", "-q", "--no-checkout", str(source), str(checkout)])
                    for step in self.document["jobs"][job_name]["steps"]:
                        if not step.get("uses", "").startswith("actions/checkout@"):
                            continue
                        options = step.get("with", {})
                        target = checkout / options.get("path", "")
                        if target != checkout:
                            target.parent.mkdir(parents=True, exist_ok=True)
                            run(["git", "clone", "-q", "--no-checkout", str(source), str(target)])
                        patterns = options.get("sparse-checkout")
                        if patterns:
                            self.assertIs(options.get("sparse-checkout-cone-mode"), False)
                            self_test_hang.run(
                                ["git", "sparse-checkout", "set", "--no-cone", "--stdin"],
                                step="sparse workflow helper checkout", cwd=target, input=patterns, stdin=subprocess.PIPE,
                                check=True, text=True, capture_output=True)
                        run(["git", "checkout", "--force", "--detach", "-q", bindings[options["ref"]]], cwd=target)
                    if job_name.startswith("test-") and job_name != "test-build":
                        step = next(s for s in self.document["jobs"][job_name]["steps"]
                                    if s.get("name") == "Await the nextest archive")
                        result = run(["bash", "-euo", "pipefail", "-c", step["run"]],
                                     cwd=checkout, check=False, env=env)
                        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertIn(f"passed step '{upload}'", result.stdout)
                        negative = run([sys.executable, "scripts/ci-await-jobs.py", "--job", "Test build",
                                        "--step", upload], cwd=checkout, check=False)
                        self.assertEqual(negative.returncode, 2, negative.stderr)
                        self.assertIn("unrecognized arguments: --step", negative.stderr)
                    self.assertEqual(run(["git", "rev-parse", "HEAD"], cwd=checkout).stdout.strip(), old)
                    self.assertEqual(run([sys.executable, "candidate.py"], cwd=checkout).stdout.strip(),
                                     "old PR code", "the build/test checkout must remain at the PR head")
                    helper_checkout = checkout / ".orbit/tmp/workflow-ci"
                    self.assertTrue(helper_checkout.is_dir(), "workflow helper checkout must exist")
                    self.assertEqual(run(["git", "rev-parse", "HEAD"], cwd=helper_checkout).stdout.strip(), workflow)
                    self.assertFalse((helper_checkout / "candidate.py").exists(), "helper checkout is sparse")
