#!/usr/bin/env python3
"""Execute release metadata and Homebrew scripts offline with hostile inputs."""

import base64
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
WORKFLOW = ROOT / ".github/workflows/release.yml"


@unittest.skipIf(yaml is None, "PyYAML not installed; the workflow guard skips locally without it")
class ReleaseWorkflowTests(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch, prefix="release-workflow-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.env = dict(PATH=os.environ["PATH"], LANG="C", TMPDIR=str(self.root),
                        GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)
        self.jobs = yaml.safe_load(WORKFLOW.read_text())["jobs"]
        self.metadata = next(step for step in self.jobs["publish-release"]["steps"]
                             if step.get("id") == "metadata")
        self.tap = self.jobs["bump-homebrew-tap"]
        self.tap_runs = [step for step in self.tap["steps"] if "run" in step]
        self.outputs = dict(version="1.2.3-rc.1",
                            darwin_arm_asset="orbit-aarch64-apple-darwin.tar.gz",
                            darwin_arm_sha="a" * 64,
                            darwin_intel_asset="orbit-x86_64-apple-darwin.tar.gz",
                            darwin_intel_sha="b" * 64)
        dist = self.root / "dist"
        dist.mkdir()
        (dist / "orbit-checksums.txt").write_text(
            f"{'a' * 64}  {self.outputs['darwin_arm_asset']}\n"
            f"{'b' * 64}  {self.outputs['darwin_intel_asset']}\n")
        self.output = self.root / "output"

    def run_script(self, step, **values):
        # Resolve the actual step env bindings, as Actions does, without ever
        # substituting expressions into Bash source.
        env = dict(self.env)
        for key, value in step.get("env", {}).items():
            output = re.fullmatch(r"\$\{\{ needs\.publish-release\.outputs\.(\w+) \}\}", value)
            if output:
                env[key] = self.outputs[output[1]]
            elif value == "${{ secrets.TAP_GITHUB_TOKEN }}":
                env[key] = "offline-test-token"
            else:
                self.fail(f"unsupported workflow env binding: {key}")
        env.update(values)
        return subprocess.run(["bash", "-e", "-o", "pipefail", "-c", step["run"]],
                              cwd=self.root, env=env, text=True, capture_output=True, timeout=10)

    def git(self, *arguments):
        result = subprocess.run(["git", *arguments], cwd=self.root, env=self.env,
                                text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip()

    def test_metadata_accepts_release_and_prerelease_tags(self):
        for tag in ("v0.0.0", "v12.34.56", "v1.2.3-rc.1", "v1.2.3-alpha-beta.2"):
            with self.subTest(tag=tag):
                self.output.write_text("")
                result = self.run_script(self.metadata, GITHUB_REF_NAME=tag,
                                         GITHUB_OUTPUT=str(self.output))
                self.assertEqual(result.returncode, 0, result.stderr)
                outputs = dict(line.split("=", 1) for line in self.output.read_text().splitlines())
                self.assertEqual(outputs, dict(self.outputs, version=tag[1:]))

    def test_metadata_rejects_hostile_and_nonrelease_tags_before_outputs(self):
        for tag in ("1.2.3", "v1.2", "v1.2.3.4", "v1.2.3-", "v1.2.3-rc..1",
                    "v1.2.3+build", "v1.2.3\nversion=evil", 'v1.2.3$(touch sentinel)',
                    'v1.2.3`touch sentinel`', 'v1.2.3";touch sentinel;#',
                    'v1.2.3$(touch${IFS}sentinel)', 'v1.2.3`touch${IFS}sentinel`'):
            with self.subTest(tag=tag):
                if "${IFS}" in tag:
                    self.git("check-ref-format", f"refs/tags/{tag}")
                self.output.write_text("")
                result = self.run_script(self.metadata, GITHUB_REF_NAME=tag,
                                         GITHUB_OUTPUT=str(self.output))
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.output.read_text(), "",
                                 "invalid tags must not publish metadata to downstream jobs")
                self.assertFalse((self.root / "sentinel").exists(),
                                 "tag data must never execute as shell source")

    def test_formula_uses_metadata_and_rejects_ruby_and_shell_payloads(self):
        result = self.run_script(self.tap_runs[0])
        self.assertEqual(result.returncode, 0, result.stderr)
        formula = self.root / "tap/Formula/orbit.rb"
        generated = formula.read_text()
        self.assertIn(f'version "{self.outputs["version"]}"', generated)
        for arch in ("arm", "intel"):
            asset = self.outputs[f"darwin_{arch}_asset"]
            self.assertIn(f'/v{self.outputs["version"]}/{asset}"', generated)
            self.assertIn(f'sha256 "{self.outputs[f"darwin_{arch}_sha"]}"', generated)

        for field in self.outputs:
            for payload in ('$(touch sentinel)', '`touch sentinel`', '";system("touch sentinel");#',
                            '#{system("touch sentinel")}', "value\ninjected"):
                with self.subTest(field=field, payload=payload):
                    original = self.outputs[field]
                    self.outputs[field] = payload
                    try:
                        result = self.run_script(self.tap_runs[0])
                    finally:
                        self.outputs[field] = original
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(formula.read_text(), generated,
                                     "unsafe metadata must not overwrite the Ruby formula")
                    self.assertFalse((self.root / "sentinel").exists())

    def test_tap_push_authentication_is_ephemeral_and_commit_message_is_data(self):
        checkout = next(step for step in self.tap["steps"] if "uses" in step)
        self.assertIs(checkout["with"].get("persist-credentials"), False,
                      "tap checkout must not persist its token for formula-generation steps")
        self.git("init", "--initial-branch=main", "tap")
        self.git("init", "--bare", "remote.git")
        self.git("-C", "tap", "remote", "add", "origin", str(self.root / "remote.git"))
        self.git("-C", "tap", "config", "push.default", "current")
        result = self.run_script(self.tap_runs[0])
        self.assertEqual(result.returncode, 0, result.stderr)
        # A real pre-push hook observes the one-command Git config; the
        # remote is local so no credential or network access is required.
        hook = self.root / "tap/.git/hooks/pre-push"
        hook.write_text('#!/bin/sh\nset -eu\n'
                        'git config --get http.https://github.com/.extraheader > ../push-header\n')
        hook.chmod(0o755)
        hostile_version = '1.2.3$(touch sentinel)`touch sentinel`"'
        self.outputs["version"] = hostile_version
        result = self.run_script(self.tap_runs[1])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git("--git-dir=remote.git", "log", "main", "-1", "--format=%s"),
                         f"orbit v{hostile_version}")
        self.assertFalse((self.root / "tap/sentinel").exists(),
                         "commit messages must treat shell metacharacters as literal data")
        authorization = base64.b64encode(b"x-access-token:offline-test-token").decode()
        self.assertEqual((self.root / "push-header").read_text().strip(),
                         f"AUTHORIZATION: basic {authorization}")
        config = self.git("-C", "tap", "config", "--local", "--list")
        self.assertNotIn(authorization, config,
                         "push credentials must not persist in tap/.git/config")
        self.assertNotIn("offline-test-token", config)
        self.assertNotIn("extraheader", config)
        before = self.git("-C", "tap", "rev-parse", "HEAD")
        result = self.run_script(self.tap_runs[1])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git("-C", "tap", "rev-parse", "HEAD"), before)


if __name__ == "__main__":
    unittest.main()
