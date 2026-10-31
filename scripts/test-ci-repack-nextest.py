#!/usr/bin/env python3
"""Exercise the transfer archive boundary, including fail-closed publication."""

import io
import os
from pathlib import Path
import random
import subprocess
import sys
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/ci-repack-nextest.py"


class RepackTests(unittest.TestCase):
    def setUp(self):
        scratch = ROOT / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(dir=scratch, prefix="repack-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def run_repack(self, mode, source, destination, parts=1, **kwargs):
        return subprocess.run([sys.executable, str(SCRIPT), mode, str(source), str(destination),
                               "--parts", str(parts)], capture_output=True, **kwargs)

    def test_repeated_binary_content_transfers_once_and_roundtrips_every_tar_byte(self):
        # Linked Rust executables repeat sections at distances beyond the
        # native archive's 2 MiB window. Reproduce that geometry, not host load.
        block = random.Random(17).randbytes(3 * 1024 * 1024)
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode="w") as archive:
            for name in ("target/debug/orbit", "target/debug/deps/one", "target/debug/deps/two"):
                member = tarfile.TarInfo(name)
                member.size = len(block)
                member.mode = 0o755
                archive.addfile(member, io.BytesIO(block))
        original = stream.getvalue()
        native = self.root / "native.tar.zst"
        subprocess.run(["zstd", "-q", "-3", "-o", str(native)], input=original, check=True)
        transfer = self.root / "transfer.tar.zst"
        result = self.run_repack("pack", native, transfer, parts=3)
        self.assertEqual(result.returncode, 0, result.stderr)
        parts = [self.root / f"transfer.tar.zst.{index}" for index in (1, 2, 3)]
        self.assertLess(sum(part.stat().st_size for part in parts), native.stat().st_size * 0.6,
                        "repeated linked binaries must not bloat the transfer beyond the native window")
        restored = self.root / "restored.tar.zst"
        # A missing part, or a count that drops the last one, must not
        # restore an apparently valid prefix of the tar stream.
        for count in (2, 4):
            with self.subTest(parts=count):
                result = self.run_repack("unpack", transfer, restored, parts=count)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(restored.exists())
        result = self.run_repack("unpack", transfer, restored, parts=3)
        self.assertEqual(result.returncode, 0, result.stderr)
        # No larger-window option: this must work with the decoder budget
        # nextest uses. Exact bytes retain metadata, paths, modes and members.
        decoded = subprocess.run(["zstd", "-q", "-dc", str(restored)], check=True,
                                 capture_output=True).stdout
        self.assertEqual(decoded, original)

    def test_unavailable_corrupt_and_failed_setup_never_publish_or_replace(self):
        # One-part transfer names: unpack reads SOURCE.1 and pack writes
        # DESTINATION.1.
        source = self.root / "source.zst.1"
        subprocess.run(["zstd", "-q", "-o", str(source)], input=b"tar bytes", check=True)
        original = source.read_bytes()
        absent = self.root / "no-tools"
        absent.mkdir()
        broken = self.root / "broken-tools"
        broken.mkdir()
        zstd = broken / "zstd"
        zstd.write_text("#!/bin/sh\nexit 7\n")
        zstd.chmod(0o755)
        corrupt = self.root / "truncated.zst.1"
        corrupt.write_bytes(source.read_bytes()[:-3])
        for mode in ("pack", "unpack"):
            for name, archive, env in (
                ("unavailable", source, dict(os.environ, PATH=str(absent))),
                ("failed", source, dict(os.environ, PATH=str(broken))),
                ("corrupt", corrupt, os.environ),
            ):
                with self.subTest(mode=mode, failure=name):
                    destination = self.root / f"{mode}-{name}.zst"
                    if mode == "unpack":
                        archive = archive.with_suffix("")
                    result = self.run_repack(mode, archive, destination, env=env)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(list(self.root.glob(f"{mode}-{name}.zst*")))
                    self.assertFalse(list(self.root.glob("nextest-repack-*")))
        # Neither mode replaces its input or an existing output: pack's
        # first part here is its own source, and unpack's output is that part.
        for mode, archive, destination in (("pack", source, source.with_suffix("")),
                                           ("unpack", source.with_suffix(""), source)):
            with self.subTest(mode=mode, failure="existing destination"):
                result = self.run_repack(mode, archive, destination)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(source.read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
