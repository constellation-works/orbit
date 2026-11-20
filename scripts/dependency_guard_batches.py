"""Pack `test-check-dependency-direction.py` batches into few guard runs.

Every guard run costs about the same, a fixed scan of the whole fixture tree
(about 185 child processes), whatever the case wrote into it. A case therefore
hands its batches here instead of running the guard itself, and the suite runs
the guard once per compatible group of batches.

Two batch kinds, with different safety arguments:

* An accepted batch asserts that the guard exits 0 and reports none of its
  files. Merged accepted batches can only add reports, never remove one, so a
  hit anywhere in the shared tree fails the run and names the file.
* A rejected batch asserts that the guard reports each of its rows. The guard
  stops a detector chain at the first detector with a hit, so a row behind
  another batch's row in the same chain is not reported in that run. The suite
  runs waves: it removes the rows a run reported and runs the unreported ones
  again, without the rows that hid them. A row never reported in a tree that
  holds only unreported rows is a failure, and is rerun alone in its own tree
  to name the case.

Merging stays per file, so one batch's files never change what the guard reads
from another's, with two exceptions this module keeps out of a group:

* two files at one path with differing contents (except `mod.rs`-style exact
  rows, which a group concatenates and checks by line);
* a method one batch defines in a shared file and another batch names. The
  guard derives the inherent methods from every defining module, so a shared
  name could make a row look rejected for another case's definition.
"""

import re
from typing import NamedTuple

IDENTIFIER = re.compile(r"[A-Za-z_]\w*")
FUNCTION = re.compile(r"\bfn\s+([A-Za-z_]\w*)")


class AcceptedBatch(NamedTuple):
    """Files that must produce no report. Paths are relative to the fixture root."""

    test: str
    sequence: int
    files: dict


class Row(NamedTuple):
    """One rejected source. `exact` rows keep their path (a group may append to it)."""

    path: str
    source: str
    exact: bool


class RejectedBatch(NamedTuple):
    """Rows the guard must report, plus the exact-path files they need."""

    test: str
    sequence: int
    rows: tuple
    shared: dict


def pack_accepted(batches):
    """Group accepted batches first-fit; two files at one path must match."""
    groups = []
    for batch in sorted(batches, key=lambda batch: batch[:2]):
        for files, members in groups:
            if all(files.get(path, source) == source for path, source in batch.files.items()):
                files.update(batch.files)
                members.append(batch)
                break
        else:
            groups.append((dict(batch.files), [batch]))
    return groups


class RejectedGroup:
    """Rejected batches that share one fixture tree and one guard run."""

    def __init__(self):
        self.shared = {}
        self.texts = {}
        self.claims = []
        self.members = []
        self.defined = set()
        self.identifiers = set()

    def try_add(self, batch):
        defined = {name for source in batch.shared.values() for name in FUNCTION.findall(source)}
        identifiers = {name for source in (*batch.shared.values(), *(row.source for row in batch.rows))
                       for name in IDENTIFIER.findall(source)}
        if defined & self.identifiers or self.defined & identifiers:
            return False
        for path, source in batch.shared.items():
            if path in self.texts or self.shared.get(path, source) != source:
                return False
        for row in batch.rows:
            if row.path in self.shared or (row.path in self.texts and not row.exact):
                return False
        self.shared.update(batch.shared)
        for row in batch.rows:
            assert row.source.endswith("\n"), row
            text = self.texts.get(row.path, "")
            first = text.count("\n") + 1
            self.texts[row.path] = text + row.source
            self.claims.append((batch, row, first, first + row.source.count("\n") - 1))
        self.members.append(batch)
        self.defined |= defined
        self.identifiers |= identifiers
        return True

    def files(self):
        return {**self.shared, **self.texts}


def pack_rejected(batches):
    """Group rejected batches first-fit into trees; order is fixed by case and sequence."""
    groups = []
    for batch in sorted(batches, key=lambda batch: batch[:2]):
        for group in groups:
            if group.try_add(batch):
                break
        else:
            groups.append(RejectedGroup())
            assert groups[-1].try_add(batch)
    return groups


def reported_rows(group, root, stdout):
    """Return the group's claims the guard named, matched by path and line span."""
    hits = {}
    for line in stdout.splitlines():
        match = re.match(r"(.+?):(\d+):", line)
        if match:
            hits.setdefault(match[1], set()).add(int(match[2]))
    return [claim for claim in group.claims
            if any(claim[2] <= number <= claim[3] for number in hits.get(str(root / claim[1].path), ()))]


def unreported_batches(batches, reported):
    """Batches reduced to the rows no run has reported yet; drop the finished ones."""
    done = {(batch.test, batch.sequence, row.path, row.source) for batch, row, *_ in reported}
    remaining = []
    for batch in batches:
        rows = tuple(row for row in batch.rows if (batch.test, batch.sequence, row.path, row.source) not in done)
        if rows:
            remaining.append(batch._replace(rows=rows))
    return remaining
