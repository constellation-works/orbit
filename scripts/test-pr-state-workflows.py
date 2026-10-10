#!/usr/bin/env python3
"""Execute PR-state gates offline and evaluate their workflow job conditions."""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

try:
    import yaml
except ImportError:
    yaml = None


ROOT = Path(__file__).resolve().parent.parent
RESPONSES = json.loads((ROOT / "scripts/fixtures/pr-state-responses.json").read_text())["responses"]


def evaluate(expression, event, state="", *, cancelled=False, dependency_skipped=False):
    """Evaluate the small expression subset used by these admission conditions.

    This is an offline condition fixture, not a GitHub Actions scheduler. The
    skipped-dependency rule still needs a status function to override implicit
    success(); model it for the non-PR case where the gate itself is skipped.
    """
    expression = expression.removeprefix("${{").removesuffix("}}").strip()
    if dependency_skipped and not re.search(r"(?:cancelled|always)\(\)", expression):
        return False
    expression = expression.replace("github.event_name", repr(event))
    expression = expression.replace("needs.pr-state.outputs.state", repr(state))
    expression = expression.replace("cancelled()", str(cancelled)).replace("always()", "True")
    expression = expression.replace("&&", " and ").replace("||", " or ")
    expression = re.sub(r"!(?!=)", " not ", expression)
    return bool(eval(expression.strip(), {"__builtins__": {}}, {}))


@unittest.skipIf(yaml is None, "PyYAML not installed; the workflow guard skips locally without it")
class PrStateWorkflowTests(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch, prefix="pr-state-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.output = self.root / "output"
        self.response = self.root / "response.json"
        self.request = self.root / "request.json"
        # Only transport is stubbed: the workflow's real Bash and jq execute.
        gh = self.root / "gh"
        gh.write_text("""#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
Path(os.environ["FIXTURE_REQUEST"]).write_text(json.dumps({
    "args": sys.argv[1:], "token": os.environ.get("GH_TOKEN")
}))
if os.environ.get("FIXTURE_API_FAILURE") == "1":
    sys.stderr.write("fixture GitHub API unavailable\\n")
    sys.exit(1)
sys.stdout.write(Path(os.environ["FIXTURE_RESPONSE"]).read_text())
""")
        gh.chmod(0o755)
        self.workflows = {}
        for path in sorted((ROOT / ".github/workflows").glob("*.y*ml")):
            document = yaml.safe_load(path.read_text())
            # YAML 1.1 reads the unquoted Actions key `on` as True.
            if "pull_request" in document.get("on", document.get(True, {})):
                self.workflows[path.name] = document
        self.assertTrue(self.workflows, "the fixture must exercise PR workflows")

    def run_gate(self, gate, response, *, api_failure=False):
        self.output.write_text("")
        self.response.write_text(json.dumps(response))
        self.request.write_text("{}")
        step = gate["steps"][0]
        bindings = {"${{ github.token }}": "offline-token",
                    "${{ github.event.pull_request.number }}": "42"}
        env = dict(PATH=f"{self.root}:{os.environ['PATH']}", LANG="C",
                   GITHUB_REPOSITORY="fixture/repository", GITHUB_OUTPUT=str(self.output),
                   FIXTURE_RESPONSE=str(self.response), FIXTURE_REQUEST=str(self.request),
                   FIXTURE_API_FAILURE="1" if api_failure else "0")
        env.update({key: bindings.get(value, value) for key, value in step["env"].items()})
        result = subprocess.run([step["shell"], "-e", "-o", "pipefail", "-c", step["run"]],
                                cwd=self.root, env=env, text=True, capture_output=True, timeout=10)
        outputs = dict(line.split("=", 1) for line in self.output.read_text().splitlines())
        # Follow the actual step-to-job output binding consumed by needs.
        binding = gate["outputs"]["state"]
        match = re.fullmatch(r"\$\{\{ steps\.([\w-]+)\.outputs\.([\w-]+) \}\}", binding)
        self.assertIsNotNone(match, "gate must publish its actual step output")
        self.assertEqual(match[1], step["id"])
        return result, outputs.get(match[2], "")

    def assert_admission(self, document, event, state, expected, *, cancelled=False):
        for name, job in document["jobs"].items():
            if name == "pr-state":
                continue
            with self.subTest(job=name, event=event, state=state, cancelled=cancelled):
                needs = job.get("needs", [])
                if isinstance(needs, str):
                    needs = [needs]
                self.assertIn("pr-state", needs, "build jobs must wait for live PR state")
                self.assertEqual(evaluate(job["if"], event, state, cancelled=cancelled,
                                          dependency_skipped=event != "pull_request"), expected)

    def test_live_states_admit_open_and_skip_merged_or_closed_jobs(self):
        for name, document in self.workflows.items():
            gate = document["jobs"]["pr-state"]
            with self.subTest(workflow=name):
                self.assertTrue(evaluate(gate["if"], "pull_request"))
                self.assertEqual(gate["permissions"], {"pull-requests": "read"})
                self.assertEqual(gate["runs-on"], "ubuntu-latest")
                self.assertLessEqual(gate["timeout-minutes"], 2)
                self.assertEqual(len(gate["steps"]), 1, "gate must run without checking out PR code")
            for state, response in RESPONSES.items():
                with self.subTest(workflow=name, response=state):
                    result, output = self.run_gate(gate, response)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(output, state)
                    self.assertEqual(json.loads(self.request.read_text()), {
                        "args": ["api", "repos/fixture/repository/pulls/42"], "token": "offline-token"})
                    self.assert_admission(document, "pull_request", output, state == "open")
                    print(f"{name}: live {state} -> build jobs {'admitted' if state == 'open' else 'skipped'}")

    def test_non_pr_events_skip_gate_and_admit_existing_jobs(self):
        for name, document in self.workflows.items():
            for event in document.get("on", document.get(True, {})):
                if event == "pull_request":
                    continue
                with self.subTest(workflow=name, event=event):
                    self.assertFalse(evaluate(document["jobs"]["pr-state"]["if"], event))
                    self.assert_admission(document, event, "", True)
                    self.assert_admission(document, event, "", False, cancelled=True)
                    print(f"{name}: {event} -> gate skipped, build jobs admitted")

    def test_api_errors_fail_gate_without_admitting_unknown_state(self):
        for name, document in self.workflows.items():
            for response, api_failure in ((RESPONSES["open"], True), ({}, False)):
                with self.subTest(workflow=name, response=response, api_failure=api_failure):
                    result, output = self.run_gate(document["jobs"]["pr-state"], response,
                                                   api_failure=api_failure)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(output, "")
                    self.assert_admission(document, "pull_request", output, False)


if __name__ == "__main__":
    unittest.main()
