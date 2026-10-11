#!/usr/bin/env python3
"""Every durability sync goes through orbit_common::fs::io.

`sync_file`, `sync_file_data` and `sqlite_synchronous` are the only places a
test run can relax a sync (docs/DEVELOPMENT.md#fixtures-on-a-loaded-host). A
direct `File::sync_all`/`sync_data` or a SQLite `synchronous` pragma with a
literal level would pay a full flush in every test run again; on macOS that is
an F_FULLFSYNC of the whole drive. clippy's `disallowed-methods` rejects the
direct file syncs in production code only, so this guard also covers tests
and the pragma, whose level clippy cannot see.
"""

from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parent.parent
HELPER = Path("crates/orbit-common/src/fs/io/atomic.rs")
DIRECT_SYNC = re.compile(r"\.sync_(?:all|data)\(\)")
PRAGMA = re.compile(r'pragma_update(?:_and_check)?\(\s*None\s*,\s*"synchronous"\s*,\s*(?P<value>[^,)]*)')
SQL_PRAGMA = re.compile(r"PRAGMA\s+synchronous\s*=", re.IGNORECASE)
LEVEL = re.compile(r"^(?:(?:crate|orbit_common)::fs::io::)?sqlite_synchronous\(")


def is_test_path(path: Path) -> bool:
    return "tests" in path.parts


def line_of(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def main() -> int:
    failures = []
    for path in sorted((ROOT / "crates").glob("*/**/*.rs")):
        relative = path.relative_to(ROOT)
        if "target" in relative.parts:
            continue
        text = path.read_text(encoding="utf-8")
        if relative != HELPER:
            for match in DIRECT_SYNC.finditer(text):
                failures.append(f"{relative}:{line_of(text, match.start())}: call "
                                "orbit_common::fs::io::sync_file or sync_file_data instead")
        if is_test_path(relative):
            continue
        for match in PRAGMA.finditer(text):
            if not LEVEL.match(match.group("value").strip()):
                failures.append(f"{relative}:{line_of(text, match.start())}: take the synchronous "
                                "level from orbit_common::fs::io::sqlite_synchronous")
        for match in SQL_PRAGMA.finditer(text):
            failures.append(f"{relative}:{line_of(text, match.start())}: set synchronous with "
                            "pragma_update and orbit_common::fs::io::sqlite_synchronous")
    for failure in failures:
        print(f"check-durability-syncs: {failure}", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
