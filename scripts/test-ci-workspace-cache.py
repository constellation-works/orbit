#!/usr/bin/env python3
"""Regress unnecessary workspace rebuilds and stale restored Cargo inputs."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest


ROOT = Path(__file__).resolve().parent.parent


class WorkspaceCacheTests(unittest.TestCase):
    def test_unchanged_inputs_reuse_outputs_but_older_changed_bytes_rebuild(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=scratch, prefix="workspace-cache-") as directory:
            workspace = Path(directory)
            source = workspace / "src"
            source.mkdir()
            (workspace / ".gitignore").write_text("/target/\n/compile.log\n/rustc-wrapper\n")
            (workspace / "Cargo.toml").write_text(
                '[package]\nname="cache-fixture"\nversion="0.1.0"\nedition="2024"\n[workspace]\n')
            (source / "lib.rs").write_text('pub fn value() -> &\'static str { include_str!("value.txt") }\n')
            (source / "main.rs").write_text('fn main() { println!("{}", cache_fixture::value()); }\n')
            asset = source / "value.txt"
            asset.write_text("alpha")
            wrapper = workspace / "rustc-wrapper"
            log = workspace / "compile.log"
            wrapper.write_text(f'''#!{sys.executable}
import json, os, sys
if "--crate-name" in sys.argv:
    with open({str(log)!r}, "a") as stream:
        stream.write(json.dumps(sys.argv[1:]) + "\\n")
os.execv(sys.argv[1], sys.argv[1:])
''')
            wrapper.chmod(0o755)
            environment = {key: value for key, value in os.environ.items()
                           if not key.startswith(("GIT_", "CARGO_", "RUSTC_"))}
            environment.update(
                RUSTUP_HOME=os.environ.get("RUSTUP_HOME", str(Path.home() / ".rustup")),
                HOME=str(workspace / "home"), CARGO_HOME=str(workspace / "cargo-home"),
                CARGO_TARGET_DIR=str(workspace / "target"), RUSTC_WRAPPER=str(wrapper),
                GIT_CEILING_DIRECTORIES=str(scratch))

            def command(*arguments):
                result = subprocess.run(arguments, cwd=workspace, env=environment,
                                        capture_output=True, text=True, timeout=60)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                return result.stdout

            def build():
                command(str(ROOT / "scripts/build-budget.py"), "--", "cargo", "build", "--offline", "--quiet")
                return command(str(workspace / "target/debug/cache-fixture")).strip()

            def cache(action):
                return json.loads(command(sys.executable, str(ROOT / "scripts/ci-workspace-cache.py"),
                                          action, "--workspace", str(workspace)))

            command("git", "init", "-q")
            command("git", "add", ".")
            command("git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                    "commit", "-qm", "fixture")
            self.assertEqual(build(), "alpha")
            cache("capture")
            compiled = log.read_text().splitlines()
            self.assertGreaterEqual(len(compiled), 2, "the control must compile both library and binary")
            for path in source.iterdir():
                os.utime(path, ns=(time.time_ns(), time.time_ns()))
            restored = cache("restore")
            self.assertEqual(restored["changed_inputs"], 0)
            self.assertEqual(build(), "alpha")
            self.assertEqual(log.read_text().splitlines(), compiled,
                             "unchanged checkout timestamps must not rebuild workspace units")

            original = asset.stat().st_mtime_ns
            asset.write_text("omega")  # Same length; an old mtime must not hide changed bytes.
            os.utime(asset, ns=(original, original))
            self.assertEqual(cache("restore")["changed_inputs"], 1)
            self.assertEqual(build(), "omega", "F2026-09-081: restored outputs must never hide older changed input")
            self.assertGreater(len(log.read_text().splitlines()), len(compiled))


if __name__ == "__main__":
    unittest.main()
