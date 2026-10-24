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


def helper_revision_errors(document):
    """Resolve each orchestration call to its preceding checkout's revision.

    Candidate validators belong to the tested code. Every other ci-* helper
    orchestrates the workflow and must follow its revision, even on an old PR.
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
                if Path(call).name == "ci-guardrails.sh":
                    continue
                call = call.removeprefix("./")
                owners = [path for path in checkouts if not path or call.startswith(path + "/")]
                owner = max(owners, key=len) if owners else None
                if owner is None or checkouts[owner] != WORKFLOW_REF:
                    errors.append(f"{name}: {step.get('name', 'run')}: {call} must use a preceding "
                                  "checkout of github.sha (protects workflow/helper revision agreement)")
    return errors


@unittest.skipIf(yaml is None, "PyYAML is required to execute workflow controls")
class WorkflowHelperRevisionTests(unittest.TestCase):
    def setUp(self):
        self.document = yaml.safe_load((ROOT / ".github/workflows/ci.yml").read_text())

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
            gh.write_text(f"#!{sys.executable}\nprint({json.dumps(job)!r})\n")
            gh.chmod(0o755)
            env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}",
                       GITHUB_REPOSITORY="fixture/repository", GITHUB_RUN_ID="1")
            bindings = {WORKFLOW_REF: workflow,
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
