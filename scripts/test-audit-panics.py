#!/usr/bin/env python3
"""Exercise the public panic inventory against workspace member manifests."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import unittest

SCRIPTS = Path(__file__).resolve().parent


class PanicInventoryTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPTS.parent / '.orbit/tmp'
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix='panic-inventory-', dir=scratch)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def audit(self, root):
        result = subprocess.run(['bash', str(root / 'scripts/audit_panics.sh'), '--json', '--sites'],
                                env=dict(os.environ), capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return json.loads(result.stdout)

    def test_workspace_members_include_extracted_crates(self):
        members = tomllib.loads((SCRIPTS.parent / 'Cargo.toml').read_text())['workspace']['members']
        expected = {tomllib.loads((SCRIPTS.parent / member / 'Cargo.toml').read_text())['package']['name']
                    for member in members}
        report = self.audit(SCRIPTS.parent)
        self.assertEqual(set(report['per_crate']), expected)
        for name in ('orbit-auth', 'orbit-automation', 'orbit-config', 'orbit-runtime', 'orbit-types'):
            self.assertIn(name, report['per_crate'])
        self.assertGreater(report['per_crate']['orbit-config']['non_test'], 0)
        critical = {'orbit-engine', 'orbit-agent', 'orbit-tools', 'orbit-exec'}
        self.assertLessEqual(set(report['blast_radius_sites']), critical)
        for name, counts in report['per_crate'].items():
            if name not in critical:
                self.assertEqual(counts['blast_radius'], 0, name)

    def test_new_member_is_counted_without_changing_blast_radius(self):
        scripts = self.root / 'scripts'
        scripts.mkdir()
        for name in ('audit_panics.py', 'audit_panics.sh', 'require-python.sh'):
            shutil.copy2(SCRIPTS / name, scripts / name)
        (self.root / 'Cargo.toml').write_text(
            '[workspace]\nmembers = ["crates/orbit-agent", "extracted-config"]\n')
        for directory, name in (('crates/orbit-agent', 'orbit-agent'), ('extracted-config', 'orbit-config')):
            crate = self.root / directory
            (crate / 'src').mkdir(parents=True)
            (crate / 'Cargo.toml').write_text(f'[package]\nname = "{name}"\nversion = "0.1.0"\n')
            (crate / 'src/lib.rs').write_text('pub fn snapshot() { panic!("config failure"); }\n')
        report = self.audit(self.root)
        self.assertEqual(set(report['per_crate']), {'orbit-agent', 'orbit-config'})
        self.assertEqual(report['totals']['non_test'], 2)
        self.assertEqual(report['totals']['blast_radius'], 1)
        self.assertEqual(report['per_crate']['orbit-config']['non_test'], 1)
        self.assertEqual(report['per_crate']['orbit-config']['blast_radius'], 0)
        self.assertEqual(set(report['blast_radius_sites']), {'orbit-agent'})


if __name__ == '__main__':
    unittest.main()
