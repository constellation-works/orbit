#!/usr/bin/env python3
"""Fail when orbit-web async handlers call the store or filesystem inline.

SQLite scans and flock waits on a tokio worker park the whole dashboard
(F2026-07-119). Handlers must run that work through `blocking()`,
`spawn_blocking`, or `run_blocking_check`. This scan is the regression
gate for that rule.

Runs from `scripts/ci-guardrails.sh` (and `make ci-fast`).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


# Every module directly under crates/orbit-web/src/api is scanned, so a new
# handler file is covered by default. Sibling `tests/` is a subdirectory and is
# not matched; the Ws extractor in state.rs is a separate task.

SKIP_ASYNC_FNS = {"blocking"}

OFFLOAD_CALLEES = ("blocking", "spawn_blocking", "run_blocking_check")

# Store / filesystem work that must not remain in an async handler body after
# offload closures are masked. Keep this list specific: in-memory projections
# (crew registry) and env reads (authorized_caller) are not the outage.
FORBIDDEN = (
    (re.compile(r"\.pin\s*\("), "state.pin()"),
    (re.compile(r"\.runtime_for\s*\("), "runtime_for()"),
    (re.compile(r"\.pipeline_reliability\s*\("), "pipeline_reliability()"),
    (re.compile(r"list_job_catalog_with_last_run\s*\("), "list_job_catalog_with_last_run()"),
    (re.compile(r"\bjob_runs_page\s*\("), "job_runs_page()"),
    (re.compile(r"\.workspace_id\s*\("), "workspace_id()"),
    (re.compile(r"\.explain_operation\s*\("), "explain_operation()"),
    (re.compile(r"\.workspace_auto_readiness\s*\("), "workspace_auto_readiness()"),
    (re.compile(r"\broutine_statuses\s*\("), "routine_statuses()"),
    (re.compile(r"\.clock_status\s*\("), "clock_status()"),
    (re.compile(r"\.automation_store\s*\("), "automation_store()"),
    (re.compile(r"\bresolve_workspace\s*\("), "resolve_workspace()"),
    (re.compile(r"\baggregate_task_scope\s*\("), "aggregate_task_scope()"),
    (re.compile(r"\bset_routine_enabled\s*\("), "set_routine_enabled()"),
    (re.compile(r"\bset_clock_enabled\s*\("), "set_clock_enabled()"),
    (re.compile(r"\bset_clock_cadence\s*\("), "set_clock_cadence()"),
)

ASYNC_FN = re.compile(r"\basync\s+fn\s+([A-Za-z_][A-Za-z0-9_]*)")


def lifetime_or_label_end(source: str, quote_idx: int) -> int:
    """Return the end of a lifetime or loop label starting at `quote_idx`, else 0.

    `'static` and `'outer:` are an apostrophe and an identifier with no closing
    apostrophe; `'a'` and `'\\n'` are character literals and must stay masked.
    """
    n = len(source)
    j = quote_idx + 1
    if j >= n or not (source[j].isalpha() or source[j] == "_"):
        return 0
    while j < n and _ident_char(source[j]):
        j += 1
    if j < n and source[j] == "'":
        return 0
    return j


def mask_strings_and_comments(source: str) -> str:
    """Replace comments and string/char/byte literals with spaces."""
    out = []
    i = 0
    n = len(source)
    while i < n:
        ch = source[i]
        nxt = source[i + 1] if i + 1 < n else ""
        if ch == "/" and nxt == "/":
            start = i
            i += 2
            while i < n and source[i] != "\n":
                i += 1
            out.append(" " * (i - start))
            continue
        if ch == "/" and nxt == "*":
            start = i
            i += 2
            while i + 1 < n and not (source[i] == "*" and source[i + 1] == "/"):
                i += 1
            i = min(n, i + 2)
            chunk = source[start:i]
            out.append("".join("\n" if c == "\n" else " " for c in chunk))
            continue
        if ch == "'":
            lifetime_end = lifetime_or_label_end(source, i)
            if lifetime_end:
                out.append(source[i:lifetime_end])
                i = lifetime_end
                continue
        if ch in "\"'":
            quote = ch
            start = i
            i += 1
            while i < n:
                if source[i] == "\\":
                    i = min(n, i + 2)
                    continue
                if source[i] == quote:
                    i += 1
                    break
                i += 1
            chunk = source[start:i]
            out.append("".join("\n" if c == "\n" else " " for c in chunk))
            continue
        if ch == "b" and nxt in "\"'":
            quote = nxt
            start = i
            i += 2
            while i < n:
                if source[i] == "\\":
                    i = min(n, i + 2)
                    continue
                if source[i] == quote:
                    i += 1
                    break
                i += 1
            chunk = source[start:i]
            out.append("".join("\n" if c == "\n" else " " for c in chunk))
            continue
        if ch == "r" or (ch == "b" and nxt == "r"):
            raw_start = i
            j = i + (2 if ch == "b" else 1)
            hashes = 0
            while j < n and source[j] == "#":
                hashes += 1
                j += 1
            if j < n and source[j] == '"':
                closer = '"' + ("#" * hashes)
                j += 1
                k = source.find(closer, j)
                i = n if k < 0 else k + len(closer)
                chunk = source[raw_start:i]
                out.append("".join("\n" if c == "\n" else " " for c in chunk))
                continue
        out.append(ch)
        i += 1
    return "".join(out)


def matching_paren(source: str, open_idx: int) -> int:
    depth = 0
    i = open_idx
    n = len(source)
    while i < n:
        ch = source[i]
        if ch == "(":
            depth += 1
        elif ch == ")":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return n - 1


def mask_offload_calls(source: str) -> str:
    """Blank out `blocking(` / `spawn_blocking(` / `run_blocking_check(` calls."""
    chars = list(source)
    i = 0
    n = len(source)
    while i < n:
        matched = None
        for name in OFFLOAD_CALLEES:
            if source.startswith(name, i) and (i == 0 or not _ident_char(source[i - 1])):
                after = i + len(name)
                while after < n and source[after].isspace():
                    after += 1
                if after < n and source[after] == "(":
                    matched = after
                    break
        if matched is None:
            i += 1
            continue
        end = matching_paren(source, matched)
        for j in range(i, end + 1):
            if chars[j] != "\n":
                chars[j] = " "
        i = end + 1
    return "".join(chars)


def _ident_char(ch: str) -> bool:
    return ch.isalnum() or ch == "_"


def matching_brace(source: str, open_idx: int) -> int:
    depth = 0
    i = open_idx
    n = len(source)
    while i < n:
        ch = source[i]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return n - 1


def async_fn_spans(source: str) -> list[tuple[str, int, int]]:
    spans = []
    for match in ASYNC_FN.finditer(source):
        name = match.group(1)
        if name in SKIP_ASYNC_FNS:
            continue
        brace = source.find("{", match.end())
        if brace < 0:
            continue
        end = matching_brace(source, brace)
        spans.append((name, brace, end))
    return spans


def line_of(source: str, index: int) -> int:
    return source.count("\n", 0, index) + 1


def scan_file(path: Path, source: str) -> list[str]:
    masked = mask_offload_calls(mask_strings_and_comments(source))
    hits = []
    for name, start, end in async_fn_spans(masked):
        body = masked[start : end + 1]
        for pattern, label in FORBIDDEN:
            for match in pattern.finditer(body):
                abs_index = start + match.start()
                hits.append(
                    f"{path}:{line_of(source, abs_index)}: async fn {name} calls {label} "
                    "inline; wrap the store/filesystem work in blocking()/spawn_blocking"
                )
    return hits


def handler_paths(repo_root: Path) -> list[Path]:
    paths = []
    api = repo_root / "crates/orbit-web/src/api"
    health = repo_root / "crates/orbit-web/src/health.rs"
    if api.is_dir():
        paths.extend(sorted(api.glob("*.rs")))
    if health.is_file():
        paths.append(health)
    return paths


def scan_tree(repo_root: Path) -> list[str]:
    hits = []
    for path in handler_paths(repo_root):
        source = path.read_text(encoding="utf-8")
        relative = path.relative_to(repo_root)
        hits.extend(scan_file(relative, source))
    return hits


def verify_fixture_reporting() -> None:
    fixture = """
pub(super) async fn bad_handler(State(state): State<DashboardState>) -> Response {
    let pinned = state.pin();
    Json(pinned).into_response()
}

pub(super) async fn good_handler(State(state): State<DashboardState>) -> Response {
    match blocking("ok", move || {
        let pinned = state.pin();
        Ok(pinned)
    }).await {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

impl Kind {
    fn label(self) -> &'static str {
        "kind"
    }
}

pub(super) async fn after_lifetime_handler(State(state): State<DashboardState>) -> Response {
    'outer: for c in ['x', '\\'', '"'] {
        break 'outer;
    }
    let pinned = state.pin();
    Json(pinned).into_response()
}
"""
    hits = scan_file(Path("fixture.rs"), fixture)
    if sum("bad_handler" in hit and "state.pin()" in hit for hit in hits) != 1:
        raise RuntimeError(f"fixture did not report the inline pin: {hits!r}")
    if sum("after_lifetime_handler" in hit and "state.pin()" in hit for hit in hits) != 1:
        raise RuntimeError(f"fixture lost the handler after a lifetime: {hits!r}")
    if len(hits) != 2:
        raise RuntimeError(f"fixture reported unexpected hits: {hits!r}")
    if any("good_handler" in hit for hit in hits):
        raise RuntimeError(f"fixture flagged an offloaded pin: {hits!r}")


def main() -> int:
    repo_root = Path(__file__).resolve().parent.parent
    try:
        verify_fixture_reporting()
        hits = scan_tree(repo_root)
    except (OSError, RuntimeError) as error:
        print(f"check-web-blocking-handlers: {error}", file=sys.stderr)
        return 2
    if hits:
        print(
            "orbit-web async handlers must offload store/filesystem work via blocking():",
            file=sys.stderr,
        )
        for hit in hits:
            print(hit, file=sys.stderr)
        return 1
    print("orbit-web blocking-handler guard passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
