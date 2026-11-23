# Retry a stopped landing on the owner

## Retry a stopped owner landing

A healthy accepted handoff can keep its native leaf binding while its owner landing
stops on a provider gate. A `BLOCKED` pull request with only running checks and no
review refusal keeps polling within the landing job's existing check-wait budget;
once the checks pass and the forge permits merging, that job lands without operator
action. Running checks grant no merge authority, and the wait never extends the
configured deadline. A review or administrative refusal still stops the job.

After a stop or an exhausted wait budget, inspect the owner's distributed claim
panel, original stop, latest landing attempt and provider state. `orbit.workflow` action `land`
reports the recorded stop cause and the retry path while completion authority remains
active; it reports an operator revoke only when that authority was revoked. Do not
resume or rebind the foreign leaf just to retry landing. Only a trusted operator may
request a same-handoff retry:

```bash
orbit task retry-handoff <handoff-id> --owner-workspace <owner-workspace-id> \
  --candidate <accepted-commit> --base <accepted-base> \
  --attempt <latest-attempt> --state stopped \
  --reason '<why the same candidate can land now>' --request <saved-request-key>
```

The MCP equivalent is `orbit.task.recover` action `retry_handoff` with `workspace`, `handoff_id`,
`expected_candidate_commit`, `expected_base_commit`, `expected_latest_attempt`,
`expected_latest_state`, `reason`, and `request_key`. The workspace must be the owner's
logical workspace ID or absolute checkout path. This is operator-only and unavailable
inside a managed run. The dashboard offers **Retry landing** with the same expectations
and retains its replay key in browser storage.

Reuse the key and exact inputs after a lost reply. The durable receipt reserves one
attempt atomically and resolves the same run on replay. Only a stopped landing or an
uncertain intent with no live owner job qualifies. The normal landing job reconciles
external intent before any merge: an already-merged exact candidate is observed and
completed without a repeated merge. Every native certificate, review, required check,
remote identity/head/base, lease and merge-certainty gate remains in force.

Original
stopped evidence stays in history. Changed or invalidated work needs its own repair
and fresh handoff; the retry accepts no `auto_fix`, raw pipeline input or head override.
