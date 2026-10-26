#!/usr/bin/env python3
"""Exercise the vendor refresh offline with hostile and ordinary npm tarballs."""

import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import self_test_hang
import shutil
import sys
import tarfile
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("vendor_checker", SCRIPTS / "check-dashboard-vendor.py")
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)
PAYLOAD = b"refreshed fixture asset\n"


class RefreshTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit" / "tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="vendor-refresh-", dir=scratch)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.repo = self.root / "repo"
        self.scripts = self.repo / "scripts"
        self.scripts.mkdir(parents=True)
        for name in ("refresh-dashboard-vendor.sh", "check-dashboard-vendor.py"):
            shutil.copy2(SCRIPTS / name, self.scripts / name)
        CHECKER.write_fixture(self.repo)
        self.package = self.repo / CHECKER.PACKAGE_DIR
        self.vendor = self.repo / CHECKER.VENDOR_DIR
        self.manifest = self.vendor / CHECKER.MANIFEST_NAME
        self.assets = json.loads(self.manifest.read_text())["assets"]
        self.before = {path: path.read_bytes() for path in self.package.rglob("*") if path.is_file()}
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.tmp = self.root / "tmp"
        self.tmp.mkdir()
        self.sentinel = self.tmp / "escaped"
        self.tarball = self.root / "fixture.tgz"
        self.extraction_log = self.root / "extraction.log"
        self.npm_log = self.root / "npm.log"
        self.write_program("npm", """import json, os, shutil, sys
from pathlib import Path
with open(os.environ['VENDOR_TEST_NPM_LOG'], 'a') as log:
    log.write(sys.argv[1] + '\\n')
if sys.argv[1] == 'pack':
    destination = Path(sys.argv[sys.argv.index('--pack-destination') + 1])
    shutil.copyfile(os.environ['VENDOR_TEST_TARBALL'], destination / 'fixture.tgz')
    print(json.dumps([{'filename': 'fixture.tgz'}]))
elif sys.argv[1] != 'install':
    raise SystemExit('unexpected npm command')
""")
        # The old signature rejects filter before entering the function. With
        # no filter it reproduces older Python's unsafe extraction behavior.
        self.write_program("python3", """import errno, os, sys, tarfile
if sys.argv[1] != '-':
    os.execv(sys.executable, [sys.executable, *sys.argv[1:]])
from pathlib import Path as _PatchedPath
if os.environ.get('VENDOR_TEST_INJECT_EXDEV') == '1':
    def _exdev_replace(self, target):
        raise OSError(errno.EXDEV, 'Invalid cross-device link')
    _PatchedPath.replace = _exdev_replace
original = tarfile.TarFile.extractall
tarfile.TarFile.extraction_filter = staticmethod(lambda member, destination: member)
def legacy_extractall(self, path='.', members=None, *, numeric_owner=False):
    with open(os.environ['VENDOR_TEST_EXTRACTION_LOG'], 'a') as log:
        log.write('unfiltered extraction\\n')
    return original(self, path, members, numeric_owner=numeric_owner)
tarfile.TarFile.extractall = legacy_extractall
sys.argv = sys.argv[1:]
exec(compile(sys.stdin.read(), '<refresh-dashboard-vendor>', 'exec'))
""")

    def write_program(self, name, body):
        path = self.bin / name
        path.write_text(f"#!{sys.executable}\n" + body)
        path.chmod(0o755)

    def write_tarball(self, extra=(), include_assets=True):
        with tarfile.open(self.tarball, "w:gz") as archive:
            directory = tarfile.TarInfo("package")
            directory.type = tarfile.DIRTYPE
            archive.addfile(directory)
            if include_assets:
                for asset in self.assets:
                    member = tarfile.TarInfo(f"package/{asset['npm_file']}")
                    member.size = len(PAYLOAD)
                    archive.addfile(member, io.BytesIO(PAYLOAD))
            for member in extra:
                data = io.BytesIO(b"escape") if member.isfile() else None
                if data is not None:
                    member.size = len(data.getvalue())
                archive.addfile(member, data)

    def refresh(self, extra_env=None):
        env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}", TMPDIR=str(self.tmp),
                   VENDOR_TEST_TARBALL=str(self.tarball), VENDOR_TEST_NPM_LOG=str(self.npm_log),
                   VENDOR_TEST_EXTRACTION_LOG=str(self.extraction_log))
        if extra_env:
            env.update(extra_env)
        return self_test_hang.run(
            ["bash", str(self.scripts / "refresh-dashboard-vendor.sh")],
            step="refresh-dashboard-vendor",
            cwd=self.repo, env=env, text=True, capture_output=True)

    def assert_refused(self, result):
        self.assertFalse(self.sentinel.exists(), "archive wrote outside its extraction directory")
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.extraction_log.exists(), "refresh called unfiltered extractall")
        for path, content in self.before.items():
            self.assertEqual(path.read_bytes(), content, f"failed refresh changed {path}")
        self.assertNotIn("install", self.npm_log.read_text().splitlines())

    def test_escaping_member_with_expected_assets_is_refused(self):
        # ../../../ escapes both extract-<package> and the mktemp directory,
        # leaving a sentinel that the refresh's EXIT trap cannot clean up.
        for name in ("package/../../../escaped", str(self.sentinel)):
            with self.subTest(name=name):
                self.write_tarball([tarfile.TarInfo(name)])
                self.assert_refused(self.refresh())

    def test_links_and_special_members_are_refused(self):
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE):
            with self.subTest(kind=kind):
                member = tarfile.TarInfo("package/link")
                member.type = kind
                member.linkname = str(self.tmp)
                follow = tarfile.TarInfo("package/link/escaped")
                self.write_tarball([member, follow])
                self.assert_refused(self.refresh())

    def test_missing_asset_is_refused(self):
        self.write_tarball(include_assets=False)
        self.assert_refused(self.refresh())

    def test_safe_archive_refreshes_assets_without_extraction(self):
        self.write_tarball()
        result = self.refresh()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.extraction_log.exists())
        manifest = json.loads(self.manifest.read_text())
        for asset in manifest["assets"]:
            self.assertEqual((self.vendor / asset["path"]).read_bytes(), PAYLOAD)
            self.assertEqual(asset["sha256"], hashlib.sha256(PAYLOAD).hexdigest())
        self.assertEqual(self.npm_log.read_text().splitlines(), ["pack", "pack", "install"])

    def test_cross_device_install_succeeds(self):
        # Simulate TMPDIR on another filesystem by forcing every staged
        # Path.replace to raise EXDEV; the refresh must still install the
        # validated bytes (copy fallback) instead of failing like the
        # pre-fix bare replace did.
        self.write_tarball()
        result = self.refresh(extra_env={"VENDOR_TEST_INJECT_EXDEV": "1"})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.extraction_log.exists())
        manifest = json.loads(self.manifest.read_text())
        for asset in manifest["assets"]:
            self.assertEqual((self.vendor / asset["path"]).read_bytes(), PAYLOAD)
            self.assertEqual(asset["sha256"], hashlib.sha256(PAYLOAD).hexdigest())
        self.assertEqual(self.npm_log.read_text().splitlines(), ["pack", "pack", "install"])
        leftovers = [path for path in self.vendor.iterdir()
                     if path.name.startswith(".vendor-refresh-")]
        self.assertEqual(leftovers, [])


if __name__ == "__main__":
    unittest.main()
