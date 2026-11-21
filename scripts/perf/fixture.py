#!/usr/bin/env python3
"""Freeze an Orbit workspace store into a perf fixture, and restore it.

    fixture.py snapshot --workspace <ws> --out <snapshot-dir> [--git-metadata <checkout>]
    fixture.py restore  --snapshot <snapshot-dir> --into <dir>
    fixture.py clean    --into <dir>

`snapshot` reads the source only: every SQLite source is opened through a
`mode=ro` URI connection (`open_source_read_only`), files are copied by
reading them, and nothing writes to, reconciles or signals the live store or a
service. `restore` verifies every digest in `snapshot-manifest.json` before it
lays out a disposable `HOME` and workspace root. See
docs/runbooks/perf-timing-protocol.md.
"""

from __future__ import annotations

import argparse
import fnmatch
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

sys.path.insert(0, str(Path(__file__).resolve().parent))
import perf_common as common  # noqa: E402

SCHEMA_VERSION = 1
MANIFEST = "snapshot-manifest.json"
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


# ── workspace resolution ─────────────────────────────────────────────────────


def read_storage_id(orbit_dir: Path) -> str:
    try:
        text = (orbit_dir / "config.yaml").read_text()
    except OSError:
        return ""
    # Same-line value only: `\s` would let a blank `workspace_id:` line capture the next key.
    match = re.search(r"^workspace_id:[ \t]*(\S+)", text, re.MULTILINE)
    return match.group(1) if match else ""


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
            key = stripped.split("=", 1)[0].strip().lower() if "=" in stripped else ""
            if key == "worktree":
                # Drop core.worktree so the restored `.git` never binds to a source path.
                continue
            if non_bare and key == "bare":
                line = re.sub(
                    r"^(\s*bare\s*=\s*)(?:true|yes|on|1)\b",
                    r"\1false",
                    line,
                    flags=re.IGNORECASE,
                )
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
        snap.copy_tree(
            selected_git_dir,
            dest_root,
            excludes=GIT_LINKED_WORKTREE_EXCLUDES,
            transform=make_transform(selected_git_dir, skip_shared_index=False),
        )
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


def rewrite_database(path: Path, pairs: list[tuple[str, str]]) -> None:
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
        tables = [r[0] for r in conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND sql NOT LIKE 'CREATE VIRTUAL%'"
            " AND name NOT LIKE 'sqlite_%'")]
        for table in tables:
            tbl = quote_ident(table)
            for column in [r[1] for r in conn.execute(f"PRAGMA table_info({tbl})")]:
                col = quote_ident(column)
                where_instr = " OR ".join(f"instr({col}, ?) > 0" for _ in olds)
                conn.execute(
                    f"UPDATE {tbl} SET {col} = perf_rewrite_path({col}) "
                    f"WHERE typeof({col}) = 'text' AND ({where_instr}) "
                    f"AND perf_rewrite_path({col}) != {col}",
                    tuple(olds),
                )
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
    clean = sub.add_parser("clean", help="remove a restored root")
    clean.add_argument("--into", required=True)
    args = parser.parse_args(argv)
    os.umask(0o077)
    try:
        return {"snapshot": cmd_snapshot, "restore": cmd_restore,
                "clean": cmd_clean}[args.command](args)
    except common.PerfRefusal as refusal:
        print(f"fixture.py {args.command}: {refusal}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
