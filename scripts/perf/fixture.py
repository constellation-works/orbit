#!/usr/bin/env python3
"""Freeze an Orbit workspace store into a perf fixture, and restore it.

    fixture.py snapshot --workspace <ws> --out <snapshot-dir> [--git-metadata <checkout>]
    fixture.py restore  --snapshot <snapshot-dir> --into <dir>
    fixture.py prepare  --into <dir> --binary <orbit>
    fixture.py clean    --into <dir>

`snapshot` reads the source only: every SQLite source is opened through a
`mode=ro` URI connection (`open_source_read_only`), files are copied by
reading them, and nothing writes to, reconciles or signals the live store or a
service. It then checks that every copied task-index row agrees with its
copied bundle, re-copying a drifted task's rows and bundle together and
refusing when they still disagree. `restore` verifies every digest in
`snapshot-manifest.json` before it lays out a disposable `HOME` and workspace
root. `prepare` brings a restored root to the state a live host running
`<orbit>` is in: it converges the managed defaults and re-records the task
envelope proofs with that binary. See docs/runbooks/perf-timing-protocol.md.
"""

from __future__ import annotations

import argparse
import datetime
import fnmatch
import importlib
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import time
import urllib.parse
from pathlib import Path

# Loaded by name after the insert: `python3 -I` keeps this directory off sys.path.
sys.path.insert(0, str(Path(__file__).resolve().parent))
common = importlib.import_module("perf_common")

SCHEMA_VERSION = 1
MANIFEST = "snapshot-manifest.json"
MANIFEST_SUMMARY = "snapshot-manifest-summary.json"
MAX_ARTIFACT_BYTES = 1_048_576
RESTORE_MARKER = ".orbit-perf-root.json"
# Columns that scope a row to a workspace. A row is kept when its value names
# the fixture workspace (registry id, name or storage id) or is NULL (host
# scoped); a table without either column is kept whole.
WORKSPACE_COLUMNS = ("workspace_id", "source_workspace")
FILTERED_DATABASES = ("orbit.db", "tasks/index.sqlite")
SQLITE_SUFFIXES = (".db", ".sqlite")
SQLITE_SIDECARS = ("-wal", "-shm", "-journal")
# Global-root entries copied whole (besides the filtered databases, the
# filtered registry and the workspace's task and friction trees).
GLOBAL_COPY = ("config.toml", "hosts.toml", "resources", "skills",
               "state/layout.version", "state/layout.compat")
# Workspace `.orbit` entries never copied: linked run worktrees, run scratch
# and disposable worker logs.
WORKSPACE_EXCLUDES = ("state/worktrees", "tmp", "state/logs")
# Per-worktree entries under `$GIT_DIR` that belong to a single checkout rather
# than the shared repository. When `--git-metadata` names a linked worktree,
# these are excluded from `common_dir` so the primary checkout's state cannot
# leak if the linked checkout lacks an entry (e.g. an orphan worktree's index).
GIT_PER_WORKTREE_PATHS = (
    "HEAD",
    "index",
    "ORIG_HEAD",
    "MERGE_HEAD",
    "MERGE_MODE",
    "MERGE_MSG",
    "SQUASH_MSG",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "FETCH_HEAD",
    "COMMIT_EDITMSG",
    "AUTO_MERGE",
    "BISECT_LOG",
    "BISECT_NAMES",
    "BISECT_START",
    "BISECT_TERMS",
    "BISECT_RUN",
    "BISECT_EXPECTED_REV",
    "BISECT_ANCESTORS_OK",
    "BISECT_FIRST_PARENT",
    "config.worktree",
    "info/sparse-checkout",
    "logs/HEAD",
    "modules",
    "rebase-apply",
    "rebase-merge",
    "refs/bisect",
    "refs/rewritten",
    "refs/worktree",
    "sequencer",
)
# Administrative entries in a linked worktree's `$GIT_DIR` that bind it to a
# shared repository or back to the source working tree.
GIT_LINKED_WORKTREE_EXCLUDES = ("commondir", "gitdir", "locked", "worktrees")
# Credential and env files are never copied, whatever directory holds them.
CREDENTIAL_NAMES = (
    ".zprofile", ".zshrc", ".zshenv", ".bashrc", ".bash_profile", ".profile",
    ".netrc", ".git-credentials", "clock.env", "auth.json", "credentials",
    "credentials.json", "credentials.toml",
)
CREDENTIAL_PATTERNS = (
    "*.env", ".env", ".env.*", "*token*", "*secret*", "*credential*",
    "*.key", "*_key", "*-key", "*.pem", "*.p12", "*.pfx", "id_rsa*",
    "id_ed25519*", "id_ecdsa*",
)
CREDENTIAL_DIRS = ("plugin-secrets", "ssh", ".ssh", ".gnupg", "secrets")
# The task index and the bundle tree are copied at different instants while
# drains write, so a task written in between has an index row from one moment
# and a bundle from another. One such row sends every task read of the
# restored root to a full bundle scan (ORB-26617). The check compares each
# row's `updated_at` with its copied `task.yaml`, then re-copies a drifted
# task's rows and bundle together, up to this many rounds, before refusing.
TASK_INDEX_DB = "tasks/index.sqlite"
TASK_INDEX_TABLE = "task_bundle_index"
TASK_BINDINGS_TABLE = "task_bundle_bindings"
TASK_ROW_KEYS = ("task_id", "source_task_id")
RECONCILE_ROUNDS = 5
# Commands `prepare` runs with an arm's own binary on a restored root, in
# order. `workspace sync` converges the managed defaults on that binary, as
# `orbit update` does on a live host; doctor opens read-only and never
# reconciles them. `task list` is a writable listing whose freshness scan
# records envelope proofs (task_envelope_stamps) for the restored files,
# which a restore gives new inodes and mtimes; read-only doctor never records
# them, so without this every doctor listing re-parses every task.yaml.
PREPARE_COMMANDS = (("workspace", "sync"), ("task", "list", "--json"))


# ── read-only source access ──────────────────────────────────────────────────


def open_source_read_only(path: Path) -> sqlite3.Connection:
    """The only way this script opens a source database: a `mode=ro` URI."""
    uri = "file:" + urllib.parse.quote(str(path)) + "?mode=ro"
    return sqlite3.connect(uri, uri=True)


def is_credential_like(rel: Path) -> bool:
    if any(part.lower() in CREDENTIAL_DIRS for part in rel.parts[:-1]):
        return True
    name = rel.name.lower()
    if name in CREDENTIAL_NAMES:
        return True
    return any(fnmatch.fnmatch(name, pattern) for pattern in CREDENTIAL_PATTERNS)


def quote_ident(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


class Snapshotter:
    def __init__(self, out: Path):
        self.out = out
        self.files: list[dict] = []
        self.databases: dict[str, dict] = {}
        self.excluded_credential_like = 0
        self.skipped_non_regular = 0

    # Plain files ──────────────────────────────────────────────────────────

    def _admit_source_file(self, src: Path, dest_rel: Path) -> bool:
        """Apply credential and non-regular exclusions before opening `src`."""
        if is_credential_like(dest_rel):
            self.excluded_credential_like += 1
            return False
        if src.is_symlink() or not src.is_file():
            self.skipped_non_regular += 1
            return False
        return True

    def copy_file(self, src: Path, dest_rel: Path) -> None:
        if not self._admit_source_file(src, dest_rel):
            return
        dest = self.out / dest_rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, dest)

    def copy_tree(self, src: Path, dest_rel: Path, excludes: tuple[str, ...] = (),
                  databases: bool = False, transform=None) -> None:
        if not src.exists():
            return
        if src.is_file():
            self.copy_file(src, dest_rel)
            return
        for dirpath, dirnames, filenames in os.walk(src):
            here = Path(dirpath)
            rel_here = here.relative_to(src)
            dirnames[:] = sorted(
                name for name in dirnames
                if (rel_here / name).as_posix() not in excludes
                and not (here / name).is_symlink()
            )
            # Empty directories matter: a task bundle without its
            # `artifacts/` directory reads as corrupt.
            for name in dirnames:
                (self.out / dest_rel / rel_here / name).mkdir(parents=True, exist_ok=True)
            for name in sorted(filenames):
                rel = rel_here / name
                if rel.as_posix() in excludes:
                    continue
                if databases and name.endswith(SQLITE_SIDECARS):
                    continue
                if databases and name.endswith(SQLITE_SUFFIXES):
                    self.backup_database(here / name, dest_rel / rel)
                    continue
                if transform is not None and transform(here / name, dest_rel / rel):
                    continue
                self.copy_file(here / name, dest_rel / rel)

    # SQLite ───────────────────────────────────────────────────────────────

    def backup_database(self, src: Path, dest_rel: Path) -> None:
        """Whole copy through SQLite's online backup API from a ro connection."""
        if not self._admit_source_file(src, dest_rel):
            return
        dest = self.out / dest_rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        source = open_source_read_only(src)
        try:
            target = sqlite3.connect(dest)
            try:
                source.backup(target)
                target.execute("PRAGMA journal_mode=DELETE")
            finally:
                target.close()
        finally:
            source.close()
        self.databases[dest_rel.as_posix()] = {
            "method": "online-backup-api", "filtered": False,
        }

    def filtered_database(self, src: Path, dest_rel: Path, keep: set[str]) -> None:
        """Copy only the fixture workspace's rows, in one read transaction.

        The online backup API copies every page, which for the shared global
        store means the whole multi-GB file plus a second pass to delete rows
        (and append-only triggers refuse those deletes). This copies the
        schema, then each table's kept rows, from one `mode=ro` connection
        inside a single read transaction, so the copy is one consistent
        snapshot of the WAL database without writing or locking it.
        """
        dest = self.out / dest_rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        source = open_source_read_only(src)
        target = sqlite3.connect(dest)
        target.execute("PRAGMA journal_mode=OFF")
        target.execute("PRAGMA synchronous=OFF")
        filters: dict[str, str | None] = {}
        try:
            source.execute("BEGIN")
            schema = source.execute(
                "SELECT type, name, tbl_name, sql FROM sqlite_master "
                "WHERE sql IS NOT NULL ORDER BY rowid"
            ).fetchall()
            for kind, name, _table, sql in schema:
                if kind == "table" and sql.upper().startswith("CREATE VIRTUAL"):
                    raise common.PerfRefusal(
                        f"{src}: virtual table {name} cannot be filtered; extend fixture.py"
                    )
            tables = [name for kind, name, _t, _s in schema
                      if kind == "table" and not name.startswith("sqlite_")]
            for kind, name, _table, sql in schema:
                if kind == "table" and not name.startswith("sqlite_"):
                    target.execute(sql)
            placeholders = ",".join("?" for _ in keep)
            for table in tables:
                columns = [row[1] for row in source.execute(
                    f"PRAGMA table_info({quote_ident(table)})")]
                scope = next((c for c in WORKSPACE_COLUMNS if c in columns), None)
                filters[table] = scope
                select = f"SELECT * FROM {quote_ident(table)}"
                params: tuple = ()
                if scope:
                    select += (f" WHERE {quote_ident(scope)} IN ({placeholders})"
                               f" OR {quote_ident(scope)} IS NULL")
                    params = tuple(sorted(keep))
                insert = (f"INSERT INTO {quote_ident(table)} VALUES "
                          f"({','.join('?' for _ in columns)})")
                cursor = source.execute(select, params)
                while batch := cursor.fetchmany(5000):
                    target.executemany(insert, batch)
            has_sequence = source.execute(
                "SELECT 1 FROM sqlite_master WHERE name='sqlite_sequence'").fetchone()
            if has_sequence:
                rows = source.execute("SELECT name, seq FROM sqlite_sequence").fetchall()
                target.execute("DELETE FROM sqlite_sequence")
                target.executemany("INSERT INTO sqlite_sequence VALUES (?, ?)", rows)
            for kind, _name, _table, sql in schema:
                if kind in ("index", "view", "trigger"):
                    target.execute(sql)
            for pragma in ("user_version", "application_id"):
                value = source.execute(f"PRAGMA {pragma}").fetchone()[0]
                target.execute(f"PRAGMA {pragma}={int(value)}")
            source.execute("COMMIT")
            target.commit()
            target.execute("PRAGMA journal_mode=DELETE")
        finally:
            target.close()
            source.close()
        self.databases[dest_rel.as_posix()] = {
            "method": "filtered-copy-single-read-transaction",
            "filtered": True,
            "scope_columns": {t: c for t, c in filters.items() if c},
            "kept_whole": sorted(t for t, c in filters.items() if not c),
        }

    # Manifest helpers ────────────────────────────────────────────────────

    def count_rows(self) -> None:
        for rel, info in self.databases.items():
            conn = sqlite3.connect(self.out / rel)
            try:
                tables = [r[0] for r in conn.execute(
                    "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")]
                info["tables"] = {
                    t: conn.execute(f"SELECT COUNT(*) FROM {quote_ident(t)}").fetchone()[0]
                    for t in tables
                }
            finally:
                conn.close()

    def distributions(self, store_rel: str) -> dict:
        path = self.out / store_rel
        result: dict = {}
        if not path.exists():
            return result
        conn = sqlite3.connect(path)
        try:
            def grouped(table: str, column: str) -> dict:
                exists = conn.execute(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?",
                    (table,)).fetchone()
                if not exists:
                    return {}
                return dict(conn.execute(
                    f"SELECT {column}, COUNT(*) FROM {quote_ident(table)} GROUP BY 1 ORDER BY 1"))
            result["job_runs_by_state"] = grouped("job_runs", "state")
            result["job_run_steps_by_state"] = grouped("job_run_steps", "state")
            result["routine_fires_by_state"] = grouped("routine_fires", "state")
            result["routine_fires_by_routine"] = grouped("routine_fires", "routine_name")
            def names(table: str) -> list:
                exists = conn.execute(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?",
                    (table,)).fetchone()
                if not exists:
                    return []
                return sorted(r[0] for r in conn.execute(
                    f"SELECT routine_name FROM {quote_ident(table)}"))
            result["routine_cursors"] = names("routine_cursors")
            result["routine_pauses"] = names("routine_pauses")
        except sqlite3.OperationalError as error:
            result["error"] = str(error)
        finally:
            conn.close()
        return result

    def list_files(self) -> None:
        self.files = []
        for dirpath, _dirs, filenames in os.walk(self.out):
            for name in filenames:
                path = Path(dirpath) / name
                rel = path.relative_to(self.out)
                if rel.as_posix() == MANIFEST:
                    continue
                self.files.append({
                    "path": rel.as_posix(),
                    "size": path.stat().st_size,
                    "sha256": common.sha256_file(path),
                })
        self.files.sort(key=lambda entry: entry["path"])


# ── task index consistency ───────────────────────────────────────────────────


_RFC3339 = re.compile(
    r"^(\d{4})-(\d{2})-(\d{2})[Tt ](\d{2}):(\d{2}):(\d{2})(?:\.(\d{1,9}))?"
    r"(Z|z|[+-]\d{2}:\d{2})$")
_ENVELOPE_UPDATED_AT = re.compile(r"^updated_at:[ \t]*(.*?)[ \t]*$", re.MULTILINE)


def rfc3339_instant(text: str) -> tuple[int, int] | None:
    """(Unix seconds, nanoseconds) of an RFC 3339 timestamp, or None.

    The index stores `+00:00` and the envelope `Z`, and either may carry 0–9
    fraction digits, so instants are compared, not strings.
    """
    match = _RFC3339.match(text.strip().strip("'\""))
    if not match:
        return None
    year, month, day, hour, minute, second = (int(g) for g in match.groups()[:6])
    fraction, offset = match.group(7) or "", match.group(8)
    try:
        moment = datetime.datetime(year, month, day, hour, minute, second,
                                   tzinfo=datetime.timezone.utc)
    except ValueError:
        return None
    seconds = int(moment.timestamp())
    if offset not in ("Z", "z"):
        sign = 1 if offset[0] == "+" else -1
        seconds -= sign * (int(offset[1:3]) * 3600 + int(offset[4:6]) * 60)
    return seconds, int(fraction.ljust(9, "0")) if fraction else 0


def envelope_updated_at(task_yaml: Path) -> str | None:
    """The top-level `updated_at` scalar of a copied `task.yaml`, or None."""
    try:
        text = task_yaml.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return None
    match = _ENVELOPE_UPDATED_AT.search(text)
    return match.group(1) if match else None


def _table_columns(conn: sqlite3.Connection, table: str) -> list[str]:
    return [row[1] for row in conn.execute(f"PRAGMA table_info({quote_ident(table)})")]


def _has_table(conn: sqlite3.Connection, table: str) -> bool:
    return conn.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name=?",
                        (table,)).fetchone() is not None


def task_index_mismatches(index_db: Path, bundles_root: Path,
                          keep: set[str]) -> dict[str, str] | None:
    """Task id → why its copied index rows and copied bundle disagree.

    Covers every task the copied index binds or indexes for the fixture
    workspace and every copied bundle directory: a task must have a binding,
    an index row and a bundle, and the row's `updated_at` must be the bundle
    envelope's. None when the copied index has no task bundle index (an
    older or synthetic store), which leaves nothing to compare.
    """
    conn = sqlite3.connect(index_db)
    try:
        if not _has_table(conn, TASK_INDEX_TABLE) or not _has_table(conn, TASK_BINDINGS_TABLE):
            return None
        placeholders = ",".join("?" for _ in keep)
        params = tuple(sorted(keep))
        indexed = {
            task_id: (workspace_id, updated_at)
            for task_id, workspace_id, updated_at in conn.execute(
                f"SELECT task_id, workspace_id, updated_at FROM {TASK_INDEX_TABLE} "
                f"WHERE workspace_id IN ({placeholders})", params)
        }
        bound = {
            task_id: workspace_id
            for task_id, workspace_id in conn.execute(
                f"SELECT task_id, workspace_id FROM {TASK_BINDINGS_TABLE} "
                f"WHERE workspace_id IN ({placeholders})", params)
        }
    finally:
        conn.close()
    bundles: dict[str, str] = {}
    for ident in sorted(keep):
        directory = bundles_root / ident
        if directory.is_dir():
            for entry in directory.iterdir():
                # Dot entries are the store's own coordination state
                # (`.task-commit-applying`), not task bundles.
                if entry.is_dir() and not entry.is_symlink() and not entry.name.startswith("."):
                    bundles.setdefault(entry.name, ident)
    problems: dict[str, str] = {}
    for task_id in sorted(set(indexed) | set(bound) | set(bundles)):
        if task_id not in bound:
            problems[task_id] = "no task_bundle_bindings row"
            continue
        if task_id not in indexed:
            problems[task_id] = "no task_bundle_index row"
            continue
        workspace_id, row_updated_at = indexed[task_id]
        task_yaml = bundles_root / workspace_id / task_id / "task.yaml"
        if task_id not in bundles or not task_yaml.is_file():
            problems[task_id] = f"index row has no copied bundle {workspace_id}/{task_id}/task.yaml"
            continue
        envelope_value = envelope_updated_at(task_yaml)
        if envelope_value is None:
            problems[task_id] = "copied task.yaml has no readable updated_at"
            continue
        row_instant = rfc3339_instant(row_updated_at or "")
        envelope_instant = rfc3339_instant(envelope_value)
        if row_instant is None or row_instant != envelope_instant:
            problems[task_id] = (f"index updated_at {row_updated_at} != "
                                 f"task.yaml updated_at {envelope_value}")
    return problems


def recopy_task_rows(source_db: Path, target_db: Path, task_ids: list[str],
                     keep: set[str]) -> None:
    """Replace the copied rows of `task_ids` in every task-keyed table.

    Rows come from one read transaction of a `mode=ro` source connection, with
    the same workspace filter the first copy applied; a task the source no
    longer has loses its copied rows.
    """
    source = open_source_read_only(source_db)
    target = sqlite3.connect(target_db)
    try:
        source.execute("BEGIN")
        id_marks = ",".join("?" for _ in task_ids)
        keep_marks = ",".join("?" for _ in keep)
        tables = [r[0] for r in target.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' "
            "AND sql NOT LIKE 'CREATE VIRTUAL%' ORDER BY name")]
        for table in tables:
            columns = _table_columns(target, table)
            key = next((c for c in TASK_ROW_KEYS if c in columns), None)
            if key is None or not _has_table(source, table):
                continue
            if _table_columns(source, table) != columns:
                raise common.PerfRefusal(
                    f"{source_db}: table {table} changed shape during the snapshot; take it again")
            tbl, col = quote_ident(table), quote_ident(key)
            select = f"SELECT * FROM {tbl} WHERE {col} IN ({id_marks})"
            params: tuple = tuple(task_ids)
            scope = next((c for c in WORKSPACE_COLUMNS if c in columns), None)
            if scope:
                select += (f" AND ({quote_ident(scope)} IN ({keep_marks})"
                           f" OR {quote_ident(scope)} IS NULL)")
                params += tuple(sorted(keep))
            rows = source.execute(select, params).fetchall()
            target.execute(f"DELETE FROM {tbl} WHERE {col} IN ({id_marks})", tuple(task_ids))
            target.executemany(
                f"INSERT INTO {tbl} VALUES ({','.join('?' for _ in columns)})", rows)
        source.execute("COMMIT")
        target.commit()
    finally:
        target.close()
        source.close()


def bind_foreign_relation_targets(source_db: Path, target_db: Path, keep: set[str]) -> int:
    """Keep the source bindings of relation targets registered to other workspaces.

    A relation target resolves when any workspace binds it. The filtered copy
    keeps only the fixture workspace's bindings, so a relation into another
    partition (ws_orbit relates to 791 tasks bound to another partition) read as
    dangling: doctor reported it as blocking an index rebuild, which live
    does not. Copies those `task_bundle_bindings` rows and their
    `workspace_bindings` rows, and nothing else of the other workspace. Returns
    how many bindings were added.
    """
    target = sqlite3.connect(target_db)
    try:
        if not all(_has_table(target, t) for t in
                   (TASK_BINDINGS_TABLE, "task_bundle_relations", "workspace_bindings")):
            return 0
        wanted = sorted(
            row[0] for row in target.execute(
                "SELECT DISTINCT target_task_id FROM task_bundle_relations WHERE target_task_id "
                f"NOT IN (SELECT task_id FROM {TASK_BINDINGS_TABLE})"))
        if not wanted:
            return 0
        source = open_source_read_only(source_db)
        try:
            source.execute("BEGIN")
            bindings: list = []
            for start in range(0, len(wanted), 500):
                chunk = wanted[start:start + 500]
                bindings += source.execute(
                    f"SELECT * FROM {TASK_BINDINGS_TABLE} WHERE task_id IN "
                    f"({','.join('?' for _ in chunk)})", chunk).fetchall()
            columns = _table_columns(target, TASK_BINDINGS_TABLE)
            workspace_index = columns.index("workspace_id")
            partitions = sorted({row[workspace_index] for row in bindings} - set(keep))
            workspaces = source.execute(
                f"SELECT * FROM workspace_bindings WHERE workspace_id IN "
                f"({','.join('?' for _ in partitions)})", partitions).fetchall() if partitions else []
            source.execute("COMMIT")
        finally:
            source.close()
        ws_columns = _table_columns(target, "workspace_bindings")
        target.executemany(
            f"INSERT OR IGNORE INTO workspace_bindings VALUES ({','.join('?' for _ in ws_columns)})",
            workspaces)
        target.executemany(
            f"INSERT OR IGNORE INTO {TASK_BINDINGS_TABLE} VALUES "
            f"({','.join('?' for _ in columns)})", bindings)
        target.commit()
        return len(bindings)
    finally:
        target.close()


def reconcile_task_index(snap: "Snapshotter", global_root: Path, home_rel: Path,
                         keep: set[str]) -> dict:
    """Make the copied task index and bundles agree, or refuse naming the drift.

    Each round re-copies every drifted task's index rows, then its bundle, so
    both come from one short window; a drain that rewrites the task again in
    that window leaves it drifted for the next round.
    """
    index_db = snap.out / home_rel / TASK_INDEX_DB
    bundles_root = snap.out / home_rel / "tasks" / "workspaces"
    if not index_db.is_file():
        return {"checked": False, "reason": f"no {TASK_INDEX_DB} in the source"}
    problems = task_index_mismatches(index_db, bundles_root, keep)
    if problems is None:
        return {"checked": False,
                "reason": f"{TASK_INDEX_DB} has no {TASK_INDEX_TABLE} table"}
    first = dict(problems)
    rounds = 0
    while problems and rounds < RECONCILE_ROUNDS:
        rounds += 1
        ids = sorted(problems)
        recopy_task_rows(global_root / TASK_INDEX_DB, index_db, ids, keep)
        for task_id in ids:
            for ident in sorted(keep):
                copied = bundles_root / ident / task_id
                if copied.exists():
                    shutil.rmtree(copied)
                snap.copy_tree(global_root / "tasks" / "workspaces" / ident / task_id,
                               home_rel / "tasks" / "workspaces" / ident / task_id)
        problems = task_index_mismatches(index_db, bundles_root, keep)
    if problems:
        shown = "; ".join(f"{task_id}: {why}" for task_id, why in list(problems.items())[:10])
        more = f" (+{len(problems) - 10} more)" if len(problems) > 10 else ""
        raise common.PerfRefusal(
            f"task index and bundles still disagree after {rounds} re-copy rounds, "
            f"refusing the snapshot: {shown}{more}")
    conn = sqlite3.connect(index_db)
    try:
        checked = conn.execute(
            f"SELECT COUNT(*) FROM {TASK_INDEX_TABLE} WHERE workspace_id IN "
            f"({','.join('?' for _ in keep)})", tuple(sorted(keep))).fetchone()[0]
    finally:
        conn.close()
    foreign = bind_foreign_relation_targets(global_root / TASK_INDEX_DB, index_db, keep)
    return {
        "checked": True,
        "tasks": checked,
        "foreign_relation_target_bindings": foreign,
        "drifted": [{"task_id": task_id, "reason": why} for task_id, why in first.items()],
        "reconcile_rounds": rounds,
    }


# ── workspace resolution ─────────────────────────────────────────────────────


_YAML_NULL_SCALARS = frozenset({"", "~", "null", "Null", "NULL"})
_QUOTED_SCALAR = re.compile(r"""(?:"([^"]*)"|'([^']*)')(?:[ \t]+#.*)?$""")


def read_storage_id(orbit_dir: Path) -> str:
    try:
        text = (orbit_dir / "config.yaml").read_text()
    except OSError:
        return ""
    # Same-line value only: `\s` would let a blank `workspace_id:` line capture the next key.
    match = re.search(r"^workspace_id:[ \t]*(.*)$", text, re.MULTILINE)
    if not match:
        return ""
    value = match.group(1).rstrip()
    if value[:1] in ("'", '"'):
        # A quoted scalar names its content; anything else after it is not a usable identity.
        quoted = _QUOTED_SCALAR.match(value)
        if not quoted:
            return ""
        return quoted.group(1) if quoted.group(1) is not None else quoted.group(2)
    # A plain scalar ends at a `#` that starts the value or follows whitespace (a YAML comment).
    value = re.sub(r"(^|[ \t]+)#.*$", "", value)
    return "" if value in _YAML_NULL_SCALARS else value


def resolve_workspace(global_root: Path, selector: str) -> dict:
    registry = json.loads((global_root / "workspaces.json").read_text())
    workspaces = registry.get("workspaces", []) or []
    checkouts = registry.get("checkouts", []) or []
    entry = next((w for w in workspaces if selector in (w.get("id"), w.get("name"))), None)
    checkout = None
    if entry is None:
        for candidate in checkouts:
            if read_storage_id(Path(candidate.get("orbit_dir", ""))) == selector:
                checkout = candidate
                entry = next((w for w in workspaces
                              if w.get("id") == candidate.get("workspace_id")), None)
                break
    if entry is None:
        raise common.PerfRefusal(f"workspace {selector!r} is not in {global_root}/workspaces.json")
    if checkout is None:
        owned = [c for c in checkouts if c.get("workspace_id") == entry["id"]]
        owned.sort(key=lambda c: c.get("role") != "owner")
        if not owned:
            raise common.PerfRefusal(f"workspace {entry['id']} has no local checkout")
        checkout = owned[0]
    orbit_dir = Path(checkout["orbit_dir"])
    storage_id = read_storage_id(orbit_dir)
    if not storage_id:
        # An empty identity equals any other empty identity, so refuse it here
        # rather than let restore match it.
        raise common.PerfRefusal(
            f"workspace {entry['id']} has no initialized identity: "
            f"{orbit_dir / 'config.yaml'} names no workspace_id")
    return {
        "registry": registry,
        "entry": entry,
        "checkout": checkout,
        "registry_id": entry["id"],
        "name": entry.get("name", ""),
        "storage_id": storage_id,
        "repo_root": checkout.get("repo_root", str(orbit_dir.parent)),
        "orbit_dir": str(orbit_dir),
    }


def filtered_registry(ws: dict, keep: set[str]) -> dict:
    registry = dict(ws["registry"])
    for key, value in list(registry.items()):
        if isinstance(value, list):
            registry[key] = [
                item for item in value
                if not isinstance(item, dict)
                or item.get("workspace_id", item.get("id")) in keep
            ]
    registry["checkouts"] = [ws["checkout"]]
    return registry


def scrub_git_config(text: str, *, non_bare: bool = False) -> str:
    """Drop remote URL userinfo and detach source worktree bindings in Git config."""
    scrubbed = re.sub(r"(://)[^/@\s]+@", r"\1", text)
    lines: list[str] = []
    in_core = False
    for line in scrubbed.splitlines(keepends=True):
        stripped = line.strip()
        if stripped.startswith("["):
            section = stripped.split("]", 1)[0][1:].strip().lower()
            in_core = section == "core"
            lines.append(line)
            continue
        if in_core:
            if "=" in stripped:
                key = stripped.split("=", 1)[0].strip().lower()
            else:
                key = stripped.split(None, 1)[0].lower() if stripped else ""
            if key == "worktree":
                # Drop core.worktree so the restored `.git` never binds to a source path.
                continue
            if non_bare and key == "bare":
                body = line.rstrip("\r\n")
                ending = line[len(body):]
                valueless = re.match(r"^(\s*bare)(\s*(?:[#;].*)?)$", body, flags=re.IGNORECASE)
                if valueless:
                    line = f"{valueless.group(1)} = false{valueless.group(2)}{ending}"
                else:
                    truthy = re.match(
                        r"^(\s*bare\s*=\s*)(?:([\"'])(true|yes|on|1)\2|(true|yes|on|1))"
                        r"(\s*(?:[#;].*)?)$",
                        body,
                        flags=re.IGNORECASE,
                    )
                    if truthy:
                        line = f"{truthy.group(1)}false{truthy.group(5)}{ending}"
        lines.append(line)
    return "".join(lines)


def _git_rev_parse_path(checkout: Path, flag: str) -> Path:
    out = subprocess.run(
        ["git", "-C", str(checkout), "rev-parse", "--path-format=absolute", flag],
        check=True, capture_output=True, text=True,
    ).stdout.strip()
    return Path(out)


def git_dir(checkout: Path) -> Path:
    return _git_rev_parse_path(checkout, "--git-dir")


def git_common_dir(checkout: Path) -> Path:
    return _git_rev_parse_path(checkout, "--git-common-dir")


def copy_git_metadata(snap: Snapshotter, out: Path, checkout: Path) -> dict:
    """Copy `checkout`'s Git metadata into a self-contained `checkout/.git`."""
    selected_git_dir = git_dir(checkout)
    common_dir = git_common_dir(checkout)
    linked = selected_git_dir.resolve() != common_dir.resolve()
    dest_root = Path("checkout/.git")

    def make_transform(root: Path, *, skip_shared_index: bool):
        def transform(src: Path, dest_rel: Path) -> bool:
            if src.name in ("config", "config.worktree") and src.parent == root:
                if src.is_symlink() or not src.is_file():
                    return True
                dest = out / dest_rel
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_text(scrub_git_config(src.read_text(), non_bare=linked))
                return True
            if skip_shared_index and src.parent == root and src.name.startswith("sharedindex."):
                return True
            return src.name.endswith(".lock")
        return transform

    common_excludes = ("worktrees",) + (GIT_PER_WORKTREE_PATHS if linked else ())
    snap.copy_tree(
        common_dir,
        dest_root,
        excludes=common_excludes,
        transform=make_transform(common_dir, skip_shared_index=linked),
    )
    if linked:
        reftable = (selected_git_dir / "reftable").is_dir()
        snap.copy_tree(
            selected_git_dir,
            dest_root,
            excludes=GIT_LINKED_WORKTREE_EXCLUDES + (("reftable",) if reftable else ()),
            transform=make_transform(selected_git_dir, skip_shared_index=False),
        )
        if reftable:
            # Reftable keeps shared and per-worktree refs in separate stacks.
            # Keep the shared stack, then replace its per-worktree refs,
            # pseudorefs and HEAD through Git so it writes a valid standalone
            # stack in the snapshot.
            dest_checkout = out / "checkout"
            prefixes = ("refs/worktree", "refs/bisect", "refs/rewritten")

            def git(cwd: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess:
                return subprocess.run(["git", "-C", str(cwd), *args], check=check,
                                      capture_output=True, text=True)

            fmt = "%(refname)%00%(objectname)%00%(symref)"

            def per_worktree_refs(cwd: Path) -> list[str]:
                refs = git(cwd, "for-each-ref", f"--format={fmt}", *prefixes).stdout.splitlines()
                root_refs = git(cwd, "for-each-ref", "--include-root-refs", f"--format={fmt}").stdout.splitlines()
                refs.extend(
                    line for line in root_refs
                    if not line.startswith("refs/") and line.split("\0", 1)[0] != "HEAD"
                )
                return refs

            for line in per_worktree_refs(dest_checkout):
                name = line.split("\0", 1)[0]
                git(dest_checkout, "update-ref", "--no-deref", "-d", name)
            for line in per_worktree_refs(checkout):
                name, oid, target = line.split("\0")
                if target:
                    git(dest_checkout, "symbolic-ref", name, target)
                else:
                    git(dest_checkout, "update-ref", name, oid)
            head = git(checkout, "symbolic-ref", "-q", "HEAD", check=False)
            if head.returncode == 0:
                git(dest_checkout, "symbolic-ref", "HEAD", head.stdout.strip())
            elif head.returncode == 1:
                oid = git(checkout, "rev-parse", "--verify", "HEAD").stdout.strip()
                git(dest_checkout, "update-ref", "--no-deref", "HEAD", oid)
            else:
                head.check_returncode()
    return {
        "checkout": str(checkout.resolve()),
        "git_dir": str(selected_git_dir),
        "common_dir": str(common_dir),
    }


# ── commands ─────────────────────────────────────────────────────────────────


def cmd_snapshot(args) -> int:
    global_root = Path(args.global_root or (common.real_home() / ".orbit")).resolve()
    out = Path(args.out).absolute()
    common.require_absent_or_empty(out)
    ws = resolve_workspace(global_root, args.workspace)
    keep = {v for v in (ws["registry_id"], ws["name"], ws["storage_id"]) if v}
    created = common.CreatedDirs(args.keep)
    created.create(out, output=True)
    ok = False
    try:
        snap = Snapshotter(out)
        epoch = time.time()
        home_rel = Path("home/.orbit")
        for rel in FILTERED_DATABASES:
            src = global_root / rel
            if src.exists():
                snap.filtered_database(src, home_rel / rel, keep)
        registry_path = out / home_rel / "workspaces.json"
        registry_path.parent.mkdir(parents=True, exist_ok=True)
        common.write_json(registry_path, filtered_registry(ws, keep))
        for rel in GLOBAL_COPY:
            snap.copy_tree(global_root / rel, home_rel / rel)
        for ident in sorted(keep):
            snap.copy_tree(global_root / "tasks" / "workspaces" / ident,
                           home_rel / "tasks" / "workspaces" / ident)
            snap.copy_tree(global_root / "frictions" / "workspaces" / ident,
                           home_rel / "frictions" / "workspaces" / ident)
        consistency = reconcile_task_index(snap, global_root, home_rel, keep)
        excludes = WORKSPACE_EXCLUDES + tuple(args.exclude or ())
        snap.copy_tree(Path(ws["orbit_dir"]), Path("checkout/.orbit"),
                       excludes=excludes, databases=True)
        git_info = None
        if args.git_metadata:
            git_info = copy_git_metadata(snap, out, Path(args.git_metadata))
        snap.count_rows()
        snap.list_files()
        manifest = {
            "schemaVersion": SCHEMA_VERSION,
            "kind": "orbit-perf-snapshot",
            "source": {
                "selector": args.workspace,
                "registry_id": ws["registry_id"],
                "name": ws["name"],
                "storage_id": ws["storage_id"],
                "global_root": str(global_root),
                "repo_root": ws["repo_root"],
                "orbit_dir": ws["orbit_dir"],
                "home": str(global_root.parent),
            },
            "source_access": (
                "read-only: every SQLite source was opened with a mode=ro URI "
                "connection (fixture.py open_source_read_only); files were copied by "
                "reading; nothing wrote to, locked for writing, reconciled or signalled "
                "the source store or any service"
            ),
            "snapshot_epoch": int(epoch),
            "snapshot_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(epoch)),
            "workspace_keys": sorted(keep),
            "databases": snap.databases,
            "state_distributions": snap.distributions((home_rel / "orbit.db").as_posix()),
            "git_metadata": git_info,
            "task_index_consistency": consistency,
            "excluded": {
                "credential_like_files": snap.excluded_credential_like,
                "non_regular_files": snap.skipped_non_regular,
                "workspace_paths": list(excludes),
            },
            "files": snap.files,
        }
        common.write_json(out / MANIFEST, manifest)
        ok = True
        print(json.dumps({"snapshot": str(out), "files": len(snap.files),
                          "workspace": ws["registry_id"]}))
        return 0
    finally:
        created.finish(ok)


def verify_snapshot(snapshot: Path) -> dict:
    manifest_path = snapshot / MANIFEST
    try:
        manifest = json.loads(manifest_path.read_text())
    except (OSError, ValueError) as error:
        raise common.PerfRefusal(f"cannot read {manifest_path}: {error}") from None
    if manifest.get("schemaVersion") != SCHEMA_VERSION:
        raise common.PerfRefusal(f"{manifest_path}: unsupported schemaVersion")
    problems = []
    listed = set()
    for entry in manifest["files"]:
        path = snapshot / entry["path"]
        listed.add(entry["path"])
        if not path.is_file():
            problems.append(f"missing {entry['path']}")
            continue
        if path.stat().st_size != entry["size"]:
            problems.append(f"size mismatch {entry['path']}")
            continue
        if common.sha256_file(path) != entry["sha256"]:
            problems.append(f"sha256 mismatch {entry['path']}")
    for dirpath, _dirs, filenames in os.walk(snapshot):
        for name in filenames:
            rel = (Path(dirpath) / name).relative_to(snapshot).as_posix()
            if rel != MANIFEST and rel not in listed:
                problems.append(f"unlisted file {rel}")
    if problems:
        shown = "; ".join(problems[:10])
        more = f" (+{len(problems) - 10} more)" if len(problems) > 10 else ""
        raise common.PerfRefusal(
            f"snapshot {snapshot} failed digest verification, refusing to restore: {shown}{more}")
    return manifest


def manifest_evidence_summary(snapshot: Path, manifest: dict,
                              revisions: dict[str, str],
                              qualifying_task_counts: dict[str, int]) -> dict:
    """Describe a verified host-side manifest without copying its file list."""
    path = (snapshot / MANIFEST).resolve(strict=True)
    digest = common.sha256_file(path)
    workspace_id = manifest["source"]["registry_id"]
    snapshot_epoch = manifest["snapshot_epoch"]
    return {
        "schemaVersion": SCHEMA_VERSION,
        "kind": "orbit-perf-snapshot-manifest-summary",
        "manifest": {
            "path_on_host": str(path),
            "size_bytes": path.stat().st_size,
            "sha256": digest,
            "file_count": len(manifest["files"]),
        },
        "qualifying_task_counts": dict(sorted(qualifying_task_counts.items())),
        "snapshot": {
            "id": f"{workspace_id}:{snapshot_epoch}:{digest[:16]}",
            "epoch": snapshot_epoch,
            "source_workspace": workspace_id,
        },
        "revisions": dict(sorted(revisions.items())),
    }


def _path_rewrites(manifest: dict, into: Path) -> list[tuple[str, str]]:
    source = manifest["source"]
    pairs = [
        (source["orbit_dir"], str(into / "checkout" / ".orbit")),
        (source["global_root"], str(into / "home" / ".orbit")),
        (source["repo_root"], str(into / "checkout")),
        (source["home"], str(into / "home")),
    ]
    return sorted(pairs, key=lambda pair: len(pair[0]), reverse=True)


TERMINATORS = ("/", '"', "'", "\n")
TEXT_REWRITE_MAX_BYTES = 4 << 20


def _compile_path_rewriter(pairs: list[tuple[str, str]]):
    """Compile a single-pass path-prefix rewriter shared by text and SQLite paths.

    Matches each stored path at most once (longest prefix first) so a
    replacement destination nested under a source root is never re-scanned.
    """
    ordered: list[tuple[str, str]] = []
    seen: set[str] = set()
    for old, new in sorted(pairs, key=lambda pair: len(pair[0]), reverse=True):
        if old and old not in seen:
            seen.add(old)
            ordered.append((old, new))
    if not ordered:
        return None
    branches = []
    for old, _new in ordered:
        escaped = re.escape(old)
        if old.startswith("/"):
            branches.append(rf"(?:^|(?<=[\s\"'=:,;(\[{{<`])|(?<=://)){escaped}")
        else:
            branches.append(escaped)
    end_chars = "".join(re.escape(end) for end in TERMINATORS)
    pattern = re.compile(rf"(?P<path>{'|'.join(branches)})(?=[{end_chars}]|\Z)")
    mapping = dict(ordered)

    def apply_rewrite(text: str) -> str:
        return pattern.sub(lambda match: mapping[match.group("path")], text)

    return apply_rewrite, [old for old, _new in ordered]


def rewrite_text(text: str, pairs: list[tuple[str, str]]) -> str:
    compiled = _compile_path_rewriter(pairs)
    if compiled is None:
        return text
    apply_rewrite, _olds = compiled
    return apply_rewrite(text)


def rewrite_text_file(path: Path, pairs: list[tuple[str, str]]) -> None:
    """Point absolute source paths in a small text file at the restored root.

    Registry, config, generation-authority and layout files record absolute
    paths; leaving them would bind the restored root back to the source.
    """
    if path.stat().st_size > TEXT_REWRITE_MAX_BYTES:
        return
    try:
        text = path.read_text(encoding="utf-8")
    except (UnicodeDecodeError, OSError):
        return
    rewritten = rewrite_text(text, pairs)
    if rewritten != text:
        path.write_text(rewritten, encoding="utf-8")


def _rewrite_table_paths(conn: sqlite3.Connection, table: str, olds: list[str]) -> None:
    """Rewrite every text value in `table` with one UPDATE.

    One statement scans the table once. Each column is assigned from its
    original value (SQLite evaluates every SET expression against the row as
    it was), and only a text value that actually changes is rewritten. The
    stored text matches one application of ``perf_rewrite_path``, the same
    result as updating each column on its own.
    """
    tbl = quote_ident(table)
    columns = [row[1] for row in conn.execute(f"PRAGMA table_info({tbl})")]
    if not columns:
        return
    assignments: list[str] = []
    predicates: list[str] = []
    params: list[str] = []
    for column in columns:
        col = quote_ident(column)
        instr = " OR ".join(f"instr({col}, ?) > 0" for _old in olds)
        predicate = (
            f"typeof({col}) = 'text' AND ({instr}) "
            f"AND perf_rewrite_path({col}) != {col}"
        )
        assignments.append(
            f"{col} = CASE WHEN {predicate} THEN perf_rewrite_path({col}) ELSE {col} END"
        )
        params.extend(olds)
        predicates.append(f"({predicate})")
        params.extend(olds)
    conn.execute(
        f"UPDATE {tbl} SET {', '.join(assignments)} WHERE {' OR '.join(predicates)}",
        tuple(params),
    )


def rewrite_database(path: Path, pairs: list[tuple[str, str]]) -> None:
    """Point stored absolute paths in `path` at the restored root.

    Each ordinary table is one UPDATE (see ``_rewrite_table_paths``), so a
    multi-gigabyte store is scanned once per table rather than once per text
    column. Triggers are dropped for the write and recreated afterwards, so
    they cannot observe the intermediate rows.
    """
    compiled = _compile_path_rewriter(pairs)
    if compiled is None:
        return
    apply_rewrite, olds = compiled
    conn = sqlite3.connect(path)
    try:
        conn.create_function("perf_rewrite_path", 1, apply_rewrite, deterministic=True)
        triggers = conn.execute(
            "SELECT name, sql FROM sqlite_master WHERE type='trigger'").fetchall()
        for name, _sql in triggers:
            conn.execute(f"DROP TRIGGER {quote_ident(name)}")
        tables = [row[0] for row in conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND sql NOT LIKE 'CREATE VIRTUAL%'"
            " AND name NOT LIKE 'sqlite_%'")]
        for table in tables:
            _rewrite_table_paths(conn, table, olds)
        for _name, sql in triggers:
            conn.execute(sql)
        conn.commit()
    finally:
        conn.close()


def restore_root(snapshot: Path, into: Path, manifest: dict | None = None) -> dict:
    """Verify `snapshot`, lay out a disposable root at `into`, return its env."""
    into = into.absolute()
    common.refuse_protected(into, "restore into")
    if manifest is None:
        manifest = verify_snapshot(snapshot)
    common.require_absent_or_empty(into)
    common.disk_gate(into)
    into.mkdir(parents=True)
    try:
        return _populate_root(snapshot, into, manifest)
    except BaseException:
        shutil.rmtree(into, ignore_errors=True)
        raise


def _is_verified_task_artifact_blob(parts: tuple[str, ...]) -> bool:
    """True for a blob under ``tasks/workspaces/<id>/<task>/artifacts/files``.

    Those bytes are named by ``artifacts/manifest.yaml`` with a size and
    SHA-256. A source path inside one is payload, not a workspace binding:
    rewriting it makes a later bundle read reject the task as corrupt.
    """
    for index in range(4, len(parts) - 1):
        if (
            parts[index - 4] == "tasks"
            and parts[index - 3] == "workspaces"
            and parts[index] == "artifacts"
            and parts[index + 1] == "files"
        ):
            return True
    return False


def _populate_root(snapshot: Path, into: Path, manifest: dict) -> dict:
    source = manifest["source"]
    if not source["storage_id"]:
        # A snapshot of an identity-less source records an empty id. The restored
        # root would also have an empty id, and the equality check below would
        # accept the two.
        raise common.PerfRefusal(
            f"snapshot {snapshot} records no workspace identity for "
            f"{source['name'] or source['registry_id']}; take it from an initialized workspace")
    shutil.copytree(snapshot / "home", into / "home")
    shutil.copytree(snapshot / "checkout", into / "checkout")
    pairs = _path_rewrites(manifest, into)
    for dirpath, _dirs, filenames in os.walk(into):
        for name in filenames:
            path = Path(dirpath) / name
            rel_parts = path.relative_to(into).parts
            if _is_verified_task_artifact_blob(rel_parts):
                continue
            if name.endswith(SQLITE_SUFFIXES):
                rewrite_database(path, pairs)
            elif ".git" not in rel_parts:
                rewrite_text_file(path, pairs)
    if fixture_storage_id(into) != source["storage_id"]:
        # Without an initialized `.orbit/` in the working directory, root
        # resolution would fall through to the enclosing checkout.
        raise common.PerfRefusal(
            f"restored {into / 'checkout' / '.orbit'} is not the initialized workspace "
            f"{source['storage_id']}")
    env = {
        "HOME": str(into / "home"),
        "USERPROFILE": str(into / "home"),
        "XDG_CONFIG_HOME": str(into / "home" / ".config"),
        "GIT_CEILING_DIRECTORIES": str(into),
    }
    info = {
        "schemaVersion": SCHEMA_VERSION,
        "snapshot": str(snapshot.absolute()),
        "snapshot_epoch": manifest["snapshot_epoch"],
        "root": str(into),
        "cwd": str(into / "checkout"),
        "env": env,
        "workspace": source["name"] or source["registry_id"],
        "registry_id": source["registry_id"],
    }
    common.write_json(into / RESTORE_MARKER, info)
    return info


def fixture_storage_id(into: Path) -> str:
    return read_storage_id(into / "checkout" / ".orbit")


def isolated_env(info: dict) -> dict:
    """Process env for a restored root: no inherited Orbit or Git authority."""
    env = {k: v for k, v in os.environ.items()
           if not k.startswith("ORBIT_") and not k.startswith("GIT_")}
    env.update(info["env"])
    return env


def cmd_restore(args) -> int:
    info = restore_root(Path(args.snapshot), Path(args.into))
    for key, value in info["env"].items():
        print(f"export {key}={json.dumps(value)}")
    print(f"cd {json.dumps(info['cwd'])}")
    print(f"# workspace selector: --workspace {info['workspace']}  (registry id {info['registry_id']})")
    print("# unset every inherited ORBIT_* and GIT_* variable before running orbit here")
    print("# before timing a binary here: fixture.py prepare --into <dir> --binary <that orbit>")
    return 0


def restored_info(into: Path) -> dict:
    """The env record `restore` wrote into a restored root."""
    marker = into.absolute() / RESTORE_MARKER
    try:
        return json.loads(marker.read_text())
    except (OSError, ValueError) as error:
        raise common.PerfRefusal(f"{into} is not a restored root: cannot read {marker}: {error}") from None


def prepare_failure(binary: Path, argv: tuple[str, ...], how: str) -> common.PerfRefusal:
    return common.PerfRefusal(
        f"preparing the restored root with {binary} failed: `orbit {' '.join(argv)}` {how}")


def cmd_prepare(args) -> int:
    into = Path(args.into).absolute()
    common.refuse_protected(into, "prepare")
    info = restored_info(into)
    binary = Path(args.binary).absolute()
    env = isolated_env(info)
    ran = []
    for argv in PREPARE_COMMANDS:
        started = time.perf_counter()
        try:
            result = subprocess.run([str(binary), *argv], cwd=info["cwd"], env=env,
                                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                    stderr=subprocess.PIPE, timeout=args.timeout)
        except subprocess.TimeoutExpired:
            raise prepare_failure(binary, argv, f"ran past {args.timeout:g} s") from None
        except OSError as error:
            raise prepare_failure(binary, argv, f"could not start: {error}") from None
        if result.returncode != 0:
            tail = result.stderr.decode("utf-8", "replace").strip().splitlines()[-5:]
            raise prepare_failure(binary, argv, f"exited {result.returncode}: {' | '.join(tail)}")
        ran.append({"command": " ".join(argv), "exit": 0,
                    "ms": round((time.perf_counter() - started) * 1000, 1)})
    print(json.dumps({"prepared": str(into), "binary": str(binary), "commands": ran}))
    return 0


def clean_root(into: Path) -> None:
    into = into.absolute()
    common.refuse_protected(into, "clean")
    if not into.exists():
        return
    if not (into / RESTORE_MARKER).is_file():
        raise common.PerfRefusal(f"refusing to clean {into}: no {RESTORE_MARKER} marker")
    shutil.rmtree(into)


def cmd_clean(args) -> int:
    clean_root(Path(args.into))
    print(f"removed {Path(args.into).absolute()}")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = parser.add_subparsers(dest="command", required=True)
    snap = sub.add_parser("snapshot", help="freeze a workspace store into a fixture")
    snap.add_argument("--workspace", required=True,
                      help="registry id, name or storage workspace_id")
    snap.add_argument("--out", required=True, help="new snapshot directory")
    snap.add_argument("--git-metadata", help="also copy this checkout's Git metadata")
    snap.add_argument("--global-root",
                      help="source global root (default: ~/.orbit of the passwd home)")
    snap.add_argument("--exclude", action="append",
                      help="workspace .orbit-relative path to leave out (repeatable)")
    snap.add_argument("--keep", action="store_true", help="keep partial output on failure")
    restore = sub.add_parser("restore", help="make a disposable root from a snapshot")
    restore.add_argument("--snapshot", required=True)
    restore.add_argument("--into", required=True)
    prepare = sub.add_parser(
        "prepare", help="converge a restored root's defaults and envelope proofs with one binary")
    prepare.add_argument("--into", required=True, help="a root `restore` made")
    prepare.add_argument("--binary", required=True, help="the orbit binary that will run there")
    prepare.add_argument("--timeout", type=float, default=300.0,
                         help="seconds each prepare command may run")
    clean = sub.add_parser("clean", help="remove a restored root")
    clean.add_argument("--into", required=True)
    args = parser.parse_args(argv)
    os.umask(0o077)
    try:
        return {"snapshot": cmd_snapshot, "restore": cmd_restore, "prepare": cmd_prepare,
                "clean": cmd_clean}[args.command](args)
    except common.PerfRefusal as refusal:
        print(f"fixture.py {args.command}: {refusal}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
