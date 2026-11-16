---
title: Post and retire fleet notices
description: "Tell agents and operators about an incident or maintenance window with a scoped, expiring notice, then close it."
---

A fleet notice tells agents and operators about a short-lived incident,
planned maintenance, or other operational news. It informs its readers; it
grants no authority and overrides no policy.

## Post, inspect and close

Run these from the workspace the notice concerns.

1. Post the notice. The command prints its ID.

   ```bash
   orbit notice post --kind incident --title "Build cache unavailable" \
     --body "Builds may take longer while the cache is restored." \
     --dedupe-key build-cache:unavailable \
     --evidence url=https://example.com/cache-incident --expires-in 12
   ```

2. Check what readers see.

   ```bash
   orbit notice list
   orbit notice show <notice-id>
   ```

3. Close it when it no longer applies.

   ```bash
   orbit notice close <notice-id> --reason "Build cache restored"
   ```

A notice also ends when it expires, or when its linked task is done. Closed
and expired notices stay listable for 30 days:

```bash
orbit notice list --status all   # or closed, expired
```

## Post options

| Option | Effect |
| --- | --- |
| `--kind` | Required. `warn`, `incident`, `maintenance` or `info`. |
| `--title` | Required. One line, at most 120 characters. |
| `--body`, `--body-file` | Plain text, at most 4096 UTF-8 bytes, inline or read from a file. |
| `--scope` | `workspace` (default) reaches the workspace you post from. `global` reaches every workspace whose readers can reach the same owner, not every independent owner. |
| `--expires-in`, `--expires-at` | Hours from now, or an RFC 3339 instant at least five minutes ahead. Give one, not both. Defaults: 72 hours for a warn or incident, 12 for maintenance, 168 for info. A request over [your cap](#who-can-post-and-close) is refused. |
| `--link <task-id>` | The task the notice is about, usually its fix. The task must belong to the same owner. The notice closes when the task is done, unless you pass `--keep-after-done`. |
| `--evidence TYPE=VALUE` | A reference of type `task`, `run`, `friction`, `pull_request`, `decision`, `url` or `commit`. Repeatable. References are stored and shown, never followed. |
| `--host` | Machine ID that a maintenance notice concerns. Repeatable. |
| `--dedupe-key` | Names the subject: 1–128 characters of lowercase letters, digits, `.`, `_`, `:`, `/` and `-`. Defaults to `task:<linked_task>` when linked, else a slug of the kind and title. |

## Report the same subject again

The dedupe key identifies a subject within its workspace. If an open notice
already holds the key, your post does not create a second notice. It updates
the notice when you are allowed to edit it. Otherwise it corroborates it: the
content stays and the report count goes up. The post reports its outcome as
`created`, `updated` or `corroborated`, and is safe to retry after a lost
reply.

## Who can post and close

Orbit takes the poster class from the session, not from a field in the post.

| Poster | Can post | Can close | Longest expiry |
| --- | --- | --- | --- |
| Operator | Any notice | Any notice | 720 hours |
| Orchestrator | Any notice | Its own crew's notices, and notices that orchestrators or agents posted | 168 hours; 72 for maintenance |
| Managed agent | `incident` or `info`, in its own workspace, with a linked task or evidence | Its own task's notices | 24 hours, also its default |
| Resident mandate | `warn`, `incident` or `info`, in its own workspace, with a linked task or evidence | Its own task's notices | 168 hours for `warn` (72 default); 24 hours for others; status lines expire at twice the heartbeat |

A managed task can hold at most 2 open notices and make 5 posts in 24 hours. A resident mandate may hold at most 10 open notices (its status line plus one warn per blocker) and make 30 posts per hour.

## Where a notice is listed

The workspace you post from is the notice's home, and that workspace's owner
holds the record.

- `orbit notice list` shows the workspace's open notices and the global
  notices homed elsewhere on the same owner, ordered by kind (`warn`,
  `incident`, `maintenance`, `info`), then newest update time.
- A federated list with no workspace merges every reachable owner. Check its
  `degraded` result for owners that could not be read.
- `orbit notice show` prints the full body in any state, and the history for
  operators.

## Notices for agents

**MCP.** The `notice` toolset exposes `orbit.notice.post`, `orbit.notice.list`,
`orbit.notice.show` and `orbit.notice.close`. A server without a role does not
include it: start it with `orbit mcp serve --toolset notice`, or choose a
[role or toolset](/how-to/mcp-integration/#toolsets) that does.
`orbit tool run` and activities reach the tools under their own permissions.

**Activity briefs.** An `agent_loop` activity receives open notices in its
prompt only when its YAML asks, with `fleet_notices: read` or `post` or a
non-empty `notice_instruction`. The brief holds up to 5 open notices, marked
as untrusted data (`warn` notices and residents' status lines are excluded: an
operator ask and a resident's own status report give a working agent nothing to
act on, and would crowd incidents out of the brief). Posting guidance, from
`fleet_notices: post` or `notice_posting_instruction`, applies whenever the
activity may call `orbit.notice.post`, even when no brief is delivered. None of
these grants authority. See the
[fleet notice instructions](/reference/activity-job-yaml/#fleet-notice-instructions)
reference for each level and its built-in text.
