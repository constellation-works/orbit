# Verify CI sweep log evidence

## Contents

- [Job-scoped log selection](#job-scoped-log-selection)
- [Attribute a failure to its job and command](#attribute-a-failure-to-its-job-and-command)
- [Verify without filing](#verify-without-filing)

## Job-scoped log selection

CI evidence schema 2 binds each failure row to one failed job: both log scopes
select that job, and checkout provenance carries its job ID. A separate
`diagnostic_unit` retains a complete runner command from its `Run` group through
its nonzero process completion, up to 256 KiB (`kind: runner_command`,
`complete: true`).

When retention exceeds that limit, collection can instead
supply `kind: runner_failure_regions`, `complete: false`, with
`command_complete: true` and `selection_complete: true`: the command boundaries
and every recognized failure anchor were scanned, but the command was not fully
retained. Regions keep the command header and command-display closing delimiter, every recognized emitted failure anchor, summaries
and the process exit.

A failing test keeps its captured output from the anchor
through the stdout/stderr block until the next test status line, the
captured-output section end, or the summary. Compiler diagnostics and other
`error:` anchors keep their next 11 context lines. The selection is bounded by
64 KiB. When that bound cuts inside a failure block, the text marks the cut
(`[... output block cut; N bytes not retained ...]`) so a reader cannot treat it
as an ordinary gap between anchors.

Regions that still cannot fit under the cap
are withheld, as before. Gaps between retained regions carry byte-omission
markers. Large left/right assertion payloads retain a 512-byte prefix.
`assertion_payload_omitted_bytes` counts only those left/right payloads, not
block cuts or ordinary gaps. The structured `command_bytes`,
`retained_source_bytes`, `omitted_bytes`, `assertion_payload_omitted_bytes` and
`failure_anchor_count` make these limits auditable.

A failure-anchor count
includes repeated reports of the same test.
The standalone log tool exposes this alternative as `failure_regions`.
Nextest `FAIL [` blocks are also scanned independently of command selection,
including outside a `Run` group and in logs with several failed commands.
They survive unrelated overlong lines and command-region overflow.

The
truncated `log` appends these blocks to its usual head/tail excerpt, allowing
up to 64 KiB of failure text in addition to `max_bytes` and omission markers.
The separate `failure_regions.text` is also capped at 64 KiB; its range metadata
is bounded by 128 records. These two text views add at most 128 KiB in total.
Each block retains up to 16 KiB of whole lines; the total allowance is shared
across at most 128 blocks, so a large early block cannot erase later anchors.

Cuts carry the output-block marker. `byte_ranges` contains zero-based,
end-exclusive `start_byte` and `end_byte` offsets into the original source
(before redaction), and retained/omitted source counts. Source or block-count
limits set `selection_complete: false`; collected blocks remain available to
a human. When nextest failures are recognized, the independent selection
supplies `failure_regions`.

These regions use `command_bytes` for the scanned
log's total size and only claim
`command_complete` when every block belongs to a completed runner command.
CI investigation requires both completeness flags before binding regions to
the known failed step. Logs without nextest failures keep the usual head/tail
behavior.

## Attribute a failure to its job and command

Complete-command diagnostics require exactly one failing command. CI attribution
requires one known failed step; all primary command log columns, including
omitted lines, must match that job and step. Nextest regions can cover several
completed commands in that same step when gh supplies consistent step columns.
Raw job API logs require exactly one failing command for automatic attribution;
otherwise their blocks remain available with `command_complete: false`.

Filing validates the evidence contract and uses selected evidence for signatures;
`log_excerpt`, `log_truncated`, and byte counts describe the head/tail display
plus any appended nextest blocks. Complete command descriptions use a 4,000-byte diagnostic display cap.
Failure-region descriptions retain the entire bounded selection (at most 64 KiB)
and its omission accounting so offline workers receive every selected failure.

Partial command retention cannot establish a complete compiler set for cross-job
compiler deduplication. Existing complete-command compiler proofs are unchanged.

Each process stdout read stops with a retryable error beyond 8 MiB (at most one
4 KiB lookahead chunk); the existing process timeout still applies. Command
selection uses at most two 256 KiB full-command buffers, two 64 KiB region
buffers and a 16 KiB line buffer. Only left/right assertion payloads may exceed
the line buffer; other overlong/invalid lines fail closed.

Explicit source
truncation notices, missing command boundaries, multiple failing commands,
unknown/ambiguous failed steps, incomplete checkout identity, exhausted read
budgets and failure-region overflow defer that job. Oversized commands are never
labelled fully retained. No extra queries or retries
are introduced; complete siblings still file.

`max_job_log_reads` caps failed-job
reads across the snapshot (default
6, maximum 25); `max_checkout_log_reads` separately caps additional same-job
checkout reads (default 3, maximum 25). The rotating investigation slot also
rotates overflow jobs in stable ID order. Legacy schema-1 failures require
recollection because their run-wide logs and checkout scans cannot establish
job attribution. Neither event nor PR head can substitute for runner checkout.

## Verify without filing

Read-only verification: inspect the run's job metadata with `gh run view <run>
--json databaseId,jobs --repo <owner/repo>`, then use the production bounded log
reader with explicit `run`, `job`, and `scope` for each failed job. Compare each
row's job ID, diagnostic, and checkout provenance with that supplying job; repeat
with reversed metadata order and a one-job budget. Do not download whole logs
into memory or use a sweep that files tasks merely to verify collection.
