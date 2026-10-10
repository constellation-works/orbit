#!/usr/bin/env python3
"""Run the documented hotfix back-merge against isolated Git repositories.

The fixture has a stale local main and a newer remote hotfix. The test executes
the hotfix flow's shell block and checks history and file contents.
"""

import os
from pathlib import Path
import re
import shlex
import subprocess
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


def command_lines(block):
    lines = []
    for raw in textwrap.dedent(block).splitlines():
        line = raw.strip()
        if line and not line.startswith("#"):
            lines.append(line)
    return lines


def back_merge_script(text):
    """Return the hotfix back-merge shell block to execute.

    The block is the fenced shell under the hotfix flow that merges and pushes
    agent-main. Callers assert repository state, not the block's wording.
    """
    chosen = []
    for match in SHELL_FENCE.finditer(hotfix_section(text)):
        block = match.group(2)
        lines = command_lines(block)
        merges = any(line.split()[:2] == ["git", "merge"] for line in lines)
        pushes = any(
            line.split()[:3] == ["git", "push", "origin"] and "agent-main" in line.split()
            for line in lines
        )
        if merges and pushes:
            chosen.append(textwrap.dedent(block).strip() + "\n")
    if len(chosen) != 1:
        raise AssertionError(
            f"expected one hotfix back-merge shell block, found {len(chosen)}"
        )
    return chosen[0]


class ReleasePromotionRecoveryTests(unittest.TestCase):
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
        result = subprocess.run(
            ["git", "-C", str(repo), *args],
            env=self.env,
            text=True,
            capture_output=True,
            timeout=30,
        )
        if check and result.returncode != 0:
            self.fail(
                f"git -C {repo.name} {shlex.join(args)} -> {result.returncode}\n"
                f"{result.stderr}{result.stdout}"
            )
        return result

    def run_git(self, *args, check=True):
        result = subprocess.run(
            ["git", *args],
            cwd=self.root,
            env=self.env,
            text=True,
            capture_output=True,
            timeout=30,
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
        hook = origin / "hooks" / "pre-receive"
        log = shlex.quote(str(self.push_log_path))
        hook.write_text(
            "#!/bin/sh\n"
            "set -eu\n"
            f"log={log}\n"
            "while read -r old new ref; do\n"
            "  printf '%s %s %s\\n' \"$old\" \"$new\" \"$ref\" >> \"$log\"\n"
            f"  if [ \"$old\" != {ZERO!r} ]; then\n"
            "    git merge-base --is-ancestor \"$old\" \"$new\" || exit 1\n"
            "  fi\ndone\n"
        )
        hook.chmod(0o755)
        self.git(origin, "config", "receive.denyNonFastForwards", "true")
        self.git(origin, "config", "core.logAllRefUpdates", "true")

    def commit_file(self, repo, name, content, message):
        (repo / name).write_text(content)
        self.git(repo, "add", "--", name)
        self.git(repo, "commit", "-m", message)

    def test_skipped_hotfix_recovery_includes_remote_main(self):
        origin = self.root / "origin.git"
        origin.mkdir()
        self.git(origin, "init", "--bare", "--initial-branch=main")
        self.write_hook(origin)

        seed = self.root / "seed"
        seed.mkdir()
        self.git(seed, "init", "--initial-branch=main")
        self.commit_file(seed, "base.txt", "base\n", "base")
        base = self.rev(seed, "HEAD")
        self.git(seed, "branch", "agent-main")
        self.git(seed, "remote", "add", "origin", str(origin))
        self.git(seed, "push", "origin", "main", "agent-main")

        operator = self.root / "operator"
        self.run_git("clone", str(origin), str(operator))
        self.git(operator, "checkout", "-b", "agent-main", "--track", "origin/agent-main")
        self.commit_file(operator, "dev.txt", DEVELOPMENT, "development")
        dev = self.rev(operator, "HEAD")
        self.git(operator, "push", "origin", "agent-main")
        self.assertEqual(self.rev(operator, "main"), base)

        other = self.root / "other"
        self.run_git("clone", str(origin), str(other))
        self.commit_file(other, "hotfix.txt", HOTFIX, "hotfix")
        hotfix = self.rev(other, "HEAD")
        self.git(other, "push", "origin", "main")
        self.assertNotEqual(hotfix, base)
        self.assertEqual(self.rev(origin, "main"), hotfix)
        self.assertEqual(self.rev(operator, "main"), base)
        self.assertEqual(self.rev(operator, "origin/main"), base)

        fetched = self.git(operator, "fetch", "origin", check=False)
        self.assertEqual(fetched.returncode, 0, fetched.stderr)
        precheck = self.git(
            operator, "merge-base", "--is-ancestor", "origin/main", "origin/agent-main", check=False
        )
        self.assertEqual(
            precheck.returncode,
            1,
            "promotion ancestry must fail while agent-main lacks the remote hotfix\n"
            f"{precheck.stderr}",
        )
        self.assertEqual(self.rev(operator, "main"), base, "fetch must leave local main stale")
        self.assertEqual(self.rev(operator, "origin/main"), hotfix)

        # The precheck fetch refreshed origin/main. Restore the stale
        # remote-tracking ref so the recovery has to refresh it itself.
        # Merging that ref, or the untouched local main, drops the hotfix.
        self.git(operator, "update-ref", "refs/remotes/origin/main", base)
        self.assertEqual(self.rev(operator, "origin/main"), base)
        self.assertEqual(self.rev(origin, "main"), hotfix)

        before_log = self.push_log()
        script = back_merge_script(RELEASING.read_text(encoding="utf-8"))
        recovered = subprocess.run(
            ["bash", "-e", "-o", "pipefail", "-c", script],
            cwd=operator,
            env=self.env,
            text=True,
            capture_output=True,
            timeout=60,
        )
        self.assertEqual(
            recovered.returncode,
            0,
            f"documented recovery failed\nstdout:\n{recovered.stdout}\nstderr:\n{recovered.stderr}",
        )

        fetched = self.git(operator, "fetch", "origin", check=False)
        self.assertEqual(fetched.returncode, 0, fetched.stderr)
        ancestry = self.git(
            operator, "merge-base", "--is-ancestor", "origin/main", "origin/agent-main", check=False
        )
        self.assertEqual(
            ancestry.returncode,
            0,
            "promotion ancestry must succeed after recovery\n" + ancestry.stderr,
        )
        self.assertEqual(self.show(origin, "agent-main:hotfix.txt"), self.show(origin, "main:hotfix.txt"))
        self.assertEqual(self.show(origin, "main:hotfix.txt"), HOTFIX)
        self.assertEqual(self.show(origin, "agent-main:dev.txt"), DEVELOPMENT)
        self.assertEqual(
            self.git(origin, "merge-base", "--is-ancestor", dev, "agent-main", check=False).returncode,
            0,
            "development commits must remain on agent-main",
        )
        self.assertEqual(
            self.git(origin, "merge-base", "--is-ancestor", hotfix, "agent-main", check=False).returncode,
            0,
            "the remote hotfix commit must be on agent-main",
        )

        tip, *parents = self.git(origin, "rev-list", "--parents", "-n", "1", "agent-main").stdout.split()
        self.assertEqual(tip, self.rev(origin, "agent-main"))
        self.assertIn(hotfix, parents, "recovery must merge the fetched remote hotfix")
        self.assertIn(dev, parents, "recovery must keep the previous agent-main tip as a parent")
        self.assertNotIn(base, parents, "recovery must not merge the stale local main commit")

        updates = [
            line.split()
            for line in self.push_log()[len(before_log):]
            if line.endswith(" refs/heads/agent-main")
        ]
        self.assertEqual(len(updates), 1, self.push_log()[len(before_log):])
        old, new, ref = updates[0]
        self.assertEqual(ref, "refs/heads/agent-main")
        self.assertEqual(old, dev)
        self.assertEqual(new, self.rev(origin, "agent-main"))
        fast_forward = self.git(origin, "merge-base", "--is-ancestor", old, new, check=False)
        self.assertEqual(fast_forward.returncode, 0, "agent-main push must be a fast-forward")


if __name__ == "__main__":
    unittest.main()
