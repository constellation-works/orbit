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

FAKE_CLAUDE = r'''import json, os, sqlite3, subprocess, sys, time, threading
prompt = sys.argv[-1]
mode = os.environ["SMOKE_FIXTURE_MODE"]
exit_code = int(os.environ.get("SMOKE_FIXTURE_EXIT", "0"))
transport = "run-mcp" if "do not use mcp tools" not in prompt and "MCP" in prompt else "run"
if mode == "cli":
    transport = "run"
tool = "orbit.task.show" if mode == "unrelated" else "orbit.task.list"
status = "failed" if mode == "failed" else "success"
code = 1 if mode == "failed" else 0

# argv0 is orbit, so the supervisor can tell this invocation's server from a
# foreign pid. The child stays up until that pid is recorded, then exits.
CHILD = r"""
import os, time
path = os.environ.get("ORBIT_SMOKE_OBSERVED", "")
deadline = time.monotonic() + 5
while time.monotonic() < deadline:
    try:
        lines = open(path, encoding="utf-8").read().splitlines()
    except OSError:
        lines = []
    if str(os.getpid()) in lines:
        raise SystemExit(0)
    time.sleep(0.01)
raise SystemExit(0)
"""

def observed_lines():
    path = os.environ.get("ORBIT_SMOKE_OBSERVED", "")
    try:
        return open(path, encoding="utf-8").read().splitlines()
    except OSError:
        return []

def insert(pid):
    database = os.path.join(os.environ["HOME"], ".orbit", "orbit.db")
    with sqlite3.connect(database) as db:
        db.execute(
            "INSERT INTO audit_events (timestamp, subcommand, tool_name, status, exit_code, pid) "
            "VALUES ('now', ?, ?, ?, ?, ?)",
            (transport, tool, status, code, pid),
        )

proc = None
missing = False
try:
    if mode == "foreign":
        # A different client, not an orbit process of this invocation.
        insert(424242)
    elif mode != "none":
        ready = threading.Event()
        def spawn():
            global proc
            proc = subprocess.Popen(["orbit", "-c", CHILD], executable=sys.executable)
            ready.set()
            if mode == "threaded":
                proc.wait()
        if mode == "threaded":
            worker = threading.Thread(target=spawn)
            worker.start()
            ready.wait(timeout=5)
        else:
            spawn()
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline and str(proc.pid) not in observed_lines():
            if proc.poll() is not None:
                break
            time.sleep(0.01)
        if str(proc.pid) not in observed_lines():
            missing = True
        else:
            insert(proc.pid)
finally:
    if proc is not None:
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
    if mode == "threaded":
        worker.join(timeout=5)
if missing:
    print("fixture orbit pid was not observed", file=sys.stderr)
    raise SystemExit(3)
print(json.dumps({"result": "tasks_returned=0"}))
raise SystemExit(exit_code)
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

    def _reset_db(self):
        with sqlite3.connect(self.db) as db:
            db.execute('DROP TABLE IF EXISTS audit_events')
            db.execute('CREATE TABLE audit_events (id INTEGER PRIMARY KEY, timestamp TEXT, '
                       'subcommand TEXT, tool_name TEXT, status TEXT, exit_code INTEGER, pid INTEGER)')
            # Pre-existing success cannot be evidence for this invocation, even
            # when its pid column is set. That pid is not this run's server.
            db.execute("INSERT INTO audit_events (id, timestamp, subcommand, tool_name, status, exit_code, pid) "
                       "VALUES (1, 'old', 'run-mcp', 'orbit.task.list', 'success', 0, 1)")

    def _run(self, script, mode, exit_code='0'):
        self._reset_db()
        return subprocess.run(['bash', str(SCRIPTS / script)], cwd=self.root,
                              env=dict(self.env, SMOKE_FIXTURE_MODE=mode, SMOKE_FIXTURE_EXIT=exit_code),
                              capture_output=True, text=True, timeout=30)

    def test_only_new_successful_mcp_task_list_evidence_passes(self):
        for script in ('smoke-claude-mcp.sh', 'smoke-claude-mcp-sandbox.sh'):
            for mode, accepted in (('requested', True), ('threaded', True), ('cli', False), ('unrelated', False),
                                   ('failed', False), ('none', False), ('foreign', False)):
                with self.subTest(script=script, mode=mode):
                    result = self._run(script, mode)
                    self.assertEqual(result.returncode, 0 if accepted else 1,
                                     result.stdout + result.stderr)
                    if accepted:
                        self.assertIn('PASS:', result.stdout)
                    else:
                        self.assertNotIn('PASS:', result.stdout)

    def test_unreadable_process_tree_is_an_attribution_failure(self):
        # Inject an OS permission refusal into the supervisor only. Its child
        # is a real subprocess and still exits normally, so this distinguishes
        # failed observation from an ordinary command failure at the CLI.
        runner = self.root / "denied-proc.py"
        runner.write_text('''import os, runpy, subprocess, sys
original_listdir, original_scandir = os.listdir, os.scandir
def denied_listdir(path):
    if str(path).startswith("/proc"):
        raise PermissionError("fixture proc denied")
    return original_listdir(path)
def denied_scandir(path):
    if str(path).startswith("/proc"):
        raise PermissionError("fixture proc denied")
    return original_scandir(path)
original_run = subprocess.run
def denied_ps(command, *args, **kwargs):
    if command[0] == "ps":
        raise PermissionError("fixture ps denied")
    return original_run(command, *args, **kwargs)
os.listdir, os.scandir = denied_listdir, denied_scandir
subprocess.run = denied_ps
sys.argv = sys.argv[1:]
runpy.run_path(sys.argv[0], run_name="__main__")
''')
        result = subprocess.run(
            [sys.executable, str(runner), str(SCRIPTS / "smoke-claude-mcp-invoke.py"),
             "--", sys.executable, "-c", "pass"],
            env=dict(self.env, ORBIT_SMOKE_OBSERVED=str(self.root / "observed")),
            capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("cannot observe the process tree", result.stderr)

    def test_foreign_row_and_nonzero_exit_cannot_pass(self):
        """A matching row inserted beside this call, or a bound row after a
        non-zero exit, is not a pass. The smoke reports its own failure (1),
        not the provider's exit status."""
        for script in ('smoke-claude-mcp.sh', 'smoke-claude-mcp-sandbox.sh'):
            for mode, exit_code in (('foreign', '0'), ('foreign', '17'), ('requested', '17'), ('requested', '2')):
                with self.subTest(script=script, mode=mode, exit_code=exit_code):
                    result = self._run(script, mode, exit_code)
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    self.assertNotIn('PASS:', result.stdout)
                    if exit_code != '0':
                        self.assertIn(f'exited {exit_code}', result.stdout + result.stderr)
                        self.assertNotEqual(result.returncode, 17)


if __name__ == '__main__':
    unittest.main()
