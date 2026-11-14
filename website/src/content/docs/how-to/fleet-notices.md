---
title: Post and retire fleet notices
description: Share scoped, expiring operational information with agents and operators.
---

Fleet notices share short-lived incidents, planned maintenance, or other information with agents and operators. They inform readers; they do not grant authority or override policy.

## Post, inspect and close

From the workspace the notice concerns, post an incident, then inspect and close it when it no longer applies. Replace the example URL and the notice ID with your own evidence and the ID returned by the post or list:

```bash
orbit notice post --kind incident --title "Build cache unavailable" \
  --body "Builds may take longer while the cache is restored." \
  --scope workspace --dedupe-key build-cache:unavailable \
  --evidence url=https://example.com/cache-incident --expires-in 12
orbit notice list
orbit notice show <notice-id>
orbit notice close <notice-id> --reason "Build cache restored"
orbit notice list --status all
```

`kind` and `title` are required. Kinds are `incident`, `maintenance`, and `info`; the title is one line of at most 120 characters. The optional body is plain text of at most 4096 UTF-8 bytes; use `--body-file` instead of `--body` to read it from a file. Repeat `--evidence TYPE=VALUE` for references of type `task`, `run`, `friction`, `pull_request`, `url`, or `commit`. References are stored and shown, not dereferenced. Maintenance notices may also name affected machine IDs with repeated `--host` flags.

The selected workspace is the notice's home, and its owner holds the record. The default `workspace` scope reaches that workspace; `--scope global` reaches workspaces whose readers can reach that owner, not every independent owner. Listing a workspace normally includes its own notices and global notices homed elsewhere on the same owner. A federated list without a workspace merges reachable owners; check the `degraded` result for owners or homes that could not be read. `list` defaults to open notices; `show` includes the full body in any state, and operators also see history.

A dedupe key identifies the subject within the home workspace. If an open notice holds the key, a poster allowed to edit it updates it; another poster corroborates it instead, increasing its report count without replacing its content. The post outcome is `created`, `updated`, or `corroborated`; a lost reply is safe to retry. If omitted, the key defaults to `task:<linked_task>` when linked, or a kind and title slug otherwise. Keys are 1–128 characters using lowercase letters, digits, `.`, `_`, `:`, `/`, and `-`.

Set `--expires-in` in hours or `--expires-at` to an RFC 3339 instant at least five minutes ahead, not both. Defaults are 72 hours for incidents, 12 for maintenance, and 168 for info, subject to poster limits. A requested expiry over the applicable cap is refused. With `--link <task-id>`, the linked task must belong to the same owner, though it may be in another of that owner's workspaces. The notice closes when that task is done unless you pass `--keep-after-done`. It can also expire or be closed manually; closing an inactive notice leaves it unchanged. Closed and expired notices remain listable for 30 days. Use `--status closed`, `expired`, or `all` to find them.

## Authority and managed agents

Orbit derives the poster class from the session, not a field in the post. An operator can close any notice; an orchestrator can close its own crew's notices and orchestrator- or agent-authored notices; a managed agent can close only its own task's notices. Managed agents may post only `incident` or `info` notices in their own workspace, with a linked task or evidence. Their default and maximum expiry is 24 hours. Posting and open-notice quotas also apply: at most 2 open notices and 5 posts in 24 hours per managed task.

## MCP and activity briefs

The MCP `notice` toolset exposes `orbit.notice.post`, `orbit.notice.list`, `orbit.notice.show`, and `orbit.notice.close`. It is opt-in on a server without a role; enable it with `orbit mcp serve --toolset notice`, or select a [role or toolset](../mcp-integration/#toolsets) that includes it. Toolset selection controls the MCP server surface; `orbit tool run` and activities can reach registered tools independently, subject to their own permissions.

Brief delivery is separate. An `agent_loop` activity asks for the brief in its catalog YAML by setting `fleet_notices: read` or `post`, or by declaring a non-empty `notice_instruction`. An activity with neither gets no notices in its prompt; for one that asks, the host includes up to 5 open notices as untrusted data. The reading text is appended only when a brief is delivered. Posting guidance, from `fleet_notices: post` or a declared `notice_posting_instruction`, applies whenever that dispatch may call `orbit.notice.post`, even if no brief is delivered. Neither the built-in texts nor the declared instructions grant authority. See the [agent policy reference](../../reference/activity-job-yaml/#fleet-notice-instructions) for each `fleet_notices` level, the built-in text it selects, and the delivery conditions.
