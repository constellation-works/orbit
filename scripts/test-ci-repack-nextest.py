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

    def run_repack(self, mode, source, destination, **kwargs):
        return subprocess.run([sys.executable, str(SCRIPT), mode, str(source), str(destination)],
                              capture_output=True, **kwargs)

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
        result = self.run_repack("pack", native, transfer)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertLess(transfer.stat().st_size, native.stat().st_size * 0.6,
                        "repeated linked binaries must not bloat the transfer beyond the native window")
        restored = self.root / "restored.tar.zst"
        result = self.run_repack("unpack", transfer, restored)
        self.assertEqual(result.returncode, 0, result.stderr)
        # No larger-window option: this must work with the decoder budget
        # nextest uses. Exact bytes retain metadata, paths, modes and members.
        decoded = subprocess.run(["zstd", "-q", "-dc", str(restored)], check=True,
                                 capture_output=True).stdout
        self.assertEqual(decoded, original)

    def test_unavailable_corrupt_and_failed_setup_never_publish_or_replace(self):
        source = self.root / "source.zst"
        subprocess.run(["zstd", "-q", "-o", str(source)], input=b"tar bytes", check=True)
        original = source.read_bytes()
        absent = self.root / "no-tools"
        absent.mkdir()
        broken = self.root / "broken-tools"
        broken.mkdir()
        zstd = broken / "zstd"
        zstd.write_text("#!/bin/sh\nexit 7\n")
        zstd.chmod(0o755)
        corrupt = self.root / "truncated.zst"
        corrupt.write_bytes(source.read_bytes()[:-3])
        for mode in ("pack", "unpack"):
            for name, archive, env in (
                ("unavailable", source, dict(os.environ, PATH=str(absent))),
                ("failed", source, dict(os.environ, PATH=str(broken))),
                ("corrupt", corrupt, os.environ),
            ):
                with self.subTest(mode=mode, failure=name):
                    destination = self.root / f"{mode}-{name}.zst"
                    result = self.run_repack(mode, archive, destination, env=env)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertFalse(destination.exists())
                    self.assertFalse(list(self.root.glob("nextest-repack-*")))
            result = self.run_repack(mode, source, source)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(source.read_bytes(), original)


if __name__ == "__main__":
    unittest.main()
