#!/usr/bin/env python3
"""Run the documented release promotion and hotfix back-port against isolated Git repositories.

The fixture origin takes changes to `main` only through a simulated pull request
merge, as the repository ruleset does, and refuses non-fast-forward updates. Each
test executes the documented shell block from RELEASING.md and checks history and
file contents.
"""

import contextlib
import os
from pathlib import Path
import re
import self_test_hang
import shlex
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parent.parent
RELEASING = ROOT / "RELEASING.md"
ZERO = "0000000000000000000000000000000000000000"
HOTFIX = "hotfix-payload-9f3c1a\n"
DEVELOPMENT = "development-payload\n"
SHELL_FENCE = re.compile(r"^[ \t]*```(sh|bash|shell)\n(.*?)^[ \t]*```[ \t]*$", re.M | re.S)


def hotfix_section(text):
    marker = "## Hotfix flow"
    start = text.find(marker)
    if start < 0:
        raise AssertionError("release document has no hotfix flow section")
    rest = text[start + len(marker):]
    following = re.search(r"(?m)^## ", rest)
    return rest[:following.start()] if following else rest


def section(text, heading, level):
    """Return the body of the Markdown section that starts at `heading`."""
    start = text.find(heading)
    if start < 0:
        raise AssertionError(f"release document has no section {heading!r}")
    rest = text[start + len(heading):]
    following = re.search(rf"(?m)^#{{1,{level}}} ", rest)
    return rest[:following.start()] if following else rest


def command_lines(block):
    lines = []
    for raw in textwrap.dedent(block).splitlines():
        line = raw.strip()
        if line and not line.startswith("#"):
            lines.append(line)
    return lines


def fenced_block(body, marker, label):
    """Return the one shell block in `body` whose command lines contain `marker`."""
    chosen = []
    for match in SHELL_FENCE.finditer(body):
        block = match.group(2)
        if any(marker in line for line in command_lines(block)):
            chosen.append(textwrap.dedent(block).strip() + "\n")
    if len(chosen) != 1:
        raise AssertionError(f"expected one {label} shell block, found {len(chosen)}")
    return chosen[0]


def promotion_script(text, version):
    """Return the 10b gate block with the release version filled in.

    The block is the fenced shell under the promote section that pushes the
    promotion branch. Callers assert repository state, not the block's wording.
    """
    body = section(text, "### 10b. Promote to `main`", 3)
    return fenced_block(body, "refs/heads/promote/", "promotion gate").replace("<X.Y.Z>", version)


def postcheck_script(text, version):
    """Return the 10c check block with the release version filled in."""
    body = section(text, "### 10c. Confirm `main` and `agent-main` agree", 3)
    return fenced_block(body, "origin/main^2", "post-promotion check").replace("<X.Y.Z>", version)


def backport_script(text, slug):
    """Return the hotfix back-port block with the hotfix slug filled in."""
    return fenced_block(
        hotfix_section(text), "git merge --squash origin/main", "hotfix back-port"
    ).replace("<slug>", slug)


class GitFixtureCase(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch, prefix="release-promotion-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        home = self.root / "home"
        home.mkdir()
        global_config = home / "gitconfig"
        global_config.write_text("")
        self.push_log_path = self.root / "push.log"
        self.env = {
            "PATH": os.environ["PATH"],
            "HOME": str(home),
            "TMPDIR": str(self.root),
            "LANG": "C",
            "LC_ALL": "C",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": str(global_config),
            "GIT_CONFIG_COUNT": "6",
            "GIT_CONFIG_KEY_0": "safe.directory",
            "GIT_CONFIG_VALUE_0": "*",
            "GIT_CONFIG_KEY_1": "commit.gpgsign",
            "GIT_CONFIG_VALUE_1": "false",
            "GIT_CONFIG_KEY_2": "tag.gpgsign",
            "GIT_CONFIG_VALUE_2": "false",
            "GIT_CONFIG_KEY_3": "init.defaultBranch",
            "GIT_CONFIG_VALUE_3": "main",
            "GIT_CONFIG_KEY_4": "advice.detachedHead",
            "GIT_CONFIG_VALUE_4": "false",
            "GIT_CONFIG_KEY_5": "protocol.file.allow",
            "GIT_CONFIG_VALUE_5": "always",
            "GIT_AUTHOR_NAME": "Orbit Test",
            "GIT_AUTHOR_EMAIL": "orbit-test@example.com",
            "GIT_COMMITTER_NAME": "Orbit Test",
            "GIT_COMMITTER_EMAIL": "orbit-test@example.com",
            "GIT_MERGE_AUTOEDIT": "no",
            "GIT_EDITOR": "true",
            "GIT_SEQUENCE_EDITOR": "true",
            "GIT_TERMINAL_PROMPT": "0",
            "GIT_CEILING_DIRECTORIES": str(self.root),
        }

    def git(self, repo, *args, check=True):
        result = self_test_hang.run(
            ["git", "-C", str(repo), *args],
            step="git",
            env=self.env,
            text=True,
            capture_output=True,
        )
        if check and result.returncode != 0:
            self.fail(
                f"git -C {repo.name} {shlex.join(args)} -> {result.returncode}\n"
                f"{result.stderr}{result.stdout}"
            )
        return result

    def run_git(self, *args, check=True):
        result = self_test_hang.run(
            ["git", *args],
            step="git",
            cwd=self.root,
            env=self.env,
            text=True,
            capture_output=True,
        )
        if check and result.returncode != 0:
            self.fail(
                f"git {shlex.join(args)} -> {result.returncode}\n{result.stderr}{result.stdout}"
            )
        return result

    def rev(self, repo, ref):
        return self.git(repo, "rev-parse", ref).stdout.strip()

    def show(self, repo, spec):
        return self.git(repo, "show", spec).stdout

    def push_log(self):
        if not self.push_log_path.exists():
            return []
        return self.push_log_path.read_text().splitlines()

    def write_hook(self, origin):
        """Log every ref update; refuse non-fast-forward updates and direct updates to main.

        Main accepts updates only while the `allow-main` marker exists, which
        stands for the pull request merge that the release operator performs.
        """
        hook = origin / "hooks" / "pre-receive"
        log = shlex.quote(str(self.push_log_path))
        allow = shlex.quote(str(origin / "allow-main"))
        hook.write_text(
            "#!/bin/sh\n"
            "set -eu\n"
            f"log={log}\n"
            f"allow={allow}\n"
            "while read -r old new ref; do\n"
            "  printf '%s %s %s\\n' \"$old\" \"$new\" \"$ref\" >> \"$log\"\n"
            "  if [ \"$ref\" = refs/heads/main ] && [ ! -e \"$allow\" ]; then\n"
            "    echo 'main takes changes only through pull requests' >&2\n"
            "    exit 1\n"
            "  fi\n"
            "done\n"
        )
        hook.chmod(0o755)
        self.git(origin, "config", "receive.denyNonFastForwards", "true")
        self.git(origin, "config", "core.logAllRefUpdates", "true")

    @contextlib.contextmanager
    def main_accepts_pull_requests(self):
        marker = self.origin / "allow-main"
        marker.touch()
        try:
            yield
        finally:
            marker.unlink()

    def commit_file(self, repo, name, content, message):
        (repo / name).write_text(content)
        self.git(repo, "add", "--", name)
        self.git(repo, "commit", "-m", message)

    def describe(self, result):
        return f"returncode {result.returncode}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"


class PromotionFixture(GitFixtureCase):
    VERSION = "1.0.0"

    def build(self):
        """Create origin (main=base, agent-main=base), an operator clone and a second clone."""
        self.origin = self.root / "origin.git"
        self.origin.mkdir()
        self.git(self.origin, "init", "--bare", "--initial-branch=main")
        self.write_hook(self.origin)

        seed = self.root / "seed"
        seed.mkdir()
        self.git(seed, "init", "--initial-branch=main")
        self.commit_file(seed, "base.txt", "base\n", "base")
        self.base = self.rev(seed, "HEAD")
        self.git(seed, "branch", "agent-main")
        self.git(seed, "remote", "add", "origin", str(self.origin))
        with self.main_accepts_pull_requests():
            self.git(seed, "push", "origin", "main", "agent-main")

        self.operator = self.root / "operator"
        self.run_git("clone", str(self.origin), str(self.operator))
        self.git(self.operator, "checkout", "-b", "agent-main", "--track", "origin/agent-main")
        self.other = self.root / "other"
        self.run_git("clone", str(self.origin), str(self.other))
        self.git(self.other, "checkout", "-b", "agent-main", "--track", "origin/agent-main")

    def pr_branch(self, version=None):
        return f"promote/v{version or self.VERSION}"

    def sync_agent_main(self):
        self.git(self.operator, "fetch", "origin")
        self.git(self.operator, "checkout", "agent-main")
        self.git(self.operator, "merge", "--ff-only", "origin/agent-main")

    def tag_release(self, version=None):
        """Commit a release on agent-main, annotate its tag, and push both."""
        version = version or self.VERSION
        self.sync_agent_main()
        self.commit_file(self.operator, "version.txt", f"{version}\n", f"release {version}")
        self.release = self.rev(self.operator, "HEAD")
        self.git(self.operator, "tag", "-a", f"v{version}", "-m", "release")
        self.assertNotEqual(
            self.rev(self.operator, f"v{version}"), self.release, "tag must be annotated"
        )
        self.git(self.operator, "push", "origin", "agent-main")
        self.git(self.operator, "push", "origin", f"v{version}")
        return self.release

    def land_development_after_tag(self):
        self.git(self.other, "pull", "--ff-only", "origin", "agent-main")
        self.commit_file(self.other, "dev.txt", DEVELOPMENT, "development after tag")
        self.development = self.rev(self.other, "HEAD")
        self.git(self.other, "push", "origin", "agent-main")

    def land_hotfix_pull_request(self, path="hotfix.txt", content=HOTFIX):
        """Merge a hotfix branch cut from main into main with a merge commit."""
        self.hotfixes = getattr(self, "hotfixes", 0) + 1
        branch = f"hotfix/test-{self.hotfixes}"
        self.git(self.other, "fetch", "origin")
        self.git(self.other, "checkout", "-B", "hotfix", "origin/main")
        self.commit_file(self.other, path, content, "hotfix")
        self.git(self.other, "push", "origin", f"HEAD:refs/heads/{branch}")
        self.merge_pull_request(branch)
        return self.rev(self.origin, "main^2")

    def merge_pull_request(self, branch):
        """Merge origin/<branch> into main with a merge commit, as the operator does on GitHub."""
        self.git(self.operator, "fetch", "origin")
        self.git(self.operator, "checkout", "-B", "pull-request", "origin/main")
        self.git(self.operator, "merge", "--no-ff", "--no-edit", f"origin/{branch}")
        with self.main_accepts_pull_requests():
            self.git(self.operator, "push", "origin", "HEAD:refs/heads/main")
        return self.rev(self.origin, "main")

    def promote(self, version=None):
        script = promotion_script(RELEASING.read_text(encoding="utf-8"), version or self.VERSION)
        self.log_before = len(self.push_log())
        # No `-e`: the documented block must stop itself, not rely on the shell.
        return self_test_hang.run(
            ["bash", "-o", "pipefail", "-c", script],
            step="release promotion",
            cwd=self.operator,
            env=self.env,
            text=True,
            capture_output=True,
        )

    def postcheck(self, version=None):
        script = postcheck_script(RELEASING.read_text(encoding="utf-8"), version or self.VERSION)
        return self_test_hang.run(
            ["bash", "-o", "pipefail", "-c", script],
            step="post-promotion check",
            cwd=self.operator,
            env=self.env,
            text=True,
            capture_output=True,
        )

    def backport(self, slug="hotfix-test"):
        script = backport_script(RELEASING.read_text(encoding="utf-8"), slug)
        self.log_before = len(self.push_log())
        return self_test_hang.run(
            ["bash", "-e", "-o", "pipefail", "-c", script],
            step="hotfix back-port",
            cwd=self.operator,
            env=self.env,
            text=True,
            capture_output=True,
        )

    def pushes_since_promote(self, ref):
        """Pushes to `ref` received since the last block started."""
        return [line for line in self.push_log()[self.log_before:] if line.endswith(f" {ref}")]

    def main_updates(self):
        return self.pushes_since_promote("refs/heads/main")

    def is_ancestor(self, ancestor, descendant):
        return self.git(
            self.origin, "merge-base", "--is-ancestor", ancestor, descendant, check=False
        ).returncode == 0

    def ref_exists(self, ref):
        return self.git(
            self.origin, "rev-parse", "--verify", "--quiet", ref, check=False
        ).returncode == 0

    def tree_gate_passes(self, branch):
        """The gate's tree test: merging origin/main into `branch` adds nothing `branch` lacks."""
        merged = self.git(self.operator, "merge-tree", "--write-tree", "origin/main", branch, check=False)
        tree = self.git(self.operator, "rev-parse", f"{branch}^{{tree}}").stdout.strip()
        return merged.stdout.splitlines()[:1] == [tree]

    def assert_refused(self, result, branch, main_sha):
        self.assertNotEqual(result.returncode, 0, self.describe(result))
        self.assertEqual(self.rev(self.origin, "main"), main_sha, "remote main must not move")
        self.assertEqual(self.main_updates(), [], "promotion must not push to main")
        self.assertFalse(self.ref_exists(f"refs/heads/{branch}"), f"promotion must not push {branch}")


class ReleasePromotionTests(PromotionFixture):
    def test_later_development_commit_is_not_promoted(self):
        self.build()
        self.tag_release()
        self.land_development_after_tag()

        result = self.promote()
        self.assertEqual(result.returncode, 0, self.describe(result))
        self.assertEqual(self.rev(self.origin, self.pr_branch()), self.release)
        self.assertFalse(self.is_ancestor(self.development, self.pr_branch()), "later development was promoted")
        self.assertEqual(self.main_updates(), [], "the gate must not push main")
        self.assertEqual(self.rev(self.origin, "main"), self.base)

        self.merge_pull_request(self.pr_branch())
        check = self.postcheck()
        self.assertEqual(check.returncode, 0, self.describe(check))
        self.assertIn(f"promoted {self.release}", check.stdout)
        self.assertEqual(self.rev(self.origin, "main^2"), self.release)
        self.assertEqual(self.show(self.origin, "main:version.txt"), f"{self.VERSION}\n")
        self.assertTrue(self.is_ancestor(f"v{self.VERSION}", "main"), "tag must stay reachable from main")
        self.assertFalse(self.is_ancestor(self.development, "main"), "later development reached main")
        self.assertEqual(self.rev(self.origin, "agent-main"), self.development)
        self.assertEqual(self.rev(self.origin, f"v{self.VERSION}^{{commit}}"), self.release)

    def test_promotion_without_later_commits_passes_the_check(self):
        self.build()
        self.tag_release()

        result = self.promote()
        self.assertEqual(result.returncode, 0, self.describe(result))
        self.assertEqual(self.rev(self.origin, self.pr_branch()), self.release)
        self.merge_pull_request(self.pr_branch())
        check = self.postcheck()
        self.assertEqual(check.returncode, 0, self.describe(check))
        self.assertEqual(self.rev(self.origin, "main^2"), self.release)

    def test_divergent_main_hotfix_refuses_promotion(self):
        self.build()
        self.tag_release()
        self.land_development_after_tag()
        hotfix = self.land_hotfix_pull_request()
        main_before = self.rev(self.origin, "main")

        result = self.promote()

        self.assert_refused(result, self.pr_branch(), main_before)
        self.assertEqual(self.show(self.origin, "main:hotfix.txt"), HOTFIX)
        self.assertTrue(self.is_ancestor(hotfix, "main"))

    def test_release_missing_from_agent_main_is_not_promoted(self):
        self.build()
        # The tagged commit exists on origin only through the tag.
        self.commit_file(self.operator, "version.txt", f"{self.VERSION}\n", "release")
        self.git(self.operator, "tag", "-a", f"v{self.VERSION}", "-m", "release")
        self.git(self.operator, "push", "origin", f"v{self.VERSION}")
        self.assertFalse(self.is_ancestor(self.rev(self.operator, "HEAD"), "agent-main"))

        result = self.promote()

        self.assert_refused(result, self.pr_branch(), self.base)

    def test_missing_tag_is_not_promoted(self):
        self.build()
        self.tag_release()
        self.git(self.operator, "tag", "-d", f"v{self.VERSION}")
        self.git(self.origin, "tag", "-d", f"v{self.VERSION}")

        result = self.promote()

        self.assert_refused(result, self.pr_branch(), self.base)

    def test_postcheck_refuses_main_with_a_commit_the_release_lacks(self):
        self.build()
        self.tag_release()
        self.assertEqual(self.promote().returncode, 0)
        self.merge_pull_request(self.pr_branch())
        self.land_hotfix_pull_request()

        check = self.postcheck()

        self.assertNotEqual(check.returncode, 0, self.describe(check))
        self.assertNotIn("promoted ", check.stdout)


class HotfixBackportTests(PromotionFixture):
    def test_backport_fetches_remote_main_and_lands_on_agent_main(self):
        self.build()
        self.tag_release()
        self.land_development_after_tag()
        hotfix = self.land_hotfix_pull_request()
        self.git(self.operator, "fetch", "origin")
        self.assertFalse(self.tree_gate_passes("origin/agent-main"), "gate must refuse before the back-port")
        # A stale remote-tracking main, as a clone from before the hotfix would have.
        self.git(self.operator, "update-ref", "refs/remotes/origin/main", self.base)

        result = self.backport()

        self.assertEqual(result.returncode, 0, self.describe(result))
        self.assertEqual(self.show(self.origin, "backport/hotfix-test:hotfix.txt"), HOTFIX)
        self.assertEqual(self.show(self.origin, "backport/hotfix-test:dev.txt"), DEVELOPMENT)
        self.assertEqual(self.main_updates(), [], "the back-port must not push main")
        self.assertEqual(self.pushes_since_promote("refs/heads/agent-main"), [], "the back-port must not push agent-main")
        self.assertTrue(self.is_ancestor(hotfix, "main"))

        # Landing the back-port as a PR merge is a fast-forward of agent-main.
        self.git(self.operator, "push", "origin", "backport/hotfix-test:refs/heads/agent-main")
        self.git(self.operator, "fetch", "origin")
        self.assertEqual(self.show(self.origin, "agent-main:hotfix.txt"), HOTFIX)
        self.assertTrue(self.tree_gate_passes("origin/agent-main"), "gate must pass once the back-port lands")

    def test_next_promotion_after_backport_passes_gate_and_check(self):
        self.build()
        self.tag_release("1.0.0")
        self.assertEqual(self.promote("1.0.0").returncode, 0)
        self.merge_pull_request(self.pr_branch("1.0.0"))
        hotfix = self.land_hotfix_pull_request()
        self.assertEqual(self.backport().returncode, 0)
        self.git(self.operator, "push", "origin", "backport/hotfix-test:refs/heads/agent-main")

        release = self.tag_release("1.0.1")
        result = self.promote("1.0.1")
        self.assertEqual(result.returncode, 0, self.describe(result))
        self.assertEqual(self.rev(self.origin, self.pr_branch("1.0.1")), release)
        self.merge_pull_request(self.pr_branch("1.0.1"))
        check = self.postcheck("1.0.1")
        self.assertEqual(check.returncode, 0, self.describe(check))
        self.assertIn(f"promoted {release}", check.stdout)
        self.assertEqual(self.show(self.origin, "main:hotfix.txt"), HOTFIX)
        self.assertTrue(self.is_ancestor(hotfix, "main"))
        self.assertEqual(self.show(self.origin, "main:version.txt"), "1.0.1\n")

    def test_hotfix_version_bump_conflicting_with_next_release_stops_promotion(self):
        self.build()
        self.tag_release("1.0.0")
        self.assertEqual(self.promote("1.0.0").returncode, 0)
        self.merge_pull_request(self.pr_branch("1.0.0"))
        # The hotfix release bumps the same version line that the next release bumps.
        self.land_hotfix_pull_request(path="version.txt", content="1.0.1\n")
        self.assertEqual(self.backport().returncode, 0)
        self.git(self.operator, "push", "origin", "backport/hotfix-test:refs/heads/agent-main")
        self.tag_release("1.0.2")
        main_before = self.rev(self.origin, "main")

        result = self.promote("1.0.2")

        self.assert_refused(result, self.pr_branch("1.0.2"), main_before)


if __name__ == "__main__":
    unittest.main()
