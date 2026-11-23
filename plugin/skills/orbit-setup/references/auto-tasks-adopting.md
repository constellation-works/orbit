# Auto-tasks: adopting a current bundled body

## Contents

- [Adopting a current bundled body from a stale fork](#adopting-a-current-bundled-body-from-a-stale-fork)

## Adopting a current bundled body from a stale fork

A genuine custom body is intentionally preserved, even if it still names a
retired tool. Sync recognizes historical shipped bodies by exact body digest;
it cannot decide which local prose to discard. Do not weaken that matcher,
delete/recreate the definition, or use `restore` to replace an existing fork.

`run-failure-patterns` has a narrower reconciliation. When its effective body
omits the bundled host-aware deduplication rule (machine id, leaf run id and
task id), `orbit auto-task show` and `list` report a stale contract, `orbit
doctor` flags it, and minting is held until the body is reconciled.
`orbit auto-task update run-failure-patterns --adopt-bundled-contract` appends
only that bundled paragraph and acceptance criterion and keeps every other
field.

It cannot be combined with another edit, and it writes nothing when the
body already carries the rule. Any other stale fork still uses the procedure
below.

Use the owning checkout and its registered workspace explicitly. This is an
operator change after review, separate from preparing or testing a repair:

1. Snapshot the effective definition through `orbit auto-task show <name>
   --workspace <owner> --json`. Agents can read effective definitions through
   the granted `orbit.auto_task` action `list` tool, selecting the named row; the CLI
   `show` also supplies layering and source diagnostics. Confirm `definition_source`
   points at the intended checkout.

   Save the complete response, including
   `layering.forked_fields`, not just the YAML or `layering.settings`: settings
   can still live in the fork itself. Record enabled, the complete schedule,
   dedupe, crew, priority, complexity, all tags and any other effective fields.
2. Extract the proposed fields from the **matching installed release's**
   bundled definition. Initialize a fresh disposable workspace with that
   executable and the target's base branch, then use `auto-task show <name>
   --json` there to obtain its uncustomized template. For candidate validation,
   obtain fields from that candidate's bundle. Take `template.description`,
   not the top-level description; parse YAML if using bundled source.

   Render
   any workspace branch placeholders for the target. Diff the effective template
   against that bundle and review every custom procedural difference. Choose
   explicitly what to replace and what to retain, including title, criteria,
   context files, required tools and comments. Keep a before snapshot, proposed
   patch, readable diff and decision record together as review artifacts.
3. Replace only the approved fields with a field-scoped update. For a
   description-only adoption:

   ```bash
   # bundled-body.txt contains the parsed template.description, reviewed above.
   orbit auto-task update <name> --workspace <owner> --body "$(cat bundled-body.txt)"
   # Or write a JSON file and use the registered tool:
   orbit tool run orbit.auto_task --input-file reviewed-body-patch.json
   ```

   The JSON shape is
   `{"action":"update","name":"<name>","workspace":"<owner>","template_patch":{"description":"<reviewed bundled body>"}}`.
   Include another body field such as `acceptance_criteria` in `template_patch`
   only when its replacement was separately reviewed. Omit operational fields.
   Do not send a whole `template` through MCP: omitted fields receive defaults.
   Do not send tags as a patch to add one tag: that field replaces the list.

Re-read immediately before applying; if the snapshot changed, review again.
   The body update has no compare-and-swap precondition, so coordinate with
   other editors and avoid concurrent edits during adoption.
4. Read back with `show --json` and compare every saved effective field. Only
   approved body fields and the edit stamps may differ. Stop on a settings
   mismatch; do not blindly replay an old full definition over a newer edit.
   Run `orbit workspace sync --workspace <owner> --check --json`, review the
   **whole** report (sync can affect other managed assets), then apply
   `orbit workspace sync --workspace <owner> --json` when its changes are agreed.

   Inspect the named auto-task's action and read back again. A settings-only
   fork migrates and reports `layering.body: managed`; enabled, schedule,
   dedupe and template settings must still equal the snapshot. Migration
   restores bundled creation metadata; save the original stamps in the review
   record rather than treating them as operational settings. Retained custom
   body fields leave it `forked`, intentionally; name those fields rather than
   claiming upstream refresh has resumed.

   User-authored definitions remain
   user-authored and do not acquire bundled management through this procedure.
5. Prove the patch first in a disposable workspace in an isolated child process
   with inherited Orbit/Git authority cleared (see the repository development
   guide's safe mutable CLI fixtures). Seed the observed fork and its complete
   settings, apply the exact payload, sync, compare before/after, and mint there
   with `orbit auto-task mint <name>`. Read the minted task back and verify its
   instructions and settings. Keep payload, snapshots, sync actions and mint
   evidence.

   Never mint on the live workspace merely to verify adoption:
   `mint` creates real work even when disabled. Existing minted tasks keep their
   original instructions and need a separate reviewed action if still active.

For a stale `friction-curation` fork, the adopted listing and group queries
must use `orbit.search` (`kind: friction`, `status: friction:open` for the open
set). Preserve the bundled 1000-hit truncation guidance and tag narrowing.
Review local resolution wording and criteria as well as the retired listing
paragraphs; replacing only the description may leave older criteria forked.
`orbit.friction.list` stays inactive and its refusal points agents to search.

After delivery, the operator still needs to approve the recorded custom-body
decisions and apply the reviewed patch on the owner, then verify settings and
sync as above. A disposable proof is not evidence of live adoption.

`mint` ignores the schedule, the dedupe policy, and `enabled`, and leaves the
scheduler's cursor untouched — so it creates real work even for a disabled definition. Inspect with
`show` first; mint only when creating that task is intended.

`list` and the dashboard read one state per definition. `skipped` means an
enabled delivery definition another machine owns, so this host never admits
work for it. Its reason is printed on stderr and carried as `skipped_reason`
by the MCP listing and the dashboard API, where its next evaluation reads
`unavailable`. `enabled` and `disabled` follow the definition's own `enabled` flag.

Over MCP (opt-in in the operator profile via `--toolset automation` or
`machine.mcp.toolsets`; without MCP use `orbit auto-task` or
`orbit tool run orbit.auto_task`): `orbit_auto_task` action `add`,
`orbit_auto_task` action `list`, `orbit_auto_task` action `update`, and
`orbit_auto_task` action `mint`; `orbit_auto_task` action `update` with `enabled` is the
kill-switch.

Adding `expected_enabled` (the observed state) makes it a checked,
atomic toggle that is refused when the definition no longer matches; that form
needs operator authority and an explicit `workspace` and accepts no other
edits. `toggle`, `delete`, `show`, `restore`, `recover`, and `reset` are not
on MCP; use `orbit auto-task`.
`delete` and `reset` need operator authority on every surface: a
non-interactive caller sets `ORBIT_OPERATOR=1`, which is audited.
