#!/usr/bin/env python3
"""CLI regression tests for scripts/perf/fixture.py snapshot and restore."""

from __future__ import annotations

import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest

PERF_DIR = Path(__file__).resolve().parent
ROOT = PERF_DIR.parent.parent
FIXTURE_PY = PERF_DIR / "fixture.py"

sys.path.insert(0, str(PERF_DIR))
import perf_common  # noqa: E402


def _scratch_parent() -> Path | None:
    """Return workspace `.orbit/tmp` when allowed by perf_common, else system tmp."""
    candidate = ROOT / ".orbit" / "tmp"
    candidate.mkdir(parents=True, exist_ok=True)
    probe = candidate / "fixture-cli-probe"
    try:
        perf_common.refuse_protected(probe, "restore into")
    except perf_common.PerfRefusal:
        return None
    return candidate


def _child_env(home: Path) -> dict[str, str]:
    env = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith("ORBIT_") and not key.startswith("GIT_")
    }
    env["HOME"] = str(home)
    env["ORBIT_PERF_SIMULATE_DISK_PCENT"] = "10"
    return env


def _build_disposable_source(source_home: Path, source_repo: Path) -> dict[str, Path]:
    """Create a self-contained disposable Orbit home + checkout for snapshot/restore."""
    source_global = source_home / ".orbit"
    source_orbit = source_repo / ".orbit"
    task_dir = source_global / "tasks" / "workspaces" / "ws_fixture" / "ORB-1"
    state_dir = source_global / "state"
    resources_dir = source_global / "resources"

    for directory in (
        task_dir / "artifacts",
        state_dir,
        resources_dir,
        source_global / "tasks",
        source_orbit / "state" / "worktrees" / "run-0" / ".orbit" / "tmp",
    ):
        directory.mkdir(parents=True, exist_ok=True)

    registry_path = source_global / "workspaces.json"
    registry_path.write_text(
        json.dumps(
            {
                "schema_version": 1,
                "workspaces": [
                    {
                        "id": "ws_fixture",
                        "name": "fixture-ws",
                        "created_at": "2026-11-21T00:00:00Z",
                    }
                ],
                "checkouts": [
                    {
                        "workspace_id": "ws_fixture",
                        "role": "owner",
                        "repo_root": str(source_repo),
                        "orbit_dir": str(source_orbit),
                    }
                ],
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )

    config_toml = source_global / "config.toml"
    config_toml.write_text(
        "\n".join(
            [
                f'global_root = "{source_global}"',
                f'repo_root = "{source_repo}"',
                f'orbit_dir = "{source_orbit}"',
                f'home_dir = "{source_home}"',
                f"single_quoted = '{source_orbit}/cache'",
                f'uri = "file://{source_repo}"',
                f"unquoted_state = {source_global}/state",
                'ordinary = "ordinary text mentioning source home and checkout"',
                f'sibling_home = "{source_home}-other/repo"',
                f'sibling_repo_in_home = "{source_repo}2/file"',
                f'unrelated_nested = "/archive{source_home}/backup"',
                f'unrelated_orbit = "/backup{source_orbit}/config.yaml"',
                'unrelated_external = "/opt/other/orbit/file"',
                "",
            ]
        ),
        encoding="utf-8",
    )

    layout_compat = state_dir / "layout.compat"
    # Deliberately no trailing newline to verify EOF path replacement.
    layout_compat.write_text(str(source_orbit), encoding="utf-8")

    layout_version = state_dir / "layout.version"
    layout_version.write_text("1\n", encoding="utf-8")

    workspace_config = source_orbit / "config.yaml"
    workspace_config.write_text(
        "\n".join(
            [
                "workspace_id: ws_fixture",
                f"repo_root: {source_repo}",
                f"global_root: {source_global}",
                f"unrelated: /mirror{source_repo}",
                'note: "ordinary text with /unrelated/path"',
                "",
            ]
        ),
        encoding="utf-8",
    )

    task_json = task_dir / "task.json"
    task_json.write_text(
        json.dumps(
            {
                "id": "ORB-1",
                "worktree_path": f"{source_orbit}/state/worktrees/run-0",
                "description": f"ordinary text and /backup{source_home}/note",
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )

    # Global SQLite store (`orbit.db`): includes exact paths, subpaths, JSON,
    # multiline newline-separated paths, a trigger, and a WITHOUT ROWID table.
    orbit_db = source_global / "orbit.db"
    conn = sqlite3.connect(orbit_db)
    try:
        conn.executescript(
            """
            CREATE TABLE job_runs (
                id TEXT PRIMARY KEY,
                workspace_id TEXT,
                orbit_dir TEXT,
                repo_root TEXT,
                global_root TEXT,
                home_dir TEXT,
                worktree_path TEXT,
                payload_json TEXT,
                multiline_paths TEXT,
                ordinary_note TEXT,
                unrelated_path TEXT
            );
            CREATE TRIGGER job_runs_no_delete
            BEFORE DELETE ON job_runs
            BEGIN
                SELECT RAISE(ABORT, 'append-only');
            END;
            CREATE TABLE kv_without_rowid (
                workspace_id TEXT NOT NULL,
                key TEXT NOT NULL,
                val TEXT NOT NULL,
                PRIMARY KEY (workspace_id, key)
            ) WITHOUT ROWID;
            """
        )
        conn.execute(
            """
            INSERT INTO job_runs VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                "run-1",
                "ws_fixture",
                str(source_orbit),
                str(source_repo),
                str(source_global),
                str(source_home),
                f"{source_orbit}/state/worktrees/run-0",
                json.dumps(
                    {
                        "orbit_dir": str(source_orbit),
                        "home": str(source_home),
                        "unrelated": f"/backup{source_home}/x",
                        "sibling_in_home": f"{source_repo}-alt",
                    }
                ),
                f"{source_repo}\n{source_global}\nordinary text",
                "ordinary text without orbit paths",
                f"/archive{source_home}/data",
            ),
        )
        conn.execute(
            "INSERT INTO kv_without_rowid VALUES (?, ?, ?)",
            ("ws_fixture", "orbit_dir", str(source_orbit)),
        )
        conn.execute(
            "INSERT INTO kv_without_rowid VALUES (?, ?, ?)",
            ("ws_fixture", "unrelated", f"{source_home}_other/file"),
        )
        conn.commit()
    finally:
        conn.close()

    # Task index SQLite database (`tasks/index.sqlite`).
    index_db = source_global / "tasks" / "index.sqlite"
    conn = sqlite3.connect(index_db)
    try:
        conn.execute(
            "CREATE TABLE task_index (id TEXT PRIMARY KEY, workspace_id TEXT, bundle_path TEXT)"
        )
        conn.execute(
            "INSERT INTO task_index VALUES (?, ?, ?)",
            ("ORB-1", "ws_fixture", str(task_json)),
        )
        conn.commit()
    finally:
        conn.close()

    # Workspace-local SQLite database (`checkout/.orbit/workspace.db`).
    workspace_db = source_orbit / "workspace.db"
    conn = sqlite3.connect(workspace_db)
    try:
        conn.execute(
            "CREATE TABLE local_state (k TEXT PRIMARY KEY, v TEXT)"
        )
        conn.executemany(
            "INSERT INTO local_state VALUES (?, ?)",
            [
                ("orbit_dir", str(source_orbit)),
                ("repo_root", str(source_repo)),
                ("ordinary", "ordinary text"),
                ("unrelated", f"/mirror{source_orbit}"),
            ],
        )
        conn.commit()
    finally:
        conn.close()

    return {
        "home": source_home,
        "global_root": source_global,
        "repo_root": source_repo,
        "orbit_dir": source_orbit,
    }


class FixtureCliRestoreTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(
            prefix="fixture-cli-", dir=_scratch_parent()
        )
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name)

    def run_fixture(self, env: dict[str, str], *args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, "-I", str(FIXTURE_PY), *args],
            env=env,
            text=True,
            capture_output=True,
            timeout=60,
        )

    def test_restore_with_nested_managed_run_scratch_rewrites_paths_once(self):
        source_home = self.base / "source"
        # Nested under source_home (owner-host layout) and sibling layout where
        # scratch sits inside source_orbit/state/worktrees/<run>/.orbit/tmp.
        source_repo = source_home / "ws" / "orbit"
        paths = _build_disposable_source(source_home, source_repo)

        # Keep snapshot outside excluded worktree directories, and test restore targets
        # nested under both source_home and source_repo/.orbit, as well as under
        # source_global.
        snapshot_dir = self.base / "snapshot"
        env = _child_env(source_home)

        snap_res = self.run_fixture(
            env,
            "snapshot",
            "--workspace",
            "ws_fixture",
            "--global-root",
            str(paths["global_root"]),
            "--out",
            str(snapshot_dir),
        )
        self.assertEqual(snap_res.returncode, 0, snap_res.stderr)

        restore_targets = {
            "under_checkout_and_home": (
                paths["orbit_dir"] / "state" / "worktrees" / "run-1" / ".orbit" / "tmp" / "root"
            ),
            "under_global_root": (
                paths["global_root"] / "state" / "worktrees" / "run-2" / ".orbit" / "tmp" / "root"
            ),
        }

        for label, restore_into in restore_targets.items():
            with self.subTest(layout=label):
                restore_into.parent.mkdir(parents=True, exist_ok=True)
                res = self.run_fixture(
                    env,
                    "restore",
                    "--snapshot",
                    str(snapshot_dir),
                    "--into",
                    str(restore_into),
                )
                self.assertEqual(res.returncode, 0, res.stderr)

                expected_home = restore_into / "home"
                expected_global = expected_home / ".orbit"
                expected_repo = restore_into / "checkout"
                expected_orbit = expected_repo / ".orbit"

                # 1. Registry resolves directly to restored checkout and orbit_dir.
                registry = json.loads((expected_global / "workspaces.json").read_text())
                checkout = registry["checkouts"][0]
                self.assertEqual(checkout["repo_root"], str(expected_repo))
                self.assertEqual(checkout["orbit_dir"], str(expected_orbit))
                self.assertTrue(Path(checkout["repo_root"]).is_dir())
                self.assertTrue((Path(checkout["orbit_dir"]) / "config.yaml").is_file())

                # 2. Config and layout files resolve to restored paths and preserve unrelated text.
                config_text = (expected_global / "config.toml").read_text()
                expected_config = "\n".join(
                    [
                        f'global_root = "{expected_global}"',
                        f'repo_root = "{expected_repo}"',
                        f'orbit_dir = "{expected_orbit}"',
                        f'home_dir = "{expected_home}"',
                        f"single_quoted = '{expected_orbit}/cache'",
                        f'uri = "file://{expected_repo}"',
                        f"unquoted_state = {expected_global}/state",
                        'ordinary = "ordinary text mentioning source home and checkout"',
                        f'sibling_home = "{source_home}-other/repo"',
                        f'sibling_repo_in_home = "{expected_home}/ws/orbit2/file"',
                        f'unrelated_nested = "/archive{source_home}/backup"',
                        f'unrelated_orbit = "/backup{paths["orbit_dir"]}/config.yaml"',
                        'unrelated_external = "/opt/other/orbit/file"',
                        "",
                    ]
                )
                self.assertEqual(config_text, expected_config)

                self.assertEqual(
                    (expected_global / "state" / "layout.compat").read_text(),
                    str(expected_orbit),
                )

                ws_yaml = (expected_orbit / "config.yaml").read_text()
                self.assertEqual(
                    ws_yaml,
                    "\n".join(
                        [
                            "workspace_id: ws_fixture",
                            f"repo_root: {expected_repo}",
                            f"global_root: {expected_global}",
                            f"unrelated: /mirror{source_repo}",
                            'note: "ordinary text with /unrelated/path"',
                            "",
                        ]
                    ),
                )

                restored_task_json = (
                    expected_global / "tasks" / "workspaces" / "ws_fixture" / "ORB-1" / "task.json"
                )
                task_data = json.loads(restored_task_json.read_text())
                self.assertEqual(
                    task_data["worktree_path"],
                    f"{expected_orbit}/state/worktrees/run-0",
                )
                self.assertEqual(
                    task_data["description"],
                    f"ordinary text and /backup{source_home}/note",
                )

                # 3. Global store (`orbit.db`) paths resolve once, including multiline and WITHOUT ROWID,
                #    triggers remain intact, and ordinary/unrelated values stay unchanged.
                conn = sqlite3.connect(expected_global / "orbit.db")
                try:
                    row = conn.execute(
                        "SELECT orbit_dir, repo_root, global_root, home_dir, worktree_path, "
                        "payload_json, multiline_paths, ordinary_note, unrelated_path "
                        "FROM job_runs WHERE id = 'run-1'"
                    ).fetchone()
                    self.assertEqual(row[0], str(expected_orbit))
                    self.assertEqual(row[1], str(expected_repo))
                    self.assertEqual(row[2], str(expected_global))
                    self.assertEqual(row[3], str(expected_home))
                    self.assertEqual(row[4], f"{expected_orbit}/state/worktrees/run-0")
                    self.assertEqual(
                        json.loads(row[5]),
                        {
                            "orbit_dir": str(expected_orbit),
                            "home": str(expected_home),
                            "unrelated": f"/backup{source_home}/x",
                            "sibling_in_home": f"{expected_home}/ws/orbit-alt",
                        },
                    )
                    self.assertEqual(
                        row[6],
                        f"{expected_repo}\n{expected_global}\nordinary text",
                    )
                    self.assertEqual(row[7], "ordinary text without orbit paths")
                    self.assertEqual(row[8], f"/archive{source_home}/data")

                    kv = dict(
                        conn.execute(
                            "SELECT key, val FROM kv_without_rowid WHERE workspace_id = 'ws_fixture'"
                        ).fetchall()
                    )
                    self.assertEqual(kv["orbit_dir"], str(expected_orbit))
                    self.assertEqual(kv["unrelated"], f"{source_home}_other/file")

                    trigger_row = conn.execute(
                        "SELECT name FROM sqlite_master WHERE type='trigger' AND name='job_runs_no_delete'"
                    ).fetchone()
                    self.assertIsNotNone(trigger_row)
                finally:
                    conn.close()

                # 4. Task index (`tasks/index.sqlite`) resolves to the restored task.json.
                conn = sqlite3.connect(expected_global / "tasks" / "index.sqlite")
                try:
                    bundle_path = conn.execute(
                        "SELECT bundle_path FROM task_index WHERE id = 'ORB-1'"
                    ).fetchone()[0]
                    self.assertEqual(bundle_path, str(restored_task_json))
                    self.assertTrue(Path(bundle_path).is_file())
                finally:
                    conn.close()

                # 5. Workspace database (`checkout/.orbit/workspace.db`) resolves to restored files.
                conn = sqlite3.connect(expected_orbit / "workspace.db")
                try:
                    local_kv = dict(conn.execute("SELECT k, v FROM local_state").fetchall())
                    self.assertEqual(local_kv["orbit_dir"], str(expected_orbit))
                    self.assertEqual(local_kv["repo_root"], str(expected_repo))
                    self.assertEqual(local_kv["ordinary"], "ordinary text")
                    self.assertEqual(local_kv["unrelated"], f"/mirror{paths['orbit_dir']}")
                finally:
                    conn.close()

                # 6. Clean removes only the restored root and leaves source intact.
                clean_res = self.run_fixture(env, "clean", "--into", str(restore_into))
                self.assertEqual(clean_res.returncode, 0, clean_res.stderr)
                self.assertFalse(restore_into.exists())
                self.assertTrue((paths["global_root"] / "workspaces.json").is_file())
                self.assertTrue((paths["orbit_dir"] / "config.yaml").is_file())

    def test_snapshot_refuses_source_without_workspace_identity(self):
        # Both checkouts keep their ordinary files but name no workspace identity:
        # one has no config.yaml, the other a blank `workspace_id:` line whose
        # value would otherwise be read from the next key.
        cases = {
            "no-config": None,
            "blank-workspace-id": "workspace_id:\nrepo_root: {repo}\n",
        }
        for label, config_text in cases.items():
            with self.subTest(source=label):
                source_home = self.base / label / "source"
                source_repo = source_home / "ws" / "orbit"
                paths = _build_disposable_source(source_home, source_repo)
                config_path = paths["orbit_dir"] / "config.yaml"
                if config_text is None:
                    config_path.unlink()
                else:
                    config_path.write_text(config_text.format(repo=source_repo), encoding="utf-8")

                snapshot_dir = self.base / label / "snapshot"
                snap_res = self.run_fixture(
                    _child_env(source_home),
                    "snapshot",
                    "--workspace",
                    "ws_fixture",
                    "--global-root",
                    str(paths["global_root"]),
                    "--out",
                    str(snapshot_dir),
                )
                self.assertEqual(snap_res.returncode, 2, snap_res.stdout + snap_res.stderr)
                self.assertFalse(snapshot_dir.exists())

    def test_restore_refuses_snapshot_recording_no_workspace_identity(self):
        source_home = self.base / "source"
        source_repo = source_home / "ws" / "orbit"
        paths = _build_disposable_source(source_home, source_repo)
        env = _child_env(source_home)

        snapshot_dir = self.base / "snapshot"
        snap_res = self.run_fixture(
            env,
            "snapshot",
            "--workspace",
            "ws_fixture",
            "--global-root",
            str(paths["global_root"]),
            "--out",
            str(snapshot_dir),
        )
        self.assertEqual(snap_res.returncode, 0, snap_res.stderr)

        # Rewrite the snapshot to the shape an identity-less source recorded: an empty
        # storage id and no checkout config.yaml. The file and its manifest entry are
        # removed together, so digest verification still passes.
        manifest_path = snapshot_dir / "snapshot-manifest.json"
        manifest = json.loads(manifest_path.read_text())
        (snapshot_dir / "checkout" / ".orbit" / "config.yaml").unlink()
        manifest["files"] = [
            entry for entry in manifest["files"] if entry["path"] != "checkout/.orbit/config.yaml"
        ]
        manifest["source"]["storage_id"] = ""
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")

        restore_into = self.base / "restored"
        res = self.run_fixture(
            env,
            "restore",
            "--snapshot",
            str(snapshot_dir),
            "--into",
            str(restore_into),
        )
        self.assertEqual(res.returncode, 2, res.stdout + res.stderr)
        self.assertFalse(restore_into.exists())

    def test_snapshot_excludes_credential_and_nonregular_workspace_databases(self):
        source_home = self.base / "source"
        source_repo = source_home / "ws" / "orbit"
        paths = _build_disposable_source(source_home, source_repo)
        orbit_dir = paths["orbit_dir"]

        def create_sqlite_db(path: Path, table: str, key: str, value: str, *, wal: bool = False) -> None:
            path.parent.mkdir(parents=True, exist_ok=True)
            conn = sqlite3.connect(path)
            try:
                if wal:
                    conn.execute("PRAGMA journal_mode=WAL")
                conn.execute(f"CREATE TABLE {table} (k TEXT PRIMARY KEY, v TEXT)")
                conn.execute(f"INSERT INTO {table} VALUES (?, ?)", (key, value))
                conn.commit()
            finally:
                conn.close()

        # Eligible workspace database in WAL mode.
        create_sqlite_db(orbit_dir / "nested" / "cache.sqlite", "cache", "status", "ok", wal=True)

        # Credential-directory and credential-named databases (valid SQLite and invalid bytes
        # to verify exclusion happens before opening the source).
        create_sqlite_db(orbit_dir / "secrets" / "vault.db", "vault", "k", "v")
        create_sqlite_db(orbit_dir / "ssh" / "keys.db", "keys", "k", "v")
        create_sqlite_db(orbit_dir / "auth_token.db", "tokens", "k", "v")
        (orbit_dir / "plugin-secrets").mkdir(parents=True, exist_ok=True)
        (orbit_dir / "plugin-secrets" / "corrupt.sqlite").write_bytes(b"not-a-sqlite-db")
        (orbit_dir / "credentials.sqlite").write_bytes(b"not-a-sqlite-db")

        # Non-regular database paths: symlink to an external DB, broken symlink, and FIFO.
        outside_db = self.base / "outside.db"
        create_sqlite_db(outside_db, "external", "k", "v")
        (orbit_dir / "linked.db").symlink_to(outside_db)
        (orbit_dir / "broken.sqlite").symlink_to(self.base / "missing.sqlite")
        expected_nonregular = 2
        if hasattr(os, "mkfifo"):
            os.mkfifo(orbit_dir / "pipe.db")
            expected_nonregular += 1

        snapshot_dir = self.base / "snapshot"
        env = _child_env(source_home)
        snap_res = self.run_fixture(
            env,
            "snapshot",
            "--workspace",
            "ws_fixture",
            "--global-root",
            str(paths["global_root"]),
            "--out",
            str(snapshot_dir),
        )
        self.assertEqual(snap_res.returncode, 0, snap_res.stdout + snap_res.stderr)

        manifest = json.loads((snapshot_dir / "snapshot-manifest.json").read_text())
        self.assertEqual(manifest["excluded"]["credential_like_files"], 5)
        self.assertEqual(manifest["excluded"]["non_regular_files"], expected_nonregular)

        excluded_rels = [
            "checkout/.orbit/secrets/vault.db",
            "checkout/.orbit/ssh/keys.db",
            "checkout/.orbit/auth_token.db",
            "checkout/.orbit/plugin-secrets/corrupt.sqlite",
            "checkout/.orbit/credentials.sqlite",
            "checkout/.orbit/linked.db",
            "checkout/.orbit/broken.sqlite",
            "checkout/.orbit/pipe.db",
        ]
        manifest_file_paths = {entry["path"] for entry in manifest["files"]}
        for rel in excluded_rels:
            self.assertNotIn(rel, manifest["databases"])
            self.assertNotIn(rel, manifest_file_paths)
            self.assertFalse((snapshot_dir / rel).exists(), f"{rel} leaked into snapshot")

        for eligible_rel in ("checkout/.orbit/workspace.db", "checkout/.orbit/nested/cache.sqlite"):
            self.assertIn(eligible_rel, manifest["databases"])
            self.assertEqual(manifest["databases"][eligible_rel]["method"], "online-backup-api")
            self.assertFalse(manifest["databases"][eligible_rel]["filtered"])
            self.assertIn(eligible_rel, manifest_file_paths)

        restore_into = self.base / "restored"
        restore_res = self.run_fixture(
            env,
            "restore",
            "--snapshot",
            str(snapshot_dir),
            "--into",
            str(restore_into),
        )
        self.assertEqual(restore_res.returncode, 0, restore_res.stdout + restore_res.stderr)
        conn = sqlite3.connect(restore_into / "checkout" / ".orbit" / "nested" / "cache.sqlite")
        try:
            row = conn.execute("SELECT v FROM cache WHERE k = 'status'").fetchone()
            self.assertEqual(row, ("ok",))
        finally:
            conn.close()


if __name__ == "__main__":
    unittest.main()
