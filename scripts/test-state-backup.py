#!/usr/bin/env python3
"""Reproduce why a raw copy of a live Orbit SQLite database is not a backup.

Manual reproduction, not a CI guard: run `python3 scripts/test-state-backup.py`
by hand. It builds a disposable WAL database under `.orbit/tmp/` with one
committed row that exists only in the write-ahead log, then takes three
snapshots of that same committed state:

* a raw file copy, in the order the old guide's fallback implied: copy the
  database file, checkpoint, then copy `-wal` and `-shm` while the connection
  stays open. The committed row is lost, yet `PRAGMA quick_check` reports `ok`.
* `VACUUM INTO`, and the online backup API that `sqlite3 .backup` uses. Both
  keep every committed row.

It exits 0 only when the raw copy loses exactly the late row and both live
snapshots keep it. No sleeps, competing processes or synthetic load are used.
"""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import sqlite3
import sys
import tempfile

REPO = Path(__file__).resolve().parent.parent
SCRATCH = Path(os.environ.get("ORBIT_SCRATCH_DIR", REPO / ".orbit" / "tmp"))
BASELINE = "baseline"
LATE = "committed-before-backup"


def read_back(path: Path) -> tuple[str, list[str]]:
    """Open a snapshot the way a restore would and return its integrity result and events."""
    conn = sqlite3.connect(path)
    try:
        check = conn.execute("PRAGMA quick_check").fetchone()[0]
        events = [row[0] for row in conn.execute("SELECT event FROM audit_events ORDER BY id")]
    finally:
        conn.close()
    return check, events


def main() -> int:
    SCRATCH.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="state-backup-repro-", dir=SCRATCH) as work_dir:
        work = Path(work_dir)
        live_dir = work / "live"
        live_dir.mkdir()
        live = live_dir / "orbit.db"

        # isolation_level=None makes every statement its own committed transaction.
        source = sqlite3.connect(live, isolation_level=None)
        try:
            source.execute("PRAGMA journal_mode=WAL")
            source.execute("CREATE TABLE audit_events (id INTEGER PRIMARY KEY, event TEXT NOT NULL)")
            source.execute("INSERT INTO audit_events (event) VALUES (?)", (BASELINE,))
            source.execute("PRAGMA wal_checkpoint(TRUNCATE)")
            source.execute("INSERT INTO audit_events (event) VALUES (?)", (LATE,))

            raw_dir = work / "raw-copy"
            raw_dir.mkdir()
            shutil.copyfile(live, raw_dir / "orbit.db")
            source.execute("PRAGMA wal_checkpoint(TRUNCATE)")
            shutil.copyfile(live_dir / "orbit.db-wal", raw_dir / "orbit.db-wal")
            shutil.copyfile(live_dir / "orbit.db-shm", raw_dir / "orbit.db-shm")

            api_snapshot = work / "online-backup.db"
            target = sqlite3.connect(api_snapshot)
            try:
                source.backup(target)
            finally:
                target.close()

            vacuum_snapshot = work / "vacuum-into.db"
            source.execute("VACUUM INTO ?", (str(vacuum_snapshot),))

            source_events = [row[0] for row in source.execute("SELECT event FROM audit_events ORDER BY id")]
        finally:
            source.close()

        raw_check, raw_events = read_back(raw_dir / "orbit.db")
        api_check, api_events = read_back(api_snapshot)
        vacuum_check, vacuum_events = read_back(vacuum_snapshot)

    print(f"live source events:            {source_events}")
    print(f"raw db+wal+shm copy:           quick_check={raw_check} events={raw_events}")
    print(f"sqlite online backup (.backup): quick_check={api_check} events={api_events}")
    print(f"VACUUM INTO:                   quick_check={vacuum_check} events={vacuum_events}")

    reproduced = (
        raw_check == "ok"
        and raw_events == [BASELINE]
        and api_check == "ok"
        and api_events == source_events
        and vacuum_check == "ok"
        and vacuum_events == source_events
    )
    if reproduced:
        print("REPRODUCED: the raw copy lost the committed row with quick_check=ok; both live snapshots kept it.")
        return 0
    print("NOT REPRODUCED: the observed results differ from the documented sequence.", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
