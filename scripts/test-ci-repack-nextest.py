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

    def pack_tar(self, members, parts):
        """Encode a tar of (name, bytes, mode[, type]) members as the build job would."""
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode="w") as archive:
            for name, data, mode, *kind in members:
                member = tarfile.TarInfo(name)
                member.mode = mode
                if kind:
                    member.type = kind[0]
                    member.linkname = "../../outside"
                    archive.addfile(member)
                    continue
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
        native = self.root / "native.tar.zst"
        native.unlink(missing_ok=True)
        subprocess.run(["zstd", "-q", "-3", "-o", str(native)], input=stream.getvalue(), check=True)
        transfer = self.root / "transfer.tar.zst"
        for stale in self.root.glob("transfer.tar.zst.*"):
            stale.unlink()
        result = self.run_repack("pack", native, transfer, parts=parts)
        self.assertEqual(result.returncode, 0, result.stderr)
        return native, transfer

    def test_repeated_binary_content_transfers_once_and_extracts_every_member(self):
        # Linked Rust executables repeat sections at distances beyond the
        # native archive's 2 MiB window. Reproduce that geometry, not host load.
        block = random.Random(17).randbytes(3 * 1024 * 1024)
        names = ("target/debug/orbit", "target/debug/deps/one", "target/debug/deps/two")
        native, transfer = self.pack_tar([(name, block, 0o755) for name in names], parts=3)
        parts = [self.root / f"transfer.tar.zst.{index}" for index in (1, 2, 3)]
        self.assertLess(sum(part.stat().st_size for part in parts), native.stat().st_size * 0.6,
                        "repeated linked binaries must not bloat the transfer beyond the native window")
        # A missing part, or a count that drops the last one, must fail
        # rather than run a shard from a prefix of the tar stream.
        for count in (2, 4):
            with self.subTest(parts=count):
                destination = self.root / f"short-{count}"
                destination.mkdir()
                result = self.run_repack("extract", transfer, destination, parts=count)
                self.assertNotEqual(result.returncode, 0)
        destination = self.root / "checkout"
        destination.mkdir()
        result = self.run_repack("extract", transfer, destination, parts=3)
        self.assertEqual(result.returncode, 0, result.stderr)
        for name in names:
            extracted = destination / name
            self.assertEqual(extracted.read_bytes(), block)
            self.assertTrue(os.access(extracted, os.X_OK), f"{name} must stay executable")

    def test_members_outside_target_links_and_escapes_are_refused(self):
        # nextest's own extraction refused unsafe members; this one must too.
        for case, member in (("outside target", ("scripts/run.sh", b"x", 0o755)),
                             ("escaping path", ("target/../escape", b"x", 0o644)),
                             ("absolute path", ("/tmp/orbit-repack-absolute", b"x", 0o644)),
                             ("symlink", ("target/debug/link", b"", 0o777, tarfile.SYMTYPE))):
            with self.subTest(case=case):
                _, transfer = self.pack_tar([("target/debug/ok", b"ok", 0o644), member], parts=1)
                destination = self.root / f"refuse-{case.replace(' ', '-')}"
                destination.mkdir()
                result = self.run_repack("extract", transfer, destination)
                self.assertNotEqual(result.returncode, 0, case)
                self.assertFalse((self.root / "escape").exists())
                self.assertFalse((destination / "escape").exists())
                self.assertFalse(Path("/tmp/orbit-repack-absolute").exists())
                self.assertFalse((destination / "scripts").exists())
                self.assertFalse((destination / "target/debug/link").is_symlink())

    def test_unavailable_corrupt_and_failed_setup_never_publish_or_replace(self):
        # One-part transfer names: extract reads SOURCE.1 and pack writes
        # DESTINATION.1. Use a real tar stream so extract reaches the decoder
        # exit-status check instead of failing earlier on tar parsing.
        stream = io.BytesIO()
        with tarfile.open(fileobj=stream, mode="w") as archive:
            member = tarfile.TarInfo("target/debug/ok")
            member.mode = 0o644
            member.size = 2
            archive.addfile(member, io.BytesIO(b"ok"))
        valid_tar = stream.getvalue()
        valid_tar_path = self.root / "valid.tar"
        valid_tar_path.write_bytes(valid_tar)
        source = self.root / "source.zst.1"
        subprocess.run(["zstd", "-q", "-o", str(source)], input=valid_tar, check=True)
        original = source.read_bytes()
        trailing = subprocess.run(["zstd", "-q", "-c"], input=b"trailing",
                                  capture_output=True, check=True).stdout
        absent = self.root / "no-tools"
        absent.mkdir()
        broken = self.root / "broken-tools"
        broken.mkdir()
        zstd = broken / "zstd"
        zstd.write_text(
            f"#!{sys.executable}\n"
            "import sys\n"
            f"if '-dc' in sys.argv:\n"
            f"    sys.stdout.buffer.write(open({str(valid_tar_path)!r}, 'rb').read())\n"
            "    sys.stdout.buffer.flush()\n"
            "    raise SystemExit(7)\n"
            "sys.stdin.buffer.read()\n"
            "raise SystemExit(0)\n"
        )
        zstd.chmod(0o755)
        corrupt = self.root / "truncated.zst.1"
        corrupt.write_bytes(original + trailing[:-3])
        for mode in ("pack", "extract"):
            for name, archive, env in (
                ("unavailable", source, dict(os.environ, PATH=str(absent))),
                ("failed", source, dict(os.environ, PATH=str(broken))),
                ("corrupt", corrupt, os.environ),
            ):
                with self.subTest(mode=mode, failure=name):
                    destination = self.root / f"{mode}-{name}.zst"
                    if mode == "extract":
                        archive = archive.with_suffix("")
                        destination.mkdir()
                    result = self.run_repack(mode, archive, destination, env=env)
                    self.assertNotEqual(result.returncode, 0)
                    if mode == "pack":
                        self.assertFalse(list(self.root.glob(f"{mode}-{name}.zst*")))
                    elif name == "unavailable":
                        self.assertEqual(list(destination.iterdir()), [])
                    self.assertFalse(list(self.root.glob("nextest-repack-*")))
        # Pack never replaces its input or an existing output: its first part
        # here is its own source.
        with self.subTest(mode="pack", failure="existing destination"):
            result = self.run_repack("pack", source, source.with_suffix(""))
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(source.read_bytes(), original)

if __name__ == "__main__":
    unittest.main()
