#!/usr/bin/env python3
"""Run MCP smoke scripts with a disposable provider and real SQLite database.

The provider follows the requested transport; nothing invokes an API or reads
live Orbit state. A sandbox-exec shim exercises the companion's script contract,
without claiming evidence about kernel confinement.
"""
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent

FAKE_CLAUDE = '''import json, os, sqlite3, sys
prompt = sys.argv[-1]
mode = os.environ['SMOKE_FIXTURE_MODE']
transport = 'run-mcp' if 'do not use mcp tools' not in prompt and 'MCP' in prompt else 'run'
if mode == 'cli':
    transport = 'run'
tool = 'orbit.task.show' if mode == 'unrelated' else 'orbit.task.list'
with sqlite3.connect(os.path.join(os.environ['HOME'], '.orbit/orbit.db')) as db:
    if mode != 'none':
        db.execute("INSERT INTO audit_events (timestamp, subcommand, tool_name, status, exit_code) VALUES ('now', ?, ?, ?, ?)",
                   (transport, tool, 'failed' if mode == 'failed' else 'success', 1 if mode == 'failed' else 0))
print(json.dumps({'result': 'tasks_returned=0'}))
'''


class McpSmokeTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPTS.parent / '.orbit/tmp'
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix='mcp-smoke-', dir=scratch)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.home = self.root / 'home'
        (self.home / '.orbit').mkdir(parents=True)
        self.db = self.home / '.orbit/orbit.db'
        self.env = dict(os.environ, HOME=str(self.home), TMPDIR=str(self.root),
                        PATH=f'{self.bin}:{os.environ["PATH"]}',
                        GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM='1')
        for name in ('GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE'):
            self.env.pop(name, None)
        subprocess.run(['git', 'init', '-q', str(self.root)], env=self.env, check=True)
        (self.root / '.mcp.json').write_text('{}')
        self.executable('claude', f'#!{sys.executable}\n' + FAKE_CLAUDE)
        self.executable('orbit', '#!/bin/sh\nexit 99\n')
        self.executable('jq', '#!/bin/sh\nexit 99\n')
        self.executable('sandbox-exec', '#!/bin/sh\nshift 2\nexec "$@"\n')
        self.assertIsNotNone(shutil.which('sqlite3'), 'SQLite CLI is required for this boundary fixture')

    def executable(self, name, text):
        path = self.bin / name
        path.write_text(text)
        path.chmod(0o755)

    def test_only_new_successful_mcp_task_list_evidence_passes(self):
        for script in ('smoke-claude-mcp.sh', 'smoke-claude-mcp-sandbox.sh'):
            for mode, accepted in (('requested', True), ('cli', False), ('unrelated', False),
                                   ('failed', False), ('none', False)):
                with self.subTest(script=script, mode=mode):
                    with sqlite3.connect(self.db) as db:
                        db.execute('DROP TABLE IF EXISTS audit_events')
                        db.execute('CREATE TABLE audit_events (id INTEGER PRIMARY KEY, timestamp TEXT, '
                                   'subcommand TEXT, tool_name TEXT, status TEXT, exit_code INTEGER)')
                        # Pre-existing success cannot be evidence for this invocation.
                        db.execute("INSERT INTO audit_events VALUES (1, 'old', 'run-mcp', 'orbit.task.list', 'success', 0)")
                    result = subprocess.run(['bash', str(SCRIPTS / script)], cwd=self.root,
                                            env=dict(self.env, SMOKE_FIXTURE_MODE=mode),
                                            capture_output=True, text=True, timeout=30)
                    self.assertEqual(result.returncode, 0 if accepted else 1,
                                     result.stdout + result.stderr)
                    if accepted:
                        self.assertIn('PASS:', result.stdout)


if __name__ == '__main__':
    unittest.main()
