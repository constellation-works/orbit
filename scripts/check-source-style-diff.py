#!/usr/bin/env python3
"""Fail on STD-12 source-style findings that a change adds.

scripts/check-source-style.py is the shared checker, carried unchanged from
constellation-works/rust-cli-template at 7539c089. It reads every tracked Rust
file and has no diff mode, and one run over this repository takes minutes. So
this wrapper runs it on two snapshots: this checkout and the merge base of HEAD
and the base revision. A snapshot holds every Rust path and Cargo manifest of its
checkout, so the checker sees the same directories, names and crate roots. The
files that differ between the two carry their text, and so does every untouched
file that says @generated: the checker exempts those by reading their leading
comment, and an empty placeholder would make a generated module look like
handwritten source. The checker's rules are per file and per path, so any other
untouched file reports the same findings on both sides and cancels out.

The findings are compared by identity: the file (followed across a rename), the
rule, the message and the text of the line it names. The first findings the merge
base holds for an identity are recorded debt, and each further finding in this
checkout is new. A file whose leading comment says @generated is exempt on both
sides, which the checker enforces.

The base is CI_TEST_BASE when set, else origin/agent-main, then agent-main. As in
check-rust-source-order.py, the compared commit is `git merge-base HEAD <base>`.
A base that cannot be resolved fails the gate rather than passing it.

The checker and the Rust formatter profile are pinned below by sha256. An edit to
either fails this gate; re-pin them only when re-syncing from the template.
"""

import contextlib
import hashlib
import os
from collections import Counter
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


ROOT = Path(__file__).resolve().parent.parent
CHECKER = ROOT / "scripts/check-source-style.py"
GENERATED = "@generated"
MODES = ("names", "style")
BASE_BRANCHES = ("refs/remotes/origin/agent-main", "refs/heads/agent-main")
# constellation-works/rust-cli-template at 7539c089.
PINNED = {
    "scripts/check-source-style.py": "edf6a6e8c838f01cfd3be43420b94766c08e01fafa63330df099794c8b1a3e40",
    "rustfmt.toml": "a72f230180ccfaf10db0b4e339b9d9066d2a678174b3da29cd9c905ee92f2cc3",
    "desktop/rustfmt.toml": "a72f230180ccfaf10db0b4e339b9d9066d2a678174b3da29cd9c905ee92f2cc3",
}
BREACH = re.compile(r"^(?P<path>.+?)(?::(?P<row>\d+))?: (?P<rule>R\d+): (?P<message>.*)$")
SUMMARY = re.compile(r"breach\(es\) of STD-12")


class GateError(Exception):
    """A condition that stops the gate before it can compare findings."""


def git(*args, cwd=ROOT, data=None, text=True):
    try:
        completed = subprocess.run(
            ["git", *args], cwd=cwd, input=data, check=True, capture_output=True, text=text
        )
    except subprocess.CalledProcessError as error:
        detail = error.stderr.decode(errors="replace") if isinstance(error.stderr, bytes) else error.stderr
        raise GateError(f"git {args[0]} failed: {(detail or '').strip()}") from error
    except OSError as error:
        raise GateError(f"git {args[0]} could not run: {error}") from error
    return completed.stdout


def has_ref(ref):
    return subprocess.run(
        ["git", "rev-parse", "--verify", "--quiet", ref], cwd=ROOT, capture_output=True
    ).returncode == 0


def is_rust(path):
    return path.endswith(".rs") and not path.startswith("target/")


def is_manifest(path):
    return path == "Cargo.toml" or path.endswith("/Cargo.toml")


def verify_pins():
    for relative, digest in PINNED.items():
        path = ROOT / relative
        if not path.is_file():
            raise GateError(f"{relative} is missing; it is pinned to the rust-cli-template copy")
        actual = hashlib.sha256(path.read_bytes()).hexdigest()
        if actual != digest:
            raise GateError(
                f"{relative} differs from the pinned rust-cli-template copy "
                f"(sha256 {actual}, pinned {digest})"
            )


def resolve_base():
    """The merge base of HEAD and the base revision."""
    revision = os.environ.get("CI_TEST_BASE", "")
    if not revision:
        revision = next((ref for ref in BASE_BRANCHES if has_ref(ref)), "")
    if not revision:
        raise GateError("no base revision: set CI_TEST_BASE or fetch origin/agent-main")
    fork = git("merge-base", "HEAD", revision).strip()
    if not fork:
        raise GateError(f"no merge base between HEAD and {revision}")
    return fork


def renamed_from(fork):
    """Maps each path renamed since the merge base to its path at the merge base."""
    fields = git("diff", "--name-status", "-M", "-z", fork, "--").split("\0")
    mapping = {}
    index = 0
    while index < len(fields):
        if fields[index][:1] in ("R", "C") and index + 2 < len(fields):
            mapping[os.path.normpath(fields[index + 2])] = os.path.normpath(fields[index + 1])
            index += 3
        else:
            index += 2
    return mapping


def generated_at(fork):
    """The merge base's Rust paths whose text contains @generated.

    The checker exempts a file only when its leading comment block says @generated,
    so a file without that text is never exempt and needs no content in a snapshot.
    """
    completed = subprocess.run(
        ["git", "grep", "-z", "-l", "-F", "-e", GENERATED, fork, "--", "*.rs"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if completed.returncode == 1:
        return set()
    if completed.returncode != 0:
        raise GateError(f"git grep failed: {completed.stderr.strip()}")
    # Each entry is `<fork>:<path>`; the fork is a commit id, which holds no colon.
    return {entry.partition(":")[2] for entry in completed.stdout.split("\0") if entry}


def sources_at_head():
    """This checkout's Rust paths with their blob ids, and its Cargo manifests."""
    listing = git("ls-files", "-z", "--cached", "--others", "--exclude-standard").split("\0")
    present = sorted({path for path in listing if path and (ROOT / path).is_file()})
    rust = [path for path in present if is_rust(path)]
    manifests = [path for path in present if is_manifest(path)]
    hashes = git("hash-object", "--stdin-paths", data="".join(f"{path}\n" for path in rust)).split()
    return dict(zip(rust, hashes)), manifests


def sources_at(fork):
    """The merge base's Rust paths with their blob ids, and its Cargo manifests."""
    rust, manifests = {}, []
    for entry in git("ls-tree", "-r", "-z", fork).split("\0"):
        if not entry:
            continue
        meta, _, path = entry.partition("\t")
        if is_rust(path):
            rust[path] = meta.split()[2]
        elif is_manifest(path):
            manifests.append(path)
    return rust, manifests


@contextlib.contextmanager
def snapshot(rust, manifests, text):
    """A scratch repository holding every path; only the paths in `text` carry content."""
    scratch = ROOT / ".orbit/tmp"
    scratch.mkdir(parents=True, exist_ok=True)
    tree = Path(tempfile.mkdtemp(prefix="source-style.", dir=scratch))
    try:
        git("init", "-q", cwd=tree)
        for path in rust:
            (tree / path).parent.mkdir(parents=True, exist_ok=True)
            (tree / path).write_bytes(text.get(path, b""))
        for path in manifests:
            (tree / path).parent.mkdir(parents=True, exist_ok=True)
            (tree / path).write_bytes(b"")
        yield tree
    finally:
        shutil.rmtree(tree, ignore_errors=True)


def findings_in(tree):
    """(path, row, rule, message) for every finding the checker reports in one snapshot."""
    found = []
    for mode in MODES:
        completed = subprocess.run(
            [sys.executable, "-I", str(CHECKER), mode],
            cwd=tree,
            capture_output=True,
            text=True,
        )
        report = completed.stderr.splitlines()
        if completed.returncode == 0 and "ok (" in completed.stdout:
            continue
        if completed.returncode != 1 or not any(SUMMARY.search(line) for line in report):
            raise GateError(f"the checker failed in {mode} mode:\n{completed.stderr.strip()}")
        for line in report:
            match = BREACH.match(line)
            if match:
                found.append((match["path"], int(match["row"] or 0), match["rule"], match["message"]))
    return found


def identify(tree, findings, mapping=None):
    """(identity, path, row) for each finding, the identity using the merge-base path."""
    mapping = mapping or {}
    cache = {}
    identified = []
    for path, row, rule, message in findings:
        text = ""
        if row:
            if path not in cache:
                try:
                    with open(tree / path, encoding="utf-8") as handle:
                        cache[path] = handle.read().split("\n")
                except OSError:
                    cache[path] = []
            if 0 < row <= len(cache[path]):
                text = cache[path][row - 1].strip()
        # The names mode reports `./src/x.rs` for a root crate; git reports `src/x.rs`.
        identity_path = os.path.normpath(path)
        base_path = mapping.get(identity_path, identity_path)
        identified.append(((base_path, rule, message, text), path, row))
    return identified


def new_findings(base, head):
    """The head findings beyond the base's count for their identity."""
    recorded = Counter(identity for identity, _, _ in base)
    seen = Counter()
    fresh = []
    for identity, path, row in sorted(head, key=lambda item: (item[1], item[2])):
        seen[identity] += 1
        if seen[identity] > recorded[identity]:
            fresh.append((path, row, identity[1], identity[2]))
    return fresh


def main():
    try:
        verify_pins()
        fork = resolve_base()
        mapping = renamed_from(fork)
        head_blobs, head_manifests = sources_at_head()
        base_blobs, base_manifests = sources_at(fork)
        changed_head = sorted(path for path in head_blobs if base_blobs.get(path) != head_blobs[path])
        changed_base = sorted(path for path in base_blobs if head_blobs.get(path) != base_blobs[path])
        # Unchanged generated files: the same blob on both sides, so both snapshots carry it.
        generated = {
            path: git("cat-file", "blob", base_blobs[path], text=False)
            for path in generated_at(fork)
            if path in base_blobs and head_blobs.get(path) == base_blobs[path]
        }
        with snapshot(
            sorted(head_blobs),
            head_manifests,
            {**generated, **{path: (ROOT / path).read_bytes() for path in changed_head}},
        ) as tree:
            head = identify(tree, findings_in(tree), mapping)
        changed_base_text = {
            path: git("cat-file", "blob", base_blobs[path], text=False) for path in changed_base
        }
        with snapshot(sorted(base_blobs), base_manifests, {**generated, **changed_base_text}) as tree:
            base = identify(tree, findings_in(tree))
    except (GateError, OSError) as error:
        print(f"source-style diff: {error}", file=sys.stderr)
        return 1

    fresh = new_findings(base, head)
    short = fork[:12]
    if fresh:
        for path, row, rule, message in fresh:
            where = f"{path}:{row}" if row else path
            print(f"{where}: {rule}: {message}", file=sys.stderr)
        print(
            f"source-style diff: {len(fresh)} new STD-12 finding(s) against {short}; "
            "recorded debt is in docs/design/architecture-restructure/4_decisions.md",
            file=sys.stderr,
        )
        return 1
    print(f"source-style diff: no new findings against {short} in {len(changed_head)} changed Rust file(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
