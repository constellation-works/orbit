## Filesystem writes

The production Clippy pass in `make ci-lint` disallows `std::fs::create_dir_all`, `std::fs::create_dir` and `std::fs::write`, including aliases. Use the `orbit_common::fs::io` helpers:

| Need | Helper |
|---|---|
| Orbit-owned directory | `create_private_dir_all`, or `create_private_dir` for exclusive creation. Owner-only on Unix regardless of umask; existing modes are left alone. |
| Replace a durable file | `atomic_write_text` / `atomic_write_bytes`: stage a private sibling, sync, rename, sync the parent. Permissions are preserved. |
| Validate between staging and commit | `StagedTextFile`; dropping an uncommitted stage removes it. |
| New private file, exclusively | `write_new_private_text`: stage and sync a private sibling, then hard-link it into place, failing if the path exists. The path never holds partial content. |
| Deliberate new-file or disposable write (not atomic) | `write_text_with_parent` |

State and task-artifact writers must use the private helpers; `orbit doctor --fix-state-directory-permissions` repairs legacy writable state. An exception is scoped to the one intentional call and names its reason in `#[allow(clippy::disallowed_methods, reason = "...")]`. Current exceptions: external source, installation, service-manager and Cargo output directories; in-place edits of dotfile-linked user TOML configs; artifact exports to user-selected paths (including symlinks and devices); scaffold sources with their `--force` behavior. None promises crash-safe replacement. Test fixtures may write directly.

## Child processes

Git failure diagnostics retain the child's exit code even when stderr is empty.
When a supervised child exits without a code, the executor appends its actual
termination status (including the signal number and name on Unix) to captured
stderr. Timeout, cancellation and output-capture limits remain separate causes;
an absent exit code alone does not identify a signal.

Production code never waits on a child without a deadline. Spawn through `orbit_common::process::run_bounded` or `run_bounded_capped`, which put the child in its own process group, kill the group at the deadline and cap captured output. `run_bounded_capped_typed` returns a failed spawn as its `io::Error` for callers that branch on the kind (the `ETXTBSY` retry in `orbit update`, the sandbox probes). `run_bounded_capped_complete_typed` also requires stdout and stderr to reach EOF within the deadline. Update version and admission-contract probes use this mode: a parent that exits while a descendant retains either pipe still times out, and the runner tears down the group and closes both pipes. The four bounded runners delegate to `orbit_common::process::supervisor` with null stdin, keep-prefix sinks, a 200 ms termination grace, and a 500 ms post-exit drain capped by the deadline. Signal interception and the stopped-descendant watch are disabled. Unix capture runs on two stop-aware pipe workers, both joined within the drain budget, so no capture worker survives the probe. Output-worker setup, readiness and read failures return an execution error after group cleanup and worker joins, including in EOF completion mode; prefix truncation and a bounded post-exit drain remain intentional outcomes. The other modes keep their existing behavior of tearing down descendants and accepting the direct child's status as soon as it exits.

The production Clippy pass disallows `std::process::Command::output` and `Command::status`; an exception needs `#[allow(clippy::disallowed_methods, reason = "...")]`. The current exceptions have no natural deadline: the SSH stdio relay in `orbit-mcp` and the interactive or operator-watched `sudo` steps of `orbit init` on Linux. Fixtures may use both.

### Recorded owner and provider cancellation

Run cancellation captures identity-bound process handles before identity-bearing liveness and token verification, so a replacement during verification cannot be adopted even if its finite-resolution start time matches. It retains those handles across TERM, both two-second grace intervals, and KILL. Linux uses `pidfd_open`/`pidfd_send_signal` and descriptor liveness; macOS uses libproc unique process IDs and audit-token PID/version signalling. macOS refreshes an exec version only while the unique process ID still matches; a concurrent exec refuses the signal rather than treating the worker as gone. Missing kernel support, unreadable identity, and other Unix platforms fail closed without numeric PID signalling. Non-Unix cancellation remains unsupported.

After the caller verifies the recorded token with the original handle retained, a live group leader authorizes a snapshot of its group members, each bound to a separate identity before the leader is rechecked. Cancellation signals those retained identities individually, never a numeric PGID. Owners in shared groups are stopped individually. Exit or group change during snapshot acquisition refuses group teardown; no later identity probe can suppress that refusal. If the retained leader exits during verification, a live or unreadable numeric group also refuses teardown rather than authorizing a replacement snapshot. Members created after the snapshot, a reused PGID, or unavailable group liveness can leave cancellation incomplete: group probes are evidence for refusal only and never authorize additional signals. Claims and reservations therefore stay held until teardown is confirmed. This favors protecting unrelated processes over forcibly clearing an uncertain group.

