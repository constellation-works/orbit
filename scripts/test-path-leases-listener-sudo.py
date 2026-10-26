#!/usr/bin/env python3
"""Run the listener-sudo recipe's tty guard as written, with heredoc-fed stdin.

Each warm-then-child block in docs/dev/qa/path-leases-listener-sudo.md is fed to
`bash -s` on stdin, as its `su - operator -c 'bash -s' << 'EOF'` heredoc does. The
guard must pass when the session has a controlling terminal, and refuse with exit 2
when it has none. A `sudo` stub on PATH keeps real sudo out, and the recipe's /tmp
paths are redirected into a scratch directory. This does not exercise `su` or sudo's
timestamp records; only the disposable guest run does.
"""

import fcntl
import os
from pathlib import Path
import pty
import subprocess
import tempfile
import termios
import unittest

RECIPE = Path(__file__).resolve().parent.parent / "docs/dev/qa/path-leases-listener-sudo.md"
OPENER = "su - operator -c 'bash -s' << 'EOF'"


def recipe_blocks():
    """Return the stdin body of each warm-then-child heredoc, in document order."""
    lines = RECIPE.read_text().splitlines()
    bodies = []
    for start, line in enumerate(lines):
        if line == OPENER:
            end = lines.index("EOF", start)
            bodies.append("\n".join(lines[start + 1:end]) + "\n")
    return bodies


def run_block(body, scratch, controlling_terminal):
    """Feed `body` to `bash -s` on stdin, with or without a controlling terminal."""
    bin_dir = scratch / "bin"
    bin_dir.mkdir(parents=True, exist_ok=True)
    sudo = bin_dir / "sudo"
    sudo.write_text("#!/bin/sh\nexit 0\n")
    sudo.chmod(0o755)
    script = scratch / "stdin.sh"
    script.write_text(body.replace("/tmp/", f"{scratch}/"))
    env = dict(os.environ, PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}")
    with script.open("rb") as stdin:
        if not controlling_terminal:
            return subprocess.run(["bash", "-s"], stdin=stdin, env=env, start_new_session=True,
                                  capture_output=True, text=True, timeout=60)
        # The pty slave becomes the session's controlling terminal. stdin stays the heredoc.
        master, slave = pty.openpty()
        try:
            return subprocess.run(
                ["bash", "-s"], stdin=stdin, env=env, start_new_session=True, pass_fds=(slave,),
                preexec_fn=lambda: fcntl.ioctl(slave, termios.TIOCSCTTY, 0),
                capture_output=True, text=True, timeout=60)
        finally:
            os.close(slave)
            os.close(master)


class ListenerSudoGuardTests(unittest.TestCase):
    def setUp(self):
        scratch = Path(__file__).resolve().parent.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="listener-sudo-guard-", dir=scratch)
        self.addCleanup(temporary.cleanup)
        self.scratch = Path(temporary.name)
        self.blocks = recipe_blocks()
        self.assertEqual(len(self.blocks), 2, "expected the case A and case B warm-then-child blocks")

    def test_guard_passes_with_a_controlling_terminal(self):
        for case, body in zip("AB", self.blocks):
            with self.subTest(case=case):
                result = run_block(body, self.scratch / case, controlling_terminal=True)
                self.assertNotIn("refusing", result.stderr, result.stderr)
                self.assertIn("warm_exit=0", result.stdout)
                self.assertIn("child_exit=", result.stdout)

    def test_guard_refuses_without_a_controlling_terminal(self):
        for case, body in zip("AB", self.blocks):
            with self.subTest(case=case):
                result = run_block(body, self.scratch / case, controlling_terminal=False)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn("refusing: this session has no controlling terminal", result.stderr)
                self.assertNotIn("warm_exit=", result.stdout)


if __name__ == "__main__":
    unittest.main()
