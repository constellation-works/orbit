#!/usr/bin/env python3
"""Run MCP smoke scripts with a disposable provider and real SQLite database.

The provider follows the requested transport; nothing invokes an API or reads
live Orbit state. A sandbox-exec shim exercises the companion's script contract,
without claiming evidence about kernel confinement.
"""
import json
import os
from pathlib import Path
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

# Fixture-local sqlite3 executable backed by Python's sqlite3 module.
# Shadows any host binary via the fixture PATH prepend so the boundary
# fixture never depends on an externally installed SQLite CLI. Supports
# the SQL/output forms the smoke scripts use: scalar `sqlite3 DB SQL`
# queries in default list mode and `sqlite3 -header -column DB SQL`
# selects on the PASS path. Real SQL execution; SQL errors exit non-zero
# like the native CLI instead of producing a skip or false pass.
FAKE_SQLITE3 = '''import sqlite3, sys
def main():
    args = sys.argv[1:]
    header = False
    column = False
    sep = '|'
    nullvalue = ''
    i = 0
    while i < len(args) and args[i].startswith('-') and args[i] != '-':
        opt = args[i]
        if opt in ('-header', '-headers'):
            header = True
            i += 1
        elif opt == '-column':
            column = True
            i += 1
        elif opt == '-list':
            column = False
            i += 1
        elif opt in ('-separator', '-sep'):
            if i + 1 < len(args):
                sep = args[i + 1]
                i += 2
            else:
                i += 1
        elif opt in ('-nullvalue', '-null-value'):
            if i + 1 < len(args):
                nullvalue = args[i + 1]
                i += 2
            else:
                i += 1
        elif opt in ('-version', '--version', '-v'):
            print('3.46.1')
            return 0
        elif opt == '--':
            i += 1
            break
        else:
            i += 1
    rest = args[i:]
    if not rest:
        print('Usage: sqlite3 [OPTIONS] FILENAME [SQL]', file=sys.stderr)
        return 1
    db_path = rest[0]
    if len(rest) > 1:
        sql = ' '.join(rest[1:])
    else:
        sql = sys.stdin.read()
        if not sql.strip():
            return 0
    if not sql.strip():
        return 0
    stripped = sql.lstrip()
    if stripped.startswith('.'):
        print('Error: unknown command "%s"' % stripped.split()[0], file=sys.stderr)
        return 1
    try:
        con = sqlite3.connect(db_path)
        try:
            cur = con.execute(sql)
            if cur.description is None:
                con.commit()
                return 0
            cols = [d[0] for d in cur.description]
            rows = cur.fetchall()
            str_rows = [[nullvalue if v is None else str(v) for v in r] for r in rows]
            if column:
                widths = []
                for ci, h in enumerate(cols):
                    w = len(h)
                    for r in str_rows:
                        if len(r[ci]) > w:
                            w = len(r[ci])
                    widths.append(w)
                def _fmt(cells):
                    return '  '.join(c.ljust(widths[ci]) for ci, c in enumerate(cells)).rstrip()
                if header:
                    print(_fmt(cols))
                    print('  '.join('-' * w for w in widths))
                for r in str_rows:
                    print(_fmt(r))
            else:
                if header:
                    print(sep.join(cols))
                for r in str_rows:
                    print(sep.join(r))
            return 0
        finally:
            try:
                con.close()
            except Exception:
                pass
    except Exception as e:
        print('Error: %s' % e, file=sys.stderr)
        return 1
if __name__ == '__main__':
    sys.exit(main())
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
        self.executable('sqlite3', f'#!{sys.executable}\n' + FAKE_SQLITE3)

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
